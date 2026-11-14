//! Every line that says `click for details` opens its details on a click, and shows the hand
//! while the pointer is over it. See `ui::transcript::line_cards::CLICK`.

use super::redcard::{popup_text, with_warning};
use super::*;
use crate::ui::transcript::line_cards::CLICK;
use letibot_transcript::{NoticeKind, OfferKind, Speaker, SystemOrigin};

/// A head attached to `s` with `item` as its one row.
fn with_item(item: TranscriptItem) -> App {
    let hub = Hub::new("s");
    hub.append_row("s#t1:0", "row", "00", &item);
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    a
}

fn agent(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Speaker::Agent,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

/// Every row the frame draws with the legend, as the cell its words start at.
fn legend_cells(screen: &[String]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    for (y, l) in screen.iter().enumerate() {
        let cells: String = crate::app::keyhints::cells(l).into_iter().collect();
        if let Some(at) = cells.find(CLICK) {
            out.push((cells[..at].chars().count() as u16, y as u16));
        }
    }
    out
}

/// The legend is there, the hand shows over it, a click opens a popup with text in it, and the
/// plain cell at the corner shows the default again.
fn every_legend_opens(mut a: App, what: &str) {
    let screen = a.screen(120, 30);
    let legends = legend_cells(&screen);
    assert!(
        !legends.is_empty(),
        "{what}: no row says `{CLICK}`:\n{}",
        screen.join("\n")
    );
    for (x, y) in legends {
        a.key(Key::Hover { x, y });
        a.screen(120, 30);
        assert_eq!(a.take_pointer(), Some(true), "{what}: no hand at ({x},{y})");
        a.key(Key::Click { x, y });
        assert!(
            a.diff_popup.is_some(),
            "{what}: a click on `{CLICK}` at ({x},{y}) opened nothing"
        );
        assert!(
            !popup_text(&mut a).trim().is_empty(),
            "{what}: the popup has no detail"
        );
        a.key(Key::Esc);
        assert!(a.diff_popup.is_none(), "{what}: esc left the popup up");
        a.screen(120, 30);
        a.key(Key::Hover { x: 0, y: 0 });
        a.screen(120, 30);
        a.take_pointer();
    }
}

/// One text per session card reader and fixture the head draws.
fn session_texts() -> Vec<(&'static str, String)> {
    let mut v: Vec<(&'static str, String)> =
        vec![
        (
            "monitor, one line a watch",
            "[monitor] `ci` fired: the run exited 0.\n[monitor] `log` expired without firing."
                .into(),
        ),
        (
            "monitor, folded",
            "[monitor] 1 watch(es) fired:\n  - `ci` (command `gh run watch` exiting), declared by \
             turn s#t8#226: FIRED after 0.3s - the command exited 2\nOnly a FIRED watch is an \
             answer about the world."
                .into(),
        ),
        (
            "parent message",
            "message from your parent session `s-g`: Two new constraints, fold them in.\n\n1. A"
                .into(),
        ),
        (
            "intent check",
            "[intent check] this turn declared 6 item(s); 0 tool call(s) succeeded and 0 ran \
             without producing a result.\n  - #1 \"x\" was declared\nboard: 0 of 6 complete - 0 in \
             progress"
                .into(),
        ),
        (
            "context check",
            "[context check] you said \"my context is nearly out\". Your context is 54% full - \
             547,198 of 999,999 tokens. That is not the wall, ... Continue."
                .into(),
        ),
        (
            "progress",
            "The harness is watching what your tool calls return, not how many you make, and the \
             last 4 rounds ran 8 calls and 0 of 8 returned a result this turn had not already \
             seen (8 ok)."
                .into(),
        ),
        (
            "deliverable",
            "[the deliverable] Your worktree `/w` has uncommitted changes, so the branch \
             `agent/x` does not hold them: ?? a.jsonl. The branch is the deliverable."
                .into(),
        ),
        (
            "answer",
            "[answer] on \"GAP\": you asked \"Postpone?\" - the operator chose \"Postpone it\""
                .into(),
        ),
        (
            "reseat",
            "[reseat] The person re-seated this conversation with `/reseat`: the conversation \
             was carried across as it was, nothing summarised. You can now call: web_fetch. The \
             notice no longer applies."
                .into(),
        ),
        (
            "overthinking",
            "You keep overthinking. Cut it short. Your last turn spent its whole output on \
             reasoning (reasoning_only) and nothing was recorded."
                .into(),
        ),
        ("unknown tag", "[something new] the daemon learned this. Second.".into()),
        ("unknown prose", "A sentence no reader knows; it goes on.".into()),
    ];
    let fixtures: [(&'static str, &str); 13] = [
        ("red", include_str!("fixtures/red_main_notice.txt")),
        (
            "killed",
            include_str!("fixtures/unjudged_main_killed_notice.txt"),
        ),
        ("asked", include_str!("fixtures/asked_question_row.txt")),
        (
            "asked v1",
            include_str!("fixtures/asked_question_row_v1.txt"),
        ),
        (
            "restart restored",
            include_str!("fixtures/restart_restored_notice.txt"),
        ),
        (
            "restart nothing",
            include_str!("fixtures/restart_nothing_notice.txt"),
        ),
        (
            "restart jobs",
            include_str!("fixtures/restart_jobs_only_notice.txt"),
        ),
        (
            "restart deliveries",
            include_str!("fixtures/restart_with_deliveries_notice.txt"),
        ),
        (
            "review cannot decide",
            include_str!("fixtures/review_cannot_decide_notice.txt"),
        ),
        (
            "review blocked",
            include_str!("fixtures/review_blocked_notice.txt"),
        ),
        (
            "review handed over",
            include_str!("fixtures/review_handed_over_notice.txt"),
        ),
        (
            "review refused",
            include_str!("fixtures/review_refused_message.txt"),
        ),
        (
            "review vetoed",
            include_str!("fixtures/review_vetoed_message.txt"),
        ),
    ];
    v.extend(fixtures.into_iter().map(|(n, t)| (n, t.to_string())));
    v
}

#[test]
fn every_session_card_opens_on_a_click_whichever_row_carried_it() {
    // `loud`, which draws every card - the operator's quiet restart included.
    let loud = |mut a: App| {
        a.visibility = Visibility::of(Profile::LOUD);
        a
    };
    for (name, text) in session_texts() {
        every_legend_opens(
            loud(with_item(agent(&text))),
            &format!("{name} (agent row)"),
        );
        every_legend_opens(
            with_item(TranscriptItem::Notice {
                kind: NoticeKind::Monitor,
                text: text.clone(),
                facts: None,
            }),
            &format!("{name} (notice row)"),
        );
    }
}

#[test]
fn a_system_row_and_an_offer_open_on_a_click() {
    every_legend_opens(
        with_item(TranscriptItem::System {
            text: format!("This conversation was compacted: {}", "x".repeat(2000)),
            origin: SystemOrigin::Bootstrap,
        }),
        "system row",
    );
    for kind in [OfferKind::Note, OfferKind::Skill] {
        every_legend_opens(
            with_item(TranscriptItem::Offer {
                kind,
                title: "skill offered: release (0.30)".into(),
                body: "the body".into(),
            }),
            "offer row",
        );
    }
}

/// A warning is not a row of the conversation, so there is no legend to click: the sentence is
/// opened from `/notes`, which is the pane's own key. This test used to walk every warning card's
/// legend; the walk it pins now is the one `/notes` needs, so it asserts the sentence is there.
#[test]
fn a_warning_is_opened_from_notes_not_from_a_row() {
    for (code, detail) in [
        (
            "prefix_stale",
            include_str!("fixtures/prefix_stale_warning.txt"),
        ),
        (
            "queue_blocked",
            include_str!("fixtures/review_blocked_after_warning.txt"),
        ),
        (
            "daemon_stopping",
            "`dead` asked this daemon to stop. Every head detaches.",
        ),
    ] {
        let mut a = with_warning(code, detail);
        let screen = a.screen(120, 30);
        let joined = screen.join("\n");
        assert!(
            !joined.contains(code) || !joined.contains(CLICK),
            "`{code}` drew a row with a legend: {joined}"
        );
        let listed = a.notes_lines().join("\n");
        assert!(
            listed.contains(code),
            "`{code}` is not in `/notes`: {listed}"
        );
    }
}

/// Plain text is not a door: the default pointer over a reply.
#[test]
fn text_that_opens_nothing_shows_the_default_pointer() {
    let mut a = with_item(TranscriptItem::Assistant {
        text: "just a reply".into(),
        tool_calls: Vec::new(),
        truncated: false,
    });
    let screen = a.screen(120, 30);
    let y = screen
        .iter()
        .position(|l| l.contains("just a reply"))
        .unwrap();
    a.key(Key::Hover { x: 4, y: y as u16 });
    a.screen(120, 30);
    assert_eq!(a.take_pointer(), None, "the hand over plain text");
}
