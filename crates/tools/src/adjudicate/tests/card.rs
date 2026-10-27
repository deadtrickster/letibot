//! **The three things the card was wrong about** — one file, because they are one
//! question asked at three seams.
//!
//! MEASURED 2026-10-26, on one package session's 178 gate decisions: 15 of its 22 asks
//! carried a baseline reading *"the meaning was never resolved"*, offered *"Allow … for
//! the rest of the session"* over them, recorded a grant that could never cover anything
//! (`Grant::covers` refuses an unresolved action before it looks at coverage), and named
//! the last program in the line — `Allow \`head\`` on a cargo build. Each of those is a
//! different failure of the same rule: *an operator is never shown a button whose effect
//! the gate would then decline to honour* — and a rung that disappears without a word is
//! the second half of it.
//!
//! So the tests here are: the rung is gone, the card says why, and what names it is the
//! program doing the work.

use super::*;

/// **A command nobody could read is not offered a session grant — and the card says why.**
///
/// `Grant::covers` refuses an unresolved action *first*, before it looks at coverage, so
/// the grant an `allow_session` answer recorded over such a call could never cover
/// anything: the operator took it, was asked again, and the audit row showed a standing
/// permission that was never going to fire.
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
    // `allow_once` stays: refusing to admit the call would be a different decision than
    // the one this rung is about.
    assert!(req.options.iter().any(|o| o.id == "allow_once"));

    // The card says which rung is missing and why, in one line, naming the word a person
    // would have to rewrite.
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

/// **And an `allow_session` that arrives anyway is refused, not honoured.**
///
/// The rung is not the only place the answer is checked: `admit` looks the selected option
/// up in the ladder it built, and an id that is not there is a refusal — *"the adjudicator
/// selected `allow_session`, which this request did not offer"*. So an older head, a
/// replayed answer or a test that named the rung anyway gets no admission AND leaves no
/// dead grant on the books.
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

/// **A command whose meaning IS read is offered the rung, and the card says nothing.**
/// The other side of the same condition, so a fix that dropped the rung everywhere would
/// fail here rather than pass quietly.
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

/// **The session rung names the program doing the work, not the last one in the line.**
///
/// `grant_program` read the last stage, so the operator's own measured command offered
/// *"Allow `export` (this class) for the rest of the session"* — `export` is the one token
/// in that line carrying no authority at all — and `… | head` offered `head`.
#[test]
fn the_session_rung_names_the_program_that_does_the_work() {
    let mut g = AdjudicatedGate::closed().with_surroundings(pinned());
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

    // And the grant recorded under it is keyed the same way — the label and the key are one
    // choice, so a later `cargo test` is covered and a later `export …` is not.
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            let opt = if req.target.contains("cargo test") {
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
    let _ = g.admit(&bash_at(&measured, "s#1"));
    let taken = g.grants().first().expect("a grant was taken").clone();
    assert_eq!(taken.coverage[0].program, "cargo");

    // The same command again runs unasked — the fixture is not consulted at all.
    let again = json!({
        "command": "cd /w && export CARGO_TARGET_DIR=$PWD/target && cargo test --offline -p y"
    });
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

/// **A shape holds across a different NUMBER of operands.**
///
/// The brief's finding 3, at the seam a person feels it: approving `sed -n 1p a.rs` and
/// then running it over two files asked again, because each operand was its own hole.
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

    // Two more operands: one more file and a second expression. Both are the same run of
    // operands, so this is the shape that was approved.
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

    // And the flag is still the flag: `sed -i` rewrites, so it is not that shape.
    let write = json!({"command": "sed -i 1p /w/a.rs"});
    let _ = g.admit(&bash_at(&write, "s#3"));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        2,
        "a write was admitted by a read's shape"
    );
}

/// **No durable rule is offered over a command that sets shell state for a later
/// stage**, and the same command still offers the session rung — which is the offer that
/// can actually help it.
///
/// The derivation behind it, and what `export*` would have admitted, is
/// `crate::permission`'s own test: `a_rule_over_export_would_admit_every_variable`.
#[test]
fn no_durable_rule_is_offered_over_a_command_that_sets_state() {
    let mut g = AdjudicatedGate::closed()
        .with_surroundings(pinned())
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
