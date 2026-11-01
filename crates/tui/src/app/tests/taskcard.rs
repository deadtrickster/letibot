//! **A finished subagent's line is a door to that child.**
//!
//! The operator, 2026-11-01: *"when subagent finished main session gets notification and it is
//! clickable. but the card shown is completely useless - i want the click to bring me to the
//! subagent session instead, so i can read its conclusions"*.
//!
//! The line stays what it was — one line per settlement, the glance — and the click becomes the
//! dive: `Act::Switch`, the gesture `ctrl-s` and a plugin pane's `switch` row already use, so the
//! child's conversation is on the screen and `esc` comes back up to the parent.
//!
//! **The popup is still there** (`alt-n`, the newest notice whole) and every other notice row's
//! click is unchanged. What changed is only what a click on a settlement's own line does.

use super::redcard::{holds, popup_text};
use super::*;

/// A settlement of one child, as `harnessd`'s `subagent_notice` writes it for a session's own
/// children (`TASK_CLOSING` and all): the label the child was started under, its state, its
/// answer, and the promise to the model.
const ONE: &str = "[task] a subagent you started has finished:\n  - `fix-login · s-sub-1` done: all \
                   green\nFull answer: `task_result` task=\"s-sub-1\".";

/// Two children in one wake — two lines, and each is its own child's.
const TWO: &str = "[task] 2 subagents you started have finished:\n  - `fix-login · s-sub-1` done: all \
                   green\n  - `audit · s-sub-2` failed: the cap is spent\nFull answer: \
                   `task_result` task=\"s-sub-1\", task=\"s-sub-2\".";

/// **The same settlement for a child that failed** — reading *why* it failed is exactly what its
/// own session holds, so a failure is the case the switch matters most in.
const FAILED: &str = "[task] a subagent you started has finished:\n  - `fix-login · s-sub-1` failed: \
                      the cap is spent before the first read\nFull answer: `task_result` \
                      task=\"s-sub-1\".";

/// A head in `s`, holding the daemon's list of its own two children, with `text` as the one row
/// the session spoke in the operator's place.
fn with_settlement(text: &str) -> App {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    shell_row(
        &mut a,
        1,
        "s.0",
        "user",
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text: text.into() }],
        },
    );
    a
}

/// The screen row the settlement's line was drawn on, and the child the frame recorded for it.
fn drawn(a: &mut App) -> (usize, String) {
    let screen = a.screen(160, 30);
    let (y, k) = *a
        .task_rows
        .first()
        .unwrap_or_else(|| panic!("no settlement row was recorded:\n{}", screen.join("\n")));
    (y, a.task_switches[k].clone())
}

/// **The line records the child it is about, as data** — the id is carried out of the fold that
/// drew the line, so a click never has to read the row back to find out which child it is for.
#[test]
fn a_settlements_line_records_the_child_it_names() {
    let mut a = with_settlement(ONE);
    let screen = a.screen(160, 30);
    let joined = screen.join("\n");
    assert!(
        joined.contains("Agent fix-login"),
        "the premise: the line names the child by its label: {joined}"
    );
    assert_eq!(a.task_rows.len(), 1, "{:?}", a.task_rows);
    assert_eq!(
        a.task_switches,
        vec!["s-sub-1".to_string()],
        "the id the notice's bullet names, carried as data"
    );
    // And the row is still a notice — its text is the daemon's, and `alt-n` opens it whole.
    assert_eq!(a.notice_rows.len(), 1, "{:?}", a.notice_rows);
    assert_eq!(newest_notice(&mut a), ONE);
}

/// **A click goes to the child's session**, not to a popup of the notice the model already read.
#[test]
fn a_click_on_the_line_switches_to_the_child() {
    let mut a = with_settlement(ONE);
    let (y, child) = drawn(&mut a);
    assert_eq!(child, "s-sub-1");
    assert_eq!(
        a.key(Key::Click { x: 10, y: y as u16 }),
        Some(Action::Switch("s-sub-1".into())),
        "the click is the session list's own gesture"
    );
    assert!(
        a.diff_popup.is_none(),
        "the click opened the popup instead of the child's conversation"
    );
}

