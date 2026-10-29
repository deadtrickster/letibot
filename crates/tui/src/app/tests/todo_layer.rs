//! **`/todo` through a layer** — the `command_` seam of `docs/plugins-plan.md` §4.7, and the
//! feeds of its Feeds section.
//!
//! The seam's shape, once, because every test here is one line of it: the head owns the slash line
//! and its own five verbs, the layer owns *what a new row may look like*, and the head performs the
//! one act it has heard of. So there are exactly four things a test can stand in:
//!
//! | the layer | the head |
//! |---|---|
//! | the built-in `todo` (`b-todo`, this binary's own module) | adds `title — body` |
//! | a project layer with a scripted answer | performs `add_todo`, ignores every other act name |
//! | a layer that passes (`-1`) | answers the line exactly as it did before there was a layer |
//! | a layer that traps | says so, and the layer below answers |
//!
//! **Nothing is faked except the layer's ANSWER.** `RanoLoader` compiles the real module for the
//! built-in, `letibot_builtins::install` is the same function the head calls, and the commands are
//! driven through `App::command` — the composer's own door — so a test that passes here is a head
//! that answers the line.

use super::*;
use letibot_plugins::{Capability, Loader, Module, Stack};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// **Every input a layer was asked with** — the only way to see what the head HANDS a layer, since
/// the answer alone cannot tell a layer that read the feeds from one that ignored them.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(String, Value)>>>);

impl Seen {
    fn record(&self, hook: &str, input: &str) {
        self.0.lock().expect("the recorder").push((
            hook.to_string(),
            serde_json::from_str(input).unwrap_or(Value::Null),
        ));
    }

    /// Everything recorded, taken — so a test asserts about one command's input and not the last.
    fn take(&self) -> Vec<(String, Value)> {
        std::mem::take(&mut *self.0.lock().expect("the recorder"))
    }

