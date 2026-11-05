//! The head server: a Unix socket per folder, mode 0600, filesystem permissions as the whole
//! auth. One connection per head; every frame answers one connection or is fanned out by the
//! hub. The protocol version is compared here before a head is seated.

use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::SessionEvent;
use crate::hub::{CommandKind, Delivery, Hub, Reply};
use crate::origin::SessionOrigin;
use crate::protocol::{
    ClientFrame, HEAD_RUN_TOOLS, PROTOCOL_VERSION, PeekShape, REJECT_NOT_IN_STORE,
    REJECT_UNKNOWN_SESSION, ServerFrame,
};
use crate::registry::{Registry, SessionWiring};
use crate::wire::{FrameReader, FrameWriter, WireError};

/// Answer a `Settings` request: `rows` is called under the lock that writes the frame. The
/// pump and this answer both write rows, so a read outside the lock could land after a newer
/// row set and leave the head on old rows.
fn answer_settings<W: std::io::Write, F: FnOnce() -> Vec<crate::protocol::SettingRow>>(
    writer: &Mutex<FrameWriter<W>>,
    rows: F,
) -> Result<(), WireError> {
    let mut w = writer.lock().unwrap();
    w.write(&ServerFrame::Settings { rows: rows() })
}

/// The default socket path. Falls back to `/tmp` when `XDG_RUNTIME_DIR` is unset, which
/// is the case in a bare test environment.
pub fn default_socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d).join("harnessd.sock"),
        None => PathBuf::from(format!("/tmp/harnessd-{}.sock", libc_getuid())),
    }
}

fn libc_getuid() -> u32 {
    // No `libc` dependency in a crate that otherwise has none: `getuid` is in the C library
    // every target links, and declaring it works on Linux and macOS alike. The uid only
    // keeps two users' `/tmp` fallback sockets apart.
    // SAFETY: the declaration matches the C library's `uid_t getuid(void)`.
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid cannot fail.
    unsafe { getuid() }
}

/// A running server. Dropping it does not stop the session: see
/// [`ServerHandle::shutdown`].
pub struct ServerHandle {
    path: PathBuf,
    registry: Arc<Registry>,
    accept: Option<JoinHandle<()>>,
    /// (dev, ino) of the socket this daemon bound, so going away removes its own file and
    /// never whatever is at that path by then. See [`unlink_if_ours`].
    socket_id: Option<(u64, u64)>,
    /// The claim on this folder, held for the process's life. Dropped LAST (declaration
    /// order), after the socket file is gone, so no daemon can slip in between.
    claim: Option<FolderClaim>,
}

/// The lock that says a folder has a daemon, because a filename cannot: a stopping daemon
/// unlinks the socket file while a new one binds, so the file is not the fact.
struct FolderClaim {
    /// Held, never read: the lock lives in this open file description and dropping it
    /// releases the claim. The `_` prefix tells the compiler the absent reader is the design.
    _file: std::fs::File,
    path: PathBuf,
}

impl FolderClaim {
    /// The lock file that belongs beside a socket: `$KEY.sock` -> `$KEY.lock`.
    fn path_for(socket: &Path) -> PathBuf {
        socket.with_extension("lock")
    }

    /// Take the claim, or say who has it.
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
            // Not a held lock but a filesystem that cannot lock at all: said out loud and
            // fallen back on, because refusing to start a daemon is worse than failing to
            // catch a second one, and silence here is the failure mode itself.
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(ClaimRefusal::Unavailable { why: e.to_string() });
            }
        }
        // Our pid inside it so a refusal can name somebody. Not the source of truth - the
        // lock is - but a refusal that only says a daemon is live is one nobody can act on.
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

    /// The pid the holder wrote, or `None` when the file is empty - a holder that took the
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

/// Why a claim was not taken: somebody else is serving this folder, or the filesystem
/// cannot hold the claim at all.
enum ClaimRefusal {
    /// Somebody else is serving this folder, and here is who.
    Live { pid: Option<u32> },
    /// This filesystem cannot hold the claim (no writable directory, an exotic mount). The
    /// caller falls back to the socket-file probe.
    Unavailable { why: String },
}

/// Remove a socket file only if the path still names OUR socket. The path is shared state, so
/// a plain removal deletes the socket of the daemon that replaced this one.
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

    /// Where this daemon's claim lives, for a caller that wants to say so - the launcher
    /// names it in the refusal it prints when a socket file has gone missing under a live
    /// daemon, which is the state a person has to clean up by hand.
    pub fn claim_path(&self) -> Option<&Path> {
        self.claim.as_ref().map(FolderClaim::path)
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// The session a bare attach lands on. `None` for a registry with nothing in it, which is
    /// a daemon that has not opened one yet - not an error, and not a reason to invent one.
    pub fn default_hub(&self) -> Option<Arc<Hub>> {
        self.registry.resolve("")
    }

    /// Stop accepting, wake every head with `Closed`, remove the socket.
    ///
    /// A daemon shutdown is an abort, as against a head detaching, which is not.
    pub fn shutdown(mut self) {
        self.registry.close();
        // Unblock the accept loop by connecting to it once.
        let _ = UnixStream::connect(&self.path);
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
        // Our own file, and only ours - see [`unlink_if_ours`]. The accept thread removes it
        // when it ends too, and removing it twice is an ignored `ENOENT`.
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
        // Best effort only. An abandoned handle leaves the socket, which is recoverable; taking
        // the session down on a drop would not be.
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

/// Can this `accept` error be retried? `ConnectionAborted` is a client that went away between
/// `connect` and `accept`, which is ordinary.
fn is_transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
    ) || matches!(e.raw_os_error(), Some(EMFILE) | Some(ENFILE))
}

