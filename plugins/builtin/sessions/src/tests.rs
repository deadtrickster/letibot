//! **The session list's policy, tested where it lives.**
//!
//! This is the copy of the grouping rule and of a row's words that a project can override, so it
//! is the copy that needs tests of its own: the host-side tests say *the list draws what the head
//! draws*, and these say *the list is the one the plan describes* — `mine` above every fold, a
//! group that cannot fold carrying no act, a session that waits on you saying so, the numbers
//! running over the sessions alone.
//!
//! They run natively (`cargo test` in this directory): the module is a `cdylib` for the host and
//! an `rlib` for these, and none of the logic below knows that a wasm ABI exists.

use super::*;

/// One session in a feed, with the defaults an ordinary row has.
fn session(id: &str, title: &str) -> Session {
    Session {
        id: id.to_string(),
        title: title.to_string(),
        slug: String::new(),
        parent: None,
        origin: "operator".into(),
        role: None,
        live: true,
        running: false,
        heads: 1,
        items: 6,
        stored: 0,
        model: "qwen".into(),
        workspace: String::new(),
        waits_on: 0,
        nags_paused: false,
    }
}

/// The answer for a feed, parsed back into the rows this layer draws.
fn draw(feed: &Feed, folded: &[(&str, bool)]) -> Vec<Out> {
    let mut path = vec![feed.here.clone()];
    let mut cur = feed.here.clone();
    for _ in 0..MAX_DEPTH {
        match feed.sessions.iter().find(|s| s.id == cur) {
            Some(s) => match s.parent.clone() {
                Some(p) => {
                    path.push(p.clone());
                    cur = p;
                }
                None => break,
            },
            None => break,
        }
    }
    let folded: std::collections::BTreeMap<String, bool> =
        folded.iter().map(|(k, v)| ((*k).to_string(), *v)).collect();
    let ctx = Ctx {
        sessions: &feed.sessions,
        here: &feed.here,
        home: &feed.home,
        folded: &folded,
        path,
    };
    rows(&ctx)
}

/// The ids a drawing holds, in order — `[subagents]` for a group's line.
fn ids(rows: &[Out]) -> Vec<String> {
    rows.iter()
        .map(|r| match r.number {
            None => format!("[{}]", r.id),
            Some(_) => r.id.clone(),
        })
        .collect()
}

fn feed(sessions: Vec<Session>, here: &str) -> Feed {
    Feed {
        sessions,
        here: here.to_string(),
        home: "/home/dead".into(),
    }
}

/// **Mine first, then one line per automatic kind** — and mine is OPEN, because its rows are why
/// the list is open at all.
#[test]
fn mine_is_open_and_every_other_kind_is_one_folded_line() {
    let mut child = session("s-c", "read the repo");
    child.parent = Some("s-a".into());
    let mut shot = session("s-shot", "one prompt, one answer");
    shot.origin = "one_shot".into();
    let f = feed(vec![session("s-a", "the habr article"), child, shot], "s-a");
    assert_eq!(
        ids(&draw(&f, &[])),
        vec!["[mine]", "s-a", "[subagents]", "[one-shots]"],
        "mine's row is drawn and the two automatic kinds are folded"
    );
}

/// **One kind of session is a plain list** — no header over the only rows there are.
#[test]
fn a_list_of_one_kind_draws_no_header() {
    let f = feed(vec![session("s-a", "only"), session("s-b", "two")], "s-a");
    assert_eq!(ids(&draw(&f, &[])), vec!["s-a", "s-b"]);
    assert!(draw(&f, &[]).iter().all(|r| r.depth == 0), "and no indent");
}

/// **A group that cannot fold carries no act on Enter**, and one that can says which way it
/// stands — that is what `Fold` is for, and what the head turns into a hint.
#[test]
fn mine_offers_no_key_and_a_folded_group_offers_its_fold() {
    let mut child = session("s-c", "read the repo");
    child.parent = Some("s-a".into());
    let f = feed(vec![session("s-a", "a"), child], "s-a");
    let drawn = draw(&f, &[]);
    let mine = drawn
        .iter()
        .find(|r| r.id == "mine")
        .expect("the mine line");
    assert_eq!(mine.open, Some(Fold::Fixed));
    assert!(mine.enter.is_none(), "no act on a row Enter cannot act on");
    let subagents = drawn
        .iter()
        .find(|r| r.id == "subagents")
        .expect("the line");
    assert_eq!(subagents.open, Some(Fold::Closed));
    assert_eq!(subagents.enter, Some(Act::fold()));
    // Opened by the person, the same row says the other way — and its rows come out.
    let open = draw(&f, &[("subagents", true)]);
    assert_eq!(
        open.iter()
            .find(|r| r.id == "subagents")
            .and_then(|r| r.open),
        Some(Fold::Open)
    );
    assert!(ids(&open).contains(&"s-c".to_string()), "{:?}", ids(&open));
}

