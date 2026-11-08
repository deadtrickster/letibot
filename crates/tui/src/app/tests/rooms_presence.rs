//! Derived presence in the head: the coordinator's worded seat state under each roster member and
//! on the Sessions rows, and the rosters asked for so the Sessions rows have it.

use super::rooms::{opened, ops, reply, rooms_reply, screen};
use super::*;
use serde_json::json;

fn row_with<'a>(rows: &'a [String], needle: &str) -> &'a str {
    rows.iter()
        .find(|r| r.contains(needle))
        .unwrap_or_else(|| panic!("no row has {needle:?}:\n{}", rows.join("\n")))
}

fn host_with_seats(a: &mut App) {
    reply(a, rooms_reply(&[("host", 2, 0, 0)]));
    reply(
        a,
        json!({"type": "roster", "room": "host", "rows": [
            {"name": "alpha", "id": "s-x", "kind": "session", "connected": true, "project": "",
             "label": "alpha", "seat": {"state": "in_turn", "since_ms": 1, "connected": true,
             "says": "in turn 3m", "doing": "letibot@z-r163"}},
            {"name": "beta", "id": "s-y", "kind": "session", "connected": true, "project": "",
             "label": "beta", "seat": {"state": "rate_limited", "until_ms": 9, "words": "",
             "connected": true, "says": "rate-limited, 4m left", "doing": ""}},
        ]}),
    );
}

#[test]
fn the_member_column_draws_the_coordinators_presence_under_each_seat() {
    let mut a = opened(1_000);
    host_with_seats(&mut a);
    let rows = screen(&mut a, 140);
    let all = rows.join("\n");
    let at = |n: &str| rows.iter().position(|r| r.contains(n)).unwrap();
    assert!(at("in turn 3m") == at("● s alpha") + 1, "{all}");
    assert!(at("letibot@z-r163") == at("in turn 3m") + 1, "{all}");
    assert!(row_with(&rows, "rate-limited, 4m left").contains("rate-limited"));
}

#[test]
fn a_sessions_row_says_the_derived_state_instead_of_running_or_idle() {
    let mut a = opened(1_000);
    a.sessions = vec![brief("s-x", "alpha", true)];
    a.rooms_sync_sessions();
    let before = screen(&mut a, 140).join("\n");
    assert!(
        before.contains("running"),
        "the head's own word until a roster says: {before}"
    );
    host_with_seats(&mut a);
    let rows = screen(&mut a, 140);
    assert!(
        rows.iter()
            .any(|r| r.contains("in turn 3m") && !r.contains("● s")),
        "{}",
        rows.join("\n")
    );
}

#[test]
fn the_rosters_of_other_rooms_are_asked_only_while_there_are_sessions_rows() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 0, 0), ("project/ab12cd34ef56", 1, 0, 0)]),
    );
    a.clock(3_500);
    let asked: Vec<String> = ops(&mut a)
        .iter()
        .filter(|o| o["type"] == "roster")
        .map(|o| o["room"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(asked, ["host"], "no Sessions rows: the selected room only");

    a.sessions = vec![brief("s-x", "alpha", true)];
    a.rooms_sync_sessions();
    a.clock(6_000);
    let asked: Vec<String> = ops(&mut a)
        .iter()
        .filter(|o| o["type"] == "roster")
        .map(|o| o["room"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(asked, ["host", "project/ab12cd34ef56"]);

    // Closed, with the sidebar on, the Sessions rows still get their rosters.
    a.pop_surface();
    a.sidebar_command("on");
    a.clock(60_000);
    let types: Vec<String> = ops(&mut a)
        .iter()
        .map(|o| {
            format!(
                "{} {}",
                o["type"].as_str().unwrap_or(""),
                o["room"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert!(types.contains(&"roster host".to_string()), "{types:?}");
    assert!(
        types.contains(&"roster project/ab12cd34ef56".to_string()),
        "{types:?}"
    );
}
