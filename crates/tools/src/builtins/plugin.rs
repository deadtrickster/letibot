//! `plugin` — **a session's model builds and loads a plugin pane while the session runs.**
//!
//! The operator, pointing at the pane language that landed in slice 1: *"this is exactly the case
//! — you building panes at runtime — i want the same in letibot"*. What a session could do until
//! now was *use* a plugin whose source already existed; what it could not do was write one.
//!
//! The loop is a handful of calls and no restart (`docs/plugins-plan.md` §5):
//!
//! ```text
//! scaffold → edit the view → build → test → load → (edit → build → load again, hot)
//! ```
//!
//! * `scaffold` — a new pane plugin in the session's own plugin directory, from a template whose
//!   `view` is a pure function with a native test beside it. A SOURCE, not a binary: the shadow
//!   gate builds what the source says.
//! * `build` — `cargo build --offline --release --target wasm32-unknown-unknown` in the plugin's
//!   directory, and the compiler's own words back when it refuses.
//! * `test` — one hook called with an input, and the view it answered, which layer answered it,
//!   the fuel it burned and any trap. This is how a model checks its own work before applying it.
//! * `load` — the shadow gate of §2.4 (build, load, a shadow stack, replay, report) *and* a call
//!   of the pane on that shadow with a synthetic input, so a new layer is proved to answer a view
//!   the host's keys can drive before anything on a screen changes. A pass applies the layer hot.
//! * `stack`, `disable`, `enable` — what is in force, and the bisect for *"which of my fixes broke
//!   this"*.
//!
//! # Why the daemon's half is a trait
//!
//! Everything above needs the session's live `Stack` — the one a head's panes are drawn from — and
//! a second stack would be a second answer to *what is in force*. So the tool asks
//! [`PluginDesk`], the daemon answers from the stack the panes use, and this crate keeps no
//! plugins of its own. The same shape `queue` uses for the merge queue, and for the same reason.
//!
//! # What it may not do
//!
//! A session plugin's scope is the session (`docs/plugins-plan.md` §3). Nothing here writes into
//! the operator's tree: `scaffold` writes under the session's own plugin directory, and a
//! capability is the manifest's to declare and the host's to enforce — `read` is the only one
//! slice 1 grants, and an `exec` look is refused with a sentence saying so.

use std::sync::Arc;

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The tool's name, in one place.
pub const TOOL: &str = "plugin";

/// **The contracts, as a tool of their own.** A model about to write a pane wants the input and
/// the output in front of it, read from the host's own types so it cannot drift from what the
/// host accepts — a different question from *what is in force in this session*, which is
/// `plugin`'s `stack`.
pub const HOOKS_TOOL: &str = "plugin_hooks";

/// **A refusal a person can act on**: the one sentence saying what did not happen, and the detail
/// it is fixed from — a compiler's own words, a gate report, the layers there are.
///
/// Two fields rather than one string because a build failure is exactly the case: the reason is
/// *"cargo build failed"* and the detail is forty lines of `error[E0425]`, which is the part the
/// model edits. Collapsing them loses the sentence a tool result is classified by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub why: String,
    pub detail: String,
}

impl Refusal {
    pub fn new(why: impl Into<String>, detail: impl Into<String>) -> Refusal {
        Refusal {
            why: why.into(),
            detail: detail.into(),
        }
    }

    /// A refusal whose detail is the same sentence, for a miss with nothing more to say.
    pub fn only(why: impl Into<String>) -> Refusal {
        let why = why.into();
        Refusal {
            detail: why.clone(),
            why,
        }
    }
}

/// **The daemon's half.** Every method answers the sentence the model reads, or a [`Refusal`].
pub trait PluginDesk: Send + Sync {
    /// A new pane plugin in this session's own plugin directory, from the template.
    fn scaffold(&self, name: &str, purpose: &str, hook: &str) -> Result<String, Refusal>;

    /// Build the plugin's source for `wasm32-unknown-unknown`, installing `plugin.wasm`.
    fn build(&self, name: &str) -> Result<String, Refusal>;

    /// Put the plugin through the shadow gate and, on a pass, apply it hot.
    fn load(&self, name: &str) -> Result<String, Refusal>;

    /// The layers, newest first, with each hook's answering layer and the recent failures.
    fn stack(&self) -> Result<String, Refusal>;

