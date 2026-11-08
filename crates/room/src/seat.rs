//! A seat's presence, derived from harness facts and never from the agent's word. `connected` only
//! proves a process holds the socket; the state says whether the agent behind it can act. The daemon
//! sends [`SeatFacts`] every tick, and the coordinator turns them into the roster's [`SeatView`].

use serde::{Deserialize, Serialize};

use crate::item::duration;

/// Facts older than this, on a seat that is still connected, are unknown rather than idle: a
/// daemon that stopped reporting is a sensor that went dark, not a seat with nothing to do.
pub const SEAT_STALE_MS: u64 = 15_000;

/// A declared status lives this long unless the seat names another TTL.
pub const STATUS_TTL_DEFAULT_MS: u64 = 30 * 60 * 1000;

/// The longest TTL a declared status may ask for.
pub const STATUS_TTL_MAX_MS: u64 = 4 * 60 * 60 * 1000;

/// Files kept in "working on", newest first.
pub const RECENT_FILES: usize = 3;

/// The outcome of the seat's last model call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub ok: bool,
    pub at_ms: u64,
    /// The provider's words, one line, when it failed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// A one-line status the agent set. It expires, and it clears when the row it was set on closes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declared {
    pub text: String,
    pub until_ms: u64,
    /// The in-progress row when it was said, empty when there was none.
    #[serde(default)]
    pub row: String,
}

/// What a seat is working on, read from its board, its workspace and its edits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingOn {
    /// The in-progress todo row's title, empty when none is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub row: String,
    /// The repository's folder name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
    /// The worktree's folder name when it is not the repository's own checkout.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub worktree: String,
    /// The last files edited, newest first, at most [`RECENT_FILES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Declared>,
}

impl WorkingOn {
    /// The declared status disagrees with the derived row: it was said on another row, or on
    /// none while a row is now in progress.
    pub fn disagrees(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.row != self.row)
    }

    /// One line for a roster row: the status (and the row when they disagree), the place, the files.
    pub fn line(&self) -> String {
        let mut parts = Vec::new();
        match &self.status {
            Some(s) if self.disagrees() && !self.row.is_empty() => {
                parts.push(format!("says \"{}\", on \"{}\"", s.text, self.row));
            }
            Some(s) if self.disagrees() => parts.push(format!("says \"{}\", no row", s.text)),
            Some(s) => parts.push(format!("\"{}\"", s.text)),
            None if !self.row.is_empty() => parts.push(format!("on \"{}\"", self.row)),
            None => {}
        }
        let mut place = self.repo.clone();
        if !self.branch.is_empty() {
            place = if place.is_empty() {
                self.branch.clone()
            } else {
                format!("{place}@{}", self.branch)
            };
        }
        if !self.worktree.is_empty() {
            place = format!("{place} in {}", self.worktree);
        }
        if !place.is_empty() {
            parts.push(place);
        }
        if !self.files.is_empty() {
            parts.push(self.files.join(", "));
        }
        parts.join(" · ")
    }
}

/// What a seat's daemon reports about it, from the harness alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatFacts {
    /// A prompt is in flight, since this instant.
    #[serde(default)]
    pub turn_since_ms: Option<u64>,
    /// The last turn ended at this instant.
    #[serde(default)]
    pub idle_since_ms: Option<u64>,
    #[serde(default)]
    pub last_call: Option<Call>,
    /// Parked on a provider limit until this instant.
    #[serde(default)]
    pub limited_until_ms: Option<u64>,
    /// The daemon's words for that limit, `paused until 21:02:27 UTC`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub limit: String,
    /// Cards open on the session that only the operator can answer.
    #[serde(default)]
    pub cards: u32,
    #[serde(default)]
    pub working: WorkingOn,
}

/// The derived state, in the order it is judged: a seat that cannot act says why first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SeatState {
    /// No connection: the daemon or the session is down.
    Stopped {
        since_ms: Option<u64>,
    },
    /// Connected, but its facts are missing or old. Never read as idle.
    Unknown {
        why: String,
    },
    RateLimited {
        until_ms: u64,
        words: String,
    },
    /// The last model call failed and no turn is retrying it.
    ProviderError {
        at_ms: u64,
        error: String,
    },
    /// Blocked on somebody else: an operator card, an ask, a queue wait.
    Waiting {
        on: Vec<String>,
        turn: bool,
    },
    InTurn {
        since_ms: u64,
    },
    Idle {
        since_ms: Option<u64>,
    },
}

impl SeatState {
    /// Whether the agent behind this seat can act on a message now.
    pub fn can_act(&self) -> bool {
        !matches!(
            self,
            SeatState::Stopped { .. }
                | SeatState::RateLimited { .. }
                | SeatState::ProviderError { .. }
        )
    }

