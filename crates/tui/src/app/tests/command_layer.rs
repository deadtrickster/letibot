//! X23 at the head: a `/` line this head has not got is offered to `command_<verb>` before it is the
//! daemon's, and the acts this head performs are the ones its own code knows. Nothing is faked but the
//! layer's answer: the real module is compiled and the lines go through `App::command`.

use super::*;
use letibot_plugins::{Capability, Loader, Module, Stack};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Every input a layer was asked with - the only way to see what the head HANDS it, since the answer
/// alone cannot tell a layer that read the line from one that ignored it.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(String, Value)>>>);

impl Seen {
    fn record(&self, hook: &str, input: &str) {
        self.0.lock().expect("the recorder").push((
            hook.to_string(),
            serde_json::from_str(input).unwrap_or(Value::Null),
        ));
    }

    fn take(&self) -> Vec<(String, Value)> {
        std::mem::take(&mut *self.0.lock().expect("the recorder"))
    }
}

/// A layer with a scripted answer: `None` passes (`-1`, the stack asks the layer below), `Some` is
/// what the head reads, and `traps` is a layer that could not answer at all.
struct Scripted {
    seen: Seen,
    answer: Option<String>,
    traps: bool,
}

impl Module for Scripted {
    fn hooks(&self) -> Vec<String> {
        vec!["command_standup".to_string()]
    }

    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String> {
        self.seen.record(hook, input);
        if self.traps {
            return Err("the layer trapped".into());
        }
        Ok(self.answer.clone())
    }

    fn fuel(&self) -> u64 {
        0
    }
}

/// The loader that puts [`Scripted`] behind a project layer's `plugin.wasm`, and the REAL engine
/// behind the built-ins - so a test that means *the layer below* gets this binary's own module.
struct ScriptedLoader {
    seen: Seen,
    answer: Option<String>,
    traps: bool,
}

impl Loader for ScriptedLoader {
    fn load(&self, _wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        Ok(Box::new(Scripted {
            seen: self.seen.clone(),
            answer: self.answer.clone(),
            traps: self.traps,
        }))
    }

    fn load_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        caps: &[Capability],
    ) -> Result<Box<dyn Module>, String> {
        crate::app::sessions_pane::RanoLoader.load_bytes(name, bytes, caps)
    }
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "letibot-command-layer-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// A project's `standup` layer: a manifest that declares the hook and the one act, and a
/// `plugin.wasm` whose content the scripted loader ignores.
fn project_layer(root: &Path, seen: &Seen, answer: Option<&str>, traps: bool) -> Stack {
    let dir = root.join(".letibot").join("plugins").join("standup");
    std::fs::create_dir_all(&dir).expect("the project's plugin dir");
    std::fs::write(
        dir.join("plugin.toml"),
        "name = \"standup\"\n\
         purpose = \"a project's own `/standup` verb, scripted for this test\"\n\
         hooks = [\"command_standup\"]\n\
         capabilities = []\n\
         actions = [\"add_todo\"]\n\
         contract = 1\n",
    )
    .expect("a manifest");
    std::fs::write(dir.join("plugin.wasm"), b"not a module").expect("a module");
    let mut stack = Stack::discover(
        None,
        Some(root),
        None,
        Arc::new(ScriptedLoader {
            seen: seen.clone(),
            answer: answer.map(str::to_string),
            traps,
        }),
    );
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "the built-ins load: {failed:?}");
    stack
}

/// The head in one conversation, with a stack of the test's choosing - the same door
/// `todo_layer.rs` uses, because the folder a test can create is not the folder the fixture names
/// and a host that disagreed about it would be rebuilt on the next command.
fn a_head_with(stack: Stack) -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));
    a
}