    fn len(&self) -> usize {
        self.0.lock().expect("the recorder").len()
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
        vec!["command_todo".to_string()]
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
/// behind the built-ins — so a test that means *the layer below* gets this binary's own module.
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
        "letibot-todo-layer-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// **A project's `todo` layer**, as a person's `edit` leaves it: a manifest that declares the hook
/// and the one act, and a `plugin.wasm` whose content the scripted loader ignores.
fn project_layer(root: &Path, seen: &Seen, answer: Option<&str>, traps: bool) -> Stack {
    let dir = root.join(".letibot").join("plugins").join("todo");
    std::fs::create_dir_all(&dir).expect("the project's plugin dir");
    std::fs::write(
        dir.join("plugin.toml"),
        "name = \"todo\"\n\
         purpose = \"a project's own reading of a `/todo` line, scripted for this test\"\n\
         hooks = [\"command_todo\"]\n\
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

/// The same stack **without the built-in table** — the shape in which a layer that could not
/// answer leaves the line to the HEAD rather than to `b-todo`: a binary whose built-in failed to
/// load (`install_builtins`' sentence, or a stale table), or a project layer trapping on a line
/// the built-in below would have passed on. It is the only shape that reaches
/// `todo_command`'s `_ if !rest.is_empty() => add_operator_row` arm, which is the arm the
/// reviewer's finding was about: the test that used `project_layer` was answered by `b-todo`
/// and so measured nothing of the head's own fallback.
fn project_layer_without_builtins(root: &Path, seen: &Seen) -> Stack {
    let dir = root.join(".letibot").join("plugins").join("todo");
    std::fs::create_dir_all(&dir).expect("the project's plugin dir");
    std::fs::write(
        dir.join("plugin.toml"),
        "name = \"todo\"\n\
         purpose = \"a project's own reading of a `/todo` line, scripted for this test\"\n\
         hooks = [\"command_todo\"]\n\
         capabilities = []\n\
         actions = [\"add_todo\"]\n\
         contract = 1\n",
    )
    .expect("a manifest");
    std::fs::write(dir.join("plugin.wasm"), b"not a module").expect("a module");
    Stack::discover(
        None,
        Some(root),
        None,
        Arc::new(ScriptedLoader {
            seen: seen.clone(),
            answer: None,
            traps: true,
        }),
    )
}

/// The head in one conversation, with a stack of the test's choosing — the same door
/// `sessions_way_out.rs` uses, because the folder a test can create is not the folder the fixture
/// names and a host that disagreed about it would be rebuilt on the next command.
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

/// **The operator's own line, end to end** — `/todo title:"a" body:"b"` is one row reading
/// `a — b`, filed as the operator's, pending, sent as `SetOperatorTodos`.
///
/// The layer is the one this BINARY carries: `Host::build` discovers the scopes, installs the
/// built-in table and compiles the module, which is the whole of *"the layer is actually
/// installed"* — the em dash in the content is the proof that the guest parsed the line, because
/// the head's own reading of it would be the literal `title:"a" body:"b"`.
#[test]
fn the_builtin_layer_answers_the_operators_title_and_body() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build("s", &workspace));

    let act = a.command(r#"todo title:"a" body:"b""#);
    match act {
        Some(Action::SetOperatorTodos { items, moved }) => {
            assert_eq!(items.len(), 1, "exactly one row: {items:?}");
            assert_eq!(items[0].content, "a — b", "the guest's join: {items:?}");
            assert_eq!(items[0].by, letibot_sessionlog::event::TodoBy::Operator);
            assert_eq!(
                items[0].status,
                letibot_sessionlog::event::TodoStatus::Pending
            );
            assert!(items[0].when.is_none() && items[0].needs.is_empty());
            assert!(moved.is_empty());
        }
        other => panic!("expected the add, got {other:?}"),
    }
    // The row is on the head's board from the moment it is sent (`echo_operator_todos`), so the
    // pane shows it without waiting for the daemon.
    assert_eq!(a.operator_todos().len(), 1);
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("added to your list"),
        "the add is confirmed in words: {:?}",
        a.notice
    );
}

/// **`done 1` is the head's, and the layer never sees it.** Every verb in `OWN_VERBS` is driven at
/// a recording layer, because the list of the head's verbs and the `match` that answers them are
/// two places that have to agree — and `when` was missing from the list once, which made
/// `/todo when 1 j7` a row called `when 1 j7`.
#[test]
fn the_heads_own_verbs_never_reach_a_layer() {
    let seen = Seen::default();
    let tmp = tempdir("own-verbs");
    let stack = project_layer(&tmp, &seen, None, false);
    let mut a = a_head_with(stack);
    // A row to act on, added by the head's own fallback (the layer passes).
    assert!(a.command("todo buy milk").is_some());
    seen.take();
    assert_eq!(a.operator_todos().len(), 1);

    // **Every word the head answers**, and not one of them is offered.
    for line in [
        "done 1",
        "rm 1",
        "postpone 1",
        "resume 1",
        "when 1 j7",
        // And a bare `/todo`, which opens the card rather than adding anything.
        "",
    ] {
        a.command(&format!("todo {line}"));
    }
    assert_eq!(
        seen.len(),
        0,
        "the head's own verbs are the head's: {:?}",
        seen.take()
    );
    // **And `done 1` still does what it did**: the first of the operator's rows is completed.
    assert!(matches!(
        a.command("todo done 1"),
        Some(Action::SetOperatorTodos { items, .. })
            if items[0].status == letibot_sessionlog::event::TodoStatus::Completed
    ));
    assert_eq!(seen.len(), 0, "still not offered");
    std::fs::remove_dir_all(&tmp).ok();
}

/// **A layer that answers an act this head does not perform adds nothing** — and says so.
///
/// `plugin.toml`'s `actions` and the head's one `add_todo` arm are two ends of one sentence: the
/// manifest is what a reviewer reads and what the gate can check, and the head is what refuses to
/// be talked into a verb. A layer that answered `{"act":{"name":"run_shell"}}` gets a line nothing
/// happens to, which is the whole of why `actions` is a list of words and not a code path.
#[test]
fn an_act_the_head_does_not_know_adds_nothing() {
    let seen = Seen::default();
    let tmp = tempdir("other-act");
    let stack = project_layer(
        &tmp,
        &seen,
        Some(r#"{"act":{"name":"run_shell","args":{"command":"rm -rf /"}}}"#),
        false,
    );
    let mut a = a_head_with(stack);

    assert_eq!(
        a.command("todo title:\"a\" body:\"b\""),
        None,
        "an unknown act is not a row"
    );
    assert!(
        a.operator_todos().is_empty(),
        "and nothing was added: {:?}",
        a.operator_todos()
    );
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("run_shell"), "the act is named: {said}");
    assert!(said.contains("nothing was added"), "{said}");
    // It WAS asked, with the line the operator typed — so the silence is the head's refusal and
    // not a layer that never ran.
    let asked = seen.take();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0, "command_todo");
    assert_eq!(asked[0].1["line"], "title:\"a\" body:\"b\"");
    std::fs::remove_dir_all(&tmp).ok();
}

/// **A layer that passes leaves the line to the head** — the way out, and what every `/todo` was
/// before there was a layer. `-1` is not *no*, it is *not mine*.
#[test]
fn a_layer_that_passes_leaves_the_line_to_the_head() {
    let seen = Seen::default();
    let tmp = tempdir("pass");
    // **No built-ins, and a layer that traps**, so the line reaches the head's own reading — the
    // fallback arm — with a failure in hand. With `project_layer` the built-in answered `buy milk`
    // itself and this test measured nothing of the arm it names.
    let stack = project_layer_without_builtins(&tmp, &seen);
    let mut a = a_head_with(stack);

    match a.command("todo buy milk") {
        Some(Action::SetOperatorTodos { items, .. }) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].content, "buy milk", "the head's own reading");
            assert_eq!(items[0].by, letibot_sessionlog::event::TodoBy::Operator);
        }
        other => panic!("expected the head's own add, got {other:?}"),
    }
    assert_eq!(seen.len(), 1, "it was asked, and it passed");
    // **And the layer's failure is what the person reads LAST** — `say` keeps only the last line,
    // so a failure said before the row's own sentence is overwritten by it and a session whose
    // layer is broken adds rows silently. This assertion is the one that fails on the old order.
    let said = a.notice.clone().unwrap_or_default();
    assert!(
        said.contains("could not answer `/todo`"),
        "the row's own sentence overwrote the layer's failure: {said}"
    );
    assert_eq!(
        a.take_sessions_failure(),
        None,
        "said once, and taken rather than left for a pane"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// **The head hands the layer the feeds, and only what it holds** — the `feeds` object beside
/// `line`: the whole board, the session's id and folder, and the project's `TODO.md` **only when
/// the head has read it**.
///
/// The `repo_todos` half is the interesting one: the todos pane reads the file at DRAW time, so a
/// head that never opened the pane holds no outline at all. That is an ABSENT key, not an empty
/// list — `[]` would tell a layer the project's list is empty, and a layer deciding whether a row
/// is already in it would believe that.
#[test]
fn the_head_hands_the_layer_the_board_and_the_session_and_the_repo_outline() {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoStatus};
    let seen = Seen::default();
    let tmp = tempdir("feeds");
    let stack = project_layer(
        &tmp,
        &seen,
        Some(r#"{"act":{"name":"add_todo","args":{"content":"a row"}}}"#),
        false,
    );
    let mut a = a_head_with(stack);

    // **A board with both halves on it**, as the daemon publishes it.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TodosUpdated {
            todos: vec![
                TodoEntry {
                    content: "the model's row".into(),
                    status: TodoStatus::InProgress,
                    by: TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                },
                TodoEntry {
                    content: "the operator's row".into(),
                    status: TodoStatus::Postponed,
                    by: TodoBy::Operator,
                    when: None,
                    needs: Vec::new(),
                },
            ],
        },
    )));
    // **No `TODO.md` read yet** — the pane has not drawn.
    a.repo_todos = None;
    assert!(a.command("todo anything").is_some());
    let asked = seen.take();
    assert_eq!(asked.len(), 1);
    let feeds = &asked[0].1["feeds"];
    assert_eq!(
        feeds["todos"],
        serde_json::json!([
            {"content": "the model's row", "status": "in_progress", "by": "model"},
            {"content": "the operator's row", "status": "postponed", "by": "operator"},
        ]),
        "the whole board, in board order, in the wire's own words"
    );
    assert_eq!(feeds["session"]["id"], "s");
    assert_eq!(
        feeds["session"]["workspace"],
        a.workspace_path().to_string_lossy().as_ref(),
        "the folder the head would look for layers in"
    );
    assert!(
        feeds.get("repo_todos").is_none(),
        "absent, not empty: the head holds no outline yet — {feeds}"
    );

    // **Now the head has drawn the pane**, and the outline travels as the pane drew it.
    a.repo_todos = Some(vec![TodoRow {
        indent: 4,
        mark: Some(TodoMark::Done),
        text: "T1 git init".into(),
        body: Vec::new(),
        item: true,
    }]);
    assert!(a.command("todo anything else").is_some());
    let asked = seen.take();
    assert_eq!(
        asked[0].1["feeds"]["repo_todos"],
        serde_json::json!([{"text": "T1 git init", "mark": "[x]", "indent": 4, "item": true}]),
        "the file's outline, as the pane holds it"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// **A layer that traps is said, and the layer below answers** — fail down, loudly
/// (`docs/plugins-plan.md` §1, principle 4). The broken layer costs the person a sentence and
/// nothing else: the built-in `todo` below it reads the line, and the row is added.
#[test]
fn a_layer_that_traps_is_said_and_the_builtin_below_answers() {
    let seen = Seen::default();
    let tmp = tempdir("trap");
    let stack = project_layer(&tmp, &seen, None, true);
    let mut a = a_head_with(stack);

    match a.command(r#"todo title:"a" body:"b""#) {
        Some(Action::SetOperatorTodos { items, .. }) => {
            assert_eq!(items.len(), 1);
            assert_eq!(
                items[0].content, "a — b",
                "the built-in below the trap parsed the line: {items:?}"
            );
        }
        other => panic!("expected the built-in's row, got {other:?}"),
    }
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("could not answer `/todo`"), "{said}");
    assert!(said.contains("the layer trapped"), "and why: {said}");
    // Said once, and not again: the sentence was taken, not left in the slot for a pane.
    assert_eq!(a.take_sessions_failure(), None);
    std::fs::remove_dir_all(&tmp).ok();
}
