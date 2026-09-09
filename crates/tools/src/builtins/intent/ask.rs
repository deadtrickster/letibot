//! `ask_user_question` — the one tool whose result is a claim about what a
//! **person** said.
//!
//! Every other tool in this crate reports something a machine can be re-asked. If
//! `grep` is wrong you run it again. If this tool is wrong, the transcript now
//! contains a sentence attributed to the operator that the operator never said,
//! and every later turn reasons from it. So the bar is higher here than anywhere
//! else in the tool set, and it is a single rule:
//!
//! > **If nobody answered, nobody answered.** Not a default, not a best guess, not
//! > "continue using your judgement". [`ToolOutcome::NotRun`] — *nothing was
//! > decided* — with a sentence saying who was not reached.
//!
//! # The prior art this is a rejection of
//!
//! `docs/tool-survey.md` §3.6 records grok-build's version, which is the most
//! developed of the five and gets exactly this wrong:
//!
//! > a 30-minute timeout becomes a *successful* result reading *"User declined to
//! > answer the questions. Continue with the task using your best judgment"*
//! > (`grok_build/ask_user_question/format.rs:21`)
//!
//! Nobody declined. Nobody was there. That result is `Ok`, so it propagates as
//! grounding ([`crate::result::propagate`]), and a subagent whose only call was
//! this one may report success. opencode's is the same failure in fewer words:
//! an unanswered question becomes `"q" = "Unanswered"` inside a successful result
//! (`question.ts:31`).
//!
//! # Four ways this refuses, and all four are `not_run`
//!
//! | | what happened | why not something else |
//! |---|---|---|
//! | [`AskError::NoHead`] | nothing is attached that could be asked | not `Abstained`: an abstention is a claim about the world, and nobody looked at the world |
//! | [`AskError::Unanswered`] | the question reached a head and nobody answered before the deadline | a deadline is not a decision |
//! | [`AskError::Nonconforming`] | an answer came back naming no offered option | the same rule [`crate::adjudicate::AskAdjudicator`] applies: a non-conforming answerer never opens the gate |
//! | [`AskError::Anonymous`] | an answer came back with nobody attached to it | an attribution to nobody is the failure this whole module exists to prevent |
//!
//! # What is not built here
//!
//! **No wire.** `crates/sessionlog` already carries `DecisionRequested` /
//! `DecisionAnswered` and a `ClientFrame::Answer`, and §11.6 of the design brief
//! says a permission and a question are one mechanism differing in `kind` —
//! [`crate::adjudicate::RequestKind::Question`] exists for exactly this. What does
//! **not** exist is an option vocabulary that can carry a free-form answer:
//! `sessionlog`'s `OptionKind` is `AllowOnce | AllowAlways | RejectOnce |
//! RejectAlways`, which cannot express *"option C: use the second approach"*.
//! Extending the wire is a protocol change and the protocol is contested this
//! session, so this module stops at the trait and says so. [`Headless`] is what
//! ships, and it refuses.

use letibot_transcript::ToolOutcome;
use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// What is being asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub text: String,
    /// The answers offered, if any. An empty list is an open question.
    pub options: Vec<String>,
    /// Why the model is stuck, in one line. Shown to the person; never used to
    /// derive an answer.
    pub because: String,
}

/// What a person said, and **who**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    /// The head or seat identity that answered. Never empty in a valid answer —
    /// see [`AskError::Anonymous`].
    pub by: String,
    /// Which offered option, when options were offered.
    pub option: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskError {
    /// Nothing is attached that could be asked.
    NoHead(String),
    /// It was posted and nobody answered.
    Unanswered(String),
    /// An answer came back that names none of the offered options.
    Nonconforming { answer: String, options: Vec<String> },
    /// An answer came back with no answerer.
    Anonymous,
    /// The head was there and the ask itself broke.
    Transport(String),
}

impl AskError {
    /// **All of them are `NotRun`.** There is no branch of this function that
    /// produces `Ok`, `Abstained` or `Denied`, and that is the property the module
    /// exists for: a failed ask never becomes an answer, and it never becomes a
    /// claim about the world either.
    pub fn outcome(&self) -> ToolOutcome {
        ToolOutcome::NotRun {
            why: self.to_string(),
        }
    }
}

impl std::fmt::Display for AskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AskError::NoHead(w) => write!(f, "{w}"),
            AskError::Unanswered(w) => write!(f, "{w}"),
            AskError::Nonconforming { answer, options } => write!(
                f,
                "an answer came back naming none of the {} option(s) offered ({}), so it \
                 was not accepted and NOBODY has answered this question. The reply was: \
                 {answer:?}",
                options.len(),
                options.join(", ")
            ),
            AskError::Anonymous => write!(
                f,
                "an answer came back with no answerer attached, so there is no person to \
                 attribute it to and it was not accepted. NOBODY has answered this \
                 question."
            ),
            AskError::Transport(e) => write!(
                f,
                "the question could not be delivered ({e}), so NOBODY was asked and \
                 nobody answered."
            ),
        }
    }
}

