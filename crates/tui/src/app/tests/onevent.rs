//! One event is one row, and that row is the model's: a warning the daemon raises beside a notice
//! the model was sent is filed BESIDE the conversation (`/notes`, `/status`), never drawn. The
//! screen with the notice and the screen with the notice plus its warning are byte-identical, which
//! is the whole of the rule and the only assertion that cannot be satisfied by hiding the row.

use super::*;
use letibot_transcript::Speaker;

/// The daemon's `[reseat]` notice (`harnessd::harness::reseated_for_the_model`), one row.
const RESEAT_ROW: &str =
    "[reseat] Your system prompt and tools were updated. New: task_start. Carry on.";
/// The `reseated` warning's own sentence - the twin this file is about.
const RESEAT_SAID: &str =
    "this conversation now speaks the prompt this daemon seats: the tool list was rebuilt";

/// The model's row for a compaction (`harness::wall`'s fork note), one row.
const COMPACTED_ROW: &str = "Earlier conversation is replaced by this summary (full history: \
                             `transcript` tool). The most recent messages follow verbatim.\n\n\
                             The session read the brief and started the build.";
/// The `compacted` warning's own sentence.
const COMPACTED_SAID: &str = "compacted: 41k tokens resident before, 9k after";

/// The model's row for a `/clear` (`harness::clear::orientation`), one row.
const CLEARED_ROW: &str = "[cleared] The operator cleared the conversation; assume nothing from \
                            before - not even a summary.";
/// The `cleared` warning's own sentence.
const CLEARED_SAID: &str = "conversation cleared: tr-1 → tr-2. The new conversation is message \
                            zero plus a 12-line orientation";

/// The model's row for a daemon restart (`restart::notice`), one row.
const RESTART_ROW: &str = "[restart] The daemon restarted (down ~1s). Nothing was running.";
/// The `restart_restored` warning's own sentence.
const RESTART_SAID: &str = "restarted (down 1s); nothing was running";

/// A head that attached empty and then watched one model row and one warning arrive, in the order
/// the daemon writes them: the row first, the warning after.
fn watched(row: TranscriptItem, code: &str, said: &str) -> App {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(
        hub.publish(testing::appended("s.0", "user")),
    ));
    a.apply(ServerFrame::Event(hub.publish(
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(row),
        },
    )));
    a.apply(ServerFrame::Event(hub.publish(SessionEvent::Warning {
        code: code.into(),
        detail: said.into(),
        compaction: None,
    })));
    a
}

fn screen(a: &mut App) -> String {
    a.screen(120, 40).join("\n")
}

/// A notice row: a delivery in the harness's voice, which is what every one of these four is.
fn notice(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Speaker::Agent,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

/// The rule, for each of the four events that used to draw two rows: the model's row is drawn, the
/// warning's sentence is not, and the warning is still there to be read - `/notes` lists it.
///
/// The transcript is read from `hist_lines`, not the screen: the screen's last lines are the status
/// line, where a refusal or a failure is said as it lands, and that is not a row of the record.
fn one_row(row: TranscriptItem, code: &str, said: &str, fingerprint: &str) {
    let mut a = watched(row, code, said);
    // Laid out first: `hist_lines` is written by the frame.
    let drawn = screen(&mut a);
    let body = a.hist_lines.join("\n");
    assert!(
        body.contains(fingerprint),
        "the model's own row is not drawn, so the event is not on the screen at all: \
         {fingerprint:?}\n{drawn}"
    );
    assert!(
        !body.contains(said),
        "the warning `{code}` drew a second row under the model's own\n{body}"
    );
    assert!(
        !a.notes
            .iter()
            .any(|(place, _)| matches!(place, Placed::Seam(_))),
        "`{code}` is placed in the conversation: {:?}",
        a.notes
    );
    assert!(
        a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == code)),
        "`{code}` left no record at all: {:?}",
        a.notes
    );
}

#[test]
fn a_reseat_is_the_models_row_and_no_other() {
    one_row(notice(RESEAT_ROW), "reseated", RESEAT_SAID, "reseated");
}

#[test]
fn a_compaction_is_the_summary_row_and_no_other() {
    let row = TranscriptItem::System {
        text: COMPACTED_ROW.into(),
        origin: letibot_transcript::SystemOrigin::Update,
    };
    one_row(row, "compacted", COMPACTED_SAID, "compacted summary");
}

#[test]
fn a_clear_is_the_orientation_row_and_no_other() {
    one_row(
        notice(CLEARED_ROW),
        "cleared",
        CLEARED_SAID,
        "cleared · The operator cleared the conversation",
    );
}

