//! Every built-in pane, asked through the real engine with the inputs a head sends.
//!
//! `PluginHost::ensure_builtin_panes` (`crates/harnessd/src/plugins.rs`) puts each built-in at a
//! session's door and asks every `pane_` hook its first frame, so a pane that cannot answer is a
//! pane nobody opened and a sentence nobody wrote. Two defects came out of that door (R157): a pane
//! that passed on a frame with no feed, and two panes that exhausted a call's fuel - which the
//! engine reports as a trap with a symbol-less backtrace - on a daemon's own session list.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use letibot_harnessd::plugins::rano::RanoLoader;
use letibot_plugins::{Manifest, Stack};
use serde_json::{Value, json};

/// A stack over an empty project and user scope, with the binary's built-ins at the bottom.
fn builtin_stack(root: &Path) -> Stack {
    let mut stack = Stack::discover(None, Some(root), None, Arc::new(RanoLoader));
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "every built-in loads: {failed:?}");
    stack
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch(&format!("letibot-builtin-panes-{tag}"));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// Every pane hook the binary carries, as `(layer, hook, the feed names it declares)`.
fn panes() -> Vec<(String, String, Vec<String>)> {
    let mut out = Vec::new();
    for b in letibot_builtins::table() {
        let manifest = Manifest::parse(b.manifest).expect("the manifest parses");
        for hook in &manifest.hooks {
            if hook.starts_with("pane_") {
                out.push((
                    b.name.to_string(),
                    hook.clone(),
                    manifest
                        .feeds
                        .iter()
                        .map(|f| f.name().to_string())
                        .collect(),
                ));
            }
        }
    }
    out
}

/// One session's row, as the head's own `App::sessions_feed` spells one: the daemon's fields plus
/// the three only a head can decide - the project a workspace belongs to, the top-level group the
/// project is in, and the `system` branch a harness-run session sits under. The panes draw their
/// tree from those three, so a list with no project is a list with no rows.
fn row(i: usize) -> Value {
    json!({
        "id": format!("s-{i}"),
        "title": format!("the session number {i}"),
        "slug": "",
        "parent": Value::Null,
        "origin": "operator",
        "live": true,
        "running": false,
        "heads": 1,
        "items": 6,
        "stored": 0,
        "model": "qwen",
        "workspace": "/home/dead/Projects/letibot/worktrees/agent-x",
        "waits_on": 0,
        "nags_paused": false,
        "moved_ms": 1_760_000_000_000u64,
        "repo": "/home/dead/Projects/letibot",
        "project": "/home/dead/Projects/letibot",
        "project_name": "letibot",
        "bucket": "own",
        "system": Value::Null,
    })
}

/// The `sessions` feed a pane is handed: the daemon's list, the session the pane belongs to, the
/// host's home and the host's clock.
fn sessions_feed(rows: Vec<Value>) -> Value {
    json!({
        "sessions": rows,
        "fleet": [],
        "unlisted": Value::Null,
        "here": "s-0",
        "home": "/home/dead",
        "now": 1_760_000_200_000u64,
    })
}

/// The feed a pane declaring `name` is handed, in the shape asked for.
fn feed_named(name: &str, shape: &str) -> Option<Value> {
    match (name, shape) {
        // An empty list is a fact, not an absence: the pane must still answer, and say so.
        ("sessions", "empty") => Some(sessions_feed(Vec::new())),
        ("sessions", "rows") => Some(sessions_feed((0..3).map(row).collect())),
        // The host builds no feed of this kind (`PluginHost::feeds_for`), so the pane is asked
        // with none - which is the first-frame case, deliberately.
        _ => None,
    }
}

/// One pane's input: every field present, as `input_value` builds it.
fn input(feeds: BTreeMap<String, Value>, (w, h): (u32, u32), state: Value) -> Value {
    json!({
        "v": letibot_plugin::pane::V,
        "state": state,
        "look": Value::Null,
        "event": Value::Null,
        "size": {"w": w, "h": h},
        "feeds": feeds,
    })
}

/// The feeds a pane is handed for `shape`, keeping only what it declared.
fn feeds_for(declared: &[String], shape: &str) -> BTreeMap<String, Value> {
    let mut feeds = BTreeMap::new();
    for want in declared {
        if let Some(value) = feed_named(want, shape) {
            feeds.insert(want.clone(), value);
        }
    }
    feeds
}

