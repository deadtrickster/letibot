//! One turn tail for every door - `TODO.md`.

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
    /// operator's ruling: *"when it finally applied the turn didnt continue"*. The
    /// model that refused has already been replaced.
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
                h.hub().warn(
                    "context_wall_continued",
                    format!(
                        "the turn reached the context wall, was compacted, and goes on from \
                         the summary ({} of at most {})",
                        walls + 1,
                        crate::sessions::WALL_CONTINUES
                    ),
                );
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
    // after it is not. With its reason - a child's failure is not `.is_ok`'d away. Except
    // the wall, whose failure is decided after its compaction.
    if let Err(e) = &out
        && !is_wall(&out)
    {
        publish_failure(h.hub(), h.last_turn_id(), e);
        // A switch that fails says how to go back - the operator's session was switched to a model
        // whose every request was refused, and the screen named the refusal and nothing else.
        if let Some(said) = h.switch_back_notice(e) {
            h.hub().warn("model_switch_refused", said);
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
        h.hub().warn(
            "auto_compact_skipped",
            format!(
                "the turn stopped at the context wall and nothing was compacted: {why}. \
                 `/compact` does it by hand."
            ),
        );
    }
    out
}

/// Announce a failed turn on the session's own log, as a terminal event. A `Warning` alone
/// would leave a head's `TurnState` `Running` for the rest of
/// the session with its spinner on a dead turn. Both frames go out, the terminal event and then the warning.
pub(crate) fn publish_failure(hub: &Arc<Hub>, turn_id: &str, e: &HarnessError) {
    hub.publish(SessionEvent::TurnFailed {
        turn_id: turn_id.to_string(),
        error: e.to_string(),
        // A failed turn commits nothing, so there is no partial to keep.
        // The field is here so a future failure that *does* keep one cannot arrive
        // looking like this.
        partial_kept: false,
    });
    hub.warn("turn_failed", e.to_string());
}

/// What a batch of firings earns: whether the bell rings, and the monitors that rang (the ones the
/// next window counts from) - an ending is in neither, since it leaves nothing to hold back.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Ring {
    pub rings: bool,
    pub spoke: Vec<String>,
}

/// Which monitors a batch of firings may ring the bell for, given when each last did. One ring per monitor per window bounds what the TTL does not: a condition can hold every tick and a ring starts a turn.
/// An ending is never held back and clears the name; one monitor's window holds nobody else's.
pub(crate) fn may_ring(
    fired: &[letibot_tools::exec::monitor::Firing],
    spoke: &std::collections::HashMap<String, std::time::Instant>,
    now: std::time::Instant,
) -> Ring {
    let mut out = Ring::default();
    for f in fired {
        if out.spoke.contains(&f.name) {
            continue;
        }
        // An ending is said at once and is not a pace to hold the next one to.
        if !f.fired.happened() {
            out.rings = true;
            continue;
        }
        let inside = spoke
            .get(&f.name)
            .and_then(|at| now.checked_duration_since(*at))
            .is_some_and(|since| since < letibot_tools::exec::monitor::FIRING_RATE);
        if inside {
            continue;
        }
        out.rings = true;
        out.spoke.push(f.name.clone());
    }
    out
}

