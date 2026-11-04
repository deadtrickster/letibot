//! Messages that arrive while the model is generating. A message is injected at the next step
//! boundary; one flagged `urgent` interrupts at the next token with its partial output kept and
//! marked truncated. Steering never rewrites or reorders what is already in the vector.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, TryRecvError};

use letibot_transcript::{TranscriptItem, UserPart};

/// A message that arrived mid-turn.
#[derive(Debug, Clone, PartialEq)]
pub struct SteeringMessage {
    pub text: String,
    /// Interrupt at the next token rather than at the next step boundary.
    pub urgent: bool,
    /// The operator's own words, a head's prompt. Consecutive operator messages coalesce into
    /// one held message, because the model reads one user turn and not a stack of fragments; a
    /// notice - the harness's injections, a fired monitor - stands alone.
    pub from_operator: bool,
    /// The parts that are not the operator's words: a picture or a file the composer attached,
    /// already turned into [`UserPart`]s. Here rather than in `text` because they are what the
    /// model is SENT, while the words carry the chip the person saw.
    pub extra: Vec<UserPart>,
    /// The session that said it, when it was another session - a parent's correction to a
    /// child, or any peer's message. One reader needs it and it is not the engine: after the
    /// turn, a message handed back must be written even though the turn did not read it.
    pub from_session: Option<String>,
    /// What sort of news this is when it is a DELIVERY rather than somebody's words - a monitor's
    /// firing. `None` for everything a person or a peer said.
    pub notice: Option<letibot_transcript::NoticeKind>,
}

impl SteeringMessage {
    pub fn normal(text: impl Into<String>) -> Self {
        SteeringMessage {
            text: text.into(),
            urgent: false,
            from_operator: false,
            extra: Vec::new(),
            from_session: None,
            notice: None,
        }
    }

    /// A delivery, not words: what the harness learned while the turn ran - a monitor firing.
    /// Its row is a `Notice`, so no reader has to decide whose words it is.
    pub fn notice(text: impl Into<String>) -> Self {
        SteeringMessage {
            text: text.into(),
            urgent: false,
            from_operator: false,
            extra: Vec::new(),
            from_session: None,
            notice: Some(letibot_transcript::NoticeKind::Monitor),
        }
    }

    /// Another session's words - a parent's correction to a child, a peer's message. Not the
    /// operator's, deliberately: an agent's utterance can neither authorise the act it races
    /// nor be taken back.
    pub fn from_a_session(text: impl Into<String>, from: impl Into<String>) -> Self {
        SteeringMessage {
            text: text.into(),
            urgent: false,
            from_operator: false,
            extra: Vec::new(),
            from_session: Some(from.into()),
            notice: None,
        }
    }

    /// The operator's own words, from a head's prompt. Coalescing-eligible.
    pub fn operator(text: impl Into<String>) -> Self {
        SteeringMessage {
            text: text.into(),
            urgent: false,
            from_operator: true,
            extra: Vec::new(),
            from_session: None,
            notice: None,
        }
    }

    /// The operator's words with what they attached - the same message, with the parts a
    /// picture or a file became. See [`SteeringMessage::extra`].
    pub fn operator_with(text: impl Into<String>, extra: Vec<UserPart>) -> Self {
        SteeringMessage {
            text: text.into(),
            urgent: false,
            from_operator: true,
            extra,
            from_session: None,
            notice: None,
        }
    }

    pub fn urgent(text: impl Into<String>) -> Self {
        SteeringMessage {
            text: text.into(),
            urgent: true,
            from_operator: false,
            extra: Vec::new(),
            from_session: None,
            notice: None,
        }
    }

    /// The transcript item this message becomes: a plain `User` item with exactly its own text,
    /// or a `Notice` for a delivery. No envelope or metadata - an inbound message must cost
    /// exactly its own tokens.
    pub fn to_item(&self) -> TranscriptItem {
        if let Some(kind) = self.notice {
            return TranscriptItem::Notice {
                kind,
                text: self.text.clone(),
            };
        }
        TranscriptItem::User {
            // `from_operator` is exactly this field's question: a steering line the model asked
            // for is the session's own words and one an operator typed is theirs, so a head
            // draws the operator's as theirs and the rest as the agent's.
            speaker: if self.from_operator {
                letibot_transcript::Speaker::Operator
            } else {
                letibot_transcript::Speaker::Agent
            },
            parts: {
                // The words first, and as ONE part - that is what the head's echo is
                // matched against (`row_says` joins the text parts, and `retire_pending`
                // spends the row's first lines), so an attachment must never split them.
                let mut parts = vec![UserPart::Text {
                    text: self.text.clone(),
                }];
                parts.extend(self.extra.iter().cloned());
                parts
            },
        }
    }
}

