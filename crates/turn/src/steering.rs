//! §5.8 — messages that arrive while the model is generating.
//!
//! The plan's three options and its choice:
//!
//! * *Queue until the turn ends* — wrong for the common case. The message is
//!   usually a correction, and waiting 90 seconds to act on it wastes the
//!   generation it was correcting.
//! * *Interrupt immediately* — truncates mid-stream, and a partial assistant turn
//!   has to be kept for cache reasons while being useless as content.
//! * **Inject at the next step boundary — chosen.** After the current generation
//!   completes, or after the current tool batch settles, whichever comes first. The
//!   message is appended as a `User` item and the loop continues with it in context.
//!
//! With an escape hatch: a message flagged `urgent` (of which `ABORT` is the
//! extreme form) interrupts at the next **token**, and the partial output is kept
//! as a transcript item marked truncated — for §13.2's cache reason, and because
//! the model should see what it had started to say.
//!
//! # The rule that keeps this three lines of policy rather than a subsystem
//!
//! **Steering never rewrites or reorders anything already in the vector.** A
//! steering message is an append like any other. There is no code path here that
//! touches the ledger; the engine appends the resulting `User` item through the
//! same door every other item goes through.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, TryRecvError};

use letibot_transcript::{TranscriptItem, UserPart};

/// A message that arrived mid-turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteeringMessage {
    pub text: String,
    /// Interrupt at the next token rather than at the next step boundary.
    pub urgent: bool,
    /// The operator's own words — a head's prompt. Consecutive operator
    /// messages coalesce into one held message (the model reads one user turn,
    /// not a stack of fragments), and a take-back drops them. A notice — the
    /// harness's own injections, a fired monitor — stands alone: merging it
    /// into the operator's text would put the harness's words in the
    /// operator's mouth.
    pub from_operator: bool,
}

impl SteeringMessage {
    pub fn normal(text: impl Into<String>) -> Self {
        SteeringMessage {
            text: text.into(),
            urgent: false,
            from_operator: false,
        }
    }

    /// The operator's own words, from a head's prompt. Coalescing-eligible.
    pub fn operator(text: impl Into<String>) -> Self {
        SteeringMessage {
            text: text.into(),
            urgent: false,
            from_operator: true,
        }
    }

    pub fn urgent(text: impl Into<String>) -> Self {
        SteeringMessage {
            text: text.into(),
            urgent: true,
            from_operator: false,
        }
    }

    /// The transcript item this message becomes.
    ///
    /// **A plain `User` item with exactly its own text.** No tool envelope, no
    /// wrapper, no listener bookkeeping — §18.1-I11 says an inbound message must
    /// cost exactly its own tokens, and this is the line that decides it. The
    /// invariant regresses the first time somebody adds "helpful" metadata here.
    pub fn to_item(&self) -> TranscriptItem {
        TranscriptItem::User {
            // **`from_operator` is exactly this field's question**, and it was already on the
            // message: a steering line the model asked for is the session's own words, and one
            // an operator typed is theirs. R42's rule is that a head draws a completion, a
            // salvage notice and a steering line as the second — so the ones that are the
            // operator's say so and the rest do not.
            speaker: if self.from_operator {
                letibot_transcript::Speaker::Operator
            } else {
                letibot_transcript::Speaker::Agent
            },
            parts: vec![UserPart::Text {
                text: self.text.clone(),
            }],
        }
    }
}

/// Where a turn looks for steering.
///
/// A trait rather than a concrete channel because the flowy connector (W12), a
/// TUI and a test each supply one differently, and none of them should have to
/// exist for the engine to compile.
pub trait SteeringSource {
    /// Non-blocking. Returns the next pending message, or `None`.
    ///
    /// Polled once per generated token for the urgent case, so it must not block
    /// and must not allocate on the empty path. Note this is a *pull from a queue
    /// something else pushed to* — §3.7's "no polling anywhere" is about how a
    /// message reaches the daemon, not about how the token loop drains an in-memory
    /// queue it already owns.
    fn try_next(&mut self) -> Option<SteeringMessage>;

    /// A take-back the operator issued from a head: every held **operator**
    /// message is dropped, because the operator pulled the queued line back
    /// into the composer to edit it and the held original must not land behind
    /// the edited resend. `true` when a take-back was acted on.
    ///
    /// Default: never — a source that cannot carry a take-back says so by
    /// doing nothing, and no implementor has to grow an arm for it.
    fn try_withdraw(&mut self) -> bool {
        false
    }

