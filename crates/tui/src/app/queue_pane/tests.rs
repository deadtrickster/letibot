//! **The queue feed, measured** — what a plugin is handed, field by field.
//!
//! The feed is a contract a reviewer reads (`docs/plugins-plan.md` §4.9's feed table), so these
//! assert the JSON rather than the intent: a state in the daemon's own word, an age in
//! milliseconds against the host's clock, `live` decided by the host's rule and not inferred from a
//! state string, and `verbs` carrying the daemon's answer about what would actually move the row.
//!
//! The fixture is built by hand rather than from a store: a store would make this test about the
//! store, and the thing under test is the translation.

use super::*;
use letibot_sessionlog::event::{
    MergeGateOutcome, MergeGateStep, QueueHere, QueueLeaseView, QueueMerge, QueueMove, QueueReview,
    QueueSnapshot, TargetHealthView,
};

/// One review, in the shape the wire carries it.
fn a_review(id: &str, state: &str, kind: &str) -> QueueReview {
    QueueReview {
        id: id.into(),
        repository: "/home/dead/Projects/letibot".into(),
        target: "main".into(),
        kind: kind.into(),
        session_id: format!("s-{id}"),
        branch: format!("agent/{id}"),
        state: state.into(),
        owner: "mac/4242/1729000000000000000".into(),
        reviewer: Some(format!("s-reviewer-{id}")),
        head_sha: "1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c".into(),
        reviewed_sha: None,
        verdict: None,
        verdict_by: None,
        attempts: 0,
        failure: String::new(),
        reasons: vec![],
        evidence: format!("the reason {id} is where it is"),
        deadline_ms: None,
        fixes: None,
        created_ms: 1_000_000,
        state_ms: 1_600_000,
        brief: format!("the ask behind {id}"),
    }
}

/// One landing.
fn a_merge(id: &str, state: &str) -> QueueMerge {
    QueueMerge {
        id: id.into(),
        review_id: format!("r-{id}"),
        repository: "/home/dead/Projects/letibot".into(),
        target: "main".into(),
        branch: format!("agent/{id}"),
        state: state.into(),
        owner: String::new(),
        approved_sha: "aaaa1111bbbb2222cccc3333dddd4444eeee5555".into(),
        approved_by: "the reviewer".into(),
        rebased_sha: None,
        landed_sha: None,
        pushed: String::new(),
        evidence: format!("why {id} is {state}"),
        gate_steps: vec![MergeGateStep {
            command: "cargo test --offline".into(),
            outcome: MergeGateOutcome::TimedOut,
            output: "the gate's own words".into(),
            started_ms: 1_500_000,
            elapsed_ms: 600_000,
        }],
        fixes: None,
        created_ms: 1_000_000,
        state_ms: 1_500_000,
    }
}

/// **A snapshot with everything a reviewer will want to see in the feed**: a live review, a
/// finished one, a landing with a gate that timed out, a red target, a stale lease, a repository
/// that is no queue's, and a move.
fn a_snapshot() -> QueueSnapshot {
    QueueSnapshot {
        at_ms: 1_700_000,
        reviews: vec![
            a_review("r-live", "reviewing", "branch"),
            a_review("r-done", "landed", "branch"),
        ],
        merges: vec![a_merge("m-gating", "gating")],
        leases: vec![QueueLeaseView {
            repository: "/home/dead/Projects/letibot".into(),
            owner: "mac/4242/1729000000000000000".into(),
            pid: 4242,
            host: "mac".into(),
            build: "0.5.1".into(),
            // **Older than the lease's lifetime**, so the feed must say STALE.
            heartbeat_ms: 1_600_000,
            acquired_ms: 1_000_000,
        }],
        targets: vec![TargetHealthView {
            repository: "/home/dead/Projects/letibot".into(),
            target: "main".into(),
            sha: "9999888877776666555544443333222211110000".into(),
            state: "red".into(),
            checked_ms: 1_650_000,
            evidence: "the gate's step 2 failed".into(),
            gate_steps: vec![],
        }],
        // **One project, two repositories, one of which never asked for a queue** — the shape the
        // `not_queued` list exists for, and the only shape in which the scope filter keeps it: a
        // repository outside this session's scope is not this pane's to name.
        not_queued: vec!["/home/dead/Projects/letim".into()],
        recent: vec![QueueMove {
            queue: "review".into(),
            entry_id: "r-live".into(),
            repository: "/home/dead/Projects/letibot".into(),
            from: Some("waiting".into()),
            to: "reviewing".into(),
            at_ms: 1_600_000,
            by: "mac/4242/1729000000000000000".into(),
            why: "the reviewer picked it up".into(),
        }],
        archived: 3,
        unreadable: None,
        here: Some(QueueHere {
            workspace: "/home/dead/Projects/letibot".into(),
            // `None` with several is *one project of several* — the head shows each of them.
            repository: None,
            repositories: vec![
                "/home/dead/Projects/letibot".into(),
                "/home/dead/Projects/letim".into(),
            ],
        }),
    }
}

