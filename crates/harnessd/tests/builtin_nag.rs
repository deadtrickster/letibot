//! K3: the built-in `todo` layer's `nag_todo` says exactly what the harness's own plan-check says
//! (`docs/plugin-extension-points-wide.md` X13). The bottom of the stack is today's daemon, so
//! arriving at this hook moves no sentence - an operator's layer above it is the one that changes
//! the answer (R127's every-free-head is such a layer).
//!
//! Every expected line here is read off the harness's own function, `unfinished_plan_for`, and the
//! input is the one `the_nag_input` builds for the daemon: there is no copy of a sentence in this
//! file, which is what makes it a proof rather than a fixture agreeing with itself.

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
/// `unfinished_plan_for`'s words: so the layer's answer and the recorded line are the same string,
/// for every shape a plan comes in.
#[test]
fn the_builtin_nag_says_what_the_native_nag_said() {
    let tmp = tempdir("nag");
    let stack = builtin_stack(&tmp);
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
            Some("b-todo"),
            "the binary's own todo layer answers"
        );
        let answer = resolved.answer.expect("the layer answers");
        let out = nag::read(&answer).unwrap_or_else(|why| panic!("{}: {answer}", why));
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