    /// Switch one layer off (or back on) without deleting it. `what` is a layer id, a plugin's
    /// name, or a pane id.
    fn set_enabled(&self, what: &str, enabled: bool) -> Result<String, Refusal>;

    /// Call a pane hook with `input` (or a synthetic C3 input) and report what it answered.
    /// `what` names the pane, the plugin or the layer; `None` is *the one this session has*.
    fn test(&self, what: Option<&str>, input: Option<&Value>) -> Result<String, Refusal>;

    /// The hook contracts this host accepts, read from the host's own types.
    fn hooks(&self, hook: Option<&str>) -> Result<String, Refusal>;
}

/// **No plugin host behind the tool** — a session the daemon does not serve (a subagent), or a
/// test registry. Every verb says so by name rather than the tool being absent, which would read
/// as a capability this build lacks.
pub struct NoPlugins;

impl NoPlugins {
    const WHY: &'static str = "this session has no plugin host — only a session the daemon itself opened has plugin \
         layers, and a subagent is opened for its parent's task";
}

impl PluginDesk for NoPlugins {
    fn scaffold(&self, _n: &str, _p: &str, _h: &str) -> Result<String, Refusal> {
        Err(Refusal::only(Self::WHY))
    }
    fn build(&self, _n: &str) -> Result<String, Refusal> {
        Err(Refusal::only(Self::WHY))
    }
    fn load(&self, _n: &str) -> Result<String, Refusal> {
        Err(Refusal::only(Self::WHY))
    }
    fn stack(&self) -> Result<String, Refusal> {
        Err(Refusal::only(Self::WHY))
    }
    fn set_enabled(&self, _w: &str, _e: bool) -> Result<String, Refusal> {
        Err(Refusal::only(Self::WHY))
    }
    fn test(&self, _w: Option<&str>, _i: Option<&Value>) -> Result<String, Refusal> {
        Err(Refusal::only(Self::WHY))
    }
    fn hooks(&self, _h: Option<&str>) -> Result<String, Refusal> {
        Err(Refusal::only(Self::WHY))
    }
}

/// **The tool.**
pub struct PluginTool {
    desk: Arc<dyn PluginDesk>,
}

impl PluginTool {
    pub fn new(desk: Arc<dyn PluginDesk>) -> Self {
        PluginTool { desk }
    }
}

const ACTIONS: &str = "`scaffold`, `build`, `load`, `stack`, `disable`, `enable` or `test`";

fn text<'a>(args: &'a Value, field: &str) -> Option<&'a str> {
    args.get(field)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// **The pane's C3 input, handed to `test` when the caller gave none**: the first frame of any
/// pane — nothing looked at, nothing pressed, no state. A plugin that cannot answer this one traps
/// on its own first frame, which is the cheapest defect to find.
pub fn synthetic_input() -> Value {
    serde_json::json!({
        "v": 1,
        "state": null,
        "look": null,
        "event": null,
        "size": {"w": 100, "h": 30},
    })
}