/// **Esc comes back up** — a switch from a card is not a one-way door, because it is the same
/// door `ctrl-s` uses: the child's `parent_session_id` is what the head reads.
#[test]
fn esc_comes_back_to_the_session_the_card_was_in() {
    let mut a = with_settlement(ONE);
    let (y, _) = drawn(&mut a);
    assert_eq!(
        a.key(Key::Click { x: 10, y: y as u16 }),
        Some(Action::Switch("s-sub-1".into()))
    );
    // The switch answered: this head is now seated in the child, with the same list in hand.
    a.apply(hello("s-sub-1", a_family(), Hub::new("s-sub-1").snapshot()));
    assert_eq!(
        a.key(Key::Esc),
        Some(Action::Switch("s".into())),
        "esc goes up to the parent, as it does from the session list"
    );
}

/// **Two children in one wake are two doors** — each line names its own child, and a click on
/// one does not go to the other.
#[test]
fn each_childs_line_switches_to_that_child() {
    let mut a = with_settlement(TWO);
    let screen = a.screen(160, 30);
    let joined = screen.join("\n");
    assert!(joined.contains("Agent fix-login"), "{joined}");
    assert!(joined.contains("Agent audit"), "{joined}");
    assert_eq!(
        a.task_switches,
        vec!["s-sub-1".to_string(), "s-sub-2".to_string()]
    );
    let rows = a.task_rows.clone();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let (first, _) = rows[0];
    assert_eq!(
        a.key(Key::Click {
            x: 10,
            y: first as u16
        }),
        Some(Action::Switch("s-sub-1".into()))
    );
    let (second, _) = rows[1];
    assert_eq!(
        a.key(Key::Click {
            x: 10,
            y: second as u16
        }),
        Some(Action::Switch("s-sub-2".into())),
        "the second line is the second child's"
    );
}

/// **A child that failed switches too** — its session is where the reason it failed is.
#[test]
fn a_failed_childs_line_switches_to_it() {
    let mut a = with_settlement(FAILED);
    let screen = a.screen(160, 30);
    assert!(
        screen.join("\n").contains("failed: the cap is spent"),
        "the premise: the line says it failed"
    );
    let (y, child) = drawn(&mut a);
    assert_eq!(child, "s-sub-1");
    assert_eq!(
        a.key(Key::Click { x: 10, y: y as u16 }),
        Some(Action::Switch("s-sub-1".into()))
    );
    assert!(a.diff_popup.is_none());
}

/// **A child the daemon has finished but still knows is resumed, not refused.** The settlement
/// arrives *because* the child finished, so its session is the one case where the switch is
/// usually to something the daemon no longer holds open — and `switch_to` brings it back from
/// the store rather than telling the operator it is gone.
#[test]
fn a_finished_child_is_resumed_from_the_store() {
    let mut a = app();
    let mut child = brief("s-sub-1", "find the bug in the reader", false);
    child.parent_session_id = Some("s".into());
    child.live = false;
    a.apply(hello(
        "s",
        vec![brief("s", "parent", false), child],
        Hub::new("s").snapshot(),
    ));
    shell_row(
        &mut a,
        1,
        "s.0",
        "user",
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text: ONE.into() }],
        },
    );
    let (y, child) = drawn(&mut a);
    assert_eq!(child, "s-sub-1");
    assert_eq!(
        a.key(Key::Click { x: 10, y: y as u16 }),
        Some(Action::ResumeSession("s-sub-1".into())),
        "a child the store holds is resumed, not refused"
    );
}

/// **A child this head can no longer name keeps the popup** — a card whose subagent is gone is
/// not a switch to nothing. The same notice drawn by a head that does not hold the child.
#[test]
fn a_card_whose_child_is_gone_keeps_the_popup() {
    let mut a = app();
    // The daemon's list holds this session and no child of it at all.
    a.apply(hello(
        "s",
        vec![brief("s", "parent", false)],
        Hub::new("s").snapshot(),
    ));
    shell_row(
        &mut a,
        1,
        "s.0",
        "user",
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text: ONE.into() }],
        },
    );
    let screen = a.screen(160, 30);
    assert_eq!(
        a.task_rows.len(),
        0,
        "a row whose child is gone is not a switch: {}\n{:?}",
        screen.join("\n"),
        a.task_rows
    );
    let (y, _) = a.notice_rows[0];
    a.key(Key::Click { x: 10, y: y as u16 });
    assert!(
        a.diff_popup.is_some(),
        "the click is the popup it always was"
    );
    assert!(
        holds(&popup_text(&mut a), "Full answer: `task_result`"),
        "the popup is the whole notice, the sentence the line does not draw included"
    );
}
