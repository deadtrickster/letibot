//! The plugin layers a session has, and the panes they answer: the daemon's half of the
//! plugin stack - which session holds which `Stack`, when a look runs, and what reaches a
//! head. The answer protocol, the precedence and the gate belong to `letibot_plugins`.
//!
//! Three submodules, by the role each plays: [`install`] builds the stack and reloads it,
//! [`hook`] asks it and reads the answers, [`pane`] draws what a head sees. What they share -
//! the host, its lock, and the helpers that would otherwise be a third copy - is here.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use letibot_plugins::{
    Builder, Capability, CargoBuilder, DeclaredTool, Failure, Loader, Manifest, Recorder, Scope,
    Stack,
};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::pane::{Block, Line, LookSpec, Panel, Role, Unavailable, View};
use letibot_sessionlog::registry::PaneDriver;
use letibot_transcript::text::clip_chars_within;
use serde_json::{Value, json};

pub mod conditions;
pub mod contract;
pub mod desk;
pub mod feed;
pub mod hook;
pub mod install;
pub mod look;
pub mod pane;
pub mod rano;
pub mod room_feed;
pub mod scaffold;
// A tool a layer declares, and the looks its hook asks the host to run: it is a `Tool` in the
// registry beside `read` and `bash`, and its three look primitives are a filesystem API.
pub mod tool;

// The dispatcher's four doors are named from here, so a caller outside this module asks the host
// for a hook by the path it always did: `plugins::CallHooks` and not where the code lives now.
pub use hook::{CallHooks, DigestHooks, FloorHooks, ReviewFloorHooks};

#[cfg(test)]
mod tests;

/// How often the hot-reload tick looks: the longest a person waits after saving a layer. Two
/// seconds, as `mergequeue::watch` polls - a stat per layer is free.
pub const POLL_EVERY: Duration = Duration::from_secs(2);

/// The rectangle a pane draws in when the head did not say one: a head too old to send its own
/// `w` and `h` gets a wide screen rather than a guess about its terminal.
pub const SIZE_W: u32 = 120;
pub const SIZE_H: u32 = 40;

/// The contract version every pane input carries. It is the SDK's constant and not a copy: a
/// hardcoded `1` outlived the contract's growth to 2, so a layer that read `v` was told no feed
/// by the host about to hand it one.
const PANE_V: u32 = letibot_plugin::pane::V;

/// The hook whose rows are the room's four controls. The match is on the hook and not the layer's
/// name: what makes a key a room verb is what the pane draws, so a project shipping its own layer
/// over `pane_room` is drawing the same room.
pub const ROOM_HOOK: &str = "room";

/// The pane's fifth control, and the one verb here that is not the room's: `claim`, `release`
/// `abandon` and `rule` are the room's state machine, while `restart` is a daemon lifetime asked of
/// the supervisor. It is declared in the manifest like the other four, so `validate_for` gates it.
pub const RESTART_VERB: &str = "restart";

/// One session's layers, its recorded inputs, and the panes a head has opened.
pub struct PluginHost {
    session: String,
    workspace: PathBuf,
    /// The session's checkout: the repository its tree belongs to (a worktree's is the checkout it
    /// was cut from), where a pane's relative `read` is looked for first - see [`look::Trees`].
    checkout: PathBuf,
    /// The session's own plugin directory - where `plugin scaffold` writes and where a
    /// session-scope layer is discovered. Beside the stack, which only knows what it found.
    session_dir: Option<PathBuf>,
    inner: Mutex<Inner>,
    /// How the gate's build step is performed: `CargoBuilder` in the daemon, a table of answers
    /// in a test, which cannot pay for the wasm target and a crate to build.
    builder: Arc<dyn Builder>,
    /// Where a pane's `sessions` feed comes from - the daemon's registry, set by
    /// [`PluginHost::with_sessions`]. `None` in a host a test built, and a layer that declares
    /// the feed is then handed no key at all rather than an empty list (see [`feed`]).
    sessions: Option<Arc<dyn feed::SessionFeed>>,
    /// Where a pane's view of the room comes from - the session's own connection to the box's
    /// coordinator, set by [`PluginHost::with_room`]. `None` in a test's host and on a box with no
    /// coordinator, and the `room` feed is then no key at all rather than an empty room.
    room: Mutex<Option<Arc<dyn room_feed::RoomFeed>>>,
    /// Where a pane's `github` feed comes from - the daemon's own reviewer seat, set by
    /// [`PluginHost::with_github`]. `None` in a test's host, and a layer declaring the feed is then
    /// handed no key at all rather than an empty seat.
    github: Option<Arc<dyn feed::GithubFeed>>,
    /// Whether this host installs the binary's own pane layers at the door a head comes through.
    /// The daemon's host does ([`PluginHost::production`]); a host built on a test's loader cannot,
    /// because a fake loader has no engine for a module held in memory.
    install_builtin_panes: bool,
    /// A head has arrived and the built-in panes are wanted; the next [`PluginHost::tick`] compiles
    /// them on a thread, so the attach itself never waits seconds for a compiler.
    builtins_wanted: std::sync::atomic::AtomicBool,
    /// The compile has been started once; it is not started twice.
    builtins_started: std::sync::atomic::AtomicBool,
}

