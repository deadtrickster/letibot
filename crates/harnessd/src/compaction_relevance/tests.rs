use super::*;
use std::sync::Arc;

use letibot_tokencore::store::{TodoBy, TodoStatus};
use letibot_transcript::Speaker;

use crate::embeddings::fixture;

/// A plan row.
fn row(content: &str) -> TodoItem {
    TodoItem {
        content: content.into(),
        status: TodoStatus::InProgress,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    }
}

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Speaker::Operator,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn answered(text: &str) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: text.into(),
        tool_calls: Vec::new(),
        truncated: false,
    }
}

/// Four exchanges of two items each, a hundred tokens apiece, and the newest exchange is where the
/// age-based plan starts: `from = 6` of eight.
fn history() -> (Vec<TranscriptItem>, Vec<u64>) {
    let items = vec![
        user("alpha beta"),
        answered("one"),
        user("kappa lambda"),
        answered("two"),
        user("mu nu"),
        answered("three"),
        user("the newest question"),
        answered("the newest answer"),
    ];
    (items, vec![100; 8])
}

/// The case the caller exists for: an old exchange that is about the plan is kept whole, and the
/// boundary moves back to its first item.
#[test]
fn an_old_exchange_about_the_plan_is_kept_in_full() {
    let (items, tokens) = history();
    let mut scores = vec![0.0; items.len()];
    // The SECOND exchange is about the row; the first and the third are about something else.
    scores[2] = 0.8;
    scores[3] = 0.7;
    let from = keep(&items, &tokens, 6, 600, &scores);
    assert_eq!(from, 2, "the pair that decided it, kept as the pair");
}

/// And an old half that is about nothing on the board keeps today's boundary: the tier does not
/// move the plan by itself.
#[test]
fn an_unrelated_history_keeps_todays_boundary() {
    let (items, tokens) = history();
    let scores = vec![0.02; items.len()];
    assert_eq!(keep(&items, &tokens, 6, 600, &scores), 6);
    // Under the floor is under the floor, however close the two are to each other.
    let mut low = vec![0.0; items.len()];
    low[2] = FLOOR - 0.01;
    assert_eq!(keep(&items, &tokens, 6, 600, &low), 6);
}

/// The room is the budget: an exchange that does not fit is not kept, and the boundary never
/// reaches zero - something must still be summarised.
#[test]
fn the_room_bounds_what_is_kept() {
    let (items, tokens) = history();
    let mut scores = vec![0.0; items.len()];
    scores[2] = 0.9;
    // The newest exchange is 200 of the 400 available, so the pair before it (400) does not fit.
    assert_eq!(keep(&items, &tokens, 6, 400, &scores), 6);
    assert_eq!(keep(&items, &tokens, 6, 600, &scores), 2);
    // Even with room for the whole history, the first pair stays summarised: a fold that folded
    // nothing is not a compaction.
    scores[0] = 0.95;
    assert_eq!(keep(&items, &tokens, 6, u64::MAX, &scores), 2);
}

/// Mode off is today's order: no tier, no scores, and the plan the age and size computed.
#[test]
fn mode_off_computes_no_scores() {
    let (items, _) = history();
    let plan = vec![row("alpha beta gamma delta")];
    assert!(scores(&items, &plan, None).is_none());
    // And a session with no plan has nothing to be relevant to.
    let (_m, _d, embedder) = fixture::model("noplan");
    let tier = Tier::over(Arc::clone(&embedder));
    assert!(scores(&items, &[], Some(&tier)).is_none());
}

/// With a tier the scores are per item, and the item about the plan leads: this is the number the
/// boundary above is moved by, so it has to be the plan's own text doing the scoring.
#[test]
fn the_plan_scores_each_item() {
    let (items, _) = history();
    let plan = vec![row("alpha beta gamma delta")];
    let (_m, _d, embedder) = fixture::model("scores");
    let tier = Tier::over(Arc::clone(&embedder));
    let scored = scores(&items, &plan, Some(&tier)).expect("scores");
    assert_eq!(scored.len(), items.len(), "{scored:?}");
    assert!(
        scored[0] > scored[2],
        "the exchange about the plan must lead the one about something else: {scored:?}"
    );
    assert!(scored[0] >= FLOOR, "{} of 1.00 is not a keep", scored[0]);
    assert!(scored[2] < FLOOR, "{} is not silence", scored[2]);
    // The query is the plan's own words, not the session's.
    assert_eq!(query(&plan), "alpha beta gamma delta");
}

/// A row's text is what the turn said, and a tool result is capped: the same 200 KB in every round
/// is the cost without the signal.
#[test]
fn an_item_is_scored_on_its_own_text() {
    assert_eq!(text_of(&user("what was asked")), "what was asked");
    assert_eq!(text_of(&answered("what was answered")), "what was answered");
    let long = "x".repeat(ITEM_CHARS * 2);
    assert_eq!(
        text_of(&TranscriptItem::ToolResult {
            call_id: "c".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: long,
            edit: None,
            origin: None,
            media: None,
        })
        .len(),
        ITEM_CHARS
    );
}
