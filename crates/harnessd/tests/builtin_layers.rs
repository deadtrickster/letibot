//! **The layers this binary carries** — `docs/plugins-plan.md` §7 step 10: the `builtin` scope is
//! a table of modules compiled into the executable, not a directory beside it.
//!
//! Four things are asserted here, and each one is a way the mechanism can be silently wrong:
//!
//! 1. **The table is not empty** — a build whose script found no layer would otherwise ship a
//!    binary whose bottom layer is nothing, and the failure would look like a head that draws a
//!    plain list.
//! 2. **A built-in loads through the real engine and answers** — `RanoLoader`, `rano_abi`, the
//!    result block: the same path a layer on disk takes, with the same admission.
//! 3. **A project layer of the same name masks it** — the whole point of a built-in being a layer.
//! 4. **A layer that asks below reaches it** — with the built-in at the bottom, `{"below": …}` has
//!    to arrive somewhere, or every project layer that edits the default is editing nothing.
//!
//! The modules are real: the built-in is the one `crates/builtins` compiled at build time, and the
//! masking layer is `tests/data/below-layer/plugin.wasm`, committed for the reason
//! `data/plugin-engine/plugin.wasm` is — a test that shells out to a compiler fails on a box
//! without a wasm target.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use letibot_harnessd::plugins::rano::RanoLoader;
use letibot_plugins::{Manifest, Scope, Stack};
use serde_json::json;

/// A stack over an empty project and user scope, with the binary's built-ins at the bottom.
fn builtin_stack(root: &Path) -> Stack {
    let mut stack = Stack::discover(None, Some(root), None, Arc::new(RanoLoader));
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "every built-in loads: {failed:?}");
    assert!(
        stack.failures().is_empty(),
        "and nothing was recorded against the stack: {:?}",
        stack.failures()
    );
    stack
}

/// The fixture layer, copied into a project's `.letibot/plugins/<name>/` — the project scope.
fn project_layer(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(".letibot").join("plugins").join(name);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/below-layer");
    std::fs::create_dir_all(&dir).expect("the project's plugin dir");
    for file in ["plugin.toml", "plugin.wasm"] {
        std::fs::copy(fixture.join(file), dir.join(file)).expect("the fixture");
    }
    dir
}

/// **What a `pane` hook is asked with** — the feed the built-in declares (`plugin.toml`'s
/// `feeds = ["sessions"]`), and nothing else. A host builds this map from the manifest, so an
/// answer that needs a feed arrives only when the layer asked for it.
fn asked_with() -> serde_json::Value {
    json!({
        "v": 2,
        "size": {"w": 100, "h": 40},
        "feeds": {"sessions": {
            "sessions": [
                {"id": "s-one", "title": "the habr article", "origin": "operator", "live": true,
                 "heads": 1, "items": 6, "model": "qwen", "workspace": "/home/dead/Projects/letibot"},
            ],
            "here": "s-one",
            "home": "/home/dead",
        }},
    })
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "letibot-builtin-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

#[test]
fn the_table_is_not_empty_and_a_builtin_loads_through_the_real_engine() {
    let table = letibot_builtins::table();
    assert!(
        !table.is_empty(),
        "a binary whose built-in scope is empty has no bottom layer at all"
    );
    for b in table {
        // The manifest is the one a reviewer reads, and it parses: a built-in with a broken
        // `plugin.toml` is refused by the same door a project layer's is.
        let manifest = Manifest::parse(b.manifest)
            .unwrap_or_else(|why| panic!("{}: plugin.toml does not parse: {why}", b.name));
        assert_eq!(
            manifest.name, b.name,
            "the directory and the manifest agree"
        );
        assert!(
            b.wasm.starts_with(b"\0asm"),
            "{}: the module is wasm, not a placeholder",
            b.name
        );
    }

    let tmp = tempdir("table");
    let stack = builtin_stack(&tmp);
    let layers: Vec<&str> = stack.layers().iter().map(|l| l.id.as_str()).collect();
    assert_eq!(layers, vec!["b-sessions"], "one layer, at the bottom");

    // **The real engine answered.** A `View` that parses is the proof that the module loaded, that
    // `rano_abi` was 1, and that the result block held a JSON document: everything before this
    // point in the stack's life is bookkeeping, and this is the one call that runs wasm.
    let input = asked_with();
    let r = stack.call("pane_sessions", &input);
    assert!(r.failures.is_empty(), "{:?}", r.failures);
    assert_eq!(r.by.as_deref(), Some("b-sessions"));
    let answer = r.answer.expect("the built-in answers");
    let view: letibot_sessionlog::pane::View =
        serde_json::from_value(answer).expect("the answer is a C3 view");
    view.validate().expect("and it is one the host can draw");
    assert!(
        view.title.contains("sessions"),
        "the pane says what it is: {view:?}"
    );
    // The layer is the operator's by definition (DECISIONS.md D17), which the package that builds
    // trust reads and nothing here depends on.
    assert!(stack.layer("b-sessions").expect("the layer").trusted);
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn a_project_layer_of_the_same_name_masks_the_builtin() {
    let tmp = tempdir("mask");
    project_layer(&tmp, "sessions");
    let stack = builtin_stack(&tmp);
    // Newest on top: the project's layer, then the binary's.
    let layers: Vec<&str> = stack.layers().iter().map(|l| l.id.as_str()).collect();
    assert_eq!(layers, vec!["p1", "b-sessions"]);

    let r = stack.call("pane_sessions", &asked_with());
    assert_eq!(
        r.by.as_deref(),
        Some("p1"),
        "the project's `sessions` answers, not the built-in's"
    );
    assert_eq!(
        stack.layer("p1").expect("the project layer").scope,
        Scope::Project
    );
    // And a layer from disk is not trusted until D17's package says so.
    assert!(!stack.layer("p1").expect("the project layer").trusted);
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn a_layer_that_asks_below_reaches_the_builtin() {
    let tmp = tempdir("below");
    project_layer(&tmp, "sessions");
    let stack = builtin_stack(&tmp);
    let r = stack.call("pane_sessions", &asked_with());
    assert!(r.failures.is_empty(), "{:?}", r.failures);
    let answer = r.answer.expect("an answer");
    // The fixture answers *below found the sessions built-in* only when the built-in's own title
    // was in what the stack handed it back — which is the built-in's answer, one layer down.
    assert_eq!(
        answer["title"], "below found the sessions built-in",
        "the layer below it answered, and what it answered reached this one: {answer}"
    );
    assert_eq!(r.by.as_deref(), Some("p1"));
    std::fs::remove_dir_all(&tmp).ok();
}