/// A head holding that snapshot, received 100 ms ago by its own clock.
fn a_head() -> App {
    let mut a = crate::app::tests::app();
    a.queues_all = a_snapshot();
    a.rescope_queues();
    a.now_ms = 1_700_100;
    // The snapshot arrived at `at_ms`; the head has held it 100 ms, which is what makes `now_ms`
    // 1_700_100 and every age below the difference between two numbers the fixture names.
    a.queues_got_ms = 1_700_000;
    a
}

/// **The feed's own clock**: the snapshot's `at_ms` plus however long the head has held it, so an
/// age the plugin draws is measured from the same instant the host pane measures from — rounded down
/// to the second, which is the finest thing a row can say (see `CLOCK_QUANTUM_MS`).
#[test]
fn the_feed_carries_the_hosts_clock_and_its_scope() {
    let a = a_head();
    let feed = a.queue_feed();
    assert_eq!(
        feed["now_ms"], 1_700_000,
        "1_700_100 rounded down to the second: the input is the host's cache key"
    );
    assert_eq!(feed["at_ms"], 1_700_000);
    assert_eq!(feed["scope"]["kind"], "here");
    assert_eq!(
        feed["scope"]["words"], "this session's repositories, one project — letibot, letim",
        "the words the pane's own header uses"
    );
    assert_eq!(
        feed["scope"]["several"], true,
        "one project of two repositories: a row has to say which"
    );
    assert_eq!(feed["view"], "review");
    assert_eq!(feed["archived"], 3);
    assert_eq!(feed["not_queued"][0], "letim");
}

/// **The clock is quantised to the second, so a frame that changes nothing a row can draw costs no
/// wasm call** — the host answers a pane once per distinct `Input`, and a millisecond clock in the
/// feed is a cache that misses on every tick.
#[test]
fn the_feeds_clock_moves_once_a_second_and_not_once_a_frame() {
    let mut a = a_head();
    let first = a.queue_feed();
    // **Within the second, the input is the SAME input** — 899 ms of frames, one answer.
    a.now_ms += 899;
    assert_eq!(
        a.queue_feed(),
        first,
        "a frame inside the second is not a new question"
    );
    // **And at the next second it is a new one** — the ages have moved, and a cache hit would draw
    // the previous second's answer.
    a.now_ms += 101;
    let second = a.queue_feed();
    assert_ne!(second, first, "a second later is a new answer");
    assert_eq!(second["now_ms"], 1_701_000);
    assert_eq!(
        second["reviews"][0]["in_state_ms"], 101_000,
        "an age in the feed is measured from the quantised clock too"
    );
}

