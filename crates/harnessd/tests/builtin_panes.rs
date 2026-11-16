//! Every built-in pane, asked through the real engine with the inputs a head sends.
//!
//! `PluginHost::ensure_builtin_panes` asks every `pane_` hook its first frame at a session's door,
//! so a pane that cannot answer is never opened. Covered: a frame with no feed, and a call's fuel
//! running out on a daemon's own session list (a trap with a symbol-less backtrace).

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

/// One session's row as the head's `App::sessions_feed` spells it: the daemon's fields plus the
/// project, its top-level group and the `system` branch a harness-run session sits under. The
/// panes draw their tree from those three, so a list with no project has no rows.
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
        // Every other feed is given as none, deliberately the first-frame case: a pane must answer before
        // anybody attached a scope, joined a room or opened the queue cluster. The github pane with a seat
        // behind it is `crate::github::tests::the_daemon_answers_the_github_feed_for_an_attached_scope`.
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

/// Every built-in pane answers the frames a head sends it: the synthetic first frame with no
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
            // Chat passes a frame it cannot draw (no feed, no clock) so the head's own chat draws
            // it: the native renderer is the safety net until this layer replaces it.
            if layer == "chat" {
                assert_eq!(r.by, None, "{case}: chat hands the frame to the head");
                continue;
            }
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

/// A daemon's own session list answers instead of trapping. Every `feeds = ["sessions"]` pane is
/// handed the whole list against a fixed fuel budget, which per-row work can run out past a few
/// hundred sessions. Six hundred rows, and the fuel is asserted too: the answer is the budget.
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
        // The nick list keeps the feed's first `SESSION_FEED_ROWS`, as the host cuts it, so its
        // last row is not 599; the sessions pane draws whatever it is handed.
        let last = if layer == "irc" {
            "the session number"
        } else {
            "the session number 599"
        };
        assert!(
            said.contains(last),
            "{case}: the list is drawn: {said:.300}"
        );
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// The `sessions` feed, under the key the panes read it by.
fn feeds_of(feed: &Value) -> BTreeMap<String, Value> {
    BTreeMap::from([("sessions".to_string(), feed.clone())])
}
