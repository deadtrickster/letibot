//! The texts the MODEL reads may not name a document section or a review item: neither is a name
//! it can look up, and neither tells it what to do instead.

use super::*;

/// A refusal's `tell` and the summary `harness status` shows are both read by the model. Each
/// keeps the half that says what to do; none carries an internal marker.
#[test]
fn the_text_the_model_reads_names_no_internal_document_or_review_item() {
    let mut g = AdjudicatedGate::closed().with_surroundings(pinned());
    let secret = format!(
        "{}/.ssh/authorized_keys",
        pinned().home.clone().unwrap_or_default()
    );
    let refused = g.admit(&call("write", &json!({"path": secret, "content": "x"})));
    let GateDecision::Refuse { tell, .. } = refused else {
        panic!("the never-write list must refuse a key's path: {refused:?}")
    };
    assert!(tell.contains("never-write list"), "{tell}");
    assert!(tell.contains("no adjudicator is consulted"), "{tell}");

    let no_gate = crate::runtime::NoBoundary.admit(&call("write", &json!({"path": "a.txt"})));
    let GateDecision::Refuse { outcome, .. } = no_gate else {
        panic!("a gate with nobody to ask must refuse: {no_gate:?}")
    };
    let ToolOutcome::NotRun { why } = outcome else {
        panic!("nobody decided, so the outcome is `not_run`: {outcome:?}")
    };
    assert!(why.contains("adjudicator"), "{why}");

    let credential = crate::exec::Grant::agent_from_env("the fixture started no agent")
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();

    let texts = [
        ("the never-write refusal", tell),
        ("the no-adjudicator refusal", why),
        (
            "the mode's summary",
            crate::mode::Mode::WRITES_ALLOWED.summary.to_string(),
        ),
        ("the credential refusal", credential),
    ];
    for (what, text) in texts {
        assert!(!text.is_empty(), "{what} said nothing at all");
        for internal in ["§", "R98", "R27", "D16", "docs/"] {
            assert!(
                !text.contains(internal),
                "{what} names `{internal}`, which the model cannot act on:\n{text}"
            );
        }
    }
    // And the actionable half survives, or this would pass on texts that had stopped saying it.
    assert!(
        crate::mode::Mode::WRITES_ALLOWED
            .summary
            .contains("ask_unconfined_code"),
        "the mode's summary must still name the setting it turns on"
    );
}
