//! What a head older than this daemon is sent in place of what it cannot decode. Every frame the
//! daemon sends unasked goes through here: the `Hello` and `Resync` snapshots, and each event.

use std::borrow::Cow;

use letibot_transcript::{Speaker, TranscriptItem};

use crate::event::{Envelope, FinishReason, SessionEvent, Timings, TodoBy, TodoEntry, Usage};
use crate::protocol::TURN_ENDED_SINCE;
use crate::view::{Snapshot, TurnState};

/// A change to what the daemon sends that an older head cannot decode, and whether this module
/// translates it for that head. A new variant on the daemon-to-head side lands with its row.
#[derive(Debug, Clone, Copy)]
pub struct Since {
    pub version: u32,
    pub what: &'static str,
    pub translated: bool,
}

/// `Speaker::Peer` landed under 74 with no bump, so a 74 head may lack it as well.
pub const SPEAKER_PEER_SINCE: u32 = 75;
pub const PARKED_SINCE: u32 = 75;
pub const TODO_BY_PEER_SINCE: u32 = 77;

pub const SINCE: &[Since] = &[
    Since {
        version: TODO_BY_PEER_SINCE,
        what: "the `Peer <handle>` todo author",
        translated: true,
    },
    Since {
        version: TURN_ENDED_SINCE,
        what: "`SessionEvent::TurnEnded`",
        translated: true,
    },
    Since {
        version: PARKED_SINCE,
        what: "`TurnState::Parked`",
        translated: true,
    },
    Since {
        version: SPEAKER_PEER_SINCE,
        what: "`Speaker::Peer` on a user row",
        translated: true,
    },
    Since {
        version: 66,
        what: "`Waiting::Limit` in a session's activity",
        translated: false,
    },
];

/// The oldest head this daemon serves: the newest change it does not translate. Below it a
/// snapshot would fail the head's decoder, so the attach is refused by name instead.
pub const OLDEST_HEAD: u32 = newest(SINCE, false);

/// What sets [`OLDEST_HEAD`], for the refusal below it.
pub(crate) fn floor_change() -> &'static str {
    SINCE
        .iter()
        .find(|c| !c.translated && c.version == OLDEST_HEAD)
        .map_or("", |c| c.what)
}

/// Heads at or past this need nothing from here.
const CURRENT_FROM: u32 = newest(SINCE, true);

const fn newest(table: &[Since], translated: bool) -> u32 {
    let mut i = 0;
    let mut at = 0;
    while i < table.len() {
        if table[i].translated == translated && table[i].version > at {
            at = table[i].version;
        }
        i += 1;
    }
    at
}

/// The snapshot as `head` can decode it.
pub(crate) fn snapshot_for(head: u32, mut s: Snapshot) -> Snapshot {
    if head >= CURRENT_FROM {
        return s;
    }
    for row in &mut s.items {
        if let Some(item) = &mut row.item {
            item_for(head, item);
        }
    }
    if head < PARKED_SINCE
        && let Some(turn) = &mut s.turn
        && let TurnState::Parked { model, .. } = &turn.state
    {
        // The park is terminal for the turn, which is what an interrupt that kept its text says.
        turn.state = TurnState::Interrupted {
            reason: format!("parked on a usage limit of {model}"),
            partial_kept: true,
        };
    }
    s
}

pub(crate) fn todos_for(head: u32, todos: &mut [TodoEntry]) {
    if head >= TODO_BY_PEER_SINCE {
        return;
    }
    for t in todos {
        if let TodoBy::Peer(handle) = &t.by {
            t.by = TodoBy::Parent(format!("Parent peer {handle}"));
        }
    }
}

fn item_for(head: u32, item: &mut TranscriptItem) {
    if head < SPEAKER_PEER_SINCE
        && let TranscriptItem::User { speaker, .. } = item
        && matches!(speaker, Speaker::Peer { .. })
    {
        // Never Operator: an older head would then draw a peer's words as the person's.
        *speaker = Speaker::Agent;
    }
}

