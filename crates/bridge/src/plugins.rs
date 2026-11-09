//! The bridge's plugin stack (`docs/plugin-extension-points-wide.md` X25): the layers this folder
//! carries in its user and project scopes, the binary's own at the bottom, and the two hooks they
//! answer here.
//!
//! A layer that declares a `[[tools]]` is listed as an MCP tool beside the native ones - from its
//! manifest, so the list costs no engine - and called through `tool_<name>`. `push_channel` is the
//! other half and lives in [`crate::push`]. The stack is built once, on the first call that needs
//! it: a bridge whose folder has no layer and whose client wants none pays for no JIT.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use letibot_plugins::{Capability, DeclaredTool, Loader, Module, Stack, ToolDecl};
use serde_json::{Value, json};

/// The one `Loader` in this binary: rano compiles the module and this answers a hook. A copy of the
/// head's and the daemon's, deliberately - `letibot_plugins` stays free of a wasm engine, and each
/// binary that runs a layer brings its own rather than pulling a JIT into everything.
pub struct RanoLoader;

impl Loader for RanoLoader {
    fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        let plugin = rano::plugin::Plugin::load_hooks(
            wasm,
            rano::plugin::Capabilities::default(),
            letibot_plugins::HOOK_PREFIXES,
        )
        .map_err(|why| format!("{}: {why}", wasm.display()))?;
        Ok(Box::new(RanoModule { plugin }))
    }

    fn load_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        _caps: &[Capability],
    ) -> Result<Box<dyn Module>, String> {
        let plugin = rano::plugin::Plugin::from_bytes_hooks(
            bytes,
            PathBuf::from(format!("<built-in {name}>")),
            rano::plugin::Capabilities::default(),
            letibot_plugins::HOOK_PREFIXES,
        )
        .map_err(|why| format!("built-in `{name}`: {why}"))?;
        Ok(Box::new(RanoModule { plugin }))
    }
}

struct RanoModule {
    plugin: rano::plugin::Plugin,
}

impl Module for RanoModule {
    fn hooks(&self) -> Vec<String> {
        self.plugin.hooks().to_vec()
    }

    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String> {
        self.plugin.call(hook, input)
    }

    fn fuel(&self) -> u64 {
        self.plugin.last_fuel()
    }
}

/// What this bridge's layers came to: a stack built once, and the fact that it was tried - a folder
/// with nothing to load must not pay for the discovery on every call.
///
/// `from_disk` is off unless the server was started as one ([`crate::plugins::Layers::of_disk`]): a
/// test's bridge answers with the native rule and builds no engine, so no suite reads the operator's
/// `~/.config/letibot/plugins` by accident.
#[derive(Default)]
pub struct Layers {
    from_disk: bool,
    tried: bool,
    stack: Option<Arc<Stack>>,
}

impl Layers {
    /// The layers of the folder this process serves: discovered and built on first use.
    pub fn of_disk() -> Layers {
        Layers {
            from_disk: true,
            ..Layers::default()
        }
    }

    /// A stack a test handed over, already built.
    pub fn with(stack: Option<Arc<Stack>>) -> Layers {
        Layers {
            from_disk: false,
            tried: true,
            stack,
        }
    }

    /// The stack for `workspace`, built on first use.
    pub fn stack(&mut self, workspace: &Path) -> Option<Arc<Stack>> {
        if !self.tried && self.from_disk {
            self.tried = true;
            self.stack = production(workspace).map(Arc::new);
        }
        self.stack.clone()
    }
}

/// The stack for one folder: the user's `~/.config/letibot/plugins`, the project's
/// `.letibot/plugins`, and this binary's own layers at the bottom. The two refusals a person is owed
/// (a layer that would not load, a pin whose bytes moved) are said on stderr rather than swallowed,
/// because a bridge has no head to draw them in.
pub fn production(workspace: &Path) -> Option<Stack> {
    let mut stack = Stack::discover(
        None,
        Some(workspace),
        user_config_dir().as_deref(),
        Arc::new(RanoLoader),
    );
    let mut said: Vec<String> = stack
        .set_trust(letibot_plugins::Trust::open_default())
        .into_iter()
        .map(|b| {
            format!(
                "the plugin layer `{}` is not pinned any more: its bytes changed after it was \
                 pinned (module sha256 {}, on disk now {})",
                b.name, b.pin.module, b.module
            )
        })
        .collect();
    said.extend(letibot_builtins::install(&mut stack));
    said.extend(stack.failures().iter().map(|f| format!("{f}")));
    if !said.is_empty() {
        eprintln!(
            "letibot-mcp: a plugin layer was not loaded, so what it answers is answered by what is \
             below it: {}",
            said.join("; ")
        );
    }
    Some(stack)
}

