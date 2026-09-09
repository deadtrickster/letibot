//! The head server: a Unix socket at `$XDG_RUNTIME_DIR/harnessd.sock`.
//!
//! §13.4: *"unix socket … filesystem permissions are the auth; no ceremony"*. So
//! the socket is created mode 0600 and there is no handshake beyond the protocol
//! version. A remote head (W8's WebSocket, not built here) adds TLS and a bearer
//! token in front of the same frames.
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
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::hub::{CommandKind, Delivery, Hub};
use crate::protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame};
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
    hub: Arc<Hub>,
    accept: Option<JoinHandle<()>>,
}

impl ServerHandle {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// Stop accepting, wake every head with `Closed`, remove the socket.
    ///
    /// This is the case §13.2 calls daemon shutdown, which *is* an abort — as
    /// against a head detaching, which is not.
    pub fn shutdown(mut self) {
        self.hub.close();
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

/// Bind and start accepting.
pub fn serve(hub: Arc<Hub>, path: impl AsRef<Path>) -> io::Result<ServerHandle> {
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

    let h = hub.clone();
    let accept = std::thread::Builder::new()
        .name("head-accept".into())
        .spawn(move || {
            for stream in listener.incoming() {
                if h.is_closed() {
                    break;
                }
                match stream {
                    Ok(s) => {
                        let h2 = h.clone();
                        let _ =
                            std::thread::Builder::new()
                                .name("head-conn".into())
                                .spawn(move || {
                                    if let Err(e) = serve_conn(h2, s) {
                                        // A head going away is the normal case and not
                                        // worth a line; anything else is.
                                        if !matches!(e, WireError::Eof) {
                                            eprintln!("head connection ended: {e}");
                                        }
                                    }
                                });
                    }
                    Err(_) => break,
                }
            }
        })?;

    Ok(ServerHandle {
        path,
        hub,
        accept: Some(accept),
    })
}

/// One connection, from ATTACH to detach.
pub fn serve_conn(hub: Arc<Hub>, stream: UnixStream) -> Result<(), WireError> {
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
    let mine = hub.session_id();
    if session_id != mine && !session_id.is_empty() {
        let mut w = writer.lock().unwrap();
        w.write(&ServerFrame::Bye {
            reason: format!("this daemon holds session {mine}, not {session_id}"),
        })?;
        return Ok(());
    }

    let a = hub.attach(kind, identity, caps, since_seq);
    let head_id = a.head_id.clone();
    {
        let mut w = writer.lock().unwrap();
        w.write(&ServerFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            session_id: mine,
            head_id: head_id.clone(),
            dropped: a.dropped,
            snapshot: a.snapshot.map(Box::new),
            resumed_from: a.resumed_from,
            scrubbed: a.scrubbed,
        })?;
        for env in &a.backlog {
            w.write(&ServerFrame::Event(env.clone()))?;
        }
    }

    // The writer thread: blocks in the hub, never in the engine.
    let pump = {
        let hub = hub.clone();
        let writer = writer.clone();
        let head_id = head_id.clone();
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
                            let _ = w.write(&ServerFrame::Bye {
                                reason: "daemon shutting down".into(),
                            });
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

    // The reader loop. Ends on Detach or on the peer closing — and **TCP close is
    // detach, never abort** (§13.2): nothing below cancels a turn.
    let result = loop {
        match reader.read::<ClientFrame>() {
            Ok(ClientFrame::Ack(ack)) => hub.ack(&head_id, ack),
            Ok(ClientFrame::Resync) => hub.request_resync(&head_id, "requested by the head"),
            Ok(ClientFrame::Detach) => break Ok(()),
            Ok(ClientFrame::Attach { .. }) => {
                let mut w = writer.lock().unwrap();
                w.write(&ServerFrame::Bye {
                    reason: "already attached".into(),
                })?;
                break Ok(());
            }
            Ok(ClientFrame::Prompt {
                client_request_id,
                expected_seq,
                text,
            }) => {
                let f = hub.submit(
                    &head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::Prompt { text },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::Interrupt {
                client_request_id,
                expected_seq,
                reason,
            }) => {
                let f = hub.submit(
                    &head_id,
                    client_request_id,
                    expected_seq,
                    CommandKind::Interrupt { reason },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Ok(ClientFrame::Answer {
                client_request_id,
                req_id,
                option_id,
            }) => {
                let f = hub.submit(
                    &head_id,
                    client_request_id,
                    0,
                    CommandKind::Answer { req_id, option_id },
                );
                writer.lock().unwrap().write(&f)?;
            }
            Err(WireError::Eof) => break Ok(()),
            Err(e) => break Err(e),
        }
    };

    hub.detach(&head_id);
    if let Some(p) = pump {
        let _ = p.join();
    }
    result
}
