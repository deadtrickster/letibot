//! **The census's numbers, and the one thing they must not become: a second counting rule.**
//!
//! Every test here is about a store, because the numbers are the store's — a fixture that built
//! the counts by hand would assert the arithmetic of the test rather than the reading of the
//! queue. `desk::status` prints the same rows, so the agreement test is a real one: the census
//! counts them one way (a predicate over rows) and `status` prints them another (a line per row),
//! and the two have to say the same thing.

use super::*;
use crate::mergequeue::serve::tests::TempDir;
use letibot_tokencore::store::{
    Approval, MergePriority, MergeState, Move, NewReview, ReviewKind, ReviewState,
};

/// **A landing row in `to`**, entered at `at` — the review it comes from is real, because a merge
/// row is only ever born from an approval (`Store::approve_review`), and a fixture that inserted
/// one directly would be a shape the queue cannot produce.
fn a_landing(store: &Store, repo: &str, id: &str, to: MergeState, at: u64) {
    let review = format!("r-{id}");
    store
        .enqueue_review(
            &NewReview {
                id: review.clone(),
                repository: repo.to_string(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: "s-child".into(),
                branch: format!("agent/{id}"),
                worktree: None,
                base_sha: "base".into(),
                head_sha: "head".into(),
                brief: "a branch".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            at,
        )
        .unwrap();
    store
        .move_review(
            &review,
            ReviewState::Waiting,
            ReviewState::Reviewing,
            &Move::new("test", "a reviewer has it"),
            at,
        )
        .unwrap();
    store
        .approve_review(
            &review,
            &Approval {
                expect: ReviewState::Reviewing,
                expect_owner: None,
                reviewed_sha: "head".into(),
                by: "reviewer:one".into(),
                reasons: vec!["fine".into()],
                files: vec![],
                commands: vec![],
                why: "approved".into(),
                merge_id: id.to_string(),
            },
            at,
        )
        .unwrap();
    store
        .move_merge(
            id,
            MergeState::Queued,
            to,
            &Move::new("test", "the gate"),
            at,
        )
        .unwrap();
}

/// **A review row in `to`** — for the tests that prove what the census does NOT count.
fn a_review(store: &Store, repo: &str, id: &str, to: ReviewState, at: u64) {
    store
        .enqueue_review(
            &NewReview {
                id: id.to_string(),
                repository: repo.to_string(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: "s-child".into(),
                branch: format!("agent/{id}"),
                worktree: None,
                base_sha: "base".into(),
                head_sha: "head".into(),
                brief: "a branch".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            at,
        )
        .unwrap();
    if to != ReviewState::Waiting {
        store
            .move_review(
                id,
                ReviewState::Waiting,
                to,
                &Move::new("test", "the reviewer gave up"),
                at,
            )
            .unwrap();
    }
}

fn store(tag: &str) -> (TempDir, Store) {
    let dir = TempDir::new(tag);
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store opens");
    (dir, store)
}

/// **The eight rows the operator reported, and the ones that are still a pass's own business.**
/// Three `failed` and five `conflict` are stuck; a `queued` landing, a `gating` one, one that
/// landed and one whose branch moved are not.
#[test]
fn the_states_a_row_is_stuck_in_are_the_ones_no_pass_will_move() {
    let (_dir, s) = store("stuck-count");
    let repo = "/tmp/stuck-count";
    for i in 0..3 {
        a_landing(
            &s,
            repo,
            &format!("m-failed-{i}"),
            MergeState::Failed,
            1_000,
        );
    }
    for i in 0..5 {
        a_landing(
            &s,
            repo,
            &format!("m-conflict-{i}"),
            MergeState::Conflict,
            1_000,
        );
    }
    a_landing(&s, repo, "m-queued", MergeState::Queued, 1_000);
    a_landing(&s, repo, "m-gating", MergeState::Gating, 1_000);
    a_landing(&s, repo, "m-landed", MergeState::Landed, 1_000);
    a_landing(&s, repo, "m-moved", MergeState::Moved, 1_000);

    let census = of(&s, &[repo.to_string()]).expect("the census");
    assert_eq!(census.total, 8, "{census:?}");
    // `desk::FLOW`'s order, which is the order `status` prints.
    assert_eq!(
        census.states,
        vec![("conflict", 5), ("failed", 3)],
        "{census:?}"
    );
    assert_eq!(
        census.said(),
        "5 conflict · 3 failed",
        "the counts as one clause"
    );
}

/// **And they are `desk::status`'s rows.** The census reads them by predicate; `status` prints a
/// line each. Counting the printed lines that name a stuck state has to give the census's numbers,
/// or the queue has two counting rules and this test is the only place that would ever say so.
#[test]
fn the_census_counts_the_rows_status_prints() {
    let (_dir, s) = store("stuck-agrees");
    let repo = "/tmp/stuck-agrees";
    for i in 0..3 {
        a_landing(
            &s,
            repo,
            &format!("m-failed-{i}"),
            MergeState::Failed,
            1_000,
        );
    }
    for i in 0..2 {
        a_landing(
            &s,
            repo,
            &format!("m-conflict-{i}"),
            MergeState::Conflict,
            1_000,
        );
    }
    a_landing(&s, repo, "m-timed-out", MergeState::TimedOut, 1_000);
    a_landing(&s, repo, "m-queued", MergeState::Queued, 1_000);

    let census = of(&s, &[repo.to_string()]).expect("the census");
    let printed = desk::status(&s, Some(repo)).expect("status");

    // The printed rows, by state word: `  `id` state `branch` → `target` at sha: …`
    let mut counted: Vec<(&str, usize)> = Vec::new();
    for line in printed.lines() {
        let Some(rest) = line.trim_start().strip_prefix('`') else {
            continue;
        };
        let Some((_, rest)) = rest.split_once('`') else {
            continue;
        };
        let Some(word) = rest.trim_start().split_whitespace().next() else {
            continue;
        };
        if matches!(word, "failed" | "timed_out" | "conflict") {
            match counted.iter_mut().find(|(w, _)| *w == word) {
                Some(slot) => slot.1 += 1,
                None => counted.push((word, 1)),
            }
        }
    }
    let mut census_words = census.states.clone();
    census_words.sort();
    counted.sort();
    assert_eq!(
        census_words, counted,
        "the census and `status` count one set of rows\n{printed}"
    );
}

/// **A review row nobody will move is NOT counted, and that is what keeps the sentence true.**
/// `serve.rs` re-seats a `rejected` or `cannot_decide` review to `waiting` by itself when the
/// branch moved past the verdict it gave — so *no pass will move them* would be false for it — and
/// one lingering rejection would hold the alarm open over an otherwise clean queue.
#[test]
fn a_parked_review_is_not_a_stuck_landing() {
    let (_dir, s) = store("stuck-reviews");
    let repo = "/tmp/stuck-reviews";
    a_review(&s, repo, "r-rejected", ReviewState::Rejected, 1_000);
    a_review(&s, repo, "r-cannot", ReviewState::CannotDecide, 1_000);

    let census = of(&s, &[repo.to_string()]).expect("the census");
    assert!(census.is_empty(), "{census:?}");
    assert!(!census.is_news(u64::MAX / 2));
}

/// **The age is the oldest row's own `state_ms`** — when it entered the state it is stuck in, which
/// is the fact nobody had: not when the queue last ran, and not when the census was composed.
#[test]
fn the_oldest_row_is_the_oldest_state_change() {
    let (_dir, s) = store("stuck-age");
    let repo = "/tmp/stuck-age";
    a_landing(&s, repo, "m-young", MergeState::Failed, 9_000_000);
    a_landing(&s, repo, "m-old", MergeState::Conflict, 1_000_000);

    let census = of(&s, &[repo.to_string()]).expect("the census");
    assert_eq!(census.oldest_ms, 1_000_000, "{census:?}");
    assert_eq!(census.oldest_age_ms(1_600_000), 600_000);
    // A clock behind the row's own stamp is no age at all, not a negative one.
    assert_eq!(census.oldest_age_ms(999_000), 0);
}

/// **A census is news only once the oldest row has STAYED bad** — the transition notice arrived
/// with the row, so a quarter-hour later is the first moment the count and the age are anything
/// the project has not already been told.
#[test]
fn a_young_census_is_not_news_and_says_when_it_will_be() {
    let (_dir, s) = store("stuck-news");
    let repo = "/tmp/stuck-news";
    a_landing(&s, repo, "m-one", MergeState::Failed, 1_000);

    let census = of(&s, &[repo.to_string()]).expect("the census");
    // A minute in: the transition's news, and the census waits out the rest of the quarter-hour.
    assert!(!census.is_news(1_000 + 60_000), "{census:?}");
    assert_eq!(
        census.news_in_ms(1_000 + 60_000),
        Some(STUCK_AFTER_MS - 60_000)
    );
    // Exactly at the line, and past it.
    assert!(census.is_news(1_000 + STUCK_AFTER_MS), "{census:?}");
    assert!(census.is_news(1_000 + 8 * 3_600_000), "{census:?}");
    assert_eq!(census.news_in_ms(1_000 + 8 * 3_600_000), None);

    // And nothing stuck is never news, however long the daemon runs.
    let empty = of(&s, &["/tmp/no-queue-here".to_string()]).expect("the census");
    assert!(empty.is_empty());
    assert!(!empty.is_news(u64::MAX / 2));
    assert_eq!(empty.news_in_ms(0), None);
    assert_eq!(empty.said(), "");
}

/// **A session in no repository is told nothing** — an empty list is not "every repository", or a
/// session seated above a folder of projects would be nagged about all of them.
#[test]
fn a_session_in_no_repository_has_no_census() {
    let (_dir, s) = store("stuck-nowhere");
    let repo = "/tmp/stuck-nowhere";
    a_landing(&s, repo, "m-one", MergeState::Failed, 1_000);
    let census = of(&s, &[]).expect("the census");
    assert!(census.is_empty(), "{census:?}");
    // And another repository's rows are not this project's.
    let other = of(&s, &["/tmp/somebody-else".to_string()]).expect("the census");
    assert!(other.is_empty(), "{other:?}");
}

/// **The age in the words a person reads an alarm in** — hours, because the incident ran for most
/// of a night and `252 min` is a number a reader has to convert.
#[test]
fn an_age_is_read_in_hours_once_it_is_hours() {
    assert_eq!(age_words(0), "0 s");
    assert_eq!(age_words(40_000), "40 s");
    assert_eq!(age_words(59_000), "59 s");
    assert_eq!(age_words(60_000), "1 min");
    assert_eq!(age_words(18 * 60_000), "18 min");
    assert_eq!(age_words(59 * 60_000 + 59_000), "59 min");
    assert_eq!(age_words(3_600_000), "1h 00m");
    assert_eq!(age_words(4 * 3_600_000 + 12 * 60_000), "4h 12m");
}

/// **The sentence the project session reads**, byte for byte — it is the deliverable, and the one
/// thing in this module a person sees.
#[test]
fn the_sentence_says_how_many_and_how_long_and_what_to_do() {
    let (_dir, s) = store("stuck-sentence");
    let repo = "/tmp/stuck-sentence";
    for i in 0..3 {
        a_landing(
            &s,
            repo,
            &format!("m-failed-{i}"),
            MergeState::Failed,
            1_000,
        );
    }
    for i in 0..5 {
        a_landing(
            &s,
            repo,
            &format!("m-conflict-{i}"),
            MergeState::Conflict,
            1_000,
        );
    }
    let census = of(&s, &[repo.to_string()]).expect("the census");
    let said =
        crate::mergequeue::notices::stuck_census(&census, 1_000 + 4 * 3_600_000 + 12 * 60_000);
    assert_eq!(
        said,
        "[queue stuck] 8 row(s) of this project's merge queue are stuck and no pass will move \
         them: 5 conflict · 3 failed. The oldest has been there 4h 12m. `/queue restart <id>` puts \
         one back in line and `/queue rm <id>` withdraws it; `queue` with `action: \"status\"` \
         names them."
    );
}

/// **And the age in that sentence GROWS with the clock** — the property the brief asked for by
/// name, asserted where it can be asserted honestly: the sentence is a pure function of the census
/// and the clock, so two deliveries two hours apart say two different ages about one unchanged
/// queue. (An end-to-end test cannot show this without sleeping for an hour; the two deliveries it
/// *does* drive are asserted in `crates/harnessd/tests/queue_census.rs`.)
#[test]
fn the_same_rows_said_later_name_a_bigger_age() {
    let (_dir, s) = store("stuck-grown");
    let repo = "/tmp/stuck-grown";
    a_landing(&s, repo, "m-one", MergeState::Failed, 1_000);
    let census = of(&s, &[repo.to_string()]).expect("the census");

    let first = crate::mergequeue::notices::stuck_census(&census, 1_000 + STUCK_AFTER_MS);
    let later =
        crate::mergequeue::notices::stuck_census(&census, 1_000 + STUCK_AFTER_MS + 2 * 3_600_000);
    assert!(first.contains("15 min"), "{first}");
    assert!(later.contains("2h 15m"), "{later}");
    assert_ne!(first, later, "the repeat is a new fact, not the same one");
}

/// **What this read costs on the door every turn ends at** — the measurement the review asked for
/// before a new hot-path query lands, kept so the next reader can re-run it rather than argue.
///
/// `Harness::stuck_census` runs from `Sessions::rearm_todo_nag`, which every turn's end calls, so
/// the question is not *is one query cheap* but *is one PROJECT's query cheap when the box holds
/// twenty others*. Twenty other repositories of twenty rows each, and ours: 420 rows in all.
///
/// **The unscoped number is an UNDER-estimate**: these rows carry the short `evidence` the fixture
/// writes, while a real rejected row's is up to 7 828 characters, and `merge_rows(None)`
/// materialises every one of them. The scoped read is proportional to one project's rows and
/// nothing else, which is the whole argument for it.
///
///     cargo test --release --offline -p letibot-harnessd --lib \
///         mergequeue::stuck::tests::what_the_scoped_read_costs -- --ignored --nocapture
#[test]
#[ignore = "a measurement, not an assertion: run it with --ignored --nocapture"]
fn what_the_scoped_read_costs() {
    use std::time::Instant;

    let (_dir, s) = store("stuck-cost");
    let ours = "/tmp/stuck-cost/ours";
    for r in 0..20 {
        let repo = format!("/tmp/stuck-cost/other-{r}");
        for i in 0..20 {
            a_landing(&s, &repo, &format!("m-{r}-{i}"), MergeState::Failed, 1_000);
        }
    }
    for i in 0..20 {
        a_landing(
            &s,
            ours,
            &format!("m-ours-{i}"),
            MergeState::Conflict,
            1_000,
        );
    }

    let scoped = || of(&s, &[ours.to_string()]).expect("the census");
    let unscoped = || {
        let all: Vec<letibot_tokencore::store::MergeRow> = s.merge_rows(None).expect("every row");
        all.iter().filter(|m| stuck_merge(m.state)).count()
    };
    // Warm both paths, so the number is not a first-touch page fault.
    for _ in 0..20 {
        scoped();
        unscoped();
    }
    let n = 200;
    let t = Instant::now();
    for _ in 0..n {
        scoped();
    }
    let each = t.elapsed() / n;
    let t = Instant::now();
    for _ in 0..n {
        unscoped();
    }
    let every = t.elapsed() / n;
    let census = scoped();
    println!(
        "stuck::of — 420 rows over 21 repositories: scoped {each:?} a call, unscoped {every:?} a \
         call; this project is {} of them",
        census.total
    );
    assert_eq!(census.total, 20, "and it counts only this project's rows");
}
