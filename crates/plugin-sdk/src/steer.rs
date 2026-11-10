//! The `steer_` hook: the turn's end - continue it with a sentence, or stop there. The harness's
//! own detectors are the input and the fallback, and two rules stay the host's, both checked before
//! the answer is used: a continuation the budget has not got, and an interrupted turn.

/// The export-name prefix: `steer_<name>`, because a kind with no name after its prefix names no hook.
pub const PREFIX: &str = "steer_";

/// The one export every host asks. The turn has one end, so a stack answers it once, top layer first.
pub const HOOK: &str = "steer_turn";

/// The hook a layer named `name` answers.
pub fn hook(name: &str) -> String {
    format!("steer_{name}")
}

/// Why a turn is at its end, which is what a project's own continue rule reads: an ordinary turn, or
/// one that began on a compaction's record (continue after compaction, off unless a project turns it
/// on).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum After {
    /// The turn began on the conversation it is in.
    #[default]
    Turn,
    /// The conversation above this turn is a compaction's record.
    Compaction,
}

/// What the deliverable check sees: the branch's work that is not committed. `None` for a session
/// with no branch of its own, which is every session the daemon did not start as a child.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Deliverable {
    /// The paths `git status --porcelain` named, so a layer can say which file it means.
    pub uncommitted: Vec<String>,
}

/// The context claim as the harness measured it: what the model said about the context, and the two
/// numbers it was judged against.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Claim {
    /// The model's sentence, which is the round's reply as the claim check read it.
    pub line: String,
    /// The tokens the conversation holds, as the harness measured them.
    pub resident: u64,
    /// The window they are held in.
    pub window: u64,
}

/// How many more turns this turn may be continued with, as the host counts them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Budget {
    /// The host's `nudges_left`: one per turn, spent by whichever continuation is taken.
    pub continues_left: usize,
}

/// Everything a layer is handed at the turn's end.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// [`V`](crate::V).
    pub v: u32,
    /// The reply that ended the turn, from its tail: enough to read the promises in it, and not the
    /// whole answer because a layer's context is the session's own.
    #[serde(default)]
    pub reply_tail: String,
    /// The rows that are still open, as the model reads them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_rows: Vec<String>,
    /// The harness's own decision, as the sentence it would have appended to carry this turn on -
    /// or `None` when it was going to end the turn. A layer that agrees with the harness answers
    /// this back, which is what a built-in layer that changes nothing is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
    /// The intent detector's finding on its own: a promise this turn made and did not start, in the
    /// harness's words, as the detector words it. `None` when it made none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// What the deliverable check sees, or `None` for a session that has no branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliverable: Option<Deliverable>,
    /// The round's context claim, when it made one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<Claim>,
    /// Consecutive rounds of this turn that returned nothing new - the progress detector's number.
    #[serde(default)]
    pub stall_rounds: usize,
    /// Why the turn is at its end.
    #[serde(default)]
    pub after: After,
    /// What the host will allow.
    #[serde(default)]
    pub budget: Budget,
}

impl Input {
    /// The input `plugin test` and the shadow gate use when a session has none recorded: a turn that
    /// ended with a promise and one continuation left, which is the shape this hook exists for.
    pub fn synthetic() -> Input {
        Input {
            v: crate::V,
            reply_tail: "Next I will add the sixth test and run the package's suite.".to_string(),
            open_rows: vec!["write the last test".to_string()],
            native: Some("the turn said it would run the suite and made no tool call".to_string()),
            intent: Some("the turn said it would run the suite and made no tool call".to_string()),
            deliverable: Some(Deliverable {
                uncommitted: vec!["crates/plugin-sdk/src/steer.rs".to_string()],
            }),
            claim: None,
            stall_rounds: 0,
            after: After::Turn,
            budget: Budget { continues_left: 1 },
        }
    }
}

/// What the host will allow, handed beside the input because it is the host's and not the layer's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Here {
    /// [`Budget::continues_left`], as the host stands when the answer is read.
    pub continues_left: usize,
    /// Whether the operator's interrupt ended this turn.
    pub interrupted: bool,
}

/// What a layer answered: a sentence to carry the turn on with, or a stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Continue the turn, saying this - the harness appends it and takes another round.
    Continue(String),
    /// End the turn here, even where a detector would have carried it on.
    Stop,
}

/// What the turn-end decision came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Go on, with the sentence the model reads.
    Continue(String),
    /// End the turn.
    Stop,
}

