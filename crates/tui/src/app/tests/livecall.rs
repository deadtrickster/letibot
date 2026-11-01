//! **A call the turn is still drawing is a door** — the operator, 2026-11-01: *"why running tool
//! call rendered in yellow is no longer clickable? it is similar degradation that happened to git
//! status"*.
//!
//! The precedent is the header's git field, and the shape is the same one: a thing is DRAWN and
//! the click map has no rectangle for it, so a click falls through to whatever is behind and
//! answers nothing. Here the thing is a call in flight, and the shape is **the counter**:
//! `[1 tool call]`, glued to the sentence that introduced the call (`live_joins`), at the rungs
//! that hide the working. `marker_rows_in` resolved a clause's run by finding the first hidden row
//! at or after the row it sits on, and the work in flight has no row at all — so the lookup
//! answered `None` and the clause was dropped. The counter was a door only when a hidden row
//! happened to exist under it, which is why it read as *sometimes it clicks, sometimes it does
//! not*.
//!
//! And the clause's columns were the row's own while a click's `x` is a terminal cell: the frame
//! pads every row with the gutter after composing it, so the record sat two cells left of what it
//! drew — the git field's defect, in the same file, one `+ gutter`.

use super::*;

/// A turn one round deep: the operator asked, the model narrated, one call is out and has not
/// returned. Nothing of the call is a transcript row — that is the state the counter has no run
/// under it.
fn a_call_in_flight(name: &str) -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env_at(1, 1_000, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(2, testing::appended("s.0", "user"))));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![UserPart::Text {
                    text: "run the tests".into(),
                }],
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        testing::appended("s.1", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::TranscriptContent {
            item_id: "s.1".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "running them now:".into(),
                tool_calls: vec![letibot_transcript::ToolCall {
                    id: "c0".into(),
                    name: name.into(),
                    arguments: "{\"command\": \"cargo test\"}".into(),
                }],
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env_at(
        6,
        1_100,
        testing::proposed("t1", "c0", name),
    )));
    a.apply(ServerFrame::Event(env_at(
        7,
        1_200,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c0".into(),
            name: name.into(),
            access: Default::default(),
        },
    )));
    a.invalidate_history();
    a
}

/// The change a finished `edit` hands back — one line rewritten.


/// Where `needle` starts on the screen, as (x, y) in terminal cells.
fn find(screen: &[String], needle: &str) -> (u16, u16) {
    for (y, l) in screen.iter().enumerate() {
        let cells: String = crate::app::keyhints::cells(l).into_iter().collect();
        if let Some(x) = cells.find(needle) {
            let x = cells[..x].chars().count();
            return (x as u16, y as u16);
        }
    }
    panic!("{needle:?} not on screen:\n{}", screen.join("\n"));
}

/// **The marker clause drawn on screen row `y`** — a `MarkerHit`'s `y` is a row of the window and
/// the conversation starts below the header, so the two are not the same number (the shift every
/// `*_rows` record gets in `main_area`).
fn clause_on(a: &App, y: u16) -> Option<&crate::app::runclick::MarkerHit> {
    let top = a
        .hits
        .iter()
        .find(|h| h.target == Target::Conversation)?
        .rect
        .y;
    a.marker_rows
        .iter()
        .find(|m| m.y == usize::from(y) - top)
}

/// **The counter with nothing under it is a door** — the operator's *"running tool call rendered
/// in yellow is no longer clickable"*, in the state where it is the only thing on the screen that
/// stands for the call.
///
/// It fails on the tree before this change, and it fails at the first assertion: `marker_rows` is
/// empty, because the clause's run was resolved by looking for a hidden row and the work in
/// flight has none. The click then falls through the conversation's rectangle and nothing at all
/// happens — no panel, no sentence.
#[test]
fn the_counter_of_a_call_with_no_row_under_it_opens_the_calls() {
    let mut a = a_call_in_flight("bash");
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let screen = a.screen(120, 30);
    let (x, y) = find(&screen, "tool call");
    // The record the click is answered from: the clause, on the row it was drawn on.
    let clause = clause_on(&a, y).unwrap_or_else(|| {
        panic!(
            "the counter is drawn and not in the click map: {:?}",
            a.marker_rows
        )
    });
    assert_eq!(clause.show, Some(Show::Tools), "the clause names the calls");
    assert_eq!(
        clause.run,
        crate::app::runclick::LIVE_RUN,
        "nothing hidden under it, so the run it stands for is the work in flight"
    );
    // And the click opens that work: the card the rung was hiding, and the way out of it.
    a.key(Key::Click { x: x + 1, y });
    let open = a.screen(120, 30).join("\n");
    assert!(
        open.contains("Running"),
        "the panel is empty while the call runs:\n{open}"
    );
    assert!(
        open.contains("esc folds the run back"),
        "the panel names no way out:\n{open}"
    );
}

/// **A click on the counter lands where the counter is drawn** — the defect the git field had, in
/// the same file: the clause's columns are the row's own and a click's `x` is a terminal cell, and
/// the frame pads every row with the gutter after composing it.
///
/// Fails on the tree before this change in the second half: the record sat two columns left of
/// what it drew, so a click on the clause's last drawn column landed past its end and answered
/// nothing.
#[test]
fn the_counters_columns_are_the_cells_it_was_drawn_in() {
    let mut a = a_call_in_flight("bash");
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let screen = a.screen(120, 30);
    let (_, y) = find(&screen, "tool call");
    let clause = clause_on(&a, y)
        .cloned()
        .expect("the clause is in the click map");
    let gutter = App::gutter(120);
    // The words the record covers are the words on the glass: `tool call`, less the count's own
    // `1 ` in front of it.
    let cells: String = crate::app::keyhints::cells(&screen[y as usize])
        .into_iter()
        .collect();
    let drawn = &cells[clause.from + gutter..clause.to + gutter];
    assert_eq!(drawn, "1 tool call", "the record is not on its own words");
    // The LAST column of the clause is inside it — the click that used to fall off the end.
    let mut b = a_call_in_flight("bash");
    b.visibility = Visibility::of(Profile::CONVERSATION);
    b.invalidate_history();
    b.screen(120, 30);
    b.key(Key::Click {
        x: (clause.to + gutter - 1) as u16,
        y,
    });
    assert!(
        b.payload_sel.is_some(),
        "a click on the clause's last column opens nothing"
    );
    // And the two cells the gutter put in front of it are not the clause.
    let mut c = a_call_in_flight("bash");
    c.visibility = Visibility::of(Profile::CONVERSATION);
    c.invalidate_history();
    c.screen(120, 30);
    c.key(Key::Click {
        x: (clause.from + gutter - 1) as u16,
        y,
    });
    assert!(
        c.payload_sel.is_none(),
        "the cell before the clause is the clause"
    );
}
