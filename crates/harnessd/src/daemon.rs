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
//! **The worker below is a dispatcher now, and every session has a reader of its own.** The
//! operator's requirement, in their words: *"one daemon, several sessions … give a thread to each
//! session and that is it"*. Until this, one thread took one command at a time across every
//! session, and a prompt to the second waited for the first's three-minute turn — measured
//! 2026-10-26 in the operator's own daemon: a prompt to session B sat queued from 14:08 to
//! 14:10:52, exactly one second after session A's turn ended.
//!
//! [`Sessions::serve_each_on_its_own_thread`] is what arms it, and [`crate::sessions::Court`] is
//! the shape: the same one a subagent already had (`harness::serve_child`). The loop here keeps
//! what is the DAEMON's — opening sessions, the review queue's own session, the idle exit — and
//! the per-session clocks (the plan check, the abandoned-call sweep, the child park) moved onto
//! the threads that may drive the harnesses they read.
//!
//! What that costs and why it is worth it: `TurnEngine` borrows the vocabulary and the dialect's
//! renderer, and two threads need both to be `Sync` — which they are (`Parts` is three `Arc`s,
//! shared read-only by every session's engine, which is what it was always for). The thing that
//! was being serialised was never a shared llama.cpp server: it was one thread doing one turn at a
//! time for unrelated conversations. A box with one GPU still arbitrates that where it always
//! did — in the server's own slots.
//!
//! # Why there is no async runtime and no timer
//!
//! §18.1-I12: *"the daemon spawns no timer or poll loop for message delivery"*.
//! [`Hub::take_own_work`] blocks on a condvar until something is pushed, and the
//! accept loop blocks on `accept`. Shutdown arrives on a **self-pipe** — the signal
//! handler writes one byte, a thread blocked in `read` wakes and closes the hub —
//! rather than on a flag some loop polls. Three blocking reads and no clock.
//!
//! **One `poll`, and only after a signal** (2026-10-26): [`stop_on_signal`] waits on the same
//! pipe for a SECOND signal — a person pressing Ctrl-C twice means it — and a one-shot's grace
//! (`ONE_SHOT_EXIT_GRACE`) is a bound on the wait, so that thread does poll in slices. It is not
//! a delivery loop: nothing is delivered through it, and it exists only once a stop has begun.
//!
//! That also settles what a signal handler is allowed to do here. `Hub::close`
//! takes a mutex and notifies a condvar; neither is async-signal-safe, so calling
//! it from the handler is a deadlock waiting for a busy Tuesday. `write(2)` on a
//! pipe is on the safe list, so the handler writes and the thread does the work.

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
        // **The number, not a bare byte** (2026-10-26): the thread below says which signal it
        // was, and Ctrl-C and a `--stop` are two different things to a person reading the log.
        let byte = [sig as u8];
        // Async-signal-safe, and the return value is deliberately ignored: there is
        // nothing a handler could do about a full pipe except make things worse,
        // and a full pipe already means a shutdown is pending.
        unsafe {
            libc::write(fd, byte.as_ptr() as *const libc::c_void, 1);
        }
    }
}

/// **How long a one-shot may take to come back from its interrupted turn** before it goes anyway.
///
/// Long enough for the interrupt to reach a round boundary, the killed command's result to be
/// recorded and the turn's rows to be written; short enough that a person who pressed Ctrl-C is
/// not left wondering. Only a one-shot gets it — see [`stop_on_signal`].
pub const ONE_SHOT_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// **Whether a one-shot's last step has returned** — set by `cli` the moment its run of steps is
/// over, because the grace in [`stop_on_signal`] is for a turn that has NOT come back.
///
/// Without this a one-shot whose turn was interrupted properly and then spent its own orderly
/// tail (five seconds of subagent threads, a flowy seat stopping) would be killed by its own
/// watchdog on the way out, with a line saying the turn never came back. That is a false report at
/// the one moment the operator is reading carefully.
static ONE_SHOT_STEPS_DONE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Say that a one-shot's run of steps is over, so its grace no longer applies.
pub fn one_shot_steps_done() {
    ONE_SHOT_STEPS_DONE.store(true, Ordering::SeqCst);
}

