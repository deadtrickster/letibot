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
    /// The process host, for a session built by [`runner_harness`]. Tests reach it
    /// to declare a protected pid, to end a scope, and to read the reap log —
    /// none of which is a tool call, and all of which a daemon does.
    pub processes: Option<Arc<crate::exec::HostProcesses>>,
    /// What mounting the MCP catalog did, if there was one. Empty otherwise, and
    /// an empty report is a real answer: nothing was offered and nothing was
    /// refused.
    pub mount: crate::builtins::external::mcp::MountReport,
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

    /// The tree's root. Tests for the write tools have to look at the disk
    /// **behind** the backend: a test that asked the tool whether it wrote is a
    /// test of the tool's opinion of itself.
    pub fn root(&self) -> &std::path::Path {
        self._dir.path()
    }

    /// What is actually on disk, read without going through the backend.
    pub fn read_file(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root().join(rel)).unwrap_or_default()
    }

    /// Write to the tree from outside the harness — somebody else's edit.
    pub fn write_file(&self, rel: &str, body: &str) {
        let p = self.root().join(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).expect("fixture dir");
        }
        std::fs::write(p, body).expect("fixture write");
    }

    /// The modification time, for asserting that a no-op write really was one.
    pub fn mtime(&self, rel: &str) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.root().join(rel))
            .ok()
            .and_then(|m| m.modified().ok())
    }
}

pub fn harness() -> Harness {
    build(Spiller::unset(), Arc::new(Unavailable), false, None)
}

pub fn harness_with_spiller(spiller: Spiller) -> Harness {
    build(spiller, Arc::new(Unavailable), false, None)
}

pub fn harness_with_retrieval(retrieval: Arc<dyn Retrieval>) -> Harness {
    build(Spiller::unset(), retrieval, false, None)
}

/// A session that can change the tree: the coder tool set, a writable backend,
/// and an adjudicator that allows.
///
/// The adjudicator is explicit rather than absent, because a harness whose gate
/// happened to admit would make every write test also a test that the gate is
/// broken.
pub fn writable_harness() -> Harness {
    writable_harness_with_gate(Some(allow_all()))
}

/// A writable session with a chosen gate. `None` attaches **no adjudicator**,
/// which is the fail-closed default a real daemon starts with.
pub fn writable_harness_with_gate(gate: Option<Box<dyn crate::runtime::Gate>>) -> Harness {
    build(Spiller::unset(), Arc::new(Unavailable), true, gate)
}

/// A writable session whose output is bounded. Clause 5 does not stop applying
/// because a tool can write.
pub fn writable_harness_with_budget(spiller: Spiller) -> Harness {
    build(spiller, Arc::new(Unavailable), true, Some(allow_all()))
}

/// An adjudicator that says yes to everything, for tests about tools rather than
/// about the gate.
///
/// Deliberately **not** exported from the crate root and deliberately not
/// something a daemon can construct by accident: it lives behind the `testing`
/// feature, and the fail-closed default is what ships.
pub fn allow_all() -> Box<dyn crate::runtime::Gate> {
    use crate::adjudicate::{AdjudicatedGate, AdjudicationDecision, AskAdjudicator};
    // The shell trust is declared here, and the declaration is the point.
    // [`crate::intent::ShellTrust`] defaults to `Unknown`, under which a bare command
    // name is unresolved — it may be an alias or a shell function, and a grammar cannot
    // see the table that would say. These harnesses assert about tools rather than
    // about the shell, so they state the assumption out loud instead of inheriting a
    // permissive default: a fixture that got the answer by default would stop testing
    // the property the moment the default moved.
    let surroundings = crate::intent::Surroundings::default().with_pinned_shell(
        "test fixture: these harnesses assert about tools, not about whether a command \
         name can be shadowed",
    );
    Box::new(
        AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "test",
            |req: &crate::adjudicate::AdjudicationRequest| {
                Some(AdjudicationDecision::selected(
                    req,
                    "allow_once",
                    "human:test",
                    "the test harness allows every gated call",
                ))
            },
        )))
        .with_surroundings(surroundings),
    )
}

/// A session that can run commands, or the reason it cannot.
///
/// **Deliberately a `Result` and not an `Option`.** A test that silently skipped
/// when there is no cgroup v2 would be a green test that measured nothing, which
/// is the same defect as the reaper whose zero is unfalsifiable — so the error
/// comes back and the caller has to say what it did about it.
///
/// `gate` is explicit for the same reason [`writable_harness`]'s is: a harness
/// whose gate happened to admit would make every exec test also a test that the
/// gate is broken.
pub fn runner_harness() -> Result<Harness, crate::exec::ExecError> {
    runner_harness_with_gate(Some(allow_all()))
}