/// What a spawned child takes from the session that spawned it: the directories of the parent's
/// session-scope layers whose bytes a person pinned, re-read on the child's own stack. Unpinned session
/// layers stay the parent's, because a child's work goes to review and an unpinned layer is an experiment.
#[derive(Clone, Debug, Default)]
pub struct Inherited {
    dirs: Vec<PathBuf>,
}

impl Inherited {
    /// A session that inherits nothing: every session the daemon starts itself.
    pub fn none() -> Inherited {
        Inherited::default()
    }

    /// What `parent` hands down. A pinned layer is one the trust book holds the bytes of, which is
    /// `Layer::trusted`; a disabled one is off in the parent and so off here.
    pub fn of(parent: &PluginHost) -> Inherited {
        let inner = parent.lock();
        Inherited {
            dirs: inner
                .stack
                .layers()
                .iter()
                .filter(|l| l.scope == Scope::Session && !l.disabled && l.trusted)
                .map(|l| l.dir.clone())
                .filter(|dir| !dir.as_os_str().is_empty())
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }
}

struct Inner {
    /// The layers in force, behind an `Arc` because a hook call runs OUTSIDE this lock: a caller
    /// takes a reference to the layers here, releases the session, calls the module, and retakes
    /// the lock to apply the answer. `Arc::make_mut` is what makes that safe - a change copies the
    /// stack rather than landing under a call that is reading the layers it started with.
    stack: Arc<Stack>,
    rec: Recorder,
    /// The panes a head has opened, by id, with what the pane returned and when it wants asking
    /// again.
    panes: BTreeMap<String, Open>,
    /// The rectangle each pane was last asked to draw in. Beside `panes` rather than inside an
    /// `Open` because a close is not a forget: reopened on the same screen, it draws at that size.
    sizes: BTreeMap<String, (u32, u32)>,
    /// Layer ids with a gate running right now, so a poll that sees the same change twice does
    /// not compile it twice.
    gating: BTreeSet<String>,
    /// What has already been said on the session's log. A failure said once is news; the same
    /// failure said every two seconds is noise a model learns to ignore.
    said: BTreeSet<String>,
    /// What the session's model has not been told yet. Drained by the worker, because a notice is
    /// a transcript row and only a harness writes one of those.
    notices: Vec<String>,
    /// When the next hot-reload poll is due.
    poll_at: Instant,
    /// Pins whose bytes moved before this host existed, held for the first tick that can say them.
    broken_pins: Vec<letibot_plugins::BrokenPin>,
    /// The hooks the binary's own layers have already been offered for. A built-in that answers a hook
    /// the daemon asks - a review's brief, a verdict's floor, a child's brief - is installed on the
    /// first ask rather than at open, because a module costs seconds to compile.
    builtin_asked: BTreeSet<String>,
}

impl Inner {
    /// The stack, for a caller that is going to change it. `Arc::make_mut` copies when a hook call
    /// holds the old one, so a reload never rewrites the layers under a module answering from them.
    fn stack_mut(&mut self) -> &mut Stack {
        Arc::make_mut(&mut self.stack)
    }

