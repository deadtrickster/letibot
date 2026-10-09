//! The head server: a Unix socket at `$XDG_RUNTIME_DIR/harnessd.sock`.
//!
//! §13.4: *"unix socket … filesystem permissions are the auth; no ceremony"*. So
//! the socket is created mode 0600 and there is no handshake beyond the protocol
//! version. A remote head (W8's WebSocket, not built here) adds TLS and a bearer
//! token in front of the same frames.
//!
//! # One socket, many sessions
//!
//! The socket serves a [`Registry`], not a `Hub`. A connection resolves its session
//! at ATTACH and can move to another with `Switch` — see [`serve_conn`], and
//! [`crate::registry`] for why the resolution is a refusal rather than a fallback.
//! [`serve`] is the single-session form and builds a one-entry registry, so there
//! is exactly one attach path.
//!
//! # Threads
//!
//! One accept thread. Per connection, two: a **writer** that blocks in
//! [`Hub::next_batch`] and a **reader** that blocks on the socket. They are
//! separate because the two directions are genuinely independent — a head that is
//! rendering a long turn must still be able to send an interrupt, and a head that
//! is typing must still be receiving deltas. A single-threaded select over both
//! would need a poll loop and would put a latency floor under both.
//!
//! **Nothing in here can stall the engine.** The writer thread is the only thing
//! that touches the socket for output, and if it blocks on a slow client the hub's
//! bounded queue fills and that head is demoted — [`crate::hub`] — while
//! `publish` returns immediately.

use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::hub::{CommandKind, Delivery, Hub, Reply};
use crate::protocol::{
    ClientFrame, HEAD_RUN_TOOLS, PROTOCOL_VERSION, PeekShape, REJECT_NOT_IN_STORE,
    REJECT_UNKNOWN_SESSION, ServerFrame,
};
use crate::registry::{Registry, SessionWiring};
use crate::wire::{FrameReader, FrameWriter, WireError};

/// The default socket path, per §13.4. Falls back to `/tmp` when
/// `XDG_RUNTIME_DIR` is unset, which is the case in a bare test environment.
pub fn default_socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d).join("harnessd.sock"),
        None => PathBuf::from(format!("/tmp/harnessd-{}.sock", libc_getuid())),
    }
}

fn libc_getuid() -> u32 {
    // Avoiding a `libc` dependency in a crate that otherwise has none: `getuid` is in the C
    // library every target links, and declaring it works on Linux and macOS alike, where
    // the `/proc/self/status` read this replaced fell back to 0 on a Mac — one fallback
    // socket for every user. The uid is only used to keep two users' fallback sockets apart.
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid cannot fail.
    unsafe { getuid() }
}

/// A running server. Dropping it does **not** stop the session: see
/// [`ServerHandle::shutdown`].
pub struct ServerHandle {
    path: PathBuf,
    registry: Arc<Registry>,
    accept: Option<JoinHandle<()>>,
    /// **(dev, ino) of the socket this daemon bound**, so going away can remove *its own*
    /// file and never whatever is at that path by then. See [`unlink_if_ours`].
    socket_id: Option<(u64, u64)>,
    /// **The claim on this folder.** Held for the process's life, and named `claim` rather
    /// than `lock` because being clear about what it is is the whole point: the kernel
    /// releases it when this process goes, so it cannot be left behind by a shutdown path
    /// that unlinked something and did not exit. Dropped LAST (declaration order), after
    /// the socket file has been removed, so no daemon can slip in between.
    claim: Option<FolderClaim>,
}

/// **The lock that says a folder has a daemon, because a filename cannot.**
///
/// The operator's own words for the defect this exists to close, and the diagnosis was
/// theirs (2026-10-04, `TODO.md`): *"Liveness is being inferred from a filename, and an
/// unlinked socket is indistinguishable from a dead daemon."* MEASURED on their box and
/// reproduced here, on a scratch socket: the file in `$XDG_RUNTIME_DIR` is UNLINKED by
/// `ServerHandle::shutdown` and by `Drop`, and a daemon asked to stop can unlink it and then
/// keep listening on the now-nameless inode. The next start finds no file, probes nothing,
/// and binds a second socket at the same path — Linux is happy to — so two daemons serve one
/// folder against one store. The transcript trigger then refuses every append
/// (`transcript_item seq must be the next one`), and the SESSION IS BRICKED: nothing can be
/// said to it, because saying anything is a write.
///
/// **An `flock` dies with the process**, including on `SIGKILL`, which no `Drop` survives —
/// and that is the property `$KEY.sock` does not have. It is the answer the tree already had
/// written down (2026-08-29: *"Single-instance guards: pidfile is defeatable, pgrep matches
/// its own launcher, use flock"*).
///
/// `std::fs::File`'s locks are `flock` on Unix and this crate takes **no new dependency**
/// for them (it says in as many words a few lines down that it does not depend on `libc`,
/// and it should not start).
///
/// **The lock file is not unlinked by anything here, deliberately.** The socket file is
/// unlinked by this daemon's own shutdown path, which is exactly what made a filename
/// worthless as a liveness test; the lock file is only ever created and written, so its name
/// keeps pointing at the inode that holds the claim. An operator who deletes it by hand
/// defeats it — and that is a hand on the box, not a code path, which is the distinction
/// the socket file could not make.
struct FolderClaim {
    /// **Held, never read.** The lock lives in this open file description, and dropping it is
    /// what releases the claim; there is deliberately no getter, and the absent reader is the
    /// design rather than an oversight (`_` so the compiler is told the same thing).
    _file: std::fs::File,
    path: PathBuf,
}

impl FolderClaim {
    /// The lock file that belongs beside a socket: `$KEY.sock` -> `$KEY.lock`.
    fn path_for(socket: &Path) -> PathBuf {
        socket.with_extension("lock")
    }

    /// **Take the claim, or say who has it.**
    fn take(socket: &Path) -> Result<FolderClaim, ClaimRefusal> {
        let path = Self::path_for(socket);
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => return Err(ClaimRefusal::Unavailable { why: e.to_string() }),
        };
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(ClaimRefusal::Live {
                    pid: Self::holder_pid(&path),
                });
            }
            // **Not a held lock: a filesystem that cannot lock at all.** Said out loud and
            // fallen back on, because refusing to START a daemon is a worse outcome than
            // failing to catch a second one, and silence here would be the failure mode
            // this whole change is about.
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(ClaimRefusal::Unavailable { why: e.to_string() });
            }
        }
        // **Our pid, inside it, so a refusal can name somebody.** Not the source of truth —
        // the lock is — but a refusal that says *"already served by a live daemon"* and
        // stops is a refusal the operator cannot act on.
        if let Err(e) = Self::write_pid(&file) {
            eprintln!(
                "could not write the claiming pid into {}: {e}. The claim itself is held, so \
                 a second daemon is still refused; only the line naming it will be empty.",
                path.display()
            );
        }
        // The claim is a fact about a folder, not a secret; 0600 keeps it the same as the
        // socket beside it and keeps another user's stray files out of the way.
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        Ok(FolderClaim { _file: file, path })
    }

    fn write_pid(file: &std::fs::File) -> io::Result<()> {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = file;
        f.set_len(0)?;
        f.seek(SeekFrom::Start(0))?;
        writeln!(f, "{}", std::process::id())?;
        f.flush()
    }

    /// The pid the holder wrote, or `None` when the file is empty — a holder that took the
    /// lock and died between the lock and the write, which the caller words as unknown
    /// rather than inventing a number.
    fn holder_pid(path: &Path) -> Option<u32> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

/// Why a claim was not taken. Three outcomes, and the middle one is the only one that stops
/// a daemon from starting.
enum ClaimRefusal {
    /// Somebody else is serving this folder, and here is who.
    Live { pid: Option<u32> },
    /// This filesystem cannot hold the claim (no writable directory, an exotic mount). The
    /// caller falls back to the probe it used before there was a lock.
    Unavailable { why: String },
}

/// **Remove a socket file only if the path still names OUR socket.**
///
/// `shutdown`, `Drop` and the accept thread all used to call `remove_file(&self.path)`,
/// which removes *whatever is at that path* — and the path is shared state. Once a second
/// daemon could take a folder (see [`FolderClaim`]) this became the way the loop repeats:
/// the old daemon's `Drop` unlinks the NEW daemon's socket, which makes the new daemon the
/// one whose file the next start cannot see. Comparing (dev, ino) makes the removal mean
/// *mine* rather than *that name*.
fn unlink_if_ours(path: &Path, id: (u64, u64)) {
    match std::fs::metadata(path) {
        Ok(m) if (m.dev(), m.ino()) == id => {
            let _ = std::fs::remove_file(path);
        }
        // Somebody else's socket is at our path now. It is not ours to remove.
        Ok(_) => {}
        // Already gone. `ENOENT` on a removal that has nothing to do is the ordinary case.
        Err(_) => {}
    }
}

