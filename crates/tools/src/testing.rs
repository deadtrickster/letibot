//! A fixture tree, a runtime over it, and a scripted retrieval backend.
//! `pub` behind the `testing` feature because the clause tests in `tests/` need the same tree the
//! unit tests use: two such trees is how they drift, and a miss is only meaningful if it is known.

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

/// The tree every test in this crate reasons about. The details are chosen for the misses: a
/// plausible guess matches nothing, a doc-only symbol gives a scoped search somewhere to point,
/// and `big.txt` is large enough to spill under a small budget.
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
    /// The process host, for a session built by [`runner_harness`]. Tests reach it to declare a
    /// protected pid, to end a scope, and to read the reap log: none of which is a tool call, and
    /// all of which a daemon does.
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
    /// The two directories the `notes` tool is scoped to, for a test that writes a fixture note
    /// or reads one back: the workspace and the box-wide notes dir.
    _notes: (std::path::PathBuf, std::path::PathBuf),
    /// The file the `host_changelog` tool records into, held so the temp
    /// directory outlives the tool's scope. A test reads it directly: a test
    /// that asked the tool whether it wrote is a test of the tool's opinion.
    _changelog: std::path::PathBuf,
    /// Held so the directory `_changelog` lives in is removed with the harness.
    _changelog_dir: TempDir,
    /// The platform `host_config` runs on here: a fixture, never this box, since a test that
    /// drove the real one would change the operator's keyboard repeat or their governor.
    _host_config: Arc<FixturePlatform>,
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

    /// The tree's root. Tests for the write tools have to look at the disk behind the backend: a
    /// test that asked the tool whether it wrote is a test of the tool's opinion of itself.
    pub fn root(&self) -> &std::path::Path {
        self._dir.path()
    }

    /// The session's scratch directory, for the unconfined external harness.
    /// `None` for every other shape, which has no scratch to look at.
    pub fn scratch_dir(&self) -> Option<&std::path::Path> {
        self._scratch.as_ref().map(|d| d.path())
    }

    /// The two directories the `notes` tool is scoped to, the workspace and a hermetic box-wide
    /// notes dir, owned so a test can keep using them across `call`s (which take the harness
    /// mutably).
    pub fn notes_dirs(&self) -> (std::path::PathBuf, std::path::PathBuf) {
        self._notes.clone()
    }

    /// The host changelog this harness records into: a temp file, so a test can read what the
    /// tool wrote without going through the tool.
    pub fn changelog_file(&self) -> &std::path::Path {
        &self._changelog
    }

    /// The platform `host_config` runs on here, so a test can read what was applied, and can
    /// assert that nothing was, which is the half a refusal is about.
    pub fn host_config(&self) -> Arc<FixturePlatform> {
        Arc::clone(&self._host_config)
    }

    /// What is actually on disk, read without going through the backend.
    pub fn read_file(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root().join(rel)).unwrap_or_default()
    }

    /// Write to the tree from outside the harness: somebody else's edit.
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

    /// Wire the completion question, the way the daemon wires it from its own job watchers.
    /// Exposed as a method because the closure's meaning is the thing under test: a test that sets
    /// it says which jobs the harness is watching and nothing else.
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

/// A session that can change the tree: the coder tool set, a writable backend, and an
/// adjudicator that allows. The adjudicator is explicit rather than absent, because a harness
/// whose gate happened to admit would make every write test also a test that the gate is broken.
pub fn writable_harness() -> Harness {
    writable_harness_with_gate(Some(allow_all()))
}

/// A writable session with a chosen gate. `None` attaches no adjudicator, which is the
/// fail-closed default a real daemon starts with.
pub fn writable_harness_with_gate(gate: Option<Box<dyn crate::runtime::Gate>>) -> Harness {
    build(Spiller::unset(), Arc::new(Unavailable), true, gate)
}

/// A read-only session whose gate admits, for a tool whose own second-gate check is the thing
/// under test: with [`harness`] the first gate refuses first and the tool is never reached, and
/// with [`writable_harness`] the backend never refuses.
pub fn read_only_harness_with_gate(gate: Option<Box<dyn crate::runtime::Gate>>) -> Harness {
    build(Spiller::unset(), Arc::new(Unavailable), false, gate)
}