    /// What a call is being answered FROM: the layers and the bytes they run. An answer that comes
    /// back to a different footprint came from a stack that is no longer in force, and is dropped
    /// the way any late answer is. A poll that found nothing changed leaves this alone, which is
    /// what a hash of the layers buys over a version counter.
    fn footprint(&self) -> Footprint {
        let mut hasher = DefaultHasher::new();
        for layer in self.stack.layers() {
            layer.id.hash(&mut hasher);
            layer.scope.prefix().hash(&mut hasher);
            layer.sha256.hash(&mut hasher);
            layer.manifest_sha256.hash(&mut hasher);
            layer.disabled.hash(&mut hasher);
            layer.trusted.hash(&mut hasher);
            layer.manifest.hooks.hash(&mut hasher);
        }
        Footprint(hasher.finish())
    }
}

/// Which layers a hook call was answered from - see [`Inner::footprint`]. Compared and never read,
/// so the number is opaque on purpose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Footprint(u64);

/// What the host knows about one open pane.
struct Open {
    /// The `state` the pane answered with, handed back to it on the next call
    /// state belongs to the host, and a plugin is stateless between calls.
    state: Option<Value>,
    /// The look the pane's last answer asked for, so a refresh can run it again.
    spec: Option<LookSpec>,
    /// The layer that answered, whose capabilities a look is checked against.
    by: String,
    /// 0 is never; C3's floor is `sessionlog::pane::MIN_EVERY_S`.
    every_s: u64,
    next_at: Option<Instant>,
}

/// What one lifecycle event came to, for the caller that performs it: the acts a layer asked for,
/// already checked against its manifest's `actions`. There is no field for stopping the event,
/// because the answer a layer writes has none - that is what *a layer cannot block a reseat* means.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lifecycle {
    /// The layer that answered, when one did. Here so a caller can say whose acts these are.
    pub by: Option<String>,
    /// One act per entry: the verb `plugin.toml` declares, and its arguments.
    pub acts: Vec<(String, Value)>,
}

/// What one render is: which `look` and `event` the first call is handed, and whether this pass
/// may run the look the answer asks for.
struct Req {
    look: Option<Value>,
    event: Option<(String, String)>,
    may_look: bool,
}

impl Req {
    /// The pane's first frame: nothing looked at, nothing pressed.
    fn fresh() -> Req {
        Req {
            look: None,
            event: None,
            may_look: true,
        }
    }

    /// The same, for a caller that must not run a look: announcing a pane to a head that has not
    /// opened it.
    fn announced() -> Req {
        Req {
            may_look: false,
            ..Req::fresh()
        }
    }
}

impl PluginHost {
    pub fn session(&self) -> &str {
        &self.session
    }

    /// The tree a look resolves against - the session's own workspace.
    pub fn workspace(&self) -> PathBuf {
        self.workspace.clone()
    }

    pub(super) fn room_source(&self) -> Option<Arc<dyn room_feed::RoomFeed>> {
        self.room.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A refusal the `plugin` tool hands a model: the sentence saying what did not happen, and the
/// detail it is fixed from - a compiler's own words, the gate's report, the layers there are.
/// Two fields because a failed build needs both: the reason, and the forty lines a model edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub why: String,
    pub detail: String,
}

impl Refused {
    fn new(why: impl Into<String>, detail: impl Into<String>) -> Refused {
        Refused {
            why: why.into(),
            detail: detail.into(),
        }
    }
}

/// What one hook's answer was, and the layer it came from - the by-line a rewrite is reported
/// with.
#[derive(Debug, Clone, PartialEq)]
pub struct Answered<T> {
    pub out: T,
    pub by: String,
}

/// The `monitor` call a layer's `arm` becomes - the same door the model's own calls use, so the
/// coalescing, the already-watched refusal and the TTL caps stay the tool's. `repeat` is on, as the
/// harness's own pull-request watch is: a watch for one push's checks is worth having for the next.
pub fn arm_call(arm: &letibot_plugin::tool_result::Arm) -> letibot_transcript::ToolCall {
    let name = arm.plugin.clone();
    let arguments = serde_json::json!({
        "name": name,
        "plugin": arm.plugin,
        "args": arm.args,
        "repeat": true,
    });
    letibot_transcript::ToolCall {
        id: format!("plugin-arm-{name}"),
        name: "monitor".into(),
        arguments: arguments.to_string(),
    }
}

/// The layer that answers a hook, as a look needs it: its id, its manifest name, its scope, and
/// what it may look at. A name and an id because two readers want different ones - mechanisms use
/// the id (`s3`), and a sentence to a model carries the name.
pub struct Answering {
    pub id: String,
    pub name: String,
    pub scope: Scope,
    pub capabilities: Vec<Capability>,
    /// Whether this layer runs freely: a built-in, or a layer whose bytes a person pinned. An
    /// `exec` look of an unpinned layer goes through the gate; of a free one, it runs.
    pub trusted: bool,
}

/// The same layer, as a sentence a model reads: `name (scope)`.
impl std::fmt::Display for Answering {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.name, self.scope)
    }
}

