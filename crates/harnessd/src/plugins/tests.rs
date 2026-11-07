//! The plugin host's census: discovery and the ladder, what a layer may answer, and the panes
//! and hooks a session's stack produces. A sibling, so the host's own code reads as code.
use super::*;
use letibot_plugins::{BuildError, Manifest, Module};
use letibot_tools::runtime::GateDecision;
use letibot_tools::schema::Access;
use std::sync::atomic::{AtomicU64, Ordering};

/// A module that answers from a file beside it, so a test can change what a layer says by
/// writing to disk - which is what a hot reload is.
struct FileModule {
    hooks: Vec<String>,
    answer: PathBuf,
}

impl Module for FileModule {
    fn hooks(&self) -> Vec<String> {
        self.hooks.clone()
    }

    fn call(&self, _hook: &str, input: &str) -> Result<Option<String>, String> {
        // A layer that traps, which is a file rather than a module because this is a fake engine.
        if self.answer.with_file_name("TRAP").exists() {
            return Err("trapped: error while executing at wasm backtrace".to_string());
        }
        // Two frames, like the real example: a pane handed a look's result answers its
        // dashboard, and one that is not answers the frame that asks for it. `second.json` is
        // that dashboard; without it a layer answers once.
        let got_look = serde_json::from_str::<Value>(input)
            .map(|v| !v["look"].is_null())
            .unwrap_or(false);
        let second = self.answer.with_file_name("second.json");
        let path = match (got_look, second.is_file()) {
            (true, true) => second,
            _ => self.answer.clone(),
        };
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some(text))
    }

    fn fuel(&self) -> u64 {
        7
    }
}

/// The layer directory holds the answer, so the loader and the builder agree about what the
/// module says.
struct FileLoader;

impl Loader for FileLoader {
    fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        let _ = std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
        let dir = wasm.parent().unwrap_or(Path::new("."));
        let manifest = Manifest::read(dir)?;
        Ok(Box::new(FileModule {
            hooks: manifest.hooks.clone(),
            answer: dir.join("answer.json"),
        }))
    }
}

/// The gate's build step without cargo: the layer's `next.json` becomes its `plugin.wasm` and
/// its `answer.json`, and a `FAIL` file makes the build fail. The real `CargoBuilder` needs the
/// wasm target and a crate to build, which is what the seam is for.
struct FileBuilder;

impl Builder for FileBuilder {
    fn build(&self, dir: &Path) -> Result<String, BuildError> {
        let log = format!("built {}", dir.display());
        if dir.join("FAIL").exists() {
            return Err(BuildError {
                why: "cargo build failed".into(),
                log: format!("{log}\nerror: cannot find value `plan` in this scope"),
            });
        }
        let next = dir.join("next.json");
        if next.exists() {
            std::fs::copy(&next, dir.join("answer.json")).expect("an answer");
            std::fs::copy(&next, dir.join("plugin.wasm")).expect("a module");
        }
        Ok(log)
    }
}

static N: AtomicU64 = AtomicU64::new(0);

fn tempdir(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "letibot-plugins-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a temp dir");
    path
}

/// A project with one layer that answers `pane_dashboard` with the text of `answer.json`.
fn project(tag: &str, answer: &str) -> (PathBuf, Arc<Hub>) {
    let root = tempdir(tag);
    let layer = root.join(".letibot/plugins/plan-dashboard");
    std::fs::create_dir_all(&layer).expect("the layer dir");
    std::fs::write(
        layer.join("plugin.toml"),
        "name = \"plan-dashboard\"\n\
         purpose = \"a project plan at a glance\"\n\
         hooks = [\"pane_dashboard\"]\n\
         capabilities = [\"read\"]\n\
         contract = 1\n",
    )
    .expect("a manifest");
    std::fs::write(layer.join("plugin.wasm"), b"a module").expect("a module");
    std::fs::write(layer.join("answer.json"), answer).expect("an answer");
    (root, Hub::new("s-plugins"))
}

fn host(root: &Path) -> PluginHost {
    PluginHost::discover(
        "s-plugins",
        root,
        None,
        None,
        Arc::new(FileLoader),
        Arc::new(FileBuilder),
    )
}

/// A view that validates: one panel, one key, no host key.
const A_VIEW: &str = r#"{"title":"one","panels":[{"id":"plan","key":"t","title":"plan",
    "summary":[{"markdown":"a summary"}]}],"every_s":0}"#;

fn views(hub: &Arc<Hub>) -> Vec<(String, Value)> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::PluginPane { pane, view } => Some((pane.clone(), view.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_layers_pane_is_named_after_the_manifest_and_the_hook() {
    let (root, _hub) = project("names", A_VIEW);
    let host = host(&root);
    assert_eq!(host.panes(), vec!["plan-dashboard/dashboard".to_string()]);
}

#[test]
fn announce_publishes_the_first_frame_of_every_pane_and_runs_no_look() {
    let (root, hub) = project("announce", A_VIEW);
    let host = host(&root);
    host.announce(&hub);
    let seen = views(&hub);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].0, "plan-dashboard/dashboard");
    assert_eq!(seen[0].1["title"], json!("one"));
}

#[test]
fn a_read_look_outside_the_workspace_is_refused() {
    let root = tempdir("outside");
    let elsewhere = tempdir("elsewhere");
    let secret = elsewhere.join("plan.json");
    std::fs::write(&secret, "{}").expect("a file outside the workspace");
    // An ABSOLUTE path, which is the interesting case: `join` on one replaces the root, and
    // what refuses it is the containment check on the canonicalised path.
    let result = look::run(
        &LookSpec::Read {
            read: secret.display().to_string(),
        },
        look::Trees::one(&root),
        &[Capability::Read],
    );
    assert_eq!(result["exit"], json!(1), "{result}");
    assert!(
        result["stderr"]
            .as_str()
            .unwrap()
            .contains("outside the session's workspace"),
        "{result}"
    );
    // And a `..` that climbs out, which is the same check one spelling over.
    let climbing = look::run(
        &LookSpec::Read {
            read: "../plan.json".into(),
        },
        look::Trees::one(&root),
        &[Capability::Read],
    );
    assert_eq!(climbing["exit"], json!(1), "{climbing}");
}

#[test]
fn a_read_look_without_the_read_capability_is_refused() {
    let root = tempdir("nocap");
    std::fs::write(root.join("plan.json"), "{}").expect("a file");
    let result = look::run(
        &LookSpec::Read {
            read: "plan.json".into(),
        },
        look::Trees::one(&root),
        &[],
    );
    assert_eq!(result["exit"], json!(1), "{result}");
    assert!(
        result["stderr"]
            .as_str()
            .unwrap()
            .contains("did not declare the `read` capability"),
        "{result}"
    );
}

#[test]
fn a_read_look_inside_the_workspace_answers_the_text_and_a_missing_file_is_exit_one() {
    let root = tempdir("inside");
    std::fs::write(root.join("plan.json"), "{\"release\":{\"name\":\"v1\"}}").expect("a file");
    let result = look::run(
        &LookSpec::Read {
            read: "plan.json".into(),
        },
        look::Trees::one(&root),
        &[Capability::Read],
    );
    assert_eq!(result["exit"], json!(0), "{result}");
    assert_eq!(result["stdout"], json!("{\"release\":{\"name\":\"v1\"}}"));
    assert_eq!(result["stderr"], json!(""));

    // A file that is not there is a refusal the pane can draw, not an error the host raises.
    let missing = look::run(
        &LookSpec::Read {
            read: ".letibot/status.json".into(),
        },
        look::Trees::one(&root),
        &[Capability::Read],
    );
    assert_eq!(missing["exit"], json!(1), "{missing}");
    assert!(!missing["stderr"].as_str().unwrap().is_empty());
}

