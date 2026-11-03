//! The queue layer's policy, tested natively - no engine, no wasm, no store.
//!
//! Every test here is a claim the operator made about the pane it replaces: live work fills the
//! frame and the past tense does not, a row says the one verb that would do something, the id is
//! under the cursor and nowhere else, a row's act names the entry it is about, and the bar and the
//! row agree about what `d` is called.

use super::*;
use serde_json::json;

/// One live review, as the host's feed spells it.
fn a_review(id: &str, state: &str, live: bool) -> Value {
    let verbs = if live {
        json!([
            {"verb": "approve", "why": ""},
            {"verb": "veto", "why": ""},
            {"verb": "restart", "why": "it is `reviewing`"},
            {"verb": "rm", "why": ""},
        ])
    } else {
        json!([
            {"verb": "approve", "why": "it has landed"},
            {"verb": "veto", "why": "it has landed"},
            {"verb": "restart", "why": "it has landed"},
            {"verb": "rm", "why": "it has landed"},
        ])
    };
    json!({
        "queue": "review", "id": id, "kind": "branch", "live": live,
        "state": state, "state_words": state.replace('_', " "),
        "mark": if live { "[~]" } else { "[x]" },
        "in_state_ms": 240_000u64, "age_ms": 300_000u64,
        "repository": "letibot", "target": "main",
        "branch": format!("agent/{id}"),
        "owner": "mac/4242", "owner_is_serving": true,
        "reviewer": format!("s-rev-{id}"), "head_sha": "1f2a3b4c5d6e", "short_sha": "1f2a3b4c5d",
        "verdict": null, "verdict_by": null, "attempts": 0, "failure": "",
        "reasons": [], "evidence": format!("why {id} is {state}"), "brief": "the ask",
        "fixes": null, "deadline_ms": null, "verbs": verbs,
    })
}

/// One live landing.
fn a_merge(id: &str, state: &str, live: bool) -> Value {
    json!({
        "queue": "merge", "id": id, "review_id": format!("r-{id}"), "live": live,
        "state": state, "state_words": state.replace('_', " "),
        "mark": if live { "[~]" } else { "[x]" },
        "in_state_ms": 120_000u64, "age_ms": 200_000u64,
        "repository": "letibot", "target": "main", "branch": format!("agent/{id}"),
        "owner": "", "owner_is_serving": false,
        "approved_sha": "aaaa1111bbbb", "short_sha": "aaaa1111bb",
        "approved_by": "the reviewer", "rebased_sha": null, "landed_sha": null, "pushed": "",
        "evidence": format!("why {id} is {state}"), "fixes": null,
        "waits_on": if state == "queued" { json!("gate running on 1f2a3b4 for 2m") } else { Value::Null },
        "gate_steps": [],
        "verbs": [
            {"verb": "approve", "why": "a merge row is approved already"},
            {"verb": "veto", "why": ""},
            {"verb": "restart", "why": "it is `queued`"},
            {"verb": "rm", "why": ""},
        ],
    })
}

/// **The fixture the operator's screenshot is about**: two rows the queue is working on and twelve
/// it has finished with.
fn a_queue() -> Value {
    let mut reviews = vec![
        a_review("r-live-1", "reviewing", true),
        a_review("r-live-2", "waiting", true),
    ];
    for i in 0..8 {
        reviews.push(a_review(&format!("r-done-{i}"), "landed", false));
    }
    let mut merges = vec![a_merge("m-live", "gating", true)];
    for i in 0..4 {
        merges.push(a_merge(&format!("m-done-{i}"), "landed", false));
    }
    json!({
        "now_ms": 1_700_100u64, "at_ms": 1_700_000u64,
        "scope": {"kind": "here", "several": false,
                  "words": "this session's repository — letibot",
                  "repositories": ["letibot"]},
        "view": "review", "unreadable": null, "archived": 0, "not_queued": [],
        "reviews": reviews, "merges": merges,
        "targets": [{"repository": "letibot", "target": "main", "sha": "9999", "short_sha": "9999888877",
                     "state": "red", "state_words": "red", "since_ms": 50_100u64,
                     "evidence": "the gate's step 2 failed", "gate_steps": []}],
        "leases": [{"repository": "letibot", "owner": "mac/4242", "pid": 4242, "host": "mac",
                    "build": "0.5.1", "heartbeat_ms": 4_000u64, "stale": false}],
        "recent": [],
        "counts": {"review": 2, "merge": 1, "finished": 12, "red_targets": 1,
                   "unjudged_targets": 0, "stale_leases": 0},
        "cursor": "",
    })
}

