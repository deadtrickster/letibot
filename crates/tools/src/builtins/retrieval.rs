//! `ask_code` and `ask_corpus`: retrieval as callable tools.
//!
//! The harness does no retrieval of its own; it calls these as tools and takes their
//! abstention as a verdict. Two rules the seam exists to enforce:
//!
//! 1. A backend that does not cover the question abstains. `covered: false`, an empty
//!    answer or an answer with no citations becomes [`ToolOutcome::Abstained`] and then
//!    the `NO_RESULT` envelope, never an `Ok` with a hedged sentence in it: a
//!    hallucination wearing a footnote is more dangerous than a naked one.
//! 2. A rewritten query is visible. If a backend searched for something other than what
//!    was asked, [`RetrievalAnswer::rewritten_query`] carries it and the tool prints it.
//!    The harness itself rewrites nothing.
//!
//! There is no MCP client here: an untested transport reports success while delivering
//! nothing. [`Unavailable`] is what answers when nothing is attached, and it returns
//! `NotRun`, which is a different fact from *the corpus does not cover it*.

use serde_json::Value;

use crate::attach::NotAttached;
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// Which of the operator's retrieval tools is being asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalKind {
    /// `ask_code` — a question about a code base.
    Code,
    /// `ask_corpus` — a question about a document corpus.
    Corpus,
}

impl RetrievalKind {
    pub fn tool_name(&self) -> &'static str {
        match self {
            RetrievalKind::Code => "ask_code",
            RetrievalKind::Corpus => "ask_corpus",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalQuery {
    pub question: String,
    /// A path, a repository, a collection: whatever the backend scopes by. Passed
    /// through untouched.
    pub scope: Option<String>,
}

/// What a retrieval backend answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalAnswer {
    pub text: String,
    /// Where the answer came from. An answer with no citations is not grounded,
    /// and this tool will not report it as though it were.
    pub citations: Vec<String>,
    /// The backend's own honesty bit: `false` means the index does not cover the
    /// question. It is not a confidence score and it is not negotiable here.
    pub covered: bool,
    /// Set when the backend searched for something other than what was asked.
    pub rewritten_query: Option<String>,
}

#[derive(Debug)]
pub enum RetrievalError {
    /// Nothing is attached. Distinguished from "no coverage" on purpose, and carried
    /// in [`NotAttached`] rather than as a sentence of its own, so every tool with
    /// absent infrastructure says the same thing. See [`crate::attach`].
    NotAttached(NotAttached),
    Transport(String),
}

impl std::fmt::Display for RetrievalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RetrievalError::NotAttached(w) => write!(f, "{w}"),
            RetrievalError::Transport(e) => write!(f, "retrieval transport: {e}"),
        }
    }
}

/// The seam to the operator's `oracle` MCP tools.
pub trait Retrieval: Send + Sync {
    fn ask(
        &self,
        kind: RetrievalKind,
        query: &RetrievalQuery,
    ) -> Result<RetrievalAnswer, RetrievalError>;

    /// For `EXPLAIN`: what is actually behind these tools in this session.
    fn describe(&self) -> String;
}

/// Nothing attached. Every call is `NotRun`, and says so in one sentence the model
/// can act on.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unavailable;

impl Retrieval for Unavailable {
    fn ask(
        &self,
        kind: RetrievalKind,
        _query: &RetrievalQuery,
    ) -> Result<RetrievalAnswer, RetrievalError> {
        Err(RetrievalError::NotAttached(
            NotAttached::new(
                kind.tool_name(),
                "no retrieval backend",
                "no corpus was queried and no index was opened",
                "attach a retrieval backend to the session; none is running on this \
                 fleet, which was checked rather than assumed (T16.6)",
            )
            .instead("`grep` and `read` over this tree"),
        ))
    }

    fn describe(&self) -> String {
        "none attached".into()
    }
}

/// One of the two tools, over whatever backend the session was given.
pub struct Ask {
    pub kind: RetrievalKind,
    pub backend: std::sync::Arc<dyn Retrieval>,
}

impl Ask {
    pub fn code(backend: std::sync::Arc<dyn Retrieval>) -> Self {
        Ask {
            kind: RetrievalKind::Code,
            backend,
        }
    }

    pub fn corpus(backend: std::sync::Arc<dyn Retrieval>) -> Self {
        Ask {
            kind: RetrievalKind::Corpus,
            backend,
        }
    }
}