/// **A row names its repository only when the scope keeps more than one** — the host's answer, from
/// the scope it RESOLVED, so a path scope over several repositories names them and a one-repository
/// project does not.
#[test]
fn the_scope_says_whether_a_row_must_name_its_repository() {
    // This session's own repository: the title names it, so no row repeats it.
    let mut one = a_head();
    one.queues_all.here = Some(QueueHere {
        workspace: "/home/dead/Projects/letibot".into(),
        repository: Some("/home/dead/Projects/letibot".into()),
        repositories: vec!["/home/dead/Projects/letibot".into()],
    });
    one.rescope_queues();
    assert_eq!(one.queue_feed()["scope"]["several"], false);

    // One project of several repositories: the rows have to say which.
    let many = a_head();
    assert_eq!(many.queue_feed()["scope"]["several"], true);

    // A named path: several, or not known to be one — so a row says which, and the words do not
    // enter into it (this is the case the prose-reading cut got wrong in both directions).
    let mut path = a_head();
    path.queue_scope = QueueScope::Path("/home/dead/Projects".to_string());
    assert_eq!(path.queue_feed()["scope"]["several"], true);

    // Every repository: likewise.
    let mut all = a_head();
    all.queue_scope = QueueScope::All;
    assert_eq!(all.queue_feed()["scope"]["several"], true);

    // No repository at all: there is nothing to name.
    let mut none = a_head();
    none.queues_all.here = Some(QueueHere {
        workspace: "/tmp".into(),
        repository: None,
        repositories: vec![],
    });
    none.rescope_queues();
    assert_eq!(none.queue_feed()["scope"]["several"], false);
}

