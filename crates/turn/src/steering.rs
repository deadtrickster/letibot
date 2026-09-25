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
}

/// Nothing ever arrives. The default for a turn with no head attached.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSteering;

impl SteeringSource for NoSteering {
    fn try_next(&mut self) -> Option<SteeringMessage> {
        None
    }
}

/// An `mpsc` channel as a steering source.
pub struct ChannelSteering {
    rx: Receiver<SteeringMessage>,
    /// A disconnected sender is not the same as an empty queue, and conflating
    /// them is how a dropped subscription looks like a quiet room. Recorded so the
    /// engine can raise it once rather than checking forever.
    disconnected: bool,
}

impl ChannelSteering {
    pub fn new(rx: Receiver<SteeringMessage>) -> Self {
        ChannelSteering {
            rx,
            disconnected: false,
        }
    }

    pub fn disconnected(&self) -> bool {
        self.disconnected
    }
}

impl SteeringSource for ChannelSteering {
    fn try_next(&mut self) -> Option<SteeringMessage> {
        match self.rx.try_recv() {
            Ok(m) => Some(m),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.disconnected = true;
                None
            }
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
}