/// The sentence the daemon puts on the frame the `askpass` helper reads, saying nobody gave a
/// password, and why not.
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
mod settings_answer_tests {
    use super::*;

    /// The rows are read under the lock that writes them. The pump and this answer both reach one
    /// socket, so a read outside the lock could be written after a newer row set. The seam is the
    /// closure, so the order is assertable without a race.
    #[test]
    fn a_settings_answer_reads_its_rows_under_the_lock_that_writes_them() {
        let writer = Mutex::new(FrameWriter::new(Vec::<u8>::new()));
        let read = || {
            assert!(
                writer.try_lock().is_err(),
                "the rows were read with the writer unlocked, so a pushed frame could land \
                 between the read and the write and the head would keep the old rows"
            );
            Vec::new()
        };
        answer_settings(&writer, read).expect("the answer is written");
    }
}

#[cfg(test)]
mod no_password_tests {
    use super::no_password_sentence;

    /// The two silences are two sentences, and neither claims a person refused: no head attached
    /// is a bug, and a head that closed the card is a refusal.
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

/// Bind and start accepting for a single session: most of the workspace has one hub and does
/// not want to know about a registry.
pub fn serve(hub: Arc<Hub>, path: impl AsRef<Path>) -> io::Result<ServerHandle> {
    serve_registry(Registry::of(hub), path)
}

/// The most bytes one `FetchRow` may return, whatever the head asked for: the cap is the
/// daemon's, because the memory is the daemon's.
pub const MAX_FETCH_ROW: usize = 64 * 1024;

/// The most rows one `FetchRows` may answer, whatever the head asked for: a page is a screen or
/// two of scrollback, and the cap is the daemon's because the rows are read in the daemon.
pub const MAX_FETCH_ROWS: usize = 256;

/// The most bytes of row text one `FetchRows` may answer, whatever the head asked for - the same
/// reason [`MAX_FETCH_ROW`] has one, one frame up: a page of 256 rows of a megabyte each is a
/// quarter-gigabyte frame, and the memory it is built in is the daemon's.
pub const MAX_FETCH_BYTES: usize = 1024 * 1024;

/// The nearest character boundary at or below `at`, for slicing a body: a byte window can land
/// inside a multi-byte character.
fn clamp_char(s: &str, at: usize) -> usize {
    let mut at = at.min(s.len());
    while at > 0 && !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Bind and start accepting for every session in `registry`. The daemon is this process, so a
/// connection from inside this process's tree is a tool call.
pub fn serve_registry(registry: Arc<Registry>, path: impl AsRef<Path>) -> io::Result<ServerHandle> {
    serve_registry_rooted(registry, path, std::process::id() as i32)
}

/// As [`serve_registry`], judging descent against `tree_root` instead of this process. For a test
/// that serves in-process and runs a head as its own child: production never calls this.
pub fn serve_registry_rooted(
    registry: Arc<Registry>,
    path: impl AsRef<Path>,
    tree_root: i32,
) -> io::Result<ServerHandle> {
    let path = path.as_ref().to_path_buf();
    // THE CLAIM COMES FIRST, AND IT DOES NOT ASK WHETHER THE FILE IS THERE. The check below is
    // gated on `path.exists()`, which a racing binder makes true and stale.
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
        // A filesystem that cannot hold the claim does not stop a daemon starting: the probe
        // below is still the belt, and this is said out loud rather than swallowed, because
        // silence is the shape of the bug.
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
    // The belt, still worn: it catches what the lock cannot, a daemon from an older build, and
    // any other program that binds this path.
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
    // Filesystem permissions are the auth: no ceremony beyond the protocol version.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    // THE ACCEPT LOOP MUST BE WAKABLE WITHOUT THE SOCKET FILE EXISTING: `shutdown` unblocks it
    // by connecting, and a path a racing daemon unlinked would leave it blocked for ever.
    let _ = listener.set_nonblocking(true);
    // What we bound, so going away can remove this and not somebody else's socket. Read off
    // the path immediately after the bind, before any other start can look at the same name.
    let socket_id = std::fs::metadata(&path).ok().map(|m| (m.dev(), m.ino()));

    let r = registry.clone();
    let deaf_path = path.clone();
    let deaf_id = socket_id;
    let accept = std::thread::Builder::new()
        .name("head-accept".into())
        .spawn(move || {
            // A failed `accept` is almost never a reason to stop accepting: `Err(_) => break` left a
            // daemon alive and deaf.
            let mut transient = 0u32;
            loop {
                if r.is_closed() {
                    break;
                }
                match listener.accept() {
                    Ok((s, _)) => {
                        // A blocking stream, explicitly: Linux does not pass the listener's
                        // non-blocking flag on to an accepted socket, and a daemon whose reads
                        // are blocking by design should not rely on the platform being kind.
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
                                    if let Err(e) = serve_conn_rooted(r2, s, tree_root) {
                                        // A head going away is the normal case and not
                                        // worth a line; anything else is.
                                        if !matches!(e, WireError::Eof) {
                                            eprintln!("head connection ended: {e}");
                                        }
                                    }
                                });
                    }
                    // Idle, and not an error: this is what makes `shutdown` able to stop a
                    // daemon whose socket file is gone. Silent on purpose - a line every poll
                    // from a healthy daemon is noise that hides the lines that matter.
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    Err(e) if is_transient(&e) => {
                        // A client that vanished before it was picked up costs nothing; running out of
                        // descriptors is worth a line and a pause.
                        transient = transient.saturating_add(1);
                        if e.kind() != io::ErrorKind::ConnectionAborted {
                            eprintln!(
                                "head accept: {e} (transient, retrying; {transient} in a row)"
                            );
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                    }
                    Err(e) => {
                        // Nothing here is recoverable - a listener that is not a listener any
                        // more. Say which errno, because the last time this happened nobody
                        // could name it.
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
            // The socket file goes when the listener does, if it is still ours: the path must stop
            // advertising a door, so a head does not connect to nothing.
            if let Some(id) = deaf_id {
                unlink_if_ours(&deaf_path, id);
            }
            // And the daemon comes down with it, gracefully: every head wakes with `Bye`,
            // every worker falls out of `next_command`, and closing twice is idempotent.
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

/// One connection's chat worker: answers `RoomChat` frames for `head` on its own thread, in
/// arrival order, and retires the head's chat state when the reader loop ends and the channel
/// goes quiet. The thread is the reader loop's to keep reading while a request is out.
fn spawn_room_chat_worker(
    registry: Arc<Registry>,
    writer: Arc<Mutex<FrameWriter<UnixStream>>>,
    head: String,
) -> std::sync::mpsc::Sender<(String, serde_json::Value)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("room-chat".into())
        .spawn(move || {
            while let Ok((client_request_id, op)) = rx.recv() {
                let f = ServerFrame::RoomChat {
                    client_request_id,
                    reply: registry.room_chat(&head, op),
                };
                let mut w = writer.lock().unwrap_or_else(|e| e.into_inner());
                if w.write(&f).is_err() {
                    break;
                }
            }
            registry.room_chat_gone(&head);
        })
        .expect("a thread for a head that chats");
    tx
}

/// One head's attachment to one session: the hub, the id it has there, and the
/// flag that tells its pump the difference between a shutdown and a move.
struct Seat {
    hub: Arc<Hub>,
    head_id: String,
    /// Set before the reader detaches this head so it can move. The pump wakes with
    /// [`Delivery::Closed`] either way.
    switching: Arc<AtomicBool>,
    pump: Option<JoinHandle<()>>,
}

/// A seat is detached however the connection ends: the loop below returns through an error arm
/// in forty-odd places, and a head left registered would be counted for ever.
impl Drop for Seat {
    fn drop(&mut self) {
        self.hub.detach(&self.head_id);
    }
}

/// One connection, from ATTACH to detach - through however many sessions.
pub fn serve_conn(registry: Arc<Registry>, stream: UnixStream) -> Result<(), WireError> {
    serve_conn_rooted(registry, stream, std::process::id() as i32)
}

/// As [`serve_conn`], judging descent against `tree_root`.
pub fn serve_conn_rooted(
    registry: Arc<Registry>,
    stream: UnixStream,
    tree_root: i32,
) -> Result<(), WireError> {
    let out = stream.try_clone()?;
    // Read before the stream is handed to the reader, because the peer is a fact about the
    // connection the kernel filled in at `connect` and nothing later can be asked about it.
    let peer = crate::peer::Peer::of(&stream, tree_root);
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

    // A differing protocol version is ACCEPTED, and said, never refused on the number alone.
    // This side cannot decide which of the two is wrong.
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

    // A seat is a head's. An inside peer may take one only to reach `sudo`'s askpass door.
    if peer.inside && kind != crate::peer::ASKPASS_KIND {
        let mut w = writer.lock().unwrap();
        w.write(&ServerFrame::Bye {
            reason: format!(
                "attaching as {kind:?} is a head's act, and this connection came from inside a                  session's tool tree; only kind {:?} may attach from there",
                crate::peer::ASKPASS_KIND
            ),
        })?;
        return Ok(());
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

    registry.note_attach(&hub.session_id(), &kind, &identity);

    let mut seat = seat_in(
        &registry, hub, peer, since_seq, &kind, &identity, &caps, &writer,
    )?;

    // The connection's chat answers, handed to a worker on the first `RoomChat` frame: `None`
    // until a head chats, so a connection that never opens the rooms view pays no thread.
    let mut chat: Option<std::sync::mpsc::Sender<(String, serde_json::Value)>> = None;

    // The reader loop. Ends on Detach or on the peer closing - and a TCP close is detach,
    // never abort: nothing below cancels a turn.
    let result = loop {
        match reader.read::<ClientFrame>() {
            // Who connected decides what it may reach. A command this daemon started carries
            // `LETIBOT_SOCKET` for `sudo` and is not the operator, so askpass is the only door it gets.
            // First arm, so it sees every frame before a door does.
            Ok(frame) if peer.refuses(&frame).is_some() => {
                let door = peer.refuses(&frame).expect("the guard said there is one");
                let (client_request_id, expected_seq) = crate::peer::request_of(&frame);
                // Which fact it was rests on the pid: a named one is a process this daemon started,
                // and an unnamed one is read as the same because a head cannot be ruled out.
                let (who, what) = match peer.pid {
                    Some(p) => (
                        format!("pid {p}"),
                        "is a process this daemon started, in its own tree or its cgroup",
                    ),
                    None => (
                        "the process".to_string(),
                        "could not be named by the kernel, which is read the same way",
                    ),
                };
                let f = ServerFrame::Rejected {
                    client_request_id,
                    reason: format!(
                        "{door} is the operator's own act, and this connection came from inside \
                         a session's tool tree: {who} on the other end of the socket {what}, so \
                         nothing of it was done. The one door served to a tool call is the \
                         password ask `sudo` needs."
                    ),
                    expected_seq,
                    actual_seq: seat.hub.head_seq(),
                };
                writer.lock().unwrap().write(&f)?;
            }
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
                // `sudo` in this session wants a password. Raise it and wait here: this connection is
                // the helper's, and it sends nothing else.
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
                    // The deadline - and WHICH silence, which the one sentence cannot carry.
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
                    seat.hub.warn(
                        "secret_late",
                        format!(
                            "{}: nothing was waiting on {req_id} — the helper had given up, \
                             or another head answered first",
                            identity
                        ),
                    );
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
            // The operator answered a command that asked them something: off the queue, because the
            // thread that would drain it is the one blocked on the answer.
            Ok(ClientFrame::PromptAnswer { req_id, line }) => {
                let session = seat.hub.session_id().to_string();
                match registry.prompt(&session) {
                    None => {
                        seat.hub.warn(
                            "nothing_to_send_to",
                            format!(
                                "{identity}: this daemon has no way to reach a running \
                                 command's stdin, so `{line}` was not sent. Nothing of yours \
                                 is running."
                            ),
                        );
                    }
                    Some(driver) => match driver.send(&session, Some(&req_id), &line) {
                        // A card was up and this settled it. The line is nowhere on the log -
                        // the settlement is the record, exactly as `SecretSettled` is for a
                        // password.
                        Ok(settled) => {
                            if let Some(id) = settled {
                                seat.hub.publish(crate::event::SessionEvent::PromptSettled {
                                    req_id: id,
                                    sent: true,
                                    by: identity.clone(),
                                });
                            }
                        }
                        // Late, and said rather than refused: the command ended, or another
                        // head answered first, so nothing was written. Worth a red line - the
                        // person typed an answer and the command did not get it.
                        Err(why) => {
                            seat.hub.warn("prompt_late", format!("{identity}: {why}"));
                        }
                    },
                }
            }
            // One line to the running command, on demand - the manual floor under the card.
            // Off the queue for the reason above, and with no `req_id`: it addresses whatever
            // operator command this session is running right now.
            Ok(ClientFrame::SendLine { line }) => {
                let session = seat.hub.session_id().to_string();
                match registry.prompt(&session) {
                    None => {
                        seat.hub.warn(
                            "nothing_to_send_to",
                            format!(
                                "{identity}: this daemon has no way to reach a running \
                                 command's stdin, so `{line}` was not sent."
                            ),
                        );
                    }
                    Some(driver) => match driver.send(&session, None, &line) {
                        Ok(settled) => {
                            // A card was up and this answered it, so the card comes down for
                            // every head - the same settlement the card's own field would have
                            // produced.
                            if let Some(id) = settled {
                                seat.hub.publish(crate::event::SessionEvent::PromptSettled {
                                    req_id: id,
                                    sent: true,
                                    by: identity.clone(),
                                });
                            }
                        }
                        // Nothing of yours is running, or its stdin is gone. A refusal and not
                        // a failure: the act had nothing to act on.
                        Err(why) => {
                            seat.hub
                                .warn("nothing_to_send_to", format!("{identity}: {why}"));
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
                // Answered here and now, off the registry, rather than queued as a command: a
                // pane that opens must answer while it is open, and a queued answer arrives
                // after the turn it was asked during.
                let f = ServerFrame::Jobs {
                    session_id: seat.hub.session_id().to_string(),
                    jobs: registry.jobs(&seat.hub.session_id()),
                    monitors: registry.monitors(&seat.hub.session_id()),
                };
                writer.lock().unwrap().write(&f)?;
            }
            // The standing-notes pane's bootstrap read: the rows are published into the registry
            // wherever the section can have changed.
            Ok(ClientFrame::ListNotes) => {
                let f = ServerFrame::StandingNotes {
                    session_id: seat.hub.session_id().to_string(),
                    notes: registry.notes(&seat.hub.session_id()),
                };
                writer.lock().unwrap().write(&f)?;
            }
            // The skills pane's bootstrap read - the rows the index names, and never a body:
            // the body travels when somebody presses Enter (`ClientFrame::LoadSkill`), which is
            // also the only door a fabric skill's body could come through.
            Ok(ClientFrame::ListSkills) => {
                let f = ServerFrame::Skills {
                    session_id: seat.hub.session_id().to_string(),
                    skills: registry.skills(&seat.hub.session_id()),
                };
                writer.lock().unwrap().write(&f)?;
            }
            // The operator hands this session one skill - `/skills NAME rest`, and the pane's
            // Enter. Queued like `OperatorShell`: it is the operator's own line, and a turn
            // already running reads it at its next round boundary.
            Ok(ClientFrame::LoadSkill {
                client_request_id,
                expected_seq,
                name,
                then,
            }) => {
                if name.trim().is_empty() {
                    let f = ServerFrame::Rejected {
                        client_request_id,
                        reason: "a skill to load needs a name: `/skills NAME`, with NAME one of \
                                 the rows the pane lists. Nothing was loaded."
                            .into(),
                        expected_seq,
                        actual_seq: seat.hub.head_seq(),
                    };
                    writer.lock().unwrap().write(&f)?;
                } else {
                    let f = seat.hub.submit(
                        &seat.head_id,
                        client_request_id,
                        expected_seq,
                        CommandKind::LoadSkill { name, then },
                    );
                    writer.lock().unwrap().write(&f)?;
                }
            }
            Ok(ClientFrame::ListTodos) => {
                // The bootstrap read: the snapshot carries items, not events, so a head
                // attaching fresh has nothing to replay. From here the `TodosUpdated` events
                // carry every change.
                let f = ServerFrame::Todos {
                    session_id: seat.hub.session_id().to_string(),
                    todos: registry.todos(&seat.hub.session_id()),
                };
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::ListMergeQueue) => {
                // The bootstrap read, answered here and now off the registry: a pane that opens must
                // answer without a turn.
                let f = ServerFrame::Queues {
                    snapshot: registry.queues_for(&seat.hub.session_id()),
                };
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::RoomChat {
                client_request_id,
                op,
            }) => {
                // Answered off the reader loop, on the connection's own worker: the
                // coordinator can hold a request for seconds, and the head's Interrupts
                // must be read while it waits. One worker keeps the replies in order.
                if chat.is_none() {
                    chat = Some(spawn_room_chat_worker(
                        Arc::clone(&registry),
                        Arc::clone(&writer),
                        seat.head_id.clone(),
                    ));
                }
                let sent = chat
                    .as_ref()
                    .is_some_and(|tx| tx.send((client_request_id.clone(), op)).is_ok());
                if !sent {
                    // The worker only ends when its own write failed, which means this socket
                    // is going; say so rather than drop the frame unheard.
                    let f = ServerFrame::RoomChat {
                        client_request_id,
                        reply: serde_json::json!({
                            "error": "the chat worker has stopped; reconnecting restarts it"
                        }),
                    };
                    writer.lock().unwrap().write(&f)?;
                }
            }
            Ok(ClientFrame::NewSession {
                client_request_id,
                title,
                workspace,
            }) => {
                // The id is the daemon's to mint: two heads racing to create "scratch" would
                // otherwise get one success and one confusing refusal, and neither of them
                // asked for a name collision.
                let id = mint_session_id(&registry);
                let mut wiring = registry.wiring(&seat.hub.session_id());
                // The head's tree, when it named one. The model, dialect and endpoint stay the
                // daemon's: those are what it is talking to, and a head does not get to change
                // them by asking for a session.
                if !workspace.is_empty() {
                    wiring.workspace = workspace;
                }
                let f = match registry.create(&id, title, wiring) {
                    Ok(_) => {
                        // WHO ASKED, which a session's row cannot otherwise say: a terminal and a package
                        // make the same shape of row.
                        registry.record_origin(&id, SessionOrigin::of_opener(&kind, &identity));
                        ServerFrame::Sessions {
                            sessions: registry.list(),
                            current: seat.hub.session_id(),
                            created: Some(id),
                        }
                    }
                    Err(e) => ServerFrame::Rejected {
                        client_request_id,
                        reason: e.to_string(),
                        expected_seq: 0,
                        actual_seq: seat.hub.head_seq(),
                    },
                };
                writer.lock().unwrap().write(&f)?;
            }
            // Bring a stored session into this daemon: idempotent, and refused by name when nothing
            // anywhere has heard of the id.
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
                        // An `oc-` id is imported, not resumed: it marks an opencode conversation, and
                        // nothing is in the store to restore one.
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
                            // The session's own wiring, from the store and not from this connection: a resumed
                            // conversation belongs to the workspace it was held in.
                            let parent = registry.parent_for(
                                &session_id,
                                None,
                                brief.parent_session_id.as_deref(),
                            );
                            match registry.create_under(
                                &session_id,
                                &brief.title,
                                brief.wiring,
                                parent,
                            ) {
                                Ok(hub) => {
                                    // Who it is for, from its own row: a session resumed after a restart comes back
                                    // knowing whether a package or a one-shot made it.
                                    registry.record_origin(&session_id, brief.origin);
                                    // THE RESTORE IS ANNOUNCED HERE, BEFORE IT CAN START: a session is registered
                                    // when a head asks for it and opened later, so the wait has to be visible.
                                    if brief.items > 0 {
                                        hub.publish(SessionEvent::Filling {
                                            what: crate::RESTORING_ROWS.into(),
                                            unit: "rows".into(),
                                            done: 0,
                                            total: brief.items as u64,
                                        });
                                    }
                                    ServerFrame::Sessions {
                                        sessions: registry.list(),
                                        current: seat.hub.session_id(),
                                        created: Some(session_id),
                                    }
                                }
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
            // Naming a session the daemon holds. A session that is only in the store is refused here
            // rather than renamed behind a resume.
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
                            // On the renamed session's own log, so that every head watching *it*
                            // is told - not only the head that asked, and not only the session
                            // this connection is sitting in.
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
                        // Refused, and the connection stays where it is: a switch that
                        // half-happened would leave a head attached to nothing, which the
                        // mid-turn attach machinery cannot recover from.
                        let f = ServerFrame::Rejected {
                            client_request_id: format!("switch:{session_id}"),
                            reason: format!("{REJECT_UNKNOWN_SESSION} {session_id:?}"),
                            expected_seq: since_seq,
                            actual_seq: seat.hub.head_seq(),
                        };
                        writer.lock().unwrap().write(&f)?;
                    }
                    Some(next) if next.session_id() == seat.hub.session_id() => {
                        // Already here. Answered rather than ignored: "I pressed the key and
                        // nothing happened" is the report this avoids.
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
                        seat = seat_in(
                            &registry, next, peer, since_seq, &kind, &identity, &caps, &writer,
                        )?;
                    }
                }
            }
            // The operator's own call, admitted before it runs. The allowlist is enforced HERE, on
            // the connection's thread, before the worker sees the frame.
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
            // What the operator's call produced. No `expected_seq`: this frame does not move the
            // session, it hands over a fact.
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
            // The operator's own shell line: a `!` command the DAEMON runs. No allowlist, because
            // there is no tool name to check: the daemon mints the call itself.
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
                    // The identity, not the head id - the row's `CallOrigin` and the door's
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
            // The model's half of `!` completion, for a prefix the history has no match for: answered
            // here and now, off the queue.
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
            // A program that owns the screen, in a pane the daemon owns (`!term`). Not a command: it
            // appends no row and moves no transcript.
            Ok(ClientFrame::TermOpen { line, cols, rows }) => {
                let said = match crate::term_line(&line) {
                    // Not this verb at all. A head that sent one is a head this daemon does
                    // not understand, so it is told rather than ignored.
                    None => Err(format!(
                        "`{line}` is not a `!term` line: the verb is `!term`, and it has to be \
                         followed by whitespace. Nothing ran."
                    )),
                    // The verb with no command is ATTACH: the pane is the session's, so a person whose head
                    // lost the rectangle still has the program.
                    Some(crate::TermLine::Attach(pane)) => match registry.terminal() {
                        None => Err("this daemon has no terminal driver, so there is no pane \
                                     to attach to. Nothing was attached."
                            .to_string()),
                        Some(driver) => driver
                            .attach(&seat.hub.session_id(), pane, &seat.hub, cols, rows)
                            .map(drop),
                    },
                    // `!term close` is the ending, and it is the head's act: the head asks the operator to
                    // confirm and then sends this.
                    Some(crate::TermLine::Close(_)) => Err(
                        "`!term close` is not a command to run: it is how a pane is ENDED, and \
                         the head sends that as its own act (after asking). Nothing ran — a \
                         program called `close` is `!term command close`."
                            .to_string(),
                    ),
                    Some(crate::TermLine::Run(command)) => match registry.terminal() {
                        None => Err("this daemon has no terminal driver, so there is no pty to \
                                     run a screen program on. Nothing ran."
                            .to_string()),
                        Some(driver) => driver
                            .open(&seat.hub.session_id(), &seat.hub, command, cols, rows)
                            .map(drop),
                    },
                };
                if let Err(reason) = said {
                    writer
                        .lock()
                        .unwrap()
                        .write(&ServerFrame::TermEnded { pane: None, reason })?;
                }
            }
            // The operator's keys, verbatim. Not a command either, and for a sharper reason
            // than the pane's: a keystroke that queued behind a running turn would be a key that
            // arrives after the thing it was answering.
            Ok(ClientFrame::TermInput { pane, bytes }) => {
                if let Some(driver) = registry.terminal() {
                    // A failed write is the pty's far end being gone, which the pane's own
                    // reader thread is about to report with a better sentence than this arm
                    // could - so it is dropped here rather than turned into a second ending.
                    let _ = driver.input(&seat.hub.session_id(), pane, &bytes);
                }
            }
            // The pane's rectangle moved - the head's fact, since the daemon has no screen.
            // `TIOCSWINSZ` on the pty, and the kernel raises `SIGWINCH` for the program's
            // foreground process group by itself.
            Ok(ClientFrame::TermResize { pane, cols, rows }) => {
                if let Some(driver) = registry.terminal() {
                    let _ = driver.resize(&seat.hub.session_id(), pane, cols, rows);
                }
            }
            // The operator left. Idempotent and quiet when there is no pane: stop cannot be wrong about
            // anything.
            Ok(ClientFrame::TermClose { pane }) => {
                if let Some(driver) = registry.terminal() {
                    let _ = driver.close(&seat.hub.session_id(), pane);
                }
            }
            // What this session's pane is running, or nothing: a read, answered on this connection like
            // every other pane frame.
            Ok(ClientFrame::TermStatus) => {
                let panes = registry
                    .terminal()
                    .map(|driver| driver.status(&seat.hub.session_id()))
                    .unwrap_or_default();
                writer
                    .lock()
                    .unwrap()
                    .write(&ServerFrame::TermStatus { panes })?;
            }
            // A head names one decision and one half of its exchange; the daemon answers with the
            // bytes or with not recorded.
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
                let hub = &seat.hub;
                answer_settings(&writer, || registry.settings(&hub.session_id()))?;
            }
            Ok(ClientFrame::Peek { session_id, shape }) => {
                // A read, not a move: the seat, its acks and its live events are untouched, and the
                // answer is the session's scrollback.
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
                // A read, not a move, the same contract `Peek` keeps: both live in this arm of the match
                // rather than on the queue.
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
                // The view first, then the store: the view is one lock and a `Vec` index, so it is the
                // fast path for a row this daemon already holds.
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
                    // Nowhere holds it: past the end of the session, or a store this daemon
                    // cannot reach. `None` rather than an empty string, because "nobody has it"
                    // and "it is empty" must not look alike.
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
            Ok(ClientFrame::FetchRows {
                session_id,
                from,
                count,
            }) => {
                let Some(hub) = registry.resolve(&session_id) else {
                    let f = ServerFrame::Rejected {
                        client_request_id: format!("fetchrows:{session_id}/{from}"),
                        reason: format!("{REJECT_UNKNOWN_SESSION} {session_id:?}"),
                        expected_seq: 0,
                        actual_seq: seat.hub.head_seq(),
                    };
                    writer.lock().unwrap().write(&f)?;
                    continue;
                };
                let want = count.min(MAX_FETCH_ROWS);
                let mut rows: Vec<(usize, crate::view::SnapshotItem)> = Vec::new();
                let mut row = from;
                while rows.len() < want {
                    let Some(item) = hub.row_at(row) else {
                        // The first ordinal the view cannot answer, so the rest of the page is
                        // the store's: one read for the page rather than one per row.
                        rows.extend(registry.rows_from_store(&session_id, row, want - rows.len()));
                        break;
                    };
                    rows.push((row, item));
                    row += 1;
                }
                // The page is bounded in BYTES as well as rows, and it gives up its OLDEST rows.
                // The rows nearest the window are the ones the reader scrolling up is next to, and a
                // page that kept its head would no longer join the window it was asked for.
                let mut bytes = 0;
                let mut keep = 0;
                for (_, it) in rows.iter().rev() {
                    let b = it.item.as_ref().map_or(0, |i| i.bytes());
                    if keep > 0 && bytes + b > MAX_FETCH_BYTES {
                        break;
                    }
                    bytes += b;
                    keep += 1;
                }
                rows.drain(..rows.len().saturating_sub(keep));
                // The ordinal the page really starts at: a source that could not hand over its first
                // row answered from a later one, and the head places a page by what it is told here.
                let starts = rows.first().map(|(at, _)| *at).unwrap_or(from);
                let f = ServerFrame::RowsFetched {
                    session_id: hub.session_id(),
                    from: starts,
                    rows: rows.into_iter().map(|(_, r)| r).collect(),
                };
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::Prompt {
                client_request_id,
                expected_seq,
                text,
                attachments,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::Prompt { text, attachments },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::SetOperatorTodos {
                client_request_id,
                expected_seq,
                items,
                moved,
            }) => {
                // Not gated and not a decision: the operator's own list is their own authoring,
                // the same trust as a prompt. `moved` is the same act one row over - a state on
                // a row they did not write, never a membership, so no row is added or reworded.
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::SetOperatorTodos { items, moved },
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
            // Closing ONE session does not go through the command queue either, for `Stop`'s reason:
            // the worker may be inside a turn.
            Ok(ClientFrame::CloseSession {
                client_request_id,
                expected_seq: _,
                who,
                reason,
            }) => {
                let session = seat.hub.session_id();
                // The caller's own reason, because there are two callers and they mean different
                // things: a migration carries the rows elsewhere, a bridge's stop settles a
                // placement. A head reading the log is owed the one that is true.
                let why = match reason.trim().trim_end_matches('.') {
                    "" => String::new(),
                    words => format!(" because {words}"),
                };
                seat.hub.warn(
                    "session_closed",
                    format!(
                        "`{who}` closed this session{why}. Nothing here is deleted: the rows stay \
                         in the store, so `letibot --session <id>` reopens it, and what a \
                         placement committed comes home as its branch. A head left attached is \
                         detached — a conversation that has moved on is not one to keep typing \
                         into."
                    ),
                );
                // Closed before it is acked: `--migrate` copies the session the moment it reads
                // `Accepted`, so the answer has to mean the rows are already written.
                let seq = seat.hub.head_seq();
                registry.close_session(&session);
                let f = crate::protocol::ServerFrame::Accepted {
                    client_request_id,
                    seq,
                    note: crate::protocol::NOTE_SESSION_CLOSED.into(),
                };
                writer.lock().unwrap().write(&f)?;
            }
            // The stop does NOT go through the command queue: one worker drains that queue and a
            // running turn owns it, so a stop queued behind a turn never arrives.
            Ok(ClientFrame::Stop {
                client_request_id,
                expected_seq: _,
                who,
            }) => {
                for brief in registry.list() {
                    if let Some(hub) = registry.get(&brief.session_id) {
                        hub.warn(
                            "daemon_stopping",
                            format!(
                                "`{who}` asked this daemon to stop. Every head detaches, \
                                 the socket goes, and the session is on disk — `letibot \
                                 --continue` reopens it. A turn already generating is \
                                 INTERRUPTED rather than waited for — it ends at its next \
                                 token, its partial output is kept, and nothing new is \
                                 started."
                            ),
                        );
                    }
                }
                // What this stop is about to end is SAID FIRST, before the ack: the sentence a head
                // needs is what the daemon killed, not what it was asked to.
                registry.announce_runs_ending();
                // Acked before the close, because after it there is no socket to ack on and a
                // head waiting for one would wait forever.
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
                // Two halves: the flag the `bash` wait loop honours mid-turn, and the queued command
                // that announces the between-turn interrupt.
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
            // The ASK and the person's answer to it both ride the command queue like `Mode` does: the
            // worker is the one writer of a session's transcript.
            Ok(ClientFrame::ModeConfirm {
                client_request_id,
                expected_seq,
                name,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::ModeConfirm { name },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::ModeConsent {
                client_request_id,
                expected_seq,
                req_id,
                yes,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::ModeConsent { req_id, yes },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::ReadJobOutput {
                client_request_id,
                job,
                offset,
            }) => {
                // Answered here and now, off the queue, for `ListJobs`'s reason, when the session's
                // host still holds the job.
                if let Some(event) = registry.job_output(&seat.hub.session_id(), &job, offset) {
                    seat.hub.publish(event);
                    continue;
                }
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
            // An operator's mark: answered HERE, on this thread, and never queued - the worker
            // may be pinned, and a mark that waits lands on the wrong turn.
            Ok(ClientFrame::Feedback {
                at_ms,
                polarity,
                note,
                item_id,
            }) => {
                crate::metrics_tap::mark(
                    &seat.hub,
                    &kind,
                    &identity,
                    at_ms,
                    &polarity,
                    &note,
                    item_id.as_deref(),
                );
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
            // A question's answer, not a permission's. Same command, same `req_id`, different
            // payload - and `Hub::submit` refuses a malformed one by name rather than letting it
            // settle the question.
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
            // A todo row's question, answered. A command and not a sink delivery: nothing is
            // blocked waiting for it, and the board it changes is the worker's (or, mid-turn,
            // the steering poll's).
            Ok(ClientFrame::AnswerTodo {
                client_request_id,
                content,
                question,
                answer,
            }) => {
                let f = seat.hub.submit(
                    &seat.head_id,
                    client_request_id,
                    0,
                    CommandKind::AnswerTodo {
                        content,
                        question,
                        answer,
                    },
                );
                writer.lock().unwrap().write(&f)?;
            }
            // A plugin pane, opened or keyed: the daemon half of the stack is the layers, the looks
            // and the gate, and all three are the harness's.
            Ok(ClientFrame::PaneOpen { pane, w, h }) => {
                let session_id = seat.hub.session_id();
                if let Some(driver) = registry.pane_driver(&session_id) {
                    driver.open(&session_id, &seat.hub, &pane, (w, h));
                }
            }
            // The head closed the pane: the daemon drops it from the refresh clock. Nothing is
            // answered - the head that stopped drawing it is the one that said so - and a pane
            // this session never had is not an error.
            Ok(ClientFrame::PaneClose { pane }) => {
                let session_id = seat.hub.session_id();
                if let Some(driver) = registry.pane_driver(&session_id) {
                    driver.close(&session_id, &pane);
                }
            }
            Ok(ClientFrame::PaneAction { pane, item, action }) => {
                let session_id = seat.hub.session_id();
                if let Some(driver) = registry.pane_driver(&session_id) {
                    driver.action(&session_id, &seat.hub, &pane, &item, &action);
                }
            }
            // A head going away is the normal case and says nothing.
            Err(WireError::Eof) => break Ok(()),
            // Anything else is said before the connection goes: hanging up in silence is how a
            // version skew presents itself.
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

/// Attach to `hub`, send the `Hello`, hand over the backlog and start the pump: the whole of
/// *you are now in this session*, in one place.
fn seat_in(
    registry: &Arc<Registry>,
    hub: Arc<Hub>,
    peer: crate::peer::Peer,
    since_seq: u64,
    kind: &str,
    identity: &str,
    caps: &crate::protocol::Caps,
    writer: &Arc<Mutex<FrameWriter<UnixStream>>>,
) -> Result<Seat, WireError> {
    // A tool call's command attaches only to reach `sudo`'s askpass door, and a seat is the way
    // there. The rest of a head's estate is not the password's business: no snapshot, no session
    // list, no wiring, no events and no vote.
    let inside = peer.inside;
    let mut held = caps.clone();
    if inside {
        held.can_decide = false;
    }
    let a = hub.attach(kind, identity, held, since_seq);
    let head_id = a.head_id.clone();
    let session_id = hub.session_id();
    // What this session's harness found about its frozen prefix, said NOW: the comparison is
    // made when the harness opens, and this is the one moment to say it.
    if let Some(said) = registry.stale_prefix(&session_id) {
        hub.warn("prefix_stale", said);
    }
    // And that this daemon's binary is older than the store it is serving. The store's floor is
    // the other half of that, said at the door.
    if let Some(said) = registry.store_older() {
        hub.warn("store_newer", said);
    }
    {
        let mut w = writer.lock().unwrap();
        // A head this daemon registered but could not tell it is here must not stay registered: the
        // `Hello` and the backlog go out, and a failure detaches.
        let wrote = w
            .write(&ServerFrame::Hello {
                protocol_version: PROTOCOL_VERSION,
                session_id: session_id.clone(),
                head_id: head_id.clone(),
                dropped: a.dropped,
                snapshot: if inside {
                    None
                } else {
                    a.snapshot.map(Box::new)
                },
                resumed_from: a.resumed_from,
                scrubbed: a.scrubbed,
                wiring: if inside {
                    Default::default()
                } else {
                    registry.wiring(&session_id)
                },
                sessions: if inside { Vec::new() } else { registry.list() },
            })
            .and_then(|()| {
                // A tool call is handed no backlog: it is not a head, so there is no history for it.
                if inside {
                    return Ok(());
                }
                let mut out = Ok(());
                for env in &a.backlog {
                    out = w.write(&ServerFrame::Event(env.clone()));
                    if out.is_err() {
                        break;
                    }
                }
                out
            });
        if let Err(e) = wrote {
            hub.detach(&head_id);
            return Err(e);
        }
    }

    // And the panes this session has, so a head that has just arrived has a list with titles
    // before anybody opens one.
    if let Some(driver) = registry.pane_driver(&session_id) {
        driver.attach(&session_id, &hub);
    }

    let switching = Arc::new(AtomicBool::new(false));
    // A tool call gets no pump: every event here belongs to a head, and this seat is not one.
    let pump = if inside {
        None
    } else {
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
                            // Frames that are not the record: the pane's byte stream. They go ahead of the
                            // events because a terminal that falls behind is a terminal that is wrong.
                            let mut r = Ok(());
                            for f in frames {
                                r = w.write(&f);
                                if r.is_err() {
                                    break;
                                }
                            }
                            r
                        }
                        Delivery::Events(b) => w.write_all(b.events().iter().map(EventOut::Event)),
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
                            // Two very different things arrive here: the daemon is going away, or
                            // this head has left for another session. Only the first is a `Bye`.
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

/// [`ServerFrame::Event`] by reference: the same bytes on the wire without a clone of the event.
#[derive(serde::Serialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
enum EventOut<'a> {
    Event(&'a crate::event::Envelope),
}

#[cfg(test)]
mod event_out_tests {
    use super::*;

    #[test]
    fn the_borrowed_event_frame_is_the_owned_one() {
        let env = crate::event::Envelope {
            session_id: "s-1".into(),
            seq: 7,
            ts: 1234,
            event: crate::testing::warn("w"),
        };
        assert_eq!(
            serde_json::to_string(&EventOut::Event(&env)).unwrap(),
            serde_json::to_string(&ServerFrame::Event(env.clone())).unwrap()
        );
    }
}

/// Detach from the session this seat is in, and stop its pump, without telling the head the
/// daemon is going away.
fn leave(seat: &mut Seat) {
    seat.switching.store(true, Ordering::SeqCst);
    // `detach` is what wakes the pump: `next_batch` returns `Closed` for a head id that is no
    // longer registered.
    seat.hub.detach(&seat.head_id);
    if let Some(p) = seat.pump.take() {
        let _ = p.join();
    }
}

/// A new session id, unique in this registry: monotonic wall-clock nanos, with a counter behind
/// them for the case where the clock does not move.
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

    /// The errnos that must not take a daemon's ears off: `ECONNABORTED` is the most ordinary accept
    /// error there is.
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
