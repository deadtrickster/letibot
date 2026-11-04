//! The answer vocabulary a head sends back when a person was asked: `option`, `note` (needs
//! an `option`), `free` and `abstain` (stands alone). A bad answer is a refusal, never a
//! repair. `letibot-tools` has the same shape, as an optional dependency here.

use serde::{Deserialize, Serialize};

/// What a person said. Not who - see [`crate::hub::Reply`]: the answerer is what
/// the daemon knows about the connection the answer arrived on, never a field the
/// answer carries.
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
    /// A deliberate no-answer, and not permission to proceed: a model that gets one
    /// must state the assumption it would make and stop. `default` so an older head's
    /// frame parses, and elided when `false` like the three fields above it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub abstain: bool,
}

/// `skip_serializing_if` for [`QuestionAnswer::abstain`]. A named function rather
/// than `std::ops::Not::not` because the reader of the attribute should not have to
/// work out which `Not` impl serde picked.
fn is_false(b: &bool) -> bool {
    !*b
}

/// Why an answer was not accepted. Every one of these means nobody has answered,
/// and none is something the harness may interpret: a malformed answer leaves the
/// question open, and the head is told so it can ask the person again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerDefect {
    /// No option, no note, no text.
    Empty,
    /// A note with no option chosen, so it qualifies nothing.
    NoteQualifiesNothing,
    /// An option index the question never offered.
    OptionNotOffered { chose: usize, offered: usize },
    /// An abstention that also carried an answer - a choice, a note, or words.
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

    /// A deliberate no-answer. The only way to spell it, and it stands alone -     /// see this module's header for why an empty [`Self::free`] is not it.
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
        // An abstention is checked first, and it has to be alone. A frame that says
        // "I am not answering" and also carries a choice has two claims in it, and a
        // validator that let either win would be the harness deciding what a person meant.
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
        // And the fourth, which is an answer too. It validates against a
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

    /// The three things that must not look alike, in the bytes a head sends: an
    /// abstention is `{"abstain":true}`, an empty answer is `{}` (refused), and silence
    /// is no frame at all.
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
        assert!(
            !serde_json::from_str::<QuestionAnswer>(&chose)
                .unwrap()
                .abstain,
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