/// A layer's amended result, held to the budget the spill already spent on the call: the payload
/// the layer saw was bounded, so an `append` or a rewrite that grows it is a plugin spending the
/// session's context. `None` is a runtime with no budget, which is no opinion and no cut.
pub fn bounded_result(text: String, cap: Option<usize>) -> String {
    let Some(cap) = cap else { return text };
    if text.len() <= cap {
        return text;
    }
    let note = format!(
        "\n\n[harness] a plugin layer's answer was cut here: it ran past the {cap} bytes a tool's \
         output is given inline, and a layer does not get to spend more of this session's context \
         than the call it answered"
    );
    // A budget too small to say why is still a budget: the bound is the promise and the line is
    // the courtesy, so a note longer than the cap loses to the bound.
    let room = cap.saturating_sub(note.len());
    let mut head = if cap <= note.len() { cap } else { room };
    while head > 0 && !text.is_char_boundary(head) {
        head -= 1;
    }
    if cap <= note.len() {
        return text[..head].to_string();
    }
    format!("{}{note}", &text[..head])
}

/// Where a write's provenance goes, on the daemon's side of `letibot_tools::provenance::Sink`. It
/// filters to what is about a layer and appends to the trust book, so *who wrote these bytes* is a
/// fact on disk. The book is its own `Arc<Trust>` and not the host's: they share the file.
pub struct Provenance {
    trust: Arc<letibot_plugins::Trust>,
    session: String,
}

impl Provenance {
    pub fn new(session: impl Into<String>) -> Provenance {
        Provenance {
            trust: letibot_plugins::Trust::open_default(),
            session: session.into(),
        }
    }

    /// The same, over a book a test hands in.
    pub fn over(trust: Arc<letibot_plugins::Trust>, session: impl Into<String>) -> Provenance {
        Provenance {
            trust,
            session: session.into(),
        }
    }
}

impl letibot_tools::provenance::Sink for Provenance {
    fn wrote(&self, path: &std::path::Path, bytes: &[u8]) {
        self.trust.note_write(path, bytes, &self.session);
    }
}

/// The hook part of a pane id (`<manifest name>/<hook name>`, without the `pane_` prefix).
fn pane_hook(pane: &str) -> &str {
    pane.split_once('/').map(|(_, hook)| hook).unwrap_or("")
}

/// The pane ids a stack's layers export, in stack order, each once.
fn pane_names(stack: &Stack) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for layer in stack.layers() {
        if layer.disabled || layer.manifest.contract != letibot_plugins::CONTRACT {
            continue;
        }
        for hook in &layer.manifest.hooks {
            let Some(name) = hook.strip_prefix("pane_") else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let pane = format!("{}/{name}", layer.manifest.name);
            if !out.contains(&pane) {
                out.push(pane);
            }
        }
    }
    out
}

/// A pane's input: the state it returned, the look's result, the key pressed, the rectangle, and
/// the feeds its layer declared. Every field is present, because a plugin deserialises this into a
/// struct and a missing key cannot be told from an empty one.
fn input_value(
    state: &Option<Value>,
    req: &Req,
    (w, h): (u32, u32),
    feeds: &BTreeMap<String, Value>,
) -> Value {
    let look = req.look.clone().unwrap_or(Value::Null);
    let event = match &req.event {
        Some((item, action)) => json!({"item": item, "action": action}),
        None => Value::Null,
    };
    json!({
        "v": PANE_V,
        "state": state.clone().unwrap_or(Value::Null),
        "look": look,
        "event": event,
        "size": {"w": w, "h": h},
        "feeds": feeds,
    })
}