/// Where a turn looks for steering. A trait rather than a concrete channel because the flowy
/// connector, a TUI and a test each supply one differently, and none should have to exist for
/// the engine to compile.
pub trait SteeringSource {
    /// Non-blocking. Returns the next pending message, or `None`. Polled once per generated
    /// token, so it must not block and must not allocate on the empty path; this is a pull from
    /// a queue something else pushed to, not polling as a way to receive.
    fn try_next(&mut self) -> Option<SteeringMessage>;

    /// A take-back the operator issued from a head: every held operator message is dropped,
    /// because the operator pulled the queued line back to edit it and the held original must
    /// not land behind the resend. `true` when a take-back was acted on.
    fn try_withdraw(&mut self) -> bool {
        false
    }

    /// Hand a message back to the source, for a turn that failed: it is already out of the
    /// source's queue and appending it would advance the ledger on an error path. No default
    /// body, because a default that silently drops is the loss this exists to prevent.
    fn give_back(&mut self, msgs: Vec<SteeringMessage>);

    /// The next drain is a real step boundary rather than one token of a stream. A source whose
    /// answer costs a read of the whole session - the results a turn may deliver do - gives it
    /// here and on no other poll, so the cost is once per round instead of once per token.
    fn at_a_step_boundary(&mut self) {}
}

/// Nothing ever arrives. The default for a turn with no head attached.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSteering;

impl SteeringSource for NoSteering {
    fn try_next(&mut self) -> Option<SteeringMessage> {
        None
    }
    fn give_back(&mut self, _msgs: Vec<SteeringMessage>) {
        // Nothing ever arrives, so nothing is ever held and nothing is ever handed
        // back. This is not the same as dropping: there is no message to drop, and
        // a `Pending` that absorbed from this source is empty by construction.
    }
}

/// An `mpsc` channel as a steering source.
pub struct ChannelSteering {
    rx: Receiver<SteeringMessage>,
    /// A disconnected sender is not the same as an empty queue, and conflating
    /// them is how a dropped subscription looks like a quiet room. Recorded so the
    /// engine can raise it once rather than checking forever.
    disconnected: bool,
    /// Messages handed back by a failed turn, drained before the channel so the
    /// next poll finds them first. See [`SteeringSource::give_back`].
    held: VecDeque<SteeringMessage>,
}

impl ChannelSteering {
    pub fn new(rx: Receiver<SteeringMessage>) -> Self {
        ChannelSteering {
            rx,
            disconnected: false,
            held: VecDeque::new(),
        }
    }

    pub fn disconnected(&self) -> bool {
        self.disconnected
    }
}

impl SteeringSource for ChannelSteering {
    fn try_next(&mut self) -> Option<SteeringMessage> {
        // A handed-back message comes out first, with the flags it had when it was
        // taken: it was taken before anything still in the channel, and an interrupt
        // is not something a later message reorders.
        if let Some(m) = self.held.pop_front() {
            return Some(m);
        }
        match self.rx.try_recv() {
            Ok(m) => Some(m),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.disconnected = true;
                None
            }
        }
    }
    fn give_back(&mut self, msgs: Vec<SteeringMessage>) {
        // A failed turn commits nothing, so the words go back rather than being appended. They
        // are drained by the next `try_next` before the channel, so the next round's greedy
        // pre-poll puts them in the prompt.
        for m in msgs {
            self.held.push_back(m);
        }
    }
}

/// Messages held for the next step boundary.
#[derive(Debug, Default)]
pub struct Pending {
    queued: Vec<SteeringMessage>,
    /// An urgent message taken out of the source before the caller was in a
    /// position to act on it. See [`Pending::hold_urgent`].
    urgent: Option<SteeringMessage>,
}

impl Pending {
    pub fn new() -> Self {
        Pending::default()
    }