/// Start this session's monitor waiter - the thread that rings the bell when a monitor the
/// session declared fires between turns. `Ok(false)` when nothing is watching yet (the
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
    let started = std::thread::Builder::new()
        .name(format!("monitor-wake-{}", &id[..id.len().min(24)]))
        .spawn(move || {
            // The waiter's own cursor, advanced by what it has already rung for. The harness has a
            // second one, which decides what is actually delivered - a firing picked up mid-turn is
            // not delivered again by the wake.
            let mut notified = monitors.settled_count();
            // When each monitor last rang: the rate bound's state, and it lives here because a
            // ring is what this thread does.
            let mut spoke: std::collections::HashMap<String, std::time::Instant> =
                std::collections::HashMap::new();
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
                // A firing inside its window is not DROPPED: the cursor above moved past it, and
                // the harness has another one, so the next wake says it - counted, in one line.
                let ring = may_ring(&fired, &spoke, std::time::Instant::now());
                if !ring.rings {
                    continue;
                }
                let now = std::time::Instant::now();
                for name in ring.spoke {
                    spoke.insert(name, now);
                }
                // Every name in this batch has now been heard, so none of them holds a fresh window.
                for f in &fired {
                    if !f.fired.happened() {
                        spoke.remove(&f.name);
                    }
                }
                bell.ring_wake(&id);
            }
        });
    if let Err(e) = started {
        // Said here as well as in the caller's warning: the banner is what a reader trusts, and *POLL ONLY* for a thread that would not start reads like one nobody armed.
        // One is a movable knob, the other this box at this moment.
        h.declare_monitor_wake_failed();
        return Err(e.to_string());
    }
    h.declare_monitor_wake();
    Ok(true)
}

#[cfg(test)]
mod firing_tests {
    use super::*;
    use letibot_tools::exec::monitor::{FIRING_RATE, Fired, Firing};
    use std::time::{Duration, SystemTime};

    fn fired(name: &str, after: Duration) -> Firing {
        Firing {
            name: name.into(),
            watch: "a path".into(),
            declared_by: "t-1".into(),
            fired: Fired::Fired {
                why: "it changed".into(),
                at: SystemTime::UNIX_EPOCH,
                after,
            },
            at: SystemTime::UNIX_EPOCH,
        }
    }

    /// A monitor's voice is bounded besides its TTL: a condition holding every tick would ring a turn per tick.
    /// A burst rings once; what fires between waits for the next ring, which reads every firing since its cursor, so nothing is lost.
    #[test]
    fn a_burst_of_one_monitors_firings_rings_once_inside_the_window() {
        let t0 = std::time::Instant::now();
        let burst: Vec<Firing> = (0..20)
            .map(|n| fired("chatty", Duration::from_millis(n)))
            .collect();
        let spoke = std::collections::HashMap::new();
        assert_eq!(
            may_ring(&burst, &spoke, t0),
            Ring {
                rings: true,
                spoke: vec!["chatty".to_string()],
            },
            "a monitor that has never spoken rings at once"
        );
        let mut spoke = std::collections::HashMap::new();
        spoke.insert("chatty".to_string(), t0);
        assert_eq!(
            may_ring(&burst, &spoke, t0 + Duration::from_secs(1)),
            Ring::default(),
            "a second burst inside the window is one turn nobody asked for"
        );
        assert!(
            may_ring(&burst, &spoke, t0 + FIRING_RATE).rings,
            "the window passing is what earns the next ring"
        );
    }

    /// Three facts the bound must not eat: a second monitor is not held by the first one's window,
    /// an ENDING is said at once (it is the last thing that monitor will say), and an ending leaves
    /// no window behind - a monitor declared again under the name rings on its first firing.
    #[test]
    fn a_second_monitor_is_not_held_by_the_first_and_an_ending_is_said_at_once() {
        let t0 = std::time::Instant::now();
        let mut spoke = std::collections::HashMap::new();
        spoke.insert("chatty".to_string(), t0);
        let other = vec![fired("build", Duration::ZERO)];
        assert_eq!(
            may_ring(&other, &spoke, t0),
            Ring {
                rings: true,
                spoke: vec!["build".to_string()],
            }
        );

        let ending = vec![Firing {
            fired: Fired::Expired {
                after: Duration::from_secs(9),
                last_seen: "no match".into(),
            },
            ..fired("chatty", Duration::from_secs(9))
        }];
        assert_eq!(
            may_ring(&ending, &spoke, t0 + Duration::from_secs(1)),
            Ring {
                rings: true,
                spoke: Vec::new(),
            },
            "the monitor's last word is not rate-bounded, and holds nothing after it"
        );
    }
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
