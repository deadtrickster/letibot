//! **The operator's own run, on a thread of its own** — and the shape of the handoff
//! in both directions.
//!
//! # The defect this exists for
//!
//! `CommandKind::OperatorShell` used to run on the daemon's single worker, and the run
//! inside it — [`ToolRuntime::invoke_operator`] waiting on the command's process — held
//! that worker for as long as the command did. One worker over many sessions is §13.2's
//! design and is not the defect; a run of the operator's OWN holding it was. Their words,
//! after it bit them twice in one day: *"while a run of theirs is in flight, nothing else
//! runs. another `!` line only queues."* While `! sudo apt install mc` sat at its
//! question, a turn in another session queued behind it, a `!send` was answerable and
//! nothing else was — the daemon was not busy, it was **hung**, and from the operator's
//! seat there was no difference to see.
//!
//! # The shape: one thread per run, two handoffs, all state on the worker
//!
//! [`crate::sessions::Sessions`] owns this module's only state: whether a run is in
//! flight for a session, and the lines queued behind it. The thread itself is spawned per
//! run and holds nothing but what the run needs — the minted call, the runtime handle,
//! the hub — and its whole life is:
//!
//! ```text
//! worker                          the run's own thread
//!   │ prepare (mint bang-<n>)           │
//!   ├────────── spawn ────────────────> │ invoke_operator (the wait lives HERE)
//!   │ free: other sessions, turns,      │ cards, !send, promotion, deadline — unchanged,
//!   │ nags, sweeps, second `!` queued   │ because they all live in the tool's wait loop
//!   │ <────────── submit_daemon ────────┤ OperatorShellResult (the run is over)
//!   │ settle: note + two rows + turn    │
//!   │ start the next queued line, if any│
//! ```
//!
//! The result comes back **as a command** ([`CommandKind::OperatorShellResult`] through
//! [`Hub::submit_daemon`]) rather than over a channel, and that is not convenience: the
//! queue is the one door whose ordering already holds (a head that pressed enter while the
//! run was in flight is served first — `Bell::next_any` puts commands ahead of wakes), the
//! worker is the one writer of the transcript and appends the rows itself, and a settle
//! that arrives mid-turn simply waits its turn in the queue instead of needing a second
//! mechanism to park it.
//!
//! # One run per session, and who enforces it
//!
//! [`letibot_sessionlog::PromptDriver`] holds ONE input handle per session — *an input
//! handle belongs to one session's run* — so two concurrent runs would make the second
//! `opened` steal the first's card and `!send`. The worker therefore starts a queued line
//! only when the previous one has settled, and the enforcement lives there (worker-owned
//! state, no lock) rather than here: this module never says no, because it is never asked
//! to run two.
//!
//! # What the thread does with a panic
//!
//! A panic on the worker would have taken the whole daemon down; a panic HERE must not
//! lose the operator's line silently. The whole body runs under [`std::panic::catch_unwind`],
//! and both of its arms end at the same place: a settle command, carrying either what the
//! run produced or a `Failed` outcome naming the panic. The rows land once either way —
//! which is the difference between a harness that reports and one that swallows.
//!
//! # What is deliberately NOT here
//!
//! **No join, no stop flag.** The thread ends when its handoff does; at daemon shutdown
//! the run ender ([`letibot_sessionlog::registry::RunEnder`]) kills the in-flight command's
//! process, so the wait returns and the settle is attempted against a hub that is closing —
//! [`Hub::submit_daemon`] answers `false`, the loss is said on stderr, and the thread is
//! gone. A join would hold the daemon's exit for the length of a killed process's reap,
//! which is exactly the shutdown cost §18.1 refuses to pay twice.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use letibot_sessionlog::hub::{CommandKind, Hub, ShellSpill};
use letibot_tools::ToolRuntime;
use letibot_transcript::ToolCall;

/// **Whether the operator's run for this session is in flight, and what waits behind it.**
///
/// One operator run per session at a time is [`letibot_sessionlog::PromptDriver`]'s own rule
/// — it holds ONE input handle, so a second concurrent run would have its first act steal
/// the first's card and `!send`. This is the enforcement, shared by the two places that can
/// start a run:
///
/// * [`crate::sessions::Sessions::dispatch`], between turns — a line arriving while one is
///   in flight is queued here rather than handed to a second thread;
/// * the round boundary's pickup (`Harness::apply_queued_head_run`), mid-turn — a `!` line
///   typed while a turn runs is parked here for the same reason, instead of running
///   synchronously on top of the run in flight. When NOTHING is in flight the pickup runs
///   the line exactly as it always did, so the common mid-turn case keeps its timing.
///
/// Both touchpoints are the worker's thread (the pickup runs inside a turn the worker is
/// holding), so the mutex below arbitrates re-entrant takes on one thread and nothing else;
/// it exists because the state is shared through an `Arc` and Rust asks for interior
/// mutability, not because two machines ever hold it.
///
/// The pending lines are drained by the settle arm, one per settle — the line started there
/// settles in its own command and drains the next — so a burst of typed lines runs in the
/// order they were typed, one at a time, with the worker free between them.
pub struct State {
    /// True from the hand-off until the settle arm has appended the rows. Set BEFORE the
    /// thread is spawned, so two lines submitted back to back cannot race the spawn.
    in_flight: AtomicBool,
    /// Lines waiting for the run in flight to settle, in submission order. Only the
    /// worker touches this, from its two call sites.
    pending: Mutex<VecDeque<(String, String)>>,
}

