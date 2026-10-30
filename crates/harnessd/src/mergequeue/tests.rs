//! **The snapshot's half of *is it stuck?*** — a `gating` row's row carries the step its gate is
//! running and the end of that step's output, so a person reading the pane does not have to open a
//! file on the daemon's box to find out whether anything is happening.
//!
//! The gate's own half (the log's path, and the tail growing as the step writes) is measured in
//! `gate::tests::a_running_steps_tail_grows_between_two_reads`; this file is what a head is
//! SENT.

use letibot_tokencore::store::{
    Approval, MergePriority, MergeState, Move, NewReview, ReviewKind, ReviewState, Store,
};

use super::gate;

/// A store with one approved branch whose landing row is `gating`, and a gate that is running a
/// step in `wt` — the state a person asks about.
fn gating_row(tag: &str) -> (Store, std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!(
        "letibot-snapshot-{tag}-{}-{}",
        std::process::id(),
        gate::now_ms()
    ));
    std::fs::create_dir_all(&dir).expect("the fixture's directory");
    let store = Store::open(&dir.join("sessions.db")).expect("the store");
    let repo = dir.display().to_string();
    store
        .enqueue_review(
            &NewReview {
                id: "r-one".into(),
                repository: repo.clone(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: "s-one".into(),
                branch: "agent/one".into(),
                worktree: None,
                base_sha: "a".repeat(40),
                head_sha: "b".repeat(40),
                brief: "make the widget blue".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            gate::now_ms(),
        )
        .expect("the review row");
    assert!(
        store
            .move_review(
                "r-one",
                ReviewState::Waiting,
                ReviewState::Reviewing,
                &Move::new("test", "claimed"),
                gate::now_ms(),
            )
            .expect("the claim"),
        "the review is claimed"
    );
    assert!(
        store
            .approve_review(
                "r-one",
                &Approval {
                    expect: ReviewState::Reviewing,
                    expect_owner: None,
                    reviewed_sha: "b".repeat(40),
                    by: "reviewer:sub-1".into(),
                    reasons: vec!["fine".into()],
                    files: vec![],
                    commands: vec![],
                    why: "accepted".into(),
                    merge_id: "m-one".into(),
                },
                gate::now_ms(),
            )
            .expect("the approval"),
        "the approval makes the merge row"
    );
    assert!(
        store
            .move_merge(
                "m-one",
                MergeState::Queued,
                MergeState::Gating,
                &Move::new("test", "gating it"),
                gate::now_ms(),
            )
            .expect("the move"),
        "the landing is gating"
    );
    (store, dir, repo)
}

/// **A `gating` landing's row carries the step its gate is on, and the end of that step's
/// output** — the snapshot is what the pane reads, so this is the field that makes *is it stuck?*
/// a question the pane can answer.
#[test]
fn a_gating_rows_snapshot_carries_the_step_its_gate_is_running() {
    let (store, dir, repo) = gating_row("gating");
    let wt = dir.join("landing");
    std::fs::create_dir_all(&wt).expect("the landing tree");
    let log = dir.join("step.log");
    std::fs::write(&log, "building\nrunning 3 tests\ntest two ... ok\n").expect("the log");
    gate::watch(&wt, &repo);
    gate::watch_step(&wt, "cargo test --offline --workspace", 2, 4, &log);

    let snap = super::snapshot(&store);
    let row = snap
        .merges
        .iter()
        .find(|m| m.id == "m-one")
        .expect("the landing is in the snapshot");
    let g = row
        .running_gate
        .as_ref()
        .expect("a gating row carries its running step");
    assert_eq!((g.step, g.of), (2, 4), "which step of how many");
    assert_eq!(g.command, "cargo test --offline --workspace");
    assert!(
        g.tail.contains("test two ... ok"),
        "the end of what it has written: {:?}",
        g.tail
    );
    assert_eq!(
        g.log_bytes,
        std::fs::metadata(&log).expect("the log").len(),
        "and how much it has written in all"
    );

    // **The step ends, and the row says nothing it cannot prove** — the entry is cleared however
    // the step returned (a `Drop` guard), so a finished gate is not drawn as *running for ever*.
    drop(gate::step_guard(&wt));
    let after = super::snapshot(&store);
    assert!(
        after
            .merges
            .iter()
            .find(|m| m.id == "m-one")
            .expect("the row")
            .running_gate
            .is_none(),
        "no step is running, so there is no tail to draw"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A landing that is not `gating` carries no running step** — the field is about the step the gate
/// is on now, and a `queued` row is not running one. A `queued` row waiting on the TARGET's gate
/// reads the target's, which is the same read on the same function (`snapshot`'s targets).
#[test]
fn a_row_that_is_not_gating_carries_no_running_step() {
    let (store, dir, repo) = gating_row("queued");
    let wt = dir.join("landing");
    std::fs::create_dir_all(&wt).expect("the landing tree");
    let log = dir.join("step.log");
    std::fs::write(&log, "still going\n").expect("the log");
    gate::watch(&wt, &repo);
    gate::watch_step(&wt, "cargo build", 1, 2, &log);
    store
        .move_merge(
            "m-one",
            MergeState::Gating,
            MergeState::Queued,
            &Move::new("test", "back in line"),
            gate::now_ms(),
        )
        .expect("the move");

    let snap = super::snapshot(&store);
    let row = snap
        .merges
        .iter()
        .find(|m| m.id == "m-one")
        .expect("the row");
    assert_eq!(row.state, "queued");
    assert!(
        row.running_gate.is_none(),
        "a queued landing is running no step: {:?}",
        row.running_gate
    );
    drop(gate::step_guard(&wt));
    let _ = std::fs::remove_dir_all(&dir);
}