    /// **Hand a message back to the source, for a turn that failed.**
    ///
    /// A message is out of the source's queue the moment [`Pending::absorb`] takes
    /// it — `try_next` dequeues it — and a turn that fails commits nothing (§5.7).
    /// So the words must go back to the source rather than being appended:
    /// appending them would advance the ledger and the prefix on an error path,
    /// which is the same loss with a different sign. The source keeps them for the
    /// next [`Self::try_next`], so the retried or next round's greedy pre-poll puts
    /// them in the prompt.
    ///
    /// MEASURED on the operator's own head: they typed a prompt while a turn was
    /// running, the daemon accepted it, and the round then failed under §5.7. The
    /// words were out of the hub's queue and never in the transcript — the head
    /// drew `queued` for the rest of the session, and it was right to keep doing
    /// so, because the words were never appended and the head has no way to know
    /// that. This method is the door that puts them back where the head left them.
    ///
    /// **No default body.** A default that silently drops is exactly the loss this
    /// exists to prevent, so every implementor must state what it does with a
    /// message it is handed back. The test that holds the property end to end is
    /// `a_failed_turn_gives_the_operators_words_back_to_the_source` in `tests/`.
    fn give_back(&mut self, msgs: Vec<SteeringMessage>);
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
        // A failed turn commits nothing (§5.7), so the words go back rather than
        // being appended. They are held here and drained by the next `try_next`,
        // before the channel, so the retried or next round's greedy pre-poll puts
        // them in the prompt.
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

    /// Drain a source, keeping non-urgent messages and returning the first urgent
    /// one. Called once per generated token, so the empty path is one `try_recv`.
    ///
    /// **The operator's consecutive messages are one message.** A prompt typed
    /// behind a long tool call sits unconsumed for minutes, and the operator
    /// keeps typing; at the boundary those fragments would land as a stack of
    /// one-line user turns, each costing its own row and its own turn of the
    /// model's attention. They merge here instead — into the held operator
    /// text, newline-joined — so the model reads one user turn. A notice
    /// (harness injection, fired monitor) still stands alone, and splits the
    /// run: what the operator typed after it is a separate message, because
    /// merging across it would put the harness's words in the operator's mouth.
    ///
    /// A take-back is honoured first, for the same reason: the operator pulled
    /// the queued line into the composer to edit it, and the held original must
    /// not land behind the edited resend.
    pub fn absorb(&mut self, source: &mut dyn SteeringSource) -> Option<SteeringMessage> {
        // A held urgent first, and before the withdraw check: it was already taken
        // out of the source, so a take-back that arrived after it cannot reach it,
        // and an interrupt is not something the operator withdraws by editing a
        // line anyway.
        if let Some(u) = self.urgent.take() {
            // **AND THE SOURCE IS STILL DRAINED, WHICH IS A BUG FIX AND NOT A TIDY-UP.**
            //
            // MEASURED on the operator's own head: they typed a prompt behind a long tool call and
            // pressed `esc esc`. The interrupt became a held urgent, and from that moment THIS EARLY
            // RETURN was taken on every poll — so `source.try_next()` was never reached again, the
            // hub's queue was never drained, and **their prompt could not be absorbed at all.** The
            // head held the echo and drew `queued` for the rest of the session (verified: the string
            // appears in no item of 2,579, because it never landed). The interrupt and the queue are
            // not two independent features; they share this one slot, and an interrupt took the
            // queue out of the picture — *"interrupts and queue do not play well together"*.
            //
            // The docstring's own promise is the requirement it was breaking: *"the one after is
            // still in the source and will be absorbed at the next boundary."* With a held urgent
            // there was no drain at the next boundary.
            //
            // **The held one still goes first**, because it was taken out of the source first and an
            // interrupt is not something a later message reorders. A NEW urgent found while draining
            // is held for the next call rather than dropped, and everything after it stays in the
            // source, exactly as the loop above always left it.
            if let Some(found) = self.drain(source) {
                self.urgent = Some(found);
            }
            return Some(u);
        }
        if source.try_withdraw() {
            // **The whole held operator run, in one go** — the engine's half of the take-back, and
            // the half that satisfies the operator's ruling (*"yes whole messages queue is
            // dequeued in one go"*, 2026-09-25). The hub drops the prompts that have not reached
            // this poll yet; this drops the ones that have.
            //
            // **Two edges, and the order of the two statements here is what holds the second.**
            //
            // * **The corrected resend survives, and not by accident.** The operator recalls,
            //   edits, and sends; that resend can be sitting in the hub when this very call runs.
            //   It is not dropped because the `retain` runs *before* the drain below: the resend
            //   is still in the source, and what this drops is the run that was already held.
            //   Reverse the two and a take-back eats the operator's edit — the same class of
            //   defect `56151c1` measured on the hub's half, where the resend it ate had never
            //   been in the queue the operator took back.
            // * **A notice is never dropped.** `from_operator: false` is the harness's own words
            //   or a fired monitor, and they stand alone.
            //
            // **What this does NOT have is a head.** The take-back that reaches here says only
            // *a* head asked; the held run it drops is every operator line in the queue, whoever
            // sent it (`HubSteering::try_next` throws `QueuedCommand::head_id` away when it makes
            // the message, and the merge below is by `from_operator`, not by author). One head's
            // `↑` therefore takes a second head's held words with it, and two heads' consecutive
            // prompts land as one user turn. Filed in the parity document as a finding, not fixed
            // here: it wants an author on `SteeringMessage`, which is a change to this type's
            // shape rather than to this rule.
            self.queued.retain(|m| !m.from_operator);
        }
        self.drain(source)
    }

