//! The two queues' store, asserted against the defects of the 2026-10-11 review (TODO.md R61):
//! every test here would fail on the v23 table, which had whole-row upserts, no owner, no
//! history, no lease, no binding of a verdict to a commit and a recovery that moved every row.

use super::*;

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "letibot-queues-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn db(&self) -> std::path::PathBuf {
        self.0.join("sessions.db")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn review(id: &str, repo: &str) -> NewReview {
    NewReview {
        id: id.into(),
        repository: repo.into(),
        target: "main".into(),
        kind: ReviewKind::Branch,
        session_id: "s-child".into(),
        branch: format!("agent/{id}"),
        worktree: Some(format!("/wt/{id}")),
        base_sha: "base".into(),
        head_sha: "head1".into(),
        brief: "do the thing".into(),
        priority: MergePriority::Subagent,
        fixes: None,
        evidence: "enqueued".into(),
    }
}

fn claim(owner: &str) -> LeaseClaim {
    LeaseClaim {
        owner: owner.into(),
        pid: 42,
        host: "box".into(),
        build: "test".into(),
    }
}

/// **A move is conditional on the state it expects, and every move that happened is history.**
/// On v23 `put_merge_entry` overwrote the whole row from a snapshot, so a second writer's stale
/// view silently undid the first's move and no record said either had happened.
#[test]
fn a_move_from_a_state_the_row_left_is_refused_and_every_real_move_is_recorded() {
    let d = TempDir::new("cas");
    let s = Store::open(&d.db()).unwrap();
    assert!(
        s.enqueue_review(&review("r1", "/repo"), "test", 10)
            .unwrap()
    );
    assert!(
        !s.enqueue_review(&review("r1", "/repo"), "test", 11)
            .unwrap(),
        "the same id enqueued twice is one row"
    );

    let first = Move::new("daemon-a", "a reviewer is on it")
        .set("owner", "daemon-a")
        .set("deadline_ms", 1_000u64);
    assert!(
        s.move_review(
            "r1",
            ReviewState::Waiting,
            ReviewState::Reviewing,
            &first,
            20
        )
        .unwrap()
    );
    // A second actor that read `waiting` before the first moved it: refused, nothing written.
    let stale = Move::new("daemon-b", "me too").set("owner", "daemon-b");
    assert!(
        !s.move_review(
            "r1",
            ReviewState::Waiting,
            ReviewState::Reviewing,
            &stale,
            21
        )
        .unwrap()
    );
    let row = s.review_row("r1").unwrap().unwrap();
    assert_eq!(row.owner, "daemon-a");
    assert_eq!(row.state, ReviewState::Reviewing);
    assert_eq!(row.state_ms, 20, "time in state is measured from the move");
    assert_eq!(row.created_ms, 10);

    // The owner guard: only the owner moves a row it holds.
    let not_mine = Move::new("daemon-b", "steal").owned_by("daemon-b");
    assert!(
        !s.move_review(
            "r1",
            ReviewState::Reviewing,
            ReviewState::Waiting,
            &not_mine,
            22
        )
        .unwrap()
    );

    let history = s.transitions("review", "r1").unwrap();
    let moves: Vec<_> = history
        .iter()
        .map(|t| (t.from_state.clone(), t.to_state.clone(), t.by.clone()))
        .collect();
    assert_eq!(
        moves,
        vec![
            (None, "waiting".to_string(), "test".to_string()),
            (
                Some("waiting".to_string()),
                "reviewing".to_string(),
                "daemon-a".to_string()
            ),
        ],
        "two moves happened and two are recorded — the refused ones are not"
    );
    assert_eq!(history[1].why, "a reviewer is on it");

    // Append-only, by trigger.
    assert!(
        s.conn
            .execute("UPDATE queue_transition SET why = 'rewritten'", [])
            .is_err()
    );
    assert!(s.conn.execute("DELETE FROM queue_transition", []).is_err());
}

/// **A move may only write the columns the queue names.** A typo is a refusal, not SQL.
#[test]
fn a_move_cannot_write_a_column_outside_its_list() {
    let d = TempDir::new("cols");
    let s = Store::open(&d.db()).unwrap();
    s.enqueue_review(&review("r1", "/repo"), "test", 10)
        .unwrap();
    let bad = Move::new("x", "y").set("state", "approved");
    assert!(matches!(
        s.move_review("r1", ReviewState::Waiting, ReviewState::Blocked, &bad, 11),
        Err(StoreError::Refused(_))
    ));
}

