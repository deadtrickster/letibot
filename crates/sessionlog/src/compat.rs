//! What a head older than [`TURN_ENDED_SINCE`] is sent in place of a frame it has no arm for.
//! It has no `TurnEnded`, so the prompt's end reaches it as the `TurnFinished` it already reads.

use std::borrow::Cow;

use crate::event::{Envelope, FinishReason, SessionEvent, Timings, Usage};
use crate::protocol::TURN_ENDED_SINCE;
use crate::view::{Snapshot, TurnState};

/// One connection's translation. Holds the last round that did not end its prompt, because the
/// ending an older head needs carries that round's numbers.
#[derive(Debug, Default)]
pub(crate) struct OlderHead {
    older: bool,
    open_round: Option<(String, FinishReason, Usage, Timings)>,
}

impl OlderHead {
    pub(crate) fn new(head_protocol: u32, snapshot: Option<&Snapshot>) -> Self {
        let mut me = OlderHead {
            older: head_protocol < TURN_ENDED_SINCE,
            open_round: None,
        };
        me.seed(snapshot);
        me
    }

    /// A snapshot replaces what this connection saw, so the open round is read off it again.
    pub(crate) fn seed(&mut self, snapshot: Option<&Snapshot>) {
        self.open_round = snapshot
            .and_then(|s| s.turn.as_ref())
            .and_then(|t| match &t.state {
                TurnState::Finished {
                    finish_reason,
                    usage,
                    timings,
                    ends_turn: false,
                } => Some((t.turn_id.clone(), finish_reason.clone(), *usage, *timings)),
                _ => None,
            });
    }

    /// The frame to send for `env`, or `None` to send nothing.
    pub(crate) fn translate<'a>(&mut self, env: &'a Envelope) -> Option<Cow<'a, Envelope>> {
        if !self.older {
            return Some(Cow::Borrowed(env));
        }
        match &env.event {
            SessionEvent::TurnFinished {
                turn_id,
                finish_reason,
                usage,
                timings,
                ends_turn,
                ..
            } => {
                self.open_round = (!ends_turn)
                    .then(|| (turn_id.clone(), finish_reason.clone(), *usage, *timings));
                Some(Cow::Borrowed(env))
            }
            SessionEvent::TurnEnded { turn_id } => {
                let (id, finish_reason, usage, timings) =
                    self.open_round.take().filter(|(id, ..)| id == turn_id)?;
                // The last round's numbers keep the meter, and no cost: that round billed it.
                Some(Cow::Owned(Envelope {
                    event: SessionEvent::TurnFinished {
                        turn_id: id,
                        finish_reason,
                        usage: Usage {
                            cost_micros_usd: None,
                            ..usage
                        },
                        timings,
                        ends_turn: true,
                        prefix_check: None,
                        dialect_template_sha: String::new(),
                    },
                    ..env.clone()
                }))
            }
            _ => Some(Cow::Borrowed(env)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::Hub;
    use crate::testing::{turn_finished, turn_finished_round, turn_started};

    fn endings(head: &mut OlderHead, hub: &Hub) -> (usize, usize, Vec<Usage>) {
        let (mut rounds, mut turn_ended, mut ends) = (0, 0, Vec::new());
        for env in hub.retained() {
            let Some(out) = head.translate(&env) else {
                continue;
            };
            match &out.event {
                SessionEvent::TurnFinished {
                    ends_turn, usage, ..
                } => {
                    rounds += 1;
                    if *ends_turn {
                        ends.push(*usage);
                    }
                }
                SessionEvent::TurnEnded { .. } => turn_ended += 1,
                _ => {}
            }
        }
        (rounds, turn_ended, ends)
    }

    /// The common path: the last round says `ends_turn`, and an older head is sent no `TurnEnded`.
    #[test]
    fn an_older_head_ends_the_turn_on_the_last_round_and_never_sees_turn_ended() {
        let hub = Hub::new("s-compat");
        hub.publish(turn_started("t1"));
        hub.publish(turn_finished_round("t1"));
        hub.publish(turn_started("t2"));
        hub.publish(turn_finished("t2"));
        let mut older = OlderHead::new(TURN_ENDED_SINCE - 1, None);
        let (rounds, turn_ended, ends) = endings(&mut older, &hub);
        assert_eq!((rounds, turn_ended, ends.len()), (2, 0, 1));
        let mut newer = OlderHead::new(TURN_ENDED_SINCE, None);
        assert_eq!(endings(&mut newer, &hub).1, 1, "a newer head is sent it");
    }

    /// A loop that returned between rounds ends with `TurnEnded` alone; an older head gets the
    /// ending it reads, with the last round's tokens and no second cost.
    #[test]
    fn an_older_head_still_ends_a_prompt_left_between_rounds() {
        let hub = Hub::new("s-compat-left");
        hub.publish(turn_started("t1"));
        let mut round = turn_finished_round("t1");
        if let SessionEvent::TurnFinished { usage, .. } = &mut round {
            usage.prompt_tokens = 1234;
            usage.cost_micros_usd = Some(5);
        }
        hub.publish(round);
        assert!(hub.end_the_prompt());
        let mut older = OlderHead::new(TURN_ENDED_SINCE - 1, None);
        let (rounds, turn_ended, ends) = endings(&mut older, &hub);
        assert_eq!((rounds, turn_ended), (2, 0));
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].prompt_tokens, 1234);
        assert_eq!(ends[0].cost_micros_usd, None);
    }

    /// A head that attached after the round reads the open round off its snapshot.
    #[test]
    fn an_older_head_that_attached_between_rounds_is_ended_from_its_snapshot() {
        let hub = Hub::new("s-compat-late");
        hub.publish(turn_started("t1"));
        hub.publish(turn_finished_round("t1"));
        let a = hub.attach("tui", "old", Default::default(), u64::MAX);
        let mut older = OlderHead::new(TURN_ENDED_SINCE - 1, a.snapshot.as_ref());
        let before = hub.retained().len();
        hub.end_the_prompt();
        let ended = hub.retained()[before..]
            .iter()
            .filter_map(|e| older.translate(e))
            .filter(|e| {
                matches!(
                    e.event,
                    SessionEvent::TurnFinished {
                        ends_turn: true,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(ended, 1);
    }
}
