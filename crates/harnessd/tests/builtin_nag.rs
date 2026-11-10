//! K3: the built-in `todo` layer's `nag_todo` says exactly what the harness's own plan-check says
//! (`docs/plugin-extension-points-wide.md` X13). The bottom of the stack is today's daemon, so
//! arriving at this hook moves no sentence - an operator's layer above it is the one that changes
//! the answer (R127's every-free-head is such a layer).
//!
//! Every expected line here is read off the harness's own function, `unfinished_plan_for`, and the
//! input is the one `the_nag_input` builds for the daemon: there is no copy of a sentence in this
//! file, which is what makes it a proof rather than a fixture agreeing with itself.
//!
//! The agreement is asked of a PLACED copy of the layer's own bytes, because the built-in itself is
//! not called any more: `nag_todo` is declared `native_copy` (R175 item 5), so a stack whose only
//! answerer is the binary's layer takes the daemon's path instead of spending a wasm call on the
//! sentence it is about to write. Both halves are below, and the second is the reason the first is
//! still worth having.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use letibot_harnessd::plugins::rano::RanoLoader;
use letibot_plugin::nag;
use letibot_plugins::Stack;
use letibot_tokencore::store::{TodoBy, TodoItem, TodoNeed, TodoStatus};
use letibot_tools::builtins::todo::{
    ChildSessions, RunningJobs, the_nag_input, the_plan_as_checked, unfinished_plan_for,
};

/// A stack over an empty project and user scope, with the binary's built-ins at the bottom - the
/// same three lines `tests/builtin_layers.rs` uses, so this test asks the module the daemon loads.
fn builtin_stack(root: &Path) -> Stack {
    let mut stack = Stack::discover(None, Some(root), None, Arc::new(RanoLoader));
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "every built-in loads: {failed:?}");
    stack
}

/// The binary's `todo` layer written out as a PROJECT layer: the same `plugin.wasm` and the same
/// `plugin.toml`, minus `native_copy`, which is a built-in's to declare and a placed layer's to be
/// refused for. A placed layer is called, so this is how the module's words are still asked.
fn copied_todo_layer(root: &Path) {
    let builtin = letibot_builtins::table()
        .iter()
        .find(|b| b.name == "todo")
        .expect("the binary carries the todo layer");
    let dir = root.join(".letibot").join("plugins").join("todo");
    std::fs::create_dir_all(&dir).expect("a project layer directory");
    let manifest = builtin
        .manifest
        .lines()
        .filter(|l| !l.starts_with("native_copy"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.join("plugin.toml"), manifest).expect("plugin.toml");
    std::fs::write(dir.join("plugin.wasm"), builtin.wasm).expect("plugin.wasm");
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch(&format!("letibot-builtin-{tag}"));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

fn row(content: &str, status: TodoStatus, by: TodoBy) -> TodoItem {
    TodoItem {
        content: content.into(),
        status,
        by,
        when: None,
        needs: Vec::new(),
    }
}

/// The boards the daemon's plans come in: an operator's row ahead of the model's, a row in
/// progress with no wait (forgotten work), a row blocked on another row, and a row the operator
/// struck off. Each is read by the check's own filter, so what is askable is not this file's claim.
fn plans() -> Vec<Vec<TodoItem>> {
    vec![
        vec![
            row("land the release", TodoStatus::Pending, TodoBy::Operator),
            row("write the tests", TodoStatus::InProgress, TodoBy::Model),
        ],
        vec![
            row("write the tests", TodoStatus::InProgress, TodoBy::Model),
            row("open the branch", TodoStatus::Completed, TodoBy::Model),
        ],
        vec![
            TodoItem {
                needs: vec![TodoNeed::Row {
                    content: "open the branch".into(),
                }],
                ..row("land the release", TodoStatus::Pending, TodoBy::Operator)
            },
            row("open the branch", TodoStatus::Pending, TodoBy::Model),
        ],
        vec![
            // Everything left waits on a row the board does not have: the check's degenerate
            // case, where the harness names no row and says what cannot start.
            TodoItem {
                needs: vec![TodoNeed::Row {
                    content: "a row somebody renamed".into(),
                }],
                ..row("land the release", TodoStatus::Pending, TodoBy::Operator)
            },
            row("struck off", TodoStatus::Cancelled, TodoBy::Model),
        ],
    ]
}

/// What the store keeps for a delivered check is `nag_turn`'s sentence, and `nag_turn` says
/// `unfinished_plan_for`'s words: so the module's answer and the recorded line are the same string,
/// for every shape a plan comes in. Asked of a placed copy of the layer's bytes, because the
/// built-in itself is not called (see [`the_builtins_nag_is_not_called`]).
#[test]
fn the_builtin_nag_says_what_the_native_nag_said() {
    let tmp = tempdir("nag");
    copied_todo_layer(&tmp);
    let stack = Stack::discover(None, Some(&tmp), None, Arc::new(RanoLoader));
    let children = ChildSessions::none();
    let jobs = RunningJobs::none();
    let clock = nag::Clock {
        since_last_ms: Some(15 * 60_000),
        repeats: 1,
        idle_ms: 15 * 60_000,
        operator_added_ms: None,
        floor_ms: 60_000,
        cap_ms: 30 * 60_000,
    };
    let mut checked = 0;
    for board in plans() {
        let askable = the_plan_as_checked(&board, &children, &jobs);
        let Some((text, named)) = unfinished_plan_for(&askable, &board, None, &children, &jobs)
        else {
            continue;
        };
        let input = the_nag_input(
            &board,
            &askable,
            &text,
            (!named.is_empty()).then_some(named.as_str()),
            clock,
            &children,
            &jobs,
            15 * 60_000,
        );
        let asked = serde_json::to_value(&input).expect("the input serialises");
        let resolved = stack.call(nag::HOOK, &asked);
        assert!(resolved.failures.is_empty(), "{:?}", resolved.failures);
        assert_eq!(
            resolved.by.as_deref(),
            Some("p1"),
            "the project's copy of the layer answers"
        );
        let answer = resolved.answer.expect("the layer answers");
        let out = nag::read(&answer).unwrap_or_else(|why| panic!("{answer}: {why}"));
        let decision = out
            .decide(&input)
            .unwrap_or_else(|why| panic!("the host takes the built-in's answer: {why}"));
        assert!(decision.says, "the built-in never holds a check back");
        assert_eq!(decision.text, text, "the layer said what the native said");
        assert_eq!(
            decision.first().as_deref(),
            (!named.is_empty()).then_some(named.as_str()),
            "and named the row the native named"
        );
        checked += 1;
    }
    assert_eq!(
        checked, 4,
        "every fixture must be a plan the check speaks about"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// And the built-in itself answers nothing at all: `nag_todo` is declared `native_copy`, so the
/// stack leaves it alone and the daemon's own check stands - which is the same sentence, for free.
#[test]
fn the_builtins_nag_is_not_called() {
    let tmp = tempdir("nag-skip");
    let stack = builtin_stack(&tmp);
    let asked = serde_json::json!({"text": "a sentence", "name": "a row"});
    let resolved = stack.call(nag::HOOK, &asked);
    assert!(
        resolved.failures.is_empty(),
        "a skipped call is not a failure: {:?}",
        resolved.failures
    );
    assert_eq!(
        resolved.answer, None,
        "the daemon's own path, not a layer's"
    );
    assert_eq!(resolved.by, None);
    std::fs::remove_dir_all(&tmp).ok();
}