    /// Drain a source, keeping non-urgent messages and returning the first urgent one: called
    /// once per generated token, so the empty path is one `try_recv`. The operator's
    /// consecutive messages merge into one; a notice stands alone and splits the run.
    pub fn absorb(&mut self, source: &mut dyn SteeringSource) -> Option<SteeringMessage> {
        // A held urgent first, and before the withdraw check: it was already taken out of the
        // source, so a take-back that arrived after it cannot reach it, and an interrupt is not
        // something the operator withdraws by editing a line.
        if let Some(u) = self.urgent.take() {
            // The source is still drained: an early return would skip `try_next` on every
            // poll, so a prompt typed behind a long call could not be absorbed at all while an
            // urgent was held. The held one still goes first, and a new urgent is held again.
            if let Some(found) = self.drain(source) {
                self.urgent = Some(found);
            }
            return Some(u);
        }
        if source.try_withdraw() {
            // The whole held operator run, in one go. The `retain` runs BEFORE the drain below,
            // so a corrected resend still in the source survives and only the already-held run
            // is dropped; a notice is never dropped, because it is not the operator's to recall.
            self.queued.retain(|m| !m.from_operator);
        }
        self.drain(source)
    }

    /// Drain the source once: queue every non-urgent message, and return the first urgent one
    /// without queueing it, so everything after it stays in the source. Extracted so the
    /// held-urgent path can use the SAME rule rather than a second copy of it.
    fn drain(&mut self, source: &mut dyn SteeringSource) -> Option<SteeringMessage> {
        while let Some(m) = source.try_next() {
            if m.urgent {
                return Some(m);
            }
            if m.from_operator
                && let Some(last) = self.queued.last_mut()
                && last.from_operator
            {
                last.text.push('\n');
                last.text.push_str(&m.text);
                // And the parts come along, or a prompt typed behind another one
                // would lose its picture at the boundary - the merge keeps one item, so
                // it has to keep everything that item was going to be.
                last.extra.extend(m.extra);
            } else {
                self.queued.push(m);
            }
        }
        None
    }

    /// Put an urgent message back, for the next [`Self::absorb`] to return. The greedy poll
    /// before a generation has no generation to interrupt yet, and dropping the message there
    /// would turn an interrupt into silence; holding it means the first poll finds it.
    pub fn hold_urgent(&mut self, m: SteeringMessage) {
        self.urgent = Some(m);
    }

    /// Everything held, in arrival order, as transcript items. Called at the step
    /// boundary.
    pub fn take_items(&mut self) -> Vec<TranscriptItem> {
        self.queued.drain(..).map(|m| m.to_item()).collect()
    }