impl Tool for Ask {
    fn schema(&self) -> ToolSchema {
        // How to use it, and what comes back. Nothing about what is indexed: that
        // sentence goes stale, and this description is linted at registration.
        let description = match self.kind {
            RetrievalKind::Code => {
                "Ask a natural-language question about a code base and get an answer with \
                 citations. Give `question`; optionally `scope` to restrict where it \
                 looks. It answers only when it has material for the question; when it \
                 has none it says so and returns no answer, which is a verdict to act on \
                 rather than to work around."
            }
            RetrievalKind::Corpus => {
                "Ask a natural-language question of a document collection and get an \
                 answer with citations. Give `question`; optionally `scope`. It answers \
                 only when it has material for the question; when it has none it says so \
                 and returns no answer, which is a verdict to act on rather than to work \
                 around."
            }
        };
        ToolSchema::new(
            self.kind.tool_name(),
            description,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "The question, in words."},
                    "scope": {"type": "string", "description": "Optional restriction on where to look."}
                },
                "required": ["question"]
            }),
            // Read: it reads an index. If a backend is reached over the network,
            // that is the backend's declaration to make, not this tool's.
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(question) = args.get("question").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                format!("{} needs a question", self.kind.tool_name()),
                format!(
                    "call `{}` again with `question` set to what you want to know, in \
                     words.",
                    self.kind.tool_name()
                ),
            );
        };
        let query = RetrievalQuery {
            question: question.to_string(),
            scope: args
                .get("scope")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        };
        ctx.progress(format!("asking {}", self.kind.tool_name()));

        match self.backend.ask(self.kind, &query) {
            // Nothing ran. Not an abstention, because nothing formed an opinion,
            // and not a failure of the corpus. One shape for that, shared with
            // every other tool whose infrastructure is absent.
            Err(RetrievalError::NotAttached(na)) => na.invocation(),
            Err(e @ RetrievalError::Transport(_)) => Invocation::failed(
                e.to_string(),
                "the retrieval backend could not be reached. `grep` and `read` still \
                 work on the tree."
                    .to_string(),
            ),
            Ok(a) => answer(self.kind, &query, a),
        }
    }
}

fn answer(kind: RetrievalKind, query: &RetrievalQuery, a: RetrievalAnswer) -> Invocation {
    let mut notes = Vec::new();
    if let Some(rewritten) = &a.rewritten_query {
        // A rewrite must be visible, or the answer is judged against a question
        // nobody asked.
        notes.push(format!(
            "the backend searched for `{rewritten}` rather than `{}`; judge the answer \
             against what you asked, not against what it searched",
            query.question
        ));
    }

    if !a.covered || a.text.trim().is_empty() {
        let mut body = format!("question: {}\n", query.question);
        if let Some(s) = &query.scope {
            body.push_str(&format!("scope: {s}\n"));
        }
        if !a.text.trim().is_empty() {
            body.push_str(&format!("what came back instead: {}\n", a.text.trim()));
        }
        body.push_str(
            "the index does not cover this. Do not rephrase and re-ask hoping for a \
             different verdict — search the tree with `grep`, or ask the operator.",
        );
        let mut inv = Invocation::abstained(
            format!("{} reports no coverage for this question", kind.tool_name()),
            body,
        );
        inv.notes = notes;
        return inv;
    }

    if a.citations.is_empty() {
        // An answer with nothing behind it is the footnote-less half of the same
        // failure. It is not reported as grounded.
        let mut inv = Invocation::abstained(
            format!("{} answered with no citations", kind.tool_name()),
            format!(
                "the backend produced text but cited nothing, so there is no source to \
                 check it against:\n\n{}",
                a.text.trim()
            ),
        );
        inv.notes = notes;
        return inv;
    }

    let mut body = a.text.trim().to_string();
    body.push_str("\n\nsources:\n");
    for c in &a.citations {
        body.push_str(&format!("  - {c}\n"));
    }
    let mut inv = Invocation::ok(body);
    inv.notes = notes;
    inv
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{Envelope, Propagation, propagate};
    use crate::testing::{Scripted, harness_with_retrieval};
    use letibot_transcript::ToolOutcome;

    #[test]
    fn no_coverage_is_an_abstention_in_the_no_result_envelope() {
        let mut h = harness_with_retrieval(Scripted::no_coverage());
        let r = h.call("ask_corpus", r#"{"question":"does it cover bats?"}"#);
        assert!(matches!(r.outcome, ToolOutcome::Abstained { .. }));
        let rendered = r.render();
        assert_eq!(Envelope::classify(&rendered), Some("NO_RESULT"));
        assert!(rendered.contains("may be cited"), "{rendered}");
    }

    #[test]
    fn an_answer_with_no_citations_does_not_count_as_grounded() {
        let mut h = harness_with_retrieval(Scripted::uncited());
        let r = h.call("ask_code", r#"{"question":"where is the ledger?"}"#);
        assert!(!r.is_grounded(), "{:?}", r.outcome);
    }

    #[test]
    fn a_rewritten_query_is_visible_in_the_result() {
        let mut h = harness_with_retrieval(Scripted::rewriting());
        let r = h.call("ask_corpus", r#"{"question":"how does spill work?"}"#);
        let notes = r.notes.join(" ");
        assert!(notes.contains("searched for"), "{notes}");
    }

    #[test]
    fn with_nothing_attached_the_call_is_not_run_rather_than_empty() {
        let mut h = harness_with_retrieval(std::sync::Arc::new(Unavailable));
        let r = h.call("ask_code", r#"{"question":"anything"}"#);
        assert!(matches!(r.outcome, ToolOutcome::NotRun { .. }));
        // And a caller with only this call may not report Ok.
        assert!(matches!(
            propagate(std::slice::from_ref(&r.outcome)),
            Propagation::Must(_)
        ));
    }
}