/// A writable session whose output is bounded. Clause 5 does not stop applying
/// because a tool can write.
pub fn writable_harness_with_budget(spiller: Spiller) -> Harness {
    build(spiller, Arc::new(Unavailable), true, Some(allow_all()))
}

/// An adjudicator that says yes to everything, for tests about tools rather than about the gate.
/// Not exported from the crate root and not something a daemon can construct by accident: it
/// lives behind the `testing` feature, and the fail-closed default is what ships.
pub fn allow_all() -> Box<dyn crate::runtime::Gate> {
    use crate::adjudicate::{AdjudicatedGate, AdjudicationDecision, AskAdjudicator};
    // The shell trust is declared here, and the declaration is the point: `ShellTrust` defaults
    // to `Unknown`, under which a bare command name is unresolved. These harnesses assert about
    // tools rather than about the shell, so they state the assumption rather than inherit a default.
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

/// The gate that refuses every gated call, with a workspace in its surroundings, so a refusal's
/// payload can be asserted on: the test reads what the model would read.
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

/// A session that can run commands, or the reason it cannot. Deliberately a `Result` and not an
/// `Option`: a test that silently skipped with no cgroup v2 would be a green test that measured
/// nothing. `gate` is explicit so a harness whose gate happened to admit cannot pass the test.
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
    // A session scratch directory, because a promoted run's output has to have somewhere to go:
    // the daemon wires this from `harness::scratch_dir()`, and a fixture without it would measure
    // the no-scratch branch instead of the one that matters.
    let scratch = TempDir::new();
    let backend = crate::backend::HostBackend::executable_with(dir.path(), Arc::clone(&host))
        .expect("fixture root")
        .with_promote_channel(promote.clone())
        .with_scratch_dir(scratch.path());
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
        _scratch: Some(scratch),
        _notes_global: TempDir::new(),
        _notes: (std::path::PathBuf::new(), std::path::PathBuf::new()),
        _changelog: std::path::PathBuf::new(),
        _changelog_dir: TempDir::new(),
        _host_config: FixturePlatform::new(),
    })
}

/// The runner's tool set over a backend that cannot start a process, for the properties `bash`
/// decides from the command text alone. It has no host, so those are measured everywhere, and a
/// call that reaches the backend gets its refusal rather than a missing cgroup.
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
        _changelog: std::path::PathBuf::new(),
        _changelog_dir: TempDir::new(),
        _host_config: FixturePlatform::new(),
    }
}

/// A session that can run commands inside a measured project-scoped boundary, or the reason it
/// cannot. A `Result` for [`runner_harness`]'s reason, and here the error branch is the more
/// interesting: a host with no usable user namespace is a real host, so the caller must say.
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

/// A confined session with a chosen gate, the confined half of [`runner_harness_with_gate`]: a
/// harness whose gate happened to admit would make every boundary test also a test that the gate
/// is broken, and a test that counts the gate's calls needs one it chose.
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
        _changelog: std::path::PathBuf::new(),
        _changelog_dir: TempDir::new(),
        _host_config: FixturePlatform::new(),
    })
}

/// A session with the network tools registered, over whatever is behind them. The gate allows,
/// because what these tests are about is the second gate: the backend seam. A session with no
/// adjudicator refuses at the first gate and never reaches the tool.
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

/// A writable, unconfined session with the network tools registered, over whatever is behind
/// them. The read-only [`external_harness`] cannot exercise the `web_fetch` scratchpad, so this
/// one roots the backend at `/` with the fixture tree as cwd, the leticode seat's shape.
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