impl State {
    pub fn new() -> Arc<State> {
        Arc::new(State {
            in_flight: AtomicBool::new(false),
            pending: Mutex::new(VecDeque::new()),
        })
    }

    /// **A run is going.** Called by the worker at hand-off, before the thread exists.
    pub fn start(&self) {
        self.in_flight.store(true, Ordering::SeqCst);
    }

    /// **The run settled.** Clears the flag; what waited is taken one line at a time
    /// with [`State::pop_pending`], by the settle arm that called this.
    pub fn end(&self) {
        self.in_flight.store(false, Ordering::SeqCst);
    }

    /// **A line typed while a run was in flight.** It runs when that run settles.
    pub fn queue(&self, line: String, who: String) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back((line, who));
    }

    /// The next line that waited, in submission order.
    pub fn pop_pending(&self) -> Option<(String, String)> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }

    /// Whether a run is in flight right now. The mid-turn pickup reads this to decide
    /// between running a line synchronously (nothing in flight — the old behaviour) and
    /// parking it here (a run would otherwise be overlapped).
    pub fn in_flight(&self) -> bool {
        self.in_flight.load(Ordering::SeqCst)
    }
}

/// **Run one operator command on a thread of its own, and hand the result back.**
///
/// Called by the worker with everything minted ([`crate::harness::Harness::prepare_operator_shell`]
/// made the `bang-<n>` id — that counter is the session's, so it is incremented THERE, once,
/// in submission order). `Err` only when the thread could not be started at all; the caller
/// runs the command synchronously rather than lose it, and says why on stderr.
///
/// The thread holds an [`Arc<ToolRuntime>`] — one exec host per session is a fact, so the
/// run goes through the session's own runtime — and nothing else that outlives this call.
pub fn spawn_run(
    session: String,
    call_id: String,
    call: ToolCall,
    line: String,
    who: String,
    runtime: Arc<ToolRuntime>,
    hub: Arc<Hub>,
) -> Result<(), String> {
    let name = format!("bang-{call_id}");
    std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || run_and_hand_back(&session, call_id, call, line, who, runtime, hub))
        .map(|_| ())
        .map_err(|e| format!("the thread for the operator's run could not be started: {e}"))
}

/// The thread's whole body. Deliberately small enough to read as one sentence:
/// wait for the command, hand back what it produced — once, whichever way it ends.
fn run_and_hand_back(
    session: &str,
    call_id: String,
    call: ToolCall,
    line: String,
    who: String,
    runtime: Arc<ToolRuntime>,
    hub: Arc<Hub>,
) {
    // The runtime's own events go to a null sink, exactly as the synchronous path's
    // do: the transcript row is the durable record and every head draws *it*.
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut quiet = letibot_tools::events::NullToolSink;
        // `turn_id` empty on purpose, as the door's runs are: this call belongs to no
        // turn, and a head that saw a turn id would draw the row inside a turn that
        // did not propose it.
        let result = runtime.invoke_operator("", &call, &mut quiet);
        let payload = result.render();
        let spill = result.spill.as_ref().map(|s| ShellSpill {
            full_bytes: s.full_bytes,
            hash: s.hash.clone(),
        });
        (result.outcome, payload, spill)
    }));
    let (outcome, payload, spill) = match ran {
        Ok(ok) => ok,
        // **Said, and the rows still land.** A panic here used to be unreachable — the
        // run held the worker, so it took the daemon with it. On its own thread it would
        // otherwise be a silent loss: the line was accepted, the command may even have
        // run, and nothing would ever say what happened to it.
        Err(p) => {
            eprintln!(
                "  {session} · the operator's run `{call_id}` panicked on its own thread; \
                 its row is appended as a failure naming that"
            );
            (
                letibot_transcript::ToolOutcome::Failed {
                    reason: format!(
                        "the run's own thread panicked before it could report: {p:?}. \
                         The command may have run; what is missing is its output."
                    ),
                },
                String::new(),
                None,
            )
        }
    };
    let queued = hub.submit_daemon(
        who.clone(),
        // The line's own submission carried the head's request id; the settle carries
        // the call id, which is this session's and unique — one string names the run
        // end to end.
        call_id.clone(),
        CommandKind::OperatorShellResult {
            line,
            who,
            call_id,
            outcome,
            payload,
            spill,
        },
    );
    if !queued {
        // The hub is closed: the daemon is stopping, and the run ender has already
        // ended this command's process. A row appended now would go to a queue
        // nobody drains.
        eprintln!(
            "  {session} · the operator's run finished after the daemon stopped; \
             its rows were not appended"
        );
    }
}
