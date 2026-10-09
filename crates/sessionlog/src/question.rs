//! The answer vocabulary a head sends back when a **person** was asked (D10).
//!
//! `PROTOCOL_VERSION` 5 adds this. It is the payload of
//! [`crate::protocol::ClientFrame::AnswerQuestion`].
//!
//! # Three things together, and none of them collapses into another
//!
//! The operator's requirement, verbatim:
//!
//! > *"we need user questions claude code style - model provided options with user
//! > notes and then opencode style free user reply input, not claude code 'chat
//! > later'"*
//!
//! So four fields, and the reason each is separate is the reason there are four:
//!
//! | field | what it is | why not folded in |
//! |---|---|---|
//! | `option` | an index into the options the model offered | the common case should cost one keystroke |
//! | `note` | a qualification **on that choice** | folding it into free text loses *which option* it qualifies, which is the load-bearing half of *"option B, but only for the CUDA box"* |
//! | `free` | a typed answer, on its own | a person who types a sentence has **answered**; Claude Code's *"chat later"* turns the question into a suspended turn instead, which is the behaviour this is named against |
//! | `abstain` | a **deliberate no-answer** | it is not silence and it is not an empty answer, and neither of those may be spelled the way a decision is |
//!
//! At least one of `option`, `free` or `abstain` must be present, and `note` needs
//! an `option` to qualify. [`QuestionAnswer::validate`] is the whole of that, and a
//! failure is a **refusal, never a repair** — see [`AnswerDefect`].
//!
//! # `abstain`: the fourth shape, and why it is not one of the other three
//!
//! The operator: *"each choice can have my note, and i can abstain or type my
//! answer"*. An abstention is a person **answering** — *"I am not answering this
//! one"* — and it has to be tellable from the two things that look like it and are
//! not:
//!
//! * **Silence** is no frame at all: the deadline passes, the tool reports
//!   `not_run`, and the sentence says *nobody answered*. Nobody looked at the
//!   question.
//! * **An empty answer** is a frame that arrived carrying nothing — the same bytes
//!   as `QuestionAnswer::default()`. It is refused as [`AnswerDefect::Empty`] and
//!   the question stays open, because an empty answer is not a shrug the harness
//!   may read.
//! * **An abstention** is a frame that says so, and it is the only one of the three
//!   a person meant.
//!
//! So it is its own field rather than an empty `free`, and it **stands alone**: an
//! abstention carrying a choice, a note or words is refused as
//! [`AnswerDefect::AbstainedAndAnswered`] rather than having one half win, because
//! a harness that picks which half a person meant is the harness this module
//! exists against.
//!
//! # This is deliberately NOT `OptionKind`
//!
//! [`crate::event::OptionKind`] is `AllowOnce | AllowAlways | RejectOnce |
//! RejectAlways`: an **adjudication** vocabulary, answering *may this run*. A
//! question answers *which way should I go*, and the consequences differ — an
//! adjudication that goes wrong runs something, an answer that goes wrong is
//! attributed to a person. Overloading one into the other would put a free-form
//! sentence where a policy engine reads a grant, so they stay two types and
//! [`crate::hub::Reply`] keeps them in two arms of one enum.
//!
//! # Why this shape exists twice
//!
//! `letibot_tools::builtins::intent::QuestionAnswer` is the same shape. That is the
//! **established pattern in this tree, not a smell**: `letibot-tools` is an optional
//! dependency here (`features = ["tools"]`) precisely so that a tool cannot reach
//! the wire, and `event::DecisionOption` / `event::OptionKind` are already duplicates
//! of `adjudicate::DecisionOption` / `adjudicate::OptionKind` for exactly this
//! reason, with `lift_tools` as the mapping. The daemon depends on both crates and
//! is where the two meet; neither crate learns about the other.
//!
//! If they ever disagree, the test at the bottom of this file is what says so.

use serde::{Deserialize, Serialize};

/// What a person said. **Not who** — see [`crate::hub::Reply`]: the answerer is what
/// the daemon knows about the connection the answer arrived on, never a field the
/// answer carries. An answer that could name its own answerer is an answer that
/// could name somebody else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    /// Index into the options the question offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<usize>,
    /// A qualification on the chosen option. Requires `option`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// A typed answer, and a first-class one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free: Option<String>,
    /// **A deliberate no-answer.** The person was asked and said, in as many
    /// words, that they are not answering this one.
    ///
    /// It is an answer, and it is **not** permission to proceed: a model that gets
    /// an abstention back must state the assumption it would have to make and stop,
    /// or ask a question the person can answer. It is also not
    /// [`Self::free`] with nothing in it — see this module's header for the three
    /// things that look like an abstention and are not.
    ///
    /// `default` so an older head's frame parses — it never sets this — and
    /// **elided when `false`**, like the three fields above it: an absent
    /// `abstain` and a `false` one are the same claim (*this is not an
    /// abstention*), so unlike `ClientFrame::Answer`'s added fields there is no
    /// distinction here for `skip_serializing_if` to lose. A frame that carries no
    /// abstention is byte-identical to what it was before this field existed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub abstain: bool,
}