pub fn runner_harness_with_gate(
    gate: Option<Box<dyn crate::runtime::Gate>>,
) -> Result<Harness, crate::exec::ExecError> {
    let dir = TempDir::new();
    fixture_tree(dir.path());
    let host = Arc::new(crate::exec::HostProcesses::new(dir.path())?);
    let backend = crate::backend::HostBackend::executable_with(dir.path(), Arc::clone(&host))
        .expect("fixture root");
    let registry = crate::runner_tools(Arc::new(Unavailable)).expect("built-ins register");
    let mut rt = ToolRuntime::new(registry, Box::new(backend));
    if let Some(g) = gate {
        rt = rt.with_gate(g);
    }
    Ok(Harness {
        rt,
        sink: RecordingToolSink::new(),
        processes: Some(host),
        mount: Default::default(),
        _dir: dir,
    })
}

/// A session that can run commands **inside a measured project-scoped boundary**,
/// or the reason it cannot.
///
/// A `Result` for the same reason [`runner_harness`] is one, and here the error
/// branch is the more interesting of the two: a host with no usable user namespace
/// is a real host — `kernel.apparmor_restrict_unprivileged_userns=1` is the Ubuntu
/// default since 24.04 — and a test that skipped there would be a green suite
/// asserting nothing about the boundary it is named after. So the caller gets the
/// refusal and has to assert something about it.
pub fn confined_harness() -> Result<Harness, crate::exec::ExecError> {
    confined_harness_with(|root| crate::exec::Bwrap::project(root).map(|b| Box::new(b) as _))
}

/// The same, over a chosen confinement, so a test can build a boundary that is
/// deliberately broken and check what the substrate does about it.
pub fn confined_harness_with(
    build: impl FnOnce(
        &std::path::Path,
    ) -> Result<Box<dyn crate::exec::Confinement>, crate::exec::ExecError>,
) -> Result<Harness, crate::exec::ExecError> {
    let dir = TempDir::new();
    fixture_tree(dir.path());
    // Canonicalised, because the backend canonicalises its root and the view is
    // compared lexically. A `/tmp` that is a symlink would otherwise put the
    // project outside its own view.
    let root = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| dir.path().to_path_buf());
    let confine = build(&root)?;
    let host = Arc::new(crate::exec::HostProcesses::confined(&root, confine)?);
    let backend = crate::backend::HostBackend::executable_with(&root, Arc::clone(&host))
        .expect("fixture root");
    let registry = crate::runner_tools(Arc::new(Unavailable)).expect("built-ins register");
    let rt = ToolRuntime::new(registry, Box::new(backend)).with_gate(allow_all());
    Ok(Harness {
        rt,
        sink: RecordingToolSink::new(),
        processes: Some(host),
        mount: Default::default(),
        _dir: dir,
    })
}

/// A session with the network tools registered, over whatever is behind them.
///
/// The gate **allows**, because what these tests are about is the second gate: the
/// backend seam. A session with no adjudicator refuses at the first gate and never
/// reaches the tool, which is [`external_harness_with_gate`]'s `None` case and is
/// itself worth a test — it is the configuration a daemon starts in.
pub fn external_harness(backends: crate::ExternalBackends) -> Harness {
    external_harness_with_gate(backends, Some(allow_all()))
}

pub fn external_harness_with_gate(
    backends: crate::ExternalBackends,
    gate: Option<Box<dyn crate::runtime::Gate>>,
) -> Harness {
    build_ext(
        Spiller::unset(),
        Arc::new(Unavailable),
        false,
        gate,
        Some(backends),
    )
}

fn build(
    spiller: Spiller,
    retrieval: Arc<dyn Retrieval>,
    writable: bool,
    gate: Option<Box<dyn crate::runtime::Gate>>,
) -> Harness {
    build_ext(spiller, retrieval, writable, gate, None)
}

fn build_ext(
    spiller: Spiller,
    retrieval: Arc<dyn Retrieval>,
    writable: bool,
    gate: Option<Box<dyn crate::runtime::Gate>>,
    external: Option<crate::ExternalBackends>,
) -> Harness {
    let dir = TempDir::new();
    fixture_tree(dir.path());
    let (backend, registry): (HostBackend, Registry) = if writable {
        (
            HostBackend::writable(dir.path()).expect("fixture root"),
            crate::coder_tools(retrieval).expect("built-ins register"),
        )
    } else {
        (
            HostBackend::new(dir.path()).expect("fixture root"),
            crate::read_only_tools(retrieval).expect("built-ins register"),
        )
    };
    // Registered, not seated: a role is what a session's prompt carries, and these
    // three plus the read-only seven are over §8.4's ceiling.
    let (registry, mount) = match &external {
        Some(b) => {
            let mut reg = crate::external_tools(registry, b).expect("external tools register");
            // A real session mounts at open, against the role's remaining seats.
            let report = crate::builtins::external::mcp::mount(
                &mut reg,
                b.mcp.clone(),
                crate::DEFAULT_MAX_TOOLS,
            );
            (reg, report)
        }
        None => (registry, Default::default()),
    };
    let mut rt = ToolRuntime::new(registry, Box::new(backend)).with_spiller(spiller);
    if let Some(g) = gate {
        rt = rt.with_gate(g);
    }
    Harness {
        rt,
        sink: RecordingToolSink::new(),
        processes: None,
        mount,
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