/// A pane in a worktree reads the project's files: a relative `read` resolves against the session's
/// CHECKOUT first, so `.letibot/status.json` is the one the checkout holds - the wall was
/// `look exit 1` on every dashboard a worktree session opened (R151).
#[test]
fn a_relative_read_in_a_worktree_resolves_against_the_checkout() {
    let root = tempdir("worktree-read");
    let repo = root.join("proj");
    let tree = repo.join("worktrees/agent-x");
    std::fs::create_dir_all(&tree).expect("the tree");
    let git = |args: &[&str]| {
        letibot_tools::testing::git(&repo)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !git(&["init", "-q"]) {
        let _ = std::fs::remove_dir_all(&root);
        return letibot_tokencore::apparatus::absent("`git`");
    }
    letibot_tools::testing::repo_config(&repo);
    if !git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "agent-x",
        tree.to_str().unwrap(),
    ]) {
        let _ = std::fs::remove_dir_all(&root);
        return letibot_tokencore::apparatus::absent("`git worktree`");
    }
    // The project's data is in the checkout, and the worktree has none of it - which is what a
    // gitignored `.letibot/` directory is.
    std::fs::create_dir_all(repo.join(".letibot")).expect("the project's dir");
    std::fs::write(
        repo.join(".letibot/status.json"),
        "{\"plan\":\"the checkout's\"}",
    )
    .expect("a status file");
    std::fs::write(tree.join("own.json"), "{\"mine\":true}").expect("a file in the worktree");

    let host = PluginHost::discover(
        "s-worktree",
        &tree,
        None,
        None,
        Arc::new(FileLoader),
        Arc::new(FileBuilder),
    );
    let trees = host.look_trees();
    assert_eq!(
        trees.checkout,
        std::fs::canonicalize(&repo).expect("the checkout"),
        "a worktree's checkout is the checkout it was cut from"
    );
    let read = look::run(
        &LookSpec::Read {
            read: ".letibot/status.json".into(),
        },
        trees,
        &[Capability::Read],
    );
    assert_eq!(read["exit"], json!(0), "{read}");
    assert!(
        read["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("the checkout's"),
        "{read}"
    );
    // And a file that is only in the worktree is still an answer: the tree the session stands in is
    // read too, so the fix does not lose what worked before it.
    let own = look::run(
        &LookSpec::Read {
            read: "own.json".into(),
        },
        trees,
        &[Capability::Read],
    );
    assert_eq!(own["exit"], json!(0), "{own}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A rebuild of a pinned layer keeps the operator's pin while its `plugin.toml` asks for the same
/// powers, and loses it the moment it asks for a new one - the wall was a `/plugin trust` after
/// every rebuild (R151).
#[test]
fn a_rebuild_keeps_a_pin_until_the_manifest_asks_for_a_new_capability() {
    let root = tempdir("repin");
    let plugins = root.join("plugins");
    let dir = plugins.join("deploy");
    std::fs::create_dir_all(&dir).expect("the layer dir");
    let manifest = |caps: &str| {
        format!(
            "name = \"deploy\"\npurpose = \"a pane\"\nhooks = [\"pane_deploy\"]\n\
             capabilities = [{caps}]\ncontract = 1\n"
        )
    };
    std::fs::write(dir.join("plugin.toml"), manifest("")).expect("a manifest");
    std::fs::write(dir.join("plugin.wasm"), b"a module").expect("a module");
    std::fs::write(dir.join("answer.json"), A_VIEW).expect("an answer");
    let trust = letibot_plugins::Trust::open(root.join("trust.json"));
    let host = PluginHost::discover(
        "s-repin",
        &root,
        Some(plugins.clone()),
        None,
        Arc::new(FileLoader),
        Arc::new(FileBuilder),
    )
    .with_trust(trust.clone());
    let hub = Hub::new("s-repin");
    host.load(&hub, "deploy").expect("the first load");
    // The operator's own door, `/plugin trust NAME`.
    trust.pin("deploy", &dir).expect("a pin");

    // A rebuild that declares the same capabilities: the pin moves with the bytes, and `load` says
    // so instead of handing the person another command to run.
    std::fs::write(dir.join("next.json"), A_VIEW).expect("a second answer");
    let said = host.load(&hub, "deploy").expect("the rebuild loads");
    assert!(said.contains("its pin held"), "{said}");
    assert!(
        host.layer("deploy").is_some_and(|l| l.trusted),
        "and the layer runs freely again without a `/plugin trust`: {said}"
    );
    let pin = trust
        .pins()
        .into_iter()
        .find(|p| p.name == "deploy")
        .expect("the pin is still in force");
    assert_eq!(
        pin.capabilities,
        Some(Vec::new()),
        "and it still records the powers that were approved"
    );

    // A rebuild that asks for `read`: a new power is a new review, so the pin is gone and `load`
    // names the one command that re-pins it and the capability that is new.
    std::fs::write(dir.join("plugin.toml"), manifest("\"read\"")).expect("a wider manifest");
    let said = host.load(&hub, "deploy").expect("the wider rebuild loads");
    assert!(said.contains("not pinned any more"), "{said}");
    assert!(said.contains("read"), "the capability that is new: {said}");
    assert!(said.contains("/plugin trust deploy"), "{said}");
    assert!(
        !host.layer("deploy").is_some_and(|l| l.trusted),
        "and it is adjudicated call by call until the person pins it again"
    );
}

/// A path that is not there is a refusal naming the path it tried, so a reader is not sent back to
/// guess which tree the host looked in.
#[test]
fn a_read_look_that_is_not_there_names_the_path_it_tried() {
    let root = tempdir("missing-names");
    let missing = look::run(
        &LookSpec::Read {
            read: ".letibot/status.json".into(),
        },
        look::Trees::one(&root),
        &[Capability::Read],
    );
    assert_eq!(missing["exit"], json!(1), "{missing}");
    let said = missing["stderr"].as_str().unwrap_or_default();
    assert!(said.contains("is not there"), "{said}");
    assert!(
        said.contains(&root.join(".letibot/status.json").display().to_string()),
        "the path it tried: {said}"
    );
}

#[test]
fn an_exec_look_answers_126_and_runs_nothing() {
    let root = tempdir("exec");
    let result = look::run(
        &LookSpec::Exec {
            exec: vec!["rm".into(), "-rf".into(), "/".into()],
        },
        look::Trees::one(&root),
        &[Capability::Exec("rm".into())],
    );
    assert_eq!(result["exit"], json!(126), "{result}");
    assert_eq!(result["stderr"], json!(look::EXEC_REFUSED));
}

#[test]
fn a_read_path_tree_is_readable_from_a_pane_look_and_nothing_else_is() {
    // `read` alone is the workspace; `read:<path>` is one tree outside it, and the pane's look
    // runner enforces the same rule the tool's does.
    let workspace = tempdir("readpath-ws");
    let elsewhere = tempdir("readpath-tree");
    let file = elsewhere.join("a.jsonl");
    std::fs::write(&file, "{}").expect("a file in the declared tree");
    let caps = [Capability::ReadPath(elsewhere.display().to_string())];
    let allowed = look::run(
        &LookSpec::Read {
            read: file.display().to_string(),
        },
        look::Trees::one(&workspace),
        &caps,
    );
    assert_eq!(allowed["exit"], json!(0), "{allowed}");

    // A file OUTSIDE the declared tree, which exists - the check is on where a path resolves
    // to, and a path that is merely absent fails earlier and for a different reason.
    let other = tempdir("readpath-out");
    let outside = other.join("not-declared.json");
    std::fs::write(&outside, "{}").expect("a file outside the declared tree");
    let refused = look::run(
        &LookSpec::Read {
            read: outside.display().to_string(),
        },
        look::Trees::one(&workspace),
        &caps,
    );
    assert_eq!(refused["exit"], json!(1), "{refused}");
    assert!(
        refused["stderr"]
            .as_str()
            .unwrap()
            .contains("outside every tree"),
        "{refused}"
    );
}

// The `tool_` seam.

/// A module that answers one scripted answer per call, so a test can stand in the middle of a
/// look round trip: round 0 asks for data, round 1 answers from it.
struct Rounds {
    hooks: Vec<String>,
    answers: Vec<String>,
    at: std::sync::atomic::AtomicUsize,
}

impl Module for Rounds {
    fn hooks(&self) -> Vec<String> {
        self.hooks.clone()
    }

    fn call(&self, _hook: &str, _input: &str) -> Result<Option<String>, String> {
        let n = self.at.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match self.answers.get(n) {
            Some(answer) => Ok(Some(answer.clone())),
            None => Err("the layer answered more times than the test scripted".to_string()),
        }
    }

    fn fuel(&self) -> u64 {
        7
    }
}

/// The loader for [`Rounds`]: the answers come from `answer.json` and then `second.json`, and
/// a `TRAP` file makes the module refuse to answer at all.
struct RoundLoader;

impl Loader for RoundLoader {
    fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        let _ = std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
        let dir = wasm.parent().unwrap_or(Path::new("."));
        let manifest = Manifest::read(dir)?;
        let mut answers = Vec::new();
        for name in ["answer.json", "second.json"] {
            let path = dir.join(name);
            if let Ok(text) = std::fs::read_to_string(&path) {
                answers.push(text);
            }
        }
        Ok(Box::new(Rounds {
            hooks: manifest.hooks.clone(),
            answers,
            at: std::sync::atomic::AtomicUsize::new(0),
        }))
    }
}

/// A project with one layer that declares a tool, and the declarations the prompt would see.
fn tool_project(tag: &str, first: &str, second: &str, caps: &str) -> (PathBuf, Vec<DeclaredTool>) {
    let root = tempdir(tag);
    let layer = root.join(".letibot/plugins/claude-transcript");
    std::fs::create_dir_all(&layer).expect("the layer dir");
    std::fs::write(
        layer.join("plugin.toml"),
        format!(
            "name = \"claude-transcript\"\npurpose = \"read another agent's store\"\n\
             hooks = [\"tool_probe\"]\ncapabilities = [{caps}]\ncontract = 1\n\n\
             [[tools]]\nname = \"probe\"\ndescription = \"read what the layer says\"\n\n\
             schema = {{ type = \"object\", properties = {{}} }}\n"
        ),
    )
    .expect("a manifest");
    std::fs::write(layer.join("plugin.wasm"), b"a module").expect("a module");
    std::fs::write(layer.join("answer.json"), first).expect("an answer");
    std::fs::write(layer.join("second.json"), second).expect("a second answer");
    let declared = Stack::declared_tools(None, Some(&root), None);
    (root, declared)
}

/// The tool, wired to a host built over `root` - the two halves `Harness::open` and
/// `Sessions::open_plugins` put together. The layer is pinned, because an unpinned one is
/// adjudicated on every call and these tests are about the seam, not the ladder.
fn tool_for(
    root: &Path,
    declared: DeclaredTool,
    hub: &Arc<Hub>,
) -> crate::plugins::tool::PluginTool {
    tool_for_with(root, declared, hub, true)
}

/// [`tool_for`], with the pin taken or not - the one argument the ladder's tests vary.
fn tool_for_with(
    root: &Path,
    declared: DeclaredTool,
    hub: &Arc<Hub>,
    pinned: bool,
) -> crate::plugins::tool::PluginTool {
    let trust = letibot_plugins::Trust::in_memory();
    if pinned {
        trust
            .pin(
                &declared.layer,
                &root.join(".letibot/plugins").join(&declared.layer),
            )
            .expect("the layer's bytes are pinned");
    }
    let host = Arc::new(
        PluginHost::discover(
            "s-tool",
            root,
            None,
            None,
            Arc::new(RoundLoader),
            Arc::new(FileBuilder),
        )
        .with_trust(trust),
    );
    let slot: Arc<std::sync::Mutex<Option<Arc<PluginHost>>>> =
        Arc::new(std::sync::Mutex::new(Some(host)));
    crate::plugins::tool::PluginTool::new(declared, "s-tool", root.to_path_buf(), slot, hub.clone())
}

/// A model's write records the sha256 of what it wrote: the tool reports the bytes, and
/// `Trust::note_write` decides whether they are about a layer at all. The hash is checked
/// against the one `Trust::pin` computes from the same file, so record and pin agree.
#[test]
fn a_models_write_records_the_sha256_of_the_bytes_it_wrote() {
    let root = tempdir("provenance");
    let trust = letibot_plugins::Trust::open(root.join("plugin-trust.json"));
    let sink = Provenance::over(trust.clone(), "s-writer");
    let dir = root.join(".letibot/plugins/one");
    std::fs::create_dir_all(&dir).expect("the layer dir");
    let manifest = dir.join("plugin.toml");
    std::fs::write(
        &manifest,
        "name = \"one\"\npurpose = \"p\"\nhooks = [\"pane_x\"]\ncapabilities = []\ncontract = 1\n",
    )
    .expect("a manifest");
    let bytes = std::fs::read(&manifest).expect("the bytes");
    let wrote = {
        use letibot_tools::provenance::Sink as _;
        sink.wrote(&manifest, &bytes);
        // A file that is not about a layer is not provenance about one, and a book of every
        // write on the box would bury the records that matter.
        sink.wrote(&root.join("README.md"), b"# not a layer");
        trust.writes_for(&dir)
    };
    assert_eq!(
        wrote.len(),
        1,
        "one record, for the layer's own file: {wrote:?}"
    );
    assert_eq!(wrote[0].session, "s-writer");
    let pin = trust.pin("one", &dir).expect("a pin");
    assert_eq!(
        wrote[0].sha256, pin.manifest,
        "the record and the pin name the same bytes"
    );
    // And it is a fact on disk: a second book over the same file reads it back.
    let reopened = letibot_plugins::Trust::open(trust.path().unwrap());
    assert_eq!(
        reopened.writes().len(),
        1,
        "the record survives the process"
    );
}

/// A layer nobody pinned is adjudicated on every call. The subject is the call itself, and it is
/// `admit_unasked`, so a layer can never put a question to a person; the decider stands in for
/// the gate, and what is under test is *did this tool ask before the layer answered?*
#[test]
fn a_layer_nobody_pinned_is_adjudicated_on_every_call() {
    struct Says(GateDecision, std::sync::Mutex<Vec<String>>);
    impl crate::plugins::tool::Decides for Says {
        fn admit_unasked(&self, name: &str, access: Access, _args: &Value) -> Option<GateDecision> {
            self.1.lock().unwrap().push(format!("{name}/{access:?}"));
            Some(match &self.0 {
                GateDecision::Admit => GateDecision::Admit,
                GateDecision::Refuse { outcome, tell } => GateDecision::Refuse {
                    outcome: outcome.clone(),
                    tell: tell.clone(),
                },
            })
        }
    }

    let (root, declared) = tool_project(
        "tool-unpinned",
        "{\"outcome\":\"ok\",\"payload\":\"answered\"}",
        "{}",
        "",
    );
    let hub = Hub::new("s-tool");
    let tool = tool_for_with(&root, declared[0].clone(), &hub, false);

    // Admitted by the ladder, and only then does the layer answer.
    let admits = Says(GateDecision::Admit, std::sync::Mutex::new(Vec::new()));
    let out = tool.call(Some(&admits), &json!({}));
    assert_eq!(out.payload, "answered", "the ladder admitted it");
    assert_eq!(
        *admits.1.lock().unwrap(),
        vec!["probe/Read".to_string()],
        "the gate was asked once, about this tool"
    );
    // Every call, not the first: a layer whose bytes nobody pinned is decided again each time
    // it is asked for something.
    let _ = tool.call(Some(&admits), &json!({}));
    assert_eq!(admits.1.lock().unwrap().len(), 2);

    // Refused by the ladder, and the refusal names the layer and both hashes.
    let refuses = Says(
        GateDecision::Refuse {
            outcome: letibot_transcript::ToolOutcome::NotRun {
                why: "it would need your approval".into(),
            },
            tell: String::new(),
        },
        std::sync::Mutex::new(Vec::new()),
    );
    let out = tool.call(Some(&refuses), &json!({}));
    match &out.outcome {
        letibot_transcript::ToolOutcome::NotRun { why } => {
            assert!(why.contains("would need your approval"), "{why}")
        }
        other => panic!("a refused layer is `not_run`, not {other:?}"),
    }
    assert!(
        out.payload.contains("`claude-transcript`"),
        "{}",
        out.payload
    );
    assert!(out.payload.contains("not pinned"), "{}", out.payload);
    assert!(out.payload.contains("sha256"), "{}", out.payload);

    // And with no gate at all, it does not run either - `run` is the caller that has none, and
    // a layer nobody pinned runs only where the ladder can be asked.
    let out = tool.run(&json!({}));
    assert!(matches!(
        out.outcome,
        letibot_transcript::ToolOutcome::NotRun { .. }
    ));
    assert!(out.payload.contains("no gate"), "{}", out.payload);
}

/// A pinned layer runs freely - the gate is never asked - and a byte change after the pin
/// un-pins it: the refusal names the layer and both hashes, and the session is told once.
#[test]
fn a_pinned_layer_runs_freely_and_a_changed_one_is_refused_by_name() {
    struct Never;
    impl crate::plugins::tool::Decides for Never {
        fn admit_unasked(&self, _: &str, _: Access, _: &Value) -> Option<GateDecision> {
            panic!("a pinned layer runs freely: the gate is never asked")
        }
    }

    let (root, declared) = tool_project(
        "tool-pinned",
        "{\"outcome\":\"ok\",\"payload\":\"answered\"}",
        "{}",
        "",
    );
    let hub = Hub::new("s-tool");
    let trust = letibot_plugins::Trust::in_memory();
    let dir = root.join(".letibot/plugins/claude-transcript");
    let pin = trust.pin("claude-transcript", &dir).expect("a pin");
    let host = Arc::new(
        PluginHost::discover(
            "s-tool",
            &root,
            None,
            None,
            Arc::new(RoundLoader),
            Arc::new(FileBuilder),
        )
        .with_trust(trust.clone()),
    );
    let slot: Arc<std::sync::Mutex<Option<Arc<PluginHost>>>> =
        Arc::new(std::sync::Mutex::new(Some(host.clone())));
    let tool = crate::plugins::tool::PluginTool::new(
        declared[0].clone(),
        "s-tool",
        root.clone(),
        slot,
        hub.clone(),
    );
    let out = tool.call(Some(&Never), &json!({}));
    assert_eq!(out.payload, "answered", "a pinned layer runs freely");

    // The bytes move: the module is rebuilt, which is the shape the rule exists for. The stack
    // picks it up as the daemon does - the poll sees it, the gate applies it - and the pin is
    // decided again against what was applied.
    std::fs::write(dir.join("plugin.wasm"), b"a module, rebuilt").expect("a rebuild");
    let pending = host.lock().stack.poll_changes();
    assert_eq!(pending.len(), 1, "the rebuild is a candidate");
    host.lock()
        .stack
        .apply(pending.into_iter().next().unwrap())
        .expect("the rebuilt layer applies");
    let news = host.lock().stack.refresh_trust();
    assert_eq!(news.len(), 1, "the pin broke: {news:?}");
    assert_eq!(news[0].name, "claude-transcript");
    // The layer is adjudicated again, so the decider that panics for a free layer is replaced
    // by one that refuses - which is what an unpinned layer's call meets.
    struct Refuses;
    impl crate::plugins::tool::Decides for Refuses {
        fn admit_unasked(&self, _: &str, _: Access, _: &Value) -> Option<GateDecision> {
            Some(GateDecision::Refuse {
                outcome: letibot_transcript::ToolOutcome::NotRun {
                    why: "it would need your approval".into(),
                },
                tell: String::new(),
            })
        }
    }
    let out = tool.call(Some(&Refuses), &json!({}));
    assert!(
        matches!(out.outcome, letibot_transcript::ToolOutcome::NotRun { .. }),
        "{:?}",
        out.outcome
    );
    assert!(
        out.payload.contains(&pin.module),
        "the pinned hash: {}",
        out.payload
    );
    assert!(
        out.payload.contains("not pinned any more"),
        "{}",
        out.payload
    );
}

#[test]
fn a_declared_tool_is_seated_and_answers_a_result_through_the_stack() {
    let (root, declared) = tool_project(
        "tool-ok",
        "{\"outcome\":\"ok\",\"payload\":\"the layer's own words\"}",
        "{}",
        "",
    );
    let hub = Hub::new("s-tool");
    let tool = tool_for(&root, declared[0].clone(), &hub);
    // What the model sees is the manifest's declaration, and nothing else.
    let schema = letibot_tools::runtime::Tool::schema(&tool);
    assert_eq!(schema.name, "probe");
    assert_eq!(schema.description, "read what the layer says");
    assert_eq!(schema.parameters["type"], json!("object"));
    let out = tool.run(&json!({}));
    assert_eq!(
        out.outcome,
        letibot_tools::runtime::Invocation::ok("").outcome
    );
    assert_eq!(out.payload, "the layer's own words");
    // And the result says who answered it, so a model can tell a layer's words from a tool's.
    assert!(
        out.notes.iter().any(|n| n.contains("answered by plugin")),
        "{:?}",
        out.notes
    );
}

#[test]
fn a_tool_asks_for_a_look_and_the_host_runs_it_against_the_declared_tree() {
    let elsewhere = tempdir("tool-tree");
    let secret = elsewhere.join("a.jsonl");
    std::fs::write(&secret, "{\"line\":1}\n{\"line\":2}\n").expect("a file");
    let first = json!({
        "looks": [{"read": secret.display().to_string(), "lines": 1}],
        "state": {"step": 1},
    })
    .to_string();
    // The second answer echoes what it was handed, so the test can read the look's result.
    let second = "{\"outcome\":\"ok\",\"payload\":\"<ECHO>\"}";
    let caps = format!("\"read:{}\"", elsewhere.display());
    let (root, declared) = tool_project("tool-look", &first, second, &caps);
    let hub = Hub::new("s-tool");
    let tool = tool_for(&root, declared[0].clone(), &hub);
    // The echo is not a thing a guest can do, so the assertion is on the recorder: the stack
    // kept the input of the second call, which carries the look's result.
    let out = tool.run(&json!({}));
    assert_eq!(
        out.outcome,
        letibot_tools::runtime::Invocation::ok("").outcome
    );
    assert!(out.payload.contains("<ECHO>"), "{}", out.payload);
}

#[test]
fn a_look_outside_the_declared_tree_is_refused_by_the_host_not_by_the_layer() {
    let (root, declared) = tool_project(
        "tool-outside",
        "{\"looks\": [{\"read\": \"/etc/hosts\"}]}",
        "{\"outcome\":\"ok\",\"payload\":\"second\"}",
        "",
    );
    // No capability at all: the layer may look at nothing, and the host is what says so.
    let looked = crate::plugins::tool::run_look(
        &json!({"read": "/etc/hosts"}),
        &declared[0].capabilities,
        &root,
    );
    assert_eq!(looked["exit"], json!(1), "{looked}");
    assert!(
        looked["error"]
            .as_str()
            .unwrap()
            .contains("declares no `read` capability"),
        "{looked}"
    );
    // And a tree it DID declare is readable, which is the other half of the same rule.
    let tree = tempdir("tool-declared");
    let file = tree.join("a.jsonl");
    std::fs::write(&file, "one\ntwo\n").expect("a file");
    let caps = [Capability::ReadPath(tree.display().to_string())];
    let ok = crate::plugins::tool::run_look(
        &json!({"read": file.display().to_string(), "from": 2, "lines": 1}),
        &caps,
        &root,
    );
    assert_eq!(ok["exit"], json!(0), "{ok}");
    assert_eq!(ok["lines"][0]["text"], json!("two"));
    assert_eq!(ok["lines"][0]["n"], json!(2));
}

#[test]
fn a_symlink_inside_the_declared_tree_is_refused_for_where_it_points() {
    // `~/.claude/projects` really holds one of these (`memory` → a folder under `~/Projects`)
    // and the check is on the RESOLVED path for exactly this reason.
    let tree = tempdir("tool-symlink");
    let outside = tempdir("tool-symlink-out");
    let secret = outside.join("secret.txt");
    std::fs::write(&secret, "not for this layer").expect("a file outside");
    let link = tree.join("memory");
    std::os::unix::fs::symlink(&outside, &link).expect("a symlink");
    let caps = [Capability::ReadPath(tree.display().to_string())];
    let looked = crate::plugins::tool::run_look(
        &json!({"read": link.join("secret.txt").display().to_string()}),
        &caps,
        &tempdir("tool-symlink-ws"),
    );
    assert_eq!(looked["exit"], json!(1), "{looked}");
    assert!(
        looked["error"]
            .as_str()
            .unwrap()
            .contains("outside every tree"),
        "{looked}"
    );
}

#[test]
fn a_layer_that_traps_or_answers_nonsense_is_a_failed_tool_not_a_dead_session() {
    // Nonsense: an object with neither `outcome` nor `looks`.
    let (root, declared) = tool_project("tool-nonsense", "{\"hello\":\"world\"}", "{}", "");
    let hub = Hub::new("s-tool");
    let out = tool_for(&root, declared[0].clone(), &hub).run(&json!({}));
    assert!(matches!(
        out.outcome,
        letibot_transcript::ToolOutcome::Failed { .. }
    ));
    assert!(
        out.payload.contains("neither `outcome` nor `looks`"),
        "{}",
        out.payload
    );

    // An outcome nobody defined.
    let (root, declared) = tool_project(
        "tool-outcome",
        "{\"outcome\":\"probably\",\"payload\":\"x\"}",
        "{}",
        "",
    );
    let hub = Hub::new("s-tool");
    let out = tool_for(&root, declared[0].clone(), &hub).run(&json!({}));
    assert!(out.payload.contains("the outcomes are"), "{}", out.payload);
}

#[test]
fn a_session_with_no_plugin_host_says_so_rather_than_failing_the_turn() {
    let (root, declared) = tool_project("tool-nohost", "{}", "{}", "");
    let slot: Arc<std::sync::Mutex<Option<Arc<PluginHost>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let tool = crate::plugins::tool::PluginTool::new(
        declared[0].clone(),
        "s-sub",
        root,
        slot,
        Hub::new("s-sub"),
    );
    let out = tool.run(&json!({}));
    match &out.outcome {
        letibot_transcript::ToolOutcome::NotRun { why } => {
            assert!(why.contains("no plugin host"), "{why}");
        }
        other => panic!("a session with no host is `not_run`, not {other:?}"),
    }
    // And the detail names what would fix it, which is the sentence a model acts on.
    assert!(out.payload.contains("parent's runner"), "{}", out.payload);
}

#[test]
fn the_builtin_transcript_layer_is_on_the_daemons_stack_and_answers_its_hook() {
    // The whole seam with the real module: the project installs the binary's own
    // `plugin.toml` - that declaration is the ask - the host puts the compiled built-in at the
    // bottom of the stack, and rano loads it.
    let workspace = tempdir("builtin-tool");
    let builtin = letibot_builtins::table()
        .iter()
        .find(|b| b.name == "claude-transcript")
        .expect("the binary carries the transcript layer");
    let layer = workspace.join(".letibot/plugins/claude-transcript");
    std::fs::create_dir_all(&layer).expect("the layer dir");
    // The manifest and nothing else: the module is the one the binary already carries, which is
    // the whole point of a built-in layer.
    std::fs::write(layer.join("plugin.toml"), builtin.manifest).expect("the manifest");
    let host = PluginHost::production("s-tool", &workspace);
    // And the documented install is clean: a `plugin.toml` with no module, because the module
    // that answers is the one the binary carries. A missing module here is not a layer that is
    // exploding, and saying so once made every session in the project carry the warning.
    {
        let inner = host.lock();
        assert!(
            inner.stack.failures().is_empty(),
            "a manifest with no module is a declaration, not a failure: {:?}",
            inner.stack.failures()
        );
    }
    // It is a tool and not a pane, so nothing of the layer's is announced to a head. The room's
    // pane is not here either: this host has no room feed, and that layer is installed lazily
    // at the door a head comes through, because a module costs seconds to compile.
    assert!(host.panes().is_empty(), "{:?}", host.panes());
    let hub = Hub::new("s-tool");
    host.announce(&hub);
    assert!(
        !hub.retained().iter().any(
            |e| matches!(&e.event, SessionEvent::Warning { code, .. } if code == "plugin_layer")
        ),
        "a correct install says nothing on the session's log: {:?}",
        hub.retained()
    );
    assert!(
        host.take_notices().is_empty(),
        "and nothing is queued for the model"
    );
    let asked = host.ask(
        &hub,
        "tool_claude_transcript",
        &json!({
            "v": 1,
            "name": "claude_transcript",
            "args": {"what": "sessions"},
            "feeds": {"home": "/nonexistent-home", "workspace": "/nonexistent-project", "now_ms": 0},
            "looks": [],
            "state": null,
        }),
    );
    let resolved = asked.expect("the built-in answers `tool_claude_transcript`");
    assert!(
        resolved.failures.is_empty(),
        "a built-in must not fail its own hook: {:?}",
        resolved.failures
    );
    let answer = resolved.answer.expect("an answer");
    // Round zero asks the host for a listing - the guest does no I/O, so the only correct
    // first answer is a look, and that is the contract working end to end.
    assert_eq!(
        answer["looks"][0]["list"],
        json!("/nonexistent-home/.claude/projects/-nonexistent-project"),
        "{answer}"
    );
}

/// A session that declared no tool from a layer gets no built-in layer at all - the half of the
/// test above that costs CPU. Compiling the binary's tool layer is ~2 s in a debug host, and a
/// session that seats no layer tool cannot call it: the `PluginTool` is what a declaration seats.
#[test]
fn a_session_that_declared_no_tool_installs_no_builtin_layer() {
    let workspace = tempdir("builtin-none");
    let host = PluginHost::production("s-nobody", &workspace);
    assert!(
        host.ask(
            &Hub::new("s-nobody"),
            "tool_claude_transcript",
            &json!({"v": 1, "args": {}, "looks": [], "state": null}),
        )
        .is_none(),
        "nothing asked for the tool, so no layer of the binary's is on this stack"
    );
}

/// Every built-in pane is on a head's list and opens by its name with no tool declared: the wall was
/// that a built-in was installed only when a session declared a tool from it, so `irc` and `github`
/// answered a pane nothing could reach (R151).
#[test]
fn every_builtin_pane_is_listed_and_opens_with_no_tool_declared() {
    let workspace = tempdir("builtin-panes");
    let host = Arc::new(PluginHost::production("s-panes", &workspace));
    let hub = Hub::new("s-panes");
    assert!(
        host.panes().is_empty(),
        "no head, no compile: {:?}",
        host.panes()
    );
    PaneDriver::attach(&*host, "s-panes", &hub);
    // The attach only asks; the compile runs on the next tick's thread, so the head never waits.
    assert!(
        host.next_at().is_some(),
        "a wanted compile puts the host on the clock"
    );
    Arc::clone(&host).tick(Arc::clone(&hub));
    // Read off the binary's own table, not written down here: a second list drifts.
    let want: Vec<String> = letibot_builtins::table()
        .iter()
        .filter_map(|b| Manifest::parse(b.manifest).ok())
        .flat_map(|m| {
            m.hooks
                .iter()
                .filter_map(|h| {
                    h.strip_prefix("pane_")
                        .filter(|n| !n.is_empty())
                        .map(|n| format!("{}/{n}", m.name))
                })
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(want.len() >= 2, "the binary carries panes: {want:?}");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !want.iter().all(|p| host.panes().contains(p)) && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let listed = host.panes();
    for pane in &want {
        assert!(
            listed.contains(pane),
            "`{pane}` is a built-in pane and no head lists it: {listed:?}"
        );
    }
    // The two that were unreachable open by their names, and the LAYER answers: no feed is its own
    // sentence on the screen, not the host's refusal panel.
    for pane in ["irc/nicks", "github/github"] {
        host.open("s-panes", &hub, pane, (100, 30));
        let (_, view) = views(&hub)
            .into_iter()
            .rfind(|(p, _)| p == pane)
            .unwrap_or_else(|| panic!("`{pane}` answered nothing"));
        assert!(
            view["panels"][0].get("unavailable").is_none(),
            "`{pane}`: {view}"
        );
    }
}

#[test]
fn a_grep_look_answers_line_numbers_byte_offsets_and_bounded_context() {
    let root = tempdir("grep");
    let file = root.join("t.jsonl");
    std::fs::write(&file, "one\ntwo\nTHREE\nfour\nfive\nsix\n").expect("a file");
    let caps = [Capability::ReadPath(root.display().to_string())];
    let looked = crate::plugins::tool::run_look(
        &json!({"grep": file.display().to_string(), "match": "three", "context": 1, "limit": 5}),
        &caps,
        &root,
    );
    assert_eq!(looked["exit"], json!(0), "{looked}");
    let hit = &looked["matches"][0];
    assert_eq!(hit["line"], json!(3));
    // Byte 8 is where the third line starts: `one\n` + `two\n` is eight bytes.
    assert_eq!(hit["byte"], json!(8));
    assert_eq!(hit["text"], json!("THREE"));
    assert_eq!(hit["before"][0]["text"], json!("two"));
    assert_eq!(hit["after"][0]["text"], json!("four"));
    // The count of what was read is what makes a miss distinguishable from a failure.
    assert_eq!(looked["scanned"], json!(6));
}

#[test]
fn a_list_look_answers_names_sizes_and_mtimes_and_a_file_is_not_a_directory() {
    let root = tempdir("list");
    std::fs::write(root.join("a.jsonl"), "0123456789").expect("a file");
    std::fs::create_dir_all(root.join("subagents")).expect("a directory");
    let caps = [Capability::ReadPath(root.display().to_string())];
    let looked =
        crate::plugins::tool::run_look(&json!({"list": root.display().to_string()}), &caps, &root);
    assert_eq!(looked["exit"], json!(0), "{looked}");
    let names: Vec<&str> = looked["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert_eq!(names, vec!["a.jsonl", "subagents"]);
    let file = &looked["entries"][0];
    assert_eq!(file["size"], json!(10));
    assert_eq!(file["dir"], json!(false));
    assert!(file["mtime"].as_u64().unwrap() > 0);

    let not_a_dir = crate::plugins::tool::run_look(
        &json!({"list": root.join("a.jsonl").display().to_string()}),
        &caps,
        &root,
    );
    assert_eq!(not_a_dir["exit"], json!(1), "{not_a_dir}");
}

#[test]
fn a_view_the_host_cannot_validate_becomes_one_panel_saying_why() {
    // `q` is one of the host's own keys (C3), so `View::validate` refuses this view.
    let bad = r#"{"title":"bad","panels":[{"id":"plan","key":"q","title":"plan"}]}"#;
    let (root, hub) = project("refused", bad);
    let host = host(&root);
    host.announce(&hub);
    let seen = views(&hub);
    assert_eq!(seen.len(), 1, "{seen:?}");
    let view: View = serde_json::from_value(seen[0].1.clone()).expect("a view");
    view.validate().expect("the host's own answer validates");
    let unavailable = view.panels[0]
        .unavailable
        .as_ref()
        .expect("one panel, and it is unavailable");
    assert!(unavailable.why.contains('q'), "{}", unavailable.why);
    assert_eq!(
        unavailable.fix,
        "the layer p1 answered something the host refused"
    );
}

#[test]
fn an_answer_that_is_not_a_view_is_the_same_unavailable_panel() {
    let (root, hub) = project("notaview", "this is not JSON at all");
    let host = host(&root);
    host.announce(&hub);
    let seen = views(&hub);
    assert_eq!(seen.len(), 1, "{seen:?}");
    let view: View = serde_json::from_value(seen[0].1.clone()).expect("a view");
    assert!(view.panels[0].unavailable.is_some());
}

#[test]
fn a_pane_that_asks_for_a_look_is_asked_again_with_its_result() {
    let (root, hub) = project("looks", A_VIEW);
    std::fs::create_dir_all(root.join(".letibot")).expect("the status dir");
    std::fs::write(
        root.join(".letibot/status.json"),
        "{\"release\":{\"name\":\"v0.2.0\"}}",
    )
    .expect("a status file");
    let layer = root.join(".letibot/plugins/plan-dashboard");
    // The two frames the real example answers: the first asks for the file and draws nothing
    // (`every_s` 0), the second is the dashboard and asks to be refreshed every 30 s.
    std::fs::write(
        layer.join("answer.json"),
        r#"{"title":"plan","panels":[{"id":"plan","key":"p","title":"plan"}],
            "look":{"read":".letibot/status.json"},"every_s":0}"#,
    )
    .expect("the frame that asks");
    std::fs::write(
        layer.join("second.json"),
        r#"{"title":"plan · v0.2.0","panels":[{"id":"tiers","key":"t","title":"tiers"}],
            "look":{"read":".letibot/status.json"},"every_s":30}"#,
    )
    .expect("the dashboard");
    let host = host(&root);
    host.render(&hub, "plan-dashboard/dashboard", Req::fresh());
    let seen = views(&hub);
    assert_eq!(
        seen.len(),
        2,
        "the frame that asks and the one after the look: {seen:?}"
    );
    assert_eq!(seen[0].1["title"], json!("plan"));
    assert_eq!(seen[1].1["title"], json!("plan · v0.2.0"));
    // And the pane is now on the refresh clock, from the answer that asked for 30 s.
    let inner = host.lock();
    let open = inner
        .panes
        .get("plan-dashboard/dashboard")
        .expect("the pane is open");
    assert_eq!(open.every_s, 30);
    assert!(open.next_at.is_some());
}

/// A refused candidate leaves the old view alone; a passing one changes it - through the same
/// `Stack::apply` the daemon uses.
#[test]
fn a_refused_candidate_keeps_the_old_view_and_a_passing_one_changes_it() {
    let (root, hub) = project("gate", A_VIEW);
    let host = host(&root);
    let layer = root.join(".letibot/plugins/plan-dashboard");
    host.render(&hub, "plan-dashboard/dashboard", Req::fresh());
    assert_eq!(views(&hub).last().unwrap().1["title"], json!("one"));

    // A candidate whose build fails: `FAIL` stops the gate at step 1, and the module changed
    // so the poll has something to hand it.
    std::fs::write(layer.join("FAIL"), b"").expect("a failing build");
    std::fs::write(
        layer.join("next.json"),
        r#"{"title":"two","panels":[{"id":"plan","key":"t","title":"plan"}]}"#,
    )
    .expect("a next answer");
    std::fs::write(layer.join("plugin.wasm"), b"a changed module").expect("a module");
    assert_eq!(host.gate_now(&hub), 0, "nothing was applied");
    assert_eq!(
        views(&hub).last().unwrap().1["title"],
        json!("one"),
        "the version that answers is the one before the refusal"
    );
    let noticed = host.take_notices();
    assert!(
        noticed
            .iter()
            .any(|n| n.contains("was refused by the shadow gate") && n.contains("p1")),
        "{noticed:?}"
    );

    // And now a candidate that passes: it says something else, and the open pane redraws.
    std::fs::remove_file(layer.join("FAIL")).expect("the build works again");
    std::fs::write(layer.join("plugin.wasm"), b"a third module").expect("a module");
    assert_eq!(host.gate_now(&hub), 1, "the candidate was applied");
    assert_eq!(
        views(&hub).last().unwrap().1["title"],
        json!("two"),
        "the open pane was rendered again from the new layer"
    );
}

/// The recorder is what the gate replays, so a pane call has to land in it - and the pane has
/// to see the `state` it returned last time.
#[test]
fn a_pane_hands_its_state_back_and_the_input_is_recorded() {
    let answer =
        r#"{"title":"one","state":{"n":2},"panels":[{"id":"plan","key":"t","title":"plan"}]}"#;
    let (root, hub) = project("state", answer);
    let host = host(&root);
    host.render(&hub, "plan-dashboard/dashboard", Req::fresh());
    host.render(&hub, "plan-dashboard/dashboard", Req::fresh());
    let inner = host.lock();
    let inputs = inner.rec.inputs("pane_dashboard");
    assert_eq!(inputs.len(), 2);
    assert_eq!(
        inputs[0]["state"],
        Value::Null,
        "the first call knows nothing"
    );
    assert_eq!(inputs[1]["state"], json!({"n": 2}), "the second is told");
    // The contract version is the SDK's own number, not a copy: a hardcoded `1` outlived the
    // contract's growth to 2, so a layer that read `v` was told no feed by the host handing it.
    assert_eq!(inputs[1]["v"], json!(letibot_plugin::pane::V));
    assert_eq!(inputs[1]["size"], json!({"w": SIZE_W, "h": SIZE_H}));
    assert!(inner.panes.contains_key("plan-dashboard/dashboard"));
}

/// A stop's action reaches the pane - read off the input the host recorded, because that input
/// IS what the plugin was handed.
#[test]
fn a_pane_is_handed_the_event_that_was_pressed() {
    let (root, hub) = project("event", A_VIEW);
    let host = host(&root);
    host.render(
        &hub,
        "plan-dashboard/dashboard",
        Req {
            look: None,
            event: Some(("tier1/jstests".to_string(), "expand".to_string())),
            may_look: false,
        },
    );
    let last = views(&hub).last().cloned().expect("a view");
    assert_eq!(last.1["title"], json!("one"));
    let inner = host.lock();
    let input = inner
        .rec
        .inputs("pane_dashboard")
        .last()
        .expect("the call was recorded");
    assert_eq!(input["event"]["item"], json!("tier1/jstests"));
    assert_eq!(input["event"]["action"], json!("expand"));
}

/// A hook no layer answers is silence, not a blank view: `PluginPaneGone` closes a pane that
/// was open, while nothing at all is a pane a head has never heard of.
#[test]
fn a_pane_nobody_answers_publishes_nothing() {
    let (root, hub) = project("gone", A_VIEW);
    let host = host(&root);
    host.render(&hub, "plan-dashboard/never", Req::fresh());
    assert!(
        views(&hub).is_empty(),
        "a hook nobody exports is not a pane with a view"
    );
}

/// A module that answers its own input's `size`, which is the only way a test can see from
/// outside that the head's rectangle reached the plugin. The title is free text, so the
/// rectangle goes there.
struct SizeModule;

impl Module for SizeModule {
    fn hooks(&self) -> Vec<String> {
        vec!["pane_dashboard".to_string()]
    }

    fn call(&self, _hook: &str, input: &str) -> Result<Option<String>, String> {
        let input: Value = serde_json::from_str(input).map_err(|e| e.to_string())?;
        Ok(Some(
            json!({
                "title": format!("{}\u{d7}{}", input["size"]["w"], input["size"]["h"]),
                "panels": [{"id": "plan", "key": "t", "title": "plan"}],
            })
            .to_string(),
        ))
    }

    fn fuel(&self) -> u64 {
        7
    }
}

struct SizeLoader;

impl Loader for SizeLoader {
    fn load(&self, _wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        Ok(Box::new(SizeModule))
    }
}

/// The rectangle the head is drawing the pane in reaches the plugin. `PaneOpen`'s `w`/`h` exist
/// because the daemon has no screen: a dashboard laid out for 120×40 in a 70-column pane wraps
/// every label for a terminal nobody is looking at.
#[test]
fn the_rectangle_a_head_sends_is_the_one_the_pane_is_asked_at() {
    let (root, hub) = project("size", A_VIEW);
    let host = PluginHost::discover(
        "s-plugins",
        &root,
        None,
        None,
        Arc::new(SizeLoader),
        Arc::new(FileBuilder),
    );
    host.open("s-plugins", &hub, "plan-dashboard/dashboard", (77, 21));
    assert_eq!(
        views(&hub).last().map(|(_, v)| v["title"].clone()),
        Some(json!("77\u{d7}21")),
        "the plugin was handed the head's rectangle"
    );
    // A resize is the same frame again with the new rectangle: one way to say *draw it at this
    // size*, whether it is the first time or the fifth.
    host.open("s-plugins", &hub, "plan-dashboard/dashboard", (100, 30));
    assert_eq!(
        views(&hub).last().map(|(_, v)| v["title"].clone()),
        Some(json!("100\u{d7}30"))
    );
    // A head older than protocol 51 sends neither: `0` is *the head did not say*, which asks
    // for the host's default rather than a pane zero columns wide.
    host.open("s-plugins", &hub, "plan-dashboard/dashboard", (0, 0));
    assert_eq!(
        views(&hub).last().map(|(_, v)| v["title"].clone()),
        Some(json!(format!("{SIZE_W}\u{d7}{SIZE_H}")))
    );
}

/// A pane the head closed comes off the refresh clock. Without it the host believes somebody
/// is looking at it for the rest of the session and asks it again every `every_s`: a wasm call
/// and a look per interval, for a screen nobody has.
#[test]
fn a_closed_pane_is_not_refreshed_again() {
    let (root, hub) = project("close", A_VIEW);
    let layer = root.join(".letibot/plugins/plan-dashboard");
    // On the clock: `every_s` 5 is C3's floor.
    std::fs::write(
        layer.join("answer.json"),
        r#"{"title":"one","panels":[{"id":"plan","key":"t","title":"plan"}],
            "every_s":5}"#,
    )
    .expect("an answer on the clock");
    let host = Arc::new(host(&root));
    let pane = "plan-dashboard/dashboard";
    host.open("s-plugins", &hub, pane, (80, 24));
    assert!(host.lock().panes.contains_key(pane), "the pane is open");
    // Due now, rather than waiting five seconds for it.
    host.lock().panes.get_mut(pane).expect("open").next_at =
        Some(Instant::now() - Duration::from_secs(1));
    let before = views(&hub).len();
    host.clone().tick(hub.clone());
    assert_eq!(
        views(&hub).len(),
        before + 1,
        "an open pane whose clock is up is asked again"
    );

    host.close("s-plugins", pane);
    assert!(
        !host.lock().panes.contains_key(pane),
        "a closed pane is not held"
    );
    let after = views(&hub).len();
    host.clone().tick(hub.clone());
    assert_eq!(
        views(&hub).len(),
        after,
        "and a closed pane is not asked again: {}",
        views(&hub).len()
    );
    // Closing twice, or a pane this session never had, is quiet rather than an error.
    host.close("s-plugins", pane);
    host.close("s-plugins", "plan-dashboard/never");
    assert_eq!(views(&hub).len(), after);
}

// The hook fixture: a layer that answers any hook from a table. The pane fixture above answers
// one hook from one file, which is enough for a pane; a hook kind is resolved by name, so the
// answers live in a table keyed by (layer directory, hook).

/// What one hook of one layer says - one answer per call, the last one repeating, so a layer
/// can ask below on its first call and answer on the second. `None` is a trap.
type Script = Vec<Option<Value>>;

#[derive(Clone, Default)]
struct Table(Arc<Mutex<BTreeMap<String, BTreeMap<String, Script>>>>);

/// The id a `&str` hook name gets in the table.
fn key(hook: &str) -> String {
    hook.to_string()
}

impl Table {
    fn new() -> Table {
        Table::default()
    }

    /// Write one layer: its manifest, a module, and its answers.
    fn layer(self: &Table, root: &Path, name: &str, hooks: &[(&str, Script)]) -> PathBuf {
        self.layer_with_caps(root, name, &[], hooks)
    }

    /// The same layer with capabilities declared - the only thing enforcement can be asked
    /// about, since a look is checked against this list.
    fn layer_with_caps(
        self: &Table,
        root: &Path,
        name: &str,
        caps: &[&str],
        hooks: &[(&str, Script)],
    ) -> PathBuf {
        let dir = root.join(".letibot/plugins").join(name);
        std::fs::create_dir_all(&dir).expect("the layer dir");
        let declared = hooks
            .iter()
            .map(|(h, _)| format!("\"{h}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let capabilities = caps
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            dir.join("plugin.toml"),
            format!(
                "name = \"{name}\"\npurpose = \"one hook, answered from a table\"\n\
                 hooks = [{declared}]\ncapabilities = [{capabilities}]\ncontract = 1\n"
            ),
        )
        .expect("a manifest");
        std::fs::write(dir.join("plugin.wasm"), b"a module").expect("a module");
        let mut table = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let entry = table.entry(name.to_string()).or_default();
        for (hook, script) in hooks {
            entry.insert(key(hook), script.clone());
        }
        dir
    }
}

struct TableModule {
    hooks: Vec<String>,
    answers: BTreeMap<String, Script>,
    calls: Mutex<BTreeMap<String, usize>>,
}

impl Module for TableModule {
    fn hooks(&self) -> Vec<String> {
        self.hooks.clone()
    }

    fn call(&self, hook: &str, _input: &str) -> Result<Option<String>, String> {
        let Some(script) = self.answers.get(hook) else {
            return Ok(None);
        };
        let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        let at = calls.entry(hook.to_string()).or_insert(0);
        let answer = script.get(*at).or_else(|| script.last()).cloned();
        *at += 1;
        match answer {
            None => Ok(None),
            Some(None) => Err("trapped: error while executing at wasm backtrace".to_string()),
            Some(Some(value)) => Ok(Some(value.to_string())),
        }
    }

    fn fuel(&self) -> u64 {
        11
    }
}

struct TableLoader {
    table: Table,
}

impl Loader for TableLoader {
    fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        let dir = wasm
            .parent()
            .ok_or_else(|| format!("{}: no directory", wasm.display()))?;
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let answers = self
            .table
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&name)
            .cloned()
            .ok_or_else(|| format!("the table has no answers for `{name}`"))?;
        Ok(Box::new(TableModule {
            hooks: answers.keys().cloned().collect(),
            answers,
            calls: Mutex::new(BTreeMap::new()),
        }))
    }
}

