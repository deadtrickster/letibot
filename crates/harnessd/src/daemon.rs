//! The process: a socket, a registry of sessions, one worker, and a clean shutdown.
//!
//! ```text
//!   head ──unix socket──> serve ──> Registry ──next_command──> worker ──> Harness::submit
//!                                    │  Hub per session                        │
//!                                    └────────────── events ───────────────────┘
//! ```
//!
//! # One worker over many sessions
//!
//! Still **one**, and still §13.2's "one authoritative reader". What changed is
//! what it waits on: [`Registry::next_command`] blocks on the cross-session bell
//! and hands back *which* session woke it. So two heads prompting two different
//! sessions are served in the order they pressed enter, and a long turn in one
//! session queues the other — which is honest about a box with one GPU, and is
//! what the operator would otherwise discover by watching two sessions both claim
//! to be generating.
//!
//! One worker rather than a thread per session is not a stopgap. `TurnEngine`
//! borrows the vocabulary and the dialect's renderer, and a second thread would
//! need both to be `Sync`; more to the point, the thing being serialised is a
//! shared llama.cpp server with a fixed number of slots, and a daemon that ran two
//! turns at once would be arbitrating a resource it does not own.
//!
//! # Why there is no async runtime and no timer
//!
//! §18.1-I12: *"the daemon spawns no timer or poll loop for message delivery"*.
//! [`Hub::take_command`] blocks on a condvar until something is pushed, and the
//! accept loop blocks on `accept`. Shutdown arrives on a **self-pipe** — the signal
//! handler writes one byte, a thread blocked in `read` wakes and closes the hub —
//! rather than on a flag some loop polls. Three blocking reads and no clock.
//!
//! That also settles what a signal handler is allowed to do here. `Hub::close`
//! takes a mutex and notifies a condvar; neither is async-signal-safe, so calling
//! it from the handler is a deadlock waiting for a busy Tuesday. `write(2)` on a
//! pipe is on the safe list, so the handler writes and the thread does the work.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use letibot_sessionlog::hub::{Hub, QueuedCommand};
use letibot_sessionlog::registry::Registry;
use letibot_sessionlog::server::{ServerHandle, serve_registry};

use crate::harness::HarnessError;
use letibot_sessionlog::registry::Work;

use crate::sessions::{Outcome, Sessions};

/// The write end of the self-pipe, for the signal handler. An `AtomicI32` because
/// that is what a handler may touch; `-1` means no handler is installed.
static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(_sig: libc::c_int) {
    let fd = SIGNAL_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = [1u8];
        // Async-signal-safe, and the return value is deliberately ignored: there is
        // nothing a handler could do about a full pipe except make things worse,
        // and a full pipe already means a shutdown is pending.
        unsafe {
            libc::write(fd, byte.as_ptr() as *const libc::c_void, 1);
        }
    }
}

/// A running daemon.
pub struct Daemon {
    server: ServerHandle,
    /// Kept alive for the life of the daemon: dropping the read end would make the
    /// shutdown thread's `read` return 0 forever.
    _pipe: Option<(OwnedFd, OwnedFd)>,
}

impl Daemon {
    /// Bind the socket and start serving every session in `registry`.
    pub fn serve(registry: Arc<Registry>, socket: &std::path::Path) -> io::Result<Daemon> {
        if let Some(parent) = socket.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let server = serve_registry(registry, socket)?;
        Ok(Daemon {
            server,
            _pipe: None,
        })
    }

    /// Bind the socket for a daemon with exactly one session, which is what
    /// `letibot-m1` and the loop test have.
    pub fn serve_one(hub: Arc<Hub>, socket: &std::path::Path) -> io::Result<Daemon> {
        Daemon::serve(Registry::of(hub), socket)
    }

    pub fn socket(&self) -> &std::path::Path {
        self.server.path()
    }

    pub fn registry(&self) -> &Arc<Registry> {
        self.server.registry()
    }