    /// **Drain the source once**: queue every non-urgent message, and RETURN the first urgent one
    /// without queueing it — everything after that urgent stays in the source, which is what the
    /// loop's `return` has always done.
    ///
    /// Extracted so the held-urgent path above can use the SAME rule. It was a `while` loop inlined
    /// in `absorb`, and the early return meant one of the two callers of that rule did not run it.
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
            } else {
                self.queued.push(m);
            }
        }
        None
    }

    /// **Put an urgent message back**, for the next [`Self::absorb`] to return.
    ///
    /// The greedy poll before a generation drains the source so the operator's
    /// words are in the prompt rather than behind it. An urgent message is not
    /// queued by `absorb` — it is handed back to be acted on — and the caller at
    /// that point has no generation to interrupt yet. Dropping it there would
    /// turn an interrupt into silence, which is the worst thing this type could
    /// do; holding it means the stream loop's first poll finds it and stops at
    /// the next token, exactly as it always did.
    pub fn hold_urgent(&mut self, m: SteeringMessage) {
        self.urgent = Some(m);
    }

    /// Everything held, in arrival order, as transcript items. Called at the step
    /// boundary.
    pub fn take_items(&mut self) -> Vec<TranscriptItem> {
        self.queued.drain(..).map(|m| m.to_item()).collect()
    }

    /// **Everything held, back to the source, for a turn that failed.**
    ///
    /// The mirror of [`Self::take_items`]: where `take_items` hands the held
    /// messages to the transcript at a step boundary, this hands them back to the
    /// source when the turn fails and commits nothing (§5.7). The operator's words
    /// are input, not what the model produced, and appending them on an error path
    /// would advance the ledger and the prefix — the same loss with a different
    /// sign. So they go back, in arrival order, with any urgent first: the urgent
    /// was taken to be acted on, and an interrupt is not something a later message
    /// reorders.
    ///
    /// After this, the `Pending` is empty: the messages are the source's again, and
    /// the next round's greedy pre-poll will put them in the prompt. The unit half
    /// of the test that holds this is
    /// `give_back_returns_the_queued_run_and_a_held_urgent_in_order` below; the end
    /// to end half is `a_failed_turn_gives_the_operators_words_back_to_the_source`
    /// in `tests/`.
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

    /// §18.1-I11 as a unit: a message costs its own text and nothing else.
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

    /// **A HELD URGENT MUST NOT STARVE THE SOURCE — and MEASURED, on the operator's own head, it
    /// did.** They typed a prompt behind a long tool call and pressed `esc esc`. The interrupt became
    /// a held urgent, and from then on `absorb` returned it from its early return on every poll, so
    /// `source.try_next()` was never reached again: the hub's queue was never drained and their
    /// prompt could not be absorbed AT ALL. The head held the echo and drew `queued` for the rest of
    /// the session — confirmed by searching every item in the transcript for the string, which
    /// appears in none of 2,579, because it never landed.
    ///
    /// This test is the promise the docstring already made: *"the one after is still in the source
    /// and will be absorbed at the next boundary."* With a held urgent there was no drain at the next
    /// boundary, which is the bug.
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

        // **THE NEXT POLL MUST STILL REACH THE SOURCE.** It returns the held urgent (it came first),
        // and it drains what is behind it — which is the operator's prompt.
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

    /// The recall-to-edit flow's daemon half: the operator pulled the queued
    /// line into the composer, so the held original must not land behind the
    /// edited resend. A notice held beside it is not the operator's to take
    /// back, and stays.
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

    /// **A failed turn gives the held messages back to the source, in order, with
    /// any urgent first.**
    ///
    /// The unit half of the fix for the measured loss: a message absorbed
    /// mid-generation is out of the source's queue the moment `absorb` takes it,
    /// and a turn that fails commits nothing (§5.7). So the words must go back to
    /// the source rather than being appended. `give_back` returns everything held —
    /// the queued run and a held urgent — in arrival order, with the urgent first,
    /// and leaves the `Pending` empty.
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
