//! **The nick list's policy, tested where it lives.**
//!
//! The sort rule, the column's arithmetic, the marks and the scope are what a project overrides and
//! what the operator reads, so they are tested here rather than through an engine: the module is a
//! `cdylib` for the host and an `rlib` for these, and nothing below knows that a wasm ABI exists.
//!
//! **The rows are rendered the way the host renders them** — [`drawn`] is rano's `row_line` for the
//! part this layer controls (a two-column lead, then the cells joined by one space). A width test
//! that measured the answer instead of the drawing would pass on a row the head then stretches.

use super::*;

/// A moment to hang every age off: the host's clock in a fixture.
const NOW: u64 = 1_000_000_000;

/// One session, with the shape an ordinary live row has.
fn s(id: &str, title: &str) -> Value {
    json!({
        "id": id,
        "title": title,
        "live": true,
        "heads": 0,
        "running": false,
        "items": 6,
        "model": "qwen",
        "workspace": "/p/letibot",
        "moved_ms": NOW - 60_000,
        "repo": "/p/letibot",
    })
}

/// `base` with fields replaced — the fixture's one way of saying *and this one is different*.
fn with(base: Value, more: Value) -> Value {
    let mut out = base;
    let (Some(to), Some(from)) = (out.as_object_mut(), more.as_object()) else {
        panic!("a session fixture is an object");
    };
    for (key, value) in from {
        to.insert(key.clone(), value.clone());
    }
    out
}

/// The `sessions` feed, as the host hands it.
fn feed_of(sessions: Vec<Value>, here: &str) -> Value {
    json!({"sessions": sessions, "here": here, "home": "/home/dead", "now": NOW})
}

/// **One call of the pane**, and the view it answered.
fn call(feed: &Value, state: &Value, rect: u32) -> Value {
    ask(feed, state, rect, None)
}

/// The same, with a key pressed on `item`.
fn press(feed: &Value, state: &Value, rect: u32, item: &str, action: &str) -> Value {
    ask(feed, state, rect, Some((item, action)))
}

fn ask(feed: &Value, state: &Value, rect: u32, event: Option<(&str, &str)>) -> Value {
    let mut input = json!({
        "v": letibot_plugin::pane::V,
        "state": state,
        "size": {"w": rect, "h": 40},
        "feeds": {"sessions": feed},
    });
    if let Some((item, action)) = event {
        input["event"] = json!({"item": item, "action": action});
    }
    let bytes = serde_json::to_vec(&input).expect("the input serialises");
    let answer = answer(&bytes).expect("the pane answered");
    serde_json::from_str(&answer).expect("the answer is a view")
}

/// The state a view hands back — what the host keeps and gives the next call.
fn kept(view: &Value) -> Value {
    view["state"].clone()
}

/// The rows of the pane's one tree block.
fn rows(view: &Value) -> Vec<Value> {
    view["panels"][0]["summary"][0]["tree"]["rows"]
        .as_array()
        .expect("a tree block")
        .clone()
}

/// The ids of the rows, `None` for a line that is not a stop (the header, the divider).
fn ids(view: &Value) -> Vec<Option<String>> {
    rows(view)
        .iter()
        .map(|r| r["id"].as_str().map(str::to_string))
        .collect()
}

/// The session rows only — every row a stop whose id is a session.
fn sessions(view: &Value) -> Vec<Value> {
    rows(view)
        .into_iter()
        .filter(|r| r["id"].as_str().is_some_and(|id| !id.starts_with('·')))
        .collect()
}

/// **A row as the host draws it**: rano's lead (`▸` and a space, or two spaces) and then the cells,
/// one space between each. A cell is its string when it carries no role.
fn drawn(row: &Value) -> String {
    let cells: Vec<String> = row["cells"]
        .as_array()
        .expect("cells")
        .iter()
        .map(|c| match c {
            Value::String(text) => text.clone(),
            other => other["text"].as_str().unwrap_or_default().to_string(),
        })
        .collect();
    format!("  {}", cells.join(" "))
}

