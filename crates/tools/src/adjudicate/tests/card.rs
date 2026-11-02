//! The card's three failures, which are one rule: an operator is never shown a button whose
//! effect the gate would then decline to honour, and a rung that disappears says so.
//!
//! The tests: the rung is gone, the card says why, and what names it is the program doing the
//! work.

use super::*;

/// `Grant::covers` refuses an unresolved action before it looks at coverage, so a session grant
/// over such a call could never cover anything: the operator would take it, be asked again, and
/// the audit row would show a standing permission that never fires.
#[test]
fn an_unreadable_command_is_never_offered_a_session_grant() {
    let mut g = AdjudicatedGate::closed().with_surroundings(pinned());
    let args = json!({"command": "cat $X"});
    let req = g.request_for(&bash_at(&args, "s#1"));
    assert!(
        !req.resolved,
        "the premise: `$X` is not in the command text"
    );
    assert!(
        !req.options.iter().any(|o| o.id == "allow_session"),
        "the ladder offered a grant that could never cover anything: {:?}",
        req.options
    );
    // `allow_once` stays: this rung is about the standing grant, not about the call.
    assert!(req.options.iter().any(|o| o.id == "allow_once"));

    // The note says which rung is missing, why, and the word a person would have to rewrite.
    let note = req
        .options_note
        .as_deref()
        .expect("a rung was left off with nothing said about it");
    assert!(
        note.starts_with("not offered for the session: "),
        "the note has to say which rung it is about: {note}"
    );
    assert!(
        note.contains("meaning is not resolved"),
        "the note has to say why: {note}"
    );
    assert!(note.contains("$X"), "the note has to name the word: {note}");
    assert_eq!(note.lines().count(), 1, "the card has one line: {note}");
}

/// `admit` looks the selected option up in the ladder it built, so an id that is not there is a
/// refusal rather than an admission: an older head or a replayed answer leaves no dead grant on
/// the books.
#[test]
fn an_allow_session_over_an_unreadable_command_is_not_honoured() {
    struct Session;
    impl Adjudicator for Session {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            AdjudicationDecision::selected(req, "allow_session", "human:test", "yes")
        }
        fn describe(&self) -> String {
            "test".into()
        }
    }
    let mut g = AdjudicatedGate::new(Box::new(Session)).with_surroundings(pinned());
    let args = json!({"command": "cat $X"});
    let d = g.admit(&bash_at(&args, "s#1"));
    assert!(
        !matches!(d, GateDecision::Admit),
        "an option the request did not offer was honoured: {d:?}"
    );
    assert!(
        g.grants().is_empty(),
        "a grant that could never cover anything was recorded: {:?}",
        g.grants()
    );
}

/// The other side of the same condition, so dropping the rung everywhere fails here rather than
/// passing quietly.
#[test]
fn a_readable_command_is_offered_the_session_grant_and_the_card_is_silent() {
    let mut g = AdjudicatedGate::closed().with_surroundings(pinned());
    // Exec, so that the ladder is the exec one and not a read admitted by the mode.
    let args = json!({"command": "sed -n 1p /w/a.rs"});
    let req = g.request_for(&bash_at(&args, "s#1"));
    assert!(req.resolved);
    assert!(matches!(req.tier, Tier::MayApprove), "{:?}", req.tier);
    assert!(
        req.options.iter().any(|o| o.id == "allow_session"),
        "a grant taken here WOULD be honoured, so the rung belongs on the ladder: {:?}",
        req.options
    );
    assert_eq!(
        req.options_note, None,
        "a complete ladder has nothing to explain"
    );
}