impl Tool for PluginTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            TOOL,
            "Build and load a plugin pane in this session, at run time. `scaffold` writes a new \
             pane plugin from a template into the session's own plugin directory; `build` \
             compiles it for wasm32 offline and returns the compiler's own words when it fails; \
             `test` calls its pane hook with an input and shows the view it answered, the layer \
             that answered and the fuel it burned; `load` puts it through the shadow gate and, on \
             a pass, makes it answer without a restart — so the head draws it; `stack` lists what \
             is in force newest first; `disable` and `enable` switch one layer without deleting \
             it. Read `plugin_hooks` for the pane's input and output contract before writing the \
             view.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["scaffold", "build", "load", "stack", "disable", "enable", "test"],
                        "description": "What to do."
                    },
                    "name": {
                        "type": "string",
                        "description": "scaffold/build/load/disable/enable: the plugin's name (letters, digits, `-`). test: the pane (`<plugin>/<hook>`), the plugin or the layer id; omit for the session's only pane."
                    },
                    "purpose": {
                        "type": "string",
                        "description": "scaffold: one line saying what the pane is for. It goes in plugin.toml, and it is what a person reads before granting the plugin anything."
                    },
                    "hook": {
                        "type": "string",
                        "enum": ["pane"],
                        "description": "scaffold: the hook kind to answer. `pane` is the only kind this host runs today."
                    },
                    "input": {
                        "type": "object",
                        "description": "test: the pane's C3 input (`plugin_hooks` shows one). Omit for a synthetic first frame: no state, no look, `v` 1."
                    }
                },
                "required": ["action"]
            }),
            // **`Exec` is the declared class because `build` runs a compiler.** A build runs
            // `cargo`, which runs the plugin's build script, so the tool as a whole reaches the
            // gate with the class `bash` has — and every verb that is NOT a build narrows itself
            // back down in `access_for`, which is the only direction the runtime honours. Getting
            // this backwards is silent: a schema declaring `Session` would have `build`'s `Exec`
            // filtered out as a widening and the compiler would run with nobody asked (the defect
            // `flowy` was found with).
            Access::Exec,
        )
    }

    /// **What THIS call amounts to**, narrowing only. `build` and `load` keep the schema's `Exec`
    /// — a `load` compiles too — and everything that touches nothing but this session's own stack
    /// says so.
    fn access_for(&self, args: &Value) -> Option<Access> {
        match args.get("action").and_then(|v| v.as_str()) {
            Some("stack") | Some("test") => Some(Access::Read),
            Some("scaffold") | Some("disable") | Some("enable") => Some(Access::Session),
            // `build` and `load` are commands: the declared class stands.
            _ => None,
        }
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(action) = text(args, "action") else {
            return Invocation::failed(
                "`plugin` needs an action",
                format!("call `plugin` with `action` = {ACTIONS}."),
            );
        };
        // A missing argument is refused here, naming what the argument builds — never passed on
        // as an empty name, which would be a plugin directory called nothing. Boxed because an
        // `Invocation` is large and this is the rare arm (the shape `queue` uses).
        let need = |field: &str, why: &str| -> Result<String, Box<Invocation>> {
            text(args, field).map(str::to_string).ok_or_else(|| {
                Box::new(Invocation::failed(
                    format!("`{action}` needs `{field}`"),
                    why,
                ))
            })
        };
        let said: Result<String, Refusal> = match action {
            "scaffold" => match (
                need(
                    "name",
                    "name the plugin after what the pane shows — letters, digits and `-`, e.g. \
                     `deploy-status`. Its directory, its crate and its pane id are built from it.",
                ),
                need(
                    "purpose",
                    "give the one line a person reads before granting this plugin anything, e.g. \
                     \"the deploy queue for each environment, and what is waiting\".",
                ),
            ) {
                (Ok(name), Ok(purpose)) => {
                    self.desk
                        .scaffold(&name, &purpose, text(args, "hook").unwrap_or("pane"))
                }
                (Err(refused), _) | (_, Err(refused)) => return *refused,
            },
            "build" => match need("name", "name the plugin to build.") {
                Ok(name) => self.desk.build(&name),
                Err(refused) => return *refused,
            },
            "load" => match need("name", "name the plugin to load.") {
                Ok(name) => self.desk.load(&name),
                Err(refused) => return *refused,
            },
            "stack" => self.desk.stack(),
            "disable" => match need(
                "name",
                "name the layer to switch off, or say `stack` to see them.",
            ) {
                Ok(name) => self.desk.set_enabled(&name, false),
                Err(refused) => return *refused,
            },
            "enable" => match need("name", "name the layer to switch back on.") {
                Ok(name) => self.desk.set_enabled(&name, true),
                Err(refused) => return *refused,
            },
            "test" => {
                // A missing `input` is the synthetic frame, not a mistake; an `input` that is not
                // an object is one, and saying so is cheaper than a plugin trapping on the wrong
                // shape.
                let input = match args.get("input") {
                    None | Some(Value::Null) => None,
                    Some(v @ Value::Object(_)) => Some(v),
                    Some(other) => {
                        return Invocation::failed(
                            format!("`test` wants the pane's input as an object, not {other}"),
                            "pass the C3 input as a JSON object — `plugin_hooks` shows one — or \
                             leave `input` out for a synthetic first frame."
                                .to_string(),
                        );
                    }
                };
                self.desk.test(text(args, "name"), input)
            }
            other => {
                return Invocation::failed(
                    format!("`{other}` is not an action of `plugin`"),
                    format!("the actions are {ACTIONS}."),
                );
            }
        };
        match said {
            Ok(line) => Invocation::ok(line),
            Err(Refusal { why, detail }) => Invocation::failed(why, detail),
        }
    }
}

