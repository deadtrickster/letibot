//! One turn tail for every door - `TODO.md`. A turn reaches a session through eight doors
//! a head's prompt, a peer's or parent's message, a wake, the idle plan-check, the
//! operator's `!` line, a subagent's task, the continuation after a wall, and the
//! continuation after a FALLBACK. Each used to carry its own copy of what happens around a
//! turn, and the copies had drifted: no waiter armed by a wake, a child's `turn_failed`
//! with no reason in it, a refused-compaction latch shared with nobody. So the sequence
//! lives here, once, and both owners call it

use std::sync::Arc;

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;

use crate::harness::{Harness, HarnessError, Reply};

/// Which door a turn came through - what the shared tail needs to know about it, and nothing
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
    /// The idle plan-check, carrying the notice it was built from - so the owner can record
    /// it as said before the clock is re-armed (the metronome rule: the same notice is not sent
    /// again every window).
    Nag(Option<String>),
    /// The queue's stuck-row census, as a turn - the clock's second subject, and it carries
    /// nothing: its identity is the DELIVERY (`TodoNagClock::census_said`), not the words
    /// because the sentence names the AGE of the oldest row and so changes at every delivery.
    Census,
    /// The turn after the operator's own `!` line.
    OperatorShell,
    /// The turn after the operator's `/reseat`: the model is told its prompt was rebuilt.
    Reseat,
    /// The turn the daemon owes after it moved the session off a model that was out - the
    /// operator's ruling, 2026-10-12: *"when it finally applied the turn didnt continue"*. The
    /// model that refused has already been replaced
    Fallback,
}

impl Door {
    /// The door as a metrics row names it.
    pub fn word(&self) -> &'static str {
        match self {
            Door::Prompt => "prompt",
            Door::Message => "message",
            Door::Wake => "wake",
            Door::Nag(_) => "nag",
            Door::Census => "census",
            Door::OperatorShell => "bang",
            Door::Reseat => "reseat",
            Door::Fallback => "fallback",
        }
    }

    /// Somebody spoke to the session: a new idle period, and the plan-check may be made again
    /// (*"but certainly not after my message"* - the check is earned back by being spoken to).
    fn is_spoken_to(&self) -> bool {
        matches!(self, Door::Prompt | Door::Message)
    }
}

/// Where the effects of the shared sequence land - the owner's half of [`run_turn`]. Two
/// implementations: `Sessions` for a session the daemon's worker holds, and a subagent's
/// own thread (`harness::ChildTurns`).
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

/// The owner that does nothing - for a harness driven directly with no daemon around it (a
/// test, or `Harness::submit_as_a_normal_session` called bare). The compaction, the failure and
/// the continuation are still the shared sequence's; only the owner's clocks are absent.
pub struct Bare;

impl TurnOwner for Bare {
    fn arm_sweep(&mut self) {}
    fn title(&mut self, _h: &mut Harness, _title: String) {}
    fn arm_wake(&mut self, _h: &mut Harness) {}
    fn turn_ended(&mut self, _h: &mut Harness, _door: &Door) {}
}

/// Run one turn through DOOR, and everything around it. `turn` is the door's own act
/// `Ok(None)` is a door that found nothing to say, and then no turn ran and nothing is
/// tidied. And the turn a fallback owes
pub fn run_turn(
    h: &mut Harness,
    owner: &mut dyn TurnOwner,
    door: Door,
    turn: impl FnOnce(&mut Harness) -> Result<Option<Reply>, HarnessError>,
) -> Option<Result<Reply, HarnessError>> {
    // Read before the door is moved: a continuation never earns a second one.
    let is_continuation = door == Door::Fallback;
    let out = one_turn(h, owner, door, turn);
    // The notice is taken whatever happens (so a fallback can never arm a turn nobody asked
    // for later), and only a turn that FAILED can have armed it.
    let continuation = h.take_fallback_continuation();
    if !is_continuation
        && matches!(&out, Some(Err(_)))
        && let Some(notice) = continuation
    {
        h.set_fallback_continuation(true);
        let ran = one_turn(h, owner, Door::Fallback, |h| {
            h.continue_after_fallback(notice).map(Some)
        });
        h.set_fallback_continuation(false);
        return ran;
    }
    out
}

