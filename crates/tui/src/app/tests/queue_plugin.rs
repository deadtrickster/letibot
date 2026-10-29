//! **The queue pane, drawn by a plugin** — `plugins/builtin/queue`, through the head's own host.
//!
//! Three claims, and each one is a seam that can be silently dead:
//!
//! | the claim | the test |
//! |---|---|
//! | the built-in `queue` layer loads and draws the pane | `the_builtin_queue_layer_draws_the_pane` |
//! | a row's act names ITS entry, not the host's cursor | `a_rows_verb_acts_on_the_entry_the_row_names` |
//! | an act the manifest does not declare is refused | `a_layer_that_uses_an_undeclared_verb_falls_back` |
//!
//! **Nothing is faked except the answer** where it can be avoided: `RanoLoader` compiles the real
//! module `crates/builtins` embedded, `letibot_builtins::install` is the function the head calls,
//! and the pane is drawn through `App::screen` — so a test that passes here is a head that draws it.

use super::*;
use letibot_plugins::{Capability, Loader, Module, Stack};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// **A layer with a scripted answer**, so a test can stand in a view the built-in would not draw —
/// and record what it was asked, which the answer alone cannot tell.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<Value>>>);

impl Seen {
    fn record(&self, input: &str) {
        self.0
            .lock()
            .expect("the recorder")
            .push(serde_json::from_str(input).unwrap_or(Value::Null));
    }

    fn take(&self) -> Vec<Value> {
        std::mem::take(&mut *self.0.lock().expect("the recorder"))
    }
}

/// A layer that answers exactly what it was given, or traps.
struct Scripted {
    seen: Seen,
    answer: Option<String>,
}

impl Module for Scripted {
    fn hooks(&self) -> Vec<String> {
        vec!["pane_queue".to_string()]
    }

