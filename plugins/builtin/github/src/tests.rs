//! The github layer's policy, tested natively - no engine, no wasm, no network.
//!
//! Every test here is a claim the operator made about the pane: a scope is one line carrying its
//! state words, every open pull request is a row under it with its review state and age, nothing is
//! drawn wider than the frame, and a fork says what it is.

use super::*;
use serde_json::json;

/// The daemon's snapshot, as `letibot_github::seat::Snapshot` spells it - the fixture each test
/// below varies by one field.
fn a_feed() -> Value {
    json!({
        "at_ms": 1_700_000_000_000u64,
        "posting": false,
        "identity": "letibot",
        "scopes": [
            {"scope": "stroppy-io", "state": "polling", "state_words": "polling every 60s",
             "why": "", "pulls": [
                {"repo": "stroppy-io/letibot", "number": 12,
                 "title": "a fix for the thing", "head_sha": "c1d2e3f",
                 "review": "drafted", "fork": false,
                 "last_action": "drafted a review of 2 commits", "age_ms": 120_000u64,
                 "session": "s-abc", "drafts": 1}]},
            {"scope": "someone/other", "state": "token missing",
             "state_words": "posting is on and no token file is there",
             "why": "", "pulls": []},
        ],
    })
}

/// The view the layer answers, through the hook's own door minus the wasm arena. `None` is a host
/// that handed over no feed at all.
fn view_of(feed: Option<&Value>) -> Value {
    view_at(feed, 80)
}

fn view_at(feed: Option<&Value>, w: u32) -> Value {
    let mut feeds = serde_json::Map::new();
    if let Some(f) = feed {
        feeds.insert("github".into(), f.clone());
    }
    let input = json!({
        "v": letibot_plugin::pane::V,
        "size": {"w": w, "h": 20},
        "feeds": Value::Object(feeds),
    });
    let bytes = serde_json::to_vec(&input).expect("the input serialises");
    let text = answer(&bytes).expect("the layer answers");
    serde_json::from_str(&text).expect("and it is JSON")
}

/// **One cell's text**, whether it is the plain string spelling or `{"text","role"}` - a cell that
/// carries a role is an object, and reading it as a string would make every assertion here vacuous.
fn text_of(cell: &Value) -> String {
    match cell {
        Value::String(s) => s.clone(),
        other => other["text"].as_str().unwrap_or_default().to_string(),
    }
}

fn cells_in(row: &Value, field: &str) -> Vec<String> {
    row[field]
        .as_array()
        .map(|cs| cs.iter().map(text_of).collect())
        .unwrap_or_default()
}

/// A row's left column, its right column and the faint lines under it, as one string - everything
/// the frame draws for that row.
fn row_text(row: &Value) -> String {
    let mut all = cells_in(row, "cells");
    all.extend(cells_in(row, "facts"));
    all.extend(
        row["under"]
            .as_array()
            .map(|us| us.iter().filter_map(|u| u.as_str().map(str::to_string)))
            .into_iter()
            .flatten(),
    );
    all.join(" ")
}

/// **What the frame draws on the row's own line** - the two columns, joined the way the grid joins
/// them. The `under` lines are separate lines and are measured separately.
fn row_line(row: &Value) -> String {
    let cells = cells_in(row, "cells").join(" ");
    let facts = cells_in(row, "facts").join(" ");
    format!("{cells} {facts}").trim().to_string()
}

/// Every row of every panel's tree, depth first - a pull request's row is its scope's child.
fn rows_of(view: &Value) -> Vec<Value> {
    fn walk(rows: &[Value], out: &mut Vec<Value>) {
        for r in rows {
            out.push(r.clone());
            if let Some(kids) = r["children"].as_array() {
                walk(kids, out);
            }
        }
    }
    let mut out = Vec::new();
    for panel in view["panels"].as_array().into_iter().flatten() {
        for block in panel["summary"].as_array().into_iter().flatten() {
            if let Some(rows) = block["tree"]["rows"].as_array() {
                walk(rows, &mut out);
            }
        }
    }
    out
}

/// Every `lines` block of the view, as one `label  value` string each - the header the seat draws.
fn lines_of(view: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for panel in view["panels"].as_array().into_iter().flatten() {
        for block in panel["summary"].as_array().into_iter().flatten() {
            for line in block["lines"].as_array().into_iter().flatten() {
                let label = line["label"].as_str().unwrap_or_default();
                let value = line["value"].as_str().unwrap_or_default();
                out.push(format!("{label}  {value}").trim().to_string());
            }
        }
    }
    out
}

/// The row a pull request's id names.
fn pull_row(view: &Value, id: &str) -> Value {
    rows_of(view)
        .into_iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no row `{id}` in {view}"))
}

#[test]
fn the_scopes_are_one_line_each_with_their_state() {
    let view = view_of(Some(&a_feed()));
    let scopes: Vec<Value> = rows_of(&view)
        .into_iter()
        .filter(|r| r["id"].as_str().is_some_and(|i| i.starts_with("scope:")))
        .collect();
    assert_eq!(
        scopes.len(),
        2,
        "one line per scope, and no line per pull here"
    );

    let first = row_text(&scopes[0]);
    assert_eq!(scopes[0]["id"], "scope:stroppy-io");
    assert!(first.contains("stroppy-io"), "{first}");
    assert!(first.contains("polling every 60s"), "{first}");
    // The count of open pull requests is on the same line, not only under it.
    assert!(first.contains("1 open"), "{first}");

    let second = row_text(&scopes[1]);
    assert_eq!(scopes[1]["id"], "scope:someone/other");
    assert!(second.contains("someone/other"), "{second}");
    assert!(
        second.contains("posting is on and no token file is there"),
        "{second}"
    );
}