/// **The verdict and the hand-off are one transaction, and the merge row carries the head the
/// verdict judged** (decision 6). On v23 the verdict was a word on a separate row with no sha,
/// and the queue landed whatever the branch was by then.
#[test]
fn an_approval_binds_the_reviewed_head_and_creates_the_merge_row_once() {
    let d = TempDir::new("approve");
    let s = Store::open(&d.db()).unwrap();
    s.enqueue_review(&review("r1", "/repo"), "test", 10)
        .unwrap();
    s.move_review(
        "r1",
        ReviewState::Waiting,
        ReviewState::Reviewing,
        &Move::new("d", "on it").set("owner", "d"),
        11,
    )
    .unwrap();
    let a = Approval {
        expect: ReviewState::Reviewing,
        expect_owner: Some("d".into()),
        reviewed_sha: "head1".into(),
        by: "reviewer:s-gk#t1".into(),
        reasons: vec!["fine".into()],
        files: vec!["a.rs".into()],
        commands: vec!["git diff base...head1".into()],
        why: "accepted".into(),
        merge_id: "m-r1".into(),
    };
    assert!(s.approve_review("r1", &a, 12).unwrap());
    assert!(
        !s.approve_review("r1", &a, 13).unwrap(),
        "a second verdict on an approved row moves nothing"
    );
    let r = s.review_row("r1").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Approved);
    assert_eq!(r.reviewed_sha.as_deref(), Some("head1"));
    assert_eq!(r.owner, "", "an approved row is nobody's");
    let m = s.merge_row("m-r1").unwrap().unwrap();
    assert_eq!(m.approved_sha, "head1");
    assert_eq!(m.state, MergeState::Queued);
    assert_eq!(m.review_id, "r1");
    assert_eq!(s.merge_rows(Some("/repo")).unwrap().len(), 1);

    // The branch moved after approval: the merge row ends `moved`, the review is waiting again,
    // with the new head — in one transaction.
    s.move_merge(
        "m-r1",
        MergeState::Queued,
        MergeState::Gating,
        &Move::new("d", "taking it").set("owner", "d"),
        14,
    )
    .unwrap();
    assert!(
        s.send_back_to_review("m-r1", "d", "head2", "d", "moved after approval", 15)
            .unwrap()
    );
    assert_eq!(
        s.merge_row("m-r1").unwrap().unwrap().state,
        MergeState::Moved
    );
    let r = s.review_row("r1").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Waiting);
    assert_eq!(r.head_sha, "head2");
    assert!(r.reviewed_sha.is_none() && r.verdict.is_none());
}

/// **One owner per repository, by compare-and-swap; a live holder is never displaced; an
/// expired one is, and the claimant is told whom it replaced** (requirement 1). On v23 every
/// daemon started a queue and nothing said which one served a row.
#[test]
fn the_lease_is_held_by_one_daemon_and_taken_over_only_when_its_heartbeat_expires() {
    let d = TempDir::new("lease");
    let a = Store::open(&d.db()).unwrap();
    let b = Store::open(&d.db()).unwrap();
    let ttl = 60_000;
    assert_eq!(
        a.acquire_queue_lease("/repo", &claim("A"), 1_000, ttl)
            .unwrap(),
        LeaseOutcome::Held { previous: None }
    );
    match b
        .acquire_queue_lease("/repo", &claim("B"), 2_000, ttl)
        .unwrap()
    {
        LeaseOutcome::HeldBy(l) => assert_eq!(l.owner, "A"),
        other => panic!("a live lease was displaced: {other:?}"),
    }
    // Another repository is another lease.
    assert_eq!(
        b.acquire_queue_lease("/other", &claim("B"), 2_000, ttl)
            .unwrap(),
        LeaseOutcome::Held { previous: None }
    );
    assert!(a.heartbeat_queue_lease("/repo", "A", 30_000).unwrap());
    // A's heartbeat stops; past the ttl, B takes it, and learns it was A's.
    match b
        .acquire_queue_lease("/repo", &claim("B"), 30_000 + ttl + 1, ttl)
        .unwrap()
    {
        LeaseOutcome::Held {
            previous: Some(prev),
        } => assert_eq!(prev.owner, "A"),
        other => panic!("an expired lease was not taken over: {other:?}"),
    }
    assert!(
        !a.heartbeat_queue_lease("/repo", "A", 100_000).unwrap(),
        "the old owner learns at its next heartbeat that it is not the owner"
    );
    assert!(b.release_queue_lease("/repo", "B", 100_001).unwrap());
    assert!(
        a.queue_leases()
            .unwrap()
            .iter()
            .all(|l| l.repository != "/repo")
    );
}