/// **One line of the chrome**, by its label - the `lines` block a panel draws above the rows.
fn chrome_line<'a>(view: &'a Value, label: &str) -> Option<&'a Value> {
    view["panels"].as_array()?.iter().find_map(|p| {
        p["summary"].as_array()?.iter().find_map(|b| {
            b.get("lines")?
                .as_array()?
                .iter()
                .find(|l| l["label"] == label)
        })
    })
}

/// The view the layer answers for a feed, as JSON - the hook's own door, minus the wasm arena.
fn view_of(feed: &Value, folded: &[(&str, bool)]) -> Value {
    let input = json!({
        "v": letibot_plugin::pane::V,
        "size": {"w": 120, "h": 40},
        "feeds": {"queue": feed},
        "folded": folded.iter().cloned().collect::<std::collections::BTreeMap<&str, bool>>(),
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

/// A row's `cells`, as strings.
fn cells_of(row: &Value) -> Vec<String> {
    row["cells"]
        .as_array()
        .map(|cs| cs.iter().map(text_of).collect())
        .unwrap_or_default()
}

/// Every row of every panel, in the order they are drawn.
fn rows(view: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    for p in view["panels"].as_array().expect("panels") {
        for b in p["summary"].as_array().expect("summary") {
            if let Some(t) = b.get("tree")
                && let Some(rs) = t["rows"].as_array()
            {
                out.extend(rs.iter());
            }
        }
    }
    out
}

/// **The operator's screenshot, fixed**: the frame is the live rows and ONE folded line saying how
/// many finished rows there are. Twelve history rows are not free rows competing with two live ones.
#[test]
fn live_rows_fill_the_frame_and_history_is_one_folded_line() {
    let feed = a_queue();
    let view = view_of(&feed, &[]);
    let drawn = rows(&view);
    let ids: Vec<&str> = drawn.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec!["r-live-1", "r-live-2", "m-live", "finished"],
        "the three live rows, then the folded history line — and no finished row among them"
    );
    // The folded line says the count, and says it is a fold.
    let history = drawn.last().expect("the history row");
    let cells = cells_of(history);
    assert!(
        cells.iter().any(|c| c.contains("finished: 12 rows")),
        "the count is on the line: {cells:?}"
    );
    assert_eq!(history["open"], false);
    assert_eq!(
        history["enter"],
        json!({"fold": true}),
        "and Enter folds it, which the host's `folded` map keeps"
    );
    // **And nothing of the twelve is drawn** - this is the assertion the screenshot was about.
    assert!(
        !view.to_string().contains("r-done-"),
        "a finished row is behind the fold, not on the frame"
    );
    // The panel names both queues' counts, the drawn view first, so `tab` has something to
    // switch and the reader can tell which half they are looking at.
    let titles: Vec<&str> = view["panels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap_or_default())
        .collect();
    assert!(
        titles.contains(&"2 in review · 1 to land"),
        "{titles:?}"
    );
}

/// **The strip follows the view**: `tab` puts the merge queue first, so the header and the rows
/// agree about which half is drawn.
#[test]
fn the_strip_names_the_drawn_view_first() {
    let mut feed = a_queue();
    feed["view"] = json!("merge");
    let view = view_of(&feed, &[]);
    let titles: Vec<&str> = view["panels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap_or_default())
        .collect();
    assert!(
        titles.contains(&"1 to land · 2 in review"),
        "{titles:?}"
    );
}

/// **The empty sentence belongs to the view** - the two queues arrive differently, and one
/// sentence for both said nothing about which half was empty.
#[test]
fn the_empty_sentence_is_the_views_own() {
    for (view, want) in [
        (
            "review",
            "a branch is put up for review when a `task_start` child finishes",
        ),
        ("merge", "a branch lands here once its review approves it"),
    ] {
        let mut feed = a_queue();
        feed["view"] = json!(view);
        feed["reviews"] = json!([]);
        feed["merges"] = json!([]);
        feed["counts"] = json!({"review": 0, "merge": 0, "finished": 0});
        let text = view_of(&feed, &[]).to_string();
        assert!(text.contains(want), "{view}: {text}");
    }
}

/// **The fold, opened**: the history rows appear, and the same line says how to close it.
#[test]
fn opening_the_fold_draws_the_history_and_says_how_to_close_it() {
    let feed = a_queue();
    let view = view_of(&feed, &[("finished", true)]);
    let drawn = rows(&view);
    assert!(
        drawn.len() > 4,
        "the finished rows are drawn when the fold is open: {}",
        drawn.len()
    );
    let history = drawn
        .iter()
        .find(|r| r["id"] == "finished")
        .expect("the history line is still the fold");
    assert_eq!(history["open"], true);
    let cells = cells_of(history);
    assert!(
        cells.iter().any(|c| c.contains("enter hides them")),
        "an open fold says how to close it: {cells:?}"
    );
}

/// A row says the one verb that would do something - `approve · veto · rm` on a `reviewing` row,
/// and on a row nothing would move, the verb you would reach for with the reason it would not. One
/// phrase per row, and the legend has the meaning.
#[test]
fn a_row_says_the_verb_that_would_move_it_and_why_when_none_would() {
    let feed = a_queue();
    let view = view_of(&feed, &[]);
    let drawn = rows(&view);
    let live = drawn
        .iter()
        .find(|r| r["id"] == "r-live-1")
        .expect("a live row");
    let facts = facts_of(live);
    assert_eq!(
        facts,
        vec![
            "for 4m",
            "you can approve, veto or withdraw it"
        ],
        "the actions that WOULD move it, and `restart` left out because it would not"
    );

    // **A row nothing would move** says why, in the same one phrase.
    let mut done = a_review("r-old", "landed", false);
    done["verbs"] = json!([
        {"verb": "approve", "why": "it has landed"},
        {"verb": "veto", "why": "it has landed"},
        {"verb": "restart", "why": "it has landed"},
        {"verb": "rm", "why": "it has landed"},
    ]);
    let mut feed = a_queue();
    feed["reviews"] = json!([done]);
    feed["merges"] = json!([]);
    feed["counts"]["finished"] = json!(1);
    let view = view_of(&feed, &[("finished", true)]);
    let drawn = rows(&view);
    let old = drawn.iter().find(|r| r["id"] == "r-old").expect("the row");
    let facts = facts_of(old);
    assert!(
        facts.iter().any(|f| f == "nothing to do: it has landed"),
        "the action they would reach for, with the daemon's own reason: {facts:?}"
    );
}

/// The `facts` column of a row, as strings.
fn facts_of(row: &Value) -> Vec<String> {
    row["facts"]
        .as_array()
        .map(|fs| {
            fs.iter()
                .map(|f| match f {
                    Value::String(s) => s.clone(),
                    other => other["text"].as_str().unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// **The address is under the cursor and on no other row** - the id exists so it can be typed into
/// `/queue approve`, and repeating it on every row was half of what made the pane illegible.
#[test]
fn the_address_is_under_the_cursor_and_on_no_other_row() {
    let mut feed = a_queue();
    feed["cursor"] = json!("r-live-2");
    let view = view_of(&feed, &[]);
    let drawn = rows(&view);
    let picked = drawn.iter().find(|r| r["id"] == "r-live-2").expect("a row");
    let under: Vec<&str> = picked["under"]
        .as_array()
        .expect("the picked row has an under line")
        .iter()
        .map(|u| u.as_str().unwrap_or_default())
        .collect();
    assert!(
        under
            .iter()
            .any(|u| u.starts_with("row r-live-2 · the head it judged 1f2a3b4c5d")),
        "the id and sha are under the cursor: {under:?}"
    );
    // **No other row carries its id.**
    for row in drawn.iter().filter(|r| r["id"] != "r-live-2") {
        let text = row.to_string();
        let id = row["id"].as_str().unwrap();
        assert!(
            !text.contains(&format!("\"row {id} · the head it judged")),
            "`{id}` prints its address on a row that is not picked: {text}"
        );
    }
    // And a row that IS the cursor's is the only one - an empty cursor means nobody's.
    let plain = view_of(&a_queue(), &[]);
    assert!(
        !plain.to_string().contains(" · the head it judged "),
        "with no row picked, no row prints an address"
    );
}

/// **Every live row's act names ITS OWN entry** - the whole of `Act::Verb`, and the reason a plugin
/// row can now say which entry the person's `a`/`v`/`r`/`d` mean.
#[test]
fn every_live_row_names_its_own_entry() {
    let view = view_of(&a_queue(), &[]);
    for row in rows(&view) {
        let id = row["id"].as_str().unwrap();
        if id == "finished" {
            assert_eq!(
                row["enter"],
                json!({"fold": true}),
                "the history line folds; it is not an entry"
            );
            continue;
        }
        let enter = row
            .get("enter")
            .unwrap_or_else(|| panic!("{id} has no act"));
        assert_eq!(
            enter["id"], id,
            "the act names the entry the row IS, not another: {enter}"
        );
        let verb = enter["verb"].as_str().expect("a verb word");
        assert!(
            ["approve", "veto", "restart", "rm"].contains(&verb),
            "`{verb}` is one of the four the manifest declares"
        );
    }
}

/// **The verbs are the daemon's words**, so the row and the key bar cannot disagree - the operator's
/// `d drop` against `d rm` was two names for one key on one screen.
#[test]
fn the_verbs_are_the_daemons_words() {
    let view = view_of(&a_queue(), &[]);
    let text = view.to_string();
    assert!(!text.contains("\"drop\""), "the pane never says `drop`");
    assert!(text.contains("approve"), "and it says the daemon's words");
    // **The legend is where the meaning lives**, once, instead of on every row, and it names the
    // action a person would use - `withdraw`, the row's word, not the head's `rm`.
    let legend = view["legend"].as_object().expect("a legend");
    for verb in ["approve", "veto", "restart", "withdraw"] {
        assert!(legend.contains_key(verb), "the legend documents `{verb}`");
    }
    // **No row carries the legend's prose.** A row says `approve`; the sentence is the legend's.
    for row in rows(&view) {
        let text = row.to_string();
        assert!(
            !text.contains("your approval over the reviewer"),
            "a row repeated the legend: {text}"
        );
    }
}

/// **A red target is an alarm and its own review is history** - the parent's correction: a
/// `RED TARGET` row that was withdrawn or superseded is not an alarm, and the alarm is the TARGET's
/// health, on its own line.
#[test]
fn a_red_target_is_a_line_of_its_own_and_its_old_review_is_history() {
    let mut feed = a_queue();
    // The target's own review, withdrawn: history, and it must not be one of the live rows.
    let mut stale = a_review("r-target-old", "withdrawn", false);
    stale["kind"] = json!("target");
    stale["target"] = json!("main");
    feed["reviews"].as_array_mut().unwrap().push(stale);
    let view = view_of(&feed, &[]);
    let drawn = rows(&view);
    assert!(
        !drawn.iter().any(|r| r["id"] == "r-target-old"),
        "a withdrawn target review is behind the fold, not on the frame"
    );
    // The alarm is the target's health, in the chrome above the rows, in words.
    let text = view.to_string();
    assert!(text.contains("main in letibot is red"), "{text}");
    assert!(text.contains("the gate's step 2 failed"), "{text}");
}

/// **A stale lease is the alarm the operator asked for** - `STALE` and how long it has been silent.
#[test]
fn a_stale_lease_says_so_and_names_the_lifetime_it_passed() {
    let mut feed = a_queue();
    feed["leases"][0]["stale"] = json!(true);
    feed["leases"][0]["heartbeat_ms"] = json!(100_100u64);
    let view = view_of(&feed, &[]);
    let stale = chrome_line(&view, "nobody is serving letibot").expect("the alarm line");
    assert_eq!(
        stale["value"], "mac (pid 4242, 0.5.1) stopped saying it was alive",
        "who has gone quiet"
    );
    assert_eq!(stale["tail"], "1m ago", "the age of the heartbeat");
    assert_eq!(stale["role"], "attention");
}

/// **A healthy lease is drawn too, with its owner and a fresh heartbeat** - the half the first cut
/// dropped: the pane spoke only when a lease was STALE, so *nobody is serving this* and *it is
/// served and well* looked the same, and the host pane this replaces said both.
#[test]
fn a_healthy_lease_names_its_owner_and_its_heartbeat() {
    let view = view_of(&a_queue(), &[]);
    let lease = chrome_line(&view, "letibot is served by mac (pid 4242, 0.5.1)")
        .expect("a line for the repository");
    assert_eq!(
        lease["value"], "its last heartbeat",
        "the holder is on the label, so the value is the one fact left"
    );
    assert_eq!(lease["tail"], "4s ago", "and how long since it last said so");
    // **And no stale line** - the legend names the word, so the chrome is what is asked.
    assert!(
        chrome_line(&view, "nobody is serving letibot").is_none(),
        "a fresh lease is not an alarm"
    );
}

/// **No lease at all is a sentence, not an absence** - *nothing is serving the queues* is the fact
/// a person needs, and a chrome that draws nothing when it has nothing says neither.
#[test]
fn no_lease_says_nothing_is_serving_the_queues() {
    let mut feed = a_queue();
    feed["leases"] = json!([]);
    let view = view_of(&feed, &[]);
    let text = view.to_string();
    assert!(
        text.contains("no daemon holds this repository's queue lease"),
        "{text}"
    );
}

/// **A row names its repository when the scope holds more than one** - and only then. The answer is
/// the host's (`scope.several`), never a reading of the scope's prose.
#[test]
fn a_row_names_its_repository_only_when_several_are_in_scope() {
    let mut feed = a_queue();
    feed["scope"]["several"] = json!(true);
    feed["scope"]["words"] = json!("/home/dead/Projects — named");
    let view = view_of(&feed, &[]);
    let drawn = rows(&view);
    let live = drawn
        .iter()
        .find(|r| r["id"] == "r-live-1")
        .expect("a live row");
    assert!(
        facts_of(live).iter().any(|f| f == "in letibot"),
        "the repository is on the row: {:?}",
        facts_of(live)
    );
    // **And with one repository in scope it is not repeated** - even when the words contain a
    // separator, which is exactly what the prose-reading cut got wrong.
    let mut one = a_queue();
    one["scope"]["several"] = json!(false);
    one["scope"]["words"] = json!("this session's repositories, one project — letibot");
    let view = view_of(&one, &[]);
    let drawn = rows(&view);
    let live = drawn
        .iter()
        .find(|r| r["id"] == "r-live-1")
        .expect("a live row");
    assert!(
        !facts_of(live).iter().any(|f| f == "in letibot"),
        "one repository, so the title says it: {:?}",
        facts_of(live)
    );
}

/// **A queued landing says what it is waiting for**, in the pane's own words.
#[test]
fn a_queued_landing_says_what_it_waits_for() {
    let mut feed = a_queue();
    feed["merges"] = json!([a_merge("m-queued", "queued", true)]);
    let view = view_of(&feed, &[]);
    let drawn = rows(&view);
    let m = drawn
        .iter()
        .find(|r| r["id"] == "m-queued")
        .expect("the row");
    let under: Vec<&str> = m["under"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u.as_str().unwrap_or_default())
        .collect();
    assert!(
        under[0].starts_with("waits for the gate: gate running on 1f2a3b4 for 2m"),
        "{under:?}"
    );
}

/// **No queue is a sentence, not an empty frame** - *nothing is in flight* and *the head has not
/// asked yet* are different facts, and only the layer knows which its list is.
#[test]
fn an_idle_queue_says_so_rather_than_drawing_nothing() {
    let mut feed = a_queue();
    feed["reviews"] = json!([]);
    feed["merges"] = json!([]);
    feed["counts"] = json!({"review": 0, "merge": 0, "finished": 0, "red_targets": 0,
                            "unjudged_targets": 0, "stale_leases": 0});
    let view = view_of(&feed, &[]);
    let titles: Vec<&str> = view["panels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap_or_default())
        .collect();
    assert!(titles.contains(&"nothing in either queue"), "{titles:?}");
    let text = view.to_string();
    assert!(
        text.contains("nothing is in flight"),
        "the tree's own empty sentence: {text}"
    );
}

/// **A row is one line**: the cells are the state, what it is about, and the state's words - and
/// the reason is the faint line under it, which is the shape the declutter branch established.
#[test]
fn a_row_is_one_line_with_the_reason_under_it() {
    let view = view_of(&a_queue(), &[]);
    let drawn = rows(&view);
    let row = drawn.iter().find(|r| r["id"] == "r-live-1").expect("a row");
    let cells = cells_of(row);
    assert_eq!(cells[0], "agent/r-live-1", "what it is about");
    assert_eq!(
        cells[1], "a reviewer is reading it",
        "the state as a sentence, not a mark and a code"
    );
    let under: Vec<&str> = row["under"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u.as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        under,
        vec!["why r-live-1 is reviewing"],
        "the reason, and nothing else — no address on an unpicked row"
    );
}

/// **A target's own review reads as a target**, never as a branch - `main's own tip`, in words,
/// because `RED TARGET main` was a code a newcomer had to be told.
#[test]
fn a_target_review_says_red_target() {
    let mut feed = a_queue();
    let mut t = a_review("r-tip", "reviewing", true);
    t["kind"] = json!("target");
    t["target"] = json!("main");
    feed["reviews"] = json!([t]);
    feed["merges"] = json!([]);
    let view = view_of(&feed, &[]);
    let drawn = rows(&view);
    let row = drawn.iter().find(|r| r["id"] == "r-tip").expect("the row");
    let cells = cells_of(row);
    assert_eq!(cells[0], "main's own tip", "{cells:?}");
}

/// A `gating` row says which step its gate is on, and how long it has been there. The tail itself
/// is not in the feed: a guest cannot read a file, and a row is one line - the detail the host
/// draws on Enter carries it.
#[test]
fn a_gating_row_says_which_step_its_gate_is_on() {
    let mut feed = a_queue();
    let mut row = a_merge("m-gating", "gating", true);
    row["waits_on"] = Value::Null;
    row["running_gate"] = json!({
        "command": "cargo test --offline",
        "step": 2,
        "of": 4,
        "running_ms": 360_000u64,
        "bytes": 12_345u64,
    });
    feed["merges"] = json!([row]);
    feed["reviews"] = json!([]);
    let view = view_of(&feed, &[]);
    let drawn = rows(&view);
    let under = drawn
        .iter()
        .find(|r| r["id"] == "m-gating")
        .expect("the landing")["under"]
        .as_array()
        .expect("an under line")
        .iter()
        .map(|u| u.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(
        under
            .iter()
            .any(|u| u == "the gate is on step 2 of 4 · 12 KB written"),
        "the step, its place, and how much it has written: {under:?}"
    );
}

/// **A `gating` row's `rm` is the verb that acts**, now that the queue lets a running landing be
/// taken out of line - a row that offered only `restart (the queue is gating it right now)` had no
/// way out of the one state a queue gets stuck in.
#[test]
fn a_gating_rows_verb_is_withdraw() {
    let mut feed = a_queue();
    let mut row = a_merge("m-gating", "gating", true);
    row["verbs"] = json!([
        {"verb": "approve", "why": "a merge row is approved already"},
        {"verb": "veto", "why": "the queue is gating it right now"},
        {"verb": "restart", "why": "the queue is gating it right now"},
        {"verb": "rm", "why": ""},
    ]);
    feed["merges"] = json!([row]);
    feed["reviews"] = json!([]);
    let view = view_of(&feed, &[]);
    let drawn = rows(&view);
    let facts = facts_of(
        drawn
            .iter()
            .find(|r| r["id"] == "m-gating")
            .expect("the row"),
    );
    assert!(
        facts.iter().any(|f| f == "you can withdraw it"),
        "the action that would move it: {facts:?}"
    );
}

// -- the cases the head's own pane drew, ported into this layer --------------------------

/// A target in the given state.
fn a_target(state: &str) -> Value {
    json!({
        "repository": "letibot", "repository_path": "/home/dead/Projects/letibot",
        "target": "main", "sha": "fedcba9876543210", "short_sha": "fedcba9876",
        "state": state, "state_words": state.replace('_', " "),
        "since_ms": 240_000u64, "evidence": format!("main is {state}"), "gate_steps": [],
    })
}

/// **A gating target is a line of its own** - the fact every queued landing of that repository
/// waits for, which no row of either queue carries.
#[test]
fn a_gating_target_says_its_gate_is_running() {
    let mut feed = a_queue();
    feed["targets"] = json!([a_target("gating")]);
    let view = view_of(&feed, &[]);
    let line = chrome_line(&view, "main in letibot is being gated").expect("the gating target's line");
    assert_eq!(line["value"], "the gate is running on fedcba9876 for 4m");
    assert_eq!(line["role"], "pending");
}

/// **A green target is drawn, quietly** - where the target stands. A pane that never said it
/// left the reader to infer health from the absence of a line.
#[test]
fn a_green_target_says_where_it_stands() {
    let mut feed = a_queue();
    feed["targets"] = json!([a_target("green")]);
    let view = view_of(&feed, &[]);
    let line = chrome_line(&view, "main in letibot").expect("the green target's line");
    assert_eq!(line["value"], "green at fedcba9876 - branches land on it");
    assert_eq!(line["role"], "faint");
}

/// **A gating landing between two steps still says the gate is on it** - the sha it was rebased
/// to and how long the state has stood, which is all the snapshot holds between steps.
#[test]
fn a_gating_landing_between_steps_says_the_gate_is_on_it() {
    let mut feed = a_queue();
    let mut row = a_merge("m-gating", "gating", true);
    row["waits_on"] = Value::Null;
    row["rebased_sha"] = json!("fedcba9876543210");
    feed["merges"] = json!([row]);
    feed["reviews"] = json!([]);
    let view = view_of(&feed, &[]);
    let under = rows(&view)
        .iter()
        .find(|r| r["id"] == "m-gating")
        .expect("the row")["under"]
        .as_array()
        .expect("an under line");
    assert!(
        under
            .iter()
            .any(|u| u.as_str().unwrap_or_default()
                == "the gate is running on fedcba9876 for 2m"),
        "{under:?}"
    );
}

/// One move of one entry, `ago_ms` milliseconds ago.
fn a_move(entry: &str, from: Option<&str>, to: &str, ago_ms: u64) -> Value {
    json!({
        "queue": "review", "entry_id": entry, "repository": "letibot",
        "from": from, "to": to, "ago_ms": ago_ms, "by": "mac/4242",
        "why": format!("{entry} to {to}"),
    })
}

/// The `detail` prose of one drawn row, owned - a test's one door on a row read whole.
fn detail_of(feed: &Value, id: &str) -> String {
    let view = view_of(feed, &[]);
    rows(&view)
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("{id} is not drawn"))["detail"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The `under` lines of one drawn row, owned.
fn under_of(feed: &Value, id: &str) -> Vec<String> {
    let view = view_of(feed, &[]);
    rows(&view)
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("{id} is not drawn"))["under"]
        .as_array()
        .map(|us| {
            us.iter()
                .map(|u| u.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// **The picked row carries its last three moves**, newest last - the recent history a person
/// reads to know the row has not always been where it is.
#[test]
fn the_picked_row_carries_its_last_three_moves() {
    let mut feed = a_queue();
    feed["cursor"] = json!("r-live-1");
    feed["recent"] = json!([
        a_move("r-live-1", None, "waiting", 300_000),
        a_move("r-live-1", Some("waiting"), "reviewing", 240_000),
        a_move("r-live-1", Some("reviewing"), "blocked", 180_000),
        a_move("r-live-1", Some("blocked"), "reviewing", 120_000),
        a_move("r-live-2", None, "waiting", 60_000),
    ]);
    let under = under_of(&feed, "r-live-1");
    let moves: Vec<&String> = under.iter().filter(|u| u.contains("→")).collect();
    assert_eq!(moves.len(), 3, "the last three, not all five: {under:?}");
    assert!(moves[0].contains("waiting for a reviewer → a reviewer is reading it"), "{under:?}");
    assert!(moves[2].contains("stopped: a person has to act → a reviewer is reading it"), "{under:?}");
    assert!(moves[2].contains("2m ago"), "{under:?}");
    assert!(moves.iter().all(|m| !m.contains("r-live-2")), "{under:?}");
}

/// **A review read whole**: every field, the failure with no attempt to its name, the verdict
/// with its sha and author, and the ask the row was produced under.
#[test]
fn a_review_detail_carries_every_field() {
    let mut feed = a_queue();
    let mut row = a_review("r-live-1", "waiting", true);
    row["failure"] =
        json!("WAITING FOR THE REVIEWER'S TREE: the clone was refused, no attempt is spent on it");
    row["attempts"] = json!(0);
    row["verdict"] = json!("rejected");
    row["verdict_by"] = json!("the operator");
    row["reviewed_sha"] = json!("abc1234567890");
    feed["reviews"] = json!([row]);
    feed["merges"] = json!([]);
    feed["recent"] = json!([a_move("r-live-1", None, "waiting", 300_000)]);
    let detail = detail_of(&feed, "r-live-1");
    for want in [
        "this is    a branch put up for review",
        "state      waiting for a reviewer · for 4m",
        "head       1f2a3b4c5d6e",
        "verdict    the operator reviewed agent/r-live-1 at abc1234567 and rejected",
        "failure    WAITING FOR THE REVIEWER'S TREE: the clone was refused",
        "the ask    the ask",
        "its moves",
        "(new) → waiting for a reviewer · 5m ago · mac/4242 said: r-live-1 to waiting",
    ] {
        assert!(detail.contains(want), "missing `{want}`:\n{detail}");
    }
    assert!(!detail.contains("attempts"), "zero is not a fact: {detail}");
}

/// **A landing read whole**: the approval with its author and review, the gate's steps with the
/// whole output of the one that failed, and every move.
#[test]
fn a_merge_detail_carries_the_gate_and_its_steps() {
    let mut feed = a_queue();
    let mut row = a_merge("m-live", "gating", true);
    row["waits_on"] = Value::Null;
    row["approved_by"] = json!("the reviewer");
    row["review_id"] = json!("r-live-9");
    row["gate_steps"] = json!([
        {"command": "cargo clippy", "outcome": "passed", "failed": false,
         "elapsed_ms": 61_000u64, "output": ""},
        {"command": "cargo test", "outcome": "timed out", "failed": true,
         "elapsed_ms": 1_800_000u64, "output": "still compiling\nno reply"},
        {"command": "cargo build", "outcome": "not run", "failed": false,
         "elapsed_ms": 0u64, "output": ""},
    ]);
    feed["merges"] = json!([row]);
    feed["reviews"] = json!([]);
    feed["recent"] = json!([a_move("m-live", None, "queued", 300_000)]);
    let detail = detail_of(&feed, "m-live");
    for want in [
        "this is    a landing: an approved branch",
        "approved   the reviewer approved it at aaaa1111bbbb (review r-live-9)",
        "the gate's steps",
        "passed: cargo clippy · 1m",
        "timed out: cargo test · 30m",
        "  still compiling",
        "  no reply",
        "not run: cargo build · 0s",
        "its moves",
        "(new) → waiting for the gate · 5m ago · mac/4242 said: m-live to queued",
    ] {
        assert!(detail.contains(want), "missing `{want}`:\n{detail}");
    }
}

/// A running gate as the feed carries it, with `still_ms` since its tail last changed.
fn a_running_gate(step: usize, of: usize, still_ms: u64, tail: &str) -> Value {
    json!({
        "command": "cargo test --offline --workspace",
        "step": step, "of": of, "running_ms": 360_000u64,
        "bytes": 12_345u64, "tail": tail, "still_ms": still_ms,
    })
}

/// **The three verdicts about a tail**: it moved, it has not moved in ten minutes, and there is
/// no snapshot new enough to say either. The last must not be dressed up as *stuck*.
#[test]
fn the_detail_of_a_running_gate_says_whether_it_moved() {
    let mut feed = a_queue();
    let mut row = a_merge("m-gating", "gating", true);
    row["waits_on"] = Value::Null;
    row["running_gate"] = a_running_gate(2, 4, 5_000, "building\nlinking");
    feed["merges"] = json!([row]);
    feed["reviews"] = json!([]);
    // **The snapshot is fresh**: the tail moved five seconds ago.
    let moved = detail_of(&feed, "m-gating");
    assert!(
        moved.contains("gate       step 2 of 4 · running 6m"),
        "{moved}"
    );
    assert!(
        moved.contains("step       cargo test --offline --workspace"),
        "{moved}"
    );
    assert!(
        moved.contains("moving     the tail moved 5s ago · 12 KB written"),
        "{moved}"
    );
    assert!(
        moved.contains("tail       the end of what it has written"),
        "{moved}"
    );
    assert!(moved.contains("  linking"), "{moved}");

    // **Ten minutes of the same bytes**: it may be stuck.
    feed["merges"][0]["running_gate"] = a_running_gate(2, 4, 20 * 60_000, "building");
    let stuck = detail_of(&feed, "m-gating");
    assert!(
        stuck.contains("the tail has not moved in 20m - it may be stuck"),
        "{stuck}"
    );

    // **No snapshot for half a minute**: the daemon has not replied, and the head's own silence
    // is not a fact about the gate.
    feed["at_ms"] = json!(0u64);
    let silent = detail_of(&feed, "m-gating");
    assert!(silent.contains("no snapshot for 28m"), "{silent}");
    assert!(!silent.contains("may be stuck"), "{silent}");

    // **Nothing written yet** is said as itself.
    feed["merges"][0]["running_gate"] = a_running_gate(1, 4, 0, "");
    feed["at_ms"] = json!(1_700_000u64);
    let empty = detail_of(&feed, "m-gating");
    assert!(empty.contains("tail       nothing written yet"), "{empty}");
    assert!(
        empty.contains("moving     the tail moved just now · 12 KB written"),
        "{empty}"
    );
}

/// **A queued landing's detail reads the TARGET's gate**, named as whose step it is - the other
/// way a queue sits still with nothing failing.
#[test]
fn a_queued_landings_detail_waits_on_the_targets_gate() {
    let mut feed = a_queue();
    let mut target = a_target("gating");
    target["running_gate"] = a_running_gate(3, 5, 4_000, "test result: ok. 18 passed");
    feed["targets"] = json!([target]);
    let mut row = a_merge("m-queued", "queued", true);
    row["repository_path"] = json!("/home/dead/Projects/letibot");
    feed["merges"] = json!([row]);
    feed["reviews"] = json!([]);
    let detail = detail_of(&feed, "m-queued");
    assert!(
        detail.contains("gate       waiting on the target's gate: step 3 of 5 · running 6m"),
        "{detail}"
    );
    assert!(detail.contains("  test result: ok. 18 passed"), "{detail}");
}

/// **The row whose detail is open is drawn even when it is history**, so the overlay the host
/// holds outlives the row landing or being withdrawn under it.
#[test]
fn the_open_row_is_drawn_even_when_it_is_history() {
    let mut feed = a_queue();
    feed["open"] = json!("r-done-0");
    let open_view = view_of(&feed, &[]);
    let drawn = rows(&open_view);
    assert!(
        drawn.iter().any(|r| r["id"] == "r-done-0"),
        "the open row is among the drawn ones: {:?}",
        drawn.iter().map(|r| r["id"].clone()).collect::<Vec<_>>()
    );
    // And it carries its detail like any drawn row.
    let open = drawn
        .iter()
        .find(|r| r["id"] == "r-done-0")
        .expect("the row");
    assert!(
        open["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("this is    a branch put up for review")
    );
    // With no row open, history stays behind the fold.
    feed["open"] = json!("");
    let closed_view = view_of(&feed, &[]);
    let drawn = rows(&closed_view);
    assert!(!drawn.iter().any(|r| r["id"] == "r-done-0"));
}
