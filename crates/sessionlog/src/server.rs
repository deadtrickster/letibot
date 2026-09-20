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
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::hub::{CommandKind, Delivery, Hub, Reply};
use crate::protocol::{
    ClientFrame, PROTOCOL_VERSION, REJECT_NOT_IN_STORE, REJECT_UNKNOWN_SESSION, ServerFrame,
};
use crate::registry::Registry;
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
    // Avoiding a `libc` dependency in a crate that otherwise has none. The uid is
    // only used to keep two users' fallback sockets apart.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))?
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

/// A running server. Dropping it does **not** stop the session: see
/// [`ServerHandle::shutdown`].
pub struct ServerHandle {
    path: PathBuf,
    registry: Arc<Registry>,
    accept: Option<JoinHandle<()>>,
}

impl ServerHandle {
    pub fn path(&self) -> &Path {
        &self.path
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
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        // Best effort only. An abandoned handle leaves the socket, which is
        // recoverable; taking the session down on a drop would not be.
        let _ = std::fs::remove_file(&self.path);
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
    // A stale socket from a crashed daemon is not a running daemon. Removing it is
    // safe *because* a live one would still be holding the bind, and the bind is
    // what would then fail — the file's existence proves nothing.
    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("{} is already served by a live daemon", path.display()),
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

    let r = registry.clone();
    let deaf_path = path.clone();
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
            // **The socket file goes when the listener does.**
            //
            // Whatever ended this loop, the path must stop advertising a door: a
            // head gets a refusal it can report instead of an eternal poll. The
            // handle's own `Drop` removes it too, and removing it twice is an
            // ignored `ENOENT` — much cheaper than the case this prevents.
            let _ = std::fs::remove_file(&deaf_path);
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

    if protocol_version != PROTOCOL_VERSION {
        // Refuse loudly. §17-S6's rule: a silent version skew looks like a bug in
        // the other half, forever.
        let mut w = writer.lock().unwrap();
        w.write(&ServerFrame::Bye {
            reason: format!(
                "protocol version {protocol_version}, this daemon speaks {PROTOCOL_VERSION}"
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
                const PATIENCE: std::time::Duration = std::time::Duration::from_secs(120);
                let deadline = crate::event::now_ms() + PATIENCE.as_millis() as u64;
                let (req_id, rx) = seat.hub.request_secret(&prompt, &command, deadline);
                let secret = match rx.recv_timeout(PATIENCE) {
                    Ok(s) => s,
                    Err(_) => {
                        seat.hub
                            .abandon_secret(&req_id, "nobody, before the deadline");
                        None
                    }
                };
                writer
                    .lock()
                    .unwrap()
                    .write(&ServerFrame::Secret { secret })?;
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
            Ok(ClientFrame::Settings) => {
                let f = ServerFrame::Settings {
                    rows: registry.settings(&seat.hub.session_id()),
                };
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::Peek { session_id }) => {
                // A read, not a move: the seat, its acks and its live events are
                // untouched, and the answer is the named session's scrollback
                // scrubbed exactly as a replay would be — a peek IS a replay, so
                // it gets the replay's wire hygiene and the ring's cap. A session
                // this daemon does not hold is a Rejected naming it, never an
                // empty Peeked: an empty answer and a missing session must not
                // look alike.
                match registry.resolve(&session_id) {
                    Some(hub) => {
                        let retained = hub.retained();
                        let (kept, _) = crate::scrub::scrub_replay(retained.iter(), &retained);
                        let f = ServerFrame::Peeked {
                            session_id: hub.session_id(),
                            dropped: hub.dropped(),
                            events: kept,
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
                item_id,
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
                let (body, total, at) = match registry.resolve(&session_id) {
                    Some(hub) => match hub.row_body(&item_id) {
                        Some(full) => {
                            let total = full.len();
                            let start = clamp_char(&full, at.min(total));
                            let want = len.min(MAX_FETCH_ROW);
                            let end = clamp_char(&full, (start + want).min(total));
                            (Some(full[start..end].to_string()), total, start)
                        }
                        // The row is not in this daemon's view: trimmed by `ViewBounds`,
                        // or it belongs to a session that never had it. `None` rather
                        // than an empty string, because "nobody has it" and "it is
                        // empty" must not look alike.
                        None => (None, 0, 0),
                    },
                    None => {
                        let f = ServerFrame::Rejected {
                            client_request_id: format!("fetchrow:{session_id}/{item_id}"),
                            reason: format!("{REJECT_UNKNOWN_SESSION} {session_id:?}"),
                            expected_seq: 0,
                            actual_seq: seat.hub.head_seq(),
                        };
                        writer.lock().unwrap().write(&f)?;
                        continue;
                    }
                };
                let f = ServerFrame::RowFetched {
                    session_id,
                    item_id,
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
                        });
                    }
                }
                // Acked before the close, because after it there is no socket to
                // ack on and a head waiting for one would wait forever.
                let f = crate::protocol::ServerFrame::Accepted {
                    client_request_id,
                    seq: seat.hub.head_seq(),
                    note: "stopping".into(),
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
                // to be set here — the worker is blocked inside the wait it would
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
            Err(WireError::Eof) => break Ok(()),
            Err(e) => break Err(e),
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