    /// The state in a person's words, ages measured at `now`.
    pub fn line(&self, now: u64, last_call: Option<&Call>) -> String {
        let ago = |at: u64| duration(now.saturating_sub(at));
        match self {
            SeatState::Stopped { since_ms: Some(at) } => format!("stopped {}", ago(*at)),
            SeatState::Stopped { since_ms: None } => "stopped".into(),
            SeatState::Unknown { why } => format!("unknown: {why}"),
            SeatState::RateLimited { until_ms, words } => {
                let left = duration(until_ms.saturating_sub(now));
                if words.is_empty() {
                    format!("rate-limited, {left} left")
                } else {
                    format!("rate-limited, {words} ({left} left)")
                }
            }
            SeatState::ProviderError { at_ms, error } => {
                format!("provider error {} ago: {error}", ago(*at_ms))
            }
            SeatState::Waiting { on, turn } => {
                let head = if *turn {
                    "in turn, waiting on"
                } else {
                    "waiting on"
                };
                format!("{head} {}", on.join(", "))
            }
            SeatState::InTurn { since_ms } => format!("in turn {}", ago(*since_ms)),
            SeatState::Idle { since_ms } => {
                let mut s = match since_ms {
                    Some(at) => format!("idle {}", ago(*at)),
                    None => "idle".into(),
                };
                match last_call {
                    Some(c) if c.ok => s.push_str(", last call ok"),
                    Some(_) => s.push_str(", last call failed"),
                    None => {}
                }
                s
            }
        }
    }
}

/// The roster's view of one seat: the derived state, the low-level `connected`, and what it works on,
/// each also worded so a head draws the coordinator's sentence instead of a copy of this logic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatView {
    #[serde(flatten)]
    pub state: SeatState,
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_call: Option<Call>,
    #[serde(default)]
    pub working: WorkingOn,
    /// `state` in words.
    #[serde(default)]
    pub says: String,
    /// `working` in words.
    #[serde(default)]
    pub doing: String,
}

/// What the coordinator knows beside the facts: the connection and what the seat waits on in the room.
#[derive(Debug, Clone, Default)]
pub struct RoomSide {
    pub connected: bool,
    pub gone_since_ms: Option<u64>,
    /// When the facts arrived, by the coordinator's clock.
    pub heard_ms: Option<u64>,
    /// Open asks it made and queue waits it declared, worded.
    pub waits: Vec<String>,
}

/// Derive a seat's view. `facts` is `None` for a seat whose daemon never reported.
pub fn derive(facts: Option<&SeatFacts>, room: &RoomSide, now: u64) -> SeatView {
    let state = state_of(facts, room, now);
    let working = facts.map(|f| f.working.clone()).unwrap_or_default();
    let last_call = facts.and_then(|f| f.last_call.clone());
    SeatView {
        says: state.line(now, last_call.as_ref()),
        doing: working.line(),
        state,
        connected: room.connected,
        last_call,
        working,
    }
}

fn state_of(facts: Option<&SeatFacts>, room: &RoomSide, now: u64) -> SeatState {
    if !room.connected {
        return SeatState::Stopped {
            since_ms: room.gone_since_ms,
        };
    }
    let Some(f) = facts else {
        return SeatState::Unknown {
            why: "its daemon reports no facts".into(),
        };
    };
    let age = room.heard_ms.map(|h| now.saturating_sub(h));
    if let Some(age) = age.filter(|a| *a > SEAT_STALE_MS) {
        return SeatState::Unknown {
            why: format!("facts {} old", duration(age)),
        };
    }
    if let Some(until) = f.limited_until_ms.filter(|u| *u > now) {
        return SeatState::RateLimited {
            until_ms: until,
            words: f.limit.clone(),
        };
    }
    let mut on = Vec::new();
    if f.cards > 0 {
        on.push(if f.cards == 1 {
            "the operator's card".to_string()
        } else {
            format!("{} operator cards", f.cards)
        });
    }
    on.extend(room.waits.iter().cloned());
    if !on.is_empty() {
        return SeatState::Waiting {
            on,
            turn: f.turn_since_ms.is_some() && f.cards == 0,
        };
    }
    if let Some(since) = f.turn_since_ms {
        return SeatState::InTurn { since_ms: since };
    }
    if let Some(c) = f.last_call.as_ref().filter(|c| !c.ok) {
        return SeatState::ProviderError {
            at_ms: c.at_ms,
            error: c.error.clone(),
        };
    }
    SeatState::Idle {
        since_ms: f.idle_since_ms,
    }
}

