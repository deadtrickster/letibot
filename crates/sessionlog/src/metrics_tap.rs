//! **The hub tap** — what a session's event stream says about itself, as metrics rows, and the
//! one place that knows which turn a mark lands on.
//!
//! `docs/metrics.md` §2.7 and §4: one line in the hub's fan-out (`Inner::append_and_fan`, under
//! the lock every publish already takes) hands each event to [`Tap::observe`]. Two jobs:
//!
//! 1. **Keep the session's "now"** — the turn running (or the last one), its round, what it is
//!    doing, and which turn each transcript row belongs to. The connection thread reads this to
//!    place an operator's mark ([`crate::protocol::ClientFrame::Feedback`]) without asking the
//!    worker, which may be pinned for tens of seconds (c12d044).
//! 2. **Emit the rows the hub already carries** — warnings by code, operator commands, subagent
//!    state changes, job settlements, decisions answered, queue moves, heads attaching. Nothing
//!    at those sites changes; the hub saw them all already. The high-rate events (`Delta`,
//!    `TokensGenerated`, `PromptProgress`, `JobOutput`) only move the "now" or are skipped.
//!
//! **A turn, for metrics, is a prompt** — every round the harness runs for one door — and its
//! id is the engine id of its FIRST round. The engine mints an id per round, and every round of
//! one prompt carries the same `began_ms` (`Harness::begin_turn_clock`), so a `TurnStarted`
//! with the `began_ms` of the turn in hand is its next round, and any other begins a new turn.
//! The harness keys its own rows (`turn.start`, `round`, `tool.call`, `turn.end`) by the same
//! rule, so a mark and the turn it marks join on `turn_id`.
//!
//! The mapping is a match and a small struct per event — it runs on the publishing thread, which
//! is cheaper than the clone the design first proposed shipping to the writer — and it costs
//! nothing at all when metrics are off: [`letibot_metrics::enabled`] is the first branch.

use std::collections::VecDeque;

use letibot_metrics::{Event, kind};

use crate::event::{DecisionOutcome, SessionEvent};

/// How many rows' turns are remembered for a mark on a past row. A head scrolled back further
/// than this marks a row with no turn — the row id is still kept on the mark.
const ITEMS: usize = 4096;

/// **The session's "now"**, as a mark sees it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Now {
    /// The turn running, or the last one; empty before the first.
    pub turn_id: String,
    pub round: u32,
    /// `streaming`, `tool`, `waiting-approval` or `idle`.
    pub state: &'static str,
    /// The tool call running, if one is.
    pub tool: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Tap {
    now: Now,
    began_ms: Option<u64>,
    /// `(item_id, turn_id, round)`, newest last.
    items: VecDeque<(String, String, u32)>,
}

impl Tap {
    pub(crate) fn now(&self) -> Now {
        let mut n = self.now.clone();
        if n.state.is_empty() {
            n.state = "idle";
        }
        n
    }

    /// The turn and round a transcript row was written in, if it is still remembered.
    pub(crate) fn item(&self, item_id: &str) -> Option<(String, u32)> {
        self.items
            .iter()
            .rev()
            .find(|(i, _, _)| i == item_id)
            .map(|(_, t, r)| (t.clone(), *r))
    }

    pub(crate) fn observe(&mut self, session: &str, e: &SessionEvent) {
        self.follow(e);
        if letibot_metrics::enabled() {
            self.emit(session, e);
        }
    }

    fn follow(&mut self, e: &SessionEvent) {
        match e {
            SessionEvent::TurnStarted {
                turn_id, began_ms, ..
            } => {
                if began_ms.is_some() && *began_ms == self.began_ms && !self.now.turn_id.is_empty()
                {
                    self.now.round += 1;
                } else {
                    self.now.turn_id = turn_id.clone();
                    self.now.round = 0;
                    self.began_ms = *began_ms;
                }
                self.now.state = "streaming";
                self.now.tool = None;
            }
            SessionEvent::ToolStarted { name, .. } => {
                self.now.state = "tool";
                self.now.tool = Some(name.clone());
            }
            SessionEvent::DecisionRequested { .. } => self.now.state = "waiting-approval",
            SessionEvent::DecisionAnswered { .. } if self.now.tool.is_some() => {
                self.now.state = "tool"
            }
            SessionEvent::ToolFinished { .. }
            | SessionEvent::TurnFinished { .. }
            | SessionEvent::TurnInterrupted { .. }
            | SessionEvent::TurnFailed { .. } => {
                self.now.state = "idle";
                self.now.tool = None;
            }
            SessionEvent::TranscriptAppended { item_id, .. } if !self.now.turn_id.is_empty() => {
                if self.items.len() >= ITEMS {
                    self.items.pop_front();
                }
                self.items.push_back((
                    item_id.clone(),
                    self.now.turn_id.clone(),
                    self.now.round,
                ));
            }
            _ => {}
        }
    }

