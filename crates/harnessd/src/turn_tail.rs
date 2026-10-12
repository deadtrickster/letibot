//! **One turn tail for every door** — `TODO.md` R68.4.
//!
//! A turn reaches a session through seven doors: a head's prompt, a peer's or parent's message,
//! a wake (a monitor fired, a job or child settled, a condition came due), the idle plan-check,
//! the operator's own `!` line, a subagent's task, and the continuation after a wall. Each one
//! used to carry its own copy of what has to happen around a turn, and the copies had drifted.
//! The 2026-10-11 review of the daemon (`TODO.md` section 4, *the daemon*, finding 6) listed what
//! each copy was missing:
//!
//! * `arm_wake` was called by the prompt, the message and the continuation — not by the wake, the
//!   nag, or either `!` door, so a monitor first declared in one of those turns got no waiter
//!   until the next prompt;
//! * the nag door armed neither the sweep nor the wake (R67 item 10 patched that one door);
//! * children ran a SECOND copy (`Harness::submit_spoken_as_a_normal_session`, plus
//!   `harness::serve_child`): no forced compaction on a wake or nag turn at the wall, never swept
//!   (the sweep walked only `Sessions.open`), `take_new_title` never drained (a child's picker
//!   row never took its title), no refused-compaction latch shared with the daemon's, and the
//!   `Answer` arm dropped its error (`.is_ok()`), so a child's `turn_failed` carried no reason.
//!
//! So the sequence lives here, once, and both owners call it: [`run_turn`] is what
//! `Sessions` runs for every door of a session the daemon's worker holds, and what
//! `harness::serve_child` runs for every door of a subagent on its own thread. What differs
//! between the two owners is not the SEQUENCE but where its effects land — a title goes to the
//! registry either way but a root's also renames its seat on the fabric; the sweep is the
//! worker's clock for one and the child's own for the other — and that is the [`TurnOwner`]
//! trait: the owner answers *how*, this module decides *when*.
//!
//! # The sequence, in order
//!
//! Before the turn: **arm the sweep** (a turn can strand a tool call, and only an idle pass can
//! close it out); **compact if the next turn would not fit** (the pre-wall check); **earn the
//! plan-check back** when somebody spoke (`Prompt`, `Message`); **start the turn's clock**.
//!
//! After each turn — the first and every continuation: **drain the title**; **arm the monitor
//! wake**; **publish a failure** (`TurnFailed` + `turn_failed`, with the reason); **compact** — the
//! pre-emptive door after a success, the refused door after a wall, and say so by name when the
//! refused door declines; and **continue past a wall** while there is measured room, at most
//! [`crate::sessions::WALL_CONTINUES`] times.
//!
//! After the last: **stop the clock**, then the **owner's idle clocks** (the plan-check is
//! re-armed, a spent nag is recorded as said, a subagent's park window starts again).
//!
//! The `!` state is the one obligation that is not here: a settled run's flag comes down in the
//! `OperatorShellResult` arm BEFORE any return (R67 item 11), because it must come down whether
//! or not a turn follows, and a turn tail cannot run for a turn that never starts.

use std::sync::Arc;

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;

use crate::harness::{Harness, HarnessError, Reply};

/// **Which door a turn came through** — what the shared tail needs to know about it, and nothing
/// more. The difference between doors is in the turn they run (the closure [`run_turn`] is
/// handed); the tail itself differs only where a door says something about the idle clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Door {
    /// A head's prompt, a scripted `--prompt`, a subagent's task or a later prompt to it.
    Prompt,
    /// A peer's or a parent's message, heard between turns.
    Message,
    /// Something the session owns settled, or a monitor fired, or a row's condition came due.
    Wake,
    /// **The idle plan-check, carrying the notice it was built from** — so the owner can record
    /// it as said before the clock is re-armed (the metronome rule: the same notice is not sent
    /// again every window).
    Nag(Option<String>),
    /// The turn after the operator's own `!` line.
    OperatorShell,
    /// The turn after the operator's `/reseat`: the model is told its prompt was rebuilt.
    Reseat,
}

