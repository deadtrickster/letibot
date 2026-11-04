use super::*;
use std::path::PathBuf;
use std::sync::Arc;

use letibot_embeddings::{Embedder, Tier};
use letibot_transcript::{ReasoningField, ToolCall};

use crate::embeddings::fixture;

/// A row as a board holds it.
fn row(content: &str) -> TodoItem {
    TodoItem {
        content: content.into(),
        status: TodoStatus::Pending,
        by: letibot_tokencore::store::TodoBy::Model,
        when: None,
        needs: Vec::new(),
    }
}

fn open_row(owner: &str, content: &str) -> OpenRow {
    OpenRow {
        owner: owner.into(),
        content: content.into(),
    }
}

fn prompt(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Speaker::Operator,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn a_task(prompt: &str) -> ToolCall {
    ToolCall {
        id: "call_task".into(),
        name: "task".into(),
        arguments: format!("{{\"prompt\":\"{prompt}\",\"role\":\"coder\"}}"),
    }
}

/// A caller's tier over the fixture's embedder: a tier holds one corpus, so a test with two
/// callers takes two of these.
fn caller(embedder: &Arc<Embedder>) -> Offers {
    Offers::new().with_tier(Some(Tier::over(Arc::clone(embedder))))
}

/// A directory that removes itself, for the log.
fn logs(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "harnessd-waits-offer-{name}-{}-{}",
        std::process::id(),
        now_secs()
    ));
    let _ = std::fs::remove_dir_all(&p);
    p
}

/// The fixture's words are its directions, so a test is written in them and the two groups are
/// as far apart as the model can make two sentences.
const ROW: &str = "alpha beta gamma delta";
const BRIEF: &str = "alpha beta gamma";
const ELSEWHERE: &str = "kappa lambda mu nu";

/// The case the caller exists for: a brief that is a row somebody else holds is offered, and the
/// line names the row, its owner and the edge.
#[test]
fn a_brief_that_is_an_open_row_gets_the_offer() {
    let (_m, _d, embedder) = fixture::model("matching");
    let rows = vec![
        open_row("the child s-2", ROW),
        open_row("this session", ELSEWHERE),
    ];
    let mut offers = caller(&embedder);
    assert!(offers.compose(&rows, BRIEF));
    let made = offers.tail();
    assert_eq!(made.len(), 1, "{made:?}");
    let TranscriptItem::User { parts, speaker } = &made[0] else {
        panic!("an offer is a user item: {made:?}")
    };
    assert_eq!(
        *speaker,
        Speaker::Agent,
        "the harness's own line, and it says so"
    );
    let UserPart::Text { text } = &parts[0] else {
        panic!("text")
    };
    assert!(text.contains(ROW), "{text}");
    assert!(text.contains("the child s-2"), "whose row it is: {text}");
    assert!(
        text.contains("needs"),
        "the edge that would join them: {text}"
    );
    assert!(!text.contains(ELSEWHERE), "the wrong row: {text}");
}

/// And a brief that is about something else says nothing at all: the line is worth reading or it
/// is not written.
#[test]
fn an_unrelated_brief_is_said_nothing() {
    let (_m, _d, embedder) = fixture::model("unrelated");
    let rows = vec![open_row("this session", ROW)];
    let mut offers = caller(&embedder);
    assert!(!offers.compose(&rows, ELSEWHERE));
    assert!(offers.tail().is_empty());
    // The row IS the best match and is still under the floor: silence here is the floor's work
    // and not an empty corpus's.
    let docs = corpus(&rows);
    let scored_corpus = Corpus::of_texts(&docs);
    let tier = Tier::over(Arc::clone(&embedder));
    scored_corpus.fill(Some(&tier)).expect("a fill");
    let (_, scored) = scored_corpus
        .best_with(ELSEWHERE, Some(&tier))
        .expect("a best");
    assert!(scored < OFFER_FLOOR, "{scored} of 1.00 is not silence");
    // An empty brief is nothing to score, and no rows is nothing to score against.
    assert!(!offers.compose(&rows, "  \n "));
    assert!(!offers.compose(&[], BRIEF));
}

/// Mode off is exactly what the daemon did before this caller existed: no tier, no offer, an
/// unchanged request.
#[test]
fn mode_off_composes_nothing() {
    let rows = vec![open_row("this session", ROW)];
    let mut offers = Offers::new();
    assert!(!offers.compose(&rows, BRIEF));
    assert!(offers.tail().is_empty());
    assert!(offers.settle(&logs("off"), "s-1", &[]).is_none());
}