#[test]
fn a_daemon_restart_is_the_notice_row_and_no_other() {
    one_row(
        notice(RESTART_ROW),
        "restart_restored",
        RESTART_SAID,
        "crashed, down ~1s",
    );
}

/// The head's signals still fire without a row: `prefix_stale` raises the reseat card, and
/// `reseated` takes it down - and neither writes a line of the conversation.
#[test]
fn the_reseat_card_still_moves_and_draws_nothing() {
    let mut a = watched(notice(RESEAT_ROW), "prefix_stale", RESEAT_SAID);
    assert!(a.reseat.is_some(), "the premise: the card was raised");
    let _ = screen(&mut a);
    assert!(
        !a.hist_lines.join("\n").contains(RESEAT_SAID),
        "the stale prompt's note is in the conversation\n{}",
        a.hist_lines.join("\n")
    );
    a.apply(ServerFrame::Event(env_at(
        0,
        7,
        SessionEvent::Warning {
            code: "reseated".into(),
            detail: RESEAT_SAID.into(),
            compaction: None,
        },
    )));
    screen(&mut a);
    assert!(
        a.reseat.is_none(),
        "the card is still up after the re-seat landed"
    );
}

/// The whole table, one code at a time: NO warning is a row of the conversation, and each one is
/// held with its destination's other half - a count on the edge for a fault, the status line for
/// live state. A code that starts drawing a row, or loses its record, fails here.
#[test]
fn no_warning_is_a_row_of_the_conversation() {
    let mut model = 0;
    let mut notes = 0;
    let mut status = 0;
    for (code, class, _) in letibot_sessionlog::warning::TABLE {
        let mut a = app();
        a.apply(hello(
            "s",
            vec![brief("s", "one", false)],
            Hub::new("s").snapshot(),
        ));
        a.apply(ServerFrame::Event(env_at(
            1,
            11,
            SessionEvent::Warning {
                code: (*code).into(),
                detail: format!("the sentence `{code}` is raised with"),
                compaction: None,
            },
        )));
        let _ = screen(&mut a);
        // A code whose arm took it as STATE (a fold's announcement, a turn's own failure, the
        // model's switch) files nothing: those return before the head reaches the table.
        if matches!(
            *code,
            "compacting"
                | "turn_failed"
                | "store_newer"
                | "prefix_current"
                | "model_switch_pending"
                | "daemon_starting"
                | "daemon_started"
                | "model_set"
        ) {
            continue;
        }
        let body = a.hist_lines.join("\n");
        let placed = a
            .notes
            .iter()
            .any(|(place, _)| matches!(place, Placed::Seam(_)));
        assert!(
            !body.contains(&format!("`{code}`")) && !placed,
            "`{code}` drew a row of the conversation\n{body}"
        );
        match letibot_sessionlog::warning::destination(code) {
            letibot_sessionlog::warning::Go::Model => model += 1,
            letibot_sessionlog::warning::Go::Status => {
                status += 1;
                // Live state is said on the status line, and it is still in `/notes`.
                assert!(
                    a.notice.as_deref().is_some_and(|n| n.contains(code)),
                    "`{code}` is live state and the status line is silent: {:?}",
                    a.notice
                );
            }
            letibot_sessionlog::warning::Go::Notes => {
                notes += 1;
                if class.is_refused() {
                    assert_eq!(a.refused, 1, "`{code}` is a refusal and the edge missed it");
                } else if class.is_failure() {
                    assert_eq!(a.failed, 1, "`{code}` is a failure and the edge missed it");
                }
            }
        }
        let _ = body;
        assert!(
            a.notes
                .iter()
                .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == *code)),
            "`{code}` left no record at all: {:?}",
            a.notes
        );
    }
    assert_eq!(model, letibot_sessionlog::warning::TWIN.len());
    assert!(notes > 100, "{notes} codes go to `/notes`");
    assert!(status > 10, "{status} are live state");
}

/// A daemon fact the model was not told is held and counted on the edge - and still not a row.
#[test]
fn a_daemon_fact_with_no_model_twin_is_held_and_counted() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env_at(
        1,
        11,
        SessionEvent::Warning {
            code: "boundary_absent".into(),
            detail: "this session has no boundary".into(),
            compaction: None,
        },
    )));
    let _ = screen(&mut a);
    assert!(
        a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == "boundary_absent")),
        "the sentence is not held: {:?}",
        a.notes
    );
    assert_eq!(a.failed, 1, "the edge does not count it");
    assert!(
        !a.hist_lines.join("\n").contains("no boundary"),
        "it drew a row: {}",
        a.hist_lines.join("\n")
    );
}