impl Door {
    /// The door as a metrics row names it (`turn.start`'s `name`, TODO R81).
    pub fn word(&self) -> &'static str {
        match self {
            Door::Prompt => "prompt",
            Door::Message => "message",
            Door::Wake => "wake",
            Door::Nag(_) => "nag",
            Door::OperatorShell => "bang",
            Door::Reseat => "reseat",
        }
    }

    /// Somebody spoke to the session: a new idle period, and the plan-check may be made again
    /// (*"but certainly not after my message"* — the check is earned back by being spoken to).
    fn is_spoken_to(&self) -> bool {
        matches!(self, Door::Prompt | Door::Message)
    }
}

/// **Where the effects of the shared sequence land** — the owner's half of [`run_turn`].
///
/// Two implementations: `Sessions` for a session the daemon's worker holds, and the serving
/// state of a subagent's own thread (`harness::ChildTurns`). Each method is one step of the
/// sequence; the ORDER is [`run_turn`]'s and no owner can reorder it.
pub trait TurnOwner {
    /// Before the turn: ask for an abandoned-call sweep a moment after it.
    fn arm_sweep(&mut self);
    /// Somebody spoke to the session ([`Door::Prompt`], [`Door::Message`]).
    fn spoken_to(&mut self, _h: &mut Harness) {}
    /// A compaction ran and forked the transcript: re-seat what this owner seats (a root's
    /// fabric block). Nothing for an owner that seats nothing.
    fn compacted(&mut self, _h: &mut Harness) {}
    /// The harness derived a title for the session.
    fn title(&mut self, h: &mut Harness, title: String);
    /// The turn may have declared a monitor: give it a waiter if it has none.
    fn arm_wake(&mut self, h: &mut Harness);
    /// The turn (and its continuations) is over: the owner's idle clocks.
    fn turn_ended(&mut self, h: &mut Harness, door: &Door);
}

/// **The owner that does nothing** — for a harness driven directly with no daemon around it
/// (a test, or `Harness::submit_as_a_normal_session` called bare). The compaction, the failure
/// and the continuation are still the shared sequence's; only the owner's clocks are absent,
/// because there are none.
pub struct Bare;

impl TurnOwner for Bare {
    fn arm_sweep(&mut self) {}
    fn title(&mut self, _h: &mut Harness, _title: String) {}
    fn arm_wake(&mut self, _h: &mut Harness) {}
    fn turn_ended(&mut self, _h: &mut Harness, _door: &Door) {}
}