#[test]
fn every_pull_request_is_a_row_with_its_review_state_action_and_age() {
    let view = view_of(Some(&a_feed()));
    let row = pull_row(&view, "pr:stroppy-io/letibot#12");
    let text = row_text(&row);
    for want in ["#12", "drafted", "drafted a review of 2 commits", "2m"] {
        assert!(text.contains(want), "`{want}` is missing from `{text}`");
    }
    // The last action is the faint line under the row, and Enter goes to the session working it.
    let under: Vec<&str> = row["under"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| u.as_str())
        .collect();
    assert!(
        under
            .iter()
            .any(|u| u.contains("drafted a review of 2 commits")),
        "{under:?}"
    );
    assert_eq!(row["enter"], json!({"switch": "s-abc"}));
    // The pull request's row hangs under its scope's, which is what `under it` means on a screen.
    let scope = rows_of(&view)
        .into_iter()
        .find(|r| r["id"] == "scope:stroppy-io")
        .expect("the scope row");
    let kids = scope["children"].as_array().expect("a scope has children");
    assert_eq!(kids.len(), 1, "{kids:?}");
    assert_eq!(kids[0]["id"], "pr:stroppy-io/letibot#12");
}

#[test]
fn no_line_is_wider_than_eighty_cells() {
    let mut feed = a_feed();
    let long = "a title so long that no eighty cell line can hold it, written out past a hundred \
                characters so that the ellipsis has something to cut away";
    assert!(long.chars().count() > 100, "the fixture is the point");
    feed["scopes"][0]["pulls"][0]["title"] = json!(long);
    let view = view_at(Some(&feed), 80);

    for row in rows_of(&view) {
        let line = row_line(&row);
        assert!(
            line.chars().count() <= 80,
            "{} cells: `{line}`",
            line.chars().count()
        );
        for under in row["under"].as_array().into_iter().flatten() {
            let text = under.as_str().unwrap_or_default();
            assert!(
                text.chars().count() <= 80,
                "{} cells: `{text}`",
                text.chars().count()
            );
        }
    }
    for line in lines_of(&view) {
        assert!(
            line.chars().count() <= 80,
            "{} cells: `{line}`",
            line.chars().count()
        );
    }

    // Truncated, and truncated in the title only: the state and the age are what a person acts on.
    let row = pull_row(&view, "pr:stroppy-io/letibot#12");
    let text = row_text(&row);
    assert!(text.contains('…'), "the title is cut: {text}");
    assert!(
        text.contains("a title so long"),
        "and its head survives: {text}"
    );
    assert!(text.contains("drafted"), "{text}");
    assert!(text.contains("2m"), "{text}");
}

#[test]
fn a_fork_is_marked_read_only() {
    let mut feed = a_feed();
    feed["scopes"][0]["pulls"][0]["fork"] = json!(true);
    let view = view_of(Some(&feed));
    let text = row_text(&pull_row(&view, "pr:stroppy-io/letibot#12"));
    assert!(text.contains("fork"), "{text}");
    assert!(text.contains("read-only"), "{text}");
}

#[test]
fn a_scope_with_no_pull_request_says_so() {
    let view = view_of(Some(&a_feed()));
    let scope = rows_of(&view)
        .into_iter()
        .find(|r| r["id"] == "scope:someone/other")
        .expect("the scope with nothing in it is still drawn");
    let text = row_text(&scope);
    assert!(text.contains("no open pull requests"), "{text}");
}

#[test]
fn no_feed_says_no_scope_is_attached() {
    // A host that handed over nothing at all, and a snapshot whose scope list is empty: the two
    // mean the same thing on a screen, and neither may be an empty panel.
    let mut empty = a_feed();
    empty["scopes"] = json!([]);
    for view in [view_of(None), view_of(Some(&empty))] {
        let drawn: Vec<String> = lines_of(&view)
            .into_iter()
            .chain(rows_of(&view).iter().map(row_text))
            .collect();
        assert!(
            drawn.iter().any(|l| l.contains("no scope is attached")),
            "{drawn:?}"
        );
    }
}

#[test]
fn posting_off_says_everything_is_a_draft() {
    let off = view_of(Some(&a_feed()));
    let shown: Vec<String> = std::iter::once(
        off["badge"]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
    .chain(lines_of(&off))
    .collect();
    assert!(
        shown.iter().any(|l| l.contains("draft")),
        "nothing is being sent, and the pane must say so: {shown:?}"
    );

    let mut on = a_feed();
    on["posting"] = json!(true);
    let view = view_of(Some(&on));
    let shown: Vec<String> = std::iter::once(
        view["badge"]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
    .chain(lines_of(&view))
    .collect();
    assert!(shown.iter().any(|l| l.contains("posting")), "{shown:?}");
}

#[test]
fn a_row_without_a_session_has_no_enter() {
    let mut feed = a_feed();
    feed["scopes"][0]["pulls"][0]["session"] = json!("");
    let view = view_of(Some(&feed));
    let row = pull_row(&view, "pr:stroppy-io/letibot#12");
    assert!(row.get("enter").is_none(), "{row}");
}

#[test]
fn every_state_word_the_pane_draws_is_in_the_legend() {
    let view = view_of(Some(&a_feed()));
    let legend = view["legend"].as_object().expect("a legend");
    for word in [
        "polling",
        "rate-limited",
        "token missing",
        "paused",
        "failed",
        "queued",
        "reviewing",
        "drafted",
        "posted",
        "re-review pending",
        "answering a mention",
        "fork",
        "read-only",
    ] {
        let sentence = legend
            .get(word)
            .and_then(|s| s.as_str())
            .unwrap_or_default();
        assert!(!sentence.is_empty(), "`{word}` is drawn and unexplained");
    }
}
