//! **A round's counts are drawn after its reply** — the operator: *"we still have problems with
//! [n tools called, m thinking lines] they often appear above the current reply or backfilled"*,
//! and their ruling on where they go: *all of it after the reply*.

use super::*;

fn conversation() -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a
}

fn said(a: &mut App, seq: u64, id: &str, text: &str) {
    a.apply(ServerFrame::Event(env(
        seq,
        testing::appended(id, "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        seq + 1,
        SessionEvent::TranscriptContent {
            item_id: id.into(),
            item: Box::new(TranscriptItem::Assistant {
                text: text.into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
}

fn result(a: &mut App, seq: u64, id: &str, call: &str) {
    a.apply(ServerFrame::Event(env(
        seq,
        testing::proposed_on("t1", call, "bash", "\"cargo test\""),
    )));
    a.apply(ServerFrame::Event(env(
        seq + 1,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: call.into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "d".into(),
            inline_bytes: 1,
            full_bytes: 1,
            spill: None,
            repairs: 0,
            edit: None,
        },
    )));
    a.apply(ServerFrame::Event(env(
        seq + 2,
        testing::appended(id, "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        seq + 3,
        SessionEvent::TranscriptContent {
            item_id: id.into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: call.into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "ok".into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
}

fn line_of(screen: &[String], needle: &str) -> usize {
    screen
        .iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{}", screen.join("\n")))
}

/// **Work after the model's reply is drawn after it, and the marker above keeps its own count.**
#[test]
fn work_after_a_reply_is_drawn_below_it_and_not_folded_into_the_run_above() {
    let mut a = conversation();
    said(&mut a, 2, "s.0", "Looking:");
    result(&mut a, 4, "s.1", "c1");
    said(&mut a, 8, "s.2", "here is what I found:");
    // The next round's call, in flight.
    a.apply(ServerFrame::Event(env(
        10,
        testing::proposed_on("t1", "c2", "bash", "\"cargo build\""),
    )));
    let screen = a.screen(120, 30);
    let text = screen.join("\n");
    let reply = line_of(&screen, "here is what I found:");
    let above = line_of(&screen, "Looking:");
    assert!(
        screen[above].contains("[1 tool call]"),
        "the marker above keeps its own one call:\n{text}"
    );
    let live = screen
        .iter()
        .rposition(|l| l.contains("tool call"))
        .expect("the work in flight is drawn");
    assert!(
        live >= reply,
        "the work in flight is at or below the reply:\n{text}"
    );
    assert_ne!(live, above, "and not folded into the marker above:\n{text}");
}

/// **While the reply is still streaming, the counts sit below it.**
#[test]
fn the_live_counts_sit_below_the_reply_that_is_streaming() {
    let mut a = conversation();
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Text,
            text: "Now I will run the build:".into(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        3,
        testing::proposed_on("t1", "c1", "bash", "\"cargo build\""),
    )));
    let screen = a.screen(120, 30);
    let text = screen.join("\n");
    let reply = line_of(&screen, "Now I will run the build:");
    let marker = line_of(&screen, "tool call");
    assert!(marker >= reply, "the counts are below the reply:\n{text}");
}

/// **A row landing in a later round of a run re-counts the marker the run's first round drew** —
/// the *"backfilled"* half: the marker sits on an earlier round's prose line, and a landing row
/// rebuilt only from its own round's head, so the number stayed stale until something unrelated
/// rebuilt the line.
#[test]
fn a_row_landing_later_in_a_run_recounts_its_marker_at_once() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    let mut seq = 1;
    let mut row = |a: &mut App, id: &str, kind: &str, item: TranscriptItem| {
        a.apply(ServerFrame::Event(env(seq, testing::appended(id, kind))));
        a.apply(ServerFrame::Event(env(
            seq + 1,
            SessionEvent::TranscriptContent {
                item_id: id.into(),
                item: Box::new(item),
            },
        )));
        seq += 2;
    };
    let assistant = |text: &str| TranscriptItem::Assistant {
        text: text.into(),
        tool_calls: Vec::new(),
        truncated: false,
    };
    let result = |call: &str| TranscriptItem::ToolResult {
        call_id: call.into(),
        name: "bash".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload: "ok".into(),
        edit: None,
        origin: None,
        media: None,
    };
    row(&mut a, "s.0", "assistant", assistant("Looking:"));
    row(&mut a, "s.1", "tool_result", result("c1"));
    // The second round: a call-only assistant row (no prose), and its result.
    row(&mut a, "s.2", "assistant", assistant(""));
    row(&mut a, "s.3", "tool_result", result("c2"));
    let screen = a.screen(120, 30);
    let line = screen
        .iter()
        .find(|l| l.contains("Looking:"))
        .unwrap()
        .clone();
    assert!(line.contains("[2 tool calls]"), "the premise: {line}");
    // A later round's thinking lands — no turn is live, so nothing but the row can move it.
    row(&mut a, "s.4", "assistant", assistant(""));
    row(
        &mut a,
        "s.5",
        "reasoning",
        TranscriptItem::Reasoning {
            text: "one more look".into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    let screen = a.screen(120, 30);
    let line = screen
        .iter()
        .find(|l| l.contains("Looking:"))
        .unwrap()
        .clone();
    assert!(
        line.contains("thinking"),
        "the marker counts the row that landed, at once: {line}"
    );
}