    fn row(&self, session: &str, k: &'static str) -> Event {
        Event::new(k)
            .session(session)
            .turn(&self.now.turn_id, Some(self.now.round))
    }

    fn emit(&self, session: &str, e: &SessionEvent) {
        let ev = match e {
            // The mark's own confirmation is not a warning about anything.
            SessionEvent::Warning { code, .. } if code == FEEDBACK_CODE => return,
            SessionEvent::Warning { code, .. } => self
                .row(session, kind::WARNING)
                .name(code.as_str())
                .outcome(class_word(code)),
            SessionEvent::CommandIssued {
                identity, command, ..
            } => self
                .row(session, kind::OPERATOR_ACTION)
                .name(command.as_str())
                .attr("identity", identity.as_str())
                .attr("mid_turn", self.now.state != "idle"),
            SessionEvent::HeadAttached { kind: k, identity, .. } => self
                .row(session, kind::OPERATOR_ACTION)
                .name("attach")
                .attr("head", k.as_str())
                .attr("identity", identity.as_str()),
            SessionEvent::HeadDetached { kind: k, identity, .. } => self
                .row(session, kind::OPERATOR_ACTION)
                .name("detach")
                .attr("head", k.as_str())
                .attr("identity", identity.as_str()),
            SessionEvent::Subagent {
                subagent_id,
                state,
                role,
                model,
                ..
            } => self
                .row(session, kind::SUBAGENT)
                .name(role.as_str())
                .outcome(state.as_str())
                .attr("child", subagent_id.as_str())
                .attr_opt("model", (!model.is_empty()).then(|| model.as_str())),
            SessionEvent::JobSettled {
                job,
                state,
                produced,
                elapsed_ms,
                ..
            } => self
                .row(session, kind::JOB)
                .name(job.as_str())
                .outcome(state.as_str())
                .dur(*elapsed_ms)
                .n1(*produced),
            SessionEvent::DecisionAnswered {
                req_id,
                outcome,
                by,
                late,
                ..
            } => self
                .row(session, kind::DECISION_ANSWER)
                .name(by.kind.as_str())
                .outcome(match outcome {
                    DecisionOutcome::Selected { option_id } => option_id.as_str(),
                    DecisionOutcome::Cancelled => "cancelled",
                    DecisionOutcome::TimedOut => "timed-out",
                })
                .attr("req_id", req_id.as_str())
                .attr("late", *late),
            SessionEvent::QueueMoved { queue, id, to, .. } => Event::new(kind::QUEUE_MOVE)
                .session(session)
                .name(queue.as_str())
                .outcome(to.as_str())
                .attr("entry_id", id.as_str()),
            _ => return,
        };
        ev.emit();
    }
}

/// **An operator's mark, recorded** — the [`crate::protocol::ClientFrame::Feedback`] arm's whole
/// body, on the connection thread: place it (the row's turn, or the turn now), emit it, and say
/// so on the session's log so every head draws the confirmation. Nothing here waits for the
/// worker. Returns the sentence it published.
pub fn mark(
    hub: &crate::hub::Hub,
    head: &str,
    identity: &str,
    at_ms: u64,
    polarity: &str,
    note: &str,
    item_id: Option<&str>,
) -> String {
    let polarity = match polarity {
        "good" | "bad" => polarity,
        _ => "note",
    };
    let (now, row) = hub.metrics_now(item_id);
    let found = row.is_some();
    let (turn, round) = row.unwrap_or_else(|| (now.turn_id.clone(), now.round));
    let tags = letibot_metrics::tags(note);
    let session = hub.session_id();
    // A head whose clock is off by more than a minute either way is not believed: the press
    // time is the point of carrying it, and a wrong one is worse than the receipt time.
    let now_ms = letibot_metrics::now_ms();
    let at = if at_ms.abs_diff(now_ms) > 60_000 { now_ms } else { at_ms };
    let recorded = letibot_metrics::enabled();
    Event::new(kind::FEEDBACK)
        .at(at)
        .session(&session)
        .turn(&turn, (!turn.is_empty()).then_some(round))
        .name(polarity)
        .attr("note", note)
        .attr("tags", tags.clone())
        .attr_opt("item_id", item_id)
        .attr("head", head)
        .attr("identity", identity)
        .attr("at_state", now.state)
        .attr_opt("tool", now.tool.clone())
        .emit();
    let place = if turn.is_empty() {
        "before this session's first turn".to_string()
    } else if item_id.is_some() && !found {
        "on a row older than this daemon remembers — kept with the row's id, no turn".to_string()
    } else {
        format!("turn {turn}, round {}", round + 1)
    };
    let said = if recorded {
        let tags = if tags.is_empty() {
            String::new()
        } else {
            format!(" · #{}", tags.join(" #"))
        };
        format!("marked {polarity} · {place}{tags}")
    } else {
        "not recorded: metrics are off on this daemon (`[metrics] enabled = false` in \
         providers.toml)"
            .to_string()
    };
    hub.publish(SessionEvent::Warning {
        code: FEEDBACK_CODE.into(),
        detail: said.clone(),
        compaction: None,
    });
    said
}