    fn call(&self, _hook: &str, input: &str) -> Result<Option<String>, String> {
        self.seen.record(input);
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
}

impl Loader for ScriptedLoader {
    fn load(&self, _wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        Ok(Box::new(Scripted {
            seen: self.seen.clone(),
            answer: self.answer.clone(),
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

/// A project directory holding one `queue` layer whose manifest is `manifest`.
fn a_project(tag: &str, manifest: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "letibot-queue-pane-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let layer = dir.join(".letibot").join("plugins").join("queue");
    std::fs::create_dir_all(&layer).expect("the project's plugin dir");
    std::fs::write(layer.join("plugin.toml"), manifest).expect("the manifest");
    // The module is a placeholder: the scripted loader answers for it, and the manifest's `admit`
    // only checks the HOOKS, which come from the module the loader hands back.
    std::fs::write(layer.join("plugin.wasm"), b"\0asm").expect("a module");
    dir
}

/// The manifest a scripted layer carries — the same shape the built-in's does.
fn a_manifest(actions: &str) -> String {
    format!(
        "name = \"queue\"\n\
         purpose = \"a scripted queue pane\"\n\
         hooks = [\"pane_queue\"]\n\
         capabilities = []\n\
         feeds = [\"queue\"]\n\
         actions = [{actions}]\n\
         contract = 1\n"
    )
}

/// **A head holding a snapshot** with two live rows and a finished one — the fixture the operator's
/// screenshot is about.
fn a_head_with_a_queue() -> App {
    let mut a = app();
    a.session_id = "s1".into();
    a.now_ms = 62_500;
    a.apply(queue_frame(queues(
        vec![
            queue_review_row("one", "reviewing"),
            queue_review_row("two", "landed"),
        ],
        vec![queue_merge_row("m-three", "queued")],
    )));
    a.queues_got_ms = 62_500;
    a.command("queue");
    a
}

/// **The built-in `queue` layer loads through the real engine and draws the pane** — the module this
/// binary carries, the same `install` the head calls, and the frame `App::screen` produces.
#[test]
fn the_builtin_queue_layer_draws_the_pane() {
    let mut a = a_head_with_a_queue();
    // The head's own host, with this binary's built-ins at the bottom — the production path.
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
        "s1",
        &std::env::temp_dir(),
    ));
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("the queue"),
        "the layer's title, not the host pane's: {screen}"
    );
    assert!(
        screen.contains("agent/one") && screen.contains("agent/three"),
        "both live rows are drawn: {screen}"
    );
    assert!(
        !screen.contains("agent/two"),
        "the finished row is behind the fold, not on the frame: {screen}"
    );
    assert!(
        screen.contains("finished · 1"),
        "and the fold says how many there are: {screen}"
    );
    // The host pane's own title is nowhere — this is the layer's pane.
    assert!(
        !screen.contains("review queue · merge queue"),
        "the host pane did not draw this: {screen}"
    );
    assert!(
        !screen.contains("asking the daemon"),
        "and it is not the empty frame: {screen}"
    );
}

/// **The plugin's view draws at 80×24 and at 239×61 without a refusal** — the two rectangles the
/// brief names, and the two a real terminal gives: a narrow one where every column competes and a
/// wide one where nothing is cut.
#[test]
fn the_plugins_view_draws_at_both_widths_without_a_refusal() {
    for (w, h) in [(80, 24), (239, 61)] {
        let mut a = a_head_with_a_queue();
        *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
            "s1",
            &std::env::temp_dir(),
        ));
        a.cfg.width = w;
        let rows = a.screen(w, h);
        let screen = rows.join("\n");
        // **A refused view is a pane that says why**, and that is what this rules out: the host
        // draws the reason in a one-panel view whose title is the pane's name.
        assert!(
            !screen.contains("this head cannot read that pane")
                && !screen.contains("the host's pane contract refuses"),
            "{w}×{h}: the view was refused:\n{screen}"
        );
        assert!(
            screen.contains("agent/one"),
            "{w}×{h}: the live row is drawn:\n{screen}"
        );
        assert!(
            rows.iter().all(|r| crate::app::line_width(r) <= w),
            "{w}×{h}: a row is wider than the pane:\n{screen}"
        );
    }
}

/// **A row's verb acts on the entry the ROW names** — the seam `Act::Verb` exists for.
///
/// The host's own cursor is on the first row; the plugin's view is written so the row it draws
/// carries `approve` for a *different* entry. Pressing `a` must send the row's entry, and that is
/// the whole difference between this and the pane it replaces.
#[test]
fn a_rows_verb_acts_on_the_entry_the_row_names() {
    let seen = Seen::default();
    let view = json!({
        "title": "a queue",
        "panels": [{"id": "rows", "title": "live", "summary": [{"tree": {"rows": [
            {"id": "row-i-am-looking-at", "cells": ["[~]", "agent/mine", "reviewing"],
             "enter": {"verb": "approve", "id": "one"}},
            {"id": "another-row", "cells": ["[ ]", "agent/other", "waiting"],
             "enter": {"verb": "approve", "id": "m-three"}},
        ]}}]}],
    })
    .to_string();
    let project = a_project(
        "act",
        &a_manifest("\"approve\", \"veto\", \"restart\", \"rm\""),
    );
    let mut stack = Stack::discover(
        None,
        Some(&project),
        None,
        Arc::new(ScriptedLoader {
            seen: seen.clone(),
            answer: Some(view),
        }),
    );
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "{failed:?}");