/// One row is offered once per session, and the log is the memory: a restart must not say it
/// again. Another session is its own memory.
#[test]
fn a_row_already_offered_is_not_offered_again() {
    let dir = logs("once");
    let (_m, _d, embedder) = fixture::model("once");
    let rows = vec![open_row("this session", ROW)];
    let mut offers = caller(&embedder);
    assert!(offers.compose(&rows, BRIEF));
    let settled = offers.settle(&dir, "s-1", &[]).expect("settled");
    assert_eq!(settled.content, ROW);
    let logged = std::fs::read_to_string(dir.join(LOG)).expect("the log");
    assert!(
        logged.contains("\"row\":\"alpha beta gamma delta\"") && logged.contains("\"taken\":false"),
        "{logged}"
    );
    let mut after = caller(&embedder);
    after.recall(&dir, "s-1");
    assert!(
        !after.compose(&rows, BRIEF),
        "a row already offered this session is not offered again"
    );
    let mut other = caller(&embedder);
    other.recall(&dir, "s-2");
    assert!(other.compose(&rows, BRIEF));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `todo_write` that names the row is the take-up, and nothing else is: a `read` of the same
/// words was a search, not the edge.
#[test]
fn a_needs_entry_is_the_take_up() {
    let dir = logs("taken");
    let (_m, _d, embedder) = fixture::model("taken");
    let rows = vec![open_row("this session", ROW)];
    let mut offers = caller(&embedder);
    assert!(offers.compose(&rows, BRIEF));
    let edge = ToolCall {
        id: "c".into(),
        name: "todo_write".into(),
        arguments: format!(
            "{{\"todos\":[{{\"content\":\"x\",\"needs\":[{{\"kind\":\"row\",\"content\":\"{ROW}\"}}]}}]}}"
        ),
    };
    assert!(offers.settle(&dir, "s-1", &[edge]).is_some());
    let logged = std::fs::read_to_string(dir.join(LOG)).expect("the log");
    assert!(logged.contains("\"taken\":true"), "{logged}");
    // A read of a note that quotes the row is not the edge, and neither is a `task` prompt.
    assert!(!names_row(
        &ToolCall {
            id: "c".into(),
            name: "read".into(),
            arguments: format!("{{\"path\":\"{ROW}\"}}"),
        },
        ROW
    ));
    assert!(!names_row(&a_task(ROW), ROW));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A finished, struck-off or postponed row is not work anybody is doing, and is never offered.
#[test]
fn a_settled_row_is_not_open() {
    assert!(open(&row(ROW)));
    assert!(open(&TodoItem {
        status: TodoStatus::InProgress,
        ..row(ROW)
    }));
    for status in [
        TodoStatus::Completed,
        TodoStatus::Cancelled,
        TodoStatus::Postponed,
    ] {
        assert!(!open(&TodoItem { status, ..row(ROW) }), "{status:?}");
    }
}

/// The brief is the prompt and the `task` prompt. A subagent prompt is the text this caller
/// exists to catch, and a `task` call carries it in its arguments rather than in the row's prose.
#[test]
fn the_draft_is_the_prompt_and_the_task_prompt() {
    let items = vec![
        prompt("carry on with the plan"),
        TranscriptItem::Assistant {
            text: "spawning a child".into(),
            tool_calls: vec![a_task("alpha beta gamma delta")],
            truncated: false,
        },
    ];
    let text = draft(&items);
    assert!(text.contains("alpha beta gamma delta"), "{text}");
    assert!(text.contains("carry on with the plan"), "{text}");
    assert!(text.contains("spawning a child"), "{text}");
    // The row's own reasoning is not a brief, and a settlement from the harness is not the
    // person's prompt.
    let items = vec![
        TranscriptItem::User {
            speaker: Speaker::Agent,
            parts: vec![UserPart::Text {
                text: "a job finished".into(),
            }],
        },
        TranscriptItem::Reasoning {
            text: "a thought".into(),
            field: ReasoningField::Inline,
            truncated: false,
        },
    ];
    assert!(
        !draft(&items).contains("a job finished"),
        "the harness's own row is not the brief"
    );
    assert!(
        task_prompt(&ToolCall {
            id: "c".into(),
            name: "read".into(),
            arguments: "{\"prompt\":\"x\"}".into(),
        })
        .is_none()
    );
}
