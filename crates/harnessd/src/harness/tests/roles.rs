// A role profile at the two seams that decide what a child IS: the tool table `role_for` gives it,
// and message zero `compose_for_seat` builds. Both are pure - no backend, no model, no store - so a
// box without bwrap or a vocabulary proves them. `crates/tools/src/profiles.rs` proves the parser,
// `crates/harnessd/src/profiles.rs` the resolution, and this file the two things a spawn leans on.

use letibot_tools::Profiles;

/// The profile called `name`, as this build ships it.
fn profile(name: &str) -> letibot_tools::Profile {
    crate::profiles::resolve(&Profiles::builtins(), name).expect("a built-in profile")
}

/// A CHILD's config, as `children.rs` builds one: the profile's seat, its overlay, and the parent's
/// own tool table as a ceiling.
fn a_child(name: &str, ceiling: Vec<String>) -> crate::config::Config {
    let mut cfg = crate::config::Config::for_this_box(std::env::temp_dir());
    cfg.session_id = format!("s-child-{name}");
    cfg.parent_session_id = Some("s-parent".into());
    cfg.seat = crate::config::Seat::parse(profile(name).seat()).expect("a seat");
    // `bash` is a daemon-wide flag and the seat table lists it behind it: the profile's own list
    // names it, so the fixture has to have the daemon that seats it.
    cfg.allow_bash = true;
    cfg.seat_overlay = crate::profiles::overlay(Some(profile(name)), Some(ceiling));
    cfg
}

/// The Done-when's tools half: a child seated on `explore` gets what its file lists and nothing
/// else - not the coder's `write`, `edit` or `task`, whatever its seat's table holds.
#[test]
fn a_role_narrows_the_tools_its_child_is_seated_with() {
    // A coder parent holds everything the explorer asks for, so this is the profile's own list.
    let parent = letibot_tools::roles::m2_coder().tools;
    let role = super::super::open::role_for(&a_child("explore", parent));
    let has = |t: &str| role.tools.iter().any(|s| s == t);

    for want in ["read", "grep", "glob", "bash", "read_spill", "recall"] {
        assert!(
            has(want),
            "`{want}` is in the profile's list: {:?}",
            role.tools
        );
    }
    for gone in [
        "write",
        "edit",
        "notes",
        "task",
        "task_start",
        "todo",
        "plugin",
    ] {
        assert!(
            !has(gone),
            "`{gone}` is not in the profile's list and must not be seated: {:?}",
            role.tools
        );
    }
    // The harness's own doors are not the role's to take: a session's verbs on itself are added
    // after the narrowing, on every seat.
    assert_eq!(
        role.tools.len(),
        role.max_tools,
        "a narrowed role's ceiling falls with it: {:?}",
        role.tools
    );
}

/// And a role cannot WIDEN: the same profile under a parent that holds only two of its six tools
/// seats two. This is the whole of *a child never gets a tool its parent lacks*.
#[test]
fn a_role_cannot_hand_over_a_tool_its_parent_lacks() {
    let parent = vec!["read".to_string(), "grep".to_string()];
    let role = super::super::open::role_for(&a_child("explore", parent));
    for gone in ["bash", "glob", "read_spill", "recall", "write"] {
        assert!(
            !role.tools.iter().any(|s| s == gone),
            "`{gone}` is not the parent's: {:?}",
            role.tools
        );
    }
    for want in ["read", "grep"] {
        assert!(role.tools.iter().any(|s| s == want), "{:?}", role.tools);
    }
}

/// A session with no overlay - every session that existed before profiles - keeps its seat's whole
/// table. The narrowing is a thing the ROLE does, and no role means no narrowing.
#[test]
fn a_session_with_no_role_keeps_its_seat() {
    let mut cfg = crate::config::Config::for_this_box(std::env::temp_dir());
    cfg.allow_bash = true;
    cfg.seat = crate::config::Seat::Coder;
    let plain = super::super::open::role_for(&cfg);
    let seat = letibot_tools::roles::m2_coder().tools;
    for want in &seat {
        assert!(
            plain.tools.iter().any(|s| s == want),
            "`{want}` is in the coder's seat and must stay: {:?}",
            plain.tools
        );
    }
}

/// The Done-when's prompt half: the role's overlay is in message zero, after the standing notes and
/// before the session's own paragraph, and the coder's overlay is not - two profiles must not be
/// able to arrive together, or the model is told two different jobs.
#[test]
fn a_roles_overlay_reaches_the_prompt_and_only_its_own() {
    let mut cfg = a_child("explore", Vec::new());
    let overlay = cfg.seat_overlay.clone().expect("the child's overlay");
    cfg.compose_for_seat(
        &crate::config::SeatFacts::with_tools(["read", "grep"]),
        &overlay,
        None,
    );
    assert!(
        cfg.system
            .contains("You are the eyes of this task, not its hands."),
        "the role's own words are in message zero: {}",
        cfg.system
    );
    assert!(
        !cfg.system.contains("You are the hands of this task"),
        "the coder's overlay is not this session's: {}",
        cfg.system
    );
    // The seat's own prompt is still there: a role adds to a seat, and never replaces one.
    assert!(
        cfg.system
            .contains("You are a careful software engineering assistant"),
        "the base prompt stands: {}",
        cfg.system
    );
}

/// The word a row shows for a session's role: the profile's name when it has one, the seat's
/// otherwise. Two children with one seat and two roles must not read as one row.
#[test]
fn a_rows_role_is_the_profile_it_was_seated_as() {
    let child = a_child("explore", Vec::new());
    assert_eq!(crate::profiles::role_name(&child), "explore");
    let mut root = crate::config::Config::for_this_box(std::env::temp_dir());
    root.seat = crate::config::Seat::Planner;
    assert_eq!(crate::profiles::role_name(&root), "planner");
}