/// The routine warning a mark is confirmed with — the heads draw it, and the tap skips it.
pub const FEEDBACK_CODE: &str = "feedback";

fn class_word(code: &str) -> &'static str {
    match crate::warning::class(code) {
        crate::warning::Class::Routine => "routine",
        crate::warning::Class::Failure => "failure",
        _ => "refused",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{FinishReason, Timings, Usage};

    fn started(id: &str, began: Option<u64>) -> SessionEvent {
        SessionEvent::TurnStarted {
            turn_id: id.into(),
            model: "m".into(),
            ledger_head: String::new(),
            began_ms: began,
        }
    }

    fn appended(id: &str) -> SessionEvent {
        SessionEvent::TranscriptAppended {
            item_id: id.into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        }
    }

    /// **One prompt's rounds are one turn**, keyed by the first round's id; a new prompt is a
    /// new turn; and each row remembers the turn and round it was written in.
    #[test]
    fn rounds_of_one_prompt_are_one_turn_and_rows_remember_theirs() {
        let mut t = Tap::default();
        t.observe("s", &started("tr#1", Some(100)));
        t.observe("s", &appended("tr.1"));
        t.observe(
            "s",
            &SessionEvent::TurnFinished {
                turn_id: "tr#1".into(),
                finish_reason: FinishReason::Eos,
                usage: Usage::default(),
                timings: Timings::default(),
            },
        );
        assert_eq!(t.now().state, "idle");
        t.observe("s", &started("tr#2", Some(100)));
        t.observe("s", &appended("tr.2"));
        assert_eq!(t.now().turn_id, "tr#1");
        assert_eq!(t.now().round, 1);
        t.observe("s", &started("tr#3", Some(200)));
        assert_eq!(t.now().turn_id, "tr#3", "a new prompt is a new turn");
        assert_eq!(t.now().round, 0);
        assert_eq!(t.item("tr.2"), Some(("tr#1".into(), 1)));
        assert_eq!(t.item("tr.9"), None);
    }

    #[test]
    fn the_hub_rows_carry_the_turn_they_happened_in() {
        let mut t = Tap::default();
        let ((), got) = letibot_metrics::capture(|| {
            t.observe("s", &started("tr#1", Some(1)));
            t.observe(
                "s",
                &SessionEvent::Warning {
                    code: "turn_failed".into(),
                    detail: "x".into(),
                    compaction: None,
                },
            );
            t.observe(
                "s",
                &SessionEvent::Warning {
                    code: FEEDBACK_CODE.into(),
                    detail: "marked".into(),
                    compaction: None,
                },
            );
            t.observe(
                "s",
                &SessionEvent::CommandIssued {
                    head_id: "h1".into(),
                    identity: "dead".into(),
                    command: "interrupt".into(),
                    client_request_id: "c".into(),
                    note: String::new(),
                },
            );
        });
        assert_eq!(got.len(), 2, "the feedback confirmation is not a warning row");
        assert_eq!(got[0].kind, kind::WARNING);
        assert_eq!(got[0].name.as_deref(), Some("turn_failed"));
        assert_eq!(got[0].outcome.as_deref(), Some("failure"));
        assert_eq!(got[0].turn_id.as_deref(), Some("tr#1"));
        assert_eq!(got[1].kind, kind::OPERATOR_ACTION);
        assert_eq!(got[1].get("mid_turn"), Some(&true.into()));
    }
}