/// **A daemon whose build does not match the store never takes a lease** — the store's schema
/// is read live, in the claim's own transaction, so a daemon running when a newer build migrated
/// the file finds out at its next claim or heartbeat.
#[test]
fn a_store_of_another_schema_grants_no_lease_and_stops_a_heartbeat() {
    let d = TempDir::new("schema");
    let s = Store::open(&d.db()).unwrap();
    assert!(matches!(
        s.acquire_queue_lease("/repo", &claim("A"), 1, 60_000)
            .unwrap(),
        LeaseOutcome::Held { .. }
    ));
    s.conn
        .execute(
            "UPDATE schema_version SET version = ?1",
            params![SCHEMA_VERSION + 1],
        )
        .unwrap();
    assert_eq!(
        s.acquire_queue_lease("/repo", &claim("A"), 2, 60_000)
            .unwrap(),
        LeaseOutcome::WrongSchema {
            store: SCHEMA_VERSION + 1,
            mine: SCHEMA_VERSION
        }
    );
    assert!(!s.heartbeat_queue_lease("/repo", "A", 3).unwrap());
}

/// **Recovery touches only this repository's rows, and only those another owner held** — on v23
/// `recover` turned EVERY `taken` row in the store `stale`, including another daemon's live one.
#[test]
fn recovery_moves_only_the_rows_a_gone_owner_held_in_this_repository() {
    let d = TempDir::new("recover");
    let s = Store::open(&d.db()).unwrap();
    for (id, repo, owner) in [
        ("r-gone", "/repo", "A"),
        ("r-mine", "/repo", "B"),
        ("r-other-repo", "/other", "A"),
    ] {
        s.enqueue_review(&review(id, repo), "t", 1).unwrap();
        s.move_review(
            id,
            ReviewState::Waiting,
            ReviewState::Reviewing,
            &Move::new(owner, "on it").set("owner", owner),
            2,
        )
        .unwrap();
    }
    assert_eq!(s.recover_queue("/repo", "B", 3).unwrap(), 1);
    assert_eq!(
        s.review_row("r-gone").unwrap().unwrap().state,
        ReviewState::Waiting
    );
    assert_eq!(
        s.review_row("r-mine").unwrap().unwrap().state,
        ReviewState::Reviewing
    );
    assert_eq!(
        s.review_row("r-other-repo").unwrap().unwrap().state,
        ReviewState::Reviewing,
        "another repository's row is another lease's business"
    );
}

/// **The target's health records a move only when the state changes.**
#[test]
fn the_targets_health_is_one_row_and_its_changes_are_history() {
    let d = TempDir::new("health");
    let s = Store::open(&d.db()).unwrap();
    let h = |state, sha: &str, at| TargetHealth {
        repository: "/repo".into(),
        target: "main".into(),
        sha: sha.into(),
        state,
        checked_ms: at,
        evidence: "gate".into(),
        gate_steps: vec![],
    };
    s.set_target_health(&h(HealthState::Green, "a", 1), "d")
        .unwrap();
    s.set_target_health(&h(HealthState::Green, "b", 2), "d")
        .unwrap();
    s.set_target_health(&h(HealthState::Red, "c", 3), "d")
        .unwrap();
    assert_eq!(s.target_health("/repo", "main").unwrap().unwrap().sha, "c");
    let t = s.transitions("target", "main").unwrap();
    assert_eq!(t.len(), 2, "green, then red: two changes, three writes");
    assert_eq!(t[1].to_state, "red");
}

/// The v23 queue's DDL, as the build before this one wrote it.
const V23_QUEUE: &str = "
CREATE TABLE merge_queue (
    id TEXT PRIMARY KEY, session_id TEXT NOT NULL, branch TEXT NOT NULL, base_sha TEXT NOT NULL,
    priority TEXT NOT NULL, needs_json TEXT NOT NULL, state TEXT NOT NULL,
    brief TEXT NOT NULL DEFAULT '', evidence TEXT NOT NULL, created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL, worktree TEXT, landed_sha TEXT,
    gate_steps_json TEXT NOT NULL DEFAULT '[]');