/// A grant keyed on the last stage would be keyed on a word carrying no authority (`export`), so
/// the rung names the stage that does the work.
#[test]
fn the_session_rung_names_the_program_that_does_the_work() {
    // A measured boundary, because `cargo test` is a runner and a host with no boundary offers
    // neither standing rung.
    let mut g = AdjudicatedGate::closed()
        .with_surroundings(pinned())
        .with_boundary(Some(measured_boundary()), None)
        .with_mode(crate::mode::Mode::WRITES_ALLOWED);
    let measured = json!({
        "command": "cd /w && export CARGO_TARGET_DIR=$PWD/target && cargo test --offline -p x"
    });
    let req = g.request_for(&bash_at(&measured, "s#1"));
    let session = req
        .options
        .iter()
        .find(|o| o.id == "allow_session")
        .expect("the rung a readable command is offered");
    assert!(
        session.label.contains("cargo"),
        "the rung does not name the build: {:?}",
        session.label
    );
    assert!(
        !session.label.contains("export"),
        "the rung names the one stage with no authority: {:?}",
        session.label
    );

    // The label and the key are one choice, so a later `cargo ...` of the same shape is covered
    // and a later `export ...` is not. `cargo fmt` and not `cargo test`, because a grant over a
    // runner is never honoured on a host with no boundary.
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            let opt = if req.target.contains("cargo fmt") {
                "allow_session"
            } else {
                "deny"
            };
            Some(AdjudicationDecision::selected(
                req,
                opt,
                "human:test",
                "yes",
            ))
        },
    )))
    .with_surroundings(pinned());
    let fmt = json!({"command": "cd /w && export CARGO_TARGET_DIR=$PWD/target && cargo fmt --all"});
    let _ = g.admit(&bash_at(&fmt, "s#1"));
    let taken = g.grants().first().expect("a grant was taken").clone();
    assert_eq!(taken.coverage[0].program, "cargo");

    // The same command again runs unasked, so the fixture is not consulted.
    let again =
        json!({"command": "cd /w && export CARGO_TARGET_DIR=$PWD/target && cargo fmt --check"});
    assert!(matches!(
        g.admit(&bash_at(&again, "s#2")),
        GateDecision::Admit
    ));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "the grant did not settle the same command twice"
    );
    assert!(
        !matches!(
            g.admit(&bash_at(
                &json!({"command": "export LD_PRELOAD=/tmp/x.so"}),
                "s#3"
            )),
            GateDecision::Admit
        ),
        "a grant over the build admitted an environment change"
    );
}

/// Each operand is otherwise its own hole, so approving one file would not cover the same run
/// over two.
#[test]
fn a_shape_holds_across_a_different_number_of_operands() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "yes",
            ))
        },
    )))
    .with_surroundings(pinned());

    let one = json!({"command": "sed -n 1p /w/a.rs"});
    assert!(matches!(
        g.admit(&bash_at(&one, "s#1")),
        GateDecision::Admit
    ));
    assert_eq!(asked.load(Ordering::Relaxed), 1);

    // One more file and a second expression: the same run of operands, so the same shape.
    let more = json!({"command": "sed -n 2p /w/b.rs /w/c.rs"});
    assert!(matches!(
        g.admit(&bash_at(&more, "s#2")),
        GateDecision::Admit
    ));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "a longer operand list asked the operator again"
    );

    // The flag is still the flag: `sed -i` rewrites, so it is not that shape.
    let write = json!({"command": "sed -i 1p /w/a.rs"});
    let _ = g.admit(&bash_at(&write, "s#3"));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        2,
        "a write was admitted by a read's shape"
    );
}

/// A rule over `export*` would admit every variable a later stage trusts, so the durable rung is
/// not offered; the session rung, which can settle this command, still is. The derivation is
/// `crate::permission`'s own test.
#[test]
fn no_durable_rule_is_offered_over_a_command_that_sets_state() {
    // A measured boundary for the reason above: without one both standing rungs go, and this
    // test is about which of them is offered.
    let mut g = AdjudicatedGate::closed()
        .with_surroundings(pinned())
        .with_boundary(Some(measured_boundary()), None)
        .with_mode(crate::mode::Mode::WRITES_ALLOWED)
        .with_permission(crate::permission::seed());
    let args =
        json!({"command": "export CARGO_TARGET_DIR=$PWD/target && cargo test --offline -p x"});
    let req = g.request_for(&bash_at(&args, "s#1"));
    assert!(
        !req.options.iter().any(|o| o.id == "allow_always"),
        "a rule was offered that the matcher would never honour, or that would \
         blanket-allow every variable: {:?}",
        req.options
    );
    assert!(
        req.options.iter().any(|o| o.id == "allow_session"),
        "the offer that CAN settle this command was dropped too: {:?}",
        req.options
    );
}