fn needs_rewrite(head: u32, event: &SessionEvent) -> bool {
    let peer_item = |i: &TranscriptItem| {
        matches!(
            i,
            TranscriptItem::User {
                speaker: Speaker::Peer { .. },
                ..
            }
        )
    };
    match event {
        SessionEvent::TranscriptAppended { item: Some(i), .. }
        | SessionEvent::TranscriptContent { item: i, .. } => {
            head < SPEAKER_PEER_SINCE && peer_item(i)
        }
        SessionEvent::TodosUpdated { todos } => {
            head < TODO_BY_PEER_SINCE && todos.iter().any(|t| matches!(t.by, TodoBy::Peer(_)))
        }
        _ => false,
    }
}

/// One event as `head` can decode it, with no per-connection state: what a peek is sent.
pub(crate) fn event_for(head: u32, env: &Envelope) -> Cow<'_, Envelope> {
    if head >= CURRENT_FROM || !needs_rewrite(head, &env.event) {
        return Cow::Borrowed(env);
    }
    let mut out = env.clone();
    match &mut out.event {
        SessionEvent::TranscriptAppended { item: Some(i), .. }
        | SessionEvent::TranscriptContent { item: i, .. } => item_for(head, i),
        SessionEvent::TodosUpdated { todos } => todos_for(head, todos),
        _ => {}
    }
    Cow::Owned(out)
}

/// One connection's translation. Holds the last round that did not end its prompt, because the
/// ending an older head needs carries that round's numbers.
#[derive(Debug, Default)]
pub(crate) struct OlderHead {
    head: u32,
    open_round: Option<(String, FinishReason, Usage, Timings)>,
}

impl OlderHead {
    pub(crate) fn new(head_protocol: u32, snapshot: Option<&Snapshot>) -> Self {
        let mut me = OlderHead {
            head: head_protocol,
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
        if self.head >= CURRENT_FROM {
            return Some(Cow::Borrowed(env));
        }
        let ended = self.ending(env)?;
        Some(match ended {
            Cow::Borrowed(e) => event_for(self.head, e),
            Cow::Owned(e) => Cow::Owned(event_for(self.head, &e).into_owned()),
        })
    }

    /// A head with no `TurnEnded` gets the prompt's end as the `TurnFinished` it reads.
    fn ending<'a>(&mut self, env: &'a Envelope) -> Option<Cow<'a, Envelope>> {
        if self.head >= TURN_ENDED_SINCE {
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

    /// The events an older head is sent live carry the same translation as its snapshot.
    #[test]
    fn an_older_head_is_sent_a_peer_row_and_a_peer_todo_it_can_decode() {
        use crate::event::TodoStatus;
        use letibot_transcript::UserPart;
        let hub = Hub::new("s-compat-peer");
        hub.publish(SessionEvent::TranscriptContent {
            item_id: "p1".into(),
            item: Box::new(TranscriptItem::User {
                parts: vec![UserPart::Text { text: "hi".into() }],
                speaker: Speaker::Peer {
                    from: "cc@x".into(),
                },
            }),
        });
        hub.publish(SessionEvent::TodosUpdated {
            todos: vec![TodoEntry {
                content: "t".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Peer("cc@x".into()),
                when: None,
                needs: Vec::new(),
            }],
        });
        let sent = |head: u32| -> Vec<SessionEvent> {
            let mut older = OlderHead::new(head, None);
            hub.retained()
                .iter()
                .filter_map(|e| older.translate(e).map(|c| c.into_owned().event))
                .collect()
        };
        let old = sent(SPEAKER_PEER_SINCE - 1);
        assert!(old.iter().all(|e| !needs_rewrite(0, e)), "{old:?}");
        assert!(old.iter().any(|e| matches!(e,
            SessionEvent::TranscriptContent { item, .. }
                if matches!(**item, TranscriptItem::User { speaker: Speaker::Agent, .. }))));
        let now = sent(crate::protocol::PROTOCOL_VERSION);
        assert!(
            now.iter().all(|e| needs_rewrite(0, e)),
            "a current head is sent them as they are"
        );
    }

    #[test]
    fn the_floor_is_the_newest_change_with_no_translation() {
        assert_eq!(OLDEST_HEAD, 66);
        assert!(floor_change().contains("Waiting::Limit"));
        assert!(
            SINCE
                .iter()
                .all(|c| c.version <= crate::protocol::PROTOCOL_VERSION)
        );
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