    let mut a = a_head_with_a_queue();
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));
    // Draw, so the pane holds the view the keys read.
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("agent/mine"), "{screen}");

    // **The cursor is on the first row, whose act names `one`.**
    a.queue_sel = 0;
    assert_eq!(
        a.key(Key::Char('a')),
        Some(Action::Slash {
            line: "queue approve one".into()
        }),
        "the row's own entry, not the host's cursor's"
    );

    // **And on the second row it is the OTHER entry** — the same act, the row's id. `v` because
    // that row is a `queued` landing, and the daemon refuses `approve` on one of those (which is
    // the same rule `verb_moves` keeps for the host pane: a merge row is approved already).
    a.queue_sel = 1;
    assert_eq!(
        a.key(Key::Char('v')),
        Some(Action::Slash {
            line: "queue veto m-three".into()
        }),
    );

    // **The feed was what the layer read**: one feed, named `queue`, with the snapshot's rows in it.
    let inputs = seen.take();
    let first = inputs.first().expect("the layer was asked");
    let feed = &first["feeds"]["queue"];
    assert_eq!(feed["reviews"][0]["id"], "one", "{feed}");
    assert_eq!(
        feed["reviews"][0]["state"], "reviewing",
        "the daemon's own word"
    );
    assert_eq!(feed["reviews"][0]["live"], true);
    assert_eq!(
        feed["merges"][0]["waits_on"],
        Value::Null,
        "a `queued` landing with no gate in flight waits on nothing"
    );
    std::fs::remove_dir_all(&project).ok();
}

/// **A row asking for a verb its manifest does not declare is refused, and the host pane is drawn**
/// — the allowlist a reviewer reads is the list of words that can be sent.
#[test]
fn a_layer_that_uses_an_undeclared_verb_falls_back() {
    let seen = Seen::default();
    let view = json!({
        "title": "a queue",
        "panels": [{"id": "rows", "title": "live", "summary": [{"tree": {"rows": [
            {"id": "one", "cells": ["agent/one"],
             "enter": {"verb": "rm", "id": "one"}},
        ]}}]}],
    })
    .to_string();
    // The manifest declares `approve` and nothing else.
    let project = a_project("undeclared", &a_manifest("\"approve\""));
    let mut stack = Stack::discover(
        None,
        Some(&project),
        None,
        Arc::new(ScriptedLoader {
            seen: seen.clone(),
            answer: Some(view),
        }),
    );
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "{failed:?}");

    let mut a = a_head_with_a_queue();
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("review queue · merge queue"),
        "the refused view falls back to the host's own pane:\n{screen}"
    );
    assert!(
        !screen.contains("a queue"),
        "and the refused view is not drawn:\n{screen}"
    );
    std::fs::remove_dir_all(&project).ok();
}

/// **A snapshot that has not arrived is nobody's to draw** — the plugin is not asked, because an
/// empty `reviews` would be a queue it drew as empty, which is the one thing the pane never does.
#[test]
fn a_queue_the_head_has_not_been_told_about_is_not_drawn_by_a_layer() {
    let seen = Seen::default();
    let project = a_project("empty", &a_manifest("\"approve\""));
    let mut stack = Stack::discover(
        None,
        Some(&project),
        None,
        Arc::new(ScriptedLoader {
            seen: seen.clone(),
            answer: Some("{\"title\":\"a queue\",\"panels\":[]}".to_string()),
        }),
    );
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "{failed:?}");

    let mut a = app();
    a.session_id = "s1".into();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack,
        a.workspace_path(),
    ));
    a.command("queue");
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("asking the daemon"),
        "the head says it is asking, not that the queue is empty:\n{screen}"
    );
    assert!(
        seen.take().is_empty(),
        "and the layer was not asked at all — there is no snapshot to hand it"
    );
    std::fs::remove_dir_all(&project).ok();
}

/// **`/queue builtin` is the way back to the head's own pane** — the escape the conversion owes, so
/// a person whose layer is broken (or unwanted) still has a queue pane.
#[test]
fn the_builtin_escape_draws_the_hosts_own_pane() {
    let mut a = a_head_with_a_queue();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
        "s1",
        &std::env::temp_dir(),
    ));
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("the queue"), "the layer's pane: {screen}");

    a.command("queue builtin");
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("review queue · merge queue"),
        "the head's own pane, by name:\n{screen}"
    );
    assert!(a.queue_builtin);

    a.command("queue plugin");
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("the queue"),
        "and the layer's again:\n{screen}"
    );
    assert!(!a.queue_builtin);
}

