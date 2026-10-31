//! **The room's pane, tested where the policy lives.**
//!
//! The rows, the marks, the words and the four verbs are what a project overrides and what the
//! operator reads, so they are tested here rather than through an engine: this module is a
//! `cdylib` for the host and an `rlib` for these, and nothing below knows that a wasm ABI exists.
//!
//! **The verb on a row is the assertion that matters.** The host performs `claim`, `release`,
//! `abandon` and `rule` on the entry a row names, and `View::validate_for` refuses a verb the
//! manifest did not declare — so a row that carried the wrong verb would be a pane whose keys act
//! on the wrong thing, or on nothing, with no test above this one to notice.

use super::*;
use serde_json::json;

/// A moment to hang every age off: the host's clock in a fixture.
const NOW: u64 = 1_000_000_000;

fn feed_of(value: Value) -> Feed {
    serde_json::from_value(value).expect("the fixture is a room feed")
}

/// The fixture's one way of saying *and this is what the room looks like*.
fn feed(rows: Value, questions: Value) -> Feed {
    feed_of(json!({
        "now_ms": NOW,
        "here": "s-a",
        "identity": "the room on /tmp/room.sock — started 12s ago and holding 3 item(s) since \
                     then; the room keeps no store, so a restart begins with no items at all (2 \
                     connection(s) served since it started)",
        "arbiter": {"session": "s-room-abc", "up": true},
        "connected": ["s-a", "s-b", "s-room-abc"],
        "lease_ms": 90_000,
        "threshold": 3,
        "unreadable": null,
        // **A host that may restart the room, and has not restarted it yet** — the fixture every
        // other test reads, so a row that grows a key nobody asked for shows up here.
        "restart": {"available": true, "why": null, "marker": null},
        "tags": rows,
        "questions": questions,
        "counts": {"items": 3, "live": 3, "claimable": 1, "claimed": 1, "abandoned": 1,
                   "frozen": 0, "done": 0, "dropped": 0},
    }))
}

/// One item row, with the fields a fixture does not care about filled in.
fn item(id: u64, body: &str, state: &str, more: Value) -> Value {
    let mut row = json!({
        "id": id,
        "entry": format!("i{id}"),
        "body": body,
        "state": state,
        "posted_by": "s-b",
        "posted_age_ms": 60_000,
        "claimant": null,
        "claim_age_ms": null,
        "beat_age_ms": null,
        "stale": false,
        "abandonments": 0,
        "claimable": state == "open",
        "frozen": false,
        "mine": false,
        "live": state != "done",
    });
    let (Some(to), Some(from)) = (row.as_object_mut(), more.as_object()) else {
        panic!("a row fixture is an object");
    };
    for (key, value) in from {
        to.insert(key.clone(), value.clone());
    }
    row
}

/// **Every row of every panel, by id** — what the assertions below walk.
fn rows(view: &Value) -> Vec<Value> {
    fn walk(rows: &[Value], out: &mut Vec<Value>) {
        for row in rows {
            out.push(row.clone());
            if let Some(children) = row["children"].as_array() {
                walk(children, out);
            }
        }
    }
    let mut out = Vec::new();
    for panel in view["panels"].as_array().expect("panels") {
        for block in panel["summary"].as_array().expect("summary") {
            if let Some(tree) = block.get("tree") {
                walk(tree["rows"].as_array().expect("rows"), &mut out);
            }
        }
    }
    out
}

/// **A cell's words**, whichever spelling it took: a plain string, or `{"text", "role"}` when it
/// carries a role. A test that read `as_str` only would compare against empty strings and pass.
fn cells_of(row: &Value, which: &str) -> Vec<String> {
    row[which]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            c.as_str()
                .map(str::to_string)
                .or_else(|| c["text"].as_str().map(str::to_string))
                .unwrap_or_default()
        })
        .collect()
}

fn row_with<'a>(view: &'a Value, id: &str) -> Value {
    rows(view)
        .into_iter()
        .find(|r| r["id"] == json!(id))
        .unwrap_or_else(|| {
            panic!(
                "no row `{id}` in {}",
                serde_json::to_string_pretty(view).unwrap()
            )
        })
}

