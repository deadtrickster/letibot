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
}

impl SteeringMessage {
    pub fn normal(text: impl Into<String>) -> Self {
        SteeringMessage {
            text: text.into(),
            urgent: false,
        }
    }

    pub fn urgent(text: impl Into<String>) -> Self {
        SteeringMessage {
            text: text.into(),
            urgent: true,
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
}

impl Pending {
    pub fn new() -> Self {
        Pending::default()
    }

    /// Drain a source, keeping non-urgent messages and returning the first urgent
    /// one. Called once per generated token, so the empty path is one `try_recv`.
    pub fn absorb(&mut self, source: &mut dyn SteeringSource) -> Option<SteeringMessage> {
        while let Some(m) = source.try_next() {
            if m.urgent {
                return Some(m);
            }
            self.queued.push(m);
        }
        None
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
        let TranscriptItem::User { parts } = m.to_item() else {
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
                TranscriptItem::User { parts } => match &parts[0] {
                    UserPart::Text { text } => text.clone(),
                    _ => unreachable!(),
                },
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(texts, ["a", "b", "c"]);
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