/// `skip_serializing_if` for [`QuestionAnswer::abstain`]. A named function rather
/// than `std::ops::Not::not` because the reader of the attribute should not have to
/// work out which `Not` impl serde picked.
fn is_false(b: &bool) -> bool {
    !*b
}

/// Why an answer was not accepted.
///
/// Every one of these means **nobody has answered the question**. None of them is a
/// default, a declining, or something the harness may interpret: a malformed answer
/// leaves the question open, and the head is told so it can ask the person again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerDefect {
    /// No option, no note, no text.
    Empty,
    /// A note with no option chosen, so it qualifies nothing.
    NoteQualifiesNothing,
    /// An option index the question never offered.
    OptionNotOffered { chose: usize, offered: usize },
    /// An abstention that also carried an answer — a choice, a note, or words.
    AbstainedAndAnswered,
}

impl std::fmt::Display for AnswerDefect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnswerDefect::Empty => write!(
                f,
                "the answer carried no option, no note and no text, so there is nothing in \
                 it; the question is still open"
            ),
            AnswerDefect::NoteQualifiesNothing => write!(
                f,
                "the answer carried a note with no option chosen, so the note qualifies \
                 nothing; the question is still open. Reading it as a free-form answer \
                 would be guessing at what a person meant"
            ),
            AnswerDefect::OptionNotOffered { chose, offered } => write!(
                f,
                "the answer chose option {chose} and {offered} option(s) were offered; the \
                 question is still open"
            ),
            AnswerDefect::AbstainedAndAnswered => write!(
                f,
                "the answer said it was an abstention and also carried a choice, a note or \
                 words; nobody's answer is both, so it was not accepted and the question is \
                 still open. Deciding which half a person meant is not the harness's to do"
            ),
        }
    }
}

/// A `Rejected.reason` code a head can branch on, for an answer that did not
/// conform. Named like the other `REJECT_*` codes so a head handles it the same way.
pub const REJECT_MALFORMED_ANSWER: &str = "malformed answer";