/// **Enter on the history row opens the fold, and the next frame draws it** — the key the row says,
/// not the map written by hand.
///
/// The first cut of this test asserted the fold's RENDERING by inserting `finished: true` itself, so
/// it proved that a view with the fold open draws the history and nothing about the key that is
/// supposed to open it — while `App::queue_folds` was written by no production path at all, and
/// every finished row (the `failed` landings the operator asked to restart, the `rejected` reviews)
/// was unreachable behind a row saying `enter opens it`.
#[test]
fn enter_on_the_history_row_opens_the_fold_and_the_next_frame_draws_it() {
    let mut a = a_head_with_a_queue();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
        "s1",
        &std::env::temp_dir(),
    ));
    let screen = a.screen(120, 30).join("\n");
    assert!(
        !screen.contains("agent/two"),
        "folded to begin with: {screen}"
    );
    assert!(
        screen.contains("enter opens it"),
        "and the row says which key opens it: {screen}"
    );
    // Walk the cursor to the history row: the live rows, then it.
    let stops = a.queue_plugin_stops;
    assert!(
        stops >= 3,
        "the pane has the live rows and the fold: {stops}"
    );
    a.queue_sel = stops - 1;

    // **Enter** — the key, not the map.
    a.key(Key::Enter);
    assert_eq!(
        a.queue_folds.get("finished"),
        Some(&true),
        "Enter on the row that folds opens the fold"
    );
    assert!(
        a.queue_open.is_none(),
        "and does not open an entry overlay for a row that is not an entry"
    );
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("agent/two"),
        "the finished row is drawn once the fold is open:\n{screen}"
    );
    assert!(
        screen.contains("enter folds it away"),
        "and the row now says how to close it:\n{screen}"
    );

    // **And Enter again closes it** — the cursor is back on the fold row, which is the last stop.
    a.queue_sel = a.queue_plugin_stops - 1;
    a.key(Key::Enter);
    assert_eq!(a.queue_folds.get("finished"), Some(&false));
    let screen = a.screen(120, 30).join("\n");
    assert!(!screen.contains("agent/two"), "closed again: {screen}");
}

/// **The bar's Enter names the fold when the cursor is on it** — the row and the bar cannot
/// disagree about what the key does, because both read the same act.
#[test]
fn the_bars_enter_names_the_fold_when_the_cursor_is_on_it() {
    let mut a = a_head_with_a_queue();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
        "s1",
        &std::env::temp_dir(),
    ));
    a.screen(120, 30);
    a.queue_sel = 0;
    let bar = a.hint_bar(1000);
    assert!(
        bar.contains("opens the row: the ask, the verdict"),
        "over a live row Enter opens the entry: {bar}"
    );
    a.queue_sel = a.queue_plugin_stops - 1;
    let bar = a.hint_bar(1000);
    assert!(
        bar.contains("opens the history"),
        "over the folded history it opens it: {bar}"
    );
    // **And once it is open, the bar says the other direction** — a bar that went on offering
    // `opens` over an open fold names a key whose press does the opposite.
    a.key(Key::Enter);
    a.screen(120, 30);
    a.queue_sel = a.queue_plugin_stops - 1;
    let bar = a.hint_bar(1000);
    assert!(bar.contains("closes the history"), "{bar}");
}

/// **Enter on a LIVE row still opens the entry**, so the fold's key did not swallow the pane's own —
/// the two meanings of Enter, side by side.
#[test]
fn enter_on_a_live_row_opens_the_entry_and_not_a_fold() {
    let mut a = a_head_with_a_queue();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
        "s1",
        &std::env::temp_dir(),
    ));
    a.screen(120, 30);
    a.queue_sel = 0;
    a.key(Key::Enter);
    assert_eq!(a.queue_open.as_deref(), Some("one"), "the row's own entry");
    assert!(
        a.queue_folds.is_empty(),
        "and no fold was touched: {:?}",
        a.queue_folds
    );
}