/// A host over a table's layers, with no builder use in these tests (nothing is loaded
/// through the gate here).
fn table_host(root: &Path, table: &Table) -> PluginHost {
    PluginHost::discover(
        "s-plugins",
        root,
        None,
        None,
        Arc::new(TableLoader {
            table: table.clone(),
        }),
        Arc::new(FileBuilder),
    )
}

/// A one-answer script.
fn says(answer: Value) -> Script {
    vec![Some(answer)]
}

/// A layer rewords a notice, and the model reads its sentence.
#[test]
fn a_layer_rewords_a_notice() {
    let root = tempdir("notice-reword");
    let table = Table::new();
    table.layer(
        &root,
        "coderabbit-wording",
        &[(
            "notice_monitor",
            says(json!({"text": "[monitor] CodeRabbit reviewed o/r#24: 3 comments — read them"})),
        )],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let said = host.notice(&hub, "monitor", "[monitor] `pr-24` fired: …", json!({}));
    assert_eq!(
        said.as_deref(),
        Some("[monitor] CodeRabbit reviewed o/r#24: 3 comments — read them")
    );
    // And the input it was handed is the host's own shape, with the kind on it - the same
    // document `plugin_hooks` prints.
    let seen = host.lock().rec.inputs("notice_monitor").to_vec();
    assert_eq!(seen.len(), 1, "the input is recorded for the gate's replay");
    assert_eq!(seen[0]["kind"], json!("monitor"));
    assert_eq!(seen[0]["v"], json!(letibot_plugin::V));
}

/// A mask switches the hook off from that layer down, and the harness's sentence is what the
/// model reads - the difference between a notice nobody patched and one a layer silenced.
#[test]
fn a_masked_hook_leaves_the_harnesss_own_sentence() {
    let root = tempdir("notice-mask");
    let table = Table::new();
    table.layer(
        &root,
        "a-below",
        &[("notice_monitor", says(json!({"text": "the layer below"})))],
    );
    table.layer(
        &root,
        "z-above",
        &[("notice_monitor", says(json!({"mask": true})))],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    assert_eq!(
        host.notice(&hub, "monitor", "the harness's own", json!({})),
        Some("the harness's own".to_string()),
        "a mask answers nothing and asks nothing below it"
    );
}

/// A layer asks below and the lower layer really is called: the witness is the lower
/// layer's trap arriving in this call's failures while the top layer still answers.
#[test]
fn a_layer_asks_below_and_the_lower_layer_is_called() {
    let root = tempdir("notice-below");
    let table = Table::new();
    table.layer(&root, "a-below", &[("notice_monitor", vec![None])]);
    table.layer(
        &root,
        "z-above",
        &[(
            "notice_monitor",
            vec![
                Some(json!({"below": {"v": 1, "kind": "monitor", "text": "lower"}})),
                Some(json!({"text": "the top layer, having seen below"})),
            ],
        )],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let resolved = host
        .ask(
            &hub,
            "notice_monitor",
            &json!({"v": 1, "kind": "monitor", "text": "x"}),
        )
        .expect("a layer answers");
    assert_eq!(
        resolved.answer,
        Some(json!({"text": "the top layer, having seen below"}))
    );
    assert_eq!(resolved.by.as_deref(), Some("p2"), "the top layer answers");
    assert!(
        resolved.failures.iter().any(|f| f.why.contains("trapped")),
        "the layer below was asked, and it trapped: {:?}",
        resolved.failures
    );
}

/// A disabled layer is not asked at all, and a hook nobody answers is not recorded: the bisect
/// has to be visible in the recorder too, or a replay would run against a layer that was off.
#[test]
fn a_disabled_layer_is_not_asked() {
    let root = tempdir("notice-disabled");
    let table = Table::new();
    table.layer(
        &root,
        "fix",
        &[("notice_monitor", says(json!({"text": "patched"})))],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let id = host.lock().stack.layers()[0].id.clone();
    host.lock().stack.set_disabled(&id, true).expect("disable");
    assert_eq!(
        host.notice(&hub, "monitor", "the harness's own", json!({})),
        Some("the harness's own".to_string())
    );
    assert!(
        host.ask(&hub, "notice_monitor", &json!({})).is_none(),
        "with the only layer off, nothing exports the hook"
    );
    assert_eq!(host.lock().rec.inputs("notice_monitor").len(), 0);
}

/// `plugin disable` outlives the daemon: a host rebuilt from the same dirs reads the layer as
/// off, and `plugin enable` turns it back on for the next one.
#[test]
fn a_disabled_layer_stays_off_when_the_host_is_rebuilt() {
    let root = tempdir("notice-disabled-restart");
    let table = Table::new();
    table.layer(
        &root,
        "fix",
        &[("notice_monitor", says(json!({"text": "patched"})))],
    );
    let hub = Hub::new("s-plugins");
    let host = table_host(&root, &table);
    host.set_enabled(&hub, "fix", false).expect("disable");

    let host = table_host(&root, &table);
    assert!(
        host.lock().stack.layers()[0].disabled,
        "back on after restart"
    );
    assert!(host.ask(&hub, "notice_monitor", &json!({})).is_none());

    host.set_enabled(&hub, "fix", true).expect("enable");
    let host = table_host(&root, &table);
    assert!(!host.lock().stack.layers()[0].disabled);
}

/// A layer that traps falls to the one below, and the model is told which failed - the loud
/// half of the fall-through.
#[test]
fn a_trapping_layer_falls_down_and_is_reported() {
    let root = tempdir("notice-trap");
    let table = Table::new();
    table.layer(
        &root,
        "a-working",
        &[(
            "notice_monitor",
            says(json!({"text": "the layer below answered"})),
        )],
    );
    table.layer(&root, "z-broken", &[("notice_monitor", vec![None])]);
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    assert_eq!(
        host.notice(&hub, "monitor", "the harness's own", json!({})),
        Some("the layer below answered".to_string())
    );
    let warned = hub
        .retained()
        .iter()
        .any(|e| matches!(&e.event, SessionEvent::Warning { code, .. } if code == "plugin_layer"));
    assert!(warned, "the failing layer is said on the session's log");
    assert!(
        host.take_notices()
            .iter()
            .any(|n| n.contains("the layer below it answered")),
        "and queued for the model, which is the one that can fix it"
    );
}

/// A session with no layers says the harness's sentence and pays nothing for it: a session that
/// patches nothing behaves as it did before there were plugins.
#[test]
fn a_session_with_no_layers_is_unchanged() {
    let root = tempdir("notice-none");
    let table = Table::new();
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    assert_eq!(
        host.notice(&hub, "monitor", "the harness's own", json!({})),
        Some("the harness's own".to_string())
    );
    assert!(
        host.ask(&hub, "notice_monitor", &json!({})).is_none(),
        "nothing was asked, so nothing was recorded"
    );
    assert!(host.lock().rec.inputs("notice_monitor").is_empty());
    assert!(hub.retained().is_empty(), "and nothing was published");
}

/// A firing may be dropped and a settlement may not: the kinds a project never wants are the
/// firing and the nudge, and a layer that tries to silence a fact the model is owed is refused
/// and told why.
#[test]
fn a_dropped_firing_is_silent_and_a_dropped_settlement_is_refused() {
    let root = tempdir("notice-drop");
    let table = Table::new();
    table.layer(
        &root,
        "quiet",
        &[
            ("notice_monitor", says(json!({"drop": true}))),
            ("notice_job", says(json!({"drop": true}))),
        ],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    assert_eq!(
        host.notice(&hub, "monitor", "a firing", json!({})),
        None,
        "a project may switch a firing off"
    );
    assert_eq!(
        host.notice(&hub, "job", "a settlement", json!({})),
        Some("a settlement".to_string()),
        "a settlement is a fact the model is owed"
    );
    assert!(
        host.take_notices()
            .iter()
            .any(|n| n.contains("may be reworded but not dropped")),
        "and the layer is told why it did nothing"
    );
}

/// An answer that is not the contract is refused, not obeyed: a layer that says `{"text": 5}`
/// must not silence a notice, and the model is told its layer is wrong.
#[test]
fn an_answer_that_is_not_a_notice_is_refused() {
    let root = tempdir("notice-wrong-shape");
    let table = Table::new();
    table.layer(
        &root,
        "wrong",
        &[("notice_monitor", says(json!({"text": 5})))],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    assert_eq!(
        host.notice(&hub, "monitor", "the harness's own", json!({})),
        Some("the harness's own".to_string())
    );
    assert!(
        host.take_notices()
            .iter()
            .any(|n| n.contains("is not a notice")),
        "the refusal names the shape"
    );
}

// `on_tool_result`: the result, before the model reads it.

/// The call every result test here answers: the plan's own example, a pull request opened.
fn a_call() -> letibot_plugin::Call {
    letibot_plugin::Call::new("bash", json!({"command": "gh pr create --fill"}))
}

/// A layer adds a sentence to a result - the hook's whole first use: the call printed a pull
/// request URL and the model is told what is watching it now.
#[test]
fn a_layer_appends_to_a_result() {
    let root = tempdir("result-append");
    let table = Table::new();
    table.layer(
        &root,
        "watch-pr",
        &[(
            "on_tool_result",
            says(json!({"append": "\n[harness] watching o/r#24 for its checks"})),
        )],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let answered = host
        .on_tool_result(&hub, &a_call(), "ok", "https://github.com/o/r/pull/24\n")
        .expect("a layer answers");
    assert_eq!(
        answered.out.said(),
        letibot_plugin::tool_result::Said::Appended("\n[harness] watching o/r#24 for its checks")
    );
    assert_eq!(answered.by, "p1");
    // And the input is the host's own shape: the call as an object, the outcome as a word
    // the payload the model would have read.
    let seen = host.lock().rec.inputs("on_tool_result").to_vec();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["call"]["name"], json!("bash"));
    assert_eq!(seen[0]["outcome"], json!("ok"));
    assert_eq!(
        seen[0]["payload"],
        json!("https://github.com/o/r/pull/24\n")
    );
    assert_eq!(seen[0]["v"], json!(letibot_plugin::V));
}

/// A layer replaces a result, and a suffix alone is a different answer.
#[test]
fn a_layer_replaces_a_result() {
    let root = tempdir("result-replace");
    let table = Table::new();
    table.layer(
        &root,
        "shorten",
        &[("on_tool_result", says(json!({"payload": "opened 24"})))],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let answered = host
        .on_tool_result(&hub, &a_call(), "ok", "a very long transcript")
        .expect("a layer answers");
    assert_eq!(
        answered.out.said(),
        letibot_plugin::tool_result::Said::Text("opened 24".into())
    );
}

/// A mask leaves the result exactly as the call produced it - the hook switched off from that
/// layer down, which is what a mask means.
#[test]
fn a_masked_result_hook_leaves_the_call_own_words() {
    let root = tempdir("result-mask");
    let table = Table::new();
    table.layer(
        &root,
        "a-below",
        &[("on_tool_result", says(json!({"append": "the layer below"})))],
    );
    table.layer(
        &root,
        "z-above",
        &[("on_tool_result", says(json!({"mask": true})))],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    assert!(
        host.on_tool_result(&hub, &a_call(), "ok", "x").is_none(),
        "a mask answers nothing and asks nothing below it"
    );
}

/// A layer asks below and edits what it is told: the dotenv-style fix's shape, over a result
/// rather than a notice.
#[test]
fn a_result_layer_asks_below_and_answers_for_itself() {
    let root = tempdir("result-below");
    let table = Table::new();
    table.layer(&root, "a-below", &[("on_tool_result", vec![None])]);
    table.layer(
        &root,
        "z-above",
        &[(
            "on_tool_result",
            vec![
                Some(json!({"below": {"v": 1, "outcome": "ok", "payload": "x"}})),
                Some(json!({"append": " (edited)"})),
            ],
        )],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let resolved = host
        .ask(
            &hub,
            "on_tool_result",
            &json!({"v": 1, "outcome": "ok", "payload": "x"}),
        )
        .expect("a layer answers");
    assert_eq!(resolved.by.as_deref(), Some("p2"), "the top layer answers");
    assert!(
        resolved.failures.iter().any(|f| f.why.contains("trapped")),
        "the layer below was asked: {:?}",
        resolved.failures
    );
}

/// A disabled layer is not asked, and a hook nobody answers is not recorded.
#[test]
fn a_disabled_result_layer_is_not_asked() {
    let root = tempdir("result-disabled");
    let table = Table::new();
    table.layer(
        &root,
        "fix",
        &[("on_tool_result", says(json!({"append": "patched"})))],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let id = host.lock().stack.layers()[0].id.clone();
    host.lock().stack.set_disabled(&id, true).expect("disable");
    assert!(host.on_tool_result(&hub, &a_call(), "ok", "x").is_none());
    assert_eq!(host.lock().rec.inputs("on_tool_result").len(), 0);
}

/// A trapping layer falls to the one below, and the model is told: a result the model is owed
/// is never lost to a layer's trap.
#[test]
fn a_trapping_result_layer_falls_down_and_is_reported() {
    let root = tempdir("result-trap");
    let table = Table::new();
    table.layer(
        &root,
        "a-working",
        &[("on_tool_result", says(json!({"append": " from below"})))],
    );
    table.layer(&root, "z-broken", &[("on_tool_result", vec![None])]);
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let answered = host
        .on_tool_result(&hub, &a_call(), "ok", "x")
        .expect("the layer below answers");
    assert_eq!(
        answered.out.said(),
        letibot_plugin::tool_result::Said::Appended(" from below")
    );
    assert!(
        host.take_notices()
            .iter()
            .any(|n| n.contains("the layer below it answered")),
        "and the failure is said to the model"
    );
}

/// A session with no layers hands the result back untouched, and records nothing.
#[test]
fn a_session_with_no_layers_rewrites_no_result() {
    let root = tempdir("result-none");
    let table = Table::new();
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    assert!(host.on_tool_result(&hub, &a_call(), "ok", "x").is_none());
    assert!(host.lock().rec.inputs("on_tool_result").is_empty());
}

/// An `arm` becomes the `monitor` call the model would have typed - one door for a watch, so
/// the coalescing and the refusals stay the tool's.
#[test]
fn an_arm_is_a_monitor_call() {
    let arm = letibot_plugin::tool_result::Arm::new("coderabbit", json!({"pr": "o/r#24"}));
    let call = arm_call(&arm);
    assert_eq!(call.name, "monitor");
    let args: Value = serde_json::from_str(&call.arguments).expect("an object");
    assert_eq!(args["name"], json!("coderabbit"));
    assert_eq!(args["plugin"], json!("coderabbit"));
    assert_eq!(args["args"], json!({"pr": "o/r#24"}));
    assert_eq!(
        args["repeat"],
        json!(true),
        "a watch repeats, as `pr_watch`'s does"
    );
}

// `on_tool_call`: the call, before the gate.

fn standing() -> letibot_tools::runtime::Standing {
    letibot_tools::runtime::Standing {
        session: "s-plugins".into(),
        mode: "allow-edits".into(),
    }
}

/// A layer rewrites a call's arguments, and the answer says what it wants.
#[test]
fn a_layer_rewrites_a_call() {
    let root = tempdir("call-rewrite");
    let table = Table::new();
    table.layer(
        &root,
        "one-watch",
        &[(
            "on_tool_call",
            says(json!({"arguments": {"name": "pr-24", "until": "checks"}})),
        )],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let asked = letibot_plugin::Call::new(
        "monitor",
        json!({"name": "pr-24-coderabbit", "until": "coderabbit"}),
    );
    let answered = host
        .on_tool_call(&hub, &asked, &standing())
        .expect("a layer answers");
    assert!(matches!(
        answered.out.verdict(),
        letibot_plugin::tool_call::Verdict::Rewrite(_)
    ));
    assert_eq!(answered.by, "p1");
    // The input carries the session's own facts - the mode is the gate's word for how much
    // this session may do unasked, and a layer is allowed to be narrower than *always*.
    let seen = host.lock().rec.inputs("on_tool_call").to_vec();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["call"]["name"], json!("monitor"));
    assert_eq!(seen[0]["session"]["mode"], json!("allow-edits"));
    assert_eq!(seen[0]["session"]["session"], json!("s-plugins"));
    assert!(seen[0]["session"]["workspace"].is_string());
    assert_eq!(seen[0]["v"], json!(letibot_plugin::V));
}

/// A layer refuses a call, and a layer answers one - the two ways of not running it.
#[test]
fn a_layer_can_refuse_or_answer_a_call() {
    let root = tempdir("call-refuse");
    let table = Table::new();
    table.layer(
        &root,
        "one-watch",
        &[(
            "on_tool_call",
            says(json!({"refuse": "already watched as pr-24; use that"})),
        )],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let asked = letibot_plugin::Call::new("monitor", json!({}));
    let answered = host
        .on_tool_call(&hub, &asked, &standing())
        .expect("answers");
    assert_eq!(
        answered.out.verdict(),
        letibot_plugin::tool_call::Verdict::Refuse("already watched as pr-24; use that")
    );

    let root = tempdir("call-answer");
    let table = Table::new();
    table.layer(
        &root,
        "knows-it",
        &[(
            "on_tool_call",
            says(json!({"answer": "pr-24 is watched for its checks"})),
        )],
    );
    let host = table_host(&root, &table);
    let answered = host
        .on_tool_call(&hub, &asked, &standing())
        .expect("answers");
    assert_eq!(
        answered.out.verdict(),
        letibot_plugin::tool_call::Verdict::Answer("pr-24 is watched for its checks")
    );
}

/// An `answer` verdict reaches the model with one line saying who answered it, in the layer's
/// own name and scope: the payload is a layer's sentence and would otherwise read like a tool's
/// output. The `Answered` id (`s3`) is what the audit rows use and a model must not be handed.
#[test]
fn an_answered_call_is_attributed_by_name_and_scope() {
    let root = tempdir("call-answer-note");
    let table = Table::new();
    table.layer(
        &root,
        "knows-it",
        &[(
            "on_tool_call",
            says(json!({"answer": "pr-24 is watched for its checks"})),
        )],
    );
    let host = Arc::new(table_host(&root, &table));
    let hub = Hub::new("s-plugins");
    let hooks = CallHooks::new(Arc::new(Mutex::new(Some(host.clone()))), hub.clone());
    let asked = letibot_plugin::Call::new("monitor", json!({}));
    let patched = letibot_tools::runtime::CallHook::before(
        &hooks,
        &asked,
        &letibot_tools::runtime::Standing::default(),
    )
    .expect("answers");
    assert_eq!(
        patched.note.as_deref(),
        Some("answered by plugin knows-it (project); the tool did not run"),
        "the note must name the layer a person wrote, not `{}`",
        patched.by
    );

    // A refusal carries none, because its outcome already says a layer refused it - and a pass
    // carries none either, because nothing was said.
    let refused = tempdir("call-refuse-note");
    let table = Table::new();
    table.layer(
        &refused,
        "one-watch",
        &[("on_tool_call", says(json!({"refuse": "already watched"})))],
    );
    let host = Arc::new(table_host(&refused, &table));
    let hooks = CallHooks::new(Arc::new(Mutex::new(Some(host))), hub);
    let patched = letibot_tools::runtime::CallHook::before(
        &hooks,
        &asked,
        &letibot_tools::runtime::Standing::default(),
    )
    .expect("answers");
    assert_eq!(patched.note, None);
}

/// A guard's malformed verdict, or a rewrite that adds an argument, refuses the call and
/// names the layer - never "runs as the model proposed".
#[test]
fn a_guard_that_answers_badly_or_widens_a_call_refuses_it() {
    for (tag, answer, want) in [
        ("bad", json!({"refuse": 5}), "not a call's verdict"),
        (
            "widen",
            json!({"arguments": {"name": "x", "extra": true}}),
            "adds the argument `extra`",
        ),
    ] {
        let root = tempdir(&format!("call-{tag}"));
        let table = Table::new();
        table.layer(&root, "guard", &[("on_tool_call", says(answer))]);
        let host = table_host(&root, &table);
        let hub = Hub::new("s-plugins");
        let asked = letibot_plugin::Call::new("monitor", json!({"name": "x"}));
        let out = host
            .on_tool_call(&hub, &asked, &standing())
            .expect("answers")
            .out;
        let letibot_plugin::tool_call::Verdict::Refuse(why) = out.verdict() else {
            panic!("{tag}: {out:?}");
        };
        assert!(why.contains("`p1`") && why.contains(want), "{tag}: {why}");
    }
}

/// A mask lets the call through untouched - the hook switched off from that layer down, which
/// for this kind means the model's call is the one that runs.
#[test]
fn a_masked_call_hook_lets_the_call_through() {
    let root = tempdir("call-mask");
    let table = Table::new();
    table.layer(
        &root,
        "a-below",
        &[("on_tool_call", says(json!({"refuse": "the layer below"})))],
    );
    table.layer(
        &root,
        "z-above",
        &[("on_tool_call", says(json!({"mask": true})))],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let asked = letibot_plugin::Call::new("monitor", json!({}));
    assert!(host.on_tool_call(&hub, &asked, &standing()).is_none());
}

/// A layer asks below before deciding: the shape a fix uses when it wants to narrow what a
/// lower layer said rather than replace it.
#[test]
fn a_call_layer_asks_below_and_answers_for_itself() {
    let root = tempdir("call-below");
    let table = Table::new();
    table.layer(&root, "a-below", &[("on_tool_call", vec![None])]);
    table.layer(
        &root,
        "z-above",
        &[(
            "on_tool_call",
            vec![
                Some(json!({"below": {"v": 1, "call": {"name": "monitor", "arguments": {}}}})),
                Some(json!({"refuse": "no"})),
            ],
        )],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let asked = letibot_plugin::Call::new("monitor", json!({}));
    let answered = host
        .on_tool_call(&hub, &asked, &standing())
        .expect("answers");
    assert_eq!(answered.by, "p2", "the top layer answers");
    assert_eq!(
        answered.out.verdict(),
        letibot_plugin::tool_call::Verdict::Refuse("no")
    );
}

/// A disabled layer is not asked, and a hook nobody answers is not recorded.
#[test]
fn a_disabled_call_layer_is_not_asked() {
    let root = tempdir("call-disabled");
    let table = Table::new();
    table.layer(
        &root,
        "fix",
        &[("on_tool_call", says(json!({"refuse": "patched"})))],
    );
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let id = host.lock().stack.layers()[0].id.clone();
    host.lock().stack.set_disabled(&id, true).expect("disable");
    let asked = letibot_plugin::Call::new("monitor", json!({}));
    assert!(host.on_tool_call(&hub, &asked, &standing()).is_none());
    assert_eq!(host.lock().rec.inputs("on_tool_call").len(), 0);
}

/// A trapping layer falls to the one below, and the model is told: a call is never left
/// undecided because a layer broke.
#[test]
fn a_trapping_call_layer_closes_the_call_and_is_reported() {
    let root = tempdir("call-trap");
    let table = Table::new();
    table.layer(
        &root,
        "a-working",
        &[("on_tool_call", says(json!({"refuse": "from below"})))],
    );
    table.layer(&root, "z-broken", &[("on_tool_call", vec![None])]);
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let asked = letibot_plugin::Call::new("monitor", json!({}));
    let answered = host
        .on_tool_call(&hub, &asked, &standing())
        .expect("answers");
    let letibot_plugin::tool_call::Verdict::Refuse(why) = answered.out.verdict() else {
        panic!("a failed guard refuses: {:?}", answered.out);
    };
    assert!(
        why.contains("guards this call and failed"),
        "the refusal says why: {why}"
    );
    assert_eq!(answered.by, "p2", "and names the layer that failed");
    assert!(
        host.take_notices()
            .iter()
            .any(|n| n.contains("guards this call and failed")),
        "and the failure is said to the model"
    );
}

/// A session with no layers shows nothing to a layer and records nothing.
#[test]
fn a_session_with_no_layers_passes_every_call_through() {
    let root = tempdir("call-none");
    let table = Table::new();
    let host = table_host(&root, &table);
    let hub = Hub::new("s-plugins");
    let asked = letibot_plugin::Call::new("bash", json!({"command": "ls"}));
    assert!(host.on_tool_call(&hub, &asked, &standing()).is_none());
    assert!(host.lock().rec.inputs("on_tool_call").is_empty());
    assert!(hub.retained().is_empty());
}

// `condition`: a monitor whose policy is a layer.

/// The daemon's half, over this test's host - the real look runner and the real stack. No gate
/// these tests are about the condition's shape, and the ones that DO adjudicate pass their own
/// [`Gate`](conditions::Unasked).
fn conditions(host: &Arc<PluginHost>) -> Arc<conditions::HostConditions> {
    conditions_with(host, None)
}

fn conditions_with(
    host: &Arc<PluginHost>,
    gate: Option<Arc<dyn conditions::Unasked>>,
) -> Arc<conditions::HostConditions> {
    conditions::HostConditions::new(
        Arc::new(Mutex::new(Some(host.clone()))),
        Hub::new("s-plugins"),
        gate,
    )
}

/// A layer's condition arms, and its answer is what the watch follows: the arming call, the
/// interval, the cap and the state, through the seam the daemon fills.
#[test]
fn a_layers_condition_arms_with_its_own_interval_and_cap() {
    use letibot_tools::exec::plugin_condition::PluginConditions as _;
    let root = tempdir("condition-arm");
    let table = Table::new();
    table.layer(
        &root,
        "coderabbit",
        &[(
            "condition_coderabbit",
            says(json!({
                "look": {"read": "status.json"},
                "every_s": 90,
                "max_ttl_s": 604800,
                "state": {"armed": true}
            })),
        )],
    );
    std::fs::write(root.join("status.json"), "{\"reviews\":3}").expect("a status file");
    let host = Arc::new(table_host(&root, &table));
    let armed = conditions(&host)
        .arm("coderabbit", json!({"pr": "o/r#24"}))
        .expect("the layer arms it");
    assert_eq!(
        armed.condition.max_ttl(),
        Some(Duration::from_secs(604800)),
        "the plugin's own cap, inside the tree's week"
    );
    assert!(armed.condition.describe().contains("coderabbit"));
    // The read look ran under the layer's `read` capability and is now ready to hand over -
    // which is the half that proves the seam reaches the workspace.
    assert_eq!(
        armed.condition.met(),
        None,
        "a file look has no firing of its own"
    );
    let seen = host.lock().rec.inputs("condition_coderabbit").to_vec();
    assert!(!seen.is_empty(), "the arming call was recorded");
    assert_eq!(seen[0]["args"], json!({"pr": "o/r#24"}));
    assert!(seen[0]["look"].is_null(), "arming asks with no look");
}

/// A watch nobody answers is refused by name, and so is one whose layer is off - the two are
/// the same sentence to a model, and it names the hook.
#[test]
fn a_condition_nobody_answers_is_refused() {
    use letibot_tools::exec::plugin_condition::PluginConditions as _;
    let root = tempdir("condition-none");
    let table = Table::new();
    let host = Arc::new(table_host(&root, &table));
    let why = match conditions(&host).arm("coderabbit", json!({})) {
        Ok(_) => panic!("nothing answers it"),
        Err(why) => why,
    };
    assert!(
        why.why.contains("no layer answers `condition_coderabbit`"),
        "{}",
        why.why
    );

    // And a layer that IS there but switched off answers nothing either.
    let root = tempdir("condition-disabled");
    let table = Table::new();
    table.layer(
        &root,
        "coderabbit",
        &[("condition_coderabbit", says(json!({"every_s": 30})))],
    );
    let host = Arc::new(table_host(&root, &table));
    let id = host.lock().stack.layers()[0].id.clone();
    host.lock().stack.set_disabled(&id, true).expect("disable");
    assert!(conditions(&host).arm("coderabbit", json!({})).is_err());
}

/// A condition layer that traps falls to the one below, which is how a session fixes a broken
/// watch without deleting it.
#[test]
fn a_trapping_condition_layer_falls_down() {
    use letibot_tools::exec::plugin_condition::PluginConditions as _;
    let root = tempdir("condition-trap");
    let table = Table::new();
    table.layer(
        &root,
        "a-working",
        &[("condition_coderabbit", says(json!({"every_s": 30})))],
    );
    table.layer(&root, "z-broken", &[("condition_coderabbit", vec![None])]);
    let host = Arc::new(table_host(&root, &table));
    let armed = conditions(&host)
        .arm("coderabbit", json!({}))
        .expect("the layer below arms it");
    assert!(armed.condition.describe().contains("coderabbit"));
    assert!(
        host.take_notices()
            .iter()
            .any(|n| n.contains("the layer below it answered")),
        "and the trap is said to the model"
    );
}

/// An `exec` look the layer did not declare runs nothing, and says which capability it wanted -
/// the enforcement point, exercised through a real armed watch.
#[test]
fn an_exec_look_without_the_capability_is_refused() {
    use letibot_tools::exec::plugin_condition::PluginConditions as _;
    let root = tempdir("condition-exec");
    let table = Table::new();
    table.layer(
        &root,
        "shell",
        &[(
            "condition_shell",
            vec![
                Some(json!({"look": {"exec": ["true"]}, "every_s": 5})),
                Some(json!({"fired": "the look came back"})),
            ],
        )],
    );
    let host = Arc::new(table_host(&root, &table));
    // The gate is never reached: the capability is checked first, because a program the layer
    // never declared is not a call anybody adjudicates. So this probe must stay empty.
    let probe = Arc::new(ProbeGate::admitting());
    let armed = conditions_with(&host, Some(probe.clone()))
        .arm("shell", json!({}))
        .expect("armed");
    // The first tick hands the refusal over, and the layer answers it.
    assert_eq!(
        armed.condition.met(),
        Some("the look came back".to_string())
    );
    let seen = host.lock().rec.inputs("condition_shell").to_vec();
    let handed = seen
        .iter()
        .find(|i| !i["look"].is_null())
        .unwrap_or_else(|| panic!("the look's result was handed back: {seen:?}"));
    assert_eq!(
        handed["look"]["exit"],
        json!(126),
        "a program the layer did not declare is not run: {handed}"
    );
    assert!(
        handed["look"]["stderr"]
            .as_str()
            .is_some_and(|s| s.contains("exec:true")),
        "and the refusal names the capability: {handed}"
    );
    assert!(
        probe.seen().is_empty(),
        "the gate was asked about an undeclared program: {:?}",
        probe.seen()
    );
}

// An `exec` look goes through the adjudicator, without asking.

/// A gate a test answers with, and what it was asked: one decision, and every command line the
/// look offered it, so a test can say both what it would do and whether it was consulted.
struct ProbeGate {
    answer: fn() -> GateDecision,
    seen: Mutex<Vec<String>>,
}

impl ProbeGate {
    fn admitting() -> ProbeGate {
        ProbeGate {
            answer: || GateDecision::Admit,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn refusing(answer: fn() -> GateDecision) -> Arc<ProbeGate> {
        Arc::new(ProbeGate {
            answer,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl conditions::Unasked for ProbeGate {
    fn admit_unasked(&self, call: &letibot_tools::GateCall<'_>) -> Option<GateDecision> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).push(
            call.args["command"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
        Some((self.answer)())
    }
}

/// The warnings one session's log holds under a code.
fn warned(hub: &Arc<Hub>, code: &str) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning {
                code: c, detail, ..
            } if c == code => Some(detail.clone()),
            _ => None,
        })
        .collect()
}

/// A layer whose one look is `exec:<program>`, and what it answers once the look is in.
fn shell_layer(root: &Path, program: &str, argv: Value) -> Table {
    let table = Table::new();
    table.layer_with_caps(
        root,
        "shell",
        &[&format!("exec:{program}")],
        &[(
            "condition_shell",
            vec![
                Some(json!({"look": {"exec": argv}, "every_s": 5})),
                Some(json!({"fired": "the look came back"})),
            ],
        )],
    );
    table
}

/// Run the watch until it fires, with a deadline: a look that is a real child is polled on the
/// next tick, so a test that called `met` once would be racing the process.
fn until_fired(condition: &Arc<dyn letibot_tools::exec::monitor::Condition>) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(said) = condition.met() {
            return said;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the look never came back"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A built-in's `exec` look runs freely - *"Only the builtins run freely"*, `exec` included
/// the layer is the binary's own bytes, so there is nobody to ask. The gate here refuses
/// everything, so without that arm the probe would have been asked and the look refused.
#[test]
fn a_builtins_exec_look_runs_without_the_gate() {
    use letibot_tools::exec::plugin_condition::PluginConditions as _;
    let root = tempdir("look-builtin");
    // No disk layer at all: the ONLY layer is the built-in below.
    let table = Table::new();
    let host = Arc::new(table_host(&root, &table));
    let answers: BTreeMap<String, Script> = [(
        key("condition_shell"),
        vec![
            Some(json!({"look": {"exec": ["true"]}, "every_s": 5})),
            Some(json!({"fired": "the look came back"})),
        ],
    )]
    .into_iter()
    .collect();
    let manifest = Manifest::parse(
        "name = \"shell\"\npurpose = \"p\"\nhooks = [\"condition_shell\"]\n\
         capabilities = [\"exec:true\"]\ncontract = 1\n",
    )
    .expect("a manifest");
    host.lock().stack.register_builtin(
        letibot_plugins::Layer {
            id: String::new(),
            scope: letibot_plugins::Scope::BuiltIn,
            dir: std::path::PathBuf::new(),
            manifest,
            sha256: String::new(),
            manifest_sha256: String::new(),
            disabled: false,
            trusted: false,
            pin: None,
        },
        Box::new(TableModule {
            hooks: answers.keys().cloned().collect(),
            answers,
            calls: Mutex::new(BTreeMap::new()),
        }),
    );
    let probe = ProbeGate::refusing(|| {
        GateDecision::refuse_and_tell(
            letibot_transcript::ToolOutcome::NotRun {
                why: "nothing a built-in runs should have reached this gate".into(),
            },
            "",
        )
    });
    let armed = conditions_with(&host, Some(probe.clone()))
        .arm("shell", json!({}))
        .expect("armed");
    assert_eq!(until_fired(&armed.condition), "the look came back");
    assert!(
        probe.seen().is_empty(),
        "a built-in runs freely: the gate was asked {:?}",
        probe.seen()
    );
    let seen = host.lock().rec.inputs("condition_shell").to_vec();
    let handed = seen
        .iter()
        .find(|i| !i["look"].is_null())
        .unwrap_or_else(|| panic!("the look's result was handed back: {seen:?}"));
    assert_eq!(handed["look"]["exit"], json!(0), "the program really ran");
}

/// The look belongs to the layer whose answer asked for it, not to the one that declares the
/// hook. A pinned layer on top passes the hook, so the answer comes from an unpinned layer below
/// and the `exec` look is that layer's - judged by the topmost declarer it ran unadjudicated.
#[test]
fn an_exec_look_is_judged_by_the_layer_whose_answer_asked_for_it() {
    use letibot_tools::exec::plugin_condition::PluginConditions as _;
    let root = tempdir("look-answerer");
    let table = Table::new();
    // `b-top` sorts last, so it is the newest and sits on top. It passes everything.
    table.layer_with_caps(
        &root,
        "a-bottom",
        &["exec:true"],
        &[(
            "condition_shell",
            vec![
                Some(json!({"look": {"exec": ["true"]}, "every_s": 5})),
                Some(json!({"fired": "the look came back"})),
            ],
        )],
    );
    table.layer_with_caps(
        &root,
        "b-top",
        &["exec:true"],
        &[("condition_shell", vec![Some(json!({"pass": true}))])],
    );
    let book = letibot_plugins::Trust::in_memory();
    book.pin("b-top", &root.join(".letibot/plugins/b-top"))
        .expect("the top layer is the operator's");
    let host = Arc::new(table_host(&root, &table).with_trust(book.clone()));
    // The premise, asserted rather than assumed: the top layer is free and the one that
    // answers is not.
    assert!(
        host.free_layer("condition_shell"),
        "the topmost declarer is pinned, which is what made the old rule wrong"
    );

    let probe = ProbeGate::refusing(|| {
        GateDecision::refuse_and_tell(
            letibot_transcript::ToolOutcome::NotRun {
                why: "would need your approval: an unpinned layer's `exec`".into(),
            },
            "",
        )
    });
    let armed = conditions_with(&host, Some(probe.clone()))
        .arm("shell", json!({}))
        .expect("armed");
    assert_eq!(until_fired(&armed.condition), "the look came back");
    assert!(
        !probe.seen().is_empty(),
        "the answering layer is unpinned, so its look meets the gate: the top layer's pin \
         cannot stand in for the layer below it"
    );
    let seen = host.lock().rec.inputs("condition_shell").to_vec();
    let handed = seen
        .iter()
        .find(|i| !i["look"].is_null())
        .unwrap_or_else(|| panic!("the look's result was handed back: {seen:?}"));
    assert_ne!(
        handed["look"]["exit"],
        json!(0),
        "the refused look did not run: {handed}"
    );
}

/// A pin that broke while no daemon was running is still said: `with_trust` re-decides every
/// layer and records a broken pin as said as it answers, so a host that dropped that answer
/// would silence `plugin_untrusted` for exactly the restart case it exists for.
#[test]
fn a_pin_that_broke_before_the_host_existed_is_said_on_its_first_tick() {
    let root = tempdir("pin-broken-restart");
    let table = Table::new();
    table.layer_with_caps(
        &root,
        "one",
        &[],
        &[("pane_dashboard", says(json!({"title": "one"})))],
    );
    let book = letibot_plugins::Trust::in_memory();
    let dir = root.join(".letibot/plugins/one");
    let pin = book.pin("one", &dir).expect("a pin");
    // The bytes move while nothing is running: a rebuild the daemon never saw.
    std::fs::write(dir.join("plugin.wasm"), b"a module, rebuilt and longer").expect("a rebuild");

    // A host built NOW - the restart - is the first thing to see the broken pin.
    let host = Arc::new(table_host(&root, &table).with_trust(book.clone()));
    let hub = Hub::new("s-plugins");
    Arc::clone(&host).tick(hub.clone());

    let said = warned(&hub, "plugin_untrusted");
    assert_eq!(said.len(), 1, "the broken pin is said once: {said:?}");
    assert!(said[0].contains("`one`"), "{}", said[0]);
    assert!(
        said[0].contains(&pin.module),
        "the pinned hash: {}",
        said[0]
    );
    // And it is said ONCE: a tick every two seconds is not a wall.
    Arc::clone(&host).tick(hub.clone());
    assert_eq!(warned(&hub, "plugin_untrusted").len(), 1);
}

/// A look the gate admits runs - and what the gate was asked is the call a `bash` of that argv
/// would have been, every word quoted.
#[test]
fn an_exec_look_the_gate_admits_runs() {
    use letibot_tools::exec::plugin_condition::PluginConditions as _;
    let root = tempdir("look-admitted");
    let table = shell_layer(&root, "true", json!(["true", "--a b", "$HOME"]));
    let host = Arc::new(table_host(&root, &table));
    let probe = Arc::new(ProbeGate::admitting());
    let armed = conditions_with(&host, Some(probe.clone()))
        .arm("shell", json!({}))
        .expect("armed");
    assert_eq!(until_fired(&armed.condition), "the look came back");
    let asked = probe.seen();
    assert!(
        !asked.is_empty(),
        "the gate is what decides whether an `exec` look runs"
    );
    assert!(
        asked.iter().all(|c| c == "true '--a b' '$HOME'"),
        "the gate reads the argv, quoted — not a shell line anybody could re-split: {asked:?}"
    );
    let seen = host.lock().rec.inputs("condition_shell").to_vec();
    let handed = seen
        .iter()
        .find(|i| !i["look"].is_null())
        .unwrap_or_else(|| panic!("the look's result was handed back: {seen:?}"));
    assert_eq!(
        handed["look"]["exit"],
        json!(0),
        "an admitted look runs: {handed}"
    );
}

/// A look the gate would have asked about does not run, the layer is told why, and the session
/// is told once.
#[test]
fn an_exec_look_the_gate_would_ask_about_does_not_run() {
    use letibot_tools::exec::plugin_condition::PluginConditions as _;
    let root = tempdir("look-asks");
    // A program whose running leaves a file behind, so "did not run" is a fact and not a
    // reading of the refusal.
    let marker = root.join("ran");
    let script = format!("touch {}", marker.display());
    let table = shell_layer(&root, "sh", json!(["sh", "-c", script]));
    let host = Arc::new(table_host(&root, &table));
    let hub = Hub::new("s-plugins");
    let probe = ProbeGate::refusing(|| {
        GateDecision::refuse_and_tell(
            letibot_transcript::ToolOutcome::NotRun {
                why: "would need your approval: `bash` wants exec access to `sh -c …`".into(),
            },
            "nothing ran",
        )
    });
    let armed = conditions::HostConditions::new(
        Arc::new(Mutex::new(Some(host.clone()))),
        hub.clone(),
        Some(probe.clone()),
    )
    .arm("shell", json!({}))
    .expect("armed");
    assert_eq!(until_fired(&armed.condition), "the look came back");
    assert!(
        !marker.exists(),
        "a look the gate would ask about must not run"
    );
    let seen = host.lock().rec.inputs("condition_shell").to_vec();
    let handed = seen
        .iter()
        .find(|i| !i["look"].is_null())
        .unwrap_or_else(|| panic!("the look's result was handed back: {seen:?}"));
    assert_eq!(handed["look"]["exit"], json!(126), "{handed}");
    let why = handed["look"]["stderr"].as_str().unwrap_or_default();
    assert!(why.contains("would need your approval"), "{why}");
    assert!(why.contains("`sh`"), "the program is named: {why}");
    let said = warned(&hub, "plugin_look");
    assert_eq!(said.len(), 1, "the session is told once: {said:?}");
    assert!(said[0].contains("shell (project)"), "{}", said[0]);
}

/// A look a rule of the operator's denies does not run either, and the sentence is the other
/// class: somebody said no.
#[test]
fn an_exec_look_the_gate_denies_does_not_run() {
    use letibot_tools::exec::plugin_condition::PluginConditions as _;
    let root = tempdir("look-denied");
    let table = shell_layer(&root, "true", json!(["true"]));
    let host = Arc::new(table_host(&root, &table));
    let probe = ProbeGate::refusing(|| {
        GateDecision::refuse_and_tell(
            letibot_transcript::ToolOutcome::Denied {
                req_id: "adj-1-0001".into(),
            },
            "REFUSED by the permission config",
        )
    });
    let armed = conditions_with(&host, Some(probe.clone()))
        .arm("shell", json!({}))
        .expect("armed");
    assert_eq!(until_fired(&armed.condition), "the look came back");
    let seen = host.lock().rec.inputs("condition_shell").to_vec();
    let handed = seen
        .iter()
        .find(|i| !i["look"].is_null())
        .unwrap_or_else(|| panic!("the look's result was handed back: {seen:?}"));
    let why = handed["look"]["stderr"].as_str().unwrap_or_default();
    assert!(why.contains("the gate denied it"), "{why}");
    assert!(why.contains("REFUSED by the permission config"), "{why}");
}

/// The notice is once per (layer, program, reason): a watch polls every ninety seconds, and the
/// same refusal is not news the second time. A refusal that changes is.
#[test]
fn a_refused_look_is_said_once_until_it_changes() {
    use letibot_tools::exec::plugin_condition::PluginConditions as _;
    let root = tempdir("look-once");
    // One answer that both asks for the look and fires on it, so every arming asks the gate
    // exactly once - the shape a watch repeating its interval has.
    let table = Table::new();
    table.layer_with_caps(
        &root,
        "shell",
        &["exec:true"],
        &[(
            "condition_shell",
            says(json!({
                "look": {"exec": ["true"]},
                "every_s": 5,
                "fired": "the look came back"
            })),
        )],
    );
    let host = Arc::new(table_host(&root, &table));
    let hub = Hub::new("s-plugins");
    let probe = ProbeGate::refusing(|| {
        GateDecision::refuse_and_tell(
            letibot_transcript::ToolOutcome::NotRun {
                why: "would need your approval: the first reason".into(),
            },
            "nothing ran",
        )
    });
    let conditions = conditions::HostConditions::new(
        Arc::new(Mutex::new(Some(host.clone()))),
        hub.clone(),
        Some(probe.clone()),
    );
    for _ in 0..3 {
        conditions.arm("shell", json!({})).expect("armed");
    }
    assert_eq!(
        probe.seen().len(),
        3,
        "each arming asked the gate, which is what makes the count below mean something"
    );
    assert_eq!(
        warned(&hub, "plugin_look").len(),
        1,
        "three firings of one refusal are one warning: {:?}",
        warned(&hub, "plugin_look")
    );

    // And a reason that changed is news again: the layer is fixed, or the gate's own reason
    // moved, and silence would leave the session believing the first sentence.
    let other = ProbeGate::refusing(|| {
        GateDecision::refuse_and_tell(
            letibot_transcript::ToolOutcome::NotRun {
                why: "would need your approval: a different reason".into(),
            },
            "nothing ran",
        )
    });
    let same = conditions::HostConditions::new(
        Arc::new(Mutex::new(Some(host.clone()))),
        hub.clone(),
        Some(other),
    );
    let armed = same.arm("shell", json!({})).expect("armed");
    assert_eq!(until_fired(&armed.condition), "the look came back");
    assert_eq!(
        warned(&hub, "plugin_look").len(),
        2,
        "a second reason is a second sentence: {:?}",
        warned(&hub, "plugin_look")
    );
}

/// A session with no gate runs no program a layer declared - *no gate* is not *every gate open*
/// which is the direction the whole entry fails in.
#[test]
fn a_session_with_no_gate_runs_no_exec_look() {
    use letibot_tools::exec::plugin_condition::PluginConditions as _;
    let root = tempdir("look-no-gate");
    let table = shell_layer(&root, "true", json!(["true"]));
    let host = Arc::new(table_host(&root, &table));
    let armed = conditions(&host).arm("shell", json!({})).expect("armed");
    assert_eq!(until_fired(&armed.condition), "the look came back");
    let seen = host.lock().rec.inputs("condition_shell").to_vec();
    let handed = seen
        .iter()
        .find(|i| !i["look"].is_null())
        .expect("the look's result was handed back");
    assert_eq!(handed["look"]["exit"], json!(126), "{handed}");
    assert!(
        handed["look"]["stderr"]
            .as_str()
            .is_some_and(|s| s.contains("no gate")),
        "{handed}"
    );
}

/// A gate that admits everything - the fixture the deferred look needs, since what that test is
/// about is the mutex, not the decision.
struct Admits;

impl letibot_tools::Gate for Admits {
    fn admit(&mut self, _call: &letibot_tools::GateCall<'_>) -> GateDecision {
        GateDecision::Admit
    }
}

/// A look is DEFERRED while the session's gate is busy with somebody else's card - not blocked
/// and not refused. `ToolRuntime::run` holds that mutex for minutes, and a look that waited
/// would park the session's monitor thread and every other watch on it, so it uses `try_lock`.
#[test]
fn an_exec_look_is_deferred_while_the_gate_is_busy() {
    use letibot_tools::exec::plugin_condition::PluginConditions as _;
    let root = tempdir("look-busy");
    // A program whose running leaves a file behind, so "did not run" is a fact.
    let marker = root.join("ran");
    let script = format!("touch {}", marker.display());
    let table = shell_layer(&root, "sh", json!(["sh", "-c", script]));
    let host = Arc::new(table_host(&root, &table));
    let hub = Hub::new("s-plugins");

    // The REAL runtime and the REAL `Unasked` impl: the deferral IS `try_lock` on this mutex
    // so a fixture gate would prove nothing about it.
    let backend = tempdir("look-busy-backend");
    let runtime = Arc::new(
        letibot_tools::ToolRuntime::new(
            letibot_tools::Registry::new(),
            Box::new(letibot_tools::HostBackend::new(&backend).expect("a backend")),
        )
        .with_gate(Box::new(Admits)),
    );
    let gate: Arc<dyn conditions::Unasked> = runtime.clone();
    let conditions = conditions::HostConditions::new(
        Arc::new(Mutex::new(Some(host.clone()))),
        hub.clone(),
        Some(gate),
    );

    // Hold the lock exactly as the model's own decision does, and fire the look from another
    // thread: it must come back promptly, with nothing run and nothing said.
    let held = runtime.gate.lock().unwrap_or_else(|e| e.into_inner());
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let conditions = conditions.clone();
        std::thread::spawn(move || {
            let armed = conditions.arm("shell", json!({})).expect("armed");
            let _ = tx.send(armed.condition.met());
        });
    }
    let said = rx.recv_timeout(Duration::from_secs(5)).expect(
        "a look waited on a gate another thread is holding — that is the session's monitor \
         thread parked behind somebody's card",
    );
    assert_eq!(said, None, "a deferred look is not a firing");
    assert!(
        !marker.exists(),
        "a look ran while the gate was busy deciding somebody else's call"
    );
    assert!(
        warned(&hub, "plugin_look").is_empty(),
        "a busy gate is not news for the plugin: {:?}",
        warned(&hub, "plugin_look")
    );
    assert!(
        host.lock()
            .rec
            .inputs("condition_shell")
            .iter()
            .all(|i| i["look"].is_null()),
        "a deferred look was handed to the layer as a result"
    );

    // And with the gate free the same look runs, which is what makes the assertions above
    // about the LOCK rather than about a look that never works. A fresh host, because the
    // fixture's script is spent one answer per arming.
    drop(held);
    let table = shell_layer(&root, "sh", json!(["sh", "-c", script]));
    let host = Arc::new(table_host(&root, &table));
    let gate: Arc<dyn conditions::Unasked> = runtime.clone();
    let conditions =
        conditions::HostConditions::new(Arc::new(Mutex::new(Some(host))), hub, Some(gate));
    let armed = conditions.arm("shell", json!({})).expect("armed");
    assert_eq!(until_fired(&armed.condition), "the look came back");
    assert!(
        marker.exists(),
        "the look did not run once the gate was free"
    );
}

// The room's pane: the feed the daemon builds, and the four keys it performs.

/// A project layer that answers `pane_room`, declares the four controls, and reads the `room`
/// feed - the manifest a project would write over the built-in.
fn room_project(tag: &str, answer: &str, actions: &str) -> PathBuf {
    let root = tempdir(tag);
    let layer = root.join(".letibot/plugins/room");
    std::fs::create_dir_all(&layer).expect("the layer dir");
    std::fs::write(
        layer.join("plugin.toml"),
        format!(
            "name = \"room\"\npurpose = \"the room\"\nhooks = [\"pane_room\"]\n\
             capabilities = []\nfeeds = [\"room\"]\nactions = [{actions}]\ncontract = 1\n"
        ),
    )
    .expect("a manifest");
    std::fs::write(layer.join("plugin.wasm"), b"a module").expect("a module");
    std::fs::write(layer.join("answer.json"), answer).expect("an answer");
    std::fs::write(layer.join("second.json"), answer).expect("a second answer");
    root
}

/// A real room on a socket - the thing a room pane is drawn from.
fn a_room(tag: &str) -> (PathBuf, letibot_room::Server) {
    let dir = tempdir(tag);
    let path = dir.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    (dir, server)
}

/// Wait for the pane's picture of the room to catch up: the feed is read on a thread of its
/// own, once a second, so a test waits for it rather than sleeping a fixed second.
fn until_the_room_is_read(link: &crate::room::link::SessionRoom, items: usize) {
    use crate::plugins::room_feed::RoomFeed as _;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snap = link.snapshot();
        if snap.unreadable.is_none() && snap.items.len() == items {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the room was never read: {:?}",
            snap.unreadable
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A module that answers the same JSON every time, for a test that presses more keys than it
/// scripted frames. [`Rounds`] is the right tool when the ANSWER is the subject; here the
/// subject is what the host does with a row, and the row is the same every time.
struct Same {
    hooks: Vec<String>,
    answer: String,
}

impl Module for Same {
    fn hooks(&self) -> Vec<String> {
        self.hooks.clone()
    }

    fn call(&self, _hook: &str, _input: &str) -> Result<Option<String>, String> {
        Ok(Some(self.answer.clone()))
    }

    fn fuel(&self) -> u64 {
        7
    }
}

struct SameLoader;

impl Loader for SameLoader {
    fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        let dir = wasm.parent().unwrap_or(Path::new("."));
        let manifest = Manifest::read(dir)?;
        let answer = std::fs::read_to_string(dir.join("answer.json"))
            .map_err(|e| format!("answer.json: {e}"))?;
        Ok(Box::new(Same {
            hooks: manifest.hooks.clone(),
            answer,
        }))
    }
}

/// A row's key is performed by the HOST, and the feed it was drawn from is the room's. The layer
/// answers a row carrying a verb and an id, and the daemon performs it: the room's own state
/// machine moves, over the session's connection.
#[test]
fn a_room_panes_key_is_performed_by_the_host() {
    let (dir, server) = a_room("room-key");
    // An item somebody else posted, so `s-room-pane` may claim the head of `ram`.
    let other = letibot_room::client::Client::connect(
        server.path(),
        letibot_room::Speaker::top_level("s-b"),
        Arc::new(|_| {}),
    )
    .expect("s-b connects");
    other
        .request(&letibot_room::Request::Post {
            id: "p1".into(),
            tag: "ram".into(),
            body: "8 GB".into(),
        })
        .expect("a post");
    let link = crate::room::link::SessionRoom::connect(
        "s-room-pane",
        server.path(),
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::new(|_: &str| {}),
        None,
    )
    .expect("the session connects");
    until_the_room_is_read(&link, 1);

    let answer = r#"{"title":"the room","panels":[{"id":"queues","title":"queues",
        "summary":[{"tree":{"rows":[{"id":"i1","cells":["8 GB"],
        "enter":{"verb":"claim","id":"i1"}}]}}]}]}"#;
    let root = room_project(
        "room-key",
        answer,
        "\"claim\", \"release\", \"abandon\", \"rule\"",
    );
    let hub = Hub::new("s-room-pane");
    let host = PluginHost::discover(
        "s-room-pane",
        &root,
        None,
        None,
        Arc::new(SameLoader),
        Arc::new(FileBuilder),
    )
    .with_room(link);

    host.open("s-room-pane", &hub, "room/room", (80, 24));
    // The feed the layer was handed is the room's: the item, its tag, and the room's own
    // identity sentence.
    let inputs = host.lock().rec.inputs("pane_room").to_vec();
    assert_eq!(inputs.len(), 1, "one render, one call");
    let feed = &inputs[0]["feeds"]["room"];
    assert_eq!(feed["here"], json!("s-room-pane"));
    assert!(
        feed["identity"]
            .as_str()
            .unwrap_or_default()
            .contains("no item store"),
        "{feed}"
    );
    assert_eq!(feed["tags"][0]["tag"], json!("ram"));
    assert_eq!(feed["tags"][0]["rows"][0]["entry"], json!("i1"));
    assert_eq!(feed["tags"][0]["rows"][0]["claimable"], json!(true));

    // And the key moves the room: the layer said *claim entry i1*, and the daemon performed it.
    host.action("s-room-pane", &hub, "room/room", "i1", "claim");
    assert_eq!(
        server.items()[0].holder(),
        Some("s-room-pane"),
        "the claim is on the session whose pane it is"
    );
    // The second render is the pane again, with the state the key made.
    let inputs = host.lock().rec.inputs("pane_room").to_vec();
    assert_eq!(inputs.len(), 2);
    assert_eq!(
        inputs[1]["feeds"]["room"]["tags"][0]["rows"][0]["claimant"],
        json!("s-room-pane"),
        "the frame after the key is the state the key made"
    );

    // A key the room refuses is said: a key that did nothing is indistinguishable from one
    // nobody pressed. `#2` is somebody else's, so a release of it is `NotHeld`.
    other
        .request(&letibot_room::Request::Post {
            id: "p2".into(),
            tag: "disk".into(),
            body: "40 GB".into(),
        })
        .expect("a second post");
    host.action("s-room-pane", &hub, "room/room", "i2", "release");
    let said = hub.retained();
    assert!(
        said.iter().any(|e| matches!(
            &e.event,
            SessionEvent::Warning { code, detail, .. }
                if code == "room_act_refused" && detail.contains("does not hold")
        )),
        "the room's refusal reaches the person: {:?}",
        said.iter()
            .map(|e| format!("{:?}", e.event))
            .collect::<Vec<_>>()
    );

    drop(other);
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The pane's restart is a FRAME to the supervisor, and its outcome is said. The control is not
/// a room verb: the layer names `restart` and the host asks the supervisor that starts
/// coordinators, and the note it leaves is in the next frame's feed.
#[test]
fn the_restart_control_asks_the_supervisor_and_its_outcome_is_said() {
    use crate::room::supervise::{RestartFrame, RoomSupervisor};
    use std::sync::Mutex as StdMutex;

    struct Ask(StdMutex<Vec<RestartFrame>>);
    impl RoomSupervisor for Ask {
        fn restart(&self, frame: &RestartFrame) -> Result<String, String> {
            self.0.lock().unwrap().push(frame.clone());
            Ok("the room was stopped and started again by a fixture".to_string())
        }
    }

    let (dir, server) = a_room("room-restart");
    let asked = Arc::new(Ask(StdMutex::new(Vec::new())));
    let link = crate::room::link::SessionRoom::connect(
        "s-room-pane",
        server.path(),
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::new(|_: &str| {}),
        Some(asked.clone()),
    )
    .expect("the session connects");
    until_the_room_is_read(&link, 0);

    // The layer answers ONE row: the restart, which is the fifth verb and not a room act.
    let answer = r#"{"title":"the room","panels":[{"id":"server","key":"s",
        "title":"the server","summary":[{"tree":{"rows":[{"id":"restart","cells":["restart"],
        "enter":{"verb":"restart","id":"restart"}}]}}]}]}"#;
    let root = room_project(
        "room-restart",
        answer,
        "\"claim\", \"release\", \"abandon\", \"rule\", \"restart\"",
    );
    let hub = Hub::new("s-room-pane");
    let host = PluginHost::discover(
        "s-room-pane",
        &root,
        None,
        None,
        Arc::new(SameLoader),
        Arc::new(FileBuilder),
    )
    .with_room(link);
    host.open("s-room-pane", &hub, "room/room", (80, 24));

    // The act runs off the connection's thread - a launcher that overran its minute must not
    // leave the person's keys queued behind it - so the test waits for the frame.
    host.action("s-room-pane", &hub, "room/room", "restart", "restart");
    let until = |what: &str, f: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("{what} never happened");
    };
    until("the supervisor is asked", &|| {
        !asked.0.lock().unwrap().is_empty()
    });
    // And a second press while one is running is refused rather than queued: one room, one
    // restart.
    let frames = asked.0.lock().unwrap().clone();
    assert_eq!(frames.len(), 1, "one press, one frame");
    assert_eq!(frames[0].by, "s-room-pane");
    assert!(
        frames[0].why.contains("pressed restart in the room pane"),
        "{}",
        frames[0].why
    );
    assert!(
        !frames[0].was_running,
        "the fixture's socket has no lease, so nothing was running to stop"
    );

    // The outcome is said, not swallowed: a key whose result nobody printed is a key a person
    // cannot trust.
    until("the outcome is said", &|| {
        !warned(&hub, "room_restarted").is_empty()
    });
    // And the note is in the next frame: when, and why. The pane's own clock asks for it in
    // production; the test asks, which is the same render on the same path.
    host.open("s-room-pane", &hub, "room/room", (80, 24));
    let inputs = host.lock().rec.inputs("pane_room").to_vec();
    let feed = &inputs.last().expect("a render after the key")["feeds"]["room"];
    assert_eq!(feed["restart"]["available"], json!(true));
    let note = &feed["restart"]["marker"];
    assert_eq!(
        note["cause"],
        json!("daemon"),
        "nothing was running: {note}"
    );
    assert_eq!(note["by"], json!("s-room-pane"));
    assert!(
        note["sentence"]
            .as_str()
            .unwrap_or_default()
            .contains("by the daemon"),
        "the note says its cause in its own words - what survives a restart is the identity's \
         to say, not the note's: {note}"
    );

    // A supervisor that refuses is a notice too: the middle register, with the reason.
    struct Refuse;
    impl RoomSupervisor for Refuse {
        fn restart(&self, _frame: &RestartFrame) -> Result<String, String> {
            Err("`letibot --room --stop` answered exit status: 1: no room on this box".into())
        }
    }
    let link = crate::room::link::SessionRoom::connect(
        "s-room-pane",
        server.path(),
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::new(|_: &str| {}),
        Some(Arc::new(Refuse)),
    )
    .expect("the session connects");
    until_the_room_is_read(&link, 0);
    let host = PluginHost::discover(
        "s-room-pane",
        &root,
        None,
        None,
        Arc::new(SameLoader),
        Arc::new(FileBuilder),
    )
    .with_room(link);
    let hub = Hub::new("s-room-pane");
    host.open("s-room-pane", &hub, "room/room", (80, 24));
    host.action("s-room-pane", &hub, "room/room", "restart", "restart");
    until("the refusal is said", &|| {
        !warned(&hub, "room_restart_refused").is_empty()
    });
    let refused = warned(&hub, "room_restart_refused");
    assert!(
        refused.iter().any(|d| d.contains("no room on this box")),
        "{refused:?}"
    );

    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A row carrying a verb the manifest did not declare is refused before it is drawn - the gate
/// `View::validate_for` keeps, on the daemon's side of the wire.
#[test]
fn a_room_row_reaching_an_undeclared_verb_is_refused() {
    let answer = r#"{"title":"the room","panels":[{"id":"queues","title":"queues",
        "summary":[{"tree":{"rows":[{"id":"i1","cells":["8 GB"],
        "enter":{"verb":"veto","id":"i1"}}]}}]}]}"#;
    // The manifest declares the four, and `veto` is not one of them.
    let root = room_project(
        "room-veto",
        answer,
        "\"claim\", \"release\", \"abandon\", \"rule\"",
    );
    let hub = Hub::new("s-plugins");
    let host = PluginHost::discover(
        "s-plugins",
        &root,
        None,
        None,
        Arc::new(RoundLoader),
        Arc::new(FileBuilder),
    );
    host.open("s-plugins", &hub, "room/room", (80, 24));
    let published = views(&hub);
    let (_, view) = published.last().expect("a frame");
    assert!(
        view["panels"][0]["unavailable"]["why"]
            .as_str()
            .unwrap_or_default()
            .contains("does not declare that action"),
        "the pane says why instead of drawing the row: {view}"
    );
    assert!(
        !warned(&hub, "plugin_view_refused").is_empty(),
        "and the session's model is told once"
    );

    // The restart is a declared action like the four, and it is refused like them too: a layer
    // that ships its own room pane and does not list `restart` gets its row refused, because the
    // gate is the manifest and not the verb's familiarity.
    let answer = r#"{"title":"the room","panels":[{"id":"server","key":"s",
        "title":"the server","summary":[{"tree":{"rows":[{"id":"restart","cells":["restart"],
        "enter":{"verb":"restart","id":"restart"}}]}}]}]}"#;
    let root = room_project(
        "room-no-restart",
        answer,
        "\"claim\", \"release\", \"abandon\", \"rule\"",
    );
    let hub = Hub::new("s-plugins");
    let host = PluginHost::discover(
        "s-plugins",
        &root,
        None,
        None,
        Arc::new(RoundLoader),
        Arc::new(FileBuilder),
    );
    host.open("s-plugins", &hub, "room/room", (80, 24));
    let published = views(&hub);
    let (_, view) = published.last().expect("a frame");
    assert!(
        view["panels"][0]["unavailable"]["why"]
            .as_str()
            .unwrap_or_default()
            .contains("does not declare that action"),
        "an undeclared `restart` is refused by the same gate: {view}"
    );
}

/// The committed `room` module runs, against a real room, through the daemon's own host. A guest
/// that compiles is not a guest that has run - the example's committed `.wasm` had never been
/// executed and trapped on its first call - so this asserts the answer is the LAYER's.
#[test]
fn a_room_linked_after_the_host_was_built_gets_its_pane() {
    let (_dir, server) = a_room("room-late-pane");
    let workspace = tempdir("room-late-pane-ws");
    let host = PluginHost::production("s-late-pane", &workspace);
    let hub = Hub::new("s-late-pane");
    // A built-in pane is the binary's, so it is here before any room is: the screen says which half
    // is missing instead of the pane not existing at all (R151).
    host.open("s-late-pane", &hub, "room/room", (100, 30));
    assert!(
        host.panes().iter().any(|p| p == "room/room"),
        "{:?}",
        host.panes()
    );
    let drawn = serde_json::to_string(&views(&hub)).unwrap_or_default();
    assert!(
        drawn.contains("hands no `room` feed"),
        "the layer's own sentence, not the host's refusal: {drawn}"
    );
    let link = crate::room::link::SessionRoom::connect(
        "s-late-pane",
        server.path(),
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::new(|_: &str| {}),
        None,
    )
    .expect("the session connects");
    host.link_room(link, &hub);
    assert!(
        host.panes().iter().any(|p| p == "room/room"),
        "{:?}",
        host.panes()
    );
    let drawn = views(&hub)
        .into_iter()
        .rfind(|(p, _)| p == "room/room")
        .map(|(_, view)| serde_json::to_string(&view).unwrap_or_default())
        .unwrap_or_default();
    assert!(
        !drawn.contains("hands no `room` feed"),
        "and the connection that arrives is drawn: {drawn}"
    );
}

#[test]
fn the_builtin_room_layer_answers_its_pane_with_a_real_room_behind_it() {
    let (dir, server) = a_room("room-builtin");
    let other = letibot_room::client::Client::connect(
        server.path(),
        letibot_room::Speaker::top_level("s-b"),
        Arc::new(|_| {}),
    )
    .expect("s-b connects");
    other
        .request(&letibot_room::Request::Post {
            id: "p1".into(),
            tag: "ram".into(),
            body: "8 GB, ~10 min".into(),
        })
        .expect("a post");
    let link = crate::room::link::SessionRoom::connect(
        "s-room-builtin",
        server.path(),
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::new(|_: &str| {}),
        None,
    )
    .expect("the session connects");
    until_the_room_is_read(&link, 1);

    let workspace = tempdir("room-builtin-ws");
    let host = PluginHost::production("s-room-builtin", &workspace).with_room(link);
    let hub = Hub::new("s-room-builtin");
    // Nothing is compiled until a head looks: the room's module is 218 KB and costs seconds of
    // CPU in a debug host, so a session nobody attaches to must not pay for it.
    assert!(
        host.panes().is_empty(),
        "a session with no head has no pane: {:?}",
        host.panes()
    );
    host.open("s-room-builtin", &hub, "room/room", (100, 30));
    assert!(
        host.panes().iter().any(|p| p == "room/room"),
        "and the door that asks for it is the door that installs it: {:?}",
        host.panes()
    );

    let (pane, view) = views(&hub)
        .into_iter()
        .rfind(|(p, _)| p == "room/room")
        .expect("the pane answered");
    assert_eq!(pane, "room/room");
    let drawn = serde_json::to_string(&view).unwrap();
    assert!(
        view["panels"][0].get("unavailable").is_none(),
        "the layer answered, rather than the host's refusal panel: {drawn}"
    );
    assert!(drawn.contains("`ram`"), "the tag is drawn: {drawn}");
    assert!(
        drawn.contains("8 GB, ~10 min"),
        "the item is drawn: {drawn}"
    );
    assert!(
        drawn.contains("no item store"),
        "and the room's own identity, which is what keeps an empty queue from reading as a \
         quiet one: {drawn}"
    );
    // The row's key is the manifest's verb, so a press would reach the host's act.
    assert!(drawn.contains("\"verb\":\"claim\""), "{drawn}");

    drop(other);
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------- X23 and X21: command_ and lifecycle_

/// A project with ONE layer that exports `hooks` and answers every one of them with `answer.json` -
/// and none when the file is not there, which is a layer that traps.
fn layer_project(
    tag: &str,
    hooks: &[&str],
    actions: &[&str],
    answer: Option<&str>,
) -> (PathBuf, Arc<Hub>) {
    let root = tempdir(tag);
    let layer = root.join(".letibot/plugins/standup");
    std::fs::create_dir_all(&layer).expect("the layer dir");
    let hooks: Vec<String> = hooks.iter().map(|h| format!("\"{h}\"")).collect();
    let actions: Vec<String> = actions.iter().map(|a| format!("\"{a}\"")).collect();
    std::fs::write(
        layer.join("plugin.toml"),
        format!(
            "name = \"standup\"\npurpose = \"a project's own verbs and boundaries\"\n\
             hooks = [{}]\ncapabilities = []\nactions = [{}]\ncontract = 1\n",
            hooks.join(", "),
            actions.join(", ")
        ),
    )
    .expect("a manifest");
    std::fs::write(layer.join("plugin.wasm"), b"a module").expect("a module");
    if let Some(answer) = answer {
        std::fs::write(layer.join("answer.json"), answer).expect("an answer");
    }
    (root, Hub::new("s-plugins"))
}

/// `/verb …` through the daemon's unknown-verb fallback, as the answer a person reads.
fn unknown_verb(host: &PluginHost, hub: &Arc<Hub>, verb: &str, line: &str) -> String {
    crate::slash::unknown_verb(Some(host), Some(hub), host.session(), verb, line)
        .lines
        .join("\n")
}

/// A `/` verb the daemon has not got is offered to `command_<verb>`, and the act the daemon can
/// perform - a sentence - is the answer (X23). A verb nothing answers is still the sentence it has
/// always been.
#[test]
fn a_command_layer_answers_an_unknown_verb() {
    let (root, hub) = layer_project(
        "command-answered",
        &["command_standup"],
        &["say"],
        Some(r#"{"act":{"name":"say","args":{"text":"standup: yesterday, today, blockers"}}}"#),
    );
    let layer = host(&root);
    assert_eq!(
        unknown_verb(&layer, &hub, "standup", "yesterday today blockers"),
        "standup: yesterday, today, blockers"
    );
    // A verb this stack has no layer for is the sentence an unknown verb has always answered, and
    // it is the same function that answered the one above.
    assert_eq!(
        unknown_verb(&layer, &hub, "nonesuch", ""),
        "/nonesuch is not a daemon verb; /help lists the head's"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A layer that traps leaves exactly today's sentence, and says so once: the fallback is never
/// broken by a layer, and never silently swallowed either.
#[test]
fn a_trapping_command_layer_leaves_the_unknown_sentence() {
    let (root, hub) = layer_project("command-traps", &["command_standup"], &["say"], None);
    let layer = host(&root);
    assert_eq!(
        unknown_verb(&layer, &hub, "standup", "x"),
        "/standup is not a daemon verb; /help lists the head's"
    );
    // The trap is on the session's log rather than swallowed: a broken layer is news the model is
    // owed, and `plugin load` is where it is fixed.
    let retained = format!("{:?}", hub.retained());
    assert!(
        retained.contains("plugin_layer") && retained.contains("command_standup"),
        "the trap is said, naming the hook: {retained}"
    );
    let _ = std::fs::remove_dir_all(&root);

    // An act the daemon does not perform is refused by name, and nothing runs.
    let (root, hub) = layer_project(
        "command-unknown-act",
        &["command_standup"],
        &["say", "add_todo"],
        Some(r#"{"act":{"name":"add_todo","args":{"content":"buy milk"}}}"#),
    );
    let layer = host(&root);
    let said = unknown_verb(&layer, &hub, "standup", "buy milk");
    assert!(said.contains("add_todo"), "the act is named: {said}");
    assert!(said.contains("only `say`"), "and what it does: {said}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A lifecycle layer cannot block a re-seat (X21). Every shape a hostile layer can answer with is
/// read as acts to perform or as nothing at all: the answer type has no field for a veto, and the
/// host's answer for the event carries none either - so a caller that has already re-seated has
/// nothing to check before deciding it happened.
#[test]
fn a_lifecycle_layer_cannot_block_a_reseat() {
    let input = letibot_plugin::lifecycle::Input {
        v: letibot_plugin::V,
        event: "reseat".into(),
        session: "s-plugins".into(),
        stale: letibot_plugin::lifecycle::Stale {
            prompt: Some("the tool list moved: `bash` left the seat".into()),
            tools: vec!["bash".into()],
        },
        rows: Vec::new(),
        now_ms: Some(1_762_000_000_000),
    };
    let input = serde_json::to_value(&input).expect("the input serialises");
    for (answer, answered) in [
        // Words that are not a lifecycle answer, and would be a veto if one were possible.
        (r#"{"block":true}"#, true),
        (r#"{"pass":false,"refuse":"do not reseat"}"#, true),
        // `mask` stops the stack below, which is the one thing it does here: the layer answered by
        // saying nothing below it may answer, and the event still happened.
        (r#"{"mask":true}"#, false),
        // A well-formed answer with nothing in it.
        (r#"{"acts":[]}"#, true),
    ] {
        let (root, hub) = layer_project(
            "lifecycle-hostile",
            &["lifecycle_reseat"],
            &["say"],
            Some(answer),
        );
        let layer = host(&root);
        let id = layer.layer("standup").expect("the layer").id.clone();
        let out = layer.lifecycle(&hub, "reseat", &input);
        assert_eq!(
            layer.recorded("lifecycle_reseat").len(),
            1,
            "the layer WAS asked `{answer}`, which is what makes this a test of its answer"
        );
        assert_eq!(
            out.by.as_deref(),
            answered.then_some(id.as_str()),
            "`{answer}`: the host read it as an answer (or as a mask cutting off the stack)"
        );
        assert!(
            out.acts.is_empty(),
            "`{answer}` asks for nothing to be performed: {:?}",
            out.acts
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // And a layer that answers as designed adds its sentence without the event being anything the
    // host could hold back: the act comes back for the caller to perform.
    let (root, hub) = layer_project(
        "lifecycle-says",
        &["lifecycle_reseat", "lifecycle_close"],
        &["say"],
        Some(
            r#"{"acts":[{"verb":"say","args":{"text":"the tool list moved: `bash` left the seat"}}]}"#,
        ),
    );
    let layer = host(&root);
    assert_eq!(
        layer.lifecycle(&hub, "reseat", &input).acts,
        vec![(
            "say".to_string(),
            serde_json::json!({ "text": "the tool list moved: `bash` left the seat" })
        )]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A lifecycle answer naming an act its manifest did not declare is the layer's bug and loses the
/// whole answer - the same rule a pane's view has, and the reason `actions` is a reviewed list.
#[test]
fn a_lifecycle_act_the_manifest_did_not_declare_performs_nothing() {
    let input = serde_json::json!({ "v": 1, "event": "open", "session": "s-plugins" });
    let (root, hub) = layer_project(
        "lifecycle-undeclared",
        &["lifecycle_open"],
        &["say"],
        Some(r#"{"acts":[{"verb":"room_say","args":{"text":"hello"}}]}"#),
    );
    let layer = host(&root);
    let out = layer.lifecycle(&hub, "open", &input);
    assert!(out.acts.is_empty(), "nothing is performed: {:?}", out.acts);
    let retained = hub.retained();
    assert!(
        format!("{retained:?}").contains("room_say"),
        "and the person is told which act: {retained:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Every verb the daemon answers is one a layer cannot take: `HOST_VERBS` is checked against the
/// daemon's own table here, so a new `/verb` cannot arrive without being added to it - the same rule
/// `HOST_KEYS` has, one namespace over. The head's half of the list is checked in the head's crate.
#[test]
fn every_daemon_verb_is_one_a_layer_cannot_take() {
    for verb in crate::slash::VERBS {
        assert!(
            letibot_plugin::command::is_host_verb(verb),
            "`/{verb}` is a daemon verb and `HOST_VERBS` does not name it"
        );
    }
}

// K8: the reserved prefixes get their callers.

/// A project with one layer that answers one hook, for the kinds that carry no pane.
fn hook_project(tag: &str, hook: &str, answer: &str) -> (PathBuf, Arc<Hub>) {
    let root = tempdir(tag);
    let layer = root.join(".letibot/plugins").join(hook);
    std::fs::create_dir_all(&layer).expect("the layer dir");
    std::fs::write(
        layer.join("plugin.toml"),
        format!(
            "name = \"{hook}\"\npurpose = \"one hook, for a test\"\nhooks = [\"{hook}\"]\n\
             capabilities = [\"read\"]\ncontract = 1\n"
        ),
    )
    .expect("a manifest");
    std::fs::write(layer.join("plugin.wasm"), b"a module").expect("a module");
    std::fs::write(layer.join("answer.json"), answer).expect("an answer");
    (root, Hub::new("s-plugins"))
}

/// A tool that prints `bytes` of one character: what a model's `bash` call produces when a test
/// is compiling, and what a digest exists to shorten.
struct Loud {
    bytes: usize,
}

impl letibot_tools::runtime::Tool for Loud {
    fn schema(&self) -> letibot_tools::schema::ToolSchema {
        letibot_tools::schema::ToolSchema::new(
            "loud",
            "print a long line, for a digest to shorten",
            json!({"type": "object", "properties": {}}),
            Access::Read,
        )
    }

    fn invoke(
        &self,
        _ctx: &mut letibot_tools::runtime::InvokeCtx<'_>,
        _args: &Value,
    ) -> letibot_tools::runtime::Invocation {
        letibot_tools::runtime::Invocation::ok("x".repeat(self.bytes))
    }
}

/// A runtime over one session's host, with [`Loud`] seated and a 200-byte inline budget - so which
/// bytes were budgeted is readable off the result.
fn loud_runtime(
    host: Arc<PluginHost>,
    hub: &Arc<Hub>,
    bytes: usize,
) -> letibot_tools::runtime::ToolRuntime {
    let slot: Arc<std::sync::Mutex<Option<Arc<PluginHost>>>> =
        Arc::new(std::sync::Mutex::new(Some(host)));
    let mut reg = letibot_tools::runtime::Registry::new();
    reg.register(Box::new(Loud { bytes }))
        .expect("it registers");
    let workspace = tempdir("digest-ws");
    let backend =
        letibot_tools::backend::HostBackend::new(&workspace).expect("a whole-host backend");
    let runtime = letibot_tools::runtime::ToolRuntime::new(reg, Box::new(backend))
        .with_gate(letibot_tools::testing::allow_all())
        .with_spiller(letibot_tools::Spiller::new(
            Box::new(letibot_tools::FixedBudget(200)),
            Box::new(letibot_tools::MemoryStore::new()),
        ));
    // The digest hook only, so what this measures is the digest and not the call path.
    let slot: Arc<std::sync::Mutex<Option<Arc<PluginHost>>>> = slot;
    let hub = hub.clone();
    runtime.with_digest_hook(Arc::new(crate::plugins::DigestHooks::new(slot, hub)))
}

fn loud_call() -> letibot_transcript::ToolCall {
    letibot_transcript::ToolCall {
        id: "c1".into(),
        name: "loud".into(),
        arguments: "{}".into(),
    }
}

/// The digest runs BEFORE the spill, so the bytes the budget is spent on are the digest's: a
/// 4000-byte output a layer shortens to one line does not spill at all (§4.5, X26).
#[test]
fn a_digest_layer_shortens_before_the_spill_budgets() {
    let (root, hub) = hook_project(
        "digest-shortens",
        "digest_compilers",
        r#"{"payload":"compiled 2 crates, 0 errors"}"#,
    );
    let host = Arc::new(host(&root));
    let rt = loud_runtime(host.clone(), &hub, 4000);
    let mut sink = letibot_tools::events::RecordingToolSink::new();
    let out = rt.invoke("t1", &loud_call(), &mut sink);
    assert_eq!(out.payload, "compiled 2 crates, 0 errors");
    assert!(
        out.spill.is_none(),
        "the digest fits the budget, so nothing spilled: {:?}",
        out.spill
    );
    assert!(
        out.notes.iter().any(|n| n.contains("digested by plugin")),
        "the model is told whose words these are: {:?}",
        out.notes
    );
    // And the layer was handed the output whole - a digest of something it never saw would be a
    // digest of a guess.
    let asked = host.recorded("digest_compilers");
    assert_eq!(
        asked
            .last()
            .and_then(|i| i["payload"].as_str())
            .map(str::len),
        Some(4000),
        "{asked:?}"
    );
    assert_eq!(asked.last().unwrap()["call"]["name"], json!("loud"));
}

/// The other half of the same fact: with no digest layer the same output spills, so the test above
/// is measuring the hook rather than a budget nobody reached.
#[test]
fn without_a_digest_layer_the_same_output_spills() {
    let (root, hub) = hook_project("digest-none", "pane_x", "{}");
    let rt = loud_runtime(Arc::new(host(&root)), &hub, 4000);
    let mut sink = letibot_tools::events::RecordingToolSink::new();
    let out = rt.invoke("t1", &loud_call(), &mut sink);
    assert!(out.spill.is_some(), "nothing shortened it: {:?}", out.spill);
    assert!(out.payload.len() < 4000, "and the body is bounded");
}

/// A digest that traps leaves today's output: the call's own bytes, budgeted as they were, and the
/// session told once that the layer failed.
#[test]
fn a_trapping_digest_layer_leaves_the_output_as_it_was() {
    let (root, hub) = hook_project("digest-trap", "digest_compilers", "{}");
    std::fs::write(root.join(".letibot/plugins/digest_compilers/TRAP"), b"").expect("a trap");
    let host = Arc::new(host(&root));
    let rt = loud_runtime(host.clone(), &hub, 4000);
    let mut sink = letibot_tools::events::RecordingToolSink::new();
    let out = rt.invoke("t1", &loud_call(), &mut sink);
    assert!(
        out.spill.is_some(),
        "the tool's own output is what was budgeted: {:?}",
        out.spill
    );
    assert!(
        !out.notes.iter().any(|n| n.contains("digested by plugin")),
        "nothing was digested: {:?}",
        out.notes
    );
    let said = host.take_notices().join("\n");
    assert!(said.contains("digest_compilers"), "{said}");
    assert!(said.contains("trapped"), "{said}");
}

/// A pinned tool layer's `exec` look runs: the capability first, then `admit_unasked` - a look
/// runs inside a model's call and cannot park it behind a card (X26, §7.11).
#[test]
fn a_pinned_tool_layer_runs_an_exec_look_through_admit_unasked() {
    struct Asked(std::sync::Mutex<Vec<String>>);
    impl crate::plugins::tool::Decides for Asked {
        fn admit_unasked(&self, name: &str, access: Access, args: &Value) -> Option<GateDecision> {
            self.0.lock().unwrap().push(format!(
                "{name}/{access:?} {}",
                args["command"].as_str().unwrap_or_default()
            ));
            Some(GateDecision::Admit)
        }

        fn run(&self, argv: &[String]) -> Result<letibot_tools::backend::Output, String> {
            Ok(letibot_tools::backend::Output {
                stdout: argv.join(" ").into_bytes(),
                stderr: Vec::new(),
                exit: 0,
            })
        }
    }

    let (root, declared) = tool_project(
        "tool-exec",
        r#"{"looks": [{"exec": ["sh", "-c", "printf exec-ran"]}]}"#,
        r#"{"outcome":"ok","payload":"answered"}"#,
        "\"exec:sh\"",
    );
    let hub = Hub::new("s-tool");
    let host = Arc::new(
        PluginHost::discover(
            "s-tool",
            &root,
            None,
            None,
            Arc::new(RoundLoader),
            Arc::new(FileBuilder),
        )
        .with_trust({
            let trust = letibot_plugins::Trust::in_memory();
            trust
                .pin(
                    &declared[0].layer,
                    &root.join(".letibot/plugins").join(&declared[0].layer),
                )
                .expect("pinned");
            trust
        }),
    );
    let slot: Arc<std::sync::Mutex<Option<Arc<PluginHost>>>> =
        Arc::new(std::sync::Mutex::new(Some(host.clone())));
    let tool = crate::plugins::tool::PluginTool::new(
        declared[0].clone(),
        "s-tool",
        root.clone(),
        slot,
        hub.clone(),
    );
    let asked = Asked(std::sync::Mutex::new(Vec::new()));
    let out = tool.call(Some(&asked), &json!({}));
    assert_eq!(out.payload, "answered", "{:?}", out.outcome);
    assert_eq!(
        *asked.0.lock().unwrap(),
        vec!["bash/Exec sh -c 'printf exec-ran'".to_string()],
        "the look was put to the gate without a card, as the call `bash` of that argv would be"
    );
    // And what it printed reached the layer: the look's result is the second input.
    let asked = host.recorded("tool_probe");
    let look = asked
        .last()
        .and_then(|i| i["looks"][0].as_object())
        .cloned()
        .expect("the look's result was handed back");
    assert_eq!(json!(look.get("exit")), json!(0));
    assert_eq!(json!(look.get("stdout")), json!("sh -c printf exec-ran"));
}

/// A read look answers what the session's view holds and not the daemon's disk: the file is inside
/// the session's own root and on the host, the store is on every seat's never-grant list, and the
/// look is refused in the fence's own words (R121 item 8's family, X26).
#[test]
fn a_tool_read_look_in_a_confined_session_reads_the_view_not_the_host() {
    // The letibot store, inside this session's workspace so that a plain `read` of the file works:
    // what refuses it is the view and nothing else.
    let (root, declared) = tool_project(
        "tool-view",
        r#"{"looks": [{"read": ".store/sessions/other/secret.json"}]}"#,
        r#"{"outcome":"ok","payload":"answered"}"#,
        "\"read\"",
    );
    let store = root.join(".store");
    let secret = store.join("sessions/other/secret.json");
    std::fs::create_dir_all(secret.parent().unwrap()).expect("the other session's dir");
    std::fs::write(&secret, "{\"not\":\"for you\"}").expect("a file on the host");
    let hub = Hub::new("s-tool");
    let host = Arc::new(
        PluginHost::discover(
            "s-tool",
            &root,
            None,
            None,
            Arc::new(RoundLoader),
            Arc::new(FileBuilder),
        )
        .with_trust({
            let trust = letibot_plugins::Trust::in_memory();
            trust
                .pin(
                    &declared[0].layer,
                    &root.join(".letibot/plugins").join(&declared[0].layer),
                )
                .expect("pinned");
            trust
        }),
    );
    let slot: Arc<std::sync::Mutex<Option<Arc<PluginHost>>>> =
        Arc::new(std::sync::Mutex::new(Some(host.clone())));
    let mut reg = letibot_tools::runtime::Registry::new();
    reg.register(Box::new(crate::plugins::tool::PluginTool::new(
        declared[0].clone(),
        "s-tool",
        root.clone(),
        slot,
        hub.clone(),
    )))
    .expect("the tool is seated");
    // The fence a confined seat draws: the store reaches this session's own directory and nothing
    // else, which is the same list a command's view obeys.
    let fence = letibot_tools::exec::fence::FileFence::default().owning(&store, "s-mine");
    let backend = letibot_tools::backend::HostBackend::new(&root)
        .expect("a backend")
        .with_file_fence(fence);
    let rt = letibot_tools::runtime::ToolRuntime::new(reg, Box::new(backend))
        .with_gate(letibot_tools::testing::allow_all());
    let mut sink = letibot_tools::events::RecordingToolSink::new();
    let out = rt.invoke(
        "t1",
        &letibot_transcript::ToolCall {
            id: "c1".into(),
            name: "probe".into(),
            arguments: "{}".into(),
        },
        &mut sink,
    );
    assert_eq!(out.payload, "answered", "{:?}", out.outcome);
    let asked = host.recorded("tool_probe");
    let look = asked
        .last()
        .and_then(|i| i["looks"][0].as_object())
        .cloned()
        .expect("the look's result was handed back");
    assert_eq!(json!(look.get("exit")), json!(1), "{look:?}");
    let why = look
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        why.contains("filesystem view") && why.contains(&store.display().to_string()),
        "the boundary's own sentence, naming the store: {why}"
    );
    // And the file is readable from here: a look that read the daemon's disk would have answered it.
    assert!(
        std::fs::read_to_string(&secret).is_ok(),
        "the file is on the daemon's disk"
    );
}

/// A read look of a path OUTSIDE the session's root is refused the way the file tools refuse it: a
/// confined seat cannot read past its root, and the daemon's own view of the disk is not the
/// session's (the bug X26 names).
#[test]
fn a_tool_read_look_outside_the_sessions_root_is_refused() {
    let elsewhere = tempdir("tool-view-outside");
    let secret = elsewhere.join("secret.json");
    std::fs::write(&secret, "{\"not\":\"for you\"}").expect("a file on the host");
    let (root, declared) = tool_project(
        "tool-view-outside",
        &format!("{{\"looks\": [{{\"read\": \"{}\"}}]}}", secret.display()),
        r#"{"outcome":"ok","payload":"answered"}"#,
        &format!("\"read:{}\"", elsewhere.display()),
    );
    let hub = Hub::new("s-tool");
    let host = Arc::new(
        PluginHost::discover(
            "s-tool",
            &root,
            None,
            None,
            Arc::new(RoundLoader),
            Arc::new(FileBuilder),
        )
        .with_trust({
            let trust = letibot_plugins::Trust::in_memory();
            trust
                .pin(
                    &declared[0].layer,
                    &root.join(".letibot/plugins").join(&declared[0].layer),
                )
                .expect("pinned");
            trust
        }),
    );
    let slot: Arc<std::sync::Mutex<Option<Arc<PluginHost>>>> =
        Arc::new(std::sync::Mutex::new(Some(host.clone())));
    let mut reg = letibot_tools::runtime::Registry::new();
    reg.register(Box::new(crate::plugins::tool::PluginTool::new(
        declared[0].clone(),
        "s-tool",
        root.clone(),
        slot,
        hub.clone(),
    )))
    .expect("the tool is seated");
    let backend = letibot_tools::backend::HostBackend::new(&root).expect("a rooted backend");
    let rt = letibot_tools::runtime::ToolRuntime::new(reg, Box::new(backend))
        .with_gate(letibot_tools::testing::allow_all());
    let mut sink = letibot_tools::events::RecordingToolSink::new();
    let out = rt.invoke(
        "t1",
        &letibot_transcript::ToolCall {
            id: "c1".into(),
            name: "probe".into(),
            arguments: "{}".into(),
        },
        &mut sink,
    );
    assert_eq!(out.payload, "answered", "{:?}", out.outcome);
    let asked = host.recorded("tool_probe");
    let look = asked
        .last()
        .and_then(|i| i["looks"][0].as_object())
        .cloned()
        .expect("the look's result was handed back");
    assert_eq!(json!(look.get("exit")), json!(1), "{look:?}");
    let why = look
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        why.contains("outside the session root"),
        "the backend's own sentence: {why}"
    );
    assert!(
        std::fs::read_to_string(&secret).is_ok(),
        "the file is on the daemon's disk"
    );
}
