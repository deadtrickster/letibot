//! **A collapsed notice stays collapsed** — the pairing R89 makes (`one_event`: one event, one
//! row) across the two doors that replace a transcript, and across the reader's own retirement.
//!
//! The operator, 2026-11-01: *"btw after certain evens all collapsed notifications reappear"*. A
//! notice is *collapsed* when its one-line card stands for the session row that says the same
//! event — a `[reseat]`, a `[restart]`, an `EMERGENCY —` for a red tip, a stale prompt. Two things
//! un-collapsed it:
//!
//! * the reader's `ctrl-n`. `is_absorbed` asked *is this note retired* and answered *then draw the
//!   row*, so a key whose whole promise is *hide this* swapped one line for the wall it replaced;
//! * any event that replaced the transcript — a `/resync`, a reattach, a switch back. `absorbed`
//!   was keyed by the row's INDEX and cleared at both doors, so every collapse went with it.
//!
//! Both are the same rule now: a row is collapsed while its note's card is on the screen **or** the
//! reader retired that note, and the pairing is keyed by the row's `item_id` so a snapshot that
//! hands back the same rows keeps it.

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

/// **The collapse, as the operator sees it**: one card for the warning and NO row of its own.
///
/// `notice_rows` is the rows a click opens — every agent and System row the frame drew — so a
/// collapsed row is one that is not in it. Asserted both ways, because either half alone is a
/// picture nobody could act on: a card with the row under it is the duplicate R89 exists to end,
/// and neither is the event gone from the screen.
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

/// **`ctrl-n` hides the notice; it does not hand its row back.**
///
/// The key's promise (`/help`): *retire every note this head is showing — hidden, still counted on
/// /status, `/notes` prints them and `/notes restore` brings them back*. Before this, retiring the
/// card put the `[reseat]` row on the screen in its place: the reader asked for the wall to go and
/// got the wall.
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

/// **And it stays retired across the events that replace the transcript.**
///
/// A `/resync` (a `Hello` carrying a fresh snapshot), a reattach and a switch away and back all
/// re-plant the warning — the same `(code, ts, detail)`, so the same key — and none of them may
/// bring back the row the reader retired.
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

/// **A note the reader did NOT retire gives its rows back when its card cannot be drawn** — the
/// rule that must not be lost with the one above.
///
/// A snapshot's warnings are history (`Placed::Before`, R19): a head that attached after the
/// reseat has no card to stand for the row, so the row is the only thing that says the event
/// happened, and hiding it would hide the event.
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