/// **`plugin_hooks`** — the contracts, from the host that enforces them.
pub struct PluginHooksTool {
    desk: Arc<dyn PluginDesk>,
}

impl PluginHooksTool {
    pub fn new(desk: Arc<dyn PluginDesk>) -> Self {
        PluginHooksTool { desk }
    }
}

impl Tool for PluginHooksTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            HOOKS_TOOL,
            "The hook contracts this host accepts, read from the host's own types so they cannot \
             drift: for the pane hook, the input a pane is handed (state, look, event, size), the \
             output it must answer (title, badge, panels, findings, legend, look, every_s, state), \
             the keys the host owns and the rules the host refuses a view for — with a worked \
             example of both. Read this before writing a view with `plugin`.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "hook": {
                        "type": "string",
                        "description": "Only this hook kind, e.g. `pane`. Omit for every kind this host runs."
                    }
                }
            }),
            Access::Read,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        match self.desk.hooks(text(args, "hook")) {
            Ok(text) => Invocation::ok(text),
            Err(Refusal { why, detail }) => Invocation::failed(why, detail),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::RecordingToolSink;
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::{ToolCall, ToolOutcome};
    use std::sync::Mutex;

    /// A desk that records what it was asked and answers a fixed line, so the tool's own half —
    /// the arguments, the access class and the shape of a refusal — is what these tests measure.
    struct Recorder {
        said: Mutex<Vec<String>>,
        fail: bool,
    }

    impl Default for Recorder {
        fn default() -> Self {
            Recorder {
                said: Mutex::new(Vec::new()),
                fail: false,
            }
        }
    }

    impl PluginDesk for Recorder {
        fn scaffold(&self, name: &str, purpose: &str, hook: &str) -> Result<String, Refusal> {
            self.said
                .lock()
                .unwrap()
                .push(format!("scaffold {name} {hook} {purpose}"));
            Ok("scaffolded".into())
        }

        fn build(&self, name: &str) -> Result<String, Refusal> {
            self.said.lock().unwrap().push(format!("build {name}"));
            if self.fail {
                return Err(Refusal::new(
                    "cargo build failed",
                    "error[E0425]: cannot find value `plan` in this scope",
                ));
            }
            Ok("built".into())
        }

        fn load(&self, name: &str) -> Result<String, Refusal> {
            self.said.lock().unwrap().push(format!("load {name}"));
            Ok("loaded".into())
        }

        fn stack(&self) -> Result<String, Refusal> {
            self.said.lock().unwrap().push("stack".into());
            Ok("no layers".into())
        }

        fn set_enabled(&self, what: &str, enabled: bool) -> Result<String, Refusal> {
            self.said.lock().unwrap().push(format!(
                "{} {what}",
                if enabled { "enable" } else { "disable" }
            ));
            Ok("moved".into())
        }

        fn test(&self, what: Option<&str>, input: Option<&Value>) -> Result<String, Refusal> {
            self.said.lock().unwrap().push(format!(
                "test {} {}",
                what.unwrap_or(""),
                input.map(|i| i.to_string()).unwrap_or_default()
            ));
            Ok("a view".into())
        }

        fn hooks(&self, hook: Option<&str>) -> Result<String, Refusal> {
            self.said
                .lock()
                .unwrap()
                .push(format!("hooks {}", hook.unwrap_or("")));
            Ok("the contracts".into())
        }
    }

    /// **A runtime over one tool, with a gate that admits** — so a refusal in a test is the tool's
    /// own and never the boundary's. The temp dir comes back with it: the backend holds a path
    /// into it, so the caller keeps it alive for the length of the test.
    fn runtime(tool: Box<dyn Tool>) -> (ToolRuntime, crate::backend::tempdir::TempDir) {
        use crate::backend::HostBackend;
        use crate::backend::tempdir::TempDir;
        let dir = TempDir::new();
        let backend = HostBackend::new(dir.path()).expect("a backend");
        let mut reg = Registry::new();
        reg.register(tool).expect("registers");
        let rt = ToolRuntime::new(reg, Box::new(backend)).with_gate(crate::testing::allow_all());
        (rt, dir)
    }

    fn call(rt: &ToolRuntime, name: &str, args: &str) -> (ToolOutcome, String) {
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &ToolCall {
                id: "c1".into(),
                name: name.into(),
                arguments: args.into(),
            },
            &mut sink,
        );
        let rendered = r.render();
        (r.outcome, rendered)
    }

    fn plugin(rec: &Arc<Recorder>) -> (ToolRuntime, crate::backend::tempdir::TempDir) {
        runtime(Box::new(PluginTool::new(rec.clone())))
    }

    #[test]
    fn every_action_reaches_its_verb_and_scaffold_defaults_to_a_pane() {
        let rec = Arc::new(Recorder::default());
        let (rt, _dir) = plugin(&rec);
        for args in [
            r#"{"action":"scaffold","name":"deploy","purpose":"the deploy queue"}"#,
            r#"{"action":"build","name":"deploy"}"#,
            r#"{"action":"load","name":"deploy"}"#,
            r#"{"action":"stack"}"#,
            r#"{"action":"disable","name":"s1"}"#,
            r#"{"action":"enable","name":"s1"}"#,
            r#"{"action":"test"}"#,
        ] {
            let (outcome, rendered) = call(&rt, TOOL, args);
            assert!(matches!(outcome, ToolOutcome::Ok), "{args}: {rendered}");
        }
        assert_eq!(
            *rec.said.lock().unwrap(),
            vec![
                "scaffold deploy pane the deploy queue".to_string(),
                "build deploy".to_string(),
                "load deploy".to_string(),
                "stack".to_string(),
                "disable s1".to_string(),
                "enable s1".to_string(),
                "test  ".to_string(),
            ]
        );
    }

    /// **The two verbs that run a build reach the gate as commands**, and the read-only ones
    /// narrow themselves back down: a `load` compiles a plugin, which runs its build script, so
    /// the schema's class is `Exec` and only the verbs that are not builds say otherwise.
    #[test]
    fn the_build_verbs_declare_exec_and_the_rest_narrow_below_it() {
        let tool = PluginTool::new(Arc::new(Recorder::default()));
        let access = |args: &str| {
            let v: Value = serde_json::from_str(args).unwrap();
            tool.access_for(&v)
        };
        // The declared class is the most consequential verb in the tool, which is the build.
        assert_eq!(tool.schema().access, Access::Exec);
        assert_eq!(access(r#"{"action":"build"}"#), None, "Exec stands");
        assert_eq!(access(r#"{"action":"load"}"#), None, "Exec stands");
        assert_eq!(access(r#"{"action":"stack"}"#), Some(Access::Read));
        assert_eq!(access(r#"{"action":"test"}"#), Some(Access::Read));
        // Everything else changes this session's own stack and nothing the operator owns.
        for action in ["scaffold", "disable", "enable"] {
            let args = format!(r#"{{"action":"{action}"}}"#);
            assert_eq!(access(&args), Some(Access::Session), "{action}");
        }
    }

    /// **`build` and `load` go to the gate like any command**: with no adjudicator they never
    /// reach the desk, which is the same treatment `bash` gets.
    #[test]
    fn a_build_reaches_the_gate_before_the_desk() {
        use crate::backend::HostBackend;
        use crate::backend::tempdir::TempDir;
        let rec = Arc::new(Recorder::default());
        let dir = TempDir::new();
        let backend = HostBackend::new(dir.path()).expect("a backend");
        let mut reg = Registry::new();
        reg.register(Box::new(PluginTool::new(rec.clone())))
            .unwrap();
        let rt = ToolRuntime::new(reg, Box::new(backend));
        let (outcome, rendered) = call(&rt, TOOL, r#"{"action":"build","name":"deploy"}"#);
        assert!(!matches!(outcome, ToolOutcome::Ok), "{rendered}");
        assert!(rec.said.lock().unwrap().is_empty(), "{rendered}");
        // A read still gets through with nobody deciding.
        let (outcome, rendered) = call(&rt, TOOL, r#"{"action":"stack"}"#);
        assert!(matches!(outcome, ToolOutcome::Ok), "{rendered}");
    }

    /// **A build's refusal is the compiler's own words**, verbatim, in the part of the result a
    /// model edits — not a summary of them.
    #[test]
    fn a_failed_build_carries_the_compiler_output_verbatim() {
        let rec = Arc::new(Recorder {
            fail: true,
            ..Recorder::default()
        });
        let (rt, _dir) = plugin(&rec);
        let (outcome, rendered) = call(&rt, TOOL, r#"{"action":"build","name":"deploy"}"#);
        assert!(matches!(outcome, ToolOutcome::Failed { .. }), "{rendered}");
        assert!(rendered.contains("cargo build failed"), "{rendered}");
        assert!(
            rendered.contains("error[E0425]: cannot find value `plan` in this scope"),
            "the compiler's own words are the report: {rendered}"
        );
    }

    /// A miss says what the argument is for, so the next call is not a guess.
    #[test]
    fn a_missing_name_names_what_the_name_builds() {
        let (rt, _dir) = plugin(&Arc::new(Recorder::default()));
        let (_, rendered) = call(&rt, TOOL, r#"{"action":"scaffold"}"#);
        assert!(rendered.contains("`name`"), "{rendered}");
        assert!(rendered.contains("deploy-status"), "{rendered}");
        // And the purpose, without which a scaffold's manifest cannot be consented to.
        let (_, rendered) = call(&rt, TOOL, r#"{"action":"scaffold","name":"x"}"#);
        assert!(rendered.contains("`purpose`"), "{rendered}");
    }

    #[test]
    fn an_unknown_action_lists_the_ones_there_are() {
        let (rt, _dir) = plugin(&Arc::new(Recorder::default()));
        let (_, rendered) = call(&rt, TOOL, r#"{"action":"squash"}"#);
        assert!(rendered.contains("`squash` is not an action"), "{rendered}");
        assert!(rendered.contains("`scaffold`"), "{rendered}");
    }

    /// `test`'s input is an object or nothing: a string handed to a pane hook is a plugin trapping
    /// on a shape it was never promised.
    #[test]
    fn a_test_input_that_is_not_an_object_is_refused_before_the_desk_sees_it() {
        let rec = Arc::new(Recorder::default());
        let (rt, _dir) = plugin(&rec);
        let (_, rendered) = call(&rt, TOOL, r#"{"action":"test","input":"plan.json"}"#);
        assert!(rendered.contains("as an object"), "{rendered}");
        assert!(
            rec.said.lock().unwrap().is_empty(),
            "nothing reached the desk"
        );
        // And an object does reach it, unchanged.
        call(
            &rt,
            TOOL,
            r#"{"action":"test","name":"plan/dashboard","input":{"v":1}}"#,
        );
        assert_eq!(
            *rec.said.lock().unwrap(),
            vec!["test plan/dashboard {\"v\":1}".to_string()]
        );
    }

    #[test]
    fn the_synthetic_input_is_a_first_frame_a_pane_can_deserialise() {
        let input = synthetic_input();
        assert_eq!(input["v"], 1);
        // Every key present, because a plugin deserialises this into a struct and a missing key
        // is a plugin that cannot tell *no look* from *an empty one*.
        for key in ["state", "look", "event"] {
            assert!(input.get(key).is_some(), "{key} is missing from {input}");
            assert_eq!(input[key], Value::Null, "{key} is not null in {input}");
        }
        assert_eq!(input["size"]["w"], 100);
        assert_eq!(input["size"]["h"], 30);
    }

    #[test]
    fn plugin_hooks_asks_the_desk_for_the_contracts() {
        let rec = Arc::new(Recorder::default());
        let (rt, _dir) = runtime(Box::new(PluginHooksTool::new(rec.clone())));
        let (outcome, rendered) = call(&rt, HOOKS_TOOL, r#"{}"#);
        assert!(matches!(outcome, ToolOutcome::Ok), "{rendered}");
        call(&rt, HOOKS_TOOL, r#"{"hook":"pane"}"#);
        assert_eq!(*rec.said.lock().unwrap(), vec!["hooks ", "hooks pane"]);
    }

    /// **A session with no daemon behind the tool says which** — never an absence that reads as a
    /// missing capability of the build.
    #[test]
    fn no_plugin_host_behind_the_tool_refuses_by_name() {
        let (rt, _dir) = runtime(Box::new(PluginTool::new(Arc::new(NoPlugins))));
        let (outcome, rendered) = call(&rt, TOOL, r#"{"action":"stack"}"#);
        assert!(matches!(outcome, ToolOutcome::Failed { .. }), "{rendered}");
        assert!(rendered.contains("no plugin host"), "{rendered}");
    }
}
