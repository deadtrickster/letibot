//! **R69: a click and a wheel notch go to what the last frame drew under the pointer** — every
//! clickable surface clicked against the frame it was drawn in, the wheel routed by position,
//! a click that hits nothing falling through, and a click in the same read as a scroll.

use super::*;
use letibot_sessionlog::protocol::{NoteEntry, NoteForm};

/// The screen row `needle` was drawn on — the row a person aims at.
fn row_of(frame: &[String], needle: &str) -> u16 {
    frame
        .iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is drawn: {frame:#?}")) as u16
}

/// Click the row `needle` is drawn on, in the frame just drawn.
fn click_on(a: &mut App, needle: &str) {
    let frame = a.screen(110, 30);
    let y = row_of(&frame, needle);
    a.key(Key::Click { x: 12, y });
}

/// The decoder's own reading of a wheel report at 1-based `(col, row)`, as the terminal sends it.
fn wheel(up: bool, col: u16, row: u16) -> Key {
    let b = format!("\x1b[<{};{col};{row}M", if up { 64 } else { 65 });
    let mut keys: Vec<Key> = rano::term::decode(b.as_bytes())
        .into_iter()
        .filter_map(key_of)
        .collect();
    assert_eq!(keys.len(), 1, "{keys:?}");
    keys.remove(0)
}

fn running(a: &mut App, seq: u64, id: &str, prompt: &str) {
    a.apply(ServerFrame::Event(env(
        seq,
        SessionEvent::Subagent {
            subagent_id: id.into(),
            state: "running".into(),
            prompt: prompt.into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
}

/// **The surfaces that had row maps and took no clicks**: the subagents, jobs, standing and
/// config panes. A click moves the cursor to the row drawn under the pointer; Enter still acts.
#[test]
fn a_click_on_each_pane_selects_the_row_drawn_under_it() {
    // Subagents.
    let mut a = app();
    running(&mut a, 1, "s-sub-1", "first child's prompt");
    running(&mut a, 2, "s-sub-2", "second child's prompt");
    a.key(Key::CtrlG);
    click_on(&mut a, "second child's prompt");
    assert_eq!(a.subagents_sel, 1, "the subagents pane took the click");
    assert_eq!(a.key(Key::Enter), Some(Action::Switch("s-sub-2".into())));

    // Jobs.
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(jobs_frame(
        "s1",
        vec![
            daemon_job("j1", "cargo test --workspace", true),
            daemon_job("j2", "make docs", true),
        ],
    ));
    a.key(Key::CtrlQ);
    click_on(&mut a, "make docs");
    assert_eq!(a.jobs_sel, 1, "the jobs pane took the click");

    // Standing notes.
    let mut a = app();
    let note = |p: &str| NoteEntry {
        path: p.into(),
        abstract_line: None,
        abstract_written: false,
        form: NoteForm::Verbatim,
    };
    a.standing = vec![note("/notes/alpha.md"), note("/notes/beta.md")];
    a.push_surface(Surface::Standing);
    click_on(&mut a, "beta.md");
    assert_eq!(a.standing_sel, 1, "the standing pane took the click");

    // Config: the head's own rows are always there.
    let mut a = app();
    a.command("config");
    click_on(&mut a, "verbosity");
    assert_eq!(
        a.config_rows()[a.config_sel].key,
        "verbosity",
        "the config pane took the click"
    );
}

/// **The decision card and the quit card take clicks on their rows**, read against where the
/// frame put them — below whatever chrome was pushed above the card.
#[test]
fn a_click_on_the_decision_ladder_and_the_quit_card_selects_that_row() {
    let mut a = tall_card(false);
    click_on(&mut a, "(deny)");
    assert_eq!(a.sel, 2, "the third rung was clicked");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Answer {
            req_id: "d1".into(),
            option_id: "deny".into(),
            pattern: None,
            note: None,
        }),
        "select and confirm stay two acts"
    );

    let mut a = app();
    a.key(Key::CtrlC);
    a.key(Key::CtrlC);
    assert!(a.quit_card, "the second ctrl-c opens the card");
    click_on(&mut a, "leave and stop the daemon");
    assert_eq!(a.quit_sel, 1);
}

/// **A click on a wrapped rung's later row selects that rung.** The rows are rano's
/// (`DecisionCard::choice_rows`); what is this head's is placing them under the card's content
/// and the chrome above it. Long labels at a narrow width put each option's id on a row after
/// its first, which is the row a miscounted offset would give to the wrong rung.
#[test]
fn a_click_on_a_wrapped_rung_selects_that_rung() {
    let mut a = tall_card(false);
    for o in &mut a.open[0].options {
        o.label = "a label long enough to wrap the rung onto a second row".into();
    }
    let ids: Vec<String> = a.open[0]
        .options
        .iter()
        .map(|o| o.option_id.clone())
        .collect();
    for (k, id) in ids.iter().enumerate().rev() {
        let frame = a.screen(50, 30);
        let y = row_of(&frame, &format!("({id})"));
        assert!(
            !frame[usize::from(y)].contains("a label"),
            "{id:?} is on its rung's first row, so this tests no wrap: {frame:#?}"
        );
        a.key(Key::Click { x: 12, y });
        assert_eq!(a.sel, k, "the click on {id:?} selected rung {}", a.sel);
    }
}

/// **A click that lands on nothing a pane drew falls through.** The todos pane took every
/// click, hit or miss, so the count labels on the composer's edge stopped working while it was
/// open — and worked while the jobs or subagents pane was.
#[test]
fn a_click_outside_the_pane_reaches_the_count_label() {
    let mut a = app();
    running(&mut a, 1, "s-sub-1", "a child");
    a.key(Key::CtrlT);
    let frame = a.screen(110, 30);
    let y = row_of(&frame, "1 subagent");
    let line = &frame[usize::from(y)];
    // In cells, not bytes: the edge is drawn in box characters.
    let x = line[..line.find("1 subagent").expect("drawn")]
        .chars()
        .count() as u16;
    a.key(Key::Click { x: x + 1, y });
    assert_eq!(
        a.top_surface(),
        Some(Surface::Subagents),
        "the label's click was swallowed by the todos pane"
    );
}

/// **The wheel scrolls what is under the pointer.** A decision card with overflowing content
/// took every notch anywhere on the screen, so a reader scrolling the transcript moved the
/// card's evidence instead.
#[test]
fn the_wheel_over_the_transcript_scrolls_the_transcript_not_the_card() {
    let mut a = tall_card(true);
    for i in 0..40u64 {
        a.apply(ServerFrame::Event(env(
            i * 2 + 10,
            testing::appended(&format!("s.{i}"), "assistant"),
        )));
        a.apply(ServerFrame::Event(env(
            i * 2 + 11,
            testing::content(&format!("s.{i}"), &format!("row {i} says a thing")),
        )));
    }
    let _ = a.screen(100, 30);
    assert!(
        a.dec_content_len > a.dec_content_room,
        "the premise: the card's content overflows"
    );
    // The first row, column 10: the conversation, above the card.
    a.key(wheel(true, 10, 1));
    assert_eq!(
        a.dec_scroll, 0,
        "the card moved under a notch aimed elsewhere"
    );
    assert!(!a.following(), "the transcript did not move");
}

/// **And over the card, the card scrolls** — the same notch, aimed at it.
#[test]
fn the_wheel_over_the_card_scrolls_the_card() {
    let mut a = tall_card(true);
    let frame = a.screen(100, 30);
    // 1-based for the report: the row of the card's first evidence line.
    let y = row_of(&frame, "line 0 of layer A");
    a.key(wheel(false, 10, y + 1));
    assert!(a.dec_scroll > 0, "the notch over the card did not move it");
}

/// **A click in the same read as a wheel step is measured against the frame the operator saw.**
/// The todos and queue panes combined the UPDATED scroll with the last frame's stop rows, so
/// trackpad momentum followed by a click selected a row three off.
#[test]
fn a_click_after_a_wheel_step_in_the_same_read_picks_the_row_that_was_drawn() {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoStatus};
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let rows = (0..40)
        .map(|i| TodoEntry {
            by: TodoBy::Operator,
            content: format!("row number {i:02}"),
            status: TodoStatus::Pending,
            when: None,
            needs: Vec::new(),
        })
        .collect();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TodosUpdated { todos: rows },
    )));
    a.key(Key::CtrlT);
    let frame = a.screen(110, 30);
    let y = row_of(&frame, "row number 05");
    let aimed = a
        .todos_stops()
        .iter()
        .position(|s| matches!(s, TodoStop::Mine(c) if c == "row number 05"))
        .expect("the row is a stop");
    // One read: a notch, then the click — no frame between them.
    a.key(Key::WheelDown);
    assert!(a.pane_scroll > 0, "the premise: the notch moved the pane");
    a.key(Key::Click { x: 12, y });
    assert_eq!(
        a.todos_sel, aimed,
        "the click landed on a row nobody saw there"
    );
}