/// C3's input, once, for a caller that has none of its own: the first frame of any pane - nothing
/// looked at, nothing pressed, no state.
fn synthetic_pane_input(feeds: &BTreeMap<String, Value>) -> Value {
    input_value(&None, &Req::fresh(), (SIZE_W, SIZE_H), feeds)
}

/// The plugin's answer, as a view this host can drive - or the sentence saying why not.
/// `validate_for` is the check that matters here: a row asking for a verb the layer never declared
/// is refused before it is drawn, because the daemon is what performs those verbs.
fn validated(answer: &Value, pane: &str, by: &str, declared: &[String]) -> Result<View, String> {
    let view: View = serde_json::from_value(answer.clone()).map_err(|e| {
        format!("the answer is not a C3 view ({e}), while `{pane}` was resolved from `{by}`")
    })?;
    view.validate_for(declared)?;
    Ok(view)
}

/// A pane that could not answer, as a pane that says so: an empty dashboard and a broken plugin
/// look identical on a screen.
fn unavailable(pane: &str, layer: &str, why: &str) -> Value {
    let view = View {
        title: pane.to_string(),
        panels: vec![Panel {
            id: "pane".to_string(),
            // One lowercase letter the host does not own, so a view this host made itself passes
            // the same `validate` a plugin's does.
            key: "u".to_string(),
            title: "pane".to_string(),
            summary: vec![Block::Lines(vec![Line {
                label: "answer".to_string(),
                value: Some("refused".to_string()),
                role: Some(Role::Failure),
                ..Line::default()
            }])],
            unavailable: Some(Unavailable {
                why: why.to_string(),
                fix: format!("the layer {layer} answered something the host refused"),
            }),
            ..Panel::default()
        }],
        ..View::default()
    };
    serde_json::to_value(&view).unwrap_or(Value::Null)
}

/// Say each stack failure once and queue it for the model, which is the one that can fix a layer -
/// which is what makes the fall-through loud rather than silent.
fn say_failures(hub: &Arc<Hub>, inner: &mut Inner, failures: &[Failure]) {
    for failure in failures {
        let said = failure.to_string();
        if !inner.said.insert(said.clone()) {
            continue;
        }
        inner.notices.push(format!(
            "a plugin layer failed and the layer below it answered: {said}"
        ));
        hub.warn("plugin_layer", said);
    }
}

/// Where a session's own layers live: `<data dir>/sessions/<id>/plugins`. One function because
/// three callers must agree - discovery, `plugin scaffold` and the daemon's host - and a
/// disagreement is a plugin written where nothing discovers it. `None` when the data dir is unknown.
pub fn session_plugins_dir(session: &str) -> Option<PathBuf> {
    crate::mergequeue::git::letibot_data_dir()
        .map(|dir| letibot_plugins::paths::session_plugin_dir(&dir, session))
}

/// `$XDG_CONFIG_HOME/letibot`, else `~/.config/letibot` - where the user scope's layers are, from
/// the one module every host reads it from: an empty `XDG_CONFIG_HOME` is unset, not a bare base.
fn user_config_dir() -> Option<PathBuf> {
    letibot_plugins::paths::config_dir()
}

/// The tools this session's layers declare, in precedence order, one per tool name. Read at
/// `Harness::open` - a declared tool is prompt bytes - and only from `plugin.toml`s: the built-in
/// scope is what the binary carries, not what this session asked for.
pub fn declared_tools(session: &str, workspace: &Path) -> Vec<DeclaredTool> {
    let mut all = Stack::declared_tools(
        session_plugins_dir(session).as_deref(),
        Some(workspace),
        user_config_dir().as_deref(),
    );
    let mut seen = BTreeSet::new();
    all.retain(|d| seen.insert(d.tool.name.clone()));
    all
}