/// **What one signal does, and what a second one does** — the whole of the signal path, on its own
/// thread because `Hub::close` takes a mutex and a handler may not.
///
/// 2026-10-26, the operator at a terminal in `~/Projects/stroppy-io`: a one-shot had a turn in
/// flight, they pressed Ctrl-C, nothing visible happened, the model went on running `ssh`
/// commands, and the process took minutes to go. Two defects, and the first is the one that ate
/// the minutes:
///
/// 1. **Nothing said anything.** The daemon's first word about a signal was its last, so a stop
///    that was working and a stop that was ignored looked exactly alike.
/// 2. **A second Ctrl-C did nothing either.** The old contract was *"a second signal is not
///    special-cased: the first one already started an orderly stop, and a 'force' path is a way to
///    lose the last turn's rows"* — which is right about the rows and wrong about the person: they
///    press it because the first one did not appear to work, and having no second gear at all is
///    what makes them wait minutes for a process they could have ended.
///
/// So: one line naming the signal and the stop; then `close`, which interrupts the turn BEFORE it
/// kills the runs it is holding (`Registry::close`); then a SECOND signal exits at once, since by
/// then the run enders have settled every row they own and sqlite's WAL is consistent.
///
/// **And a one-shot gets a bound.** A serving daemon's orderly stop is bounded by its own steps and
/// cutting it short throws away work it is finishing; a one-shot has nothing to finish — the step
/// in flight is exactly what the person is asking to be rid of — so if the turn has not come back
/// within [`ONE_SHOT_EXIT_GRACE`] it goes anyway, and says why.
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

    /// Install SIGINT/SIGTERM handlers that close every session.
    ///
    /// Closing the registry is what ends everything else: every hub closes, the
    /// bell closes, `next_command` returns `None`, every attached head wakes with
    /// `Closed`, and the worker loop falls out. What each signal says, and what a SECOND one does,
    /// is [`stop_on_signal`]'s — this only installs it.
    ///
    /// `one_shot` is the one thing that differs between the two ways a daemon is run: a run of
    /// steps has no orderly stop worth waiting for, so it is given a bound.
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

    /// Run the daemon's own loop until the registry closes.
    ///
    /// # The dispatcher, and the readers it hands work to
    ///
    /// **Arming the threads is the first thing it does** ([`Sessions::serve_each_on_its_own_thread`]),
    /// and after that the two arms that used to run a turn — `Work::Command` and `Work::Woken` — are
    /// reachable only for a session with no court: `Hub::try_command` answers `None` for a hub owned
    /// by its own thread, so its commands are never popped here, and a wake for one is handed to
    /// that thread. They are kept, and kept working, because they are the same two arms a daemon
    /// without threads has always run and the log lines are the same.
    ///
    /// What stays here is what belongs to the DAEMON: a session being created, the review queue's
    /// own pass, the idle exit. The per-session clocks moved to the sessions' threads.
    pub fn run(
        &self,
        sessions: &mut Sessions<'_>,
        on_reply: impl Fn(&str, OutcomeSaid<'_>) + Send + Sync + 'static,
    ) {
        let sink: ReplySink = Arc::new(on_reply);
        sessions.serve_each_on_its_own_thread(sink.clone());
        // **The worker's wait, with the clock as an input.** `next_work_until` gives up at a
        // deadline and says so, which is what lets this loop act on a PAUSE rather than only on an
        // event — and the one thing that wanted to is the idle plan-check the operator asked for:
        // *"maybe wait for a timeout actually. so send it when model is idling."* The deadline is
        // recomputed every pass because any turn re-arms it.
        //
        // **Three clocks, one deadline, and one of them never stands down.** The plan-check and
        // the sweep are `next_nag_at`'s; the third is the store read for the merge-queue reviews
        // the sessions this daemon holds are owed, which is what makes a review asked for by
        // ANOTHER daemon (or by a daemon that then restarted) arrive at all — a ring is a bell and
        // a bell is per-daemon, so there is no event to wait for. See `Sessions::next_idle_at`.
        loop {
            let deadline = sessions.next_idle_at();
            match self.registry().next_work_until(deadline) {
                // Nothing to do and the deadline is not here yet — the registry never returns this
                // while there is work, and a session with no check armed and no review clock
                // passes `None`, so this arm is only reachable when one of the three clocks is
                // actually due.
                WorkOrIdle::Idle => {
                    // **The per-session clocks are the sessions' own now.** Every session on a
                    // thread of its own keeps its plan check, its abandoned-call sweep and its
                    // child park THERE — on the one thread that may drive its harness — so this
                    // pass does not ask them anything it would then stand still waiting for. The
                    // block below is what a daemon that never armed the threads still does here,
                    // and it is the same code every fixture drives.
                    if !sessions.each_on_its_own_thread() {
                        // **A call nothing will answer is closed out HERE**, on the idle pass, and the
                        // placement is the liveness test: the round loop is synchronous, so a round in
                        // flight would be holding this very thread and this line could not run. See
                        // `Sessions::sweep_abandoned_calls`.
                        let pass = std::time::Instant::now();
                        let swept = sessions.sweep_abandoned_calls();
                        crate::instruments::worker_pass("sweep", pass, swept);
                        if swept > 0 {
                            eprintln!("  swept {swept} abandoned tool call(s)");
                        }
                        // **And the room's reminders, onto the boards this daemon holds.** Before
                        // the plan check, because that check reads the board and a row for an item
                        // this session has just released would otherwise be asked about for a
                        // whole window. A session on its own thread runs its own copy
                        // (`court_main`), because its board is on that thread; this is the
                        // daemon's half, and it is the only caller a fixture that drives
                        // `Sessions` by hand gets. See `crate::room::remind`.
                        let pass = std::time::Instant::now();
                        let held = sessions.fill_room_boards();
                        crate::instruments::worker_pass("room", pass, held);
                        let pass = std::time::Instant::now();
                        let ran = sessions.deliver_due_nags();
                        crate::instruments::worker_pass("todo-check", pass, ran);
                        if ran > 0 {
                            eprintln!("  todo check -> {ran} session(s)");
                        }
                        // **And the queue's stuck-row census, on the same idle arm and as a
                        // NOTICE** — `Harness::note_to_model`, a transcript row and no turn, so
                        // it costs no model call. The clock is the plan-check's own
                        // (`TodoNagClock::census_due_at`), because the daemon's clock is the only
                        // clock; what it says is `mergequeue::stuck`'s, and how often is
                        // `STUCK_REPEAT_MS`. See `Sessions::note_due_censuses`.
                        let pass = std::time::Instant::now();
                        let noted = sessions.note_due_censuses();
                        crate::instruments::worker_pass("queue-census", pass, noted);
                        if noted > 0 {
                            eprintln!("  queue census -> {noted} session(s)");
                        }
                        // **And a reopened subagent idle past its window goes back to the store**
                        // (R68's idle-child policy, `Config::child_park_after`). On this pass for the
                        // sweep's reason: no round is running, so nothing it holds is mid-call.
                        let pass = std::time::Instant::now();
                        let parked = sessions.park_idle_children();
                        crate::instruments::worker_pass("park", pass, parked);
                        if parked > 0 {
                            eprintln!("  parked {parked} idle subagent(s) in the store");
                        }
                        // **And the plugin tick** (W4 of `docs/plugins-slice1.md`): the hot-reload
                        // poll every two seconds, the panes whose `every_s` has come round, and the
                        // notices a layer owes its session's model. On this pass and never inside a
                        // turn, so `plugin_tick` starts the gate — which runs `cargo` — on a thread
                        // and returns: a session's work must not wait for a compiler, and a pane's
                        // query is answered on its own connection's thread.
                        sessions.plugin_tick();
                    }
                    // **And the reviews this daemon's sessions host**, read from the store — the
                    // durable half of the queue's ring. It is due on its own clock rather than on
                    // this pass's, because this arm is also entered for a nag or a sweep. See
                    // `Sessions::serve_reviews` for why the store and not the ring.
                    if sessions.reviews_due() {
                        // **The review queue's own SESSION, started once and then nagged like any
                        // other.** The operator's design: *"reviews are subagents managed by the
                        // main gatekeeper session which is hardcoded … it is a normal auto-driving
                        // session that treats review queue as todo list - so nags and all"*. So the
                        // session exists first and its BOARD is the queue — one row per entry
                        // waiting for a verdict, filled from the store on this same clock — and the
                        // review is a `task` CHILD of it: `serve_reviews` below starts one for each
                        // row nobody is working, and the row then says which child it is.
                        //
                        // Silence is the ordinary case: `Ok(false)` is every pass after the first
                        // (the session is already open), and the rows say nothing of their own — a
                        // session's board is read by attaching to it or by the pane. Only a session
                        // that was STARTED and one that could not open are worth a line.
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
                    // **And the daemon itself goes when nothing has needed it for its window**
                    // (`Config::idle_exit_after`) — last on the pass, so a review or a nag that
                    // the pass just started holds it. The registry is closed exactly as a signal
                    // closes it, so the stop that follows is the ordinary one.
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
                        // **ARM THE SWEEP BEFORE THE TURN RUNS.** A turn can end with a tool call
                        // nothing answered — the executor thread and its process can both vanish —
                        // and the mechanism that closes such a call out can only run on an idle
                        // pass. Without this deadline the worker would sleep until the next command
                        // and the stranded call would sit in the head exactly as it did for the
                        // operator: eight minutes of a spinner with `esc esc` dead.
                        //
                        // **Armed by the turn itself now, not here** (R68.4): every door's turn goes
                        // through `turn_tail::run_turn`, which arms the sweep before the turn — so a
                        // door reached some other way (a scripted submit, a test, a future arm)
                        // cannot forget it, which is how the nag door did (R67 item 10).
                        let at = crate::instruments::Clock::now();
                        let outcome = sessions.dispatch(&session_id, &cmd);
                        crate::instruments::worker_cmd(&session_id, &cmd, at, &outcome);
                        sink(&session_id, OutcomeSaid::Command(&cmd, &outcome));
                    }
                    // **The session a head closed is let go of here** — `ClientFrame::CloseSession`,
                    // which a migration sends before it copies the rows to another machine
                    // (`docs/remote-hosts.md` RH4). The registry entry went on the connection's
                    // thread; what is left is the harness, its backend and its children, and this
                    // is the one place that knows how to drop them (`Sessions::close`).
                    //
                    // Said out loud, because a session that stops existing under a head that is
                    // still attached is otherwise the quiet shape this tree has been bitten by
                    // before: the head sees its socket close and cannot say whether the daemon
                    // went or the session did.
                    Work::Close(session_id) => {
                        let held = sessions.close(&session_id);
                        eprintln!(
                            "  {session_id} · closed{}; the store keeps its rows",
                            if held { "" } else { " (it was not open here)" }
                        );
                    }
                    Work::Woken(session_id) => {
                        // **A monitor fired while nothing was running.** T24's *"wakes the
                        // loop when it fires"*, which until now had no caller: a firing was
                        // visible in `job_list` and nothing acted on it, which is a poll.
                        //
                        // It is served on the worker like everything else — one
                        // authoritative reader (§13.2) — and it is served **after** every
                        // queued command, because the bell drains wakes last and a head
                        // that pressed enter is waiting while a monitor is not.
                        //
                        // `Ignored` is a real outcome here and the common one under load: a
                        // firing the running turn already picked up through steering has
                        // been delivered, and the shared cursor is what stops the wake
                        // telling the model the same thing twice.
                        //
                        // **AND A WAKE ARMS THE SWEEP TOO, WHICH IT DID NOT.** A wake runs a turn
                        // whenever it has anything to say — a child's settlement, a background job's
                        // completion, a merge-queue ring that finds an entry waiting for a gate —
                        // and that turn dispatches tool calls exactly as a prompt's does, so it can
                        // strand one exactly as a prompt's does. Only the command arm above armed
                        // this deadline, so a call stranded by a WAKE-driven turn was never looked
                        // at: the worker went back to a wait with no clock in it at all.
                        //
                        // MEASURED 2026-10-10, and it is why this line is here: the last three
                        // turns before a nine-hour silence were all `monitor -> …` — wake-driven,
                        // every one — and the daemon was woken only by the operator's command the
                        // next morning. The nag is not this deadline and cannot be: `nag_should_arm`
                        // stands a session down for an unchanged plan BY DESIGN, and a wake-driven
                        // turn over an unchanged plan leaves the worker nothing to wake for. The
                        // sweep is the clock that exists for exactly this — a call nothing
                        // answered — and a turn that ends is what arms it. The shared turn tail
                        // arms it (R68.4), for this door and every other.
                        match sessions.wake(&session_id) {
                            outcome @ Outcome::Replied(_) => {
                                sink(&session_id, OutcomeSaid::Wake(&outcome))
                            }
                            outcome @ Outcome::Failed(_) => {
                                sink(&session_id, OutcomeSaid::Wake(&outcome))
                            }
                            // A wake never compacts, and `Ignored`/`HandedOn` are the two answers
                            // that say nothing happened here — a settlement already taken at a
                            // round boundary, and a session whose own thread was handed it. The
                            // outcome is the worker's one vocabulary, so the arms stay.
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
