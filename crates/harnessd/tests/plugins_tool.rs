//! **The `plugin` tool, from the model's side** — every action, through the real desk, the real
//! plugin host and the real stack.
//!
//! # What is faked, and why the fakes are where they are
//!
//! `plugin load` runs a module and `plugin build` runs `cargo`. A test that did both for every
//! assertion would need the wasm target, a rano checkout and three seconds per build — so the
//! `Loader`/`Builder` seams (`docs/plugins-slice1.md` C2) are what most of this file replaces:
//! a layer answers from files beside it, and a build writes its own module. That is the same
//! bargain `crates/plugins`' own tests make, one level up, and it is what lets a refusal — a view
//! the host will not draw, a module that traps — be asserted without compiling anything.
//!
//! **One test runs the whole loop for real** (`the_whole_loop_writes_a_pane_that_a_head_can_draw`):
//! `scaffold` → `build` (cargo, `wasm32-unknown-unknown`, offline) → `load` (rano compiling the
//! guest) → the pane published on the hub. It skips through `apparatus::present` when the wasm
//! target or the rano SDK is not on this machine, because both are provisionable apparatus rather
//! than logic — a skip that says so, and that `LETIBOT_REQUIRE_APPARATUS=1` turns into a failure
//! so a box that HAS them can prove it.
//!
//! # Nothing here talks to a model or a daemon
//!
//! The hub is a real `Hub` (that is where a pane is published), the stack is a real `Stack`, and
//! the tool is invoked through a real `ToolRuntime` with a gate that admits — so a refusal in
//! these tests is the plugin tooling's own and never the boundary's.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_harnessd::plugins::desk::SessionPlugins;
use letibot_harnessd::plugins::scaffold;
use letibot_harnessd::plugins::{PluginHost, session_plugins_dir};
use letibot_plugins::{BuildError, Builder, Capability, Loader, Manifest, Module, WASM};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_tools::backend::HostBackend;
use letibot_tools::builtins::plugin::{PluginDesk, PluginHooksTool, PluginTool};
use letibot_tools::runtime::{Registry, ToolRuntime};
use letibot_transcript::{ToolCall, ToolOutcome};
use serde_json::Value;

/// The session every test in this file is.
const SESSION: &str = "s-plugins-tool";

// ---------------------------------------------------------------------------------------------
// A temp tree
// ---------------------------------------------------------------------------------------------

struct Tmp {
    path: PathBuf,
}

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let path = std::env::temp_dir().join(format!(
            "letibot-plugins-tool-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("project")).expect("the temp tree");
        std::fs::create_dir_all(path.join("plugins")).expect("the session's plugin dir");
        Tmp { path }
    }

    fn project(&self) -> PathBuf {
        self.path.join("project")
    }

    /// The session's own plugin directory — what `PluginHost::for_session` discovers and what
    /// `plugin scaffold` writes into.
    fn plugins(&self) -> PathBuf {
        self.path.join("plugins")
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------------------------
// The fakes: a layer that answers from files, and a build that writes its own module
// ---------------------------------------------------------------------------------------------

/// **A module that answers from files beside it.** `answer.json` is what the layer says;
/// `second.json`, when it exists, is what it says once a look's result is in the input (the shape
/// the example dashboard has: one frame that asks for the file, one that draws it). A `TRAP` file
/// makes every call a trap, which is how `load`'s refusal of a trapping module is asserted.
struct FileModule {
    hooks: Vec<String>,
    dir: PathBuf,
}

impl Module for FileModule {
    fn hooks(&self) -> Vec<String> {
        self.hooks.clone()
    }

    fn call(&self, _hook: &str, input: &str) -> Result<Option<String>, String> {
        if self.dir.join("TRAP").exists() {
            return Err("trapped: error while executing at wasm backtrace".to_string());
        }
        let look = serde_json::from_str::<Value>(input)
            .map(|v| !v["look"].is_null())
            .unwrap_or(false);
        let second = self.dir.join("second.json");
        let path = match (look, second.is_file()) {
            (true, true) => second,
            _ => self.dir.join("answer.json"),
        };
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some(text))
    }

    /// A number a test can recognise in `plugin test`'s report — the fuel a layer costs is
    /// otherwise only ever printed from a real module.
    fn fuel(&self) -> u64 {
        4242
    }
}

struct FileLoader;

impl Loader for FileLoader {
    fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        let bytes = std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
        if bytes == b"not a module" {
            return Err(format!("{}: not a wasm module", wasm.display()));
        }
        let dir = wasm.parent().unwrap_or(Path::new("."));
        let manifest = Manifest::read(dir)?;
        Ok(Box::new(FileModule {
            hooks: manifest.hooks.clone(),
            dir: dir.to_path_buf(),
        }))
    }
}

/// **The gate's step 1, without cargo**: the layer's `next.json` becomes its `answer.json` and a
/// module, and a `FAIL` file makes the build fail with a compiler's own words.
struct FileBuilder;

impl Builder for FileBuilder {
    fn build(&self, dir: &Path) -> Result<String, BuildError> {
        let log = format!("building {}", dir.display());
        if dir.join("FAIL").exists() {
            return Err(BuildError {
                why: "cargo build failed (exit status: 1)".into(),
                log: format!("{log}\nerror[E0425]: cannot find value `plan` in this scope"),
            });
        }
        let next = dir.join("next.json");
        if next.exists() {
            std::fs::copy(&next, dir.join("answer.json")).expect("an answer");
        }
        // Every successful build writes a module; what it says lives beside it.
        std::fs::write(dir.join(WASM), b"built module").expect("a module");
        Ok(log)
    }
}