/// Clamp an asked TTL to the declared-status rule: default when absent or zero, capped at the max.
pub fn status_ttl(asked_ms: Option<u64>) -> u64 {
    match asked_ms {
        None | Some(0) => STATUS_TTL_DEFAULT_MS,
        Some(t) => t.min(STATUS_TTL_MAX_MS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here(heard: u64) -> RoomSide {
        RoomSide {
            connected: true,
            heard_ms: Some(heard),
            ..RoomSide::default()
        }
    }

    #[test]
    fn a_connected_seat_with_no_or_old_facts_is_unknown_never_idle() {
        let v = derive(None, &here(0), 1_000);
        assert!(matches!(v.state, SeatState::Unknown { .. }), "{v:?}");
        let f = SeatFacts::default();
        let v = derive(Some(&f), &here(0), SEAT_STALE_MS + 1_001);
        assert!(v.says.starts_with("unknown: facts"), "{}", v.says);
        assert!(v.connected);
    }

    #[test]
    fn a_seat_with_no_connection_is_stopped_whatever_its_last_facts_said() {
        let f = SeatFacts {
            turn_since_ms: Some(10),
            ..SeatFacts::default()
        };
        let room = RoomSide {
            connected: false,
            gone_since_ms: Some(1_000),
            ..RoomSide::default()
        };
        let v = derive(Some(&f), &room, 61_000);
        assert_eq!(v.says, "stopped 1m");
        assert!(!v.state.can_act());
        assert!(!v.connected);
    }

    #[test]
    fn the_states_are_judged_limit_then_waits_then_turn_then_error_then_idle() {
        let now = 100_000;
        let mut f = SeatFacts {
            turn_since_ms: Some(40_000),
            limited_until_ms: Some(now + 60_000),
            limit: "paused until 21:02:27 UTC".into(),
            last_call: Some(Call {
                ok: false,
                at_ms: 90_000,
                error: "503".into(),
            }),
            cards: 1,
            ..SeatFacts::default()
        };
        let room = RoomSide {
            waits: vec!["ask #4 of serenedb".into()],
            ..here(now)
        };
        let v = derive(Some(&f), &room, now);
        assert_eq!(v.says, "rate-limited, paused until 21:02:27 UTC (1m left)");
        assert!(!v.state.can_act());
        f.limited_until_ms = Some(now - 1);
        let v = derive(Some(&f), &room, now);
        assert_eq!(
            v.says, "waiting on the operator's card, ask #4 of serenedb",
            "a card stops the turn, so it is not said as in turn"
        );
        f.cards = 0;
        let v = derive(Some(&f), &here(now), now);
        assert_eq!(v.says, "in turn 1m", "a turn retrying a failure is in turn");
        f.turn_since_ms = None;
        let v = derive(Some(&f), &here(now), now);
        assert_eq!(v.says, "provider error 10s ago: 503");
        assert!(!v.state.can_act());
        f.last_call = Some(Call {
            ok: true,
            at_ms: 95_000,
            error: String::new(),
        });
        f.idle_since_ms = Some(70_000);
        let v = derive(Some(&f), &here(now), now);
        assert_eq!(v.says, "idle 30s, last call ok");
        assert!(v.state.can_act());
    }

    #[test]
    fn working_on_shows_both_only_when_the_declared_status_disagrees() {
        let mut w = WorkingOn {
            row: "wire the port".into(),
            repo: "letibot".into(),
            branch: "z-r163".into(),
            worktree: "wt-z-r163".into(),
            files: vec!["a.rs".into(), "b.rs".into()],
            status: None,
        };
        assert_eq!(
            w.line(),
            "on \"wire the port\" · letibot@z-r163 in wt-z-r163 · a.rs, b.rs"
        );
        w.status = Some(Declared {
            text: "bisecting the flake".into(),
            until_ms: 1,
            row: "wire the port".into(),
        });
        assert!(!w.disagrees());
        assert!(w.line().starts_with("\"bisecting the flake\" · letibot"));
        w.status.as_mut().unwrap().row = "another row".into();
        assert!(w.disagrees());
        assert!(
            w.line()
                .starts_with("says \"bisecting the flake\", on \"wire the port\" · "),
            "{}",
            w.line()
        );
    }

    #[test]
    fn a_status_ttl_defaults_to_thirty_minutes_and_caps_at_four_hours() {
        assert_eq!(status_ttl(None), 30 * 60_000);
        assert_eq!(status_ttl(Some(0)), 30 * 60_000);
        assert_eq!(status_ttl(Some(60_000)), 60_000);
        assert_eq!(status_ttl(Some(24 * 3_600_000)), 4 * 3_600_000);
    }

    #[test]
    fn the_view_is_additive_on_the_wire() {
        let v = derive(None, &RoomSide::default(), 0);
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["state"], "stopped");
        assert_eq!(j["connected"], false);
        assert_eq!(j["says"], "stopped");
        let back: SeatView = serde_json::from_value(j).unwrap();
        assert_eq!(back, v);
    }
}