/// The user's config directory, as the daemon and the head resolve it.
fn user_config_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("letibot"))
}

/// The `tool_` layers of the user and project scopes, as MCP tools, in the order the stack resolves
/// them: a name two layers declare is the higher one's. Read from the manifests alone - the same
/// door the daemon composes a session's prompt through - so `tools/list` never compiles a module.
///
/// A tool may not take a native tool's name: the daemon's tools are how a client is driven and the
/// bridge's list is one namespace, so a layer that asks for `leticode_start` is dropped here and
/// said once rather than shadowing what the client already has.
pub fn tools(workspace: &Path, native: &[String]) -> Vec<Value> {
    tools_in(workspace, user_config_dir().as_deref(), native)
}

/// The same over an explicit user root: the user scope is the operator's config directory, and a
/// test that must not read this box's is the one caller that says so.
pub fn tools_in(workspace: &Path, user: Option<&Path>, native: &[String]) -> Vec<Value> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for declared in Stack::declared_tools(None, Some(workspace), user) {
        let DeclaredTool {
            layer,
            scope,
            tool,
            capabilities,
        } = declared;
        if native.contains(&tool.name) {
            eprintln!(
                "letibot-mcp: the plugin layer `{layer}` declares the tool `{}`, and a plugin tool \
                 may not take a native one's name - it is not listed",
                tool.name
            );
            continue;
        }
        if !seen.insert(tool.name.clone()) {
            continue;
        }
        out.push(listed(
            &layer,
            scope.to_string().as_str(),
            &tool,
            &capabilities,
        ));
    }
    out
}

/// One declared tool as the MCP list spells it. The description says which layer answers it and
/// what the layer may reach, because that is the half of a tool's contract a client cannot see.
fn listed(layer: &str, scope: &str, tool: &ToolDecl, caps: &[Capability]) -> Value {
    let reach = if caps.is_empty() {
        "it reads nothing and runs nothing".to_string()
    } else {
        format!("capabilities: {caps:?}")
    };
    json!({
        "name": tool.name,
        "description": format!("{} (plugin `{layer}`, {scope}; {reach})", tool.description),
        "inputSchema": tool.schema,
    })
}

/// What this folder's layers declare, by name - the list `tools/call` checks a name against.
pub fn names(workspace: &Path, native: &[String]) -> Vec<String> {
    tools(workspace, native)
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect()
}