// ---------------------------------------------------------------------------------------------
// The rig: a session's plugin host, the desk over it, and the tool a model would call
// ---------------------------------------------------------------------------------------------

struct Rig {
    tmp: Tmp,
    hub: Arc<Hub>,
    rt: ToolRuntime,
    _backend: letibot_tools::backend::tempdir::TempDir,
}

impl Rig {
    fn new(tag: &str) -> Rig {
        Rig::with(FileLoader, FileBuilder, tag)
    }

    fn with(loader: impl Loader + 'static, builder: impl Builder + 'static, tag: &str) -> Rig {
        Rig::built(loader, builder, tag, None)
    }

    /// The same, with **the daemon's session list** — the feed a pane on disk is drawn from
    /// (`docs/plugins-plan.md` §4.9). `Sessions::open_plugins` is what hands a real host one; here
    /// the fixture's list stands in for it.
    fn with_feed(
        loader: impl Loader + 'static,
        builder: impl Builder + 'static,
        tag: &str,
        feed: Arc<dyn letibot_harnessd::plugins::feed::SessionFeed>,
    ) -> Rig {
        Rig::built(loader, builder, tag, Some(feed))
    }

    fn built(
        loader: impl Loader + 'static,
        builder: impl Builder + 'static,
        tag: &str,
        feed: Option<Arc<dyn letibot_harnessd::plugins::feed::SessionFeed>>,
    ) -> Rig {
        let tmp = Tmp::new(tag);
        let hub = Hub::new(SESSION);
        let host = PluginHost::discover(
            SESSION,
            tmp.project(),
            Some(tmp.plugins()),
            None,
            Arc::new(loader),
            Arc::new(builder),
        );
        let host = Arc::new(match feed {
            Some(feed) => host.with_sessions(feed),
            None => host,
        });
        // The slot the daemon fills in `Sessions::open_plugins`, filled here by hand — which is
        // the whole of what that function does for this tool.
        let slot: Arc<Mutex<Option<Arc<PluginHost>>>> = Arc::new(Mutex::new(Some(host.clone())));
        let desk: Arc<dyn PluginDesk> = Arc::new(SessionPlugins::new(
            SESSION,
            Some(tmp.plugins()),
            slot,
            hub.clone(),
        ));
        let backend = letibot_tools::backend::tempdir::TempDir::new();
        let mut reg = Registry::new();
        reg.register(Box::new(PluginTool::new(desk.clone())))
            .and_then(|_| reg.register(Box::new(PluginHooksTool::new(desk))))
            .expect("both plugin tools register");
        let rt = ToolRuntime::new(
            reg,
            Box::new(HostBackend::new(backend.path()).expect("a backend")),
        )
        .with_gate(letibot_tools::testing::allow_all());
        Rig {
            tmp,
            hub,
            rt,
            _backend: backend,
        }
    }

    /// One tool call, as the engine makes it.
    fn call(&self, tool: &str, args: &str) -> (ToolOutcome, String) {
        let mut sink = letibot_tools::events::RecordingToolSink::new();
        let r = self.rt.invoke(
            "t1",
            &ToolCall {
                id: "c1".into(),
                name: tool.into(),
                arguments: args.into(),
            },
            &mut sink,
        );
        let rendered = r.render();
        (r.outcome, rendered)
    }

    /// The same, asserting the call succeeded — most actions are expected to work.
    fn ok(&self, args: &str) -> String {
        let (outcome, rendered) = self.call("plugin", args);
        assert!(matches!(outcome, ToolOutcome::Ok), "{args}: {rendered}");
        rendered
    }

    fn refused(&self, args: &str) -> String {
        let (outcome, rendered) = self.call("plugin", args);
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failed { .. } | ToolOutcome::NotRun { .. }
            ),
            "{args} was expected to refuse: {rendered}"
        );
        rendered
    }

    /// A layer on disk, as `scaffold` would leave one plus whatever the test's fakes read.
    fn layer(&self, name: &str, answer: &str) -> PathBuf {
        let dir = self.tmp.plugins().join(name);
        std::fs::create_dir_all(&dir).expect("a layer dir");
        write_manifest(
            &dir,
            name,
            &[&format!("pane_{}", name.replace('-', "_"))],
            &[],
        );
        std::fs::write(dir.join(WASM), b"a module").expect("a module");
        std::fs::write(dir.join("answer.json"), answer).expect("an answer");
        dir
    }

    /// What has been published to this session's heads, in order.
    fn panes(&self) -> Vec<(String, Value)> {
        self.hub
            .retained()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::PluginPane { pane, view } => Some((pane.clone(), view.clone())),
                _ => None,
            })
            .collect()
    }
}

fn write_manifest(dir: &Path, name: &str, hooks: &[&str], caps: &[&str]) {
    let hooks = hooks
        .iter()
        .map(|h| format!("\"{h}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let caps = caps
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "name = \"{name}\"\npurpose = \"what this pane is for\"\nhooks = [{hooks}]\n\
             capabilities = [{caps}]\ncontract = 1\n"
        ),
    )
    .expect("a manifest");
}

/// A view that validates: one panel, one key the host does not own, no view behind it.
const A_VIEW: &str = r#"{"title":"the deploy queue","panels":[
    {"id":"queue","key":"t","title":"queue","summary":[{"markdown":"three waiting"}]}]}"#;

// ---------------------------------------------------------------------------------------------
// scaffold, stack, build
// ---------------------------------------------------------------------------------------------

