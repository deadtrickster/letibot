//! The layers this binary carries - `docs/plugins-plan.md`: the `builtin` scope
//! is a table of modules compiled into the executable, not a directory beside it. Four
//! things are asserted, each a way the mechanism can be silently wrong.

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

/// The fixture layer, copied into a project's `.letibot/plugins/<name>/` - the project scope.
fn project_layer(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(".letibot").join("plugins").join(name);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/below-layer");
    std::fs::create_dir_all(&dir).expect("the project's plugin dir");
    for file in ["plugin.toml", "plugin.wasm"] {
        std::fs::copy(fixture.join(file), dir.join(file)).expect("the fixture");
    }
    dir
}

/// What a `pane` hook is asked with - the feed the built-in declares (`plugin.toml`'s
/// `feeds = ["sessions"]`) and nothing else. A host builds this map from the manifest, so an answer
/// that needs a feed arrives only when the layer asked for it.
fn asked_with() -> serde_json::Value {
    json!({
        "v": letibot_plugin::pane::V,
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
    let dir = letibot_sessionlog::testing::scratch(&format!("letibot-builtin-{tag}"));
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
    // The expectation is READ OFF THE TABLE, not written down here: a second list of the
    // binary's own layers drifts the moment a built-in is added, and it did, twice in one day.
    // REVERSED is the behaviour and not the intent
    let mut built_in: Vec<String> = letibot_builtins::table()
        .iter()
        .map(|b| format!("b-{}", b.name))
        .collect();
    built_in.reverse();
    assert_eq!(layers, built_in, "the binary's own layers, at the bottom");

    // The real engine answered: a `View` that parses is proof the module loaded, that `rano_abi` was
    // 1, and that the result block held a JSON document. This is the one call that runs wasm.
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
    // The layer is the operator's by definition, which the package that builds
    // trust reads and nothing here depends on.
    assert!(stack.layer("b-sessions").expect("the layer").trusted);
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn a_project_layer_of_the_same_name_masks_the_builtin() {
    let tmp = tempdir("mask");
    project_layer(&tmp, "sessions");
    let stack = builtin_stack(&tmp);
    // Newest on top: the project's layer, then every one of the binary's - reversed for the reason
    // the table assertion above spells out (`register_builtin_bytes` stacks newest-first).
    let layers: Vec<&str> = stack.layers().iter().map(|l| l.id.as_str()).collect();
    let mut want = vec![String::from("p1")];
    let mut built_in: Vec<String> = letibot_builtins::table()
        .iter()
        .map(|b| format!("b-{}", b.name))
        .collect();
    built_in.reverse();
    want.extend(built_in);
    assert_eq!(layers, want);

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
    // And a layer from disk is not trusted until the package says so.
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
    // was in what the stack handed it back - which is the built-in's answer, one layer down.
    assert_eq!(
        answer["title"], "below found the sessions built-in",
        "the layer below it answered, and what it answered reached this one: {answer}"
    );
    assert_eq!(r.by.as_deref(), Some("p1"));
    std::fs::remove_dir_all(&tmp).ok();
}

/// What one compaction hands the `compact_` layer: the harness's own sections, and a candidate for
/// each kind the host can pin. A record's size is the host's, so the numbers are a small one.
fn compaction_asked_with() -> serde_json::Value {
    json!({
        "v": 1,
        "sections": letibot_turn::Template::headings(),
        "candidates": [
            {"id": "row:1", "kind": "row", "tokens": 12, "score": 0.9},
            {"id": "agents_md", "kind": "agents_md", "tokens": 900, "score": 1.0},
            {"id": "row:2", "kind": "row", "tokens": 30, "score": 0.9},
        ],
        "budget": {"summary_tokens": 8_000, "pinned_tokens": 2_000},
    })
}

#[test]
fn agents_md_is_pinned_by_the_builtin_compact_layer() {
    let tmp = tempdir("compact-pin");
    let stack = builtin_stack(&tmp);
    let input = compaction_asked_with();
    let r = stack.call(letibot_plugin::compact::HOOK, &input);
    assert!(r.failures.is_empty(), "{:?}", r.failures);
    assert_eq!(r.by.as_deref(), Some("b-compact"));
    let answer = letibot_plugin::compact::Answer::parse(r.answer.expect("the built-in answers"))
        .expect("a compaction answer");
    let asked: letibot_plugin::compact::Input = serde_json::from_value(input).expect("the input");
    answer.validate(&asked).expect("what the host offered");
    // The project's own file first, then the rows: the order is the pin budget's order, so the
    // last thing the record loses is the operator's rules.
    assert_eq!(answer.pin.first().map(String::as_str), Some("agents_md"));
    assert_eq!(
        answer.pin.iter().map(String::as_str).collect::<Vec<_>>(),
        ["agents_md", "row:1", "row:2"],
        "{answer:?}"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn the_builtin_compact_layer_asks_what_summary_instruction_asked() {
    let tmp = tempdir("compact-sections");
    let stack = builtin_stack(&tmp);
    let input = compaction_asked_with();
    let r = stack.call(letibot_plugin::compact::HOOK, &input);
    assert!(r.failures.is_empty(), "{:?}", r.failures);
    let answer = letibot_plugin::compact::Answer::parse(r.answer.expect("the built-in answers"))
        .expect("a compaction answer");
    let asked: letibot_plugin::compact::Input = serde_json::from_value(input).expect("the input");
    // The layer's list and the harness's own are the same eight, in the same order - a heading the
    // layer invented would be refused here, and one it dropped would be a record that lost a section.
    assert_eq!(
        answer.kept_sections(&asked),
        letibot_turn::SUMMARY_SECTIONS,
        "the built-in asks for the harness's own sections"
    );
    // And the record written on its answer is the instruction `summary_instruction` writes, byte for
    // byte: it adds no sentence of its own, and pins are the host's to carry.
    let budget = letibot_turn::summary_budget(262_144, 6_880, 16_384, 200_000);
    let template = letibot_turn::Template {
        sections: answer.kept_sections(&asked).to_vec(),
        extra: answer.extra_instruction.clone(),
        pinned: Vec::new(),
    };
    assert_eq!(
        letibot_turn::instruction_with(&budget, &template),
        letibot_turn::summary_instruction(&budget)
    );
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn the_builtin_steer_layer_says_what_the_harness_decided() {
    let tmp = tempdir("steer");
    let stack = builtin_stack(&tmp);
    let mut input = serde_json::to_value(letibot_plugin::steer::Input::synthetic())
        .expect("the SDK's own example");
    let r = stack.call(letibot_plugin::steer::HOOK, &input);
    assert!(r.failures.is_empty(), "{:?}", r.failures);
    assert_eq!(r.by.as_deref(), Some("b-steer"));
    let answer = letibot_plugin::steer::Answer::parse(r.answer.expect("the built-in answers"))
        .expect("a turn-end answer");
    // The sentence the harness would have appended, said back: the layer is the decision it was
    // asked to replace.
    let native = input["native"].as_str().map(str::to_string);
    assert_eq!(
        answer,
        letibot_plugin::steer::Answer::Continue(native.expect("a native sentence"))
    );
    // And a turn the harness decided to end is a turn the layer ends.
    input.as_object_mut().expect("an object").remove("native");
    let r = stack.call(letibot_plugin::steer::HOOK, &input);
    assert!(r.failures.is_empty(), "{:?}", r.failures);
    assert_eq!(
        letibot_plugin::steer::Answer::parse(r.answer.expect("the built-in answers"))
            .expect("an answer"),
        letibot_plugin::steer::Answer::Stop
    );
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn the_builtin_deciders_are_on_the_daemons_stack() {
    // The third door: a built-in whose hook is neither a pane nor a tool was installed at no door
    // at all, so *the harness's own decision is a layer* was true of nothing.
    let tmp = tempdir("deciders");
    let host = letibot_harnessd::plugins::PluginHost::production("s-deciders", &tmp);
    let doc = host.stack_doc();
    for hook in ["steer_turn", "compact_summary"] {
        assert!(doc.contains(hook), "`{hook}` is on nobody's stack:\n{doc}");
    }
    for id in ["b-steer", "b-compact"] {
        assert!(
            doc.contains(id),
            "`{id}` is not on the daemon's stack:\n{doc}"
        );
    }
    std::fs::remove_dir_all(&tmp).ok();
}