/// **A closed fold takes its rows with it and the cursor comes back to the fold row** — a `queue_sel`
/// left past the end of the shortened list is a pane drawn with no cursor on it.
#[test]
fn closing_the_fold_keeps_the_cursor_on_a_row_that_is_there() {
    let mut a = a_head_with_a_queue();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
        "s1",
        &std::env::temp_dir(),
    ));
    a.queue_folds.insert("finished".into(), true);
    a.screen(120, 30);
    // The cursor is deep in the history — the last row of the open list.
    let open = a.queue_plugin_stops;
    a.queue_sel = open - 1;
    a.queue_folds.insert("finished".into(), false);
    let rows = a.screen(120, 30);
    assert!(
        a.queue_plugin_stops < open,
        "the premise: closing the fold took rows with it ({} stops, then {})",
        open,
        a.queue_plugin_stops
    );
    assert_eq!(
        a.queue_sel,
        a.queue_plugin_stops - 1,
        "the cursor was clamped back onto the fold row, which is the last stop"
    );
    assert!(
        rows.iter().any(|l| l.contains('▸')),
        "and the frame has a cursor on it:\n{}",
        rows.join("\n")
    );
}

/// **A finished row's verb acts on IT once the fold is open** — the operator's own need: *"so merge
/// queue has 4 failed items, we need a way to restart them"*. The four failed items are finished
/// rows, so before the fold could be opened there was no key that could reach one.
#[test]
fn a_finished_rows_verb_acts_on_the_row_the_fold_revealed() {
    let mut a = a_head_with_a_queue();
    a.apply(queue_frame(queues(
        vec![queue_review_row("one", "reviewing")],
        vec![
            queue_merge_row("m-three", "queued"),
            queue_merge_row("m-bad", "failed"),
        ],
    )));
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
        "s1",
        &std::env::temp_dir(),
    ));
    a.queue_folds.insert("finished".into(), true);
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("agent/bad"),
        "the failed landing is on the frame:\n{screen}"
    );
    // The stops are the live review, the live landing, the failed one, then the fold row.
    a.queue_sel = 2;
    assert_eq!(
        a.key(Key::Char('r')),
        Some(Action::Slash {
            line: "queue restart m-bad".into()
        }),
        "`r` restarts the row the fold revealed"
    );
    assert_eq!(a.key(Key::Enter), None);
    assert_eq!(
        a.queue_open.as_deref(),
        Some("m-bad"),
        "and Enter opens that row whole"
    );
}

/// **`/queue builtin` changes WHO draws the pane and nothing about WHICH queues** — the scope the
/// person set survives the escape and the way back.
#[test]
fn the_builtin_escape_keeps_the_scope_the_person_set() {
    let mut a = a_head_with_a_queue();
    // **This session is in one repository**, so the Here scope says so and `/queue all` says
    // something else — without that difference the test could not tell the scopes apart.
    a.queues_all.here = Some(letibot_sessionlog::event::QueueHere {
        workspace: "/home/dead/Projects/letibot/letibot".into(),
        repository: Some("/home/dead/Projects/letibot/letibot".into()),
        repositories: vec!["/home/dead/Projects/letibot/letibot".into()],
    });
    a.rescope_queues();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
        "s1",
        &std::env::temp_dir(),
    ));
    let screen = a.screen(140, 30).join("\n");
    assert!(
        screen.contains("this session's repository — letibot"),
        "the premise: the pane is on this session's repository:\n{screen}"
    );

    a.command("queue all");
    let screen = a.screen(140, 30).join("\n");
    assert!(screen.contains("every repository"), "{screen}");

    // **The escape, and the way back** — neither touches the scope.
    a.command("queue builtin");
    assert_eq!(
        a.queue_scope,
        QueueScope::All,
        "the scope was reset by the escape"
    );
    let screen = a.screen(140, 30).join("\n");
    assert!(
        screen.contains("every repository ·"),
        "the head's own pane, still on every repository:\n{screen}"
    );
    assert!(
        !screen.contains("— this session's repository"),
        "and not quietly back on this session's:\n{screen}"
    );

    a.command("queue plugin");
    assert_eq!(a.queue_scope, QueueScope::All);
    let screen = a.screen(140, 30).join("\n");
    assert!(screen.contains("every repository"), "{screen}");
}

