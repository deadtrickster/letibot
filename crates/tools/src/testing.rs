//! A fixture tree, a runtime over it, and a scripted retrieval backend.
//!
//! `pub` behind the `testing` feature because the clause tests in `tests/` need the
//! same tree the unit tests use: an acceptance test for "a miss is self-correcting"
//! is only meaningful against a tree whose misses are known, and having two of
//! those trees is how they drift.

use std::sync::Arc;

use letibot_transcript::ToolCall;

use crate::backend::{HostBackend, tempdir::TempDir};
use crate::builtins::retrieval::{
    Retrieval, RetrievalAnswer, RetrievalError, RetrievalKind, RetrievalQuery, Unavailable,
};
use crate::events::RecordingToolSink;
use crate::result::ToolResult;
use crate::runtime::{Registry, ToolRuntime};
use crate::spill::Spiller;

/// The tree every test in this crate reasons about.
///
/// The details are chosen for the misses, which is the interesting half:
///
/// - `src/lib.rs` has `pub fn parse_args()`, so `^\s*fn parse_args\(` — a plausible
///   guess — matches **nothing**, and its bare identifier matches.
/// - `cache_prompt` occurs only under `docs/`, so a search scoped to `src/` misses
///   and has somewhere real to point.
/// - `src/util/` exists and `src/parser/` does not, so a glob for the second has a
///   surrounding listing worth printing.
/// - `big.txt` is large enough to spill under a small budget.
pub fn fixture_tree(root: &std::path::Path) {
    let w = |rel: &str, body: &str| {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("fixture dir");
        }
        std::fs::write(p, body).expect("fixture file");
    };
    w("README.md", "letibot\na harness\n");
    w("Cargo.toml", "[package]\nname = \"fixture\"\n");
    w(
        "src/lib.rs",
        "use std::io;\n\
         pub fn parse_args(argv: &[String]) -> u8 {\n\
         \x20   let ledger = TokenLedger::new();\n\
         \x20   0\n\
         }\n",
    );
    w(
        "src/util/helper.rs",
        "pub fn help() -> &'static str { \"h\" }\n",
    );
    w(
        "docs/notes.md",
        "The server takes cache_prompt on every request.\n\
         It also takes return_tokens, which is how the ledger stays authoritative.\n",
    );
    w("tests/basic.rs", "#[test]\nfn works() {}\n");
    let big: String = (0..2_000).map(|i| format!("filler line {i}\n")).collect();
    w("big.txt", &big);
}

/// A runtime over the fixture tree, with the read-only built-ins registered.
pub struct Harness {
    pub rt: ToolRuntime,
    pub sink: RecordingToolSink,
    /// Held so the tree outlives the backend.
    _dir: TempDir,
}

impl Harness {
    /// One call, as the engine would make it.
    pub fn call(&mut self, name: &str, arguments: &str) -> ToolResult {
        let call = ToolCall {
            id: format!("call_{}", self.sink.events.len()),
            name: name.to_string(),
            arguments: arguments.to_string(),
        };
        self.rt.invoke("turn_1", &call, &mut self.sink)
    }
}

pub fn harness() -> Harness {
    build(Spiller::unset(), Arc::new(Unavailable))
}

pub fn harness_with_spiller(spiller: Spiller) -> Harness {
    build(spiller, Arc::new(Unavailable))
}

pub fn harness_with_retrieval(retrieval: Arc<dyn Retrieval>) -> Harness {
    build(Spiller::unset(), retrieval)
}

fn build(spiller: Spiller, retrieval: Arc<dyn Retrieval>) -> Harness {
    let dir = TempDir::new();
    fixture_tree(dir.path());
    let backend = HostBackend::new(dir.path()).expect("fixture root");
    let registry: Registry = crate::read_only_tools(retrieval).expect("built-ins register");
    Harness {
        rt: ToolRuntime::new(registry, Box::new(backend)).with_spiller(spiller),
        sink: RecordingToolSink::new(),
        _dir: dir,
    }
}

/// A retrieval backend that answers from a script.
///
/// The four scripts are the four cases §8.2 distinguishes, and they are the reason
/// the seam is a trait: none of them needs a corpus, an embedder or a network.
pub struct Scripted {
    answer: Result<RetrievalAnswer, &'static str>,
}

impl Scripted {
    pub fn answering() -> Arc<dyn Retrieval> {
        Arc::new(Scripted {
            answer: Ok(RetrievalAnswer {
                text: "The ledger is append-only.".into(),
                citations: vec!["docs/notes.md:2".into()],
                covered: true,
                rewritten_query: None,
            }),
        })
    }

    /// The measured failure's first half: the corpus honestly says it does not
    /// cover the question.
    pub fn no_coverage() -> Arc<dyn Retrieval> {
        Arc::new(Scripted {
            answer: Ok(RetrievalAnswer {
                text: String::new(),
                citations: vec![],
                covered: false,
                rewritten_query: None,
            }),
        })
    }

    /// Text with nothing behind it — the footnote-less hallucination.
    pub fn uncited() -> Arc<dyn Retrieval> {
        Arc::new(Scripted {
            answer: Ok(RetrievalAnswer {
                text: "Probably in the ledger module.".into(),
                citations: vec![],
                covered: true,
                rewritten_query: None,
            }),
        })
    }

    /// A backend that searched for something else. §9.4's case.
    pub fn rewriting() -> Arc<dyn Retrieval> {
        Arc::new(Scripted {
            answer: Ok(RetrievalAnswer {
                text: "Spilling writes the full output to a store.".into(),
                citations: vec!["docs/notes.md:1".into()],
                covered: true,
                rewritten_query: Some("output retention policy".into()),
            }),
        })
    }

    pub fn broken() -> Arc<dyn Retrieval> {
        Arc::new(Scripted {
            answer: Err("connection refused"),
        })
    }
}

impl Retrieval for Scripted {
    fn ask(
        &self,
        _kind: RetrievalKind,
        _query: &RetrievalQuery,
    ) -> Result<RetrievalAnswer, RetrievalError> {
        match &self.answer {
            Ok(a) => Ok(a.clone()),
            Err(e) => Err(RetrievalError::Transport((*e).to_string())),
        }
    }

    fn describe(&self) -> String {
        "scripted".into()
    }
}
