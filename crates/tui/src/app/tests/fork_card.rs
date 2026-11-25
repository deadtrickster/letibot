//! The fork card: the message to fork at, chosen from the conversation's own turns rather than
//! counted. The row is named by the id the head holds, which is what the daemon resolves.

use super::*;
use letibot_transcript::{Speaker, TranscriptItem, UserPart};

/// One row of the conversation as a head holds it. `ts` is 0 - unknown - so the label a test reads
/// carries no clock, which is what a row replayed from an old log looks like.
fn row(id: &str, text: &str, speaker: Speaker) -> SnapshotItem {
    SnapshotItem {
        item_id: id.into(),
        kind: "user".into(),
        ledger_head: String::new(),
        ts: 0,
        item: Some(TranscriptItem::User {
            parts: vec![UserPart::Text { text: text.into() }],
            speaker,
        }),
        citations: Vec::new(),
    }
}

fn answer(id: &str) -> SnapshotItem {
    SnapshotItem {
        item_id: id.into(),
        kind: "assistant".into(),
        ledger_head: String::new(),
        ts: 0,
        item: Some(TranscriptItem::Assistant {
            text: "working on it".into(),
            tool_calls: Vec::new(),
            truncated: false,
        }),
        citations: Vec::new(),
    }
}

/// The conversation: two of the operator's turns, the agent's rows between them, and a peer's line
/// that is not the operator's to fork at.
fn a_conversation() -> App {
    let mut a = app();
    a.items = vec![
        row("s#t0.0", "fix the parser", Speaker::Operator),
        answer("s#t0.1"),
        row(
            "s#t0.2",
            "a room line",
            Speaker::Peer {
                from: "cc@rano".into(),
            },
        ),
        row("s#t0.4", "now do the jobs pane", Speaker::Operator),
        answer("s#t0.5"),
    ];
    a
}

/// `/fork` alone opens the card over the operator's turns, and Enter forks at the row the cursor is
/// on - by its id, because a number would be the daemon's count of a history this head only sees
/// part of.
#[test]
fn the_fork_card_lists_the_operators_turns_and_enter_forks_at_the_row_it_names() {
    let mut a = a_conversation();
    assert_eq!(a.command("fork"), None, "the card opens, nothing is sent");
    assert_eq!(a.pick, Some(Pick::Fork));
    let rows: Vec<String> = a
        .pick_values()
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(
        rows,
        ["fix the parser", "now do the jobs pane"],
        "the operator's turns and nobody else's, in the order they were said"
    );
    assert_eq!(a.mode_sel, 1, "the cursor opens on the newest turn");
    match a.key(Key::Enter) {
        Some(Action::Slash { line }) => assert_eq!(
            line, "fork s#t0.4",
            "Enter names the row the cursor was on, not its position"
        ),
        other => panic!("Enter on the card did not fork: {other:?}"),
    }
    assert_eq!(a.pick, None, "and the card is done with");
}

/// The card is a door, not a replacement: `/fork N` still reaches the daemon, and a conversation
/// with no turn of the operator's says so instead of opening an empty card.
#[test]
fn a_typed_message_number_is_still_the_daemons_and_an_empty_conversation_says_so() {
    let mut a = a_conversation();
    // A daemon's reply needs a session to be in: the default arm refuses to send a line without one.
    a.session_id = "s1".into();
    match a.command("fork 2") {
        Some(Action::Slash { line }) => assert_eq!(line, "fork 2", "typed, a number is sent"),
        other => panic!("`/fork 2` did not reach the daemon: {other:?}"),
    }
    assert_eq!(a.pick, None, "and it opened no card");

    let mut fresh = app();
    fresh.items = vec![answer("s#t0.1")];
    assert_eq!(fresh.command("fork"), None);
    assert_eq!(fresh.pick, None, "no card over nothing");
    let said = fresh.notice.clone().unwrap_or_default();
    assert!(
        said.contains("no message to fork at yet"),
        "the operator is told why: {said}"
    );
}