/// A session over a chosen backend, with the coder tool set and a gate that allows: the fixture
/// for a tool whose files are somewhere the daemon's own box is not. The `notes` tool is scoped to
/// `(workspace, global)`, which is the point - a scope that disagrees with the backend is what a
/// placed session's daemon-side scope looks like.
pub fn harness_over(
    backend: Box<dyn crate::backend::ExecBackend>,
    workspace: std::path::PathBuf,
    global: std::path::PathBuf,
) -> Harness {
    let mut registry = crate::coder_tools(Arc::new(Unavailable)).expect("built-ins register");
    let scope: Arc<dyn crate::builtins::notes::NotesScope> = Arc::new(TestNotesScope {
        workspace: workspace.clone(),
        global: global.clone(),
    });
    registry
        .register(Box::new(crate::builtins::notes::NotesTool::new(scope)))
        .expect("notes registers");
    let rt = ToolRuntime::new(registry, backend).with_gate(allow_all());
    Harness {
        rt,
        sink: RecordingToolSink::new(),
        processes: None,
        mount: Default::default(),
        promote: None,
        _dir: TempDir::new(),
        _scratch: None,
        _notes_global: TempDir::new(),
        _notes: (workspace, global),
        _changelog: std::path::PathBuf::new(),
        _changelog_dir: TempDir::new(),
        _host_config: FixturePlatform::new(),
    }
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
    // `unconfined` roots the backend at `/` with the fixture tree as cwd, the way the leticode
    // seat does: relative paths still start in the tree, but an absolute path is reachable. The
    // scratch is a per-session temp dir, set on the backend the way the daemon does.
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
    // The host changelog, over a scope of this fixture's own: a temp file per harness, so a test
    // never records into this box's real config dir. Registered before `with_session_tools`, which
    // puts a `nowhere` one behind any registry that has none, and the scoped one can record.
    let changelog_dir = TempDir::new();
    let changelog_file = changelog_dir.path().join(crate::builtins::changelog::FILE);
    let changelog_scope: Arc<dyn crate::builtins::changelog::ChangelogScope> =
        Arc::new(TestChangelogScope {
            file: changelog_file.clone(),
            who: "s-fixture".to_string(),
        });
    let mut registry = registry;
    registry
        .register(Box::new(
            crate::builtins::changelog::HostChangelogTool::new(changelog_scope),
        ))
        .expect("the host changelog registers");
    // The host config's platform is a FIXTURE, and its privilege is a fixed refusal: a test that
    // drove this box's real `defaults`/`xset` would change the operator's keyboard repeat, and one
    // that detected privilege would answer differently on a root box than on theirs.
    let host_config = FixturePlatform::new();
    registry
        .register(Box::new(
            crate::builtins::host_config::HostConfigTool::new(std::sync::Arc::new(
                TestChangelogScope {
                    file: changelog_file.clone(),
                    who: "s-fixture".to_string(),
                },
            ))
            .with_platform(
                Arc::clone(&host_config) as Arc<dyn crate::builtins::host_config::Platform>
            )
            .with_privilege(crate::builtins::host_config::Privilege::None(
                "the fixture has no askpass, and this is the sentence a person would read".into(),
            )),
        ))
        .expect("the host config registers");
    // The session tool rides every registry, exactly as the real harness seats it: the read-only
    // roles name `todo_write`, so a registry they resolve against has to include it. An empty
    // board, since these tests are not about the todo pane.
    let mut registry = crate::with_session_tools(
        registry,
        Arc::new(crate::builtins::todo::TodoBoard::new(Vec::new())),
        None,
        Arc::new(crate::builtins::task::NoTaskRunner),
        Arc::new(crate::builtins::skill::SkillRegistry::default()),
        Arc::new(crate::builtins::lsp::LspConfig::default()),
    )
    .expect("todo_write registers");
    // And the notes tool, over a scope of this fixture's own: a temp dir per harness, so a test
    // never reads this box's real config. Registered in the read-only shape too, because the
    // tool's read verbs must refuse honestly there rather than vanish.
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
    // Registered, not seated: a role is what a session's prompt carries, and these three plus the
    // read-only seven are over the seat ceiling.
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
        _changelog: changelog_file,
        _changelog_dir: changelog_dir,
        _host_config: host_config,
    }
}

/// The `notes` tool's scope for a fixture: the fixture tree as the workspace
/// and a per-harness temp dir as the box-wide notes, so a test that lists or
/// reads never depends on (or writes to) this box's real config directory.
struct TestNotesScope {
    workspace: std::path::PathBuf,
    global: std::path::PathBuf,
}
/// The `host_changelog` tool's scope for a fixture: one temp file and a chosen writer, so a test
/// records without ever touching this box's real config dir.
struct TestChangelogScope {
    file: std::path::PathBuf,
    who: String,
}

impl crate::builtins::changelog::ChangelogScope for TestChangelogScope {
    fn path(&self) -> std::path::PathBuf {
        self.file.clone()
    }