/// **Run one turn through DOOR, and everything around it.** See the module docs for the
/// sequence. `turn` is the door's own act; `Ok(None)` is a door that found nothing to say (a wake
/// whose settlement was already delivered, a nag whose plan moved) — no turn ran, nothing is
/// tidied after it, and the answer is `None`.
pub fn run_turn(
    h: &mut Harness,
    owner: &mut dyn TurnOwner,
    door: Door,
    turn: impl FnOnce(&mut Harness) -> Result<Option<Reply>, HarnessError>,
) -> Option<Result<Reply, HarnessError>> {
    owner.arm_sweep();
    if door.is_spoken_to() {
        owner.spoken_to(h);
    }
    // **The wall is checked BEFORE the send** — measured 2026-10-02: a resumed session sent
    // 1,463,497 tokens against a 1,048,576 window, because every check ran after a turn and a
    // reopened session had taken none. Before the clock: the compaction is tidying, not a round.
    if h.compact_if_at_the_wall() {
        owner.compacted(h);
    }
    // **A turn is one prompt however many rounds it takes, so the clock starts here** and is
    // carried through every round and continuation of it.
    h.begin_turn_clock(letibot_sessionlog::event::now_ms());
    crate::instruments::turn_begins(h, door.word());
    let out = match turn(h) {
        Ok(Some(reply)) => Ok(reply),
        Err(e) => Err(e),
        Ok(None) => {
            crate::instruments::turn_ends(h, None, 0);
            h.end_turn_clock();
            return None;
        }
    };
    let before = h.transcript_id().to_string();
    let mut out = settle(h, owner, out);
    // R93.E1: a continuation says "continue from the summary above", so it follows only a turn
    // whose wall was actually compacted — a fork, a new transcript.
    let mut compacted = h.transcript_id() != before;
    // **A wall that compacted is not the end of the prompt.** The continuation is
    // `Harness::continue_after_wall`'s user item, and each one is a whole turn with this same
    // settle. Room is MEASURED, not read off a flag (the no-progress guard turns `auto_compact`
    // off without freeing anything), and `WALL_CONTINUES` caps the cycles the room check
    // cannot see. When either gate closes, the last wall error is the answer.
    let mut walls = 0;
    while is_wall(&out) && compacted && walls < crate::sessions::WALL_CONTINUES {
        h.measure_resident();
        // **Not closed on an estimate.** A metered base the provider has not counted is the
        // fork's estimate, and the continuation's first round is what measures it — the
        // estimate said ≈ 1,039,055 for a base deepseek counted at 507,355 (pg-noop,
        // 2026-10-11). Over the window, the provider refuses and the refused door folds,
        // inside `WALL_CONTINUES`.
        if !h.base_is_unmeasured() && !h.config().room_for_next_turn(h.ledger_len() as u64) {
            break;
        }
        walls += 1;
        let next = h.continue_after_wall();
        let before = h.transcript_id().to_string();
        out = settle(h, owner, next);
        compacted = h.transcript_id() != before;
    }
    crate::instruments::turn_ends(h, Some(&out), walls);
    // A start that outlived its turn would be inherited by the next prompt's first round.
    h.end_turn_clock();
    owner.turn_ended(h, &door);
    Some(out)
}

fn is_wall(out: &Result<Reply, HarnessError>) -> bool {
    matches!(out, Err(HarnessError::ContextWall { .. }))
}

/// **What follows every turn, the first and each continuation.**
fn settle(
    h: &mut Harness,
    owner: &mut dyn TurnOwner,
    out: Result<Reply, HarnessError>,
) -> Result<Reply, HarnessError> {
    if let Some(title) = h.take_new_title() {
        owner.title(h, title);
    }
    owner.arm_wake(h);
    // **The failure reaches the head FIRST**: it is the answer to what was asked, and the
    // tidying after it is not. With its reason — a child's failure used to be `.is_ok()`'d away.
    if let Err(e) = &out {
        publish_failure(h.hub(), h.last_turn_id(), e);
    }
    // **The wall is the one failure that must still compact**, and the two outcomes take two
    // DIFFERENT doors: a success takes the pre-emptive door (would the next turn fit?), which the
    // no-progress guard stands down; a turn stopped AT the wall takes the refused door, which
    // does not read that flag — MEASURED 2026-10-09, `s-1789919514688401228`: one compaction,
    // then thirty provider refusals in a row, because every recovery was gated on the flag.
    let wall = is_wall(&out);
    let attempted = if out.is_ok() {
        h.compact_if_at_the_wall()
    } else if wall {
        h.compact_because_refused()
    } else {
        false
    };
    if attempted {
        owner.compacted(h);
    }
    if wall && !attempted {
        // No arm for a refused door that stood down: it never does (`crate::wall`). The
        // latch that did (`forced_gave_up`) was set on an estimate of a folded base.
        let why = if h.config().context_window.is_none() {
            "not configured — no --context-window is set, so there is no wall to measure against"
        } else {
            "automatic compaction is off for this session"
        };
        h.hub().publish(SessionEvent::Warning {
            code: "auto_compact_skipped".into(),
            detail: format!(
                "the turn stopped at the context wall and nothing was compacted: {why}. \
                 `/compact` does it by hand."
            ),
            compaction: None,
        });
    }
    out
}