    /// Install SIGINT/SIGTERM handlers that close every session.
    ///
    /// Closing the registry is what ends everything else: every hub closes, the
    /// bell closes, `next_command` returns `None`, every attached head wakes with
    /// `Closed`, and the worker loop falls out. A second signal is not
    /// special-cased — the first one already started an orderly stop, and a "force"
    /// path is a way to lose the last turn's rows.
    pub fn catch_signals(&mut self) -> io::Result<()> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `pipe` writes two fds into a two-element array.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let (read, write) = (to_owned(fds[0]), to_owned(fds[1]));
        SIGNAL_FD.store(write.as_raw_fd(), Ordering::SeqCst);

        for sig in [libc::SIGINT, libc::SIGTERM] {
            // SAFETY: `on_signal` is `extern "C"` and touches only an atomic and
            // `write(2)`, both of which are permitted in a handler.
            unsafe {
                libc::signal(sig, on_signal as *const () as libc::sighandler_t);
            }
        }

        let registry = self.registry().clone();
        let rfd = read.as_raw_fd();
        std::thread::Builder::new()
            .name("harnessd-signal".into())
            .spawn(move || {
                let mut buf = [0u8; 1];
                // Blocks. No timer, no poll: the byte arrives or it does not.
                let n = unsafe {
                    libc::read(rfd, buf.as_mut_ptr() as *mut libc::c_void, 1)
                };
                if n > 0 {
                    registry.close();
                }
            })?;
        self._pipe = Some((read, write));
        Ok(())
    }

    /// Run the single command worker until the registry closes.
    ///
    /// One worker, by construction: §13.2's "one authoritative reader, shared by
    /// many heads", now shared by many *sessions* as well. Two heads may prompt at
    /// once and the second is queued, which is what a human expects from a shared
    /// session — it is not a race the daemon has to arbitrate.
    pub fn run(
        &self,
        sessions: &mut Sessions<'_>,
        mut on_reply: impl FnMut(&str, &QueuedCommand, Outcome),
    ) {
        while let Some(work) = self.registry().next_work() {
            match work {
                // A session was created — by a head's `/new`, or by a `--continue`
                // asking for one out of the store. Opened **here**, on the worker,
                // rather than on the connection thread that asked: the worker is the
                // one authoritative reader (§13.2), and a harness built on a socket
                // thread would be a second one.
                //
                // A failure is already announced on that session's own log by
                // `Sessions::open`; the daemon keeps serving, because one session
                // that cannot be rebuilt is not the others' problem.
                Work::Open(session_id) => {
                    match sessions.open(&session_id) {
                        Err(e) => eprintln!("  {session_id} · not opened: {e}"),
                        // Already open: the daemon's own first session, whose banner
                        // was printed at startup. Saying it twice would suggest two
                        // resumes happened.
                        Ok(false) => {}
                        Ok(true) => {
                            if let Some(r) = sessions.resume_report(&session_id) {
                                eprintln!(
                                    "  {session_id} · resumed {} row(s), {} tokens, head {}",
                                    r.rows,
                                    r.tokens,
                                    &r.head[..16.min(r.head.len())]
                                );
                                for note in &r.notes {
                                    eprintln!("    note: {note}");
                                }
                            } else {
                                eprintln!("  {session_id} · opened, nothing to resume");
                            }
                        }
                    }
                }
                Work::Command(session_id, cmd) => {
                    let outcome = sessions.dispatch(&session_id, &cmd);
                    on_reply(&session_id, &cmd, outcome);
                }
            }
        }
    }

    /// Stop accepting, wake every head with `Closed`, remove the socket.
    pub fn shutdown(self) {
        self.server.shutdown();
    }
}

/// SAFETY wrapper: turn a raw fd from `pipe(2)` into something `OwnedFd` accepts.
fn to_owned(fd: libc::c_int) -> std::os::fd::OwnedFd {
    // SAFETY: the caller has just created `fd` and transfers ownership here.
    unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) }
}

impl HarnessError {
    /// Whether the daemon should keep serving after this.
    ///
    /// A turn that failed is not a daemon that failed: §5.7's whole point is that a
    /// failed turn is a *recorded* outcome. Only a setup or store failure ends the
    /// process, because both mean the next turn would fail the same way and the
    /// rows would stop being written.
    pub fn is_fatal(&self) -> bool {
        matches!(self, HarnessError::Setup(_) | HarnessError::Store(_))
    }
}