/// The answer, as a view the host can drive - what `/pane` and the session picker both draw. The
/// sentence says which pane and which frame, because a trap's own words are a wasm backtrace.
fn drawn(answer: Option<Value>, case: &str) -> letibot_sessionlog::pane::View {
    let answer = answer.unwrap_or_else(|| panic!("{case}: the pane passed instead of answering"));
    let view: letibot_sessionlog::pane::View = serde_json::from_value(answer.clone())
        .unwrap_or_else(|e| panic!("{case}: not a C3 view ({e}), from {answer}"));
    view.validate()
        .unwrap_or_else(|e| panic!("{case}: a view the host refuses: {e}"));
    view
}

/// No failure recorded against any layer - a trap is a failure here, and the layer below it is
/// what would answer, so an absence of failures is what makes the answer the pane's own.
fn no_failure(stack: &Stack) -> bool {
    stack.failures().is_empty()
}

/// **Every built-in pane answers the frames a head sends it**: the synthetic first frame with no
/// feed at all, an empty feed, a frame at 20x5, and its own `state` handed back.
#[test]
fn every_builtin_pane_answers_the_frames_a_head_sends() {
    let tmp = tempdir("frames");
    let stack = builtin_stack(&tmp);
    let mut asked = 0usize;
    for (layer, hook, declared) in panes() {
        let frames = [
            ("the first frame, with no feed", BTreeMap::new(), (120, 40)),
            ("the first frame at 20x5", BTreeMap::new(), (20, 5)),
            ("an empty feed", feeds_for(&declared, "empty"), (120, 40)),
            (
                "an empty feed at 20x5",
                feeds_for(&declared, "empty"),
                (20, 5),
            ),
        ];
        let mut state = Value::Null;
        for (what, feeds, size) in frames {
            let r = stack.call(&hook, &input(feeds, size, state.clone()));
            let case = format!("{layer}/{hook}, {what}");
            assert!(r.failures.is_empty(), "{case}: {:?}", r.failures);
            assert_eq!(
                r.by.as_deref(),
                Some(format!("b-{layer}").as_str()),
                "{case}: answered by"
            );
            let view = drawn(r.answer, &case);
            // The state a pane answers with is handed back to it on the next frame; a pane that
            // could not read its own answer back would answer once and then fail.
            state = view.state.clone().unwrap_or(Value::Null);
            asked += 1;
        }
    }
    std::fs::remove_dir_all(&tmp).ok();
    assert!(no_failure(&stack), "{:?}", stack.failures());
    assert!(asked >= 8, "the binary's panes were asked: {asked} frames");
}

/// **A daemon's own session list answers instead of trapping.** Every pane that declares
/// `feeds = ["sessions"]` is handed the WHOLE list, and a call has a fixed fuel budget:
/// `pane_sessions` drew a row for each session through a `Value` tree, `pane_nicks` sorted its
/// roster with a scan and an allocation per comparison, and both read the feed out of the SDK's
/// `Value` envelope and then parsed it again. Past a few hundred sessions the fuel ran out and the
/// engine answered a trap - 17 and 18 frames of wasm backtrace against no symbols, which is what
/// room #1029 saw. Six hundred rows is where that was reproduced and where this now passes; the
/// fuel is asserted and not only the answer, because the answer is the budget.
#[test]
fn a_daemon_session_list_answers_instead_of_trapping() {
    let tmp = tempdir("rows");
    let stack = builtin_stack(&tmp);
    let rows: Vec<Value> = (0..600).map(row).collect();
    let feed = sessions_feed(rows);
    for (layer, hook) in [("sessions", "pane_sessions"), ("irc", "pane_nicks")] {
        let id = format!("b-{layer}");
        let r = stack.call(hook, &input(feeds_of(&feed), (120, 40), Value::Null));
        let case = format!("{layer}/{hook} with 600 sessions");
        assert!(r.failures.is_empty(), "{case}: {:?}", r.failures);
        assert_eq!(r.by.as_deref(), Some(id.as_str()), "{case}: answered by");
        let fuel = stack.fuel(&id).unwrap_or(u64::MAX);
        // Captured unless somebody asks (`--nocapture`): the number this change moved is worth
        // being able to read off a run, and worth nothing on a green one.
        eprintln!("{case}: {fuel} of {} fuel", rano::plugin::HOOK_FUEL);
        assert!(
            fuel < rano::plugin::HOOK_FUEL,
            "{case}: {fuel} of {} fuel",
            rano::plugin::HOOK_FUEL
        );
        let view = drawn(r.answer, &case);
        // The list is drawn, to its last row: an answer that skipped the work would be no answer.
        let said = serde_json::to_string(&view).unwrap_or_default();
        assert!(
            said.contains("the session number 599"),
            "{case}: the last session is on the list"
        );
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// The `sessions` feed, under the key the panes read it by.
fn feeds_of(feed: &Value) -> BTreeMap<String, Value> {
    BTreeMap::from([("sessions".to_string(), feed.clone())])
}