/// **`scaffold` writes the three files, and the stack finds them** — the first step of the loop,
/// and the one that decides every identifier the rest of it uses.
#[test]
fn scaffold_writes_a_plugin_the_stack_then_lists() {
    let rig = Rig::new("scaffold");
    let said = rig.ok(
        r#"{"action":"scaffold","name":"deploy-status","purpose":"the deploy queue, per environment"}"#,
    );
    assert!(said.contains("scaffolded `deploy-status`"), "{said}");
    assert!(
        said.contains("deploy-status/deploy_status"),
        "the pane id the loop's later calls use is named at the first step: {said}"
    );
    let dir = rig.tmp.plugins().join("deploy-status");
    for f in ["Cargo.toml", "plugin.toml", "src/lib.rs"] {
        assert!(dir.join(f).is_file(), "{f} was not written: {said}");
    }
    // The template is a source, not a binary: the gate builds what is here.
    assert!(!dir.join(WASM).exists());
    // And the stack is honest about what is IN FORCE: a scaffold is a source, and nothing about
    // it is applied until `plugin load` says so — so a layer that is on disk and not loaded is
    // not listed as if it were answering.
    let stack = rig.ok(r#"{"action":"stack"}"#);
    assert!(stack.contains("no layers at all"), "{stack}");
}

#[test]
fn scaffolding_the_same_name_twice_is_refused_and_names_the_one_that_is_there() {
    let rig = Rig::new("scaffold-twice");
    rig.ok(r#"{"action":"scaffold","name":"deploy","purpose":"one"}"#);
    let refused = rig.refused(r#"{"action":"scaffold","name":"deploy","purpose":"two"}"#);
    assert!(refused.contains("already a plugin"), "{refused}");
    // And a name that is not a name is refused with the one it would have to be.
    let refused = rig.refused(r#"{"action":"scaffold","name":"Deploy Status","purpose":"x"}"#);
    assert!(refused.contains("lowercase letters"), "{refused}");
    assert!(
        refused.contains("deploy-status"),
        "the suggestion: {refused}"
    );
}

#[test]
fn a_hook_kind_this_host_does_not_run_is_refused_with_the_one_it_does() {
    let rig = Rig::new("hook-kind");
    // **`condition` is a kind now** (2b): a layer that answers it is scaffolded, and its hook is
    // named after the subject, not the kind alone.
    let said = rig.ok(r#"{"action":"scaffold","name":"watch","purpose":"x","hook":"condition"}"#);
    assert!(said.contains("answers `condition_watch`"), "{said}");
    // And a kind this host does not run is refused with the ones it does.
    let refused =
        rig.refused(r#"{"action":"scaffold","name":"digest-it","purpose":"x","hook":"digest"}"#);
    assert!(refused.contains("`digest` is not a hook kind"), "{refused}");
    assert!(refused.contains("on_tool_call"), "{refused}");
}

/// **`build` compiles the layer and installs the artifact** — and a failed build hands back the
/// compiler's own words, because that is the half a model edits.
#[test]
fn build_installs_the_module_and_a_failure_returns_the_compilers_words() {
    let rig = Rig::new("build");
    rig.ok(r#"{"action":"scaffold","name":"deploy","purpose":"the queue"}"#);
    let dir = rig.tmp.plugins().join("deploy");
    let said = rig.ok(r#"{"action":"build","name":"deploy"}"#);
    assert!(said.contains("built `deploy`"), "{said}");
    assert!(dir.join(WASM).is_file(), "{said}");

    std::fs::write(dir.join("FAIL"), b"").expect("a failing build");
    let refused = rig.refused(r#"{"action":"build","name":"deploy"}"#);
    assert!(
        refused.contains("the build of `deploy` failed"),
        "{refused}"
    );
    assert!(
        refused.contains("error[E0425]: cannot find value `plan` in this scope"),
        "the compiler's own words: {refused}"
    );
}

#[test]
fn building_a_plugin_this_session_does_not_have_names_the_ones_it_does() {
    let rig = Rig::new("build-missing");
    rig.ok(r#"{"action":"scaffold","name":"present","purpose":"x"}"#);
    let refused = rig.refused(r#"{"action":"build","name":"absent"}"#);
    assert!(refused.contains("no plugin `absent`"), "{refused}");
    assert!(
        refused.contains("present"),
        "the miss names what is there: {refused}"
    );
}

// ---------------------------------------------------------------------------------------------
// load: the shadow gate, and what it refuses
// ---------------------------------------------------------------------------------------------

/// **A good plugin is applied hot and its pane is published** — the end of the loop, and the only
/// thing an operator sees. The pane arrives on the hub without anybody opening it, which is how a
/// head learns its title (C4).
#[test]
fn load_applies_a_good_plugin_and_publishes_its_pane() {
    let rig = Rig::new("load-good");
    rig.layer("deploy", A_VIEW);
    let said = rig.ok(r#"{"action":"load","name":"deploy"}"#);
    assert!(said.contains("loaded `deploy` as layer `s1`"), "{said}");
    assert!(
        said.contains("a view the host can drive"),
        "the shadow's own call is reported: {said}"
    );
    let panes = rig.panes();
    assert_eq!(panes.len(), 1, "{panes:?}");
    assert_eq!(panes[0].0, "deploy/deploy");
    assert_eq!(panes[0].1["title"], "the deploy queue");
}

/// **A view the host would refuse never reaches a screen.** `q` is one of the host's own keys
/// (C3), so a panel claiming it is a view the head cannot drive — and the layer does not answer
/// afterwards either.
#[test]
fn load_refuses_a_view_the_host_would_not_draw() {
    let rig = Rig::new("load-bad-view");
    rig.layer(
        "deploy",
        r#"{"title":"bad","panels":[{"id":"p","key":"q","title":"p"}]}"#,
    );
    let refused = rig.refused(r#"{"action":"load","name":"deploy"}"#);
    assert!(
        refused.contains("answers something the host refuses"),
        "{refused}"
    );
    assert!(
        refused.contains("are the host's keys"),
        "the gate's own words about the key: {refused}"
    );
    assert!(
        refused.contains("nothing was applied"),
        "and what did not happen: {refused}"
    );
    // Nothing answered, and nothing on a screen: a refused candidate is not a layer.
    assert!(rig.panes().is_empty(), "{:?}", rig.panes());
    let stack = rig.ok(r#"{"action":"stack"}"#);
    assert!(stack.contains("no layers at all"), "{stack}");
}

/// **A module that traps is refused, and the refusal says nothing was applied.**
#[test]
fn load_refuses_a_plugin_that_traps() {
    let rig = Rig::new("load-trap");
    let dir = rig.layer("deploy", A_VIEW);
    std::fs::write(dir.join("TRAP"), b"").expect("a trap");
    let refused = rig.refused(r#"{"action":"load","name":"deploy"}"#);
    assert!(refused.contains("fails when it is called"), "{refused}");
    assert!(refused.contains("trapped"), "{refused}");
    assert!(rig.panes().is_empty());
}

/// **The gate's own steps run first**: a build that fails stops `load` before any of the module's
/// answers are looked at, and the report says so.
#[test]
fn load_reports_a_failed_build_with_the_compiler_output() {
    let rig = Rig::new("load-build-fail");
    let dir = rig.layer("deploy", A_VIEW);
    std::fs::write(dir.join("FAIL"), b"").expect("a failing build");
    let refused = rig.refused(r#"{"action":"load","name":"deploy"}"#);
    assert!(
        refused.contains("the shadow gate refused `deploy`"),
        "{refused}"
    );
    assert!(refused.contains("build:"), "which step: {refused}");
    assert!(
        refused.contains("E0425"),
        "and the compiler's words: {refused}"
    );
}

/// **A reload is `load` again**, and the pane changes — a layer's second version takes effect on
/// the next call, with no restart (`docs/plugins-plan.md` §2.3).
#[test]
fn loading_a_rebuilt_layer_replaces_what_the_pane_says() {
    let rig = Rig::new("load-reload");
    let dir = rig.layer("deploy", A_VIEW);
    rig.ok(r#"{"action":"load","name":"deploy"}"#);
    assert_eq!(rig.panes().last().unwrap().1["title"], "the deploy queue");

    std::fs::write(
        dir.join("next.json"),
        r#"{"title":"the deploy queue · v2","panels":[{"id":"queue","key":"t","title":"queue"}]}"#,
    )
    .expect("a second answer");
    std::fs::write(dir.join(WASM), b"a rebuilt module").expect("a new module");
    let said = rig.ok(r#"{"action":"load","name":"deploy"}"#);
    assert!(said.contains("as layer `s1`"), "the same layer: {said}");
    assert_eq!(
        rig.panes().last().unwrap().1["title"],
        "the deploy queue · v2",
        "the rebuilt layer answers, and the head is redrawn"
    );
}

// ---------------------------------------------------------------------------------------------
// test, stack, disable/enable, plugin_hooks
// ---------------------------------------------------------------------------------------------

/// **`test` reports the answer, the layer that answered, the fuel, and whether the host would
/// draw it** — the read that a model makes before it applies anything.
#[test]
fn test_names_the_view_the_layer_and_the_fuel() {
    let rig = Rig::new("test");
    rig.layer("deploy", A_VIEW);
    rig.ok(r#"{"action":"load","name":"deploy"}"#);
    let said = rig.ok(r#"{"action":"test","name":"deploy/deploy"}"#);
    assert!(said.contains("`pane_deploy` asked with"), "{said}");
    assert!(said.contains("answered by `s1`"), "{said}");
    assert!(said.contains("4242 fuel units"), "the cost: {said}");
    assert!(said.contains("the deploy queue"), "the view: {said}");
    assert!(said.contains("a view the host can drive"), "{said}");
}

/// **A view the host would refuse is REPORTED as refused**, not hidden: `test` is where a model
/// finds that out, and silence there would send it to `load` for the same answer.
///
/// The layer here answers a good first frame and a bad one once a look's result is in the input —
/// which is how a real plugin comes to draw something the host will not take, and the one case
/// `load` cannot catch for it (it asked the synthetic frame, not this one).
#[test]
fn test_says_when_the_host_would_refuse_the_view() {
    let rig = Rig::new("test-refuse");
    let dir = rig.layer(
        "deploy",
        r#"{"title":"deploy","panels":[{"id":"p","key":"t","title":"p"}],
            "look":{"read":".letibot/deploy.json"}}"#,
    );
    std::fs::write(
        dir.join("second.json"),
        r#"{"title":"drawn","panels":[{"id":"p","key":"f","title":"p"}]}"#,
    )
    .expect("a bad frame");
    rig.ok(r#"{"action":"load","name":"deploy"}"#);
    let said = rig.ok(r#"{"action":"test","name":"deploy/deploy",
            "input":{"v":1,"state":null,"event":null,"size":{"w":80,"h":20},
                      "look":{"exit":0,"stdout":"{}","stderr":""}}}"#);
    assert!(said.contains("The host would REFUSE"), "{said}");
    assert!(
        said.contains("`f`") && said.contains("host's keys"),
        "the host's own reason: {said}"
    );
}

/// **A pane that asks for a look says so**, which is the frame a head draws while the host reads
/// the file — and the reason a first frame may legitimately have no numbers in it.
#[test]
fn test_says_when_the_pane_asks_for_a_look() {
    let rig = Rig::new("test-look");
    rig.layer(
        "deploy",
        r#"{"title":"deploy","panels":[{"id":"p","key":"t","title":"p"}],
            "look":{"read":".letibot/deploy.json"}}"#,
    );
    rig.ok(r#"{"action":"load","name":"deploy"}"#);
    let said = rig.ok(r#"{"action":"test","name":"deploy/deploy"}"#);
    assert!(said.contains("it asks the host for a look"), "{said}");
    assert!(said.contains(".letibot/deploy.json"), "{said}");
}

/// **`disable` takes a layer out without deleting it, and `enable` puts it back** — the bisect for
/// *"which of my fixes broke this"*.
#[test]
fn disable_takes_a_layer_out_and_enable_puts_it_back() {
    let rig = Rig::new("disable");
    rig.layer("deploy", A_VIEW);
    rig.ok(r#"{"action":"load","name":"deploy"}"#);
    let said = rig.ok(r#"{"action":"disable","name":"deploy"}"#);
    assert!(said.contains("layer `s1` (`deploy`) is off"), "{said}");
    let stack = rig.ok(r#"{"action":"stack"}"#);
    assert!(stack.contains("DISABLED"), "{stack}");
    assert!(
        stack.contains("nobody"),
        "the hook has no answering layer: {stack}"
    );
    // Nothing answered while it was off: the pane is not on the session's list, and saying which
    // is better than answering with the view of a layer somebody switched off.
    let refused = rig.refused(r#"{"action":"test"}"#);
    assert!(refused.contains("name the hook to test"), "{refused}");
    assert!(
        refused.contains("pane_deploy"),
        "the miss names the hooks there are: {refused}"
    );
    let said = rig.ok(r#"{"action":"enable","name":"deploy"}"#);
    assert!(said.contains("is on"), "{said}");
    let said = rig.ok(r#"{"action":"test"}"#);
    assert!(said.contains("the deploy queue"), "{said}");
}

#[test]
fn an_unknown_layer_is_refused_with_the_ones_there_are() {
    let rig = Rig::new("unknown-layer");
    rig.layer("deploy", A_VIEW);
    rig.ok(r#"{"action":"load","name":"deploy"}"#);
    let refused = rig.refused(r#"{"action":"disable","name":"nope"}"#);
    assert!(refused.contains("not a layer of this session"), "{refused}");
    assert!(refused.contains("s1 (deploy)"), "{refused}");
}

/// **`plugin_hooks` is the host's own contract**, and it is what a model reads before it writes a
/// view: the input, the output, the keys and a worked example.
#[test]
fn plugin_hooks_is_the_hosts_own_contract_with_a_worked_example() {
    let rig = Rig::new("hooks");
    let (outcome, said) = rig.call("plugin_hooks", "{}");
    assert!(matches!(outcome, ToolOutcome::Ok), "{said}");
    for field in [
        "state", "look", "event", "size", "panels", "findings", "every_s",
    ] {
        assert!(
            said.contains(field),
            "the contract never names `{field}`: {said}"
        );
    }
    // The worked example is in the document, and it is one the host accepts: it parses into a
    // View and validates (the contract module's own tests hold the round trip; this is the
    // reader's half).
    let start = said.find("```json\n{").expect("an example") + "```json\n".len();
    let end = said[start..].find("\n```").expect("the fence closes") + start;
    let example: Value = serde_json::from_str(&said[start..end]).expect("the example is JSON");
    let view: letibot_sessionlog::pane::View =
        serde_json::from_value(example).expect("the example is a view");
    view.validate()
        .expect("the example is one the host accepts");
    // And a kind this host does not run is refused with the ones it does.
    let (outcome, said) = rig.call("plugin_hooks", r#"{"hook":"digest"}"#);
    assert!(matches!(outcome, ToolOutcome::Failed { .. }), "{said}");
    assert!(said.contains("`digest` is not a hook kind"), "{said}");
    // **The four patch points are pages of their own**, each built from the SDK's types.
    let (outcome, said) = rig.call("plugin_hooks", r#"{"hook":"condition"}"#);
    assert!(matches!(outcome, ToolOutcome::Ok), "{said}");
    for field in ["look", "every_s", "max_ttl_s", "fired", "state", "github"] {
        assert!(
            said.contains(field),
            "the condition page never names `{field}`"
        );
    }
}

/// **A session with no host behind the tool says which** — never an absence reading as a missing
/// capability of the build. (A subagent's harness is opened by its parent's runner, so this is a
/// real state and not a test fiction.)
#[test]
fn a_session_with_no_plugin_host_refuses_the_actions_that_need_one() {
    let tmp = Tmp::new("no-host");
    let hub = Hub::new(SESSION);
    let desk: Arc<dyn PluginDesk> = Arc::new(SessionPlugins::new(
        SESSION,
        Some(tmp.plugins()),
        Arc::new(Mutex::new(None)),
        hub,
    ));
    let backend = letibot_tools::backend::tempdir::TempDir::new();
    let mut reg = Registry::new();
    reg.register(Box::new(PluginTool::new(desk.clone())))
        .and_then(|_| reg.register(Box::new(PluginHooksTool::new(desk))))
        .unwrap();
    let rt = ToolRuntime::new(
        reg,
        Box::new(HostBackend::new(backend.path()).expect("a backend")),
    )
    .with_gate(letibot_tools::testing::allow_all());
    let mut sink = letibot_tools::events::RecordingToolSink::new();
    let load = rt
        .invoke(
            "t1",
            &ToolCall {
                id: "c1".into(),
                name: "plugin".into(),
                arguments: r#"{"action":"load","name":"deploy"}"#.into(),
            },
            &mut sink,
        )
        .render();
    assert!(load.contains("no plugin host yet"), "{load}");
    // Scaffold needs no host: it is a file operation on the session's own directory.
    let scaffold = rt
        .invoke(
            "t1",
            &ToolCall {
                id: "c2".into(),
                name: "plugin".into(),
                arguments: r#"{"action":"scaffold","name":"deploy","purpose":"x"}"#.into(),
            },
            &mut sink,
        )
        .render();
    assert!(scaffold.contains("scaffolded `deploy`"), "{scaffold}");
}

// ---------------------------------------------------------------------------------------------
// The real thing: a scaffolded plugin, built by cargo, loaded by rano, drawn by a head
// ---------------------------------------------------------------------------------------------

/// **Is the wasm target installed?** Read off the toolchain's own `rustlib` directory rather than
/// by running `rustup`, so the check costs no process — and every way of *having* it (rustup, a
/// distro toolchain, `RUSTUP_HOME` elsewhere) puts the directory in one of the places asked.
fn wasm_target() -> bool {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(h) = std::env::var("RUSTUP_HOME") {
        roots.push(PathBuf::from(h));
    }
    if let Ok(h) = std::env::var("HOME") {
        roots.push(PathBuf::from(&h).join(".rustup"));
        // A toolchain installed beside the SDKs rather than by rustup.
        roots.push(PathBuf::from(&h).join(".local/rustup"));
    }
    roots
        .iter()
        .flat_map(|root| {
            std::fs::read_dir(root.join("toolchains"))
                .into_iter()
                .flatten()
        })
        .flatten()
        .any(|t| {
            t.path()
                .join("lib/rustlib/wasm32-unknown-unknown/lib")
                .is_dir()
        })
}

/// **The whole loop, for real**: `scaffold` → `build` → `load` → the pane on the hub.
///
/// The pieces are the production ones at every step — `CargoBuilder` runs cargo for
/// `wasm32-unknown-unknown` offline, `RanoLoader` is wasmtime compiling the guest, the stack is the
/// session's own, and the pane is published to a real `Hub`. What the guest answers is the
/// template's own view, which is exactly what a model gets at the first step and what the head
/// would draw.
///
/// **Both apparatuses are provisionable and both are named**: the wasm target, and the rano SDK the
/// template's `Cargo.toml` points at. `LETIBOT_REQUIRE_APPARATUS=1` turns either absence into a
/// failure, so a box that has them cannot pass by skipping.
#[test]
fn the_whole_loop_writes_a_pane_that_a_head_can_draw() {
    let sdk = scaffold::sdk();
    let here = wasm_target() && sdk.found;
    let Some(()) = letibot_tokencore::apparatus::present(
        &format!(
            "the `wasm32-unknown-unknown` target and the rano SDK at {} — one builds the guest, \
             the other is what it links",
            sdk.path
        ),
        here,
    ) else {
        return;
    };
    let rig = Rig::with(
        letibot_harnessd::plugins::rano::RanoLoader,
        letibot_plugins::CargoBuilder,
        "real-loop",
    );
    rig.ok(
        r#"{"action":"scaffold","name":"first-pane","purpose":"the template's own view, drawn"}"#,
    );
    let built = rig.ok(r#"{"action":"build","name":"first-pane"}"#);
    assert!(built.contains("plugin.wasm"), "{built}");
    assert!(
        rig.tmp.plugins().join("first-pane").join(WASM).is_file(),
        "cargo produced no module: {built}"
    );
    let loaded = rig.ok(r#"{"action":"load","name":"first-pane"}"#);
    assert!(
        loaded.contains("loaded `first-pane` as layer `s1`"),
        "{loaded}"
    );
    let panes = rig.panes();
    let (pane, view) = panes.last().expect("the pane reached the hub");
    assert_eq!(pane, "first-pane/first_pane");
    let title = view["title"].as_str().unwrap_or_default();
    assert!(title.starts_with("first-pane"), "{view}");
    let as_view: letibot_sessionlog::pane::View =
        serde_json::from_value(view.clone()).expect("a C3 view");
    as_view
        .validate()
        .expect("a view the host's keys can drive");
    // And `test` asks the same hook through the stack the head is drawn from.
    let said = rig.ok(r#"{"action":"test"}"#);
    assert!(said.contains("answered by `s1`"), "{said}");
    assert!(said.contains("first-pane"), "{said}");
}

/// **The SDK path the template writes is the one this box can resolve**, so the real loop above is
/// not gated on a path that could never be right — and a box with `LETIBOT_RANO_SDK` set reads
/// that.
#[test]
fn the_templates_sdk_is_resolved_from_the_box() {
    let sdk = scaffold::sdk();
    assert!(
        sdk.path.ends_with("sdks/rust"),
        "`{}` is not an SDK path",
        sdk.path
    );
    if let Ok(explicit) = std::env::var("LETIBOT_RANO_SDK") {
        assert_eq!(sdk.path, explicit);
    }
}

/// **`session_plugins_dir` is the one spelling of the session scope** — the directory the host
/// discovers, the tool scaffolds into and the daemon builds its host over.
#[test]
fn the_session_scope_is_one_directory_for_every_reader() {
    let dir = session_plugins_dir(SESSION).expect("this box has a data dir");
    assert!(
        dir.ends_with(format!("sessions/{SESSION}/plugins")),
        "{dir:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// The seat, and the daemon's half of the wiring
// ---------------------------------------------------------------------------------------------

/// An adjudicator that exists to be ATTACHED, not to decide — the seat under test seats tools that
/// must be adjudicated, and nothing in these tests runs a turn. `wired.rs` makes the same one for
/// the same reason.
struct Attached;

impl letibot_tools::Adjudicator for Attached {
    fn decide(
        &self,
        req: &letibot_tools::AdjudicationRequest,
    ) -> letibot_tools::AdjudicationDecision {
        letibot_tools::AdjudicationDecision::unavailable(
            req,
            "test",
            "this adjudicator exists to be attached, not to decide",
        )
    }

    fn describe(&self) -> String {
        "test adjudicator (attached, decides nothing)".into()
    }
}

/// A session config on the byte vocabulary with a provider that is named and never called: no
/// GGUF, no server, no key — `idle_clock.rs`'s and `plugins_pane.rs`'s fixture.
fn byte_config(session: &str, seat: letibot_harnessd::config::Seat) -> letibot_harnessd::Config {
    let mut cfg = letibot_harnessd::Config::for_this_box("/tmp");
    cfg.dialect = letibot_harnessd::Dialect::Qwen;
    cfg.seat = seat;
    cfg.session_id = session.to_string();
    cfg.vocab_gguf = None;
    cfg.provider = Some(letibot_harnessd::config::ProviderConfig {
        name: "deepseek".into(),
        model: None,
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg
}

/// **The seat that writes code carries the loop**, which is the brief's *"seated where `task` is:
/// the leticode and coder seats"* — and the reason it must be proved on a real registry is that a
/// role naming a tool nobody registered REFUSES to open at all, so the failure mode of getting
/// this wrong is every `leticode` session on the box.
#[test]
fn the_leticode_seat_seats_the_plugin_tools_on_a_real_registry() {
    use letibot_harnessd::config::Seat;
    let cfg = byte_config("s-plugins-seat", Seat::Leticode);
    let parts = letibot_harnessd::Parts::load(&cfg).expect("the byte vocabulary loads");
    let h = letibot_harnessd::Harness::open_with(
        &parts,
        cfg,
        Hub::new("s-plugins-seat"),
        Some(Box::new(Attached)),
        None,
    )
    .expect("a leticode session opens with the plugin tools seated");
    let seated = &h.wiring().seated;
    for tool in ["plugin", "plugin_hooks"] {
        assert!(
            seated.iter().any(|t| t == tool),
            "`{tool}` is not in the prompt: {seated:?}"
        );
    }
    // And a harness opened by hand has no host: the daemon builds that when it opens the session
    // (`Sessions::open_plugins`), so the tool must say so rather than inventing an empty stack.
    assert!(
        h.plugin_host().is_none(),
        "a bare Harness::open is not a session the daemon serves"
    );
}

/// **The daemon hands the tool the very host its panes are drawn from** — the one line that keeps
/// `plugin load` from building a second stack, and the reason `plugin_host` exists at all.
#[test]
fn opening_a_session_through_sessions_fills_the_plugin_slot() {
    use letibot_harnessd::Sessions;
    use letibot_sessionlog::registry::Registry;
    let cfg = byte_config("s-plugins-slot", letibot_harnessd::config::Seat::default());
    let store = std::env::temp_dir().join(format!(
        "letibot-plugins-slot-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let mut cfg = cfg;
    cfg.store = Some(store.clone());
    cfg.socket = store.with_extension("sock");
    let parts = letibot_harnessd::Parts::load(&cfg).expect("the byte vocabulary loads");
    let registry = Registry::new();
    registry
        .create(cfg.session_id.clone(), "", Sessions::wiring(&cfg))
        .expect("a fresh registry");
    let sessions = Sessions::open_first(&parts, cfg.clone(), registry).expect("the session opens");
    let h = sessions
        .harness_of(&cfg.session_id)
        .expect("the session the daemon holds");
    assert!(
        h.plugin_host().is_some(),
        "`Sessions::open_plugins` did not fill the slot, so `plugin load` would build a second \
         stack and the pane it loaded would not be the pane a head draws"
    );
    for f in [store.clone(), store.with_extension("sock")] {
        let _ = std::fs::remove_file(f);
    }
}

// ---------------------------------------------------------------------------------------------
// The nick list: the built-in layer's own loop, for real
// ---------------------------------------------------------------------------------------------

/// The sessions the nick list is drawn from, as the daemon's registry would answer them: two
/// conversations and a subagent — which a nick list must NOT show.
struct Nicks;

impl letibot_harnessd::plugins::feed::SessionFeed for Nicks {
    fn rows(&self) -> Vec<Value> {
        let now = letibot_sessionlog::registry::now_ms();
        vec![
            serde_json::json!({"id": SESSION, "title": "the habr article", "parent": null,
                "live": true, "running": true, "heads": 1, "items": 6, "stored": 6,
                "model": "qwen", "workspace": "/p/letibot", "waits_on": 0,
                "nags_paused": false, "moved_ms": now - 60_000, "repo": "/p/letibot"}),
            serde_json::json!({"id": "s-other", "title": "the rust rewrite", "parent": null,
                "live": true, "running": false, "heads": 0, "items": 3, "stored": 3,
                "model": "qwen", "workspace": "/p/letibot", "waits_on": 0,
                "nags_paused": false, "moved_ms": now - 120_000, "repo": "/p/letibot"}),
            serde_json::json!({"id": "s-sub-1", "title": "read the repo",
                "parent": SESSION, "live": true, "running": false, "heads": 0, "items": 2,
                "stored": 2, "model": "qwen", "workspace": "/p/letibot", "waits_on": 0,
                "nags_paused": false, "moved_ms": now - 90_000, "repo": "/p/letibot"}),
        ]
    }
}

/// **`plugins/builtin/irc`, copied into a session's plugin directory as a layer of its own.**
///
/// The built-in is compiled by `crates/builtins` at build time, so its own `Cargo.toml` points at
/// its neighbours by relative path (`../abi`, `../../../crates/plugin-sdk`) — which resolve inside
/// this repository and nowhere else. A copy is rewritten to absolute paths, which is exactly what a
/// person iterating on a built-in outside the binary does.
fn copy_the_nick_list(dir: &Path) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/harnessd sits two levels under the repository root")
        .to_path_buf();
    let from = repo.join("plugins/builtin/irc");
    std::fs::create_dir_all(dir).expect("the layer's directory");
    std::fs::copy(from.join("plugin.toml"), dir.join("plugin.toml")).expect("the manifest");
    std::fs::copy(from.join("Cargo.lock"), dir.join("Cargo.lock")).expect("the lock");
    std::fs::create_dir_all(dir.join("src")).expect("src");
    std::fs::copy(from.join("src/lib.rs"), dir.join("src/lib.rs")).expect("the layer");
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"letibot-builtin-irc\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
             publish = false\n\n[workspace]\n\n[lib]\ncrate-type = [\"cdylib\"]\n\n\
             [dependencies]\n\
             letibot-guest-abi = {{ path = \"{}\" }}\n\
             letibot-plugin = {{ path = \"{}\" }}\n\
             serde = {{ version = \"1\", features = [\"derive\"] }}\n\
             serde_json = \"1\"\n\n\
             [profile.release]\nopt-level = \"s\"\nstrip = true\nlto = true\npanic = \"abort\"\n",
            repo.join("plugins/builtin/abi").display(),
            repo.join("crates/plugin-sdk").display(),
        ),
    )
    .expect("a manifest the session's directory can build");
}

/// **The live loop, for the nick list**: `plugin build` → `plugin load` → the pane redrawn, with
/// nothing of the host rebuilt in between.
///
/// The pieces are the production ones at every step: `CargoBuilder` runs cargo for
/// `wasm32-unknown-unknown` offline, `RanoLoader` is wasmtime compiling the guest, the stack is the
/// session's own, and the pane is published to a real `Hub` — the frame a head draws. What it is
/// asked with is the session feed (`docs/plugins-plan.md` §4.9), so the rows are the daemon's
/// sessions and not a fixture inside the guest.
#[test]
fn the_nick_list_builds_loads_and_is_drawn_without_rebuilding_the_host() {
    let here = wasm_target();
    let Some(()) = letibot_tokencore::apparatus::present(
        "the `wasm32-unknown-unknown` target, which compiles the nick list's guest",
        here,
    ) else {
        return;
    };
    let rig = Rig::with_feed(
        letibot_harnessd::plugins::rano::RanoLoader,
        letibot_plugins::CargoBuilder,
        "irc-loop",
        Arc::new(Nicks),
    );
    copy_the_nick_list(&rig.tmp.plugins().join("irc"));

    // 1. `plugin build` — cargo, the wasm target, offline.
    let built = rig.ok(r#"{"action":"build","name":"irc"}"#);
    assert!(built.contains("plugin.wasm"), "{built}");
    assert!(
        rig.tmp.plugins().join("irc").join(WASM).is_file(),
        "cargo produced no module: {built}"
    );

    // 2. `plugin load` — through the shadow gate, then the stack the head is drawn from.
    let loaded = rig.ok(r#"{"action":"load","name":"irc"}"#);
    assert!(loaded.contains("loaded `irc` as layer `s1`"), "{loaded}");

    // 3. The pane, redrawn: the nick list, from the feed, with the subagent absent.
    let panes = rig.panes();
    let (pane, view) = panes.last().expect("the pane reached the hub");
    assert_eq!(pane, "irc/nicks");
    let rows = view["panels"][0]["summary"][0]["tree"]["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("a tree block: {view}"));
    let header = rows[0]["cells"][0]["text"].as_str().unwrap_or_default();
    assert!(
        header.contains("2 here") && header.contains("1 working"),
        "the header counts the channel: {header}"
    );
    let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
    assert!(
        ids.contains(&SESSION) && ids.contains(&"s-other"),
        "the two conversations: {ids:?}"
    );
    assert!(
        !ids.contains(&"s-sub-1"),
        "a subagent is not a nick: {ids:?}"
    );
    let mine = rows
        .iter()
        .find(|r| r["id"] == SESSION)
        .expect("the session whose pane this is");
    assert_eq!(
        mine["enter"],
        serde_json::json!({"switch": SESSION}),
        "every row is the host's own switch"
    );
    assert_eq!(mine["cells"][0]["text"], "»", "and you are marked");
    // The view is one the host will draw and its keys can drive.
    let as_view: letibot_sessionlog::pane::View =
        serde_json::from_value(view.clone()).expect("a C3 view");
    as_view.validate().expect("a view the host accepts");

    // 4. And `plugin test` asks the same hook through the same stack, so a person iterating sees
    //    the same rows without a head at all.
    let said = rig.ok(r#"{"action":"test"}"#);
    assert!(said.contains("answered by `s1`"), "{said}");
    assert!(said.contains("the habr article"), "{said}");
}