/// **A session's row says everything a row can say**: the slug, `WAITING ON YOU`, paused nags, and
/// a fact column.
#[test]
fn a_row_says_what_it_is() {
    let mut s = session("s-a", "the habr article");
    s.slug = "habr".into();
    s.waits_on = 2;
    s.stored = 412;
    s.heads = 2;
    s.running = true;
    let f = feed(vec![s], "s-a");
    let row = &draw(&f, &[])[0];
    assert_eq!(
        row.cells[0].text,
        "habr · the habr article · WAITING ON YOU (2)"
    );
    assert_eq!(
        row.facts[0].text, "generating · 412 rows · 2 heads · qwen",
        "the fact column, in the order a reader scans it"
    );
    assert_eq!(
        row.facts[0].role,
        Some(Role::Pending),
        "a running turn is lit"
    );
    assert_eq!(row.enter, Some(Act::switch("s-a")));
}

/// **A number is the n-th SESSION** — a group's line takes none, because a number is something you
/// type to go somewhere.
#[test]
fn numbers_run_over_the_sessions_and_never_over_a_group() {
    let mut child = session("s-c", "read the repo");
    child.parent = Some("s-a".into());
    let f = feed(vec![session("s-a", "a"), session("s-b", "b"), child], "s-a");
    let numbers: Vec<(String, Option<usize>)> = draw(&f, &[("subagents", true)])
        .into_iter()
        .map(|r| (r.id, r.number))
        .collect();
    assert_eq!(
        numbers,
        vec![
            ("mine".to_string(), None),
            ("s-a".to_string(), Some(1)),
            ("s-b".to_string(), Some(2)),
            ("subagents".to_string(), None),
            ("s-c".to_string(), Some(3)),
        ]
    );
}

/// **A head standing in a sub-session draws its FAMILY** — the parent and its children, and no
/// groups at all.
#[test]
fn the_family_view_has_no_groups() {
    let mut child = session("s-c", "read the repo");
    child.parent = Some("s-a".into());
    let f = feed(vec![session("s-a", "the parent"), child], "s-c");
    assert_eq!(ids(&draw(&f, &[])), vec!["s-a", "s-c"]);
    assert_eq!(draw(&f, &[])[1].depth, 1);
}

/// **A reviewer is the queue's work**, by its title when the head has no seat for it — the rule
/// `letibot_sessionlog::origin` states, repeated here as the copy a project overrides.
#[test]
fn a_reviewer_is_grouped_with_the_queue_by_its_title() {
    let queue = "s-gatekeeper-0123456789ab";
    let mut q = session(queue, "the review queue");
    q.origin = "queue".into();
    let mut reviewer = session(
        "s-gatekeeper-0123456789ab-sub-1",
        "You are the gatekeeper. review it",
    );
    reviewer.parent = Some(queue.into());
    let f = feed(vec![session("s-a", "a"), q, reviewer], "s-a");
    // The group opened AND the queue's own session opened: a reviewer nests under it, which is
    // the tree a group line does not open by itself.
    let drawn = draw(
        &f,
        &[("review queue", true), ("s-gatekeeper-0123456789ab", true)],
    );
    assert!(
        ids(&drawn).contains(&"s-gatekeeper-0123456789ab-sub-1".to_string()),
        "the reviewer is under review queue: {:?}",
        ids(&drawn)
    );
}

/// **A workspace under the head's home is drawn as `~/…`**, and the full id sits under every row.
#[test]
fn a_row_under_is_the_id_and_a_shortened_path() {
    let mut s = session("s-a", "a");
    s.workspace = "/home/dead/Projects/letibot".into();
    let f = feed(vec![s], "s-a");
    assert_eq!(draw(&f, &[])[0].under[0], "s-a  ~/Projects/letibot");
}