impl ServerHandle {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// **Where this daemon's claim lives**, for a caller that wants to say so — the launcher
    /// names it in the refusal it prints when a socket file has gone missing under a live
    /// daemon, which is the state a person has to clean up by hand.
    pub fn claim_path(&self) -> Option<&Path> {
        self.claim.as_ref().map(FolderClaim::path)
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// The session a bare attach lands on. `None` for a registry with nothing in
    /// it, which is a daemon that has not opened one yet — not an error, and not a
    /// reason to invent one.
    pub fn default_hub(&self) -> Option<Arc<Hub>> {
        self.registry.resolve("")
    }

    /// Stop accepting, wake every head with `Closed`, remove the socket.
    ///
    /// This is the case §13.2 calls daemon shutdown, which *is* an abort — as
    /// against a head detaching, which is not.
    pub fn shutdown(mut self) {
        self.registry.close();
        // Unblock the accept loop by connecting to it once.
        let _ = UnixStream::connect(&self.path);
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
        // **Our own file, and only ours** — see [`unlink_if_ours`]. The accept thread removes
        // the socket when it ends too, and removing it twice is an ignored `ENOENT`.
        if let Some(id) = self.socket_id {
            unlink_if_ours(&self.path, id);
        }
        // `self.claim` is dropped here, last, after the path is clear: the lock is released
        // only once this daemon's socket is gone, so a start that slots in behind us cannot
        // have its file removed by us on the way out.
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        // Best effort only. An abandoned handle leaves the socket, which is
        // recoverable; taking the session down on a drop would not be.
        //
        // **And it removes the socket only if that path still names OUR socket.** This was
        // the way the two-daemon loop repeated: an old daemon's `Drop` unlinked whatever
        // file was at the path, which by then could be the NEW daemon's, leaving the live
        // one invisible to every liveness test the box has. `unlink_if_ours` compares
        // (dev, ino); a path that now holds somebody else's socket is left alone.
        if let Some(id) = self.socket_id {
            unlink_if_ours(&self.path, id);
        }
    }
}

/// `EMFILE` and `ENFILE`. Named here rather than pulled from `libc`, which this
/// crate does not depend on and should not start depending on for two integers
/// that have been fixed on Linux since before this program existed.
const EMFILE: i32 = 24;
const ENFILE: i32 = 23;

/// **Can this `accept` error be retried?**
///
/// `ConnectionAborted` is a client that went away between `connect` and
/// `accept`, and is ordinary. The two out-of-descriptors errnos are the process
/// or the box being briefly out of room, which other threads are already fixing.
/// `Interrupted` and `WouldBlock` are the usual suspects and cost nothing to
/// include. Everything else means the listener itself is broken.
fn is_transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
    ) || matches!(e.raw_os_error(), Some(EMFILE) | Some(ENFILE))
}

/// Bind and start accepting for a single session.
///
/// Kept because most of the workspace has exactly one hub and does not want to
/// know about a registry to serve it. It is a thin wrapper and not a second
/// implementation: [`Registry::of`] wraps the hub and [`serve_registry`] does the
/// work, so there is one attach path and one switch path.
/// **Nobody gave a password, and WHICH silence that was** — the sentence the daemon puts on
/// the frame the `askpass` helper reads.
///
/// Built here, where the head count is known, because the helper cannot know it: it held
/// one connection and got a `None`. That `None` was three facts wearing one word —
/// *no head was attached*, *a head was attached and nobody answered*, and *a person
/// declined* (that one is written at the call site, where the refusal arrives) — and the
/// operator's report was that the sentence they got covered all three and told them
/// nothing. The first is a fault in the wiring and the second is a person who did not act;
/// they need different next actions, so they are different sentences.
///
/// MEASURED 2026-10-08: `no password was given — no head answered before the deadline, or
/// the person refused`.
fn no_password_sentence(heads: usize) -> String {
    if heads == 0 {
        "no head was attached to this session, so no card could reach anybody".to_string()
    } else {
        format!(
            "a head was attached and nothing was answered before the deadline ({heads} attached)"
        )
    }
}

#[cfg(test)]
mod no_password_tests {
    use super::no_password_sentence;

    /// **The two silences are two sentences**, and neither says "or the person refused".
    ///
    /// This test cannot fail on the code before it: the function did not exist. What it
    /// pins is the distinction the old wording destroyed — no head attached is a bug, a
    /// head attached and silent is a person who did not act — and that a refusal is not
    /// among them (it is written where the refusal arrives, as a decision).
    #[test]
    fn no_password_says_which_silence_it_was() {
        let nobody = no_password_sentence(0);
        let nobody_answered = no_password_sentence(2);
        assert!(nobody.contains("no head was attached"), "{nobody}");
        assert!(!nobody.contains("deadline"), "{nobody}");
        assert!(nobody_answered.contains("attached"), "{nobody_answered}");
        assert!(nobody_answered.contains("deadline"), "{nobody_answered}");
        assert_ne!(nobody, nobody_answered);
        for s in [&nobody, &nobody_answered] {
            assert!(
                !s.contains("refused"),
                "a refusal is a decision and has its own sentence: {s}"
            );
        }
    }
}

pub fn serve(hub: Arc<Hub>, path: impl AsRef<Path>) -> io::Result<ServerHandle> {
    serve_registry(Registry::of(hub), path)
}

/// The most bytes one `FetchRow` may return, whatever the head asked for.
///
/// **The daemon's cap, not the head's request.** A head that sent a large `len` — by a bug,
/// or by a version that meant something else by it — must not be able to make the daemon
/// serialise a megabyte onto the wire. The head pages in windows of a screenful, so this is
/// an order of magnitude above any window it draws and far below a payload.
pub const MAX_FETCH_ROW: usize = 64 * 1024;

