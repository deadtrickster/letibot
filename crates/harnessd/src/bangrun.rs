//! The operator's own run, on a thread of its own: one thread per run, two
//! handoffs, and all of the state on the worker.
//!
//! An operator's run must not hold the daemon's single worker for as long as the
//! command does, or a turn in another session queues behind it and the daemon looks
//! hung. The result comes back as a command, not over a channel, because the queue's
//! ordering already holds and the worker is the one writer of the transcript.
//!
//! The worker spawns the thread and frees itself; its whole life is
//!
//! ```text
//! worker                          the run's own thread
//!   │ prepare (mint bang-<n>)           │
//!   ├────────── spawn ────────────────> │ invoke_operator (the wait lives HERE)
//!   │ free: other sessions, turns,      │ cards, !send, promotion, deadline - unchanged,
//!   │ nags, sweeps, second `!` queued   │ because they all live in the tool's wait loop
//!   │ <────────── submit_daemon ────────┤ OperatorShellResult (the run is over)
//!   │ settle: note + two rows + turn    │
//!   │ start the next queued line, if any│
//! ```

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use letibot_sessionlog::hub::{CommandKind, Hub, ShellSpill};
use letibot_tools::ToolRuntime;
use letibot_transcript::ToolCall;

use crate::jobwatch::{JobWatchSink, JobWatchers};

/// Whether the operator's run for this session is in flight, and what waits behind it. One
/// run per session is `PromptDriver`'s own rule (one input handle), and this is the
/// enforcement.
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

    /// A run is going. Called by the worker at hand-off, before the thread exists.
    pub fn start(&self) {
        self.in_flight.store(true, Ordering::SeqCst);
    }

    /// The run settled. Clears the flag; what waited is taken one line at a time with
    /// [`State::pop_pending`], by the settle arm that called this.
    pub fn end(&self) {
        self.in_flight.store(false, Ordering::SeqCst);
    }

    /// A line typed while a run was in flight. It runs when that run settles.
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
    /// between running a line synchronously (nothing in flight) and parking it here.
    pub fn in_flight(&self) -> bool {
        self.in_flight.load(Ordering::SeqCst)
    }
}

/// One operator line on its way to a thread of its own, with everything already minted. A
/// value rather than eight positional arguments: four of them are strings a reader would
/// have to tell apart by position.
pub struct OperatorRun {
    pub session: String,
    pub call_id: String,
    pub call: ToolCall,
    pub line: String,
    pub who: String,
    pub runtime: Arc<ToolRuntime>,
    pub hub: Arc<Hub>,
    pub watch: Option<Arc<JobWatchers>>,
}

/// Run one operator command on a thread of its own, and hand the result back. Called by the
/// worker with everything minted: the `bang-<n>` counter is the session's, so it is
/// incremented there, once, in submission order.
pub fn spawn_run(run: OperatorRun) -> Result<(), String> {
    let name = format!("bang-{}", run.call_id);
    std::thread::Builder::new()
        .name(name)
        .spawn(move || run_and_hand_back(run))
        .map(|_| ())
        .map_err(|e| format!("the thread for the operator's run could not be started: {e}"))
}

/// The thread's whole body: wait for the command, hand back what it produced, once
/// whichever way it ends.
fn run_and_hand_back(run: OperatorRun) {
    let OperatorRun {
        session,
        call_id,
        call,
        line,
        who,
        runtime,
        hub,
        watch,
    } = run;
    // The runtime's events go to a null sink, exactly as the synchronous path's do: the
    // transcript row is the durable record and every head draws it. Null, and WATCHED
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut quiet = JobWatchSink::new(letibot_tools::events::NullToolSink, watch);
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
        // Said, and the rows still land. On the worker a panic here took the daemon
        // with it; on its own thread it would otherwise be a silent loss, since the
        // line was accepted and the command may even have run.
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
    // Take any promote request that outlived the wait. A request that landed in the
    // final window has no consumer left, and a flag left set is a trap: the next
    // unrelated foreground call in this session would take it and promote itself.
    let mut payload = payload;
    if hub.take_promote_request().is_some() {
        payload.push_str(
            "\n\n[a background request (Ctrl+O) arrived as this run ended; there was \
             no process left to move]",
        );
    }
    let queued = hub.submit_daemon(
        who.clone(),
        // The line's own submission carried the head's request id; the settle carries
        // the call id, which is this session's and unique, so one string names the run
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
