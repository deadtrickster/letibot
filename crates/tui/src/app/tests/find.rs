//! The find bar: `ctrl-f` over the open conversation - a tint on every match as the query is typed,
//! a count, a jump to a hit that is not on the screen, and Esc that keeps the reader's place - and
//! the same question over the project's conversations, whose hits are rows the daemon answered with.

use super::*;
use crate::app::find::Scope;
use letibot_sessionlog::protocol::SearchHit;

/// A conversation of `n` rows in which row `hit` carries `word`: the tail is what a fresh head
/// draws, so a hit near the top is a row off the loaded window.
fn a_conversation(n: usize, hits: &[(usize, &str)]) -> App {
    let mut a = app();
    a.session_id = "s-find".into();
    a.items = (0..n)
        .map(|i| {
            let word = hits
                .iter()
                .find(|(at, _)| *at == i)
                .map(|(_, w)| *w)
                .unwrap_or("nothing here");
            SnapshotItem {
                item_id: format!("i{i}"),
                kind: "assistant".into(),
                ledger_head: String::new(),
                ts: 0,
                item: Some(TranscriptItem::Assistant {
                    text: format!("row {i} says {word}"),
                    tool_calls: Vec::new(),
                    truncated: false,
                }),
                citations: Vec::new(),
            }
        })
        .collect();
    a.invalidate_history();
    a
}

/// Type a query into the bar, one key at a time, the way a person does.
fn type_query(a: &mut App, query: &str) {
    for c in query.chars() {
        a.key(Key::Char(c));
    }
}

/// The frame as one string, escapes and all: the tint is an escape, so it has to be read there.
fn frame(a: &mut App, w: usize, h: usize) -> String {
    a.screen(w, h).join("\n")
}

/// The bar's own row - the hint bar's slot while the bar is open.
fn bar(a: &mut App, w: usize, h: usize) -> String {
    a.screen(w, h).last().cloned().unwrap_or_default()
}

/// A query tints every match on the screen and counts the hits the head holds: the row carries the
/// reverse-video mark and the bar says which hit of how many.
#[test]
fn a_typed_query_tints_its_hits_and_counts_them() {
    let mut a = a_conversation(40, &[(1, "cache key"), (38, "cache key")]);
    // The premise: the first frame draws the tail and not the top row.
    let before = frame(&mut a, 100, 20);
    assert!(!before.contains("row 1 says"), "{before}");

    a.key(Key::CtrlF);
    type_query(&mut a, "cache key");
    let after = frame(&mut a, 100, 20);
    assert!(a.find.is_some(), "ctrl-f opened no bar");
    assert!(after.contains("\u{1b}[7m"), "no match was tinted:\n{after}");
    let bar = bar(&mut a, 100, 20);
    assert!(bar.contains("1/2"), "the counter is wrong: {bar:?}");
    // The reader is on the first hit, and that row was not on the screen a frame ago.
    assert!(
        after.contains("row 1 says"),
        "the first hit was not drawn:\n{after}"
    );
}

/// A jump reaches a hit that is not in the loaded window: Enter takes the next one, the view moves
/// to it, and the tint is on the row under the reader's eye.
#[test]
fn enter_jumps_to_a_hit_past_the_loaded_window() {
    let mut a = a_conversation(60, &[(2, "cache key"), (58, "cache key")]);
    a.key(Key::CtrlF);
    type_query(&mut a, "cache key");
    let first = frame(&mut a, 100, 20);
    assert!(first.contains("row 2 says"), "{first}");

    a.key(Key::Enter);
    let second = frame(&mut a, 100, 20);
    let bar = bar(&mut a, 100, 20);
    assert!(bar.contains("2/2"), "the counter did not move: {bar:?}");
    assert!(
        second.contains("row 58 says"),
        "the second hit was not put on the screen:\n{second}"
    );
    assert!(
        !second.contains("row 2 says"),
        "the view did not move:\n{second}"
    );
}

/// Esc closes the bar and keeps the place: the row the reader jumped to is still the row drawn, and
/// the bar's row is the hint bar again.
#[test]
fn esc_closes_the_bar_and_keeps_the_scroll() {
    let mut a = a_conversation(60, &[(2, "cache key")]);
    a.key(Key::CtrlF);
    type_query(&mut a, "cache key");
    let parked = frame(&mut a, 100, 20);
    assert!(parked.contains("row 2 says"), "{parked}");

    a.key(Key::Esc);
    assert!(a.find.is_none(), "esc left the bar open");
    let after = frame(&mut a, 100, 20);
    assert!(
        after.contains("row 2 says"),
        "the place was not kept:\n{after}"
    );
    assert!(
        !after.contains("enter next · shift-enter back"),
        "the find bar is still drawn: {after}"
    );
    assert!(
        after.contains("ctrl-s sessions"),
        "the hint bar did not come back: {after}"
    );
    assert!(
        !after.contains("\u{1b}[7m"),
        "the tint outlived the bar:\n{after}"
    );
}