/// The seam to whatever can reach a person: a TUI, the flowy connector, an ACP
/// client.
pub trait Questioner: Send + Sync {
    /// Post the question and wait for an answer, or say why there is none.
    ///
    /// Blocking is the implementation's business, and so is its deadline: this
    /// crate has no async runtime and a deadline that expired is
    /// [`AskError::Unanswered`], which is the same fact one layer in.
    fn ask(&self, q: &Question) -> Result<Answer, AskError>;

    /// For `EXPLAIN`: who could actually be asked in this session.
    fn describe(&self) -> String;
}

/// **The default, and it refuses.**
///
/// The name is the fact: there is no head. Every call is `NotRun` and the sentence
/// says who was not reached, so a model that gets this back knows it is stuck
/// rather than believing it was told to proceed.
#[derive(Debug, Default, Clone, Copy)]
pub struct Headless;

impl Questioner for Headless {
    fn ask(&self, _q: &Question) -> Result<Answer, AskError> {
        Err(AskError::NoHead(
            "no head is attached to this session, so NOBODY was asked and nobody \
             answered. This is not a refusal and it is not permission to proceed on an \
             assumption: the question is still open. State the assumption you would have \
             to make, and stop, or take the path that does not need the answer."
                .into(),
        ))
    }

    fn describe(&self) -> String {
        "none attached — every `ask_user_question` call refuses with not_run".into()
    }
}

/// The tool.
pub struct AskUserQuestion {
    pub questioner: std::sync::Arc<dyn Questioner>,
}

impl AskUserQuestion {
    pub fn new(questioner: std::sync::Arc<dyn Questioner>) -> Self {
        AskUserQuestion { questioner }
    }
}

/// The most options one question may offer before the tool trims and says so.
const MAX_OPTIONS: usize = 8;

impl Tool for AskUserQuestion {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "ask_user_question",
            "Ask the person driving this session a question, and wait for their answer. \
             Give `question`; optionally `options`, a short list of answers to choose \
             between, and `because`, one line saying what you are stuck on. It returns \
             an answer only when a person actually gave one, attributed to them. If \
             nobody is attached, or nobody answers, it returns no result and says which \
             — that is never a default answer and never permission to proceed on an \
             assumption, so do not treat it as one.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "The question, in words, answerable without reading the transcript."},
                    "options": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Answers to choose between. Omit for an open question. An answer naming none of these is not accepted."
                    },
                    "because": {"type": "string", "description": "One line: what you are stuck on and why the answer changes what you do."}
                },
                "required": ["question"]
            }),
            // Session: it changes nothing the operator owns and reads nothing off
            // disk; it interrupts a person. Declaring `Read` — which is what a
            // tool set that had no word for this would do — would be the
            // under-declaration `docs/tool-survey.md` §1.4 names in grok-build.
            Access::Session,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(text) = args.get("question").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "ask_user_question needs a question",
                "call `ask_user_question` again with `question` set to what you need to \
                 know, in words a person can answer without reading the transcript.",
            );
        };
        let mut options: Vec<String> = args
            .get("options")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let mut trimmed = None;
        if options.len() > MAX_OPTIONS {
            trimmed = Some(options.len());
            options.truncate(MAX_OPTIONS);
        }

        let q = Question {
            text: text.to_string(),
            options: options.clone(),
            because: args
                .get("because")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        };
        ctx.progress("waiting for a person to answer");

        let mut inv = match self.questioner.ask(&q) {
            Err(e) => {
                let mut body = format!("question: {}\n", q.text);
                if !q.options.is_empty() {
                    body.push_str(&format!("options offered: {}\n", q.options.join(" | ")));
                }
                body.push_str(&e.to_string());
                Invocation {
                    outcome: e.outcome(),
                    payload: body,
                    notes: vec![],
                    edit: None,
                }
            }
            Ok(a) => accept(&q, a),
        };
        if let Some(n) = trimmed {
            inv = inv.with_note(format!(
                "{n} options were offered and only the first {MAX_OPTIONS} were shown; a \
                 question with more choices than that is two questions"
            ));
        }
        inv
    }
}

/// Turn an answer into a result, refusing the two shapes that would attribute a
/// sentence to nobody.
fn accept(q: &Question, a: Answer) -> Invocation {
    if a.by.trim().is_empty() {
        let e = AskError::Anonymous;
        return Invocation {
            outcome: e.outcome(),
            payload: format!("question: {}\n{e}", q.text),
            notes: vec![],
            edit: None,
        };
    }
    if !q.options.is_empty() {
        let named = a
            .option
            .as_deref()
            .map(|o| q.options.iter().any(|x| x == o))
            .unwrap_or(false)
            || q.options.iter().any(|x| x == a.text.trim());
        if !named {
            let e = AskError::Nonconforming {
                answer: a.text.clone(),
                options: q.options.clone(),
            };
            return Invocation {
                outcome: e.outcome(),
                payload: format!("question: {}\n{e}", q.text),
                notes: vec![],
                edit: None,
            };
        }
    }
    let mut body = format!("question: {}\n", q.text);
    body.push_str(&format!("{} answered: {}\n", a.by, a.text.trim()));
    if let Some(o) = &a.option {
        body.push_str(&format!("option chosen: {o}\n"));
    }
    Invocation::ok(body).with_note(format!(
        "this answer is attributed to {}; it is what they said, not what the harness \
         inferred",
        a.by
    ))
}