/// One turn's whole sequence - the body of [`run_turn`], and never called from inside itself.
fn one_turn(
    h: &mut Harness,
    owner: &mut dyn TurnOwner,
    door: Door,
    turn: impl FnOnce(&mut Harness) -> Result<Option<Reply>, HarnessError>,
) -> Option<Result<Reply, HarnessError>> {
    owner.arm_sweep();
    // A turn is taken before the door's own act and the clock starts only once the door has
    // something to say. What this buys is the door that answers `Ok(None)`: no clock was
    // started, so no reader is told a turn was in flight.
    h.turn_taken();
    if door.is_spoken_to() {
        owner.spoken_to(h);
    }
    // The wall is checked BEFORE the send - measured 2026-10-02: a resumed session sent
    // 1,463,497 tokens against a 1,048,576 window, because every check ran after a turn and a
    // reopened session had taken none. Before the clock: the compaction is tidying, not a round.
    if h.compact_if_at_the_wall() {
        owner.compacted(h);
    }
    // A turn is one prompt however many rounds it takes, so the clock starts once - inside the
    // door's own act, where a turn that never speaks never starts it (see this function's doc)
    // and where every round and continuation of one prompt leaves it where it was.
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
    // .E1: a continuation says "continue from the summary above", so it follows only a turn
    // whose wall was actually compacted - a fork, a new transcript.
    let mut compacted = h.transcript_id() != before;
    // A wall that compacted is not the end of the prompt. Each continuation is a whole turn
    // with this same settle. Room is MEASURED, not read off a flag (the no-progress guard
    // turns `auto_compact` off without freeing anything)
    let mut walls = 0;
    while is_wall(&out) {
        h.measure_resident();
        // Not closed on an estimate. A metered base the provider has not counted is the fork's
        // estimate, and the continuation's first round is what measures it: the estimate said
        // ≈ 1,039,055 for a base deepseek counted at 507,355.
        let room = h.base_is_unmeasured() || h.config().room_for_next_turn(h.ledger_len() as u64);
        // Only the wall that ends the prompt is its failure - *"there was a wall of red
        // 'compaction was attempted blabla'"*: each wall the daemon carried on from was published
        // as the turn's failure, a red footer on a turn that had not ended.
        match wall_goes_on(compacted, walls, room) {
            WallNext::Fails => {
                if let Err(e) = &out {
                    publish_failure(h.hub(), h.last_turn_id(), e);
                }
                break;
            }
            WallNext::GoesOn => {
                h.hub().publish(SessionEvent::Warning {
                    code: "context_wall_continued".into(),
                    detail: format!(
                        "the turn reached the context wall, was compacted, and goes on from \
                         the summary ({} of at most {})",
                        walls + 1,
                        crate::sessions::WALL_CONTINUES
                    ),
                    compaction: None,
                });
            }
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

/// What follows a turn stopped at the context wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WallNext {
    /// It compacted, there is room, and continuations are left: the prompt goes on.
    GoesOn,
    /// The prompt ends here, and the wall is its failure.
    Fails,
}

/// Whether a wall is the prompt's end - compacted (a fork happened), under
/// [`crate::sessions::WALL_CONTINUES`] continuations, with room for the next turn.
pub(crate) fn wall_goes_on(compacted: bool, walls: usize, room: bool) -> WallNext {
    if compacted && walls < crate::sessions::WALL_CONTINUES && room {
        WallNext::GoesOn
    } else {
        WallNext::Fails
    }
}

fn is_wall(out: &Result<Reply, HarnessError>) -> bool {
    matches!(out, Err(HarnessError::ContextWall { .. }))
}

/// What follows every turn, the first and each continuation.
fn settle(
    h: &mut Harness,
    owner: &mut dyn TurnOwner,
    out: Result<Reply, HarnessError>,
) -> Result<Reply, HarnessError> {
    if let Some(title) = h.take_new_title() {
        owner.title(h, title);
    }
    owner.arm_wake(h);
    // The failure reaches the head FIRST: it is the answer to what was asked, and the tidying
    // after it is not. With its reason - a child's failure used to be `.is_ok`'d away. Except
    // the wall, whose failure is decided after its compaction.
    if let Err(e) = &out
        && !is_wall(&out)
    {
        publish_failure(h.hub(), h.last_turn_id(), e);
        // A switch that fails says how to go back - the operator's session was switched to a model
        // whose every request was refused, and the screen named the refusal and nothing else.
        if let Some(said) = h.switch_back_notice(e) {
            h.hub().publish(SessionEvent::Warning {
                code: "model_switch_refused".into(),
                detail: said,
                compaction: None,
            });
        }
    }
    // The wall is the one failure that must still compact, and the two outcomes take two
    // DIFFERENT doors: a success takes the pre-emptive door (would the next turn fit?), which
    // the no-progress guard stands down
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

/// Announce a failed turn on the session's own log, as a terminal event. It used to publish
/// a `Warning` and nothing else, so a head's `TurnState` stayed `Running` for the rest of
/// the session and its spinner span at a dead turn. Both frames go out
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

/// Start this session's monitor waiter - the thread that rings the bell when a monitor the
/// session declared fires between turns. `Ok(false)` when nothing is watching yet (T24's
/// rule that watchers must not accumulate); `Err` when the thread could not be started.
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
            // The waiter's own cursor, advanced by what it has already rung for. The harness has a
            // second one, which decides what is actually delivered - a firing picked up mid-turn is
            // not delivered again by the wake.
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

#[cfg(test)]
mod wall_tests {
    use super::*;

    /// Only the last wall fails the prompt - a compacted wall with room and continuations
    /// left goes on, and is not published as the turn's failure.
    #[test]
    fn a_wall_goes_on_only_while_it_compacted_has_room_and_continuations_left() {
        let cap = crate::sessions::WALL_CONTINUES;
        assert_eq!(wall_goes_on(true, 0, true), WallNext::GoesOn);
        assert_eq!(wall_goes_on(true, cap - 1, true), WallNext::GoesOn);
        assert_eq!(
            wall_goes_on(true, cap, true),
            WallNext::Fails,
            "out of continuations"
        );
        assert_eq!(
            wall_goes_on(false, 0, true),
            WallNext::Fails,
            "nothing was compacted"
        );
        assert_eq!(
            wall_goes_on(true, 0, false),
            WallNext::Fails,
            "no room after it"
        );
    }
}