/// **A review, in the words a person reads it in** — the state as the daemon spells it, the age in
/// milliseconds, `live` from the host's own rule, and the verbs the daemon would actually take.
#[test]
fn a_review_row_is_the_states_own_words_and_the_verbs_that_would_move_it() {
    let a = a_head();
    let feed = a.queue_feed();
    let live = &feed["reviews"][0];
    assert_eq!(live["id"], "r-live");
    assert_eq!(live["queue"], "review");
    assert_eq!(live["state"], "reviewing");
    assert_eq!(live["state_words"], "reviewing");
    assert_eq!(live["live"], true, "`reviewing` is the queue working on it");
    assert_eq!(
        live["in_state_ms"], 100_000,
        "now 1_700_100 − state 1_600_000, on the quantised clock"
    );
    assert_eq!(live["repository"], "letibot");
    assert_eq!(live["branch"], "agent/r-live");
    assert_eq!(
        live["owner"], "mac/4242",
        "the claim's start stamp is dropped"
    );
    assert_eq!(
        live["owner_is_serving"], true,
        "the lease on that repository is this row's owner"
    );
    assert_eq!(live["short_sha"], "1f2a3b4c5d");
    assert_eq!(live["evidence"], "the reason r-live is where it is");
    assert_eq!(live["brief"], "the ask behind r-live");
    // **The daemon's own answer about the verbs**: `approve` and `veto` would move a `reviewing`
    // row, `restart` would not, and the feed says WHY rather than leaving the plugin to guess.
    let verbs = live["verbs"].as_array().expect("a verb list");
    let why = |word: &str| {
        verbs
            .iter()
            .find(|v| v["verb"] == word)
            .unwrap_or_else(|| panic!("{word} is in the list: {verbs:?}"))["why"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(why("approve"), "", "approve would move it");
    assert_eq!(why("veto"), "", "veto would move it");
    assert!(
        why("restart").contains("reviewing"),
        "restart would not, and the reason names the state: {:?}",
        why("restart")
    );

    // **A finished row says so**, and a `landed` review is not the queue's work any more.
    let done = &feed["reviews"][1];
    assert_eq!(done["live"], false);
    assert_eq!(done["state"], "landed");
}

/// **A landing's gate, in the words a person reads** — `"timed out"`, not the Rust enum's serde
/// spelling — and what a queued row is waiting for.
#[test]
fn a_gate_step_is_the_persons_words_and_not_the_enums_spelling() {
    let a = a_head();
    let feed = a.queue_feed();
    let m = &feed["merges"][0];
    assert_eq!(m["queue"], "merge");
    assert_eq!(m["state"], "gating");
    assert_eq!(m["live"], true);
    assert_eq!(m["short_sha"], "aaaa1111bb");
    let step = &m["gate_steps"][0];
    assert_eq!(step["command"], "cargo test --offline");
    assert_eq!(step["outcome"], "timed out");
    assert_eq!(step["failed"], true);
    assert_eq!(step["elapsed_ms"], 600_000);
    assert_eq!(step["output"], "the gate's own words");
}

/// **The lease's heartbeat and the STALE warning** — the two facts the pane's chrome carries, so a
/// plugin can draw them without inventing a lifetime.
#[test]
fn a_stale_lease_is_flagged_with_the_daemons_own_lifetime() {
    let a = a_head();
    let feed = a.queue_feed();
    let lease = &feed["leases"][0];
    assert_eq!(lease["repository"], "letibot");
    assert_eq!(lease["owner"], "mac/4242");
    assert_eq!(lease["pid"], 4242);
    assert_eq!(
        lease["heartbeat_ms"], 100_000,
        "100 s on the quantised clock — still past the 90 s lease lifetime"
    );
    assert_eq!(
        lease["stale"], true,
        "100 s is past the 90 s lease lifetime"
    );
    assert_eq!(feed["counts"]["stale_leases"], 1);
}

/// **A red target is the emergency, and the counts say what the pane's strip says.**
#[test]
fn a_red_target_and_the_counters_the_strip_carries() {
    let a = a_head();
    let feed = a.queue_feed();
    let target = &feed["targets"][0];
    assert_eq!(target["target"], "main");
    assert_eq!(target["state"], "red");
    assert_eq!(target["since_ms"], 50_000);
    assert_eq!(target["evidence"], "the gate's step 2 failed");
    let counts = &feed["counts"];
    assert_eq!(counts["review"], 1, "one live review");
    assert_eq!(counts["merge"], 1, "one live landing");
    assert_eq!(counts["finished"], 1, "the landed review");
    assert_eq!(counts["red_targets"], 1);
}

/// **A move, and the repository that is no queue's** — the last two things the pane says.
#[test]
fn a_move_and_a_repository_that_never_asked_for_a_queue() {
    let a = a_head();
    let feed = a.queue_feed();
    let mv = &feed["recent"][0];
    assert_eq!(mv["entry_id"], "r-live");
    assert_eq!(mv["from"], "waiting");
    assert_eq!(mv["to"], "reviewing");
    assert_eq!(mv["ago_ms"], 100_000);
    assert_eq!(mv["by"], "mac/4242");
    assert_eq!(mv["why"], "the reviewer picked it up");
    assert_eq!(feed["not_queued"][0], "letim");
}

/// **A row the daemon would refuse says why, in the feed** — so a plugin's row can name the verb
/// that would do something instead of printing four and letting the person find out.
#[test]
fn a_row_nothing_would_move_carries_every_reason() {
    let mut a = a_head();
    a.queues_all.reviews[1].state = "landed".into();
    a.queues_all.merges.clear();
    a.rescope_queues();
    let feed = a.queue_feed();
    let done = &feed["reviews"][1];
    assert_eq!(done["live"], false);
    let verbs = done["verbs"].as_array().expect("a verb list");
    assert_eq!(verbs.len(), 4, "all four are reported: {verbs:?}");
    assert!(
        verbs.iter().all(|v| v["why"] != ""),
        "a `landed` row takes no verb, and every one says why: {verbs:?}"
    );
}

/// **`live` is the HOST's rule, not a word the plugin greps for.** The two sets are the ones
/// `ui/panes/queue.rs` opens and closes with, and this is the test that keeps the feed's copy of
/// them the same copy.
#[test]
fn live_is_the_hosts_own_rule() {
    for open in ["waiting", "reviewing", "blocked"] {
        assert!(review_open(open), "{open} is the review queue's work");
    }
    for done in [
        "approved",
        "rejected",
        "cannot_decide",
        "withdrawn",
        "archived",
        "landed",
    ] {
        assert!(!review_open(done), "{done} is history");
    }
    for open in ["queued", "gating", "blocked"] {
        assert!(merge_open(open), "{open} is the merge queue's work");
    }
    for done in [
        "landed",
        "failed",
        "timed_out",
        "conflict",
        "moved",
        "withdrawn",
    ] {
        assert!(!merge_open(done), "{done} is history");
    }
}