/// Run one declared tool through the stack: the same `tool_` input the daemon builds, with no looks.
///
/// The bridge has no session view - no backend, no boundary of its own - so a layer that asks for a
/// look is refused in words rather than answered off this process's disk. A tool that needs one
/// works in the daemon, and the refusal says so.
pub fn call(stack: &Stack, name: &str, args: &Value, workspace: &Path) -> Result<Value, String> {
    let hook = format!("tool_{name}");
    let input = json!({
        "v": 1,
        "name": name,
        "args": args,
        "feeds": {
            "home": std::env::var("HOME").unwrap_or_default(),
            "workspace": workspace.display().to_string(),
            "session": crate::rooms::handle(),
            "now_ms": now_ms(),
        },
        "looks": [],
        "state": Value::Null,
    });
    let resolved = stack.call(&hook, &input);
    if let Some((layer, why)) = &resolved.refused {
        return Err(format!(
            "the plugin layer `{layer}` failed to answer `{hook}`: {why}"
        ));
    }
    let Some(answer) = resolved.answer else {
        let broke: Vec<String> = resolved.failures.iter().map(|f| f.to_string()).collect();
        return Err(if broke.is_empty() {
            format!(
                "no plugin layer answers `{hook}`: this folder's layers declare the tool `{name}` \
                 and none of them answers it - the module may not have loaded, or a layer above it \
                 masked the hook"
            )
        } else {
            format!("`{hook}` was not answered: {}", broke.join("; "))
        });
    };
    let by = resolved.by.unwrap_or_default();
    if answer.get("looks").is_some() {
        return Err(format!(
            "`{}` asked for a look, and the bridge runs none: a look is read through the session's \
             own view, and a bridge has no session. The tool works in the daemon, where a session \
             does",
            if by.is_empty() { name } else { &by }
        ));
    }
    let outcome = answer.get("outcome").and_then(Value::as_str).unwrap_or("");
    let payload = answer.get("payload").and_then(Value::as_str).unwrap_or("");
    let reason = answer.get("reason").and_then(Value::as_str).unwrap_or("");
    let said = format!("answered by the plugin layer `{by}`");
    match outcome {
        "ok" => Ok(json!({ "payload": payload, "by": said })),
        "failed" | "abstained" | "refused" => Err(if reason.is_empty() {
            format!("`{name}` was {outcome} by `{by}`: {payload}")
        } else {
            format!("`{name}` was {outcome} by `{by}`: {reason}")
        }),
        _ => Err(format!(
            "`{by}` answered `{hook}` with something that is not a tool result: {answer}. An answer \
             is `{{\"outcome\":\"ok\",\"payload\":\"…\"}}` (or `failed`, `abstained`, `refused` \
             with a `reason`)"
        )),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Bridge;

    /// A directory of its own per test, removed when it ends.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let p = std::env::temp_dir()
                .join(format!("letibot-bridge-tools-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }

        /// One layer of a folder's scope, declaring one tool.
        fn layer(&self, scope: &str, name: &str, tool: &str) -> PathBuf {
            let dir = self.0.join(scope).join("plugins").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            // No inline table: this tree's TOML reader refuses one in a `schema`, so the schema is
            // spelled as the tables `plugins/builtin/*/plugin.toml` spell it.
            let manifest = format!(
                "name = \"{name}\"\npurpose = \"a tool this folder added\"\n\
                 hooks = [\"tool_{tool}\"]\ncapabilities = []\ncontract = 1\n\n\
                 [[tools]]\nname = \"{tool}\"\ndescription = \"what {tool} does\"\n\n\
                 [tools.schema]\ntype = \"object\"\n\n\
                 [tools.schema.properties.what]\ntype = \"string\"\ndescription = \"what to look at\"\n"
            );
            std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
            std::fs::write(dir.join("plugin.wasm"), b"a table, not a module").unwrap();
            // A fixture whose manifest does not parse is a test that proves nothing, and the
            // scanner skips one silently - so it is read here, where the failure names itself.
            let parsed = letibot_plugins::Manifest::read(&dir).expect("the fixture's manifest");
            assert!(parsed.tools.iter().any(|t| t.name == tool), "{parsed:?}");
            dir
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A `tool_` layer of the folder a bridge serves is an MCP tool like the native ones: listed
    /// after them, with the layer's own schema, and callable by the name it declares.
    #[test]
    fn a_user_tool_layer_is_listed_as_an_mcp_tool() {
        let dir = Dir::new("listed");
        dir.layer(".letibot", "lint", "lint_tree");
        let bridge = Bridge::new().with_names("sessions.db", dir.0.clone());

        let list = crate::tools::list_with(&bridge);
        let tools = list["tools"].as_array().expect("a tool list");
        let at = tools
            .iter()
            .position(|t| t["name"] == "lint_tree")
            .expect("the layer's tool is listed");
        assert_eq!(at, tools.len() - 1, "after the bridge's own tools");
        assert_eq!(tools[at]["inputSchema"]["type"], "object");
        let said = tools[at]["description"].as_str().unwrap_or_default();
        assert!(
            said.contains("what lint_tree does") && said.contains("`lint`"),
            "{said}"
        );
        assert!(
            said.contains("project") && said.contains("reads nothing"),
            "the layer and what it may reach are named: {said}"
        );
        assert!(
            crate::tools::names_with(&bridge)
                .iter()
                .any(|n| n == "lint_tree"),
            "a name in the list is a name `tools/call` takes"
        );
        // And the native tools are all still there, ahead of it.
        assert!(tools.iter().any(|t| t["name"] == "leticode_start"));
    }

    /// A user-scope layer is read through the same door - the OTHER root a person writes a layer in.
    #[test]
    fn a_user_scope_layer_is_listed_too() {
        let dir = Dir::new("user");
        let layer = dir.layer("config", "notes", "note_search");
        let declared = tools_in(
            &dir.0,
            layer.parent().and_then(Path::parent),
            &["leticode_start".to_string()],
        );
        assert_eq!(declared.len(), 1, "{declared:?}");
        assert_eq!(declared[0]["name"], "note_search");
    }

    /// A plugin tool may not take a native name: the client's tool list is one namespace, and the
    /// bridge's own verbs are how it is driven.
    #[test]
    fn a_plugin_tool_may_not_shadow_a_native_one() {
        let dir = Dir::new("shadow");
        dir.layer(".letibot", "boss", "leticode_start");
        let bridge = Bridge::new().with_names("sessions.db", dir.0.clone());
        let list = crate::tools::list_with(&bridge);
        let names: Vec<&str> = list["tools"]
            .as_array()
            .map(|t| t.iter().filter_map(|t| t["name"].as_str()).collect())
            .unwrap_or_default();
        assert_eq!(
            names.iter().filter(|n| **n == "leticode_start").count(),
            1,
            "one `leticode_start`, and it is the bridge's own: {names:?}"
        );
    }

    /// A declared tool is CALLED through the stack, in the input shape the daemon builds.
    #[test]
    fn a_listed_tool_is_called_through_the_stack() {
        let dir = Dir::new("called");
        dir.layer(".letibot", "lint", "lint_tree");
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stack = asking(
            &dir,
            &asked,
            "{\"outcome\":\"ok\",\"payload\":\"three files\"}",
        );
        let said = call(&stack, "lint_tree", &json!({"what": "."}), &dir.0).expect("an answer");
        assert_eq!(said["payload"], "three files");
        let seen = asked.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let input: Value = serde_json::from_str(&seen[0]).expect("the input is JSON");
        assert_eq!(input["name"], "lint_tree");
        assert_eq!(input["args"]["what"], ".");
        assert!(input["feeds"]["workspace"].as_str().is_some());
    }

    /// A layer that asks for a look gets a refusal, not a read off this process's disk.
    #[test]
    fn a_look_is_refused_rather_than_read_off_the_bridge() {
        let dir = Dir::new("look");
        dir.layer(".letibot", "lint", "lint_tree");
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stack = asking(&dir, &asked, "{\"looks\":[{\"read\":\"/etc/hostname\"}]}");
        let why = call(&stack, "lint_tree", &json!({}), &dir.0).expect_err("a refusal");
        assert!(
            why.contains("asked for a look") && why.contains("no session"),
            "{why}"
        );
        assert!(
            asked.lock().unwrap().len() == 1,
            "it was asked once and not again with a look"
        );
    }

    /// The folder's stack, over a loader that answers from a table and records every input - what
    /// the two tests above need and nothing a production path constructs.
    fn asking(dir: &Dir, asked: &Arc<std::sync::Mutex<Vec<String>>>, answer: &str) -> Arc<Stack> {
        Arc::new(Stack::discover(
            None,
            Some(dir.0.as_path()),
            None,
            Arc::new(Table {
                asked: asked.clone(),
                answer: Some(answer.to_string()),
            }),
        ))
    }

    struct Table {
        asked: Arc<std::sync::Mutex<Vec<String>>>,
        answer: Option<String>,
    }

    impl Loader for Table {
        fn load(&self, _wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
            Ok(Box::new(Asked {
                asked: self.asked.clone(),
                answer: self.answer.clone(),
            }))
        }
    }

    /// One module on a table: it answers every hook it declares with the same bytes, and writes down
    /// what it was asked.
    struct Asked {
        asked: Arc<std::sync::Mutex<Vec<String>>>,
        answer: Option<String>,
    }

    impl Module for Asked {
        fn hooks(&self) -> Vec<String> {
            vec!["tool_lint_tree".to_string()]
        }

        fn call(&self, _hook: &str, input: &str) -> Result<Option<String>, String> {
            self.asked.lock().unwrap().push(input.to_string());
            Ok(self.answer.clone())
        }

        fn fuel(&self) -> u64 {
            1
        }
    }
}

/// The stack a test builds without an engine: a table of hook answers, keyed by the hook's name.
/// Nothing in production constructs one; it exists so a test can prove the bridge's own rules (the
/// ask that cannot be silenced) without compiling wasm.
#[cfg(test)]
pub mod testing {
    use super::*;
    use std::collections::BTreeMap;

    pub struct Answers {
        /// `hook` -> the answer text, or `None` for *this layer passes*.
        pub table: BTreeMap<String, Option<String>>,
        /// Layers that answer every hook with a trap, to prove a broken layer loses nothing.
        pub traps: bool,
    }

    impl Loader for Answers {
        fn load(&self, wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
            Ok(Box::new(Tabled {
                wasm: wasm.display().to_string(),
                table: self.table.clone(),
                traps: self.traps,
            }))
        }
    }

    struct Tabled {
        wasm: String,
        table: BTreeMap<String, Option<String>>,
        traps: bool,
    }

    impl Module for Tabled {
        fn hooks(&self) -> Vec<String> {
            self.table.keys().cloned().collect()
        }

        fn call(&self, hook: &str, _input: &str) -> Result<Option<String>, String> {
            if self.traps {
                return Err(format!("{} trapped", self.wasm));
            }
            Ok(self.table.get(hook).cloned().flatten())
        }

        fn fuel(&self) -> u64 {
            1
        }
    }
}