    fn host(&self) -> String {
        "fixture-box".to_string()
    }

    fn who(&self) -> String {
        self.who.clone()
    }
}

/// The platform a fixture's `host_config` runs on: two settings, one that needs root and one that
/// does not, and every apply recorded. A fixture rather than the real arm because the tool's whole
/// point is that it changes the machine, and `applied` is what a refusal is measured against.
pub struct FixturePlatform {
    knob: std::sync::Mutex<String>,
    rooted: std::sync::Mutex<String>,
    applied: std::sync::Mutex<Vec<String>>,
}

/// The fixture's settings, in the shape a platform's are.
const FIXTURE_SETTINGS: &[crate::builtins::host_config::Setting] = &[
    crate::builtins::host_config::Setting {
        name: "fixture-knob",
        unit: "a word",
        needs_root: false,
        note: "read from the fixture, written by the fixture",
    },
    crate::builtins::host_config::Setting {
        name: "fixture-root-knob",
        unit: "a word",
        needs_root: true,
        note: "read from the fixture, written with `sudo` — so a fixture without privilege refuses",
    },
];

impl FixturePlatform {
    /// A platform whose two settings read `before` and `root-before`.
    pub fn new() -> Arc<FixturePlatform> {
        Arc::new(FixturePlatform {
            knob: std::sync::Mutex::new("before".to_string()),
            rooted: std::sync::Mutex::new("root-before".to_string()),
            applied: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// What `fixture-knob` is now.
    pub fn knob(&self) -> String {
        self.knob.lock().expect("the fixture's knob").clone()
    }

    /// Every command the tool has run here, in order; empty after a refusal.
    pub fn applied(&self) -> Vec<String> {
        self.applied.lock().expect("the fixture's log").clone()
    }

    fn setting(name: &str) -> Option<&'static crate::builtins::host_config::Setting> {
        FIXTURE_SETTINGS.iter().find(|s| s.name == name)
    }

    fn cell(&self, name: &str) -> Option<&std::sync::Mutex<String>> {
        match name {
            "fixture-knob" => Some(&self.knob),
            "fixture-root-knob" => Some(&self.rooted),
            _ => None,
        }
    }
}

impl crate::builtins::host_config::Platform for FixturePlatform {
    fn name(&self) -> &'static str {
        "fixture"
    }

    fn settings(&self) -> &'static [crate::builtins::host_config::Setting] {
        FIXTURE_SETTINGS
    }

    fn read(
        &self,
        setting: &str,
    ) -> Result<crate::builtins::host_config::Reading, crate::builtins::host_config::Refusal> {
        match self.cell(setting) {
            Some(cell) => Ok(crate::builtins::host_config::Reading {
                value: cell.lock().expect("the fixture's value").clone(),
                how: "the fixture".to_string(),
                note: String::new(),
            }),
            None => Err(crate::builtins::host_config::Refusal::new(
                format!("the fixture has no `{setting}`"),
                "The fixture's settings are `fixture-knob` and `fixture-root-knob`. Nothing was \
                 changed.",
            )),
        }
    }

    fn check(
        &self,
        setting: &str,
        value: &str,
    ) -> Result<(), crate::builtins::host_config::Refusal> {
        match Self::setting(setting) {
            // The fixture takes one word, which is all its settings are.
            Some(_) if !value.trim().is_empty() => Ok(()),
            Some(_) => Err(crate::builtins::host_config::Refusal::new(
                format!("`{setting}` takes a word, and this is empty"),
                "Nothing was changed.",
            )),
            None => Err(crate::builtins::host_config::Refusal::new(
                format!("the fixture has no `{setting}`"),
                "The fixture's settings are `fixture-knob` and `fixture-root-knob`. Nothing was \
                 changed.",
            )),
        }
    }