/// **The layer is asked once a second, not once a frame** — the host's cache is keyed by the input,
/// and the feed's clock is quantised for exactly this.
///
/// Measured through a scripted layer that records every input it is handed: two frames inside one
/// second are ONE call, and the frame after the second ticks is the second.
#[test]
fn the_layer_is_asked_once_a_second_and_not_once_a_frame() {
    let seen = Seen::default();
    let view = json!({
        "title": "a queue",
        "panels": [{"id": "rows", "title": "live", "summary": [{"tree": {"rows": [
            {"id": "one", "cells": ["[~]", "agent/one", "reviewing"],
             "enter": {"verb": "approve", "id": "one"}},
        ]}}]}],
    })
    .to_string();
    let project = a_project("cache", &a_manifest("\"approve\""));
    let mut stack = Stack::discover(
        None,
        Some(&project),
        None,
        Arc::new(ScriptedLoader {
            seen: seen.clone(),
            answer: Some(view),
        }),
    );
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "{failed:?}");

    let mut a = a_head_with_a_queue();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack,
        a.workspace_path(),
    ));
    a.screen(120, 30);
    assert_eq!(seen.take().len(), 1, "the first frame asks the layer");

    // **Half a second later**: the clock has moved, the input has not.
    a.now_ms += 500;
    a.redraw = true;
    a.screen(120, 30);
    assert_eq!(
        seen.take().len(),
        0,
        "a frame inside the same second is a cache hit, not a wasm call"
    );

    // **And the next second is a new question** — an age has moved, and the pane must be redrawn.
    a.now_ms += 500;
    a.redraw = true;
    a.screen(120, 30);
    assert_eq!(seen.take().len(), 1, "a second later is a new call");
    std::fs::remove_dir_all(&project).ok();
}

