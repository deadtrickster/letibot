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
//! The loop below is a dispatcher, and every session has a reader of its own
//! ([`Sessions::serve_each_on_its_own_thread`] arms it, `Court` is the shape), because a
//! prompt to the second session must not wait for the first one's three-minute turn.
//!
//! # Why there is no async runtime and no timer
//!
//! I12: *"the daemon spawns no timer or poll loop for message delivery"*.
//! [`Hub::take_own_work`] blocks on a condvar until something is pushed and the accept
//! loop blocks on `accept`; a stop arrives on a self-pipe, because a signal handler may
//! `write(2)` but may not take a mutex. One `poll` exists, and only after a signal.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;
use letibot_sessionlog::server::{ServerHandle, serve_registry};

use crate::harness::HarnessError;
use letibot_sessionlog::registry::{Work, WorkOrIdle};

use crate::sessions::{Outcome, OutcomeSaid, ReplySink, Sessions};

/// The write end of the self-pipe, for the signal handler. An `AtomicI32` because
/// that is what a handler may touch; `-1` means no handler is installed.
static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(sig: libc::c_int) {
    let fd = SIGNAL_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        // The NUMBER, not a bare byte: the thread below says which signal it was, and Ctrl-C
        // and a `--stop` read differently to a person looking at the log.
        let byte = [sig as u8];
        // Async-signal-safe, and the return value is deliberately ignored: there is
        // nothing a handler could do about a full pipe except make things worse
        // and a full pipe already means a shutdown is pending.
        unsafe {
            libc::write(fd, byte.as_ptr() as *const libc::c_void, 1);
        }
    }
}

/// How long a one-shot may take to come back from its interrupted turn before it goes
/// anyway. Long enough for the killed command's result and the turn's rows to be written
/// short enough that a person who pressed Ctrl-C is not left wondering.
pub const ONE_SHOT_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether a one-shot's last step has returned, set by `cli` the moment its run of steps is over
/// the grace in [`stop_on_signal`] is for a turn that has NOT come back, and a one-shot that spent
/// its own orderly tail would otherwise be killed by its own watchdog, with a false report.
static ONE_SHOT_STEPS_DONE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Say that a one-shot's run of steps is over, so its grace no longer applies.
pub fn one_shot_steps_done() {
    ONE_SHOT_STEPS_DONE.store(true, Ordering::SeqCst);
}

/// The last act before a forced exit: every booted guest comes down and everything a placed session started on a host ends, each line saying where copy-out landed or how many processes went.
/// A forced exit closes no backend, so a VM or placed shell would otherwise outlive the daemon.
fn bring_guests_down() {
    for said in letibot_tools::remote::placements_down() {
        eprintln!("harnessd: {said}");
    }
    for said in letibot_tools::remote::guests_down() {
        eprintln!("harnessd: {said}");
    }
}