/// A verb this head has not got is offered to `command_<verb>`, and the act it answers with is what
/// happens - the line never reaches the daemon, because a layer that answered owns the verb.
#[test]
fn an_unknown_head_verb_is_offered_to_its_command_layer() {
    let seen = Seen::default();
    let tmp = tempdir("unknown-verb");
    let stack = project_layer(
        &tmp,
        &seen,
        Some(r#"{"act":{"name":"add_todo","args":{"content":"stand up notes"}}}"#),
        false,
    );
    let mut a = a_head_with(stack);

    match a.command("standup write it up") {
        Some(Action::SetOperatorTodos { items, .. }) => {
            assert_eq!(items.len(), 1, "the layer's one act: {items:?}");
            assert_eq!(items[0].content, "stand up notes");
            assert_eq!(items[0].by, letibot_sessionlog::event::TodoBy::Operator);
        }
        other => panic!("expected the layer's act, got {other:?}"),
    }
    // What the layer was handed: the verb it is named after, the REST of the line as typed (the
    // shape `/todo`'s layer has always had), and the session it was typed in.
    let asked = seen.take();
    assert_eq!(asked.len(), 1, "one hook, asked once: {asked:?}");
    assert_eq!(asked[0].0, "command_standup");
    assert_eq!(asked[0].1["verb"], "standup");
    assert_eq!(asked[0].1["line"], "write it up");
    assert_eq!(asked[0].1["session"], "s");
    std::fs::remove_dir_all(&tmp).ok();
}

/// A layer that passes leaves the line to the daemon exactly as it was: today's path for every verb
/// this head does not know, and the reason the pass is `-1` rather than an empty answer.
#[test]
fn a_layer_that_passes_still_sends_the_line_to_the_daemon() {
    let seen = Seen::default();
    let tmp = tempdir("passes");
    let stack = project_layer(&tmp, &seen, None, false);
    let mut a = a_head_with(stack);

    match a.command("standup write it up") {
        Some(Action::Slash { line }) => assert_eq!(line, "standup write it up"),
        other => panic!("expected the daemon's line, got {other:?}"),
    }
    assert_eq!(seen.take().len(), 1, "it was asked, and it passed");
    std::fs::remove_dir_all(&tmp).ok();
}

/// A layer that traps is still not the end of the verb: the line goes to the daemon as it always
/// did, and the person is told why the layer was no help.
#[test]
fn a_trapping_layer_leaves_the_verb_to_the_daemon() {
    let seen = Seen::default();
    let tmp = tempdir("traps");
    let stack = project_layer(&tmp, &seen, None, true);
    let mut a = a_head_with(stack);

    assert!(matches!(
        a.command("standup write it up"),
        Some(Action::Slash { .. })
    ));
    let said = a.notice.clone().unwrap_or_default();
    assert!(
        said.contains("could not answer `/standup`"),
        "the trap is said where it was met: {said}"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// The other act this head performs: a sentence. A verb that only answers something needs nothing
/// more, and it is said rather than turned into a board row nobody asked for.
#[test]
fn a_say_act_is_said_on_the_head_and_sends_nothing() {
    let seen = Seen::default();
    let tmp = tempdir("says");
    let stack = project_layer(
        &tmp,
        &seen,
        Some(r#"{"act":{"name":"say","args":{"text":"yesterday: K10; today: the tests"}}}"#),
        false,
    );
    let mut a = a_head_with(stack);

    assert_eq!(a.command("standup"), None);
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("yesterday: K10"), "the layer's words: {said}");
    assert!(a.operator_todos().is_empty(), "and no row was invented");
    std::fs::remove_dir_all(&tmp).ok();
}

/// An act this head does not perform is said, not swallowed, and the line does not ALSO go to the
/// daemon: a layer that answered owns the verb, and half-performing it twice is worse than either.
#[test]
fn an_act_the_head_does_not_know_says_so_and_sends_nothing() {
    let seen = Seen::default();
    let tmp = tempdir("other-act");
    let stack = project_layer(
        &tmp,
        &seen,
        Some(r#"{"act":{"name":"run_shell","args":{"command":"rm -rf /"}}}"#),
        false,
    );
    let mut a = a_head_with(stack);

    assert_eq!(a.command("standup write it up"), None);
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("run_shell"), "the act is named: {said}");
    assert!(
        said.contains("not sent to the daemon"),
        "and the line did not go on: {said}"
    );
    assert!(a.operator_todos().is_empty(), "nothing was added");
    std::fs::remove_dir_all(&tmp).ok();
}

/// Every verb this head answers is either one a layer cannot take or one a LAYER SHIPPED WITH this
/// build answers: `HOST_VERBS` is checked against the head's own tables here, so a new `/verb` cannot
/// arrive without a decision about it - the same rule `HOST_KEYS` has, one namespace over. `/todo`
/// is the exception the built-in `command_todo` layer IS, which is the hook's whole design.
#[test]
fn every_head_verb_is_one_a_layer_cannot_take() {
    let covered = |verb: &str| {
        letibot_plugin::command::is_host_verb(verb)
            || letibot_plugin::command::is_builtin_verb(verb)
    };
    for (name, _) in SLASH_COMMANDS {
        // A row that names a verb and a modifier (`reseat summarise`) is the same verb: the base is
        // in the list, and the arm that reads the modifier is not a second verb.
        if name.contains(char::is_whitespace) {
            continue;
        }
        assert!(
            covered(name),
            "`/{name}` is a head verb and neither `HOST_VERBS` nor `BUILTIN_VERBS` names it"
        );
    }
    // Aliases too: `/v` answers a verb, so a layer taking that word would be shadowed by an arm
    // nobody offered.
    for alias in HEAD_COMMAND_ALIASES {
        let verb = alias.trim_start_matches('/');
        assert!(
            covered(verb),
            "`/{verb}` is a head alias and nothing names it"
        );
    }
    // And the built-in list is not a hole in the rule: a word in it is one the shipped layers
    // really do answer.
    for verb in letibot_plugin::command::BUILTIN_VERBS {
        assert!(
            SLASH_COMMANDS.iter().any(|(n, _)| n == verb),
            "`/{verb}` is called a built-in layer's verb and no head arm answers it"
        );
    }
}
