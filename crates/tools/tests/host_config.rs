//! `host_config` through the whole runtime — the half the unit tests cannot see.
//!
//! The unit tests in `builtins/host_config/tests.rs` drive `dispatch` with a platform and a
//! changelog of their own. These check the three things that only exist once a session is built:
//! the seat holds the tool, `set` reaches the gate as an **exec**, and a backend opened read-only
//! refuses before anything runs. Every platform here is the fixture's — no test in this tree
//! changes the operator's keyboard repeat or their governor.

use letibot_tools::testing::{harness, writable_harness};

/// **A `set` through the runtime lands in the changelog and on the fixture's platform**, and the
/// record's `before` is what the fixture read.
#[test]
fn a_set_through_the_runtime_is_applied_and_recorded() {
    let mut h = writable_harness();
    let seen = h
        .call(
            "host_config",
            r#"{"verb":"set","setting":"fixture-knob","value":"after",
                "why":"the fixture asked","words":"do it in the fixture"}"#,
        )
        .render();
    assert!(seen.contains("applied on fixture"), "{seen}");
    assert!(seen.contains("`before` → `after`"), "{seen}");
    assert!(seen.contains("recorded #1"), "{seen}");

    // The platform ran the command the fixture spells, and nothing else.
    assert_eq!(h.host_config().applied(), vec!["fixture-write after"]);
    // The file the record went into is the changelog's, read here without the tool.
    let line = std::fs::read_to_string(h.changelog_file()).expect("the changelog was written");
    assert_eq!(line.lines().count(), 1, "{line}");
    assert!(line.contains(r#""before":"before""#), "{line}");
    assert!(line.contains(r#""after":"after""#), "{line}");
}

/// **A read through the runtime costs no decision** — `settings` and `read` narrow to `Read`, so
/// they answer even in a session whose gate allows nothing.
#[test]
fn a_read_through_the_runtime_answers_with_no_adjudicator() {
    let mut h = harness();
    let seen = h
        .call("host_config", r#"{"verb":"read","setting":"fixture-knob"}"#)
        .render();
    assert!(seen.contains("fixture's `fixture-knob`"), "{seen}");
    assert!(seen.contains("`before`"), "{seen}");
}

/// **A read-only session refuses a change, and which gate refuses it depends on what is attached.**
///
/// Two gates stand above the platform and they are in a fixed order: with no adjudicator the call
/// is `not run` before the tool is reached, and with an adjudicator that admits, the backend's own
/// read-only answer is what refuses. Neither runs anything.
#[test]
fn a_set_from_a_read_only_session_is_refused_before_the_platform() {
    let set = r#"{"verb":"set","setting":"fixture-knob","value":"after",
                  "why":"the fixture asked","words":"do it in the fixture"}"#;

    // No adjudicator: the fail-closed gate, which never reaches the tool.
    let mut h = harness();
    let seen = h.call("host_config", set).render();
    assert!(seen.contains("no adjudicator is attached"), "{seen}");
    assert!(h.host_config().applied().is_empty(), "nothing ran");
    assert!(
        !h.changelog_file().exists(),
        "and nothing was recorded: {:?}",
        h.changelog_file()
    );

    // An adjudicator that admits, over a backend opened read-only: the tool is reached and its
    // own second gate is what refuses, naming the backend.
    let mut h = letibot_tools::testing::read_only_harness_with_gate(Some(
        letibot_tools::testing::allow_all(),
    ));
    let seen = h.call("host_config", set).render();
    assert!(seen.contains("opened read-only"), "{seen}");
    assert!(h.host_config().applied().is_empty(), "nothing ran");
    assert!(!h.changelog_file().exists(), "and nothing was recorded");
}

/// **A setting that needs root, with no way to ask, is refused through the runtime too** — the
/// fixture's privilege is a fixed refusal, which is what makes this assertable on any box.
#[test]
fn a_root_setting_through_the_runtime_names_the_missing_helper() {
    let mut h = writable_harness();
    let seen = h
        .call(
            "host_config",
            r#"{"verb":"set","setting":"fixture-root-knob","value":"after",
                "why":"the fixture asked","words":"do it in the fixture"}"#,
        )
        .render();
    assert!(seen.contains("needs root"), "{seen}");
    assert!(seen.contains("no askpass"), "{seen}");
    assert!(h.host_config().applied().is_empty(), "nothing ran");
}

/// **`settings` names the platform**, so a model that asked in the wrong vocabulary is told which
/// one it is in.
#[test]
fn settings_through_the_runtime_names_the_platform() {
    let mut h = harness();
    let seen = h.call("host_config", r#"{"verb":"settings"}"#).render();
    assert!(seen.contains("fixture — 2 setting(s)"), "{seen}");
    assert!(seen.contains("fixture-knob"), "{seen}");
    assert!(seen.contains("needs root"), "{seen}");
}
