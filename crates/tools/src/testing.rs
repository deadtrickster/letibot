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
    /// The promote channel the backend reads, for a test to simulate a head's Ctrl+B
    /// by setting it before a `bash` call runs.
    pub promote: Option<Arc<std::sync::Mutex<Option<String>>>>,
    /// Held so the tree outlives the backend.
    _dir: TempDir,
    /// Held so the session's scratch directory outlives the backend, for the
    /// unconfined external harness. `None` for every other shape.
    _scratch: Option<TempDir>,
    /// Held so the box-wide notes directory outlives the `notes` tool, which
    /// reads it through its scope rather than through the backend.
    _notes_global: TempDir,
    /// The two directories the `notes` tool is scoped to, for a test that
    /// writes a fixture note or reads one back: the workspace and the
    /// box-wide notes dir.
    _notes: (std::path::PathBuf, std::path::PathBuf),
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

    /// The session's scratch directory, for the unconfined external harness.
    /// `None` for every other shape, which has no scratch to look at.
    pub fn scratch_dir(&self) -> Option<&std::path::Path> {
        self._scratch.as_ref().map(|d| d.path())
    }

    /// The two directories the `notes` tool is scoped to — the workspace and
    /// a hermetic box-wide notes dir — owned, so a test can keep using them
    /// across `call`s (which take the harness mutably).
    pub fn notes_dirs(&self) -> (std::path::PathBuf, std::path::PathBuf) {
        self._notes.clone()
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

    /// **Wire the R23 question**, the way the daemon wires it from its own job
    /// watchers.
    ///
    /// Exposed as a method rather than left to a test poking `rt` because the
    /// closure's meaning is the thing under test: *"is this job's completion already
    /// being delivered to the model?"* A test that sets it says which jobs the
    /// harness is watching and nothing else, and a runtime that never calls this
    /// behaves exactly as it did before R23 existed — which is what the
    /// contrast case needs.
    pub fn with_completion_delivered(&mut self, f: crate::runtime::CompletionDelivered) {
        self.rt.completion_delivered = Some(f);
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

/// A read-only session whose gate admits — for a tool whose OWN second-gate
/// check is the thing under test: with [`harness`] the first gate (no
/// adjudicator) refuses first and the tool is never reached, and with
/// [`writable_harness`] the backend never refuses.
pub fn read_only_harness_with_gate(gate: Option<Box<dyn crate::runtime::Gate>>) -> Harness {
    build(Spiller::unset(), Arc::new(Unavailable), false, gate)
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

/// The gate that refuses every gated call, with a workspace in its surroundings,
/// so a refusal's payload can be asserted on: R9's test reads what the model
/// would read.
pub fn deny_all() -> Box<dyn crate::runtime::Gate> {
    use crate::adjudicate::{AdjudicatedGate, AdjudicationDecision, AskAdjudicator};
    let surroundings = crate::intent::Surroundings::from_env("/home/dead/Projects/letibot");
    Box::new(
        AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "test",
            |req: &crate::adjudicate::AdjudicationRequest| {
                Some(AdjudicationDecision::selected(
                    req,
                    "deny",
                    "human:test",
                    "the test harness refuses every gated call",
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
    let promote: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let backend = crate::backend::HostBackend::executable_with(dir.path(), Arc::clone(&host))
        .expect("fixture root")
        .with_promote_channel(promote.clone());
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
        promote: Some(promote),
        _dir: dir,
        _scratch: None,
        _notes_global: TempDir::new(),
        _notes: (std::path::PathBuf::new(), std::path::PathBuf::new()),
    })
}

/// **The runner's tool set over a backend that cannot start a process.**
///
/// For the properties `bash` decides from the command **text** and nothing else — the
/// terminal rule ([`crate::exec::terminal`]), the argument checks, the scope names —
/// which are all reached before the backend is consulted.
///
/// A `runner_harness` would be the wrong instrument for those and not because it is
/// wrong: it needs a delegated cgroup v2 subtree, so on a host without one a test of a
/// property that never touches a process would take `runner!`'s refusal branch and
/// assert nothing. This one has no host at all, so the property is measured everywhere
/// — and `processes: None` is itself part of the fixture: a call that reaches the
/// backend gets the backend's refusal, which is how a test tells *the terminal rule did
/// not fire* from *the terminal rule fired and something else refused afterwards*.
pub fn text_only_runner_harness() -> Harness {
    let dir = TempDir::new();
    fixture_tree(dir.path());
    let backend = HostBackend::writable(dir.path()).expect("fixture root");
    let registry = crate::runner_tools(Arc::new(Unavailable)).expect("built-ins register");
    // A gate that allows, so a MODEL's call reaches the tool: the contrast the terminal
    // rule's wiring test needs is the model's entry, and a session with no adjudicator
    // would refuse it at the first gate and never reach the tool at all.
    let rt = ToolRuntime::new(registry, Box::new(backend)).with_gate(allow_all());
    Harness {
        rt,
        sink: RecordingToolSink::new(),
        processes: None,
        mount: Default::default(),
        promote: None,
        _dir: dir,
        _scratch: None,
        _notes_global: TempDir::new(),
        _notes: (std::path::PathBuf::new(), std::path::PathBuf::new()),
    }
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
    confined_harness_with_gate(None, build)
}

/// A confined session with a chosen gate — the confined half of
/// [`runner_harness_with_gate`], and it exists for the same reason: a harness whose gate
/// happened to admit would make every boundary test also a test that the gate is broken,
/// and a test that wants to COUNT the gate's calls (the operator's own `!` path) needs a
/// gate it chose rather than one that answers.
pub fn confined_harness_with_gate(
    gate: Option<Box<dyn crate::runtime::Gate>>,
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
    let mut rt = ToolRuntime::new(registry, Box::new(backend));
    if let Some(g) = gate {
        rt = rt.with_gate(g);
    } else {
        rt = rt.with_gate(allow_all());
    }
    Ok(Harness {
        rt,
        sink: RecordingToolSink::new(),
        processes: Some(host),
        mount: Default::default(),
        promote: None,
        _dir: dir,
        _scratch: None,
        _notes_global: TempDir::new(),
        _notes: (std::path::PathBuf::new(), std::path::PathBuf::new()),
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
        false,
    )
}

/// A **writable, unconfined** session with the network tools registered, over
/// whatever is behind them.
///
/// The read-only [`external_harness`] cannot exercise the `web_fetch` scratchpad:
/// the page is written to disk through the backend, and a backend that was opened
/// read-only has no scratchpad to write to. The scratchpad is under `/tmp`, which
/// a backend rooted at the fixture tree cannot see, so this one roots the backend
/// at `/` with the fixture tree as cwd — the leticode seat's shape — and allows
/// the gate, so a `web_fetch` call reaches the tool and the tool can put the page
/// where it says it did.
pub fn writable_external_harness(backends: crate::ExternalBackends) -> Harness {
    build_ext(
        Spiller::unset(),
        Arc::new(Unavailable),
        true,
        Some(allow_all()),
        Some(backends),
        true,
    )
}

fn build(
    spiller: Spiller,
    retrieval: Arc<dyn Retrieval>,
    writable: bool,
    gate: Option<Box<dyn crate::runtime::Gate>>,
) -> Harness {
    build_ext(spiller, retrieval, writable, gate, None, false)
}

fn build_ext(
    spiller: Spiller,
    retrieval: Arc<dyn Retrieval>,
    writable: bool,
    gate: Option<Box<dyn crate::runtime::Gate>>,
    external: Option<crate::ExternalBackends>,
    unconfined: bool,
) -> Harness {
    let dir = TempDir::new();
    fixture_tree(dir.path());
    // `unconfined` roots the backend at `/` with the fixture tree as cwd, the way
    // the leticode seat does: relative paths still start in the tree, but an
    // absolute path — the session's scratch directory — is reachable. The scratch
    // is a per-session temp dir, set on the backend the way the daemon does.
    let scratch = if unconfined {
        Some(TempDir::new())
    } else {
        None
    };
    let (backend, registry): (HostBackend, Registry) = if writable && unconfined {
        (
            HostBackend::writable("/")
                .expect("root")
                .with_cwd(dir.path())
                .expect("fixture cwd")
                .with_scratch_dir(scratch.as_ref().unwrap().path()),
            crate::coder_tools(retrieval).expect("built-ins register"),
        )
    } else if writable {
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
    // The session tool rides every registry, exactly as the real harness seats it:
    // the read-only roles name `todo_write`, so a registry they resolve against has
    // to include it. An empty board — these tests are not about the todo pane.
    let mut registry = crate::with_session_tools(
        registry,
        Arc::new(crate::builtins::todo::TodoBoard::new(Vec::new())),
        Arc::new(crate::builtins::task::NoTaskRunner),
        Arc::new(crate::builtins::skill::SkillRegistry::default()),
        Arc::new(crate::builtins::lsp::LspConfig::default()),
    )
    .expect("todo_write registers");
    // And the notes tool, over a scope of this fixture's own: the box-wide
    // notes dir is a temp dir per harness so a test never reads this box's
    // real config, and the workspace is the fixture tree — the same two facts
    // the daemon scopes the tool with. Registered in the read-only shape too,
    // because the tool's read verbs are the ones that must refuse honestly
    // there (naming the second gate), not vanish.
    let notes_global = TempDir::new();
    let notes_scope: Arc<dyn crate::builtins::notes::NotesScope> = Arc::new(TestNotesScope {
        workspace: dir.path().to_path_buf(),
        global: notes_global.path().to_path_buf(),
    });
    let notes_dirs = (dir.path().to_path_buf(), notes_global.path().to_path_buf());
    registry
        .register(Box::new(crate::builtins::notes::NotesTool::new(
            notes_scope,
        )))
        .expect("notes registers");
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
        promote: None,
        _dir: dir,
        _scratch: scratch,
        _notes_global: notes_global,
        _notes: notes_dirs,
    }
}

/// The `notes` tool's scope for a fixture: the fixture tree as the workspace
/// and a per-harness temp dir as the box-wide notes, so a test that lists or
/// reads never depends on (or writes to) this box's real config directory.
struct TestNotesScope {
    workspace: std::path::PathBuf,
    global: std::path::PathBuf,
}

impl crate::builtins::notes::NotesScope for TestNotesScope {
    fn workspace(&self) -> std::path::PathBuf {
        self.workspace.clone()
    }
    fn global_dir(&self) -> std::path::PathBuf {
        self.global.clone()
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