/// The nearest character boundary at or below `at`, for slicing a body.
///
/// A byte window can land inside a multi-byte character, and a head cannot render half a
/// glyph. The daemon is the side that knows the encoding, so it is the side that rounds.
fn clamp_char(s: &str, at: usize) -> usize {
    let mut at = at.min(s.len());
    while at > 0 && !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Bind and start accepting for every session in `registry`.
pub fn serve_registry(registry: Arc<Registry>, path: impl AsRef<Path>) -> io::Result<ServerHandle> {
    let path = path.as_ref().to_path_buf();
    // **THE CLAIM COMES FIRST, AND IT DOES NOT ASK WHETHER THE FILE IS THERE.**
    //
    // This is the whole fix for the operator's 2026-10-04 entry, and the order is the
    // substance of it: the check below is gated on `path.exists()`, so a folder whose socket
    // file was unlinked by a stopping daemon had NO check at all — the bind then succeeded
    // at a path that already had a live listener on a nameless inode, and two daemons served
    // one store until the transcript trigger refused every write. A held `flock` says what
    // the filename cannot: somebody is here. See [`FolderClaim`].
    let claim = match FolderClaim::take(&path) {
        Ok(c) => Some(c),
        Err(ClaimRefusal::Live { pid }) => {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                match pid {
                    Some(pid) => format!(
                        "{} is already served by a live daemon (pid {pid}). This refusal is the \
                         lock beside the socket, not the socket file: a daemon that was asked to \
                         stop unlinks that file while it is still listening, so its absence \
                         proves nothing. Two daemons for one folder interleave rows into one \
                         transcript and the store refuses them all — stop the one named here \
                         (`letibot --stop`) and start again.",
                        path.display()
                    ),
                    None => format!(
                        "{} is already served by a live daemon (its pid could not be read from \
                         {}). Stop it and start again: letibot --stop.",
                        path.display(),
                        FolderClaim::path_for(&path).display()
                    ),
                },
            ));
        }
        // **A filesystem that cannot hold the claim does not stop a daemon starting.** The
        // probe below is still the belt; it is only the braces that are missing, and this is
        // said out loud rather than swallowed, because silence is the shape of the bug.
        Err(ClaimRefusal::Unavailable { why }) => {
            eprintln!(
                "sessionlog: cannot take the folder claim on {} ({why}). Starting anyway, with \
                 the socket-file probe as the only guard, so a second daemon for this folder \
                 could go unnoticed.",
                FolderClaim::path_for(&path).display()
            );
            None
        }
    };
    // **The belt, still worn** — and its comment corrected, because the version of it that
    // stood here is why nobody looked further: *"the bind is what would then fail"* is true
    // only while the live daemon's file still HAS that name. Unlink it and the bind succeeds
    // at a fresh inode, which is exactly the hole above. This still catches the cases the
    // lock cannot: a daemon from a build older than this one, and any other program that
    // bound the path without taking the claim.
    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!(
                        "{} is already served by a live daemon (it does not hold the folder \
                         lock, so it is an older build or another program)",
                        path.display()
                    ),
                ));
            }
            Err(_) => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    let listener = UnixListener::bind(&path)?;
    // Filesystem permissions are the auth (§13.4).
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    // **THE ACCEPT LOOP MUST BE WAKABLE WITHOUT THE SOCKET FILE EXISTING.**
    //
    // `shutdown` unblocks the loop by connecting to the path — which works exactly once, in
    // the state where the path still names this listener. Unlink the file (which `shutdown`
    // and `Drop` themselves do, and which is the ordinary state of a daemon that was asked to
    // stop) and the connect goes to `ENOENT`, the accept thread stays blocked in `accept()` on
    // a nameless inode, and the `join` in `shutdown` waits forever. MEASURED 2026-10-05, and it
    // is the operator's own field note from 2026-10-04: *"`2210747` went on TERM, `2124394`
    // ignored TERM for fourteen seconds and needed KILL"* — a daemon that has lost its file
    // cannot be stopped politely.
    //
    // So the loop is non-blocking and checks the registry's flag between polls. The cost is a
    // poll interval on the IDLE path (20 ms between a head's connect and its accept, once per
    // attach) and the gain is a stop that does not depend on a filename. The `WouldBlock` arm
    // below is deliberately before `is_transient`, which would classify it as transient and
    // print a line every 50 ms for as long as the daemon is idle.
    let _ = listener.set_nonblocking(true);
    // **What we bound, so going away can remove this and not somebody else's socket.**
    // Read off the path immediately after the bind, before any other start can be looking at
    // the same name.
    let socket_id = std::fs::metadata(&path).ok().map(|m| (m.dev(), m.ino()));

    let r = registry.clone();
    let deaf_path = path.clone();
    let deaf_id = socket_id;
    let accept = std::thread::Builder::new()
        .name("head-accept".into())
        .spawn(move || {
            // **A failed `accept` is almost never a reason to stop accepting.**
            //
            // This loop used to read `Err(_) => break`, and a daemon paid for it in
            // the field: the accept thread ended, the `UnixListener` it owns was
            // dropped, the fd closed — and nothing else changed. The socket FILE
            // stayed, because the `remove_file` lives on `ServerHandle::drop` and
            // main still held the handle. The worker went on sleeping in
            // `daemon.run`. So the process was alive, logged nothing, looked
            // healthy, and was permanently deaf; a head then sat on a socket path
            // with no listener, polling forever, because "not up yet" and "never
            // coming back" are the same thing from out there.
            //
            // Measured on 2026-09-20: pid 1023699, 26 fds and not one socket, no
            // `head-accept` thread, `5a0a8f3c88f8.sock` on disk with nothing behind
            // it. The trigger was a head that died while its connection was still
            // in the backlog — the operator quit the daemon that had spawned it —
            // which is `ECONNABORTED`, the most ordinary accept error there is.
            //
            // The errno was discarded by `Err(_)`, so it could not even be named
            // afterwards. Now: transients are retried, everything else is said out
            // loud, and a loop that really does end takes the daemon with it
            // instead of leaving a deaf process holding a lie.
            let mut transient = 0u32;
            loop {
                if r.is_closed() {
                    break;
                }
                match listener.accept() {
                    Ok((s, _)) => {
                        // **A blocking stream, explicitly.** The listener is non-blocking (see
                        // above); Linux does not pass that flag on to an accepted socket, and
                        // "the platform does not do the surprising thing" is not something a
                        // daemon whose reads are blocking by design should rely on.
                        let _ = s.set_nonblocking(false);
                        transient = 0;
                        if r.is_closed() {
                            break;
                        }
                        let r2 = r.clone();
                        let _ =
                            std::thread::Builder::new()
                                .name("head-conn".into())
                                .spawn(move || {
                                    if let Err(e) = serve_conn(r2, s) {
                                        // A head going away is the normal case and not
                                        // worth a line; anything else is.
                                        if !matches!(e, WireError::Eof) {
                                            eprintln!("head connection ended: {e}");
                                        }
                                    }
                                });
                    }
                    // **Idle, and not an error.** See the `set_nonblocking` above: this is
                    // what makes `shutdown` able to stop a daemon whose socket file is gone,
                    // and it is silent on purpose — the transient arm below prints, and a line
                    // every poll interval from a healthy idle daemon is noise that hides the
                    // lines that matter.
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    Err(e) if is_transient(&e) => {
                        // A client that vanished before it was picked up costs
                        // nothing and is not worth a line. Running out of file
                        // descriptors is worth one, and is worth a pause: spinning
                        // on `EMFILE` burns a core and frees nothing, while the
                        // descriptors that would fix it are being closed by other
                        // threads.
                        transient = transient.saturating_add(1);
                        if e.kind() != io::ErrorKind::ConnectionAborted {
                            eprintln!(
                                "head accept: {e} (transient, retrying; {transient} in a row)"
                            );
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                    }
                    Err(e) => {
                        // Nothing here is recoverable — a listener that is not a
                        // listener any more. Say which errno, because the whole
                        // point of the incident above was that nobody could.
                        eprintln!(
                            "head accept failed and cannot continue: {e} ({:?}). This daemon \
                             can no longer be reached, so it is stopping rather than \
                             running on deaf.",
                            e.kind()
                        );
                        break;
                    }
                }
            }
            // **The socket file goes when the listener does — if it is still ours.**
            //
            // Whatever ended this loop, the path must stop advertising a door: a head gets a
            // refusal it can report instead of an eternal poll. The handle's own `Drop`
            // removes it too, and removing it twice is an ignored `ENOENT` — much cheaper
            // than the case this prevents. `unlink_if_ours` is what keeps "the path" from
            // meaning "whatever is there now", which for a daemon that is on its way out can
            // be a newer daemon's socket.
            if let Some(id) = deaf_id {
                unlink_if_ours(&deaf_path, id);
            }
            // And the daemon comes down with it, gracefully: every head wakes with
            // `Bye`, every worker falls out of `next_command`, and the operator
            // sees a session end instead of a process that answers nothing. A
            // shutdown that got here first has already done this and closing twice
            // is idempotent.
            r.close();
        })?;

    Ok(ServerHandle {
        path,
        registry,
        accept: Some(accept),
        socket_id,
        claim,
    })
}

/// One head's attachment to one session: the hub, the id it has there, and the
/// flag that tells its pump the difference between a shutdown and a move.
struct Seat {
    hub: Arc<Hub>,
    head_id: String,
    /// Set before the reader detaches this head so it can move. The pump wakes with
    /// [`Delivery::Closed`] either way — the hub cannot tell it why — so without
    /// this the head would be sent `Bye` for a switch, and a head that is told the
    /// daemon is going away closes the window.
    switching: Arc<AtomicBool>,
    pump: Option<JoinHandle<()>>,
}