/// **The pane's rows, on the three items a queue is read for**: a tag with two of them, one claimed
/// with the age of its beat, and an abandoned one with its count.
#[test]
fn a_tag_draws_its_items_with_the_beat_age_and_the_count() {
    let feed = feed(
        json!([
            {"tag": "ram", "counts": {"items": 2, "live": 2, "claimable": 1, "claimed": 1},
             "rows": [
                item(1, "8 GB, ~10 min", "open", json!({})),
                item(2, "2 GB", "claimed", json!({
                    "claimant": "s-b", "claim_age_ms": 30_000, "beat_age_ms": 2_000,
                    "claimable": false,
                })),
             ]},
            {"tag": "disk", "counts": {"items": 1, "live": 1, "claimable": 1, "abandoned": 1},
             "rows": [
                item(3, "40 GB", "abandoned", json!({
                    "abandonments": 2, "claimable": true,
                    "last_why": "its beat stopped — the last one was 1m 30s old",
                })),
             ]},
        ]),
        json!([]),
    );
    let view = view(&feed);

    // **A tag row, then its items** — the queue per tag, which is the shape of the whole thing.
    let ram = row_with(&view, "t:ram");
    assert_eq!(ram["children"].as_array().unwrap().len(), 2);
    assert_eq!(ram["cells"][0], json!({"text": "`ram`", "role": "strong"}));
    let drawn = rows(&view);
    assert_eq!(
        drawn.iter().filter(|r| r["id"] == json!("i1")).count(),
        1,
        "an item is drawn once, under its own tag"
    );

    // **The claimed one**: who holds it, how long, and the age of the beat.
    let claimed = row_with(&view, "i2");
    let facts = cells_of(&claimed, "facts");
    assert!(facts.contains(&"held by s-b".to_string()), "{facts:?}");
    assert!(facts.contains(&"30s".to_string()), "held for: {facts:?}");
    assert!(facts.contains(&"beat 2s".to_string()), "{facts:?}");
    // **A claim somebody else holds offers no key**: `claim` would be refused `Busy` and `release`
    // would be refused `NotHeld`, so the row carries neither.
    assert_eq!(claimed["enter"], json!(null));
    assert_eq!(claimed["actions"], json!([]));

    // **The abandoned one says how many it has lost**, and it is claimable again.
    let abandoned = row_with(&view, "i3");
    let facts = cells_of(&abandoned, "facts");
    assert!(facts.contains(&"2 lost".to_string()), "{facts:?}");
    assert_eq!(
        abandoned["enter"],
        json!({"verb": "claim", "id": "i3"}),
        "an abandoned item is claimable again, at the head of its tag"
    );
    assert!(
        abandoned["under"][1]
            .as_str()
            .unwrap_or_default()
            .starts_with("last: its beat stopped"),
        "{}",
        abandoned["under"]
    );

    // **The open one leads with `claim`**, and offers nothing else.
    let open = row_with(&view, "i1");
    assert_eq!(open["enter"], json!({"verb": "claim", "id": "i1"}));
    assert_eq!(open["actions"], json!([]));
}

/// **A row this session holds leads with `release` and offers `x` for an abandonment** — two
/// intentions on one room verb, which is the only place two of the four controls share a row.
#[test]
fn a_row_this_session_holds_leads_with_release_and_offers_abandon() {
    let feed = feed(
        json!([{"tag": "ram", "counts": {"items": 1, "claimed": 1},
        "rows": [item(7, "8 GB", "claimed", json!({
            "claimant": "s-a", "claim_age_ms": 5_000, "beat_age_ms": 1_000,
            "claimable": false, "mine": true,
        }))]}]),
        json!([]),
    );
    let view = view(&feed);
    let row = row_with(&view, "i7");
    assert_eq!(
        row["enter"],
        json!({"verb": "release", "id": "i7"}),
        "the person's own claim: Enter gives it up"
    );
    assert_eq!(
        row["actions"],
        json!([{"key": "x", "label": "mark abandoned", "verb": "abandon"}]),
        "and `x` is the harder sentence on the same room verb"
    );
    // And it says whose it is, in the strong role.
    assert_eq!(
        row["facts"][0],
        json!({"text": "held by s-a", "role": "strong"})
    );
}

/// **A frozen item offers nothing** — it is not claimable, and its question is the `a` panel.
#[test]
fn a_frozen_item_says_so_and_offers_no_key() {
    let feed = feed(
        json!([{"tag": "ram", "counts": {"items": 1, "live": 1, "frozen": 1},
        "rows": [item(9, "8 GB", "abandoned", json!({
            "abandonments": 3, "frozen": true, "claimable": false, "question": 2,
        }))]}]),
        json!([]),
    );
    let view = view(&feed);
    let row = row_with(&view, "i9");
    assert_eq!(row["enter"], json!(null));
    assert!(
        row["under"].as_array().unwrap().iter().any(|u| u
            .as_str()
            .unwrap_or_default()
            .starts_with("frozen until a ruling")),
        "{}",
        row["under"]
    );
}

