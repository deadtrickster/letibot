//! A collapsed notice stays collapsed: the pairing `one_event` makes (one event, one row) holds
//! across the doors that replace a transcript and across the reader's own retirement.
//!
//! A notice is collapsed when its one-line card stands for the session row that says the same event
//! - a `[reseat]`, a `[restart]`, an `EMERGENCY` for a red tip, a stale prompt. The rule is that a
//! row is collapsed while its note's card is on the screen OR the reader retired that note, and the
//! pairing is keyed by the row's `item_id`, so a snapshot handing back the same rows keeps it. Keyed
//! by index, every resync, reattach and switch back would clear it.

use super::*;
use letibot_transcript::Speaker;

/// The daemon's `[reseat]` notice (`harnessd::harness::reseated_for_the_model`), one row.
const RESEAT_ROW: &str =
    "[reseat] Your system prompt and tools were updated. New: task_start. Carry on.";
/// The `prefix_stale` sentence that card stands for, cut to its first clause.
const RESEAT_SAID: &str = "session s was seated 10-08 21:14 with a prompt this daemon would not \
                            compose now: the tool schemas have changed since.";

/// The two events a reseat writes, published to a hub so a snapshot carries the same envelope —
/// and fed to the head as the same envelopes, which is what makes the note's key identical by
/// both routes (`(code, ts, detail)`, `note_key`).
fn reseat(hub: &Hub) -> Vec<letibot_sessionlog::event::Envelope> {
    let mut out = vec![hub.publish(testing::appended("s.0", "user"))];
    out.push(hub.publish(SessionEvent::TranscriptContent {
        item_id: "s.0".into(),
        item: Box::new(TranscriptItem::User {
            speaker: Speaker::Agent,
            parts: vec![UserPart::Text {
                text: RESEAT_ROW.into(),
            }],
        }),
    }));
    out.push(hub.publish(SessionEvent::Warning {
        code: "reseated".into(),
        detail: RESEAT_SAID.into(),
        compaction: None,
    }));
    out
}

/// A head that attached empty and then **watched** the reseat happen — the row, then the warning,
/// which is the order the daemon writes them in and the order that files the note at the row's
/// seam.
fn watched() -> (App, std::sync::Arc<Hub>) {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    for e in reseat(&hub) {
        a.apply(ServerFrame::Event(e));
    }
    (a, hub)
}

fn screen(a: &mut App) -> String {
    a.screen(120, 40).join("\n")
}

/// The collapse, as the operator sees it: one card for the warning and NO row of its own.
/// `notice_rows` is the rows a click opens, so a collapsed row is one that is not in it - asserted
/// both ways, since a card with the row under it is the duplicate the pairing exists to end.
fn collapsed(a: &mut App, what: &str) {
    let s = screen(a);
    assert_eq!(
        a.notice_rows.len(),
        0,
        "{what}: the row is drawn again\n{s}"
    );
    assert_eq!(a.card_rows.len(), 1, "{what}: and its card is not\n{s}");
}

#[test]
fn a_reseat_is_one_card_and_no_row() {
    let (mut a, _hub) = watched();
    collapsed(&mut a, "the premise");
    assert_eq!(
        a.absorbed.len(),
        1,
        "the premise: one row is the one the card stands for — {:?}",
        a.absorbed
    );
}

/// `ctrl-n` hides the notice; it does not hand its row back. Retiring the card must not put the
/// `[reseat]` row on the screen in its place: the reader asked for the wall to go, and the note is
/// still counted on `/status`.
#[test]
fn a_retired_notice_keeps_its_row_collapsed() {
    let (mut a, _hub) = watched();
    a.key(Key::CtrlN);
    let s = screen(&mut a);
    assert_eq!(
        a.notice_rows.len(),
        0,
        "the row of the notice the reader retired is drawn again\n{s}"
    );
    assert_eq!(a.card_rows.len(), 0, "and the card it retired is gone\n{s}");
    // Retired is not deleted (R10): the note is still held, still counted, still listed.
    assert_eq!(a.notes.len(), 1, "{:?}", a.notes);
    assert_eq!(a.retired_notes(), 1);
    assert!(
        a.notes_lines().join("\n").contains("reseated"),
        "`/notes` still names it: {:?}",
        a.notes_lines()
    );
}

/// And it stays retired across the events that replace the transcript: a `/resync`, a reattach and a
/// switch away and back all re-plant the same note under the same key, and none of them may bring
/// back the row the reader retired.
#[test]
fn a_retired_notice_stays_collapsed_across_a_resync_and_a_switch() {
    let (mut a, hub) = watched();
    a.key(Key::CtrlN);

    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    let s = screen(&mut a);
    assert_eq!(a.notice_rows.len(), 0, "a resync drew the row again\n{s}");

    a.apply(hello(
        "other",
        vec![brief("other", "two", false)],
        Hub::new("other").snapshot(),
    ));
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    let s = screen(&mut a);
    assert_eq!(
        a.notice_rows.len(),
        0,
        "a switch away and back drew the row again\n{s}"
    );
    assert_eq!(
        a.card_rows.len(),
        0,
        "and the retired card is not back\n{s}"
    );
}

/// A note the 64-note cap evicted still holds its row collapsed: the pairing outlives the note it
/// names, because it is about the reader's retirement. `file_note` drops the oldest note while
/// `absorbed` keeps a pairing per id, so asking the notes alone hands the row back at a rebuild.
#[test]
fn a_retired_notice_whose_note_was_evicted_keeps_its_row_collapsed() {
    let (mut a, _hub) = watched();
    a.key(Key::CtrlN);
    // 64 more of another kind: the reseat's note is the oldest, so the cap drops it.
    for i in 0..64u64 {
        a.apply(ServerFrame::Event(env_at(
            0,
            10_000 + i,
            SessionEvent::Warning {
                code: "queue_blocked".into(),
                detail: format!("the queue is blocked by row {i}"),
                compaction: None,
            },
        )));
    }
    assert!(
        !a.notes
            .iter()
            .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == "reseated")),
        "the premise — the reseat's note was not evicted: {:?}",
        a.notes.len()
    );
    // The rebuild the expansion does, which is the door that draws the cached rows again.
    a.invalidate_history();
    let s = screen(&mut a);
    assert_eq!(
        a.notice_rows.len(),
        0,
        "the evicted note gave its row back\n{s}"
    );
}

/// A note the reader did NOT retire gives its rows back when its card cannot be drawn: a snapshot's
/// warnings are history, so a head that attached after the event has no card to stand for the row,
/// and hiding the row would hide the event.
#[test]
fn a_snapshot_warning_with_no_card_gives_its_row_back() {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let _ = reseat(&hub);
    // A head that never watched it: the whole conversation arrives in one snapshot.
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    let s = screen(&mut a);
    assert_eq!(
        a.notice_rows.len(),
        1,
        "the row is the only thing that says the reseat happened\n{s}"
    );
    assert_eq!(a.card_rows.len(), 0, "and there is no card for it\n{s}");
}