/// What one signal does and what a second one does, on its own thread because `Hub::close`
/// takes a mutex and a handler may not. The first names itself and closes the registry
/// which interrupts the turn before it kills the runs holding it
fn stop_on_signal(registry: Arc<Registry>, fd: std::os::fd::RawFd, one_shot: bool) {
    let mut buf = [0u8; 1];
    // Blocks. No timer, no poll: the byte arrives or it does not.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, 1) };
    if n <= 0 {
        return;
    }
    let what = match libc::c_int::from(buf[0]) {
        libc::SIGINT => "SIGINT (Ctrl-C)",
        libc::SIGTERM => "SIGTERM",
        _ => "a signal",
    };
    eprintln!("harnessd: {what} — interrupting the turn and stopping; a second one exits now");
    registry.close();

    let started = std::time::Instant::now();
    loop {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // A one-shot polls in slices so its grace expires; a serving daemon waits for ever, and
        // its thread dies with the process it belongs to.
        let wait_ms = if one_shot {
            let left = ONE_SHOT_EXIT_GRACE.saturating_sub(started.elapsed());
            (left.as_millis().min(200) as i32).max(1)
        } else {
            -1
        };
        let ready = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
        if ready > 0 {
            let mut again = [0u8; 1];
            let n = unsafe { libc::read(fd, again.as_mut_ptr() as *mut libc::c_void, 1) };
            if n > 0 {
                eprintln!(
                    "harnessd: a second {} — exiting now (every row the run enders settled is \
                     written; the turn's own rows are not)",
                    if again[0] == libc::SIGTERM as u8 {
                        "SIGTERM"
                    } else {
                        "Ctrl-C"
                    }
                );
                bring_guests_down();
                std::process::exit(130);
            }
        }
        if one_shot
            && !ONE_SHOT_STEPS_DONE.load(Ordering::SeqCst)
            && started.elapsed() >= ONE_SHOT_EXIT_GRACE
        {
            eprintln!(
                "harnessd: the turn did not come back within {}s of {what} — exiting anyway; the \
                 store is consistent to the last settled row",
                ONE_SHOT_EXIT_GRACE.as_secs()
            );
            bring_guests_down();
            std::process::exit(130);
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

    /// Install SIGINT/SIGTERM handlers that close every session. Closing the registry ends
    /// everything else: every hub closes, `next_command` returns `None`, every attached head
    /// wakes with `Closed` and the worker loop falls out.
    pub fn catch_signals(&mut self, one_shot: bool) -> io::Result<()> {
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
            .spawn(move || stop_on_signal(registry, rfd, one_shot))?;
        self._pipe = Some((read, write));
        Ok(())
    }

    /// Run the daemon's own loop until the registry closes. Arming the session threads is the
    /// first thing it does
    pub fn run(
        &self,
        sessions: &mut Sessions<'_>,
        on_reply: impl Fn(&str, OutcomeSaid<'_>) + Send + Sync + 'static,
    ) {
        let sink: ReplySink = Arc::new(on_reply);
        sessions.serve_each_on_its_own_thread(sink.clone());
        // The worker's wait, with the clock as an input: `next_work_until` gives up at a deadline
        // and says so, which is what lets this loop act on a PAUSE and not only on an event. Three
        // clocks feed it and one never stands down
        loop {
            let deadline = sessions.next_idle_at();
            match self.registry().next_work_until(deadline) {
                // Nothing to do and the deadline is not here yet: the registry never answers
                // this while there is work, and a session with no check armed and no review clock
                // passes `None`, so this arm is reachable only when a clock is actually due.
                WorkOrIdle::Idle => {
                    // The per-session clocks belong to the sessions' own threads: each keeps its plan check
                    // its abandoned-call sweep and its child park THERE, on the one thread that may drive its
                    // harness.
                    if !sessions.each_on_its_own_thread() {
                        // A call nothing will answer is closed out HERE, on the idle pass, and the placement is
                        // the liveness test: the round loop is synchronous, so a round in flight would be holding
                        // this very thread and this line could not run.
                        let pass = std::time::Instant::now();
                        let swept = sessions.sweep_abandoned_calls();
                        crate::instruments::worker_pass("sweep", pass, swept);
                        if swept > 0 {
                            eprintln!("  swept {swept} abandoned tool call(s)");
                        }
                        // And the room's reminders, onto the boards this daemon holds: before the plan check
                        // because that check reads the board and a row for an item this session has just released
                        // would otherwise be asked about for a whole window.
                        let pass = std::time::Instant::now();
                        let held = sessions.fill_room_boards();
                        crate::instruments::worker_pass("room", pass, held);
                        let pass = std::time::Instant::now();
                        let ran = sessions.deliver_due_nags();
                        crate::instruments::worker_pass("todo-check", pass, ran);
                        if ran > 0 {
                            eprintln!("  todo check -> {ran} session(s)");
                        }
                        // And the queue's stuck-row census, on the same idle arm and as a TURN one model call per
                        // interval per project while a row is stuck, because a notice reaches nobody. The clock is
                        // the plan check's, the daemon's clock being the only clock
                        let pass = std::time::Instant::now();
                        let rang = sessions.deliver_due_censuses();
                        crate::instruments::worker_pass("queue-census", pass, rang);
                        if rang > 0 {
                            eprintln!("  queue census -> {rang} session(s)");
                        }
                        // And a reopened subagent idle past its window goes back to the store
                        // (`Config::child_park_after`), on this pass for the sweep's reason: no
                        // round is running, so nothing it holds is mid-call.
                        let pass = std::time::Instant::now();
                        let parked = sessions.park_idle_children();
                        crate::instruments::worker_pass("park", pass, parked);
                        if parked > 0 {
                            eprintln!("  parked {parked} idle subagent(s) in the store");
                        }
                        // And the plugin tick: the hot-reload poll every two seconds, the panes whose `every_s`
                        // has come round, and the notices a layer owes its session's model. Never inside a turn
                        // so `plugin_tick` starts the gate (which runs `cargo`) on a thread and returns
                        sessions.plugin_tick();
                    }
                    // And the reviews this daemon's sessions host, read from the store: the
                    // durable half of the queue's ring, due on its own clock rather than on this
                    // pass's, because this arm is also entered for a nag or a sweep.
                    if sessions.reviews_due() {
                        // The review queue's own SESSION, started once and then nagged like any other
                        match sessions.ensure_gatekeeper() {
                            Ok(true) => eprintln!(
                                "  the review queue is session {} — attach to it and its board is the \
                                 queue",
                                sessions.review_session()
                            ),
                            Ok(false) => {}
                            Err(e) => eprintln!("  the review queue's session did not open: {e}"),
                        }
                        sessions.fill_gatekeeper_board();
                        let pass = std::time::Instant::now();
                        let served = sessions.serve_reviews();
                        crate::instruments::worker_pass("review", pass, served);
                        if served > 0 {
                            eprintln!("  reviews -> {served} gatekeeper(s)");
                        }
                    }
                    // And the daemon itself goes when nothing has needed it for its window
                    // (`Config::idle_exit_after`), last on the pass so a review or a nag the pass just started
                    // holds it.
                    if sessions.idle_exit_due() {
                        eprintln!(
                            "  idle for {} min: no head, turn, job, subagent, monitor or queue work \
                             — exiting; every session is in the store",
                            sessions.idle_window().map_or(0, |w| w.as_secs() / 60)
                        );
                        self.registry().close();
                        break;
                    }
                    continue;
                }
                WorkOrIdle::Closed => break,
                WorkOrIdle::Work(work) => match work {
                    // A session was created, by a head's `/new` or by a `--continue` asking for one out of the
                    // store. Opened HERE, on the worker, rather than on the connection thread that asked
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
                        // The sweep is armed BEFORE the turn runs: a turn can end with a tool call nothing
                        // answered, and only an idle pass closes such a call out. `turn_tail::run_turn` arms it
                        // which every door's turn goes through, so a door reached some other way cannot forget it.
                        let at = crate::instruments::Clock::now();
                        let outcome = sessions.dispatch(&session_id, &cmd);
                        crate::instruments::worker_cmd(&session_id, &cmd, at, &outcome);
                        sink(&session_id, OutcomeSaid::Command(&cmd, &outcome));
                    }
                    // The session a head closed is let go of here: the registry entry went on the connection's
                    // thread, and what is left is the harness, its backend and its children, which only
                    // `Sessions::close` knows how to drop.
                    Work::Close(session_id) => match sessions.close(&session_id) {
                        Some(said) if said.is_empty() => {
                            eprintln!("  {session_id} · closed; the store keeps its rows")
                        }
                        // A placement's branch and its commit, or why neither came: the operator
                        // has no other way to learn where the work landed.
                        Some(said) => {
                            eprintln!("  {session_id} · closed; the store keeps its rows. {said}")
                        }
                        None => eprintln!(
                            "  {session_id} · closed (it was not open here); the store keeps its \
                             rows"
                        ),
                    },
                    Work::Woken(session_id) => {
                        // A monitor fired while nothing was running, served on the worker like everything else and
                        // AFTER every queued command: a head that pressed enter is waiting, and a monitor is not.
                        // `Ignored` is a real and common outcome
                        match sessions.wake(&session_id) {
                            outcome @ Outcome::Replied(_) => {
                                sink(&session_id, OutcomeSaid::Wake(&outcome))
                            }
                            outcome @ Outcome::Failed(_) => {
                                sink(&session_id, OutcomeSaid::Wake(&outcome))
                            }
                            // A wake never compacts, and `Ignored`/`HandedOn` are the two answers
                            // that say nothing happened here: a settlement already taken at a round
                            // boundary, and a session whose own thread was handed it.
                            Outcome::Compacted(_) | Outcome::Ignored | Outcome::HandedOn => {}
                        }
                    }
                },
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
    /// Whether the daemon should keep serving after this. A failed turn is not a failed daemon
    /// a failed turn is a recorded outcome. Only a setup or store failure ends the process
    /// because the next turn would fail the same way and the rows would stop being written.
    pub fn is_fatal(&self) -> bool {
        matches!(self, HarnessError::Setup(_) | HarnessError::Store(_))
    }
}
