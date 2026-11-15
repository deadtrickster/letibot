//! A thinking marker inside a group of tool cards is drawn as one more card, `  ▸ N thinking
//! lines`, so the group does not read as broken. Between prose it keeps its brackets.

use super::*;

const THOUGHT: &str = "THOUGHT-BETWEEN-CARDS";

fn result(call: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: call.into(),
        name: "read".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload: format!("PAYLOAD-{call}"),
        edits: Vec::new(),
        origin: None,
        media: None,
    }
}

fn said(text: &str, calls: &[&str]) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: text.into(),
        tool_calls: calls
            .iter()
            .map(|id| letibot_transcript::ToolCall {
                id: (*id).into(),
                name: "read".into(),
                arguments: format!("{{\"path\": \"src/{id}.rs\"}}"),
            })
            .collect(),
        truncated: false,
    }
}

fn thought() -> TranscriptItem {
    TranscriptItem::Reasoning {
        text: format!("{THOUGHT}\nsecond line\nthird line"),
        field: letibot_transcript::ReasoningField::ReasoningContent,
        truncated: false,
    }
}

fn session(a: &mut App) {
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
}

/// Prose, a card, a thought with a silent assistant row, a second card: one group with a gap.
fn between_cards() -> App {
    let mut a = app();
    session(&mut a);
    shell_row(&mut a, 1, "s.0", "assistant", said("let me look:", &["c0"]));
    shell_row(&mut a, 3, "s.1", "tool_result", result("c0"));
    shell_row(&mut a, 5, "s.2", "reasoning", thought());
    shell_row(&mut a, 7, "s.3", "assistant", said("", &["c1"]));
    shell_row(&mut a, 9, "s.4", "tool_result", result("c1"));
    a.visibility = Visibility::of(Profile::NORMAL);
    a.invalidate_history();
    a
}

/// Two paragraphs with a thought between them and no card in sight.
fn between_prose() -> App {
    let mut a = app();
    session(&mut a);
    shell_row(&mut a, 1, "s.0", "assistant", said("first paragraph.", &[]));
    shell_row(&mut a, 3, "s.1", "reasoning", thought());
    shell_row(
        &mut a,
        5,
        "s.2",
        "assistant",
        said("second paragraph.", &[]),
    );
    a.visibility = Visibility::of(Profile::NORMAL);
    a.invalidate_history();
    a
}

fn cells(l: &str) -> String {
    crate::app::keyhints::cells(l).into_iter().collect()
}

fn marker_line(screen: &[String]) -> String {
    screen
        .iter()
        .map(|l| cells(l))
        .find(|l| l.contains("thinking line"))
        .unwrap_or_else(|| panic!("no marker:\n{}", screen.join("\n")))
}

#[test]
fn a_thought_between_two_cards_is_drawn_at_the_card_indent() {
    let mut a = between_cards();
    let screen = a.screen(100, 30);
    let line = marker_line(&screen);
    // The column the cards' own `▸` is drawn at.
    let card = screen
        .iter()
        .map(|l| cells(l))
        .find(|l| l.contains("PAYLOAD-c0"))
        .expect("the first card is on screen");
    let col = |l: &str| l.chars().position(|c| c == '▸');
    assert!(
        col(&line) == col(&card) && line.trim() == "▸ 3 thinking lines",
        "the marker is not a card row: {line:?}\n{}",
        screen.join("\n")
    );
    // And the group is not split: no blank line on either side of it.
    let at = screen.iter().position(|l| cells(l) == line).unwrap();
    assert!(
        !cells(&screen[at - 1]).trim().is_empty() && !cells(&screen[at + 1]).trim().is_empty(),
        "the marker broke the group:\n{}",
        screen.join("\n")
    );
}

#[test]
fn a_thought_between_prose_keeps_its_brackets() {
    let mut a = between_prose();
    let line = marker_line(&a.screen(100, 30));
    assert!(line.contains("[3 thinking lines]"), "{line:?}");
    assert!(!line.contains('▸'), "{line:?}");
}

#[test]
fn ctrl_r_still_opens_the_thought_between_cards() {
    let mut a = between_cards();
    assert!(!a.screen(100, 30).join("\n").contains(THOUGHT));
    a.key(Key::CtrlR);
    assert!(a.thinking_open());
    let open = a.screen(100, 30).join("\n");
    assert!(open.contains(THOUGHT), "ctrl-r opened nothing:\n{open}");
    a.key(Key::CtrlR);
    assert!(!a.screen(100, 30).join("\n").contains(THOUGHT));
}

#[test]
fn a_click_on_the_card_marker_opens_the_thought() {
    let mut a = between_cards();
    let screen = a.screen(100, 30);
    let (y, line) = screen
        .iter()
        .enumerate()
        .find(|(_, l)| cells(l).contains("thinking line"))
        .expect("the marker is on screen");
    let x = cells(line).chars().position(|c| c == '3').unwrap();
    a.key(Key::Click {
        x: x as u16 + 1,
        y: y as u16,
    });
    let open = a.screen(100, 30).join("\n");
    assert!(open.contains(THOUGHT), "the click opened nothing:\n{open}");
}
