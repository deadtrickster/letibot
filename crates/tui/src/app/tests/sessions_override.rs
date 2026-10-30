//! **An override, proved end to end** — `docs/plugins-plan.md` §7 step 10's *"then overriding the
//! list is one `sessions` layer in a project or a session, and a small one, since it can call below
//! and edit the built-in's tree"*.
//!
//! The layer is `plugins/examples/sessions-override`: a real wasm module, built and committed, that
//! asks for the layer below and rewrites two lines of what it finds. This file copies it into a
//! project's `.letibot/plugins/sessions/`, builds the head's stack over that project, and asserts
//! on **what is drawn** — not on the JSON: the question is whether the person looking at the list
//! sees the project's list.
//!
//! Nothing is faked: `RanoLoader` compiles both modules, the built-in `sessions` layer is the one
//! this binary carries, and the stack does the ask-below itself.

use super::*;
use letibot_plugins::Stack;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The example, as a project's layer: `plugin.toml` and the committed `plugin.wasm`.
fn the_project_with_the_override() -> PathBuf {
    let project = std::env::temp_dir().join(format!(
        "letibot-override-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let dir = project.join(".letibot").join("plugins").join("sessions");
    std::fs::create_dir_all(&dir).expect("the project's plugin dir");
    let example =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/examples/sessions-override");
    for file in ["plugin.toml", "plugin.wasm"] {
        std::fs::copy(example.join(file), dir.join(file))
            .unwrap_or_else(|e| panic!("{}: {e}", example.join(file).display()));
    }
    project
}

/// A daemon with a conversation of the operator's and a one-shot's leftovers — the two kinds the
/// override treats differently.
fn a_head_with_a_one_shot() -> App {
    let mut a = app();
    let mut shot = brief("s-shot", "one prompt, one answer", false);
    shot.origin = letibot_sessionlog::SessionOrigin::OneShot;
    let mut package = brief("s-pkg", "make me a landing page", false);
    package.origin = letibot_sessionlog::SessionOrigin::Package;
    a.apply(hello(
        "s-habr",
        vec![brief("s-habr", "the habr article", false), shot, package],
        Hub::new("s-habr").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    a
}

/// **The project's layer answers, above the built-in, and the drawing is the project's** — the
/// group is renamed, its rows are gone, and every other row is the built-in's.
#[test]
fn a_projects_sessions_layer_renames_a_group_and_hides_its_rows() {
    let project = the_project_with_the_override();
    let mut stack = Stack::discover(
        None,
        Some(&project),
        None,
        Arc::new(crate::app::sessions_pane::RanoLoader),
    );
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "{failed:?}");

    // The project's layer is on top, the built-ins below it — and **the built-ins are in the order
    // `crates/builtins` installed them, newest on top**, which is the stack's own rule for a scope
    // (`docs/plugins-plan.md` §2.1) applied to a table the binary carries: the table is sorted by
    // name and the stack reverses it, so `b-queue` stands below `b-sessions`, which stands below
    // `b-todo` — and `b-irc` (the nick list, whose hook is `pane_nicks`) is the newest of them and
    // stands at the bottom of the reversed table. **Nothing answers two hooks**, so nothing depends
    // on that order — a test that pins it is how the next layer to share a hook finds out it has to
    // think about it.
    //
    // **Read off the table, not written down**: a second hand-kept list drifts the moment another
    // package adds a built-in, and REVERSED because `register_builtin_bytes` stacks the table
    // newest-first. The head's stack installs the whole table (`letibot_builtins::install`), tools
    // included: a pane is the head's to draw, and a tool is a row in a list the head may show, so a
    // built-in that declares one is a layer here whether or not any session seats it.
    let mut want = vec![String::from("p1")];
    let mut built_in: Vec<String> = letibot_builtins::table()
        .iter()
        .map(|b| format!("b-{}", b.name))
        .collect();
    built_in.reverse();
    want.extend(built_in);
    assert_eq!(
        stack
            .layers()
            .iter()
            .map(|l| l.id.as_str())
            .collect::<Vec<_>>(),
        want
    );

    let mut a = a_head_with_a_one_shot();
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));

    let drawn = a.picker_lines(100);
    let text = drawn.join("\n");
    assert!(
        text.contains("leftovers (0)"),
        "the project's name for the group:\n{text}"
    );
    assert!(
        text.contains("this project keeps no such rows on its list"),
        "and its own legend:\n{text}"
    );
    assert!(
        !text.contains("one prompt, one answer"),
        "the one-shot the project hid is not drawn:\n{text}"
    );
    // **Everything else is the built-in's**: the conversation, the other group, and the numbers.
    assert!(text.contains("the habr article"), "{text}");
    assert!(text.contains("packages (1)"), "{text}");

    // **And it still works as a list**: the numbers are the built-in's numbering less the hidden
    // row, and Enter takes the session a number names.
    // The package's own group opens like any other, and the row under it is still the built-in's.
    a.set_row_fold("packages", true);
    let drawn = a.picker_lines(100).join("\n");
    assert!(
        drawn.contains("make me a landing page"),
        "the package's row is drawn once its group is open:\n{drawn}"
    );
    // **The cursor is a row, not a line**: the index `picker_sel` counts is the drawn LIST's
    // stops, so it is found in that list rather than in the screen that draws it.
    a.picker_sel = a
        .picker_rows()
        .iter()
        .position(|r| r.id == "s-pkg")
        .expect("the package's row");
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-pkg".into())));

    // **And the hidden row is out of the numbering too**: a number is a place in the list that was
    // drawn, so the package is `2` here — where the built-in would have numbered a one-shot `2`
    // and the package `3`.
    assert_eq!(
        a.pick("2"),
        Some(Action::Switch("s-pkg".into())),
        "the second session drawn is the package's conversation"
    );
    assert_eq!(a.pick("3"), None, "and there is no third");
    std::fs::remove_dir_all(&project).ok();
}

/// **A project layer that names a verb it did not declare is refused, and the head's own list is
/// drawn** — the manifest is what a reviewer reads, and a view that reaches past it is not drawn.
#[test]
fn a_layer_that_uses_an_undeclared_act_falls_back() {
    let project = the_project_with_the_override();
    // The same module, with the acts taken out of its manifest: every row it answers carries
    // `switch`, and the layer no longer declares it.
    let dir = project.join(".letibot").join("plugins").join("sessions");
    let manifest = std::fs::read_to_string(dir.join("plugin.toml")).expect("a manifest");
    std::fs::write(
        dir.join("plugin.toml"),
        manifest.replace("actions = [\"switch\", \"fold\"]", "actions = []"),
    )
    .expect("a manifest without its acts");
    let mut stack = Stack::discover(
        None,
        Some(&project),
        None,
        Arc::new(crate::app::sessions_pane::RanoLoader),
    );
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "{failed:?}");

    let mut a = a_head_with_a_one_shot();
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));
    let text = a.picker_lines(100).join("\n");
    assert!(
        text.contains("one-shots (1)"),
        "the layer's view is refused, so the head's own list is drawn:\n{text}"
    );
    assert!(
        !text.contains("leftovers (0)"),
        "and the refused view is not what a person sees:\n{text}"
    );
    std::fs::remove_dir_all(&project).ok();
}
