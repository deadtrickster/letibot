//! Notices that say what to do, diagnostics that stay out of the conversation, and a room message
//! drawn as chat. See `letibot_sessionlog::warning::Action`.

use super::redcard::{rows_with, with_notice, with_warning};
use super::*;

/// One detail for every code: a sentence no card reads, so the only thing under test is what the
/// table says about the code rather than what a reader makes of the words.
const DETAIL: &str = "a sentence no card reads, and this is its second clause";

/// Every code a head draws carries its action on the line; a code it does not draw in the
/// conversation is a developer's, filed beside it for `/notes` and never a row.
#[test]
fn every_code_draws_its_action_or_is_not_drawn() {
    use letibot_sessionlog::warning::TABLE;
    let (mut held, mut state) = (0, 0);
    for (code, _class, _action) in TABLE {
        let mut a = with_warning(code, DETAIL);
        let _ = a.screen(200, 40);
        let listed = a.notes_lines().join("\n");
        // One record: no code draws a row of the conversation, whatever it asks the reader to do.
        assert!(
            !a.notes
                .iter()
                .any(|(place, _)| matches!(place, Placed::Seam(_))),
            "`{code}` took a row of the conversation"
        );
        if a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == *code))
        {
            assert!(
                listed.contains(code),
                "`{code}` is held and `/notes` does not list it"
            );
            held += 1;
        } else {
            // A code the head takes as state rather than as a note (the model's switch, a store's
            // build, a turn's own failure) files nothing, and that is its answer.
            state += 1;
        }
    }
    // The floors: a head that filed nothing, or one that kept every code as state, would pass
    // every assert above without checking anything.
    assert!(held > 100, "{held} codes held in `/notes`");
    assert!(state < 20, "{state} codes kept as state");
}

/// A session's developer diagnostics get ONE line, whatever arrives: the codes themselves are
/// listed by `/notes` and written to the daemon's log, and the conversation points at them once.
#[test]
fn a_sessions_developer_diagnostics_are_one_info_line() {
    let mut a = with_warning(
        "memory_creep",
        "the resident grew on three readings running",
    );
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    // Not a row: a developer's diagnostic is `/notes` and the daemon's log, and the conversation
    // gets nothing at all - not even the one line it used to.
    assert_eq!(rows_with(&screen, "diagnostics ·").len(), 0, "{joined}");
    // More of them: the listing grows and the screen does not.
    for (code, detail) in [
        ("memory_bound_broken", "a bound did not hold"),
        ("ledger_chain_mismatch", "row 41's head is not the one"),
        (
            "prefix_divergence",
            "the prompt differs from what the server was sent",
        ),
        (
            "frame_capture_failed",
            "the refused frame could not be written",
        ),
    ] {
        a.apply(ServerFrame::Event(env(
            9,
            SessionEvent::Warning {
                code: code.into(),
                detail: detail.into(),
                compaction: None,
            },
        )));
    }
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    assert_eq!(rows_with(&screen, "diagnostics ·").len(), 0, "{joined}");
    for code in [
        "memory_creep",
        "memory_bound_broken",
        "ledger_chain_mismatch",
        "prefix_divergence",
        "frame_capture_failed",
    ] {
        assert!(
            a.notes
                .iter()
                .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == code)),
            "`{code}` is not listed by `/notes`: {:?}",
            a.notes.len()
        );
    }
}

/// A room message is chat: who said it, the room and what they said - never a generic row with a
/// label in front of the header the room itself wrote.
#[test]
fn a_room_message_is_drawn_as_chat_and_carries_no_notice_label() {
    let line = "#41 [project] from dead (say) to all\n> the gate is green, landing it now";
    let mut a = with_notice(line);
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    assert_eq!(
        rows_with(
            &screen,
            "chat dead · #41 in project · the gate is green, landing it now"
        )
        .len(),
        1,
        "{joined}"
    );
    let row = rows_with(&screen, "#41 in project")
        .first()
        .map(|l| l.to_string())
        .unwrap_or_else(|| panic!("{joined}"));
    assert!(!row.contains("notice"), "a room message is chat: {row}");

    // In a row of several deliveries it is its own line, drawn as chat, and the notice beside it
    // is still a notice: the split is on the room's own header.
    let mut a = with_notice(&format!(
        "{line}\n\n[monitor] `ci` fired: the run exited 0."
    ));
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    assert_eq!(
        rows_with(&screen, "chat dead · #41 in project").len(),
        1,
        "{joined}"
    );
    assert_eq!(rows_with(&screen, "monitor · ci").len(), 1, "{joined}");
    assert_eq!(a.notice_rows.len(), 2, "{:?}", a.notice_rows);
}