CREATE INDEX merge_queue_state_idx ON merge_queue (state, priority, created_ms);
CREATE TABLE merge_review (
    entry_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, branch TEXT NOT NULL,
    base_sha TEXT NOT NULL, asked_ms INTEGER NOT NULL, answered_ms INTEGER, decision TEXT,
    attempts INTEGER NOT NULL DEFAULT 0, failed_ms INTEGER, failure TEXT NOT NULL DEFAULT '',
    reasons_json TEXT NOT NULL DEFAULT '[]', files_json TEXT NOT NULL DEFAULT '[]',
    commands_json TEXT NOT NULL DEFAULT '[]');
";

/// **Decision 13, as a migration** — every v23 row archived, nothing deleted; the waiting,
/// taken and stale rows carried forward as reviews; the waiting row whose review said `reject`
/// (`queue-spin`) not carried; and an OLD daemon's own write to `merge_queue` refused by the new
/// table's shape.
#[test]
fn a_v23_store_is_archived_whole_and_its_waiting_rows_go_to_review() {
    let d = TempDir::new("v23");
    {
        let s = Store::open(&d.db()).unwrap();
        // Make the store look like v23: the new tables out, the old ones in.
        s.conn
            .execute_batch(
                "DROP TABLE merge_queue; DROP TABLE review_queue; DROP TABLE queue_transition;
                 DROP TABLE queue_lease; DROP TABLE target_health;",
            )
            .unwrap();
        s.conn.execute_batch(V23_QUEUE).unwrap();
        for (id, state) in [
            ("a-pass-reads-one-row", "waiting"),
            ("queue-spin", "waiting"),
            ("group-the-queue-messages", "stale"),
            ("taken-one", "taken"),
            ("thinking-merge", "landed"),
            ("pg-noop-x", "failed"),
            ("conflicted", "conflict"),
            ("vetoed-one", "vetoed"),
        ] {
            s.conn
                .execute(
                    "INSERT INTO merge_queue (id, session_id, branch, base_sha, priority, needs_json,
                        state, brief, evidence, created_ms, updated_ms, worktree)
                     VALUES (?1, 's-child', ?2, 'b', 'subagent', '[]', ?3, 'ask', 'why', 1, 1, '/wt')",
                    params![id, format!("agent/{id}"), state],
                )
                .unwrap();
        }
        s.conn
            .execute(
                "INSERT INTO merge_review (entry_id, session_id, branch, base_sha, asked_ms,
                    answered_ms, decision) VALUES ('queue-spin', 's-gk', 'agent/queue-spin', 'b', 1, 2, 'reject')",
                [],
            )
            .unwrap();
        s.conn
            .execute("UPDATE schema_version SET version = 23", [])
            .unwrap();
    }
    let s = Store::open(&d.db()).unwrap();
    assert_eq!(s.stored_schema_version().unwrap(), SCHEMA_VERSION);
    let archived = s.archived_v23_queue().unwrap();
    assert_eq!(archived.len(), 8, "every v23 row is kept");
    let carried: Vec<String> = s
        .review_rows(None)
        .unwrap()
        .into_iter()
        .map(|r| {
            assert_eq!(r.state, ReviewState::Waiting);
            assert_eq!(r.repository, "", "a git fact the daemon resolves");
            r.id
        })
        .collect();
    assert_eq!(
        carried,
        vec![
            "a-pass-reads-one-row".to_string(),
            "group-the-queue-messages".to_string(),
            "taken-one".to_string()
        ]
    );
    assert!(s.merge_rows(None).unwrap().is_empty());
    // An old build's own write: the column list of v23 against the table of v24.
    assert!(
        s.conn
            .execute(
                "INSERT INTO merge_queue (id, session_id, branch, base_sha, priority, needs_json,
                    state, brief, evidence, created_ms, updated_ms)
                 VALUES ('x', 's', 'b', 'b', 'subagent', '[]', 'waiting', '', '', 1, 1)",
                [],
            )
            .is_err(),
        "a daemon of the old build cannot write the new merge queue"
    );
    assert!(
        s.conn
            .prepare("SELECT needs_json FROM merge_queue")
            .is_err(),
        "nor read it"
    );
    // Opening again carries nothing twice.
    drop(s);
    let s = Store::open(&d.db()).unwrap();
    assert_eq!(s.review_rows(None).unwrap().len(), 3);
}