/// Announce a failed turn on the session's own log, as a **terminal** event.
///
/// `crates/ui/DESIGN.md` §4.5, paid. This used to publish a `Warning` and nothing
/// else, so a head's `TurnState` stayed `Running` for the rest of the session and
/// its spinner span at a dead turn — the head covered for it with *"nothing
/// received for 17.0s"*, which cannot tell a failed turn from a slow one. Only the
/// party that saw the error can, and that party is this function.
///
/// Both frames go out: `TurnFailed` is what stops the spinner and puts the reason
/// under the turn, and the `Warning` is what puts it in the scrollback with a code
/// somebody can grep the log for. They are not the same disclosure — one is state,
/// the other is history — and neither is a substitute for the other. Since R68.4 a
/// subagent's failures come through here too: its own thread used to publish a
/// `turn_failed` warning with no reason in it and no `TurnFailed` at all.
pub(crate) fn publish_failure(hub: &Arc<Hub>, turn_id: &str, e: &HarnessError) {
    hub.publish(SessionEvent::TurnFailed {
        turn_id: turn_id.to_string(),
        error: e.to_string(),
        // §5.7 commits nothing on a failed turn, so there is no partial to keep.
        // The field is here so a future failure that *does* keep one cannot arrive
        // looking like this.
        partial_kept: false,
    });
    hub.publish(SessionEvent::Warning {
        code: "turn_failed".into(),
        detail: e.to_string(),
        compaction: None,
    });
}

/// **Start this session's monitor waiter** — the thread that rings the bell when a monitor the
/// session declared fires between turns. `Ok(false)` when nothing is watching yet (no thread is
/// started for a session with no monitors: T24's rule that watchers must not accumulate);
/// `Err` when the thread could not be started.
///
/// One function for both owners, so a subagent's monitors wake it the same way a root's do: the
/// ring names the session, the worker routes it (`Sessions::wake` — a child's goes to its own
/// reader), and the session's own `Harness::wake` delivers the firing.
///
/// **The waiter ends with its session** (R68.3), not only with the daemon: it holds the hub
/// weakly, and a hub that was closed (the session was closed or parked) ends it at the next
/// recheck.
pub(crate) fn spawn_monitor_wake(
    registry: &Arc<Registry>,
    h: &mut Harness,
    session_id: &str,
    recheck: std::time::Duration,
) -> Result<bool, String> {
    let Some(monitors) = h.monitors().cloned() else {
        return Ok(false);
    };
    // Read, not assumed: arm only once something is actually watching.
    if monitors.live_names().is_empty() {
        return Ok(false);
    }
    let bell = registry.bell().clone();
    let hub = registry
        .get(session_id)
        .map(|h| Arc::downgrade(&h))
        .unwrap_or_default();
    let id = session_id.to_string();
    std::thread::Builder::new()
        .name(format!("monitor-wake-{}", &id[..id.len().min(24)]))
        .spawn(move || {
            // The waiter's **own** cursor, advanced by what it has already rung for. The harness
            // has a second one, shared with that session's steering source, which decides what
            // is actually delivered — a firing picked up mid-turn is not delivered again by the
            // wake. Two cursors because they answer two questions: "have I rung for this?" and
            // "has the model seen this?"
            let mut notified = monitors.settled_count();
            let alive = || !bell.is_closed() && hub.upgrade().is_some_and(|h| !h.is_closed());
            while alive() {
                let fired = monitors.wait_for_any(notified, recheck);
                if !alive() {
                    break;
                }
                if fired.is_empty() {
                    continue;
                }
                notified += fired.len();
                bell.ring_wake(&id);
            }
        })
        .map_err(|e| e.to_string())?;
    h.declare_monitor_wake();
    Ok(true)
}