impl QuestionAnswer {
    pub fn choosing(option: usize) -> Self {
        QuestionAnswer {
            option: Some(option),
            ..Default::default()
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn free(text: impl Into<String>) -> Self {
        QuestionAnswer {
            free: Some(text.into()),
            ..Default::default()
        }
    }

    /// **A deliberate no-answer.** The only way to spell it, and it stands alone —
    /// see this module's header for why an empty [`Self::free`] is not it.
    pub fn abstaining() -> Self {
        QuestionAnswer {
            abstain: true,
            ..Default::default()
        }
    }

    /// `offered` is how many options the question put up.
    pub fn validate(&self, offered: usize) -> Result<(), AnswerDefect> {
        let has_free = self.free.as_deref().is_some_and(|f| !f.trim().is_empty());
        let has_note = self.note.as_deref().is_some_and(|n| !n.trim().is_empty());
        // **An abstention is checked first, and it has to be alone.** A frame that
        // says "I am not answering" and also carries a choice is a frame with two
        // claims in it, and a validator that let either win would be the harness
        // deciding what a person meant.
        if self.abstain {
            return if self.option.is_some() || has_free || has_note {
                Err(AnswerDefect::AbstainedAndAnswered)
            } else {
                Ok(())
            };
        }
        if self.option.is_none() {
            if has_note {
                return Err(AnswerDefect::NoteQualifiesNothing);
            }
            if !has_free {
                return Err(AnswerDefect::Empty);
            }
            return Ok(());
        }
        match self.option {
            Some(i) if i >= offered => Err(AnswerDefect::OptionNotOffered { chose: i, offered }),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_shapes_a_person_can_answer_in_are_all_valid() {
        assert!(QuestionAnswer::choosing(0).validate(2).is_ok());
        assert!(
            QuestionAnswer::choosing(1)
                .with_note("only for the CUDA box")
                .validate(2)
                .is_ok()
        );
        assert!(
            QuestionAnswer::free("neither — split it")
                .validate(2)
                .is_ok()
        );
        // **And the fourth, which is an answer too.** It validates against a
        // question that offered no choices at all, because an abstention is not
        // about the ladder.
        assert!(QuestionAnswer::abstaining().validate(0).is_ok());
        assert!(QuestionAnswer::abstaining().validate(2).is_ok());
    }

    #[test]
    fn an_abstention_that_also_carries_an_answer_is_refused_rather_than_halved() {
        // The three ways a frame could say both things at once, and none of them
        // may be resolved by a rule about which field wins: which half a person
        // meant is not the harness's to decide.
        for both in [
            QuestionAnswer {
                option: Some(0),
                abstain: true,
                ..Default::default()
            },
            QuestionAnswer {
                note: Some("hmm".into()),
                abstain: true,
                ..Default::default()
            },
            QuestionAnswer {
                free: Some("go with b".into()),
                abstain: true,
                ..Default::default()
            },
        ] {
            assert_eq!(
                both.validate(2),
                Err(AnswerDefect::AbstainedAndAnswered),
                "{both:?}"
            );
            assert!(
                AnswerDefect::AbstainedAndAnswered
                    .to_string()
                    .contains("still open"),
                "a defect must leave the question open"
            );
        }
    }

    #[test]
    fn the_four_defects_are_refusals_and_not_repairs() {
        assert_eq!(
            QuestionAnswer::default().validate(2),
            Err(AnswerDefect::Empty)
        );
        assert_eq!(
            QuestionAnswer {
                note: Some("hmm".into()),
                ..Default::default()
            }
            .validate(2),
            Err(AnswerDefect::NoteQualifiesNothing)
        );
        assert_eq!(
            QuestionAnswer::choosing(2).validate(2),
            Err(AnswerDefect::OptionNotOffered {
                chose: 2,
                offered: 2
            })
        );
        for d in [
            AnswerDefect::Empty,
            AnswerDefect::NoteQualifiesNothing,
            AnswerDefect::OptionNotOffered {
                chose: 9,
                offered: 2,
            },
            AnswerDefect::AbstainedAndAnswered,
        ] {
            assert!(
                d.to_string().contains("still open"),
                "a defect must leave the question open: {d}"
            );
        }
    }

    /// **The three things that must not look alike**, in the bytes a head sends.
    ///
    /// * an abstention is `{"abstain":true}` — a frame, and a claim;
    /// * an empty answer is `{}` — a frame, and nothing in it (refused);
    /// * silence is no frame at all, which this type cannot spell and that is the
    ///   point: there is nothing here that means "nobody answered".
    #[test]
    fn an_abstention_is_its_own_bytes_and_not_an_empty_answer() {
        let abstain = serde_json::to_string(&QuestionAnswer::abstaining()).unwrap();
        assert_eq!(abstain, r#"{"abstain":true}"#, "{abstain}");
        let empty = serde_json::to_string(&QuestionAnswer::default()).unwrap();
        assert_eq!(empty, "{}", "{empty}");
        assert_ne!(abstain, empty);
        // And an answer that is not an abstention does not carry the field at all,
        // so every frame that existed before it did is byte-identical.
        let chose = serde_json::to_string(&QuestionAnswer::choosing(1)).unwrap();
        assert_eq!(chose, r#"{"option":1}"#, "{chose}");
        assert_eq!(
            serde_json::from_str::<QuestionAnswer>(&chose)
                .unwrap()
                .abstain,
            false,
            "an older head's frame parses as *not an abstention*"
        );
    }

    #[test]
    fn absent_fields_are_absent_on_the_wire_rather_than_null() {
        // A head that chose an option sends one key. `null`s would double the frame
        // and make "not answered" and "answered with nothing" look alike.
        let j = serde_json::to_string(&QuestionAnswer::choosing(1)).unwrap();
        assert_eq!(j, r#"{"option":1}"#, "{j}");
        let j2 = serde_json::to_string(&QuestionAnswer::free("x")).unwrap();
        assert_eq!(j2, r#"{"free":"x"}"#, "{j2}");
    }

    #[test]
    fn it_round_trips() {
        for a in [
            QuestionAnswer::choosing(1).with_note("only for the CUDA box"),
            QuestionAnswer::abstaining(),
        ] {
            let j = serde_json::to_string(&a).unwrap();
            assert_eq!(serde_json::from_str::<QuestionAnswer>(&j).unwrap(), a);
        }
    }

    /// The two copies of this shape must agree, or a head and a tool disagree about
    /// what a person said. This is the test the module docs promise.
    #[cfg(feature = "tools")]
    #[test]
    fn the_wire_shape_and_the_tool_shape_agree() {
        use letibot_tools::builtins::intent::QuestionAnswer as ToolAnswer;
        for (wire, tool) in [
            (QuestionAnswer::choosing(1), ToolAnswer::choosing(1)),
            (
                QuestionAnswer::choosing(1).with_note("n"),
                ToolAnswer::choosing(1).with_note("n"),
            ),
            (QuestionAnswer::free("f"), ToolAnswer::free("f")),
            (QuestionAnswer::abstaining(), ToolAnswer::abstaining()),
            (QuestionAnswer::default(), ToolAnswer::default()),
            // The defect too: a head that accepts a frame the tool refuses (or the
            // reverse) is a person told their answer landed and then not answered.
            (
                QuestionAnswer {
                    abstain: true,
                    free: Some("both".into()),
                    ..Default::default()
                },
                ToolAnswer {
                    abstain: true,
                    free: Some("both".into()),
                    ..Default::default()
                },
            ),
        ] {
            assert_eq!(
                serde_json::to_string(&wire).unwrap(),
                serde_json::to_string(&tool).unwrap(),
                "the two copies serialise differently"
            );
            // And they must agree about what is valid, or a head accepts an answer
            // the tool then refuses.
            assert_eq!(
                wire.validate(2).is_ok(),
                tool.validate(2).is_ok(),
                "the two copies disagree about validity: {wire:?}"
            );
        }
    }
}
