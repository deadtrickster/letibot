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
//! **This is operator-backed policy, not a preference.** D10: *"we need user
//! questions claude code style - model provided options with user notes and then
//! opencode style free user reply input, not claude code 'chat later'"*, with the
//! unanswered case staying `not_run` and never becoming a default.
//!
//! # Six ways this refuses, and all six are `not_run`
//!
//! | | what happened | why not something else |
//! |---|---|---|
//! | [`AskError::NoHead`] | nothing is attached that could be asked | not `Abstained`: an abstention is a claim about the world, and nobody looked at the world |
//! | [`AskError::Unanswered`] | the question reached a head and nobody answered before the deadline | a deadline is not a decision, and *"chat later"* is not an answer |
//! | [`AskError::Nonconforming`] | an answer chose an option that was never offered | the same rule [`crate::adjudicate::AskAdjudicator`] applies: a non-conforming answerer never opens the gate |
//! | [`AskError::Empty`] | an answer arrived with no option, no note and no free text | there is nothing in it to attribute to anybody |
//! | [`AskError::NoteQualifiesNothing`] | a note arrived with no option chosen | reading it as free text would be the harness reinterpreting what a person said, which is the one thing this module must never do |
//! | [`AskError::Anonymous`] | an answer came back with nobody attached to it | an attribution to nobody is the failure this whole module exists to prevent |
//!
//! # The answer vocabulary (D10)
//!
//! Three things **together**, and the fields are separate because folding them
//! loses information:
//!
//! ```text
//!   QuestionAnswer { option: Option<usize>, note: Option<String>, free: Option<String> }
//! ```
//!
//! * `option` — a model-provided choice, so the common case is one keystroke.
//! * `note` — a qualification **on that choice**. Folding it into free text loses
//!   *which option* it qualifies, which is usually the load-bearing half of
//!   *"option B, but only for the CUDA box"*.
//! * `free` — a first-class answer on its own. A person who wants to type a
//!   sentence has answered the question; they have not deferred it.
//!
//! At least one of `option` or `free` must be present, and `note` needs an
//! `option` to qualify. [`QuestionAnswer::validate`] is where those hold, and it is
//! the only door into an `Ok`.
//!
//! # This vocabulary is deliberately NOT `OptionKind`
//!
//! `crates/sessionlog`'s `OptionKind` is `AllowOnce | AllowAlways | RejectOnce |
//! RejectAlways` and [`crate::adjudicate::OptionKind`] is its five-way cousin. Both
//! are **adjudication** vocabularies: they answer *may this run*. A question answers
//! *which way should I go*, and the consequences differ — an adjudication that goes
//! wrong runs something, an answer that goes wrong is attributed to a person.
//! Overloading one into the other would put a free-form sentence where a policy
//! engine reads a grant, so they stay two types.
//!
//! # What the wire has, and what it still needs
//!
//! `PROTOCOL_VERSION` is 5 and [`crate::builtins::intent::ask::QuestionAnswer`] is
//! what `ClientFrame::AnswerQuestion` carries head → daemon. What is **not** wired
//! is the daemon → head direction: posing a question rides on
//! `SessionEvent::DecisionRequested`'s existing `kind` field (§11.6 — *"a permission
//! and a question are one mechanism, differing in `kind`"*), but that event's
//! `options` are `DecisionOption`s carrying `OptionKind`, so a question's plain-text
//! choices have nowhere to sit. Adding a `SessionEvent` variant touches nine files,
//! two of which are under concurrent rewrite, so it is reported rather than done.
//! See the crate report for exactly what the head must grow. [`Headless`] is what
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