/// The role tokens and the cycle are one vocabulary: `me:` narrows to the operator's own rows and
/// Tab steps the query through the cycle, rewriting the tokens it searches.
#[test]
fn a_role_token_and_the_cycle_narrow_the_same_rows() {
    let mut a = a_conversation(6, &[(1, "cache key"), (4, "cache key")]);
    // Row 4 becomes the model's words rather than the operator's.
    a.items[1].item = Some(TranscriptItem::User {
        parts: vec![UserPart::Text {
            text: "row 1 says cache key from the person".into(),
        }],
        speaker: letibot_transcript::Speaker::Operator,
    });
    a.invalidate_history();
    a.key(Key::CtrlF);
    type_query(&mut a, "me: cache key");
    let hits = a.find.as_ref().map(|f| f.hits.len()).unwrap_or(0);
    assert_eq!(hits, 1, "`me:` kept the model's row");
    a.key(Key::Tab);
    let query = a.find.as_ref().map(|f| f.query.clone()).unwrap_or_default();
    assert_eq!(
        query, "agent: cache key",
        "the cycle did not step: {query:?}"
    );
    let hits = a.find.as_ref().map(|f| f.hits.len()).unwrap_or(0);
    assert_eq!(hits, 1, "`agent:` kept the person's row");
}

/// The wider scope asks the daemon, the default one names no session - so the project's main
/// conversations are what it reads - and the answer is drawn as session, folder, time, role and
/// the line around the match. Enter opens the conversation the hit is in.
#[test]
fn a_project_search_draws_the_hits_and_opens_the_one_under_the_cursor() {
    let mut a = a_conversation(4, &[]);
    a.key(Key::CtrlF);
    type_query(&mut a, "cache");
    a.key(Key::CtrlF);
    let asked = a
        .queued
        .iter()
        .rev()
        .find_map(|q| match q {
            Action::Search {
                query,
                session,
                all,
                ..
            } => Some((query.clone(), session.clone(), *all)),
            _ => None,
        })
        .expect("the wider scope asked the daemon");
    assert_eq!(asked.0, "cache");
    assert_eq!(asked.1, None, "a project's search names no session");
    assert!(!asked.2, "the main conversations are the default");
    assert_eq!(a.drawn(), Drawn::Surface(Surface::Search));

    // The daemon's answer, including a hit from an ARCHIVED conversation: the store reads the
    // archive and the pane draws it like any other row.
    a.apply(ServerFrame::Searched {
        query: "cache".into(),
        session: None,
        from: 0,
        hits: vec![
            SearchHit {
                session: "s-main".into(),
                title: "the cache work".into(),
                folder: "/home/dead/Projects/letibot".into(),
                at_ms: 1_700_000_000_000,
                role: "mine".into(),
                tool: String::new(),
                row: 7,
                item_id: "s-main#t0.7".into(),
                excerpt: "…the cache key is warm…".into(),
            },
            SearchHit {
                session: "s-old".into(),
                title: "an archived conversation".into(),
                folder: "/home/dead/Projects/letibot".into(),
                at_ms: 1_690_000_000_000,
                role: "agent".into(),
                tool: String::new(),
                row: 2,
                item_id: "s-old#t0.2".into(),
                excerpt: "…an old cache key…".into(),
            },
        ],
        more: false,
        scanned: 40,
        sessions: 2,
        why: None,
    });
    assert_eq!(a.find.as_ref().map(|f| f.results.len()), Some(2));
    let shown = frame(&mut a, 110, 24);
    for want in [
        "the cache work",
        "an archived conversation",
        "letibot",
        "mine",
        "agent",
        "the cache key is warm",
    ] {
        assert!(shown.contains(want), "{want:?} is not drawn:\n{shown}");
    }
    // Enter opens the conversation the hit is in, at the row that matched.
    assert_eq!(a.key(Key::Down), None, "moving the cursor sends nothing");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Switch("s-old".into())),
        "enter did not open the hit's conversation"
    );
    let goto = a.find.as_ref().and_then(|f| f.goto.clone());
    assert_eq!(goto, Some(("s-old".into(), "s-old#t0.2".into(), 2)));
    // And `alt-o` asks again, this time for the sessions that are not the operator's own.
    a.key(Key::AltO);
    let all = a.queued.iter().rev().find_map(|q| match q {
        Action::Search { all, .. } => Some(*all),
        _ => None,
    });
    assert_eq!(all, Some(true), "alt-o did not widen the scope");
    assert_eq!(a.find.as_ref().and_then(|f| f.scope), Some(Scope::Sessions));
}