/// **The badge is the room's counts, in the role that makes a person open the pane** — a frozen item
/// or an open question is attention; a held one is pending; an idle room is faint.
#[test]
fn the_badge_counts_what_the_room_is_waiting_on() {
    let mut frozen = feed(json!([]), json!([]));
    frozen.counts.frozen = 1;
    assert_eq!(view(&frozen)["badge"]["role"], json!("attention"));
    assert_eq!(
        view(&frozen)["badge"]["text"],
        json!("3 live · 1 held · 1 frozen")
    );

    let mut asked = feed(json!([]), json!([]));
    asked.questions.push(Question {
        id: 1,
        entry: "q1".into(),
        ..Question::default()
    });
    assert_eq!(
        view(&asked)["badge"]["text"],
        json!("3 live · 1 held · 1 to rule")
    );

    let mut idle = feed(json!([]), json!([]));
    idle.counts = Counts::default();
    assert_eq!(
        view(&idle)["badge"],
        json!({"text": "0 live", "role": "faint"})
    );
}

/// **Each of the four verbs appears on the row that would do something**, which is this layer's
/// half of the round trip: the host performs them, the layer names them.
#[test]
fn the_four_controls_are_each_on_the_row_that_would_move() {
    let feed = feed(
        json!([{"tag": "ram", "counts": {"items": 2, "claimable": 1, "claimed": 1},
        "rows": [
           item(1, "8 GB", "open", json!({})),
           item(2, "2 GB", "claimed", json!({
               "claimant": "s-a", "mine": true, "claimable": false,
               "claim_age_ms": 1_000, "beat_age_ms": 500,
           })),
        ]}]),
        json!([{
            "id": 4, "entry": "q4", "item": 1, "tag": "ram", "body": "8 GB",
            "about": "item_raised", "asked_age_ms": 9_000,
            "text": "Item #1 under `ram` has abandoned 3 claimants: …",
            "lost": 3, "behind": 1,
        }]),
    );
    let view = view(&feed);

    // `claim` on the claimable item, `release` on this session's own, `abandon` as its `x`, and
    // `rule` on each decision the question offers.
    assert_eq!(row_with(&view, "i1")["enter"]["verb"], json!("claim"));
    assert_eq!(row_with(&view, "i2")["enter"]["verb"], json!("release"));
    assert_eq!(
        row_with(&view, "i2")["actions"][0]["verb"],
        json!("abandon")
    );
    let decisions: Vec<(String, String)> = rows(&view)
        .into_iter()
        .filter(|r| r["id"].as_str().unwrap_or_default().starts_with("q4:"))
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["enter"]["id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        decisions.contains(&("q4:retry".into(), "q4:retry".into())),
        "{decisions:?}"
    );
    assert!(
        decisions.contains(&("q4:drop".into(), "q4:drop".into())),
        "{decisions:?}"
    );
    // **One `hand` row per session the room has met**, because a hand names one and the room
    // refuses a name it has never seen.
    for session in ["s-a", "s-b", "s-room-abc"] {
        let want = format!("q4:hand:{session}");
        assert!(
            decisions.contains(&(want.clone(), want)),
            "no row hands the item to `{session}`: {decisions:?}"
        );
    }
    for r in rows(&view) {
        if r["enter"]["verb"] == json!("rule") {
            assert_eq!(
                r["enter"]["verb"],
                json!("rule"),
                "every decision row carries the SAME verb and a different entry"
            );
        }
    }
}

/// **The identity is drawn, verbatim** — the sentence that keeps *restarted clean* and *restarted
/// empty* apart, and the reason a pane is trusted at all.
#[test]
fn the_room_says_what_it_is_and_who_arbitrates_it() {
    let feed = feed(json!([]), json!([]));
    let view = view(&feed);
    let drawn = serde_json::to_string(&view).unwrap();
    assert!(drawn.contains("started 12s ago"), "{drawn}");
    assert!(drawn.contains("no store"), "{drawn}");
    assert!(drawn.contains("s-room-abc"), "{drawn}");
    assert!(drawn.contains("connected"), "the arbiter is up: {drawn}");
    // **An arbiter that is not connected is the one thing a person must see** — its questions wait.
    let mut down = feed_of(json!({
        "identity": "the room on /tmp/room.sock — started 4s ago",
        "arbiter": {"session": "s-room-abc", "up": false},
        "connected": ["s-a"],
    }));
    down.arbiter.as_mut().unwrap().up = false;
    let drawn = serde_json::to_string(&super::view(&down)).unwrap();
    assert!(drawn.contains("NOT connected"), "{drawn}");
    assert!(drawn.contains("\"attention\""), "{drawn}");
}