/// **What a person said** — D10's shape, and the type that goes on the wire.
///
/// Three independent fields, because the three things they carry are independent:
/// a choice, a qualification *on that choice*, and a sentence typed instead of
/// choosing. See the module docs for why none of them collapses into another.
///
/// `Serialize`/`Deserialize` because `letibot_sessionlog`'s
/// `ClientFrame::AnswerQuestion` carries exactly this, head → daemon.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QuestionAnswer {
    /// Index into the question's `options`. One keystroke, which is the point of
    /// offering options at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<usize>,
    /// A qualification on the chosen option. Requires `option`: a note qualifying
    /// nothing is not a note, and silently promoting it to free text would be the
    /// harness deciding what a person meant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// A typed answer, and **a first-class one**. Named explicitly against Claude
    /// Code's *"chat later"*, which turns a question into a suspended turn rather
    /// than an answered one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free: Option<String>,
}

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

    /// **The only door into an `Ok`.** Every rule D10 states is here, and each
    /// failure is a `not_run` rather than a repair.
    pub fn validate(&self, offered: usize) -> Result<(), AskError> {
        let has_free = self.free.as_deref().is_some_and(|f| !f.trim().is_empty());
        let has_note = self.note.as_deref().is_some_and(|n| !n.trim().is_empty());
        if self.option.is_none() && !has_free {
            if has_note {
                return Err(AskError::NoteQualifiesNothing);
            }
            return Err(AskError::Empty);
        }
        if has_note && self.option.is_none() {
            return Err(AskError::NoteQualifiesNothing);
        }
        if let Some(i) = self.option
            && i >= offered
        {
            return Err(AskError::Nonconforming { chose: i, offered });
        }
        Ok(())
    }

    /// How it reads back to the model, given the question it answers.
    pub fn render(&self, q: &Question) -> String {
        let mut s = String::new();
        if let Some(i) = self.option {
            let label = q.options.get(i).map(|s| s.as_str()).unwrap_or("(unknown)");
            s.push_str(&format!("chose option {i}: {label}\n"));
            if let Some(n) = &self.note {
                // On its own line and labelled, because a note that reads as part
                // of the option label is a note that changes what the option said.
                s.push_str(&format!("with the note: {}\n", n.trim()));
            }
        }
        if let Some(f) = &self.free {
            s.push_str(&format!("and said: {}\n", f.trim()));
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskError {
    /// Nothing is attached that could be asked.
    NoHead(String),
    /// It was posted and nobody answered.
    Unanswered(String),
    /// An answer chose an option index the question never offered.
    Nonconforming { chose: usize, offered: usize },
    /// An answer arrived with nothing in it.
    Empty,
    /// A note arrived with no option chosen, so it qualifies nothing.
    NoteQualifiesNothing,
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
            AskError::Nonconforming { chose, offered } => write!(
                f,
                "an answer chose option {chose} and only {offered} option(s) were offered, \
                 so it was not accepted and NOBODY has answered this question."
            ),
            AskError::Empty => write!(
                f,
                "an answer arrived with no option chosen, no note and no text, so there is \
                 nothing in it and NOBODY has answered this question. An empty answer is \
                 not a shrug the harness may interpret."
            ),
            AskError::NoteQualifiesNothing => write!(
                f,
                "a note arrived with no option chosen, so it qualifies nothing and was not \
                 accepted; NOBODY has answered this question. A note goes WITH a choice. \
                 Reading it as a free-form answer instead would be the harness deciding \
                 what a person meant, which it does not do."
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
    ///
    /// The `String` is **who answered**, read from the head that answered and never
    /// from the answer itself. A tuple rather than a field on
    /// [`QuestionAnswer`] on purpose: `QuestionAnswer` is what a *person types* and
    /// goes on the wire from their head; the identity is what the *daemon knows*
    /// about the connection it arrived on. An answer that could name its own
    /// answerer is an answer that could name somebody else.
    fn ask(&self, q: &Question) -> Result<(QuestionAnswer, String), AskError>;

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
    fn ask(&self, _q: &Question) -> Result<(QuestionAnswer, String), AskError> {
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
                    needs_in_view: Vec::new(),
                    media: None,
                }
            }
            Ok((a, by)) => accept(&q, a, by),
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

/// Turn an answer into a result, refusing every shape that would attribute
/// something to nobody or reinterpret what a person said.
fn accept(q: &Question, a: QuestionAnswer, by: String) -> Invocation {
    let refuse = |e: AskError| Invocation {
        outcome: e.outcome(),
        payload: format!("question: {}\n{e}", q.text),
        notes: vec![],
        edit: None,
        needs_in_view: Vec::new(),
        media: None,
    };
    if by.trim().is_empty() {
        return refuse(AskError::Anonymous);
    }
    if let Err(e) = a.validate(q.options.len()) {
        return refuse(e);
    }
    let mut body = format!("question: {}\n{by} ", q.text);
    body.push_str(a.render(q).trim_start());
    Invocation::ok(body).with_note(format!(
        "this answer is attributed to {by}; it is what they said, not what the harness \
         inferred. A note qualifies the option it came with — do not read it as a \
         separate instruction."
    ))
}

#[cfg(any(test, feature = "testing"))]
pub use scripted::ScriptedQuestioner;

#[cfg(any(test, feature = "testing"))]
mod scripted {
    use super::*;

    /// A questioner that answers from a script. Every case D10 names is a
    /// constructor, and none of them needs a head, a socket or a person.
    pub struct ScriptedQuestioner(pub Result<(QuestionAnswer, String), AskError>);

    impl ScriptedQuestioner {
        /// The opencode case: a typed sentence, and a real answer.
        pub fn free(by: &str, text: &str) -> Self {
            ScriptedQuestioner(Ok((QuestionAnswer::free(text), by.into())))
        }

        /// The Claude Code case: one keystroke.
        pub fn choosing(by: &str, option: usize) -> Self {
            ScriptedQuestioner(Ok((QuestionAnswer::choosing(option), by.into())))
        }

        /// D10's third thing: a choice **and** a qualification on it.
        pub fn choosing_with_note(by: &str, option: usize, note: &str) -> Self {
            ScriptedQuestioner(Ok((
                QuestionAnswer::choosing(option).with_note(note),
                by.into(),
            )))
        }

        pub fn anonymous() -> Self {
            ScriptedQuestioner(Ok((QuestionAnswer::free("go with b"), String::new())))
        }

        pub fn nonconforming(by: &str, option: usize) -> Self {
            ScriptedQuestioner(Ok((QuestionAnswer::choosing(option), by.into())))
        }

        pub fn empty(by: &str) -> Self {
            ScriptedQuestioner(Ok((QuestionAnswer::default(), by.into())))
        }

        pub fn note_only(by: &str, note: &str) -> Self {
            ScriptedQuestioner(Ok((
                QuestionAnswer {
                    note: Some(note.into()),
                    ..Default::default()
                },
                by.into(),
            )))
        }

        pub fn silent() -> Self {
            ScriptedQuestioner(Err(AskError::Unanswered(
                "the question was posted to the attached head and nobody answered before \
                 the deadline, so NOBODY has answered it. A deadline passing is not a \
                 decision, not a declining, and not a `chat later`: the question is still \
                 open."
                    .into(),
            )))
        }
    }

    impl Questioner for ScriptedQuestioner {
        fn ask(&self, _q: &Question) -> Result<(QuestionAnswer, String), AskError> {
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
        let r = ask(Arc::new(ScriptedQuestioner::anonymous()), Q);
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }));
        assert!(r.render().contains("no answerer"), "{}", r.render());
    }

    #[test]
    fn an_answer_choosing_an_option_nobody_offered_is_refused() {
        let r = ask(
            Arc::new(ScriptedQuestioner::nonconforming("deadtrickster", 7)),
            Q,
        );
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }));
        let out = r.render();
        assert!(out.contains("NOBODY has answered"), "{out}");
        assert!(out.contains("only 2 option"), "{out}");
    }

    // ---- D10: the three things, together -------------------------------

    #[test]
    fn d10_one_keystroke_is_an_answer() {
        let r = ask(
            Arc::new(ScriptedQuestioner::choosing("deadtrickster", 1)),
            Q,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok);
        let out = r.render();
        assert!(out.contains("chose option 1: b"), "{out}");
        assert!(out.contains("attributed to deadtrickster"), "{out}");
    }

    #[test]
    fn d10_a_note_stays_attached_to_the_option_it_qualifies() {
        // The load-bearing half of "option B, but only for the CUDA box" is WHICH
        // option it qualifies. Folding the note into free text loses that, which is
        // why they are two fields.
        let r = ask(
            Arc::new(ScriptedQuestioner::choosing_with_note(
                "deadtrickster",
                1,
                "only for the CUDA box",
            )),
            Q,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok);
        let out = r.render();
        assert!(out.contains("chose option 1: b"), "{out}");
        assert!(
            out.contains("with the note: only for the CUDA box"),
            "{out}"
        );
        assert!(out.contains("qualifies the option it came with"), "{out}");
    }

    #[test]
    fn d10_free_text_is_a_first_class_answer_and_not_a_chat_later() {
        let r = ask(
            Arc::new(ScriptedQuestioner::free(
                "deadtrickster",
                "neither — split it in two",
            )),
            Q,
        );
        assert_eq!(
            r.outcome,
            ToolOutcome::Ok,
            "a typed answer answers the question; it does not defer it"
        );
        assert!(r.render().contains("split it in two"), "{}", r.render());
    }

    #[test]
    fn d10_an_empty_answer_is_not_a_shrug_the_harness_may_read() {
        let r = ask(Arc::new(ScriptedQuestioner::empty("deadtrickster")), Q);
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }), "{r:?}");
        assert!(r.render().contains("not a shrug"), "{}", r.render());
    }

    #[test]
    fn d10_a_note_qualifying_nothing_is_not_promoted_to_free_text() {
        let r = ask(
            Arc::new(ScriptedQuestioner::note_only("deadtrickster", "hmm, maybe")),
            Q,
        );
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }), "{r:?}");
        assert!(
            r.render().contains("deciding what a person meant"),
            "{}",
            r.render()
        );
    }

    #[test]
    fn an_open_question_is_answered_by_free_text_alone() {
        let r = ask(
            Arc::new(ScriptedQuestioner::free(
                "deadtrickster",
                "use the second one",
            )),
            r#"{"question":"which approach?"}"#,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok);
    }

    #[test]
    fn the_answer_vocabulary_round_trips_on_the_wire() {
        // `ClientFrame::AnswerQuestion` carries exactly this, so a field that does
        // not survive serde is a field a head cannot send.
        let a = QuestionAnswer::choosing(1).with_note("only for the CUDA box");
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<QuestionAnswer>(&json).unwrap(), a);
        // Absent fields are absent, not null: a head that chose an option sends
        // three keys, not five.
        assert!(!json.contains("free"), "{json}");
        let free = QuestionAnswer::free("split it");
        let j2 = serde_json::to_string(&free).unwrap();
        assert!(!j2.contains("option") && !j2.contains("note"), "{j2}");
    }

    #[test]
    fn validation_is_the_only_door_and_it_is_not_a_repair() {
        // Two options offered.
        assert!(QuestionAnswer::choosing(0).validate(2).is_ok());
        assert!(QuestionAnswer::free("x").validate(2).is_ok());
        assert!(
            QuestionAnswer::choosing(1)
                .with_note("y")
                .validate(2)
                .is_ok()
        );
        assert_eq!(
            QuestionAnswer::choosing(2).validate(2),
            Err(AskError::Nonconforming {
                chose: 2,
                offered: 2
            })
        );
        assert_eq!(QuestionAnswer::default().validate(2), Err(AskError::Empty));
        assert_eq!(
            QuestionAnswer {
                note: Some("m".into()),
                ..Default::default()
            }
            .validate(2),
            Err(AskError::NoteQualifiesNothing)
        );
        // Every one of those is `not_run`. There is no repair path.
        for e in [
            AskError::Empty,
            AskError::NoteQualifiesNothing,
            AskError::Nonconforming {
                chose: 9,
                offered: 2,
            },
            AskError::Anonymous,
        ] {
            assert!(matches!(e.outcome(), ToolOutcome::NotRun { .. }), "{e:?}");
        }
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