/// **The live loop, measured**: a `plugin.wasm` rebuilt in the session's own plugin directory is a
/// pane that redraws — no rebuild of `harnessd`, no rebuild of `letibot-tui`, no restart.
///
/// This is the whole reason the pane is a plugin. It is also the one thing a test has to be careful
/// about: the module here is the REAL built-in one (this binary's own `queue` layer, written to the
/// session scope), so what reloads is a module the engine actually compiles and runs — not a fake.
///
/// The two answers differ in a way only the module can produce: the copy in the session scope is
/// the built-in's bytes under a DIFFERENT manifest name, so a pane drawn after the reload is drawn
/// by a layer whose id is the session's, not `b-queue`. The view is the same because the bytes are;
/// what is asserted is that the layer the head is asking CHANGED without anybody restarting it.
#[test]
fn a_rebuilt_layer_in_the_sessions_plugin_directory_redraws_the_pane() {
    // **A session-scope `queue` layer, written the way `plugin build` leaves it.**
    let data = std::env::temp_dir().join(format!(
        "letibot-live-loop-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let session = "s-live";
    let layer = data
        .join("letibot/sessions")
        .join(session)
        .join("plugins/queue");
    std::fs::create_dir_all(&layer).expect("the session's plugin dir");
    let builtin = letibot_builtins::table()
        .iter()
        .find(|b| b.name == "queue")
        .expect("this binary carries the queue layer");
    std::fs::write(layer.join("plugin.wasm"), builtin.wasm).expect("the module");
    std::fs::write(layer.join("plugin.toml"), builtin.manifest).expect("the manifest");
    // **`XDG_DATA_HOME` is how `session_plugins_dir` finds it** — set for this test only, and put
    // back at the end, because it is process-wide and a leaked one would point every other test's
    // session scope at a temp directory.
    let was = std::env::var_os("XDG_DATA_HOME");
    // SAFETY: this test is the only one that touches it, and it restores it below.
    unsafe { std::env::set_var("XDG_DATA_HOME", &data) };

    let mut a = a_head_with_a_queue();
    a.session_id = session.into();
    // A head whose stack is built for this session, so the session scope is discovered.
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
        session,
        &std::env::temp_dir(),
    ));
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("agent/one"),
        "the session's layer draws the pane:\n{screen}"
    );
    // **The layer answering is the session's, not the built-in** — a session scope is above the
    // built-in scope, so the id is `s1` and not `b-queue`. Its hash is taken here so the reload
    // below has something to differ from.
    let before: String;
    {
        let slot = a.plugin_host();
        let host = slot.as_ref().expect("a host");
        let layer = host
            .reader(crate::app::queue_pane::HOOK, crate::app::queue_pane::FEED)
            .expect("a queue layer");
        assert_eq!(
            layer.id, "s1",
            "the session scope answers, above the built-in"
        );
        before = layer.sha256.clone();
    }

    // **Rewrite the module — the same bytes, a newer file.** `poll_changes` keys on the fingerprint
    // (mtime and length), so a rewrite that lands in the same second still has to be noticed; the
    // loop's own guarantee is that a REBUILT module is a different file.
    let wasm = layer.join("plugin.wasm");
    let mut bytes = std::fs::read(&wasm).expect("the module");
    // **A custom section is what a rebuild changes that a module can carry and still be valid** —
    // `0x00`, a length, a name and a payload, which every engine ignores. A trailing byte would be
    // a module the loader refuses, and a reload test whose new module does not load measures the
    // refusal rather than the loop.
    bytes.extend_from_slice(&[0x00, 0x04, 0x01, 0x61, 0x00, 0x00]);
    std::fs::write(&wasm, &bytes).expect("the rewritten module");

    // **The clock passes, the pane is drawn again, and the layer is re-read.** Forward, and past
    // the poll's interval — a clock that went backwards would be a head whose tick never fires,
    // which is a thing to test somewhere else and not a thing to test here by accident.
    let later = a.now_ms + 3_000;
    a.clock(later);
    a.redraw = true;
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("agent/one"),
        "the pane still draws after the reload:\n{screen}"
    );
    {
        let slot = a.plugin_host();
        let host = slot.as_ref().expect("a host");
        let layer = host
            .reader(crate::app::queue_pane::HOOK, crate::app::queue_pane::FEED)
            .expect("a queue layer");
        // **The layer is no longer the bytes the built-in carries** — which is what a reload
        // means, and it is asserted without a second hasher: the built-in's hash is read off the
        // layer the binary installs, and the session layer's must differ from it now.
        assert_ne!(
            layer.sha256, before,
            "the layer's bytes are the REBUILT module's, with no restart"
        );
    }

    match was {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
    std::fs::remove_dir_all(&data).ok();
}

/// **The pane, printed** — a probe that writes what the layer draws at the two widths, so the
/// answer can quote a frame rather than describe one. `#[ignore]` because it asserts nothing: the
/// two-width test above is the one that fails a build.
///
/// `cargo test -p letibot-tui --lib -- --ignored --nocapture probe_render_the_pane`.
#[test]
#[ignore]
fn probe_render_the_pane() {
    // **The operator's own shape**: two rows the queue is working on, four it has failed with, and
    // the history folded away — the frame that was *"illegible mess"* before this pane.
    let fixture = || {
        let mut a = a_head_with_a_queue();
        a.apply(queue_frame(queues(
            vec![
                queue_review_row("one", "reviewing"),
                queue_review_row("two", "rejected"),
            ],
            vec![
                queue_merge_row("m-three", "gating"),
                queue_merge_row("m-four", "failed"),
                queue_merge_row("m-five", "timed_out"),
                queue_merge_row("m-six", "conflict"),
                queue_merge_row("m-seven", "failed"),
            ],
        )));
        *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
            "s1",
            &std::env::temp_dir(),
        ));
        a
    };
    for (w, h) in [(80, 24), (239, 61)] {
        for open in [false, true] {
            let mut a = fixture();
            a.cfg.width = w;
            if open {
                a.queue_folds.insert("finished".into(), true);
            }
            let screen = a.screen(w, h);
            eprintln!(
                "===== {w}x{h}, history {} =====",
                if open { "open" } else { "folded" }
            );
            for row in screen {
                eprintln!("|{}", row.trim_end());
            }
        }
    }
}