/// **An empty room is not a silent one**: the tree says what an empty queue means here, and the
/// identity above it says how long the room has been up.
#[test]
fn an_empty_room_says_that_emptiness_is_what_a_start_looks_like() {
    let mut empty = feed(json!([]), json!([]));
    empty.counts = Counts::default();
    let view = view(&empty);
    let drawn = serde_json::to_string(&view).unwrap();
    assert!(drawn.contains("this room keeps no store"), "{drawn}");
    assert!(drawn.contains("started 12s ago"), "{drawn}");
    assert_eq!(view["badge"]["text"], json!("0 live"));
}

/// **A long-lived empty room and a just-restarted one are two different screens** — the operator's
/// requirement in as many words, and the reason the identity sentence and the note both exist.
///
/// Both are empty, both say the room keeps no store, and the difference is the two lines a person
/// reads first: how long it has been up, and whether anything restarted it.
#[test]
fn an_empty_room_that_was_just_restarted_is_not_an_empty_room_that_has_been_quiet() {
    // The room that has been up for hours with nothing in it: no note, and the identity's age.
    let mut quiet = feed(json!([]), json!([]));
    quiet.counts = Counts::default();
    quiet.identity = "the room on /tmp/room.sock — started 3h 2m ago and holding 0 item(s) since \
                     then; the room keeps no store, so a restart begins with no items at all (9 \
                     connection(s) served since it started)"
        .to_string();
    let quiet = view(&quiet);
    let quiet_drawn = serde_json::to_string(&quiet).unwrap();
    assert!(quiet_drawn.contains("3h 2m ago"), "{quiet_drawn}");
    assert!(
        quiet_drawn.contains("nothing has restarted this room since it began"),
        "the quiet room says so rather than leaving it to be guessed: {quiet_drawn}"
    );

    // The room that began four seconds ago because somebody pressed the key.
    let mut fresh = feed(json!([]), json!([]));
    fresh.counts = Counts::default();
    fresh.identity = "the room on /tmp/room.sock — started 4s ago and holding 0 item(s) since \
                     then; the room keeps no store, so a restart begins with no items at all (1 \
                     connection(s) served since it started)"
        .to_string();
    fresh.restart.marker = Some(Marker {
        age_ms: 6_000,
        cause: "asked".to_string(),
        sentence: "restarted 6s ago: `s-a` pressed restart in the room pane. A restart begins \
                   with no items at all."
            .to_string(),
    });
    let fresh = view(&fresh);
    let fresh_drawn = serde_json::to_string(&fresh).unwrap();
    assert!(fresh_drawn.contains("started 4s ago"), "{fresh_drawn}");
    assert!(
        fresh_drawn.contains("`s-a` pressed restart"),
        "the note says why: {fresh_drawn}"
    );
    assert!(
        !fresh_drawn.contains("nothing has restarted this room since it began"),
        "a room that HAS been restarted does not say nothing has: {fresh_drawn}"
    );
    assert_ne!(quiet_drawn, fresh_drawn, "two empty rooms, two screens");
}

