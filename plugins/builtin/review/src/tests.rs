//! The review layer's own words, tested natively: a copy of the host's policy is only worth having
//! if it is the same words, and what a person can check here is the rendering - the two revisions in
//! the diffs, the shapes one per line, and the answer for the queue's own prompt.

use super::*;

fn entry() -> Entry {
    Entry::of(
        "add a gatekeeper seat",
        "agent/gatekeeper",
        "a4fbe70",
        "9d1c2ab",
        "/build/wt-gatekeeper",
    )
}

/// The body names the two revisions in every diff it tells the reviewer to run, and there is no
/// placeholder left in it: a brief that says `@BASE@` is a reviewer sent to a branch that does not
/// exist.
#[test]
fn the_body_renders_the_base_and_the_head_everywhere() {
    let said = policy(&entry());
    assert!(!said.contains(BASE) && !said.contains(HEAD), "{said}");
    for want in [
        "git diff a4fbe70...9d1c2ab",
        "git diff --stat a4fbe70...9d1c2ab",
        "git log --oneline -5 a4fbe70..9d1c2ab",
    ] {
        assert!(said.contains(want), "`{want}` is missing from: {said}");
    }
    assert!(said.starts_with("The rules:\n1. Read the code."), "{said}");
}

/// Every rule is there, in order, and the two paragraphs a reviewer needs at the end: the six rules
/// are the whole of what this layer exists to say, and one dropped is a review that fails on it.
#[test]
fn the_body_is_the_six_rules_and_the_shapes() {
    let said = policy(&entry());
    for want in [
        "1. Read the code.",
        "2. Do not fix it.",
        "3. Judge the artifact against the ask.",
        "4. **Hand the verdict in with `review_verdict`.**",
        "5. **One command per call.**",
        "6. **Read the references, and say which.**",
        "The shapes a review needs, one per line",
        "Two things that are not exceptions.",
    ] {
        assert!(said.contains(want), "`{want}` is missing from: {said}");
    }
    let at = |what: &str| said.find(what).unwrap_or_else(|| panic!("no `{what}`"));
    assert!(at("1. Read") < at("6. **Read the references"));
}

/// The queue's own prompt gets nothing: this layer says what a review is, and the session that runs
/// reviews has its words from the host. An answer other than *unchanged* there would replace them.
#[test]
fn the_queues_own_prompt_is_left_alone() {
    assert!(answer(&BriefIn::queue()).is_none());
    let said = answer(&BriefIn::of(entry())).expect("a reviewer's brief");
    assert!(said.brief.is_some(), "{said:?}");
    assert!(!said.says_nothing());
}

/// The answer is the contract's own type, so a field renamed in the host breaks this build rather
/// than a brief nobody can read.
#[test]
fn the_answer_round_trips_as_the_sdks_brief() {
    let input = serde_json::to_string(&BriefIn::of(entry())).expect("the host's input");
    let text = say(input.as_bytes()).expect("an answer");
    let out: Brief = serde_json::from_str(&text).expect("a brief");
    assert_eq!(out.brief, Some(policy(&entry())));
}
