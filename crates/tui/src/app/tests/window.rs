//! The ctrl-v window: paging an open output, and scrolling past its edges.

use super::*;

/// A payload that fits needs no view, and claiming one would be a lie: the seam
/// would offer a page with nothing behind it.
#[test]
fn a_short_payload_opens_no_view() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a_result_row(&mut a, 2, "i1", "one line\n");
    a.key(Key::CtrlV);
    assert_eq!(
        a.payload_sel, None,
        "a one-line result has nothing to page, so a view on it is a claim"
    );
}

/// **The ctrl-v window does not hold the composer's Enter.** The operator: *"untill i
/// hit ctrl-v again - i couldnt sent my new prompt"*. With a window open and a line
/// typed, Enter sends it and closes the window; with nothing typed it does nothing.
#[test]
fn enter_sends_a_typed_line_past_an_open_ctrl_v_window() {
    let mut a = app();
    let payload: String = (0..60).map(|n| format!("output line {n}\n")).collect();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("r.0", "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "r.0".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "c0".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload,
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
    a.screen(80, 24);
    a.key(Key::CtrlV);
    a.screen(80, 24);
    assert!(a.payload_sel.is_some(), "the window is open");
    assert!(
        a.key(Key::Enter).is_none(),
        "an empty Enter with the window open does nothing"
    );
    assert!(a.payload_sel.is_some(), "and leaves the window open");
    a.set_composer("next prompt");
    let act = a.key(Key::Enter);
    assert!(
        matches!(&act, Some(Action::Prompt(t)) if t == "next prompt"),
        "Enter with the window open did not send the line: {act:?}"
    );
    assert_eq!(a.payload_sel, None, "sending closes the window");
}