    fn apply(
        &self,
        setting: &str,
        value: &str,
        privilege: &crate::builtins::host_config::Privilege,
    ) -> Result<crate::builtins::host_config::Applied, crate::builtins::host_config::Refusal> {
        self.check(setting, value)?;
        let Some(s) = Self::setting(setting) else {
            return Err(crate::builtins::host_config::Refusal::new(
                format!("the fixture has no `{setting}`"),
                "Nothing was changed.",
            ));
        };
        let command = if s.needs_root {
            let (argv, _) = privilege
                .as_root(&[format!("fixture-write {value}")])
                .map_err(|r| {
                    crate::builtins::host_config::Refusal::new(
                        format!("{} (the fixture's own sentence)", r.what),
                        r.why,
                    )
                })?;
            argv.join(" ")
        } else {
            format!("fixture-write {value}")
        };
        self.applied
            .lock()
            .expect("the fixture's log")
            .push(command.clone());
        if let Some(cell) = self.cell(setting) {
            *cell.lock().expect("the fixture's value") = value.to_string();
        }
        Ok(crate::builtins::host_config::Applied { ran: vec![command] })
    }
}

impl crate::builtins::notes::NotesScope for TestNotesScope {
    fn workspace(&self) -> std::path::PathBuf {
        self.workspace.clone()
    }
    fn global_dir(&self) -> std::path::PathBuf {
        self.global.clone()
    }
}

/// A retrieval backend that answers from a script. The four scripts are the four cases the seam
/// distinguishes, and they are the reason it is a trait: none of them needs a corpus, an embedder
/// or a network.
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

    /// Text with nothing behind it: the footnote-less hallucination.
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

    /// A backend that searched for something else.
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

/// A fixture's `git`, with none of this box's configuration in it. A box whose global config sets
/// `commit.gpgsign` signs every fixture commit and refuses it with no key, so global and system
/// config are dropped, the identity is supplied here, and nothing may prompt.
pub fn git(dir: &std::path::Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(dir)
        // `/dev/null` is this tree's only spelling: every box it builds on is a unix.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "letibot fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@letibot.invalid")
        .env("GIT_COMMITTER_NAME", "letibot fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@letibot.invalid");
    cmd
}

/// Write an executable stub and return only once it can be exec'd: a file written then exec'd
/// races every fork, since a child holds the write descriptor until its own exec. The probe runs
/// the stub with no arguments, so a no-argument run must be harmless and quick.
#[cfg(unix)]
pub fn write_stub(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).expect("stub");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("stub mode");
    for _ in 0..100 {
        match std::process::Command::new(path)
            .env(STUB_PROBE, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            _ => return,
        }
    }
    panic!("{} stayed busy for two seconds", path.display())
}

/// The variable [`write_stub`]'s probe sets, for a stub whose real run takes no arguments either
/// and therefore cannot recognise the probe by `$#`. See that function.
#[cfg(unix)]
pub const STUB_PROBE: &str = "LETIBOT_STUB_PROBE";

/// The race `write_stub` closes, forced to happen, so the fix is measured rather than waited for:
/// the real failure is one run in three hundred under load. Linux only: see the test.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::io::AsRawFd;
    use std::time::{Duration, Instant};

    const BODY: &str = "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nexit 3\n";

    /// A stub a forked child still holds is waited for, not exec'd into `Text file busy`: the
    /// window is microseconds in life and a second wide here, with `FD_CLOEXEC` cleared so the
    /// holder keeps the descriptor across its own exec. Linux only, since `ETXTBSY` is its own.
    #[test]
    fn a_stub_a_forked_child_holds_is_waited_for() {
        let dir = std::env::temp_dir().join(format!("letibot-write-stub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory for the stub");
        let stub = dir.join("held");

        let mut f = std::fs::File::create(&stub).expect("create");
        f.write_all(BODY.as_bytes()).expect("write");
        f.flush().expect("flush");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("mode");
        // SAFETY: `fcntl` with `F_SETFD` and a zero: no state this process reads is touched, and
        // the descriptor is one this test owns.
        unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETFD, 0) };
        let mut holder = std::process::Command::new("sleep")
            .arg("1")
            .spawn()
            .expect("a holder to fork");
        drop(f);

        let refused = std::process::Command::new(&stub)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect_err("the holder keeps the file busy");
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::ExecutableFileBusy,
            "the reproduction has to be the real one: {refused}"
        );

        let began = Instant::now();
        write_stub(&stub, BODY);
        let waited = began.elapsed();
        assert!(
            waited >= Duration::from_millis(500),
            "write_stub returned in {waited:?}, before the holder could have let go of it"
        );
        assert_eq!(holder.wait().expect("the holder exits").code(), Some(0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