/// One connection, from ATTACH to detach — through however many sessions.
pub fn serve_conn(registry: Arc<Registry>, stream: UnixStream) -> Result<(), WireError> {
    let out = stream.try_clone()?;
    let mut reader = FrameReader::new(stream);
    let writer = Arc::new(Mutex::new(FrameWriter::new(out)));

    let first: ClientFrame = reader.read()?;
    let ClientFrame::Attach {
        protocol_version,
        session_id,
        since_seq,
        kind,
        identity,
        caps,
    } = first
    else {
        let mut w = writer.lock().unwrap();
        w.write(&ServerFrame::Bye {
            reason: "the first frame must be ATTACH".into(),
        })?;
        return Ok(());
    };

    // **A differing protocol version is ACCEPTED, and said — never refused on the
    // number alone.**
    //
    // The operator's ruling, on losing a 17-day-old daemon to a bare `Bye` while a
    // *newer* head stood there unable to read the conversation its warm KV cost
    // minutes to rebuild: *"so ideally it would be like - connect, look around and
    // make informed decision"*. This side of the socket cannot make that decision
    // for the person — it does not know what they came for — but it can do the
    // first two thirds: connect them, and hand over everything it holds. The
    // `Hello` below states this daemon's own `PROTOCOL_VERSION`, so the head can
    // compare and decide; a head with no comparator of its own is still seated,
    // because a conversation readable-and-scrollable beats a closed door.
    //
    // What the old gate protected is still protected, one level up: §17-S6's rule
    // — *"a silent version skew looks like a bug in the other half, forever"* —
    // survives as LOUDNESS rather than as a refusal. This paragraph is the
    // daemon's own record of the skew, on the stderr a person reads when they
    // wonder why a head is cautious; the head's record is the `protocol_skew`
    // sentence its `Hello` comparison produces; and if the two builds later
    // exchange a frame one of them cannot read, the read loop below still answers
    // with a `Bye` naming both versions before the socket goes. Nothing about the
    // skew is silent in any direction — the door is simply open.
    //
    // **This cannot rescue a daemon already running.** A daemon built before this
    // change carries the old gate and goes on refusing until it is restarted; the
    // acceptance here is a fact about every daemon started from this build on.
    if protocol_version != PROTOCOL_VERSION {
        let which = if protocol_version > PROTOCOL_VERSION {
            "newer"
        } else {
            "older"
        };
        eprintln!(
            "head {identity:?} (kind {kind:?}) attached speaking protocol \
             {protocol_version}, {which} than this daemon's {PROTOCOL_VERSION}. Serving \
             it rather than refusing: the skew is for the head to judge and say, and this \
             daemon still answers a frame it cannot read with the loud `Bye` that names \
             both builds."
        );
    }

    let Some(hub) = registry.resolve(&session_id) else {
        let mut w = writer.lock().unwrap();
        let held: Vec<String> = registry.list().into_iter().map(|s| s.session_id).collect();
        w.write(&ServerFrame::Bye {
            reason: if held.is_empty() {
                format!("{REJECT_UNKNOWN_SESSION}: this daemon holds none yet")
            } else {
                format!(
                    "{REJECT_UNKNOWN_SESSION} {session_id:?}; it holds {}",
                    held.join(", ")
                )
            },
        })?;
        return Ok(());
    };

    let mut seat = seat_in(&registry, hub, since_seq, &kind, &identity, &caps, &writer)?;

    // The reader loop. Ends on Detach or on the peer closing — and **TCP close is
    // detach, never abort** (§13.2): nothing below cancels a turn.
    let result = loop {
        match reader.read::<ClientFrame>() {
            Ok(ClientFrame::Ack(ack)) => seat.hub.ack(&seat.head_id, ack),
            Ok(ClientFrame::Resync) => seat
                .hub
                .request_resync(&seat.head_id, "requested by the head"),
            Ok(ClientFrame::Detach) => break Ok(()),
            Ok(ClientFrame::Attach { .. }) => {
                let mut w = writer.lock().unwrap();
                w.write(&ServerFrame::Bye {
                    reason: "already attached; use Switch to change session".into(),
                })?;
                break Ok(());
            }
            Ok(ClientFrame::Askpass { prompt, command }) => {
                // `sudo` in this session wants a password. Raise it, wait here —
                // this connection is the helper's and sends nothing else — and
                // answer on it. The deadline is sudo's patience, roughly: past it
                // the helper exits and sudo reports that no password was given.
                //
                // **The head count is taken BEFORE the card is raised, and it is what
                // makes the failure readable.** `None` on the wire says only that no
                // password came back; whether that is *the card reached nobody* or *a
                // person did not answer* is decided by whether there was anybody to ask.
                const PATIENCE: std::time::Duration = std::time::Duration::from_secs(120);
                let deadline = crate::event::now_ms() + PATIENCE.as_millis() as u64;
                let heads = seat.hub.attached_heads();
                let (req_id, rx) = seat.hub.request_secret(&prompt, &command, deadline);
                let (secret, why) = match rx.recv_timeout(PATIENCE) {
                    // A password: nothing to explain.
                    Ok(Some(s)) => (Some(s), None),
                    // A head answered the card and declined it. That is a decision, and
                    // the helper must not report it as a timeout.
                    Ok(None) => (
                        None,
                        Some("a head was shown the card and the person declined it".to_string()),
                    ),
                    // The deadline — and WHICH silence, which is the fact one sentence
                    // could not carry.
                    Err(_) => {
                        seat.hub
                            .abandon_secret(&req_id, "nobody, before the deadline");
                        (None, Some(no_password_sentence(heads)))
                    }
                };
                writer
                    .lock()
                    .unwrap()
                    .write(&ServerFrame::Secret { secret, why })?;
            }
            Ok(ClientFrame::Secret { req_id, secret }) => {
                // A head's answer. Not a command: it is never queued, announced or
                // logged with its payload. `give_secret` publishes the settlement.
                if !seat.hub.give_secret(&req_id, secret, &identity) {
                    seat.hub.publish(crate::event::SessionEvent::Warning {
                        code: "secret_late".into(),
                        detail: format!(
                            "{}: nothing was waiting on {req_id} — the helper had given up, \
                             or another head answered first",
                            identity
                        ),

                        compaction: None,
                    });
                }
            }
            Ok(ClientFrame::Screen {
                req_id,
                cols,
                rows_n,
                rows,
            }) => {
                // Not a command: it answers a request already in flight, and
                // queueing it behind a running turn would guarantee it arrives
                // after the tool call that asked has given up.
                seat.hub.give_screen(&req_id, cols, rows_n, rows);
            }
            // **The operator answered a command that asked them something.**
            //
            // Off the queue, and this is the sharpest case of it in the file: the run's own
            // thread is **blocked inside the very command that is asking** (it waits on the
            // job; a `!` run has had a thread of its own since this path stopped holding the
            // daemon's worker), and the worker — which serves every session — may be inside
            // another session's turn. A line queued behind either would be drained by neither.
            // See `PROTOCOL_VERSION` 33.
            Ok(ClientFrame::PromptAnswer { req_id, line }) => {
                let session = seat.hub.session_id().to_string();
                match registry.prompt(&session) {
                    None => {
                        seat.hub.publish(crate::event::SessionEvent::Warning {
                            code: "nothing_to_send_to".into(),
                            detail: format!(
                                "{identity}: this daemon has no way to reach a running \
                                 command's stdin, so `{line}` was not sent. Nothing of yours \
                                 is running."
                            ),
                            compaction: None,
                        });
                    }
                    Some(driver) => match driver.send(&session, Some(&req_id), &line) {
                        // A card was up and this settled it. The line is nowhere on the
                        // log — the settlement is the record, exactly as `SecretSettled`
                        // is for a password.
                        Ok(settled) => {
                            if let Some(id) = settled {
                                seat.hub.publish(crate::event::SessionEvent::PromptSettled {
                                    req_id: id,
                                    sent: true,
                                    by: identity.clone(),
                                });
                            }
                        }
                        // **Late, and said rather than refused.** The command ended, or
                        // another head answered first, so nothing was written — the same
                        // shape and the same sentence as `secret_late`, and for its
                        // reason: the person typed an answer and the command did not get
                        // it, which is worth a red line.
                        Err(why) => {
                            seat.hub.publish(crate::event::SessionEvent::Warning {
                                code: "prompt_late".into(),
                                detail: format!("{identity}: {why}"),
                                compaction: None,
                            });
                        }
                    },
                }
            }
            // **One line to the running command, on demand** — `!send`, the manual floor
            // under the card. Off the queue for the reason above, and with no `req_id`: it
            // addresses whatever operator command this session is running right now.
            Ok(ClientFrame::SendLine { line }) => {
                let session = seat.hub.session_id().to_string();
                match registry.prompt(&session) {
                    None => {
                        seat.hub.publish(crate::event::SessionEvent::Warning {
                            code: "nothing_to_send_to".into(),
                            detail: format!(
                                "{identity}: this daemon has no way to reach a running \
                                 command's stdin, so `{line}` was not sent."
                            ),
                            compaction: None,
                        });
                    }
                    Some(driver) => match driver.send(&session, None, &line) {
                        Ok(settled) => {
                            // A card was up and this answered it, so the card comes down
                            // for every head — the same settlement the card's own field
                            // would have produced.
                            if let Some(id) = settled {
                                seat.hub.publish(crate::event::SessionEvent::PromptSettled {
                                    req_id: id,
                                    sent: true,
                                    by: identity.clone(),
                                });
                            }
                        }
                        // **Nothing of yours is running**, or its stdin is gone. A refusal
                        // and not a failure: the act had nothing to act on, which is
                        // `!term` with no pane one verb over.
                        Err(why) => {
                            seat.hub.publish(crate::event::SessionEvent::Warning {
                                code: "nothing_to_send_to".into(),
                                detail: format!("{identity}: {why}"),
                                compaction: None,
                            });
                        }
                    },
                }
            }
            Ok(ClientFrame::ListSessions) => {
                let f = ServerFrame::Sessions {
                    sessions: registry.list(),
                    current: seat.hub.session_id(),
                    created: None,
                };
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::ListJobs) => {
                // Answered here and now, off the registry, rather than queued as a
                // command: `/job` went through the command queue and so arrived
                // after the turn it was asked during. A pane that opens must answer
                // while it is open.
                let f = ServerFrame::Jobs {
                    session_id: seat.hub.session_id().to_string(),
                    jobs: registry.jobs(&seat.hub.session_id()),
                };
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::ListTodos) => {
                // The bootstrap read: the snapshot carries items, not events, so
                // a head attaching fresh has nothing to replay. From here the
                // `TodosUpdated` events carry every change.
                let f = ServerFrame::Todos {
                    session_id: seat.hub.session_id().to_string(),
                    todos: registry.todos(&seat.hub.session_id()),
                };
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::ListMergeQueue) => {
                // The bootstrap read, the way `ListTodos` is: the snapshot carries the whole
                // queue, every state, so a head attaching mid-flight sees the whole queue
                // rather than only later changes. From here the `MergeEntryAdded` and
                // `MergeEntryMoved` events carry every change.
                //
                // Answered here and now, off the registry, rather than queued as a command:
                // a pane that opens must answer while it is open. The queue is daemon-level,
                // so there is no `session_id` — the `session_id` on each entry is the entry's
                // origin, not a filter.
                let f = ServerFrame::MergeQueue {
                    entries: registry.merge_entries(),
                    reviews: registry.merge_reviews(),
                };
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::NewSession {
                client_request_id,
                title,
                workspace,
            }) => {
                // The id is the daemon's to mint: two heads racing to create
                // "scratch" would otherwise get one success and one confusing
                // refusal, and neither of them asked for a name collision.
                let id = mint_session_id(&registry);
                let mut wiring = registry.wiring(&seat.hub.session_id());
                // The head's tree, when it named one. The model, dialect and endpoint
                // stay the daemon's: those are what it is talking to and a head does
                // not get to change them by asking for a session.
                if !workspace.is_empty() {
                    wiring.workspace = workspace;
                }
                let f = match registry.create(&id, title, wiring) {
                    Ok(_) => ServerFrame::Sessions {
                        sessions: registry.list(),
                        current: seat.hub.session_id(),
                        created: Some(id),
                    },
                    Err(e) => ServerFrame::Rejected {
                        client_request_id,
                        reason: e.to_string(),
                        expected_seq: 0,
                        actual_seq: seat.hub.head_seq(),
                    },
                };
                writer.lock().unwrap().write(&f)?;
            }
            // Bring a stored session into this daemon. Idempotent, and refused by
            // name when nothing anywhere has heard of the id: a typo that silently
            // created an empty session named after the typo would look exactly like a
            // resume that found nothing to restore.
            Ok(ClientFrame::ResumeSession {
                client_request_id,
                session_id,
            }) => {
                let f = if registry.get(&session_id).is_some() {
                    ServerFrame::Sessions {
                        sessions: registry.list(),
                        current: seat.hub.session_id(),
                        created: Some(session_id),
                    }
                } else {
                    match registry.resumable(&session_id) {
                        // **R6: an `oc-` id is imported, not resumed.** `oc-` marks an
                        // opencode conversation; the rest is opencode's own id. There is
                        // nothing in the store to restore on the first ask, so the session
                        // is created here and the daemon's open reads the database in. A
                        // *second* ask finds it in the store and takes the resume arm above,
                        // so re-running the same id resumes rather than importing twice.
                        None if session_id.starts_with("oc-") => {
                            match registry.create(&session_id, "", SessionWiring::default()) {
                                Ok(_) => ServerFrame::Sessions {
                                    sessions: registry.list(),
                                    current: seat.hub.session_id(),
                                    created: Some(session_id),
                                },
                                Err(e) => ServerFrame::Rejected {
                                    client_request_id,
                                    reason: e.to_string(),
                                    expected_seq: 0,
                                    actual_seq: seat.hub.head_seq(),
                                },
                            }
                        }
                        None => ServerFrame::Rejected {
                            client_request_id,
                            reason: format!("{REJECT_NOT_IN_STORE}: {session_id:?}"),
                            expected_seq: 0,
                            actual_seq: seat.hub.head_seq(),
                        },
                        Some(brief) => {
                            // The session's own wiring, from the store — not this
                            // connection's. A resumed conversation belongs to the
                            // workspace it was had in, and copying the current
                            // session's would put the wrong root on the picker row
                            // for the whole of its life.
                            match registry.create(&session_id, &brief.title, brief.wiring) {
                                Ok(_) => ServerFrame::Sessions {
                                    sessions: registry.list(),
                                    current: seat.hub.session_id(),
                                    created: Some(session_id),
                                },
                                Err(e) => ServerFrame::Rejected {
                                    client_request_id,
                                    reason: e.to_string(),
                                    expected_seq: 0,
                                    actual_seq: seat.hub.head_seq(),
                                },
                            }
                        }
                    }
                };
                writer.lock().unwrap().write(&f)?;
            }
            // Naming a session the daemon holds. A session that is only in the store
            // is refused here rather than renamed behind the daemon's back: the store
            // is the daemon's to write, and two writers on one row is how a title
            // set in a picker vanishes when the other daemon exits.
            Ok(ClientFrame::RenameSession {
                client_request_id,
                session_id,
                title,
            }) => {
                let f = if registry.get(&session_id).is_none() {
                    ServerFrame::Rejected {
                        client_request_id,
                        reason: format!("{REJECT_UNKNOWN_SESSION} {session_id:?}"),
                        expected_seq: 0,
                        actual_seq: seat.hub.head_seq(),
                    }
                } else {
                    match registry.rename(&session_id, &title) {
                        Err(e) => ServerFrame::Rejected {
                            client_request_id,
                            reason: format!("not renamed: {e}"),
                            expected_seq: 0,
                            actual_seq: seat.hub.head_seq(),
                        },
                        Ok(()) => {
                            // On the renamed session's own log, so that every head
                            // watching *it* is told — not only the head that asked,
                            // and not only the session this connection is sitting in.
                            if let Some(hub) = registry.get(&session_id) {
                                hub.publish(crate::SessionEvent::SessionRenamed {
                                    title: title.clone(),
                                });
                            }
                            ServerFrame::Sessions {
                                sessions: registry.list(),
                                current: seat.hub.session_id(),
                                created: None,
                            }
                        }
                    }
                };
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::Switch {
                session_id,
                since_seq,
            }) => {
                match registry.resolve(&session_id) {
                    None => {
                        // Refused, and the connection **stays where it is**. A
                        // switch that half-happened would leave a head attached to
                        // nothing, which is the one state the mid-turn attach
                        // machinery cannot recover from.
                        let f = ServerFrame::Rejected {
                            client_request_id: format!("switch:{session_id}"),
                            reason: format!("{REJECT_UNKNOWN_SESSION} {session_id:?}"),
                            expected_seq: since_seq,
                            actual_seq: seat.hub.head_seq(),
                        };
                        writer.lock().unwrap().write(&f)?;
                    }
                    Some(next) if next.session_id() == seat.hub.session_id() => {
                        // Already here. Answered rather than ignored: "I pressed the
                        // key and nothing happened" is the report this avoids.
                        let f = ServerFrame::Sessions {
                            sessions: registry.list(),
                            current: seat.hub.session_id(),
                            created: None,
                        };
                        writer.lock().unwrap().write(&f)?;
                    }
                    Some(next) => {
                        leave(&mut seat);
                        registry.set_default(&next.session_id());
                        seat =
                            seat_in(&registry, next, since_seq, &kind, &identity, &caps, &writer)?;
                    }
                }
            }
            // **The operator's own call, admitted before it runs** — R24 part two,
            // decision 4.
            //
            // **The allowlist is enforced HERE, on the connection's thread**, and that is
            // the whole reason it is a constant in this crate rather than a field on the
            // head. A head is going to run the thing either way — it is in the operator's
            // own terminal — so what the daemon owns is *what may be recorded as part of
            // the conversation*. A daemon that trusted the name it was sent would bound
            // nothing.
            //
            // **Refused in a sentence.** A `Rejected` naming the list, not a dropped
            // frame: a head that cannot say why is a head that retries, and the operator
            // is the one who would have to guess.
            Ok(ClientFrame::OperatorCall {
                client_request_id,
                expected_seq,
                call_id,
                name,
                arguments,
                execute,
            }) => {
                if !HEAD_RUN_TOOLS.contains(&name.as_str()) {
                    let f = ServerFrame::Rejected {
                        client_request_id,
                        reason: format!(
                            "`{name}` is not a call this door runs for the operator. It records \
                             what was run, and the names it accepts are {}. `bash` and `write` \
                             behind a composer's chord would put a shell one keystroke from \
                             where the operator is typing, and a corpus row could no longer \
                             say whether that was the guard's answer, the operator's act, or \
                             a shell. Nothing ran.",
                            HEAD_RUN_TOOLS.join(", ")
                        ),
                        expected_seq,
                        actual_seq: seat.hub.head_seq(),
                    };
                    writer.lock().unwrap().write(&f)?;
                } else {
                    // The identity, not the head id: the gate records `human:<who>`, and
                    // the row's `CallOrigin` carries the same string, so the two records
                    // name the actor the same way.
                    let who = seat
                        .hub
                        .identity_of(&seat.head_id)
                        .unwrap_or_else(|| seat.head_id.clone());
                    let f = seat.hub.submit(
                        &seat.head_id,
                        client_request_id,
                        expected_seq,
                        CommandKind::OperatorCall {
                            call_id,
                            name,
                            arguments,
                            who,
                            execute,
                        },
                    );
                    writer.lock().unwrap().write(&f)?;
                }
            }
            // **What the operator's call produced.**
            //
            // No `expected_seq`: this frame does not move the session, it hands over a fact
            // the session is missing. A `call_id` this daemon is not holding is refused by
            // name rather than appended — see `CommandKind::OperatorResult`'s own handling,
            // which is where the pending set lives.
            Ok(ClientFrame::OperatorResult {
                call_id,
                outcome,
                payload,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    String::new(),
                    0,
                    CommandKind::OperatorResult {
                        call_id,
                        outcome,
                        payload,
                    },
                );
                writer.lock().unwrap().write(&f)?;
            }
            // **The operator's own shell line — a `!` command the DAEMON runs.**
            //
            // No allowlist to check, because there is no tool name: the line owns itself, and
            // the frame's own docs record why the door was not widened instead. What IS
            // checked here is the bang — the one fact that separates *the operator's shell
            // line* from *a sentence this frame could otherwise file as one* — and it is
            // checked on the connection's thread for the same reason the door's list is: a
            // daemon that trusted the line it was sent would write rows nothing stands behind.
            //
            // **No gate, no adjudication row, no `OperatorCallAllowed`.** The operator typed
            // the line; there was nobody left to ask, and saying otherwise would be a decision
            // nobody made. The gate is also structurally out of reach: the run goes through
            // `ToolRuntime::invoke_operator`, which is the door's own ungated path.
            Ok(ClientFrame::OperatorShell {
                client_request_id,
                expected_seq,
                line,
            }) => {
                if crate::operator_shell_command(&line).is_none() {
                    let f = ServerFrame::Rejected {
                        client_request_id,
                        reason: format!(
                            "`{line}` is not a `!` command: the bang has to be the first \
                             character and something has to follow it. A line that does not \
                             start with `!` is a prompt. Nothing ran."
                        ),
                        expected_seq,
                        actual_seq: seat.hub.head_seq(),
                    };
                    writer.lock().unwrap().write(&f)?;
                } else {
                    // The identity, not the head id — the row's `CallOrigin` and the door's
                    // records name the actor the same way.
                    let who = seat
                        .hub
                        .identity_of(&seat.head_id)
                        .unwrap_or_else(|| seat.head_id.clone());
                    let f = seat.hub.submit(
                        &seat.head_id,
                        client_request_id,
                        expected_seq,
                        CommandKind::OperatorShell { line, who },
                    );
                    writer.lock().unwrap().write(&f)?;
                }
            }
            // **The model's half of the `!` completion.** The history is the head's own and is
            // the first answer; this is what the head asks for when the history has no match
            // for the prefix. The daemon builds the prompt from the session's own rows and
            // asks the LOCAL model — the suggester the daemon installed, or nothing when it
            // installed none.
            //
            // **Answered here and now, off the queue, for the reason `ListJobs` is**: a
            // completion must arrive while the operator is still typing, and queueing it
            // behind a running turn would guarantee it arrives after the line is sent. The
            // call is bounded by the suggester (a small output cap and a timeout), and a
            // suggestion that does not arrive is nothing: the answer is an empty list, never
            // a wait.
            //
            // **Nothing is submitted.** The answer is a list of candidate lines; the head
            // draws them as candidates, marked as the model's, and only a Tab fills the
            // composer with one. Enter is still the operator's.
            Ok(ClientFrame::SuggestShell {
                client_request_id,
                expected_seq: _,
                prefix,
            }) => {
                let lines = match registry.suggester() {
                    Some(s) => s.suggest(
                        &seat.hub,
                        &registry.wiring(&seat.hub.session_id()).workspace,
                        &prefix,
                    ),
                    None => Vec::new(),
                };
                let f = ServerFrame::ShellSuggestions {
                    client_request_id,
                    prefix,
                    lines,
                };
                writer.lock().unwrap().write(&f)?;
            }
            // **A program that owns the screen, in a pane the daemon owns.** (`!term`.)
            //
            // Not a command, and the sharpest case of it in this file: this appends no row,
            // moves no seq and is answered on this connection. Queueing a pane behind a
            // running turn would make it open minutes after it was asked for.
            //
            // **Every refusal is a `TermEnded`, and there is no second spelling.** A pane
            // that could not start is a pane that is over before it began, and the head's act
            // is the same either way — close the pane and say why — so the sentence is the
            // whole of the difference. `Rejected` would need a `client_request_id` and an
            // `expected_seq`, and this frame has neither: it is not a command and it has
            // nothing to be stale against.
            Ok(ClientFrame::TermOpen { line, cols, rows }) => {
                let said = match crate::term_line(&line) {
                    // Not this verb at all. A head that sent one is a head this daemon does
                    // not understand, so it is told rather than ignored.
                    None => Err(format!(
                        "`{line}` is not a `!term` line: the verb is `!term`, and it has to be \
                         followed by whitespace. Nothing ran."
                    )),
                    // **The verb with no command is ATTACH** — see `TerminalDriver::attach`.
                    // It was a refusal (*"`!term` needs a command to run"*), and the refusal
                    // was wrong for the reason the operator hit: the pane is the session's,
                    // so a person whose head lost the rectangle still has a program running
                    // and nothing to leave. A session with no pane answers with a sentence
                    // through the same `TermEnded` every other unstartable pane uses.
                    Some(crate::TermLine::Attach) => match registry.terminal() {
                        None => Err("this daemon has no terminal driver, so there is no pane \
                                     to attach to. Nothing was attached."
                            .to_string()),
                        Some(driver) => {
                            driver.attach(&seat.hub.session_id(), &seat.hub, cols, rows)
                        }
                    },
                    // **`!term close` is the ending, and it is the head's act.** The head
                    // asks the operator to confirm it and then sends `TermClose` — see
                    // `PROTOCOL_VERSION`'s 34 section — so a `TermOpen` carrying this line is
                    // a head that did not do that, and running a program called `close`
                    // instead would be this daemon and the head disagreeing about what the
                    // same bytes mean. The sentence names the way to run such a program,
                    // because that is the cost of the word and it is paid in the open.
                    Some(crate::TermLine::Close) => Err(
                        "`!term close` is not a command to run: it is how a pane is ENDED, and \
                         the head sends that as its own act (after asking). Nothing ran — a \
                         program called `close` is `!term command close`."
                            .to_string(),
                    ),
                    Some(crate::TermLine::Run(command)) => match registry.terminal() {
                        None => Err("this daemon has no terminal driver, so there is no pty to \
                                     run a screen program on. Nothing ran."
                            .to_string()),
                        Some(driver) => {
                            driver.open(&seat.hub.session_id(), &seat.hub, command, cols, rows)
                        }
                    },
                };
                if let Err(reason) = said {
                    writer
                        .lock()
                        .unwrap()
                        .write(&ServerFrame::TermEnded { reason })?;
                }
            }
            // **The operator's keys, verbatim.** Not a command either, and for a sharper
            // reason than the pane's: a keystroke that queued behind a running turn would be
            // a key that arrives after the thing it was answering.
            Ok(ClientFrame::TermInput { bytes }) => {
                if let Some(driver) = registry.terminal() {
                    // A failed write is the pty's far end being gone, which the pane's own
                    // reader thread is about to report with a better sentence than this arm
                    // could — so it is dropped here rather than turned into a second ending.
                    let _ = driver.input(&seat.hub.session_id(), &bytes);
                }
            }
            // **The pane's rectangle moved** — the head's fact, since the daemon has no
            // screen. `TIOCSWINSZ` on the pty, and the kernel raises `SIGWINCH` for the
            // program's foreground process group by itself.
            Ok(ClientFrame::TermResize { cols, rows }) => {
                if let Some(driver) = registry.terminal() {
                    let _ = driver.resize(&seat.hub.session_id(), cols, rows);
                }
            }
            // **The operator left.** Idempotent and quiet when there is no pane: *"stop"* is
            // not a request that can be wrong about anything, and a head that leaves twice
            // gets nothing rather than a refusal. The ending itself comes from the driver's
            // reader thread, with the operator's own act in the sentence — see
            // `TermSession::close`.
            Ok(ClientFrame::TermClose) => {
                if let Some(driver) = registry.terminal() {
                    let _ = driver.close(&seat.hub.session_id());
                }
            }
            // **What this session's pane is running, or nothing.** A read, answered on this
            // connection like every other pane frame: a question about a live program queued
            // behind a running turn would be an answer about the past.
            //
            // **It is what a head draws instead of a row.** A head that has detached, or
            // switched session, still has to say *something is running in here* — and the
            // operator's rule is that a detach is not an event, so there is no row to file.
            // The `None` arm is not an error: a session nobody ran `!term` in has no pane,
            // and the head draws nothing at all.
            //
            // A daemon with no terminal driver answers `None` rather than refusing: *there is
            // no pane* is the true statement, and it is the same one a driver with no pane for
            // this session gives.
            Ok(ClientFrame::TermStatus) => {
                let command = registry
                    .terminal()
                    .and_then(|driver| driver.status(&seat.hub.session_id()));
                writer
                    .lock()
                    .unwrap()
                    .write(&ServerFrame::TermStatus { command })?;
            }
            // **R11's locator, leticl's ask.** A head names one decision and one half of its
            // exchange; the daemon answers with the bytes or with *not recorded*. The same
            // shape `FetchRow` uses, and for the same reason: this is the head asking for
            // something big it does not normally hold.
            //
            // **Answered even when there is nothing**, and that is the point of
            // `body: Option`: an empty `body` and a `body` of `""` are different facts, and a
            // head that could not tell them apart would draw "the oracle said nothing" over
            // "nobody kept this".
            Ok(ClientFrame::FetchDiagnostic { request_id, kind }) => {
                let body = registry.diagnostic(&request_id, kind);
                let total = body.as_ref().map(|b| b.len()).unwrap_or(0);
                let f = ServerFrame::Diagnostic {
                    request_id,
                    kind,
                    body,
                    total,
                };
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::Settings) => {
                let f = ServerFrame::Settings {
                    rows: registry.settings(&seat.hub.session_id()),
                };
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::Peek { session_id, shape }) => {
                // A read, not a move: the seat, its acks and its live events are
                // untouched, and the answer is the named session's scrollback
                // scrubbed exactly as a replay would be — a peek IS a replay, so
                // it gets the replay's wire hygiene and the ring's cap. A session
                // this daemon does not hold is a Rejected naming it, never an
                // empty Peeked: an empty answer and a missing session must not
                // look alike.
                //
                // **`PeekShape::Rows` answers with the session's own view instead**, and it is
                // the same read rather than a second one: a child IS a session — the operator,
                // 2026-10-03: *"yes subagents are not even scratch session they are session, just
                // sub sessions"* — so the rows an attach would be given are the rows this returns.
                // **The two shapes are alternatives and not a pair**: a head that asked for rows
                // gets the snapshot and an EMPTY ring, because sending both would put the same
                // session on the wire twice and leave a head free to draw it twice.
                //
                // **No scrub on the rows**, and the asymmetry is the point: `scrub_replay`
                // exists because a ring of *events* carries interactive-only frames a late reader
                // must not act on, and a `Snapshot` is built from the view — which is already the
                // durable half of that stream. Scrub what is a replay; a view is not one.
                match registry.resolve(&session_id) {
                    Some(hub) => {
                        let (events, snapshot) = match shape {
                            PeekShape::Rows => (Vec::new(), Some(Box::new(hub.snapshot()))),
                            PeekShape::Events => {
                                let retained = hub.retained();
                                let (kept, _) =
                                    crate::scrub::scrub_replay(retained.iter(), &retained);
                                (kept, None)
                            }
                        };
                        let f = ServerFrame::Peeked {
                            session_id: hub.session_id(),
                            dropped: hub.dropped(),
                            events,
                            snapshot,
                        };
                        writer.lock().unwrap().write(&f)?;
                    }
                    None => {
                        let f = ServerFrame::Rejected {
                            client_request_id: format!("peek:{session_id}"),
                            reason: format!("{REJECT_UNKNOWN_SESSION} {session_id:?}"),
                            expected_seq: 0,
                            actual_seq: seat.hub.head_seq(),
                        };
                        writer.lock().unwrap().write(&f)?;
                    }
                }
            }
            Ok(ClientFrame::FetchRow {
                session_id,
                row,
                at,
                len,
            }) => {
                // A read, not a move — the same contract `Peek` above keeps, and the
                // reason both live in this arm of the match rather than in the seat.
                //
                // **The window is clamped here, not trusted from the head.** `len` is what
                // the head *asked* for; one request must not be able to return a megabyte
                // because a head sent a big number, so the cap is the daemon's. And `at`
                // is clamped to the body rather than refused, because a head paging
                // forward does not know where the end is and asking past it is how it
                // finds out.
                let Some(hub) = registry.resolve(&session_id) else {
                    let f = ServerFrame::Rejected {
                        client_request_id: format!("fetchrow:{session_id}/{row}"),
                        reason: format!("{REJECT_UNKNOWN_SESSION} {session_id:?}"),
                        expected_seq: 0,
                        actual_seq: seat.hub.head_seq(),
                    };
                    writer.lock().unwrap().write(&f)?;
                    continue;
                };
                // **The view first, then the store.** The view is one lock and a `Vec`
                // index, so it is the fast path for a row this daemon is holding; the
                // store is the answer for an ordinal its bounded view has trimmed
                // (2,000 rows, 8 MB of bodies), which is the case `FetchRow` exists for.
                // A registry with no store answers `None` here and behaves exactly as it
                // always did. See [`letibot_sessionlog::registry::RowSource`].
                let full = hub
                    .row_body_at(row)
                    .or_else(|| registry.row_body_from_store(&session_id, row));
                let (body, total, at) = match full {
                    Some(full) => {
                        let total = full.len();
                        let start = clamp_char(&full, at.min(total));
                        let want = len.min(MAX_FETCH_ROW);
                        let end = clamp_char(&full, (start + want).min(total));
                        (Some(full[start..end].to_string()), total, start)
                    }
                    // Nowhere holds it: past the end of the session, or a store this
                    // daemon cannot reach. `None` rather than an empty string, because
                    // "nobody has it" and "it is empty" must not look alike.
                    None => (None, 0, 0),
                };
                let f = ServerFrame::RowFetched {
                    session_id,
                    row,
                    at,
                    body,
                    total,
                };
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::Prompt {
                client_request_id,
                expected_seq,
                text,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::Prompt { text },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::SetOperatorTodos {
                client_request_id,
                expected_seq,
                items,
            }) => {
                // **Not gated and not a decision.** The operator's own todo list is their own
                // authoring, sent to the daemon that keeps the board — the same trust as a prompt.
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::SetOperatorTodos { items },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::WithdrawPrompts {
                client_request_id,
                expected_seq,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::WithdrawPrompts,
                );
                writer.lock().unwrap().write(&f)?;
            }
            // **The stop does NOT go through the command queue**, and that was
            // the whole of its first version's failure. One worker drains that
            // queue and a running turn owns it, so a `Stop` submitted mid-turn
            // sat behind the turn doing nothing — the operator, 2026-09-17:
            // *"i stopped mid turn and harness kept it running"*. Right, and
            // the signal path never had that problem: `catch_signals` closes
            // the REGISTRY from its own thread, and the worker falls out of
            // `next_command` wherever it happens to be.
            //
            // So this rings the same bell from this connection's thread. The
            // announcement goes first, while the hubs are still open and every
            // other head can still be reached; then the registry closes and
            // that is the stop.
            Ok(ClientFrame::Stop {
                client_request_id,
                expected_seq: _,
                who,
            }) => {
                for brief in registry.list() {
                    if let Some(hub) = registry.get(&brief.session_id) {
                        hub.publish(crate::event::SessionEvent::Warning {
                            code: "daemon_stopping".into(),
                            detail: format!(
                                "`{who}` asked this daemon to stop. Every head detaches, \
                                 the socket goes, and the session is on disk — `letibot \
                                 --continue` reopens it. A turn already generating \
                                 finishes its round; nothing new is started."
                            ),

                            compaction: None,
                        });
                    }
                }
                // Acked before the close, because after it there is no socket to
                // ack on and a head waiting for one would wait forever.
                let f = crate::protocol::ServerFrame::Accepted {
                    client_request_id,
                    seq: seat.hub.head_seq(),
                    note: crate::protocol::NOTE_STOPPING.into(),
                };
                writer.lock().unwrap().write(&f)?;
                registry.close();
                return Ok(());
            }
            Ok(ClientFrame::Interrupt {
                client_request_id,
                expected_seq,
                reason,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::Interrupt { reason },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::Promote {
                client_request_id,
                expected_seq,
            }) => {
                // Two halves: the flag the `bash` wait loop honours mid-turn, and the
                // queued command that announces the between-turns case. The flag has
                // to be set here — the waiter (a turn's bash wait, or the operator's
                // own run on its thread) is blocked inside the wait it would
                // otherwise be asked to deliver this to.
                seat.hub.request_promote_from(&seat.head_id);
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::Promote,
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::CompactSession {
                client_request_id,
                expected_seq,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::Compact,
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::ReseatSession {
                client_request_id,
                expected_seq,
                summarise,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::Reseat { summarise },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::Mode {
                client_request_id,
                expected_seq,
                name,
                consented,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::Mode { name, consented },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::ReadJobOutput {
                client_request_id,
                job,
                offset,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    seat.hub.head_seq(),
                    CommandKind::ReadJobOutput { job, offset },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::Slash {
                client_request_id,
                expected_seq,
                line,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::Slash { line },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::Answer {
                client_request_id,
                req_id,
                option_id,
                pattern,
                note,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    0,
                    CommandKind::Answer {
                        req_id,
                        reply: Reply::Permission {
                            option_id,
                            pattern,
                            note,
                        },
                    },
                );
                writer.lock().unwrap().write(&f)?;
            }
            // A question's answer, not a permission's (`PROTOCOL_VERSION` 5). Same
            // command, same `req_id`, different payload — and `Hub::submit` refuses a
            // malformed one by name rather than letting it settle the question.
            Ok(ClientFrame::AnswerQuestion {
                client_request_id,
                req_id,
                answer,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    0,
                    CommandKind::Answer {
                        req_id,
                        reply: Reply::Question(answer),
                    },
                );
                writer.lock().unwrap().write(&f)?;
            }
            // A head going away is the normal case and says nothing.
            Err(WireError::Eof) => break Ok(()),
            // **Anything else is said before the connection goes.**
            //
            // This read `break Err(e)` and hung up in silence, which is how a
            // version skew presented: the head sent a frame this daemon had never
            // heard of, the deserialize failed here, the socket closed, and the
            // head exited with `the daemon closed the connection` — true, useless,
            // and identical to a crash. Measured 2026-09-20: `ReadJobOutput` from
            // a new head to a daemon started twenty-eight minutes before it landed
            // killed the head on every Enter, with nothing on either screen naming
            // a version.
            //
            // §17-S6's rule is already written down for the ATTACH check a few
            // hundred lines up — *"a silent version skew looks like a bug in the
            // other half, forever"* — and it is no less true after ATTACH than
            // during it. A frame this daemon cannot read is almost always a head
            // from the future, so the reason says both numbers and what to do.
            // Best effort: the socket may already be gone, which is why the write
            // is not `?`.
            Err(e) => {
                let reason = format!(
                    "this connection sent a frame this daemon could not read ({e}). \
                     This daemon speaks protocol {PROTOCOL_VERSION}; a head built \
                     against a newer one will do this on the first frame the two do \
                     not share. Restart the daemon so both halves are the same build.",
                );
                if let Ok(mut w) = writer.lock() {
                    let _ = w.write(&ServerFrame::Bye { reason });
                }
                break Err(e);
            }
        }
    };

    seat.hub.detach(&seat.head_id);
    if let Some(p) = seat.pump.take() {
        let _ = p.join();
    }
    result
}

/// Attach to `hub`, send the `Hello`, hand over the backlog and start the pump.
///
/// The whole of "you are now in this session", in one function, so that the
/// first attach and a `Switch` cannot drift: a switch that forgot the backlog would
/// lose exactly the events between the snapshot and the first pumped one, which is
/// the gap §13.2 spends a page closing.
fn seat_in(
    registry: &Arc<Registry>,
    hub: Arc<Hub>,
    since_seq: u64,
    kind: &str,
    identity: &str,
    caps: &crate::protocol::Caps,
    writer: &Arc<Mutex<FrameWriter<UnixStream>>>,
) -> Result<Seat, WireError> {
    let a = hub.attach(kind, identity, caps.clone(), since_seq);
    let head_id = a.head_id.clone();
    let session_id = hub.session_id();
    {
        let mut w = writer.lock().unwrap();
        w.write(&ServerFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            session_id: session_id.clone(),
            head_id: head_id.clone(),
            dropped: a.dropped,
            snapshot: a.snapshot.map(Box::new),
            resumed_from: a.resumed_from,
            scrubbed: a.scrubbed,
            wiring: registry.wiring(&session_id),
            sessions: registry.list(),
        })?;
        for env in &a.backlog {
            w.write(&ServerFrame::Event(env.clone()))?;
        }
    }

    let switching = Arc::new(AtomicBool::new(false));
    let pump = {
        let hub = hub.clone();
        let writer = writer.clone();
        let head_id = head_id.clone();
        let switching = switching.clone();
        std::thread::Builder::new()
            .name("head-pump".into())
            .spawn(move || {
                loop {
                    let d = hub.next_batch(&head_id, 256);
                    let mut w = match writer.lock() {
                        Ok(w) => w,
                        Err(p) => p.into_inner(),
                    };
                    let r = match d {
                        Delivery::Frames(frames) => {
                            // **Frames that are not the record**: the pane's byte stream.
                            // See [`Delivery::Frames`] — they are written ahead of the
                            // events because a screen's repaint must not sit behind a batch
                            // of deltas, and none of them touches the log, the seq or the
                            // ack.
                            let mut r = Ok(());
                            for f in frames {
                                r = w.write(&f);
                                if r.is_err() {
                                    break;
                                }
                            }
                            r
                        }
                        Delivery::Events(b) => {
                            let mut r = Ok(());
                            for env in b.events() {
                                r = w.write(&ServerFrame::Event(env.clone()));
                                if r.is_err() {
                                    break;
                                }
                            }
                            r
                        }
                        Delivery::Resync {
                            reason,
                            dropped,
                            snapshot,
                            scrubbed,
                        } => w.write(&ServerFrame::Resync {
                            reason,
                            dropped,
                            snapshot,
                            scrubbed,
                        }),
                        Delivery::Closed => {
                            // Two very different things arrive here: the daemon is
                            // going away, or this head has left for another
                            // session. Only the first is a `Bye`.
                            if !switching.load(Ordering::SeqCst) {
                                let _ = w.write(&ServerFrame::Bye {
                                    reason: "daemon shutting down".into(),
                                });
                            }
                            return;
                        }
                    };
                    if r.is_err() {
                        return;
                    }
                }
            })
            .ok()
    };

    Ok(Seat {
        hub,
        head_id,
        switching,
        pump,
    })
}

/// Detach from the session this seat is in, and stop its pump, without telling the
/// head the daemon is going away.
fn leave(seat: &mut Seat) {
    seat.switching.store(true, Ordering::SeqCst);
    // `detach` is what wakes the pump: `next_batch` returns `Closed` for a head id
    // that is no longer registered. Joining before attaching elsewhere is what
    // keeps the frames on this socket in one order — two pumps writing through one
    // mutex would interleave the old session's tail into the new one's `Hello`.
    seat.hub.detach(&seat.head_id);
    if let Some(p) = seat.pump.take() {
        let _ = p.join();
    }
}

/// A new session id, unique in this registry.
///
/// Monotonic wall-clock nanos, which is what `harnessd` already mints for its
/// first session, with a counter behind it for the case two heads ask in the same
/// nanosecond.
fn mint_session_id(registry: &Arc<Registry>) -> String {
    let base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut id = format!("s-{base}");
    let mut n = 0;
    while registry.get(&id).is_some() {
        n += 1;
        id = format!("s-{base}-{n}");
    }
    id
}

#[cfg(test)]
mod accept_tests {
    use super::*;

    /// **The errnos that must not take a daemon's ears off.**
    ///
    /// This loop read `Err(_) => break` and a daemon paid for it: pid 1023699 on
    /// 2026-09-20, alive with no `head-accept` thread, no socket among its 26 fds,
    /// and a socket file on disk with nothing behind it. The trigger was a head
    /// that died while its connection was still in the backlog — `ECONNABORTED`,
    /// the most ordinary accept error there is.
    ///
    /// Asserted as a classification rather than through a real `accept`, because
    /// making a listener return `EMFILE` on demand means exhausting the process's
    /// descriptors and that is a test which breaks whatever runs beside it.
    #[test]
    fn an_aborted_client_and_a_full_table_are_both_retryable() {
        for (kind, why) in [
            (io::ErrorKind::ConnectionAborted, "a client that went away"),
            (io::ErrorKind::Interrupted, "a signal"),
            (io::ErrorKind::WouldBlock, "nothing ready yet"),
        ] {
            assert!(
                is_transient(&io::Error::new(kind, "x")),
                "{kind:?} is {why} and must be retried"
            );
        }
        for errno in [EMFILE, ENFILE] {
            assert!(
                is_transient(&io::Error::from_raw_os_error(errno)),
                "errno {errno} is descriptors, which other threads are already freeing"
            );
        }
    }

    /// And the other half: a listener that is genuinely broken must NOT be
    /// retried, or the loop spins forever on an fd that will never accept again.
    /// `EBADF` is that case, and it is the one the retry must not swallow.
    #[test]
    fn a_broken_listener_is_not_retryable() {
        const EBADF: i32 = 9;
        assert!(!is_transient(&io::Error::from_raw_os_error(EBADF)));
        assert!(!is_transient(&io::Error::new(
            io::ErrorKind::PermissionDenied,
            "x"
        )));
    }
}