/// **The restart is a declared row, and it is offered only where it would do something.**
///
/// The `s` panel is the operator's control (*"that panel must explicitly let me to restart the
/// server"*); a host with no supervisor draws the reason instead of a key that would refuse.
#[test]
fn the_restart_control_is_a_declared_row_and_is_not_offered_without_a_supervisor() {
    let offered = view(&feed(json!([]), json!([])));
    let row = row_with(&offered, "restart");
    assert_eq!(
        row["enter"],
        json!({"verb": "restart", "id": "restart"}),
        "the control names the verb the manifest declares"
    );
    let under = row["under"].as_array().unwrap();
    assert!(
        under.iter().any(|u| u
            .as_str()
            .unwrap_or_default()
            .contains("a restart begins with no items at all")),
        "what survives is stated, not assumed: {under:?}"
    );
    // The panel is reachable by its own key, and it says which verb it is.
    assert_eq!(offered["panels"][2]["key"], json!("s"));
    assert_eq!(offered["panels"][2]["id"], json!("server"));

    // **A host that may not restart the room offers no key** — the coordinator's own daemon is
    // the case: a restart from its pane would kill the feed that reports it.
    let mut none = feed(json!([]), json!([]));
    none.restart.available = false;
    none.restart.why = Some(
        "this host was not given a supervisor for the box's room, so there is no restart key here \
         — a coordinator is started by the launcher (`letibot --room`)"
            .to_string(),
    );
    let none = view(&none);
    let row = row_with(&none, "restart");
    assert_eq!(row["enter"], json!(null), "no key where none would work");
    assert_eq!(cells_of(&row, "cells")[1], "not offered here");
    let under = row["under"].as_array().unwrap();
    assert!(
        under.iter().any(|u| u
            .as_str()
            .unwrap_or_default()
            .contains("was not given a supervisor")),
        "and the reason is on the row: {under:?}"
    );
}

/// **A room that could not be read draws one panel saying why**, not an empty queue — and the
/// restart stays on the pane, because a room nobody can read is the one most likely to be dead.
#[test]
fn an_unreadable_room_says_so_instead_of_drawing_an_empty_queue() {
    let mut broken = feed(json!([]), json!([]));
    broken.identity = "the room on /tmp/room.sock".to_string();
    broken.unreadable = Some("the room at /tmp/room.sock did not answer in 10s".to_string());
    let view = view(&broken);
    assert_eq!(view["panels"].as_array().unwrap().len(), 2);
    assert_eq!(
        view["panels"][0]["unavailable"]["why"],
        json!("the room at /tmp/room.sock did not answer in 10s")
    );
    assert_eq!(
        view["panels"][0]["unavailable"]["fix"]
            .as_str()
            .unwrap()
            .contains("harnessd --room"),
        true
    );
    // **The control is still there**, on the panel that needs nothing from the room: an unreadable
    // room is the one a person restarts.
    assert_eq!(view["panels"][1]["id"], json!("server"));
    assert_eq!(
        row_with(&view, "restart")["enter"],
        json!({"verb": "restart", "id": "restart"}),
        "the restart survives the room being unreadable"
    );
}

/// **A host that hands no feed is told apart from a room with nothing in it.**
#[test]
fn no_feed_is_its_own_sentence() {
    let view = no_feed();
    let drawn = serde_json::to_string(&view).unwrap();
    assert!(drawn.contains("hands no `room` feed"), "{drawn}");
}

/// **Every act in the view is one of the five the manifest declares** — the half of
/// `View::validate_for` this layer can check on its own. The host runs the real one (`validate`
/// plus the manifest) before the pane is drawn; what is asserted here is that the layer never
/// reaches for a sixth word, which is the defect that check would find.
#[test]
fn every_act_names_one_of_the_five_controls() {
    let feed = feed(
        json!([{"tag": "ram", "counts": {"items": 2, "claimable": 1, "claimed": 1},
        "rows": [
           item(1, "8 GB", "open", json!({})),
           item(2, "2 GB", "claimed", json!({
               "claimant": "s-a", "mine": true, "claimable": false,
               "claim_age_ms": 1_000, "beat_age_ms": 500,
           })),
        ]}]),
        json!([{"id": 2, "entry": "q2", "item": 1, "tag": "ram", "body": "8 GB",
                "about": "item_raised", "asked_age_ms": 1_000, "text": "…", "lost": 3,
                "behind": 0}]),
    );
    let view = view(&feed);
    let declared = ["claim", "release", "abandon", "rule", "restart"];
    let mut seen: Vec<String> = Vec::new();
    for row in rows(&view) {
        if let Some(verb) = row["enter"]["verb"].as_str() {
            seen.push(verb.to_string());
        }
        for action in row["actions"].as_array().into_iter().flatten() {
            if let Some(verb) = action["verb"].as_str() {
                seen.push(verb.to_string());
            }
        }
    }
    assert!(!seen.is_empty(), "the fixture offers keys");
    for verb in &seen {
        assert!(
            declared.contains(&verb.as_str()),
            "`{verb}` is not one of the five"
        );
    }
    // And all four are reached by this one fixture, so the set is not accidentally a subset.
    for want in declared {
        assert!(
            seen.iter().any(|v| v == want),
            "`{want}` is never offered: {seen:?}"
        );
    }
}
