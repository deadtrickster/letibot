//! The monitors pane: what this session is watching, counted on the composer's edge.

use super::*;
use letibot_sessionlog::protocol::MonitorEntry;

fn attached() -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a
}

fn table(monitors: Vec<MonitorEntry>) -> ServerFrame {
    ServerFrame::Jobs {
        session_id: "s".into(),
        jobs: Vec::new(),
        monitors,
    }
}

fn live(name: &str) -> MonitorEntry {
    MonitorEntry {
        name: name.into(),
        watching: "a timer of 60s".into(),
        owner: "session".into(),
        declared_by: "turn 4".into(),
        repeat: true,
        live: true,
        ends_at_ms: letibot_sessionlog::registry::now_ms() + 600_000,
        fired: 2,
        ended: String::new(),
    }
}

fn ended(name: &str, how: &str) -> MonitorEntry {
    MonitorEntry {
        name: name.into(),
        watching: "job j4".into(),
        live: false,
        ended: how.into(),
        ..MonitorEntry::default()
    }
}

/// **A monitor that is still waiting is on the screen** — counted on the composer's edge, and a
/// click on the count opens its pane and asks for the table. Before this a monitor showed only
/// once it fired; an ended one is not counted.
#[test]
fn a_watching_monitor_is_counted_and_the_count_opens_its_pane() {
    let mut a = attached();
    a.apply(table(vec![
        live("tick"),
        ended("build", "FIRED after 3.0s — exited 0"),
    ]));
    let screen = a.screen(100, 24);
    let hits = a.box_top_hits.expect("the edge was drawn");
    let row = &screen[hits.row];
    assert!(row.contains("1 monitor watching"), "edge row: {row:?}");
    let (col, w) = hits.monitors.expect("its label is a target");
    assert_eq!(
        a.key(Key::Click {
            x: (col + w / 2) as u16,
            y: hits.row as u16,
        }),
        Some(Action::ListJobs),
        "opening the pane asks for the table it draws"
    );
    assert!(a.is_open(Surface::Monitors));
    assert!(!a.is_open(Surface::Jobs), "its own pane, not the jobs pane");
    let pane = a.screen(120, 30).join("\n");
    assert!(pane.contains("▸ [~] tick"), "{pane}");
    assert!(pane.contains("watching a timer of 60s"), "{pane}");
    assert!(pane.contains("repeats · fired 2×"), "{pane}");
    assert!(
        pane.contains("[x] build — FIRED after 3.0s — exited 0"),
        "{pane}"
    );
}

/// **`/monitors` opens it, and `x` retires the one under the cursor** through the daemon's verb,
/// so the record says the operator did it. With nothing live, `x` is the composer's letter.
#[test]
fn x_retires_the_monitor_under_the_cursor() {
    let mut a = attached();
    a.apply(table(vec![live("tick"), live("port")]));
    a.command("monitors");
    assert!(a.is_open(Surface::Monitors));
    a.key(Key::Down);
    assert_eq!(
        a.key(Key::Char('x')),
        Some(Action::Slash {
            line: "monitor retire port".into()
        })
    );
    a.apply(table(vec![ended(
        "port",
        "retired by the operator after 9s",
    )]));
    assert_eq!(a.key(Key::Char('x')), None, "nothing live to retire");
    assert!(
        !a.box_top(100).0.contains("monitor"),
        "an ended monitor is not counted"
    );
}

/// **Enter shows a monitor whole, with the call that armed it** — the operator: *"i want to see
/// monitor command in the details"*, looking at a timer whose row said nothing of what it was for.
#[test]
fn enter_shows_the_monitor_and_the_call_that_armed_it() {
    let mut a = attached();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("r.1", "assistant"),
    )));
    a.record_item(
        "r.1",
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![letibot_transcript::ToolCall {
                id: "c1".into(),
                name: "monitor".into(),
                arguments: r#"{"name":"tick","timer_ms":60000,"repeat":true,"owner":"session"}"#
                    .into(),
            }],
            truncated: false,
        },
    );
    a.apply(table(vec![live("tick")]));
    a.command("monitors");
    a.key(Key::Enter);
    assert!(
        a.is_open(Surface::Listing),
        "the details opened over the pane"
    );
    let shown = a.screen(100, 40).join("\n");
    assert!(shown.contains("watching   a timer of 60s"), "{shown}");
    assert!(shown.contains("the call that armed it:"), "{shown}");
    assert!(shown.contains("\"timer_ms\": 60000"), "{shown}");
    a.key(Key::Esc);
    assert!(a.is_open(Surface::Monitors), "esc comes back to the pane");
}