    /// Everything held, back to the source, for a turn that failed: the mirror of
    /// [`Self::take_items`]. They go back in arrival order with any urgent first, and the
    /// `Pending` is empty afterwards, so the next round's greedy pre-poll puts them in.
    pub fn give_back(&mut self, source: &mut dyn SteeringSource) {
        let mut msgs = Vec::new();
        if let Some(u) = self.urgent.take() {
            msgs.push(u);
        }
        msgs.append(&mut self.queued);
        if !msgs.is_empty() {
            source.give_back(msgs);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    pub fn len(&self) -> usize {
        self.queued.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    struct Fixed(Vec<SteeringMessage>);

    impl SteeringSource for Fixed {
        fn try_next(&mut self) -> Option<SteeringMessage> {
            if self.0.is_empty() {
                None
            } else {
                Some(self.0.remove(0))
            }
        }
        fn give_back(&mut self, msgs: Vec<SteeringMessage>) {
            // A handed-back message goes to the front, so the next `try_next` finds
            // it first: it was taken before anything still in the vector.
            self.0.splice(0..0, msgs);
        }
    }

    /// As a unit: a message costs its own text and nothing else.
    #[test]
    fn a_steering_message_becomes_a_plain_user_item_with_no_envelope() {
        let m = SteeringMessage::normal("the spec changed - RFC 2812 rather than 1459");
        let TranscriptItem::User { parts, .. } = m.to_item() else {
            panic!("steering must not invent a role")
        };
        assert_eq!(parts.len(), 1);
        let UserPart::Text { text } = &parts[0] else {
            panic!()
        };
        assert_eq!(text, "the spec changed - RFC 2812 rather than 1459");
    }

    /// A held urgent must not starve the source: an early return on every poll
    /// would never reach `try_next` again, so a prompt typed behind a long call is never absorbed.
    #[test]
    fn a_held_urgent_does_not_starve_the_source() {
        let mut src = Fixed(vec![
            SteeringMessage::urgent("ABORT"),
            SteeringMessage::operator("my prompt behind a long call"),
        ]);
        let mut pending = Pending::new();

        // the greedy poll before a generation: the urgent is taken and HELD, because there is no
        // generation here to act on yet
        let u = pending.absorb(&mut src).expect("the urgent one comes back");
        assert_eq!(u.text, "ABORT");
        pending.hold_urgent(u);

        // THE NEXT POLL MUST STILL REACH THE SOURCE. It returns the held urgent (it came first),
        // and it drains what is behind it - which is the operator's prompt.
        let again = pending
            .absorb(&mut src)
            .expect("the held one is still returned");
        assert_eq!(again.text, "ABORT", "the held urgent still goes first");
        assert_eq!(
            pending.len(),
            1,
            "**and the operator's prompt was absorbed rather than stranded**: there is nothing left \
             in the source either"
        );
        assert!(src.0.is_empty(), "the source is drained: {:?}", src.0);
        let items = pending.take_items();
        assert_eq!(items.len(), 1, "one held message, ready for the boundary");
    }

    /// And a SECOND urgent found while draining is held rather than dropped, so fixing the
    /// starvation cannot lose an interrupt instead.
    #[test]
    fn an_urgent_found_while_draining_is_held_not_dropped() {
        let mut src = Fixed(vec![
            SteeringMessage::urgent("FIRST"),
            SteeringMessage::urgent("SECOND"),
        ]);
        let mut pending = Pending::new();
        let first = pending.absorb(&mut src).expect("the first");
        pending.hold_urgent(first);

        let again = pending.absorb(&mut src).expect("the held one");
        assert_eq!(again.text, "FIRST", "the earlier one goes first");
        let third = pending
            .absorb(&mut src)
            .expect("**the second was not lost**");
        assert_eq!(third.text, "SECOND");
    }

    #[test]
    fn ordinary_messages_wait_for_the_step_boundary() {
        let mut src = Fixed(vec![
            SteeringMessage::normal("one"),
            SteeringMessage::normal("two"),
        ]);
        let mut pending = Pending::new();
        assert!(pending.absorb(&mut src).is_none(), "nothing interrupts");
        assert_eq!(pending.len(), 2);
        assert_eq!(pending.take_items().len(), 2);
        assert!(pending.is_empty());
    }

    #[test]
    fn an_urgent_message_stops_the_scan_and_is_returned_rather_than_queued() {
        let mut src = Fixed(vec![
            SteeringMessage::normal("one"),
            SteeringMessage::urgent("ABORT"),
            SteeringMessage::normal("three"),
        ]);
        let mut pending = Pending::new();
        let urgent = pending.absorb(&mut src).expect("the urgent one");
        assert_eq!(urgent.text, "ABORT");
        // The one before it is still held; the one after is still in the source
        // and will be absorbed at the next boundary. Nothing is dropped.
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn arrival_order_is_preserved_because_a_correction_may_correct_a_correction() {
        let mut src = Fixed(vec![
            SteeringMessage::normal("a"),
            SteeringMessage::normal("b"),
            SteeringMessage::normal("c"),
        ]);
        let mut pending = Pending::new();
        pending.absorb(&mut src);
        let texts: Vec<String> = pending
            .take_items()
            .into_iter()
            .map(|i| match i {
                TranscriptItem::User { parts, .. } => match &parts[0] {
                    UserPart::Text { text } => text.clone(),
                    _ => unreachable!(),
                },
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(texts, ["a", "b", "c"]);
    }

    /// The operator typing behind a long tool call: three fragments queued, one
    /// user turn at the boundary.
    #[test]
    fn the_operators_consecutive_messages_are_one_message() {
        let mut src = Fixed(vec![
            SteeringMessage::operator("also fix the parser"),
            SteeringMessage::operator("and add a test for it"),
            SteeringMessage::operator("run the suite after"),
        ]);
        let mut pending = Pending::new();
        assert!(pending.absorb(&mut src).is_none());
        assert_eq!(pending.len(), 1, "one held message, not three fragments");
        let items = pending.take_items();
        assert_eq!(items.len(), 1);
        let TranscriptItem::User { parts, .. } = &items[0] else {
            panic!("steering is a user item")
        };
        let UserPart::Text { text } = &parts[0] else {
            panic!()
        };
        assert_eq!(
            text,
            "also fix the parser\nand add a test for it\nrun the suite after"
        );
    }

    /// A notice stands alone and splits the run: what the operator typed after
    /// it is their own next message, not a continuation of the harness's words.
    #[test]
    fn a_notice_splits_the_operators_run() {
        let mut src = Fixed(vec![
            SteeringMessage::operator("one"),
            SteeringMessage::normal("watch fired: the build is red"),
            SteeringMessage::operator("two"),
        ]);
        let mut pending = Pending::new();
        assert!(pending.absorb(&mut src).is_none());
        assert_eq!(pending.len(), 3, "operator, notice, operator");
    }

    struct WithWithdraw {
        msgs: Vec<SteeringMessage>,
        withdraw: bool,
    }

    impl SteeringSource for WithWithdraw {
        fn try_next(&mut self) -> Option<SteeringMessage> {
            if self.msgs.is_empty() {
                None
            } else {
                Some(self.msgs.remove(0))
            }
        }
        fn try_withdraw(&mut self) -> bool {
            std::mem::take(&mut self.withdraw)
        }
        fn give_back(&mut self, msgs: Vec<SteeringMessage>) {
            self.msgs.splice(0..0, msgs);
        }
    }

    /// The recall-to-edit flow's daemon half: the held original must not land behind the edited
    /// resend. A notice held beside it is not the operator's to take back, and stays.
    #[test]
    fn a_take_back_drops_the_held_operator_text_and_keeps_notices() {
        let mut src = WithWithdraw {
            msgs: vec![SteeringMessage::operator("half a thought")],
            withdraw: false,
        };
        let mut pending = Pending::new();
        pending.absorb(&mut src);
        assert_eq!(pending.len(), 1);
        src.withdraw = true;
        assert!(pending.absorb(&mut src).is_none());
        assert!(pending.is_empty(), "the recalled line is not held any more");

        let mut src = WithWithdraw {
            msgs: vec![
                SteeringMessage::operator("one"),
                SteeringMessage::normal("a fired monitor"),
            ],
            withdraw: false,
        };
        let mut pending = Pending::new();
        pending.absorb(&mut src);
        src.withdraw = true;
        pending.absorb(&mut src);
        assert_eq!(pending.len(), 1, "the notice is not the operator's to drop");
        let items = pending.take_items();
        let TranscriptItem::User { parts, .. } = &items[0] else {
            panic!("steering is a user item")
        };
        let UserPart::Text { text } = &parts[0] else {
            panic!()
        };
        assert_eq!(text, "a fired monitor");
    }

    #[test]
    fn a_dropped_sender_is_recorded_rather_than_read_as_an_empty_queue() {
        let (tx, rx) = channel();
        let mut s = ChannelSteering::new(rx);
        tx.send(SteeringMessage::normal("hi")).unwrap();
        assert!(s.try_next().is_some());
        assert!(!s.disconnected());
        drop(tx);
        assert!(s.try_next().is_none());
        assert!(
            s.disconnected(),
            "silence from a dropped subscription is not a quiet room"
        );
    }

    /// A failed turn gives the held messages back to the source, in order, with any urgent
    /// first: a message absorbed mid-generation is out of the source's queue, and a turn that
    /// fails commits nothing, so the words must go back rather than be appended.
    #[test]
    fn give_back_returns_the_queued_run_and_a_held_urgent_in_order() {
        let mut src = Fixed(vec![
            SteeringMessage::operator("first line"),
            SteeringMessage::operator("second line"),
            SteeringMessage::urgent("ABORT"),
            SteeringMessage::normal("a notice"),
        ]);
        let mut pending = Pending::new();

        // The greedy pre-poll: the operator lines coalesce, the urgent is returned
        // and held (there is no generation to act on yet), and the notice behind it
        // stays in the source.
        let u = pending.absorb(&mut src).expect("the urgent one comes back");
        pending.hold_urgent(u);

        // A later poll: the held urgent is returned again (and re-held), and the
        // notice behind it is absorbed.
        let u = pending
            .absorb(&mut src)
            .expect("the held urgent comes back again");
        pending.hold_urgent(u);

        assert_eq!(
            pending.len(),
            2,
            "one coalesced operator run and one notice"
        );
        assert!(pending.urgent.is_some(), "the urgent is still held");

        // The turn fails: everything held goes back to the source, urgent first.
        pending.give_back(&mut src);
        assert!(
            pending.is_empty(),
            "the Pending is empty after the give-back"
        );
        assert!(pending.urgent.is_none());

        // The source has them back, in order: the urgent first, then the queued run.
        let back: Vec<SteeringMessage> = std::iter::from_fn(|| src.try_next()).collect();
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].text, "ABORT", "the urgent goes first");
        assert!(back[0].urgent);
        assert_eq!(
            back[1].text, "first line\nsecond line",
            "the coalesced operator run"
        );
        assert!(back[1].from_operator);
        assert_eq!(back[2].text, "a notice");
        assert!(!back[2].from_operator);
    }
}