impl Answer {
    /// The answer a layer wrote, or the sentence saying why it is not one. The wire is
    /// `{"continue": "…"}` or `{"stop": true}` and exactly one of them, so an answer that says both
    /// is refused rather than read as whichever the parser met first.
    pub fn parse(v: serde_json::Value) -> Result<Answer, String> {
        let object = v.as_object().ok_or_else(|| {
            format!("an answer is `{{\"continue\": …}}` or `{{\"stop\": true}}`, not {v}")
        })?;
        let go_on = object.get("continue").and_then(serde_json::Value::as_str);
        let stop = object.get("stop").and_then(serde_json::Value::as_bool);
        match (go_on, stop, object.len()) {
            (Some(text), None, 1) if !text.trim().is_empty() => {
                Ok(Answer::Continue(text.to_string()))
            }
            (None, Some(true), 1) => Ok(Answer::Stop),
            (Some(_), Some(_), _) => {
                Err("it answered both `continue` and `stop`, and a turn ends one way".to_string())
            }
            (Some(_), None, _) => Err(format!(
                "it answered `continue` beside fields this hook does not have, so what the turn \
                 would do is not what it says: {v}"
            )),
            (_, Some(other), _) if !other => Err(format!(
                "it answered `stop: {other}`, and a stop is `true`: a stop that is not a stop is \
                 the turn going on with nothing said"
            )),
            _ => Err(format!(
                "it answered neither `continue` nor `stop`, so the turn ends as the harness \
                 decided: {v}"
            )),
        }
    }

    /// What this answer may do here. The host's two rules are not the layer's to keep, and both
    /// refusals are sentences that say which one it was.
    pub fn decide(&self, here: &Here) -> Result<Decision, String> {
        match self {
            Answer::Stop => Ok(Decision::Stop),
            Answer::Continue(_) if here.interrupted => Err(
                "it asked to continue a turn the operator interrupted, and an interrupt is the \
                 turn being ENDED: a layer cannot overrule the stop"
                    .to_string(),
            ),
            Answer::Continue(_) if here.continues_left == 0 => Err(format!(
                "it asked to continue a turn with no continuation left (the host allows {}, and \
                 this turn spent it), so continuing would be one more round than the budget has",
                here.continues_left
            )),
            Answer::Continue(text) => Ok(Decision::Continue(text.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn here(continues_left: usize, interrupted: bool) -> Here {
        Here {
            continues_left,
            interrupted,
        }
    }

    /// The budget is the host's, and a layer that spends what it has not got is refused.
    #[test]
    fn a_continue_past_the_budget_is_refused() {
        let answer = Answer::Continue("go on".into());
        let why = answer
            .decide(&here(0, false))
            .expect_err("no continuation left");
        assert!(why.contains("no continuation left"), "{why}");
        assert_eq!(
            answer.decide(&here(1, false)).expect("one left"),
            Decision::Continue("go on".into())
        );
    }

    /// An interrupted turn is the operator's stop, not a turn waiting for another round - so the
    /// budget being left is not the question.
    #[test]
    fn an_interrupted_turn_is_never_continued() {
        let answer = Answer::Continue("go on".into());
        let why = answer
            .decide(&here(3, true))
            .expect_err("an interrupt ends the turn");
        assert!(why.contains("interrupted"), "{why}");
        // A stop is what an interrupt already is, and saying so is not refused.
        assert_eq!(
            Answer::Stop.decide(&here(0, true)).expect("a stop"),
            Decision::Stop
        );
    }

    /// The wire is one of two objects, and an answer that is neither is refused with the shape.
    #[test]
    fn an_answer_is_a_sentence_or_a_stop_and_nothing_else() {
        assert_eq!(
            Answer::parse(json!({"continue": "one more round"})).expect("a continue"),
            Answer::Continue("one more round".into())
        );
        assert_eq!(
            Answer::parse(json!({"stop": true})).expect("a stop"),
            Answer::Stop
        );
        for bad in [
            json!({}),
            json!({"continue": "  "}),
            json!({"continue": "a", "stop": true}),
            json!({"stop": false}),
            json!({"continue": "a", "why": "because"}),
            json!("go on"),
        ] {
            assert!(
                Answer::parse(bad.clone()).is_err(),
                "an answer that is not the shape is refused: {bad}"
            );
        }
    }

    /// The synthetic input is the host's own shape, and the hook name is one a host finds.
    #[test]
    fn the_synthetic_input_is_the_hosts_own_shape() {
        let input = serde_json::to_value(Input::synthetic()).expect("serialises");
        for field in ["v", "reply_tail", "native", "after", "budget"] {
            assert!(input.get(field).is_some(), "no `{field}`: {input}");
        }
        assert_eq!(input["v"], json!(crate::V));
        assert_eq!(input["after"], json!("turn"));
        assert_eq!(input["budget"]["continues_left"], json!(1));
        assert_eq!(hook("turn"), HOOK);
    }
}