#[cfg(any(test, feature = "testing"))]
pub use scripted::ScriptedQuestioner;

#[cfg(any(test, feature = "testing"))]
mod scripted {
    use super::*;

    /// A questioner that answers from a script. The five cases are the five
    /// branches, and none of them needs a head, a socket or a person.
    pub struct ScriptedQuestioner(pub Result<Answer, AskError>);

    impl ScriptedQuestioner {
        pub fn answering(by: &str, text: &str) -> Self {
            ScriptedQuestioner(Ok(Answer {
                text: text.into(),
                by: by.into(),
                option: None,
            }))
        }

        pub fn choosing(by: &str, option: &str) -> Self {
            ScriptedQuestioner(Ok(Answer {
                text: option.into(),
                by: by.into(),
                option: Some(option.into()),
            }))
        }

        pub fn anonymous(text: &str) -> Self {
            ScriptedQuestioner(Ok(Answer {
                text: text.into(),
                by: String::new(),
                option: None,
            }))
        }

        pub fn nonconforming(by: &str, text: &str) -> Self {
            ScriptedQuestioner(Ok(Answer {
                text: text.into(),
                by: by.into(),
                option: Some("something-nobody-offered".into()),
            }))
        }

        pub fn silent() -> Self {
            ScriptedQuestioner(Err(AskError::Unanswered(
                "the question was posted to the attached head and nobody answered before \
                 the deadline, so NOBODY has answered it. A deadline passing is not a \
                 decision and not a declining: the question is still open."
                    .into(),
            )))
        }
    }

    impl Questioner for ScriptedQuestioner {
        fn ask(&self, _q: &Question) -> Result<Answer, AskError> {
            self.0.clone()
        }

        fn describe(&self) -> String {
            "scripted".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{Envelope, Propagation, propagate};
    use std::sync::Arc;

    fn ask(q: Arc<dyn Questioner>, args: &str) -> crate::result::ToolResult {
        let mut h = crate::testing::harness();
        h.rt.registry
            .register(Box::new(AskUserQuestion::new(q)))
            .expect("registers");
        h.call("ask_user_question", args)
    }

    const Q: &str = r#"{"question":"which approach?","options":["a","b"]}"#;

    #[test]
    fn no_head_is_not_run_and_never_a_default() {
        let r = ask(Arc::new(Headless), Q);
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }), "{r:?}");
        let out = r.render();
        assert_eq!(Envelope::classify(&out), Some("TOOL_ERROR"));
        assert!(out.contains("NOBODY was asked"), "{out}");
        assert!(
            !out.to_lowercase().contains("best judg"),
            "the survey's failure, reproduced: {out}"
        );
        assert!(!r.is_grounded());
    }

    #[test]
    fn nobody_answering_is_not_a_declining() {
        let r = ask(Arc::new(ScriptedQuestioner::silent()), Q);
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }));
        assert!(r.render().contains("still open"), "{}", r.render());
    }

    #[test]
    fn an_answer_with_nobody_behind_it_is_refused() {
        let r = ask(Arc::new(ScriptedQuestioner::anonymous("a")), Q);
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }));
        assert!(r.render().contains("no answerer"), "{}", r.render());
    }

    #[test]
    fn an_answer_naming_no_offered_option_is_refused() {
        let r = ask(
            Arc::new(ScriptedQuestioner::nonconforming("deadtrickster", "c")),
            Q,
        );
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }));
        assert!(r.render().contains("NOBODY has answered"), "{}", r.render());
    }

    #[test]
    fn a_real_answer_is_ok_and_carries_who_said_it() {
        let r = ask(Arc::new(ScriptedQuestioner::choosing("deadtrickster", "b")), Q);
        assert_eq!(r.outcome, ToolOutcome::Ok);
        let out = r.render();
        assert!(out.contains("deadtrickster answered"), "{out}");
        assert!(out.contains("attributed to deadtrickster"), "{out}");
    }

    #[test]
    fn an_open_question_does_not_need_an_option() {
        let r = ask(
            Arc::new(ScriptedQuestioner::answering("deadtrickster", "use the second one")),
            r#"{"question":"which approach?"}"#,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok);
    }

    #[test]
    fn a_caller_whose_only_call_was_an_unanswered_ask_cannot_report_ok() {
        // §8.2's propagation, which is the reason the outcome class matters: this
        // is what stops a subagent turning "nobody answered" into a result.
        let r = ask(Arc::new(Headless), Q);
        assert!(matches!(
            propagate(&[r.outcome]),
            Propagation::Must(ToolOutcome::Failed { .. })
        ));
    }

    #[test]
    fn the_description_says_how_to_use_it_and_nothing_about_the_operator() {
        let s = AskUserQuestion::new(Arc::new(Headless)).schema();
        assert_eq!(crate::schema::lint_description(&s.description), vec![]);
    }
}
