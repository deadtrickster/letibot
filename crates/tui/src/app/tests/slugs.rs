//! **Subagents named by their slugs** — the operator: *"lets do slugs for subagents too"*. Every
//! place the head named a child by its session id (or by nothing but its brief) names it by the
//! slug the daemon gave it, and a child nobody named is drawn exactly as before.

use super::*;

/// A spawn the head saw, with the name the daemon published on it.
fn named_spawn(id: &str, slug: &str, task: &str) -> SessionEvent {
    SessionEvent::Subagent {
        subagent_id: id.into(),
        state: "running".into(),
        prompt: task.into(),
        role: "coder".into(),
        task: task.into(),
        model: String::new(),
        answer: None,
        slug: slug.into(),
    }
}

/// **The pane names each child by its slug and short id**; a child with no name keeps its short
/// id alone, as every pane before names drew it.
#[test]
fn the_subagents_pane_names_a_child_by_its_slug() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        named_spawn(
            "s-1789919514688401228-sub-3",
            "fix-login",
            "fix the login page",
        ),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        named_spawn("s-1789919514688401228-sub-4", "", "an older daemon's child"),
    )));
    a.key(Key::CtrlG);
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("fix-login · …28-sub-3"), "{screen}");
    assert!(screen.contains("…28-sub-4 · role coder"), "{screen}");
    assert!(
        !screen.contains(" · …28-sub-4"),
        "no name was invented: {screen}"
    );
}

/// **A child rebuilt from the daemon's list keeps its name** — the list carries the slug after a
/// restart, when no spawn event is left to fold — and the header of a head standing in the child
/// names it, and its parent, by their slugs.
#[test]
fn the_list_carries_the_name_into_the_pane_and_the_header() {
    let mut family = a_family();
    family[1].slug = "find-the-bug".into();
    let mut a = app();
    a.apply(hello("s", family.clone(), Hub::new("s").snapshot()));
    let row = a
        .subagents
        .iter()
        .find(|s| s.session_id == "s-sub-1")
        .expect("rebuilt from the list");
    assert_eq!(row.slug, "find-the-bug");
    assert_eq!(row.name(), "find-the-bug · s-sub-1");

    // Inside the child: the header names it by its slug, not its 256-character brief.
    a.apply(hello("s-sub-1", family, Hub::new("s-sub-1").snapshot()));
    let header = a.header_line(120);
    assert!(header.contains("find-the-bug"), "{header}");
    assert!(header.contains("subagent of parent"), "{header}");
    assert!(
        !header.contains("find the bug in the reader"),
        "the brief is not the name: {header}"
    );
}

/// **A child's completion notice reads by its name**, whether the daemon wrote the label
/// (`fix-login · s-…`, since names) or only the handle (every stored notice before them) — and an
/// old notice of a child nobody named folds exactly as it always did.
#[test]
fn a_folded_task_notice_names_the_child_by_its_slug_old_and_new() {
    use crate::ui::transcript::blocks::folded_notice;
    let new = "[task] a subagent you started has finished:\n  \
               - `fix-login · s-1789919514688401228-sub-3` done: all green\n\
               Full answer: `task_result` task=\"s-1789919514688401228-sub-3\".";
    assert_eq!(
        folded_notice(new, &[]).expect("it folds"),
        "Agent fix-login · …28-sub-3 · done: all green"
    );
    // The same child, known to the pane: its task joins the line, found by the id inside the
    // label.
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        named_spawn(
            "s-1789919514688401228-sub-3",
            "fix-login",
            "fix the login page",
        ),
    )));
    assert_eq!(
        folded_notice(new, &a.subagents).expect("it folds"),
        "Agent fix-login · …28-sub-3 · fix the login page · done: all green"
    );
    // An old notice — the bare handle — of a child the pane knows by name: named all the same.
    let old = "[task] a subagent you started has finished:\n  \
               - `s-1789919514688401228-sub-3` done: all green\n\
               Full answer: `task_result` task=\"s-1789919514688401228-sub-3\".";
    assert_eq!(
        folded_notice(old, &a.subagents).expect("it folds"),
        "Agent fix-login · …28-sub-3 · fix the login page · done: all green"
    );
    // And an old notice of a child nobody named: byte for byte what it always was.
    assert_eq!(
        folded_notice(old, &[]).expect("it folds"),
        "Agent s-1789919514688401228-sub-3 · done: all green"
    );
}

/// **A wait on a child draws its name**, the handle first as a job's does (`j97 · pg-timing`).
#[test]
fn a_wait_on_a_named_child_draws_its_slug() {
    use crate::app::todos::WaitOn;
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        named_spawn(
            "s-1789919514688401228-sub-3",
            "fix-login",
            "fix the login page",
        ),
    )));
    assert_eq!(
        a.wait_label(&WaitOn::Child("s-1789919514688401228-sub-3".into())),
        "…28-sub-3 · fix-login"
    );
    // The attention line names it the same way the pane does.
    assert_eq!(
        a.child_label("s-1789919514688401228-sub-3"),
        "fix-login · …28-sub-3"
    );
    // A child the head knows nothing about: its short id.
    assert_eq!(a.child_label("s-1789919514688401228-sub-9"), "…28-sub-9");
}