/// The text of a row's cell `i`.
fn cell(row: &Value, i: usize) -> &str {
    match &row["cells"][i] {
        Value::String(text) => text,
        other => other["text"].as_str().unwrap_or_default(),
    }
}

/// The role of a row's cell `i`, or `None` when it takes the row's.
fn role(row: &Value, i: usize) -> Option<&str> {
    row["cells"][i]["role"].as_str()
}

/// **Three top-level sessions in three states are a channel**: the header counts them, and each row
/// says with one mark what it is.
#[test]
fn three_sessions_in_three_states_are_a_channel() {
    let f = feed_of(
        vec![
            // You: a head is attached, and a turn is running.
            with(
                s("s-a", "the habr article"),
                json!({"heads": 2, "running": true}),
            ),
            // Somebody else's, working, and nothing is attached to it.
            with(
                s("s-b", "the rust rewrite"),
                json!({"running": true, "moved_ms": NOW - 120_000}),
            ),
            // On disk: no daemon holds it.
            with(
                s("s-c", "stroppy notes"),
                json!({"live": false, "stored": 412, "moved_ms": NOW - 3 * 86_400_000}),
            ),
        ],
        "s-a",
    );
    let view = call(&f, &Value::Null, 100);
    let drawn = sessions(&view);
    assert_eq!(drawn.len(), 3, "three sessions, and the subagent-free list");
    assert_eq!(
        ids(&view)[0],
        None,
        "the header is a line, not a stop: a cursor that could rest on it spends a keystroke"
    );
    assert_eq!(
        cell(&rows(&view)[0], 0),
        "#letibot · 3 here · 2 working",
        "the header counts the channel"
    );
    // The marks, in the order the rows are drawn: you first, then the working one, then the stored.
    assert_eq!(
        drawn.iter().map(|r| cell(r, 0)).collect::<Vec<_>>(),
        vec!["»", "+", "·"]
    );
    assert_eq!(role(&drawn[0], 0), Some("strong"), "» is the strong role");
    assert_eq!(role(&drawn[1], 0), Some("pending"), "+ is the running role");
    assert_eq!(role(&drawn[2], 0), Some("muted"), "· is the muted role");
    // The fact is an age, and the age is what the row is for.
    assert_eq!(
        drawn.iter().map(|r| cell(r, 2)).collect::<Vec<_>>(),
        vec!["1m", "2m", "3d"]
    );
    // Every row is the host's own switch, declared in the manifest.
    assert_eq!(drawn[1]["enter"], json!({"switch": "s-b"}));
    // And every row carries the four keys, so they work wherever the cursor rests.
    let letters: Vec<&str> = drawn[0]["actions"]
        .as_array()
        .expect("actions")
        .iter()
        .map(|a| a["key"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(letters, vec!["w", "n", "h", "s"]);
}

/// **A subagent is not a nick.** A row whose `parent` is set is somebody's work, not a conversation
/// you switch to, and a list that listed them would bury the four channels a person is in.
#[test]
fn a_subagent_is_not_in_the_nick_list() {
    let f = feed_of(
        vec![
            with(s("s-a", "the habr article"), json!({"heads": 1})),
            with(
                s("s-a-sub-1", "read the repo"),
                json!({"parent": "s-a", "heads": 0}),
            ),
            with(s("s-b", "the rust rewrite"), json!({})),
        ],
        "s-a",
    );
    let view = call(&f, &Value::Null, 100);
    let listed: Vec<String> = sessions(&view)
        .iter()
        .map(|r| r["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(listed, vec!["s-a", "s-b"], "the child is not a channel");
    assert_eq!(
        cell(&rows(&view)[0], 0),
        "#letibot · 2 here · 0 working",
        "and it is not counted either"
    );
}

/// **The session you are in is marked** — `»`, the strong name, and the first row, because that is
/// the one a person always wants to find.
#[test]
fn the_session_you_are_in_is_marked() {
    let f = feed_of(
        vec![
            with(s("s-a", "somebody else"), json!({"heads": 3})),
            with(s("s-b", "where i am"), json!({"heads": 1})),
        ],
        "s-b",
    );
    let view = call(&f, &Value::Null, 100);
    let drawn = sessions(&view);
    assert_eq!(
        drawn[0]["id"], "s-b",
        "the session you are in is the first row, whatever else is attached"
    );
    assert_eq!(cell(&drawn[0], 0), "»");
    assert_eq!(role(&drawn[0], 1), Some("strong"), "and its name is strong");
    assert_eq!(cell(&drawn[1], 0), "@", "the other one has a head, not you");
    assert_eq!(role(&drawn[1], 1), Some("plain"));
}

/// **The column does not stretch.** A pane handed a hundred columns draws a thirty-column column
/// with the fact pinned to ITS right edge — the whole reason the fact is a padded cell rather than
/// the host's `facts` (which the host right-aligns against the rectangle).
#[test]
fn the_column_is_the_width_it_chose_and_not_the_rect() {
    let f = feed_of(
        vec![
            with(
                s("s-a", "the habr article"),
                json!({"heads": 1, "running": true}),
            ),
            with(s("s-b", "the rust rewrite"), json!({"running": true})),
            with(s("s-c", "stroppy notes"), json!({"live": false})),
        ],
        "s-a",
    );
    for (rect, want) in [(30_u32, 20_usize), (100, 30)] {
        let view = call(&f, &Value::Null, rect);
        for row in sessions(&view) {
            assert_eq!(
                cells(&drawn(&row)),
                want,
                "at a {rect}-column rectangle every row is {want} columns: {row}"
            );
            // The fact is the last thing on the row, so the row's width is where it ends.
            assert!(
                ["1m", "2m"].contains(&cell(&row, 2)) || cell(&row, 2) == "1m",
                "the fact is still there at {rect}: {row}"
            );
        }
    }
    // And the wide rectangle is not merely un-stretched: the fact sits at column 30, not at 100.
    let wide = call(&f, &Value::Null, 100);
    let first = &sessions(&wide)[0];
    assert_eq!(cells(&drawn(first)), 30);
    assert!(
        drawn(first).ends_with("1m"),
        "the fact is the row's last column: {:?}",
        drawn(first)
    );
}

/// **A name in a wide script does not push the fact out of the column.** Measured in the terminal's
/// cells rather than in characters, because that is what the host measures.
#[test]
fn a_wide_name_still_ends_the_fact_at_the_columns_edge() {
    let f = feed_of(
        vec![with(
            s("s-a", "日本語のタイトルはとても長いです"),
            json!({"heads": 1}),
        )],
        "s-a",
    );
    let view = call(&f, &Value::Null, 100);
    let row = &sessions(&view)[0];
    assert_eq!(cells(&drawn(row)), 30, "{:?}", drawn(row));
    assert!(drawn(row).ends_with('m'), "{:?}", drawn(row));
}

/// **The order is a roster, not a ranking**: the same set keeps its order even when a member's
/// state changes, because the host's cursor is an INDEX into these rows.
#[test]
fn the_order_is_pinned_until_the_roster_changes() {
    let quiet = feed_of(
        vec![
            with(s("s-a", "a"), json!({"heads": 1, "moved_ms": NOW - 10_000})),
            with(s("s-b", "b"), json!({"moved_ms": NOW - 20_000})),
            with(s("s-c", "c"), json!({"moved_ms": NOW - 30_000})),
        ],
        "s-a",
    );
    let first = call(&quiet, &Value::Null, 100);
    assert_eq!(
        ids(&first).into_iter().flatten().collect::<Vec<String>>(),
        vec!["s-a", "s-b", "s-c"],
        "you first, then the idle ones by recency"
    );
    // `s-c` starts working. The rule would move it to the top of the working band — and it does
    // NOT, because the set of sessions did not change.
    let busy = feed_of(
        vec![
            with(s("s-a", "a"), json!({"heads": 1, "moved_ms": NOW - 10_000})),
            with(
                s("s-b", "b"),
                json!({"running": true, "moved_ms": NOW - 20_000}),
            ),
            with(
                s("s-c", "c"),
                json!({"running": true, "moved_ms": NOW - 30_000}),
            ),
        ],
        "s-a",
    );
    let second = call(&busy, &kept(&first), 100);
    assert_eq!(
        ids(&second).into_iter().flatten().collect::<Vec<String>>(),
        vec!["s-a", "s-b", "s-c"],
        "a row that moves under the cursor is a row the person has to find again"
    );
    // One appears: the roster is recomputed, and the rule puts it in its band.
    let grown = feed_of(
        vec![
            with(s("s-a", "a"), json!({"heads": 1, "moved_ms": NOW - 10_000})),
            with(
                s("s-b", "b"),
                json!({"running": true, "moved_ms": NOW - 20_000}),
            ),
            with(
                s("s-c", "c"),
                json!({"running": true, "moved_ms": NOW - 30_000}),
            ),
            with(s("s-d", "d"), json!({"heads": 2, "moved_ms": NOW - 5_000})),
        ],
        "s-a",
    );
    let third = call(&grown, &kept(&second), 100);
    assert_eq!(
        ids(&third).into_iter().flatten().collect::<Vec<String>>(),
        vec!["s-a", "s-d", "s-b", "s-c"],
        "you, then the heads, then the working ones"
    );
}

/// **Hiding is a strip, and the strip is a stop** — a pane that hides itself to nothing is a pane
/// somebody has to guess their way back into, and the key that brings it back has to be on a row
/// the cursor can rest on.
#[test]
fn hiding_leaves_a_strip_that_says_how_to_come_back() {
    let f = feed_of(
        vec![
            with(s("s-a", "the habr article"), json!({"heads": 1})),
            with(s("s-b", "the rust rewrite"), json!({"running": true})),
        ],
        "s-a",
    );
    let open = call(&f, &Value::Null, 100);
    let hidden = press(&f, &kept(&open), 100, "s-b", "hide");
    let strip = rows(&hidden);
    assert_eq!(strip.len(), 1, "one strip, not nothing: {strip:?}");
    assert_eq!(strip[0]["id"], STRIP, "and it is a stop");
    let said = cell(&strip[0], 0);
    assert!(said.contains("2 here"), "{said}");
    assert!(said.contains("1 working"), "{said}");
    assert!(
        said.contains("h shows"),
        "the way back is on the strip: {said}"
    );
    let letters: Vec<&str> = strip[0]["actions"]
        .as_array()
        .expect("actions")
        .iter()
        .map(|a| a["key"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(letters, vec!["w", "n", "h", "s"], "and `h` still works");
    // Back, and it comes back as it left: same rows, same width.
    let back = press(&f, &kept(&hidden), 100, STRIP, "show");
    assert_eq!(ids(&back), ids(&open));
    assert_eq!(
        cells(&drawn(&sessions(&back)[0])),
        cells(&drawn(&sessions(&open)[0])),
        "the width survives a hide"
    );
}

/// **Wider and narrower are the column's own, clamped** to a third of the rectangle and to the
/// minimum — the two ends a person cannot press past.
#[test]
fn wider_and_narrower_are_clamped_to_a_third_and_to_the_minimum() {
    let f = feed_of(vec![with(s("s-a", "a"), json!({"heads": 1}))], "s-a");
    let start = call(&f, &Value::Null, 100);
    assert_eq!(cells(&drawn(&sessions(&start)[0])), 30, "the default");
    let wider = press(&f, &kept(&start), 100, "s-a", "wider");
    assert_eq!(
        cells(&drawn(&sessions(&wider)[0])),
        33,
        "a third of a hundred columns"
    );
    let again = press(&f, &kept(&wider), 100, "s-a", "wider");
    assert_eq!(
        cells(&drawn(&sessions(&again)[0])),
        33,
        "and no further: the conversation needs its two thirds"
    );
    let narrower = press(&f, &kept(&again), 100, "s-a", "narrower");
    assert_eq!(cells(&drawn(&sessions(&narrower)[0])), 29);
    // A narrow rectangle: the minimum, and `w` has nowhere to go.
    let narrow = press(&f, &Value::Null, 30, "s-a", "wider");
    assert_eq!(cells(&drawn(&sessions(&narrow)[0])), 20, "the minimum");
    // And at the minimum the marks and the fact are still there: the NAME is what goes.
    let row = &sessions(&narrow)[0];
    assert_eq!(cell(row, 0), "»");
    assert!(!cell(row, 2).is_empty());
}

/// **The scope is a filter, and the header says what it hides** — the channel's name IS the scope,
/// so a narrowed list can never look like a complete one.
#[test]
fn the_scope_narrows_to_the_project_and_says_what_it_hides() {
    let f = feed_of(
        vec![
            with(
                s("s-a", "this worktree"),
                json!({"workspace": "/p/letibot/worktrees/agent-x", "heads": 1, "repo": ""}),
            ),
            with(
                s("s-b", "the checkout"),
                json!({"workspace": "/p/letibot", "repo": ""}),
            ),
            with(
                s("s-c", "another project"),
                json!({"workspace": "/p/stroppy-io", "repo": ""}),
            ),
        ],
        "s-a",
    );
    // The workspace you are in: one of three, and the header says so.
    let path = call(&f, &Value::Null, 100);
    assert_eq!(
        cell(&rows(&path)[0], 0),
        "#agent-x · 1 of 3 · 0 working",
        "the channel is the folder, and `1 of 3` is not a complete list"
    );
    assert_eq!(
        ids(&path).into_iter().flatten().collect::<Vec<String>>(),
        vec!["s-a"],
        "nothing outside the workspace, and nothing is working outside it"
    );
    // The project: the worktree's REPOSITORY, which is what a person means by "this project".
    let repo = press(&f, &kept(&path), 100, "s-a", "scope");
    assert_eq!(
        cell(&rows(&repo)[0], 0),
        "#letibot · 2 of 3 · 0 working",
        "the worktree and its checkout are one project"
    );
    assert_eq!(
        ids(&repo).into_iter().flatten().collect::<Vec<String>>(),
        vec!["s-a", "s-b"]
    );
    // Everything.
    let every = press(&f, &kept(&repo), 100, "s-a", "scope");
    assert_eq!(cell(&rows(&every)[0], 0), "#all · 3 here · 0 working");
    // And round again.
    let round = press(&f, &kept(&every), 100, "s-a", "scope");
    assert_eq!(cell(&rows(&round)[0], 0), "#agent-x · 1 of 3 · 0 working");
}

/// **A `repo` the host asked git about wins over the worktree convention** — the convention is the
/// fallback for a host that did not say, not the rule.
#[test]
fn the_feeds_repository_wins_over_the_worktree_convention() {
    let f = feed_of(
        vec![
            with(
                s("s-a", "here"),
                json!({"workspace": "/elsewhere/wt/x", "repo": "/p/letibot", "heads": 1}),
            ),
            with(
                s("s-b", "the checkout"),
                json!({"workspace": "/p/letibot", "repo": "/p/letibot"}),
            ),
        ],
        "s-a",
    );
    let repo = press(&f, &Value::Null, 100, "s-a", "scope");
    assert_eq!(
        ids(&repo).into_iter().flatten().collect::<Vec<String>>(),
        vec!["s-a", "s-b"],
        "a worktree outside the repository's own folder is still the same project"
    );
}

/// **A session the scope hides but that is WORKING keeps a place** — a narrowed list that hides the
/// one agent doing something is how a person loses an agent.
#[test]
fn a_working_session_outside_the_scope_keeps_a_place() {
    let f = feed_of(
        vec![
            with(
                s("s-a", "here"),
                json!({"workspace": "/p/letibot", "heads": 1}),
            ),
            with(
                s("s-b", "elsewhere, idle"),
                json!({"workspace": "/p/other"}),
            ),
            with(
                s("s-c", "elsewhere, working"),
                json!({"workspace": "/p/other", "running": true}),
            ),
        ],
        "s-a",
    );
    let view = call(&f, &Value::Null, 100);
    let drawn = rows(&view);
    let texts: Vec<String> = drawn.iter().map(|r| cell(r, 0).to_string()).collect();
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("· 2 elsewhere · 1 working")),
        "the divider says how many were left out and how many of those work: {texts:?}"
    );
    assert_eq!(
        ids(&view).into_iter().flatten().collect::<Vec<String>>(),
        vec!["s-a", "s-c"],
        "the working one is a row under the divider — a stop, so Enter switches there"
    );
    let working = sessions(&view)
        .into_iter()
        .find(|r| r["id"] == "s-c")
        .expect("the working one");
    assert_eq!(cell(&working, 0), "+", "and it keeps its mark");
}

/// **A running session that has gone quiet is drawn as news** — the whole question the fact column
/// answers, and the one place its role is not `faint` or `pending`.
#[test]
fn a_running_session_that_has_gone_quiet_is_drawn_as_news() {
    let f = feed_of(
        vec![
            with(
                s("s-a", "busy"),
                json!({"heads": 1, "running": true, "moved_ms": NOW - 20 * 60_000}),
            ),
            with(
                s("s-b", "busy, but fine"),
                json!({"running": true, "moved_ms": NOW - 30_000}),
            ),
            with(s("s-c", "idle"), json!({"moved_ms": NOW - 5 * 60_000})),
        ],
        "s-a",
    );
    let view = call(&f, &Value::Null, 100);
    let by_id = |id: &str| {
        sessions(&view)
            .into_iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("{id}"))
    };
    assert_eq!(cell(&by_id("s-a"), 2), "20m");
    assert_eq!(
        role(&by_id("s-a"), 2),
        Some("attention"),
        "twenty minutes into a turn with no row is the operator's decision"
    );
    assert_eq!(
        role(&by_id("s-b"), 2),
        Some("pending"),
        "thirty seconds is not"
    );
    assert_eq!(
        role(&by_id("s-c"), 2),
        Some("faint"),
        "an idle age is a fact"
    );
}

/// **A host that hands no feed gets a screen that says so** — an empty nick list and a broken host
/// look identical, and only one of them is the plugin's problem.
#[test]
fn a_host_with_no_feed_says_so() {
    let input = json!({"v": letibot_plugin::pane::V, "size": {"w": 100, "h": 40}});
    let bytes = serde_json::to_vec(&input).expect("the input");
    let answer = answer(&bytes).expect("the pane answered");
    let view: Value = serde_json::from_str(&answer).expect("a view");
    let said = view["panels"][0]["summary"][0]["tree"]["empty"]
        .as_str()
        .unwrap_or_default();
    assert!(said.contains("no `sessions` feed"), "{said}");
}

/// **A waiting session says so in its name**, because a switcher is exactly where being waited on
/// must not be missed.
#[test]
fn a_session_waiting_on_you_is_marked_in_its_name() {
    let f = feed_of(
        vec![with(
            s("s-a", "needs an answer"),
            json!({"heads": 1, "waits_on": 2}),
        )],
        "s-a",
    );
    let view = call(&f, &Value::Null, 100);
    let listed = sessions(&view);
    let name = cell(&listed[0], 1).trim_end();
    assert_eq!(name, "needs an answer!", "the `!` is the mark");
    assert!(
        view["legend"]["!"].is_string(),
        "and the legend says what it means"
    );
}

/// **A list with nothing in it is still a row** — an `empty` sentence would leave the pane with no
/// stop at all, and the keys that widen the column or the scope are on a stop.
#[test]
fn an_empty_feed_is_a_row_so_the_keys_stay_reachable() {
    // A daemon that has answered with nothing: the case a head sees before its first `Sessions`
    // frame lands, and the case a filter can leave behind.
    let f = feed_of(vec![], "s-gone");
    let view = call(&f, &Value::Null, 100);
    let rows = rows(&view);
    let empty = rows
        .iter()
        .find(|r| r["id"] == EMPTY)
        .unwrap_or_else(|| panic!("an empty-list row: {rows:?}"));
    assert!(
        empty["actions"].as_array().is_some_and(|a| !a.is_empty()),
        "the keys are on the row that says there is nothing"
    );
    assert!(
        cell(empty, 0).contains("no sessions here"),
        "and it says what it is: {:?}",
        cell(empty, 0)
    );
    // The header is still there, and it counts nothing rather than claiming a complete list.
    assert_eq!(cell(&rows[0], 0), "#all · 0 here · 0 working");
}

/// **At the minimum width the list is still a list** — the mark and the fact never go, the name is
/// cut, and nothing runs past the column's edge. This is the drawing itself, asserted, because a
/// column that degrades into a broken grid is the one thing a narrow pane cannot afford.
#[test]
fn the_minimum_width_keeps_the_mark_and_the_fact_and_cuts_the_name() {
    let f = feed_of(
        vec![
            with(
                s("s-a", "the habr article"),
                json!({"heads": 1, "running": true, "moved_ms": NOW - 30_000}),
            ),
            with(
                s("s-b", "the rust rewrite"),
                json!({"moved_ms": NOW - 3 * 60_000}),
            ),
        ],
        "s-a",
    );
    let view = call(&f, &json!({"w": MIN_W}), 100);
    let drawn: Vec<String> = rows(&view).iter().map(drawn).collect();
    assert_eq!(
        drawn,
        vec![
            "  #letibot 2/2 1+".to_string(),
            "  » the habr ar… now".to_string(),
            "    the rust rew… 3m".to_string(),
        ],
        "the column at its narrowest: {drawn:#?}"
    );
    for row in &drawn {
        assert!(cells(row) <= MIN_W, "nothing runs past the column: {row:?}");
    }
}

/// **The divider is fitted too**: a scope that hides sessions says how many, and at the minimum it
/// keeps that count rather than a sentence cut in the middle of a word.
#[test]
fn the_divider_keeps_the_count_when_the_words_do_not_fit() {
    let f = feed_of(
        vec![
            with(
                s("s-a", "here"),
                json!({"workspace": "/p/letibot", "heads": 1}),
            ),
            with(
                s("s-b", "elsewhere, working"),
                json!({"workspace": "/p/other", "running": true}),
            ),
            with(s("s-c", "elsewhere"), json!({"workspace": "/p/other"})),
            with(s("s-d", "elsewhere"), json!({"workspace": "/p/other"})),
        ],
        "s-a",
    );
    let wide = call(&f, &Value::Null, 100);
    assert!(
        rows(&wide)
            .iter()
            .any(|r| cell(r, 0) == "· 3 elsewhere · 1 working"),
        "the whole sentence at a comfortable width"
    );
    let narrow = call(&f, &json!({"w": MIN_W}), 100);
    assert!(
        rows(&narrow).iter().any(|r| cell(r, 0) == "· 3 elsewhere"),
        "and the count that matters at the minimum: {:?}",
        rows(&narrow).iter().map(|r| cell(r, 0)).collect::<Vec<_>>()
    );
}
