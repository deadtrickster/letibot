//! **A reviewer's refusal reaches a LIVE session — the child that wrote the branch, or the session
//! that enqueued it.**
//!
//! The defect this file exists against, measured on the night of 2026-10-11: the queue put a
//! refusal where the work is — the row is `rejected` with the verdict on it, and the pane draws
//! that — while the child that did the work may be dead by then, and the session that ran
//! `task_start` was told by a `!` line on the head's screen. A `!` line is not something a model
//! can act on, so six of that night's entries sat refused while the sessions that enqueued them
//! carried on as if they were fine.
//!
//! R61 rebuilt the queues and deleted `tests/refusal_notice.rs`, which pinned the old door
//! (`report_verdict`). The property is the same and the door is [`deliver_refusal`], reached the
//! one way a refusal is reached now: a reviewer hands `reject` in at the desk and [`serve`] moves
//! the row. So every test here goes through [`serve`] with a real store, a real desk and a
//! `deliver` seam that answers BY NAME — the sessions in `live` take the message, the rest refuse
//! with the sentence the real door gives (`HarnessTaskRunner::send`'s own words). A stub that
//! answered by a flag would let a door that asked the wrong session pass. That the seam IS the
//! session's queue — a message a model reads — is `harness::queue_e2e`'s half.

use std::path::Path;
use std::sync::{Arc, Mutex};

use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::{
    MergePriority, Move, NewReview, ReviewKind, ReviewState, SessionRecord, Store,
};
use letibot_tools::builtins::task::{TaskRunner, TaskSpec, TaskStatus};
use letibot_tools::builtins::verdict::ReviewDesk;
use letibot_tools::gatekeeper::{Decision, LookedAt, ReviewRequest, Submission};

use super::*;
use crate::mergequeue::serve::tests::TempDir;
use crate::mergequeue::settings::QueueSettings;
use crate::sessions::gatekeeper::ReviewerPlacement;

// ===== Fixtures =====

/// **The two parties a refusal is between, and a third that has nothing to do with it.** The
/// stranger is not decoration: a notice that fires for everybody is a notice nobody reads.
const ROOT: &str = "s-root";
const CHILD: &str = "s-root-sub-1";
const STRANGER: &str = "s-stranger";
/// A branch review's id is the child's handle (`enqueue_finished`), and so is its `session_id`.
const ID: &str = CHILD;
const BRANCH: &str = "agent/the-work";
const WORKTREE: &str = "/tmp/the-work";
const HEAD: &str = "head-1";
/// The review queue's session, and this daemon's lease identity.
const HOST: &str = "s-gatekeeper";
const ME: &str = "test/review";

/// The reviewer's complaints, in its own words — one long enough that a summary could not be
/// mistaken for it.
fn reasons() -> Vec<String> {
    vec![
        "the retry is not bounded: `loop` can run for ever".to_string(),
        "the test it added asserts nothing".to_string(),
    ]
}

fn session(store: &Store, id: &str, parent: Option<&str>) {
    store
        .put_session(&SessionRecord {
            id: id.to_string(),
            title: None,
            model_id: String::new(),
            dialect_sha: String::new(),
            workspace_root: String::new(),
            owner: String::new(),
            approvers: Vec::new(),
            role: None,
            parent_session_id: parent.map(str::to_string),
        })
        .expect("the session row");
}

/// **The queue as the daemon leaves it when a branch is put up for review**: a child under the
/// root that ran the `task_start`, a review row whose `session_id` is the CHILD, and a stranger
/// that shares the store. The child's row names its parent on purpose: the root is only reachable
/// by walking, so a fixture written as its own root would pass whether the walk worked or not.
fn the_queue(tag: &str) -> (TempDir, Store) {
    the_queue_with(tag, true)
}

/// [`the_queue`], with or without the child's own session row.
fn the_queue_with(tag: &str, child_row: bool) -> (TempDir, Store) {
    let dir = TempDir::new(&format!("refusal-{tag}"));
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    session(&store, ROOT, None);
    if child_row {
        session(&store, CHILD, Some(ROOT));
    }
    session(&store, STRANGER, None);
    store
        .enqueue_review(
            &NewReview {
                id: ID.into(),
                repository: "/the/repository".into(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: CHILD.into(),
                branch: BRANCH.into(),
                worktree: Some(WORKTREE.into()),
                base_sha: "base".into(),
                head_sha: HEAD.into(),
                brief: "do the work".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            1,
        )
        .expect("the review row");
    (dir, store)
}

/// A runner whose reviewers are always still running — what is under test is the verdict they
/// hand in, not their lives.
struct Reviewers;

impl TaskRunner for Reviewers {
    fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
        Err("no reviewer is started in this file".into())
    }
    fn collect(&self, _handle: &str, _timeout: std::time::Duration) -> TaskStatus {
        TaskStatus::Running {
            note: None,
            child: None,
        }
    }
}

/// What the door sent, as `(session, text)`, and what it raised, as `(code, detail)`.
#[derive(Default)]
struct Heard {
    sent: Mutex<Vec<(String, String)>>,
    raised: Mutex<Vec<(String, String)>>,
}

impl Heard {
    fn sent(&self) -> Vec<(String, String)> {
        self.sent.lock().unwrap().clone()
    }
    fn raised(&self, code: &str) -> Vec<String> {
        self.raised
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _)| c == code)
            .map(|(_, d)| d.clone())
            .collect()
    }
}

/// **A daemon that answers by NAME**: the sessions in `live` take the message; the rest refuse
/// with the sentence `HarnessTaskRunner::send` gives for a session that is not live.
fn a_daemon(live: &[&str], heard: Arc<Heard>) -> impl Fn(&str, &str) -> Result<String, String> {
    let live: Vec<String> = live.iter().map(|s| s.to_string()).collect();
    move |to: &str, text: &str| {
        if !live.iter().any(|s| s == to) {
            return Err(format!(
                "no session `{to}` is live in this daemon, so there is nothing to say it to"
            ));
        }
        heard
            .sent
            .lock()
            .unwrap()
            .push((to.to_string(), text.to_string()));
        Ok(format!("`{to}` was told"))
    }
}

/// **One reviewer's verdict, handed in and served** — the row claimed by this daemon with the
/// reviewer on it, the desk opened for that reviewer, the submission at the desk, and then
/// `passes` passes of [`serve`]: the first writes the verdict, the rest are the passes after it.
#[allow(clippy::too_many_arguments)]
fn judge(
    store: &Store,
    from: ReviewState,
    reviewer: &str,
    decision: Decision,
    said: Vec<String>,
    deliver: &dyn Fn(&str, &str) -> Result<String, String>,
    heard: Arc<Heard>,
    passes: usize,
) {
    assert!(
        store
            .move_review(
                ID,
                from,
                ReviewState::Reviewing,
                &Move::new(ME, "claimed")
                    .set("owner", ME)
                    .set("reviewer", reviewer),
                2,
            )
            .unwrap(),
        "the fixture must really have claimed the row"
    );
    let desk = ReviewDesk::new();
    desk.opened(
        HOST,
        reviewer,
        ID,
        ReviewRequest {
            brief: "do the work".into(),
            branch: BRANCH.into(),
            base_sha: "base".into(),
            head_sha: HEAD.into(),
            cwd: String::new(),
        },
    );
    desk.submit(
        reviewer,
        Submission {
            decision,
            reasons: said,
            looked_at: LookedAt {
                files: vec!["crates/x.rs".into()],
                commands: vec!["git diff base...head-1".into()],
            },
        },
    )
    .expect("the reviewer's verdict is taken at the desk");
    let events = move |e: SessionEvent| {
        if let SessionEvent::Warning { code, detail, .. } = e {
            heard.raised.lock().unwrap().push((code, detail));
        }
    };
    let settings = |_: &Path| Ok(QueueSettings::default());
    let pool = Pool {
        store,
        runner: &Reviewers,
        desk: &desk,
        host: HOST,
        me: ME,
        placement: ReviewerPlacement::Host,
        events: &events,
        deliver,
        settings: &settings,
        clones: std::env::temp_dir(),
    };
    for _ in 0..passes {
        serve(&pool).expect("a pass");
    }
}

// ===== Who a refusal reaches =====

/// **The child that wrote the branch is told, and nobody else is** — as a message to that live
/// session, carrying the reviewer's reasons WHOLE, the branch, the head it judged and the worktree.
///
/// The negative half is asserted in the same test on purpose: a door that told every session on
/// the store about every refusal would pass a test of the positive half alone.
#[test]
fn the_child_that_wrote_the_branch_is_told_the_reviewers_own_words() {
    let (_dir, store) = the_queue("the-child");
    let heard = Arc::new(Heard::default());
    // Every session is live — so what decides the recipient is the ROW's tree, not who could have
    // been reached.
    let deliver = a_daemon(&[CHILD, ROOT, STRANGER], heard.clone());
    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::Reject,
        reasons(),
        &deliver,
        heard.clone(),
        1,
    );

    let went = heard.sent();
    assert_eq!(went.len(), 1, "one refusal is one message: {went:?}");
    assert_eq!(
        went[0].0, CHILD,
        "to the child, and not the root or the stranger"
    );
    let text = &went[0].1;
    for asked in [
        // **The reviewer's OWN words, not a paraphrase.** A summary would be this code writing the
        // reviewer's half for it, and the child would be sent to fix something it did not say.
        "the retry is not bounded: `loop` can run for ever",
        "the test it added asserts nothing",
        // **The branch, the head the verdict is bound to, and the tree the work is in.**
        BRANCH,
        HEAD,
        WORKTREE,
        "REFUSED",
    ] {
        assert!(text.contains(asked), "`{asked}` is missing from: {text}");
    }
    assert!(
        heard.raised("refusal_undelivered").is_empty(),
        "a refusal the child took is not the operator's"
    );

    // **The verdict is on the row**, bound to the head it judged.
    let r = store.review_row(ID).unwrap().expect("the row");
    assert_eq!(r.state, ReviewState::Rejected, "{r:?}");
    assert_eq!(r.verdict.as_deref(), Some("reject"));
    assert_eq!(r.reasons, reasons());
    assert_eq!(r.reviewed_sha.as_deref(), Some(HEAD));
    assert_eq!(r.verdict_by.as_deref(), Some("reviewer:s-gatekeeper-sub-1"));
}

/// **When the child's session is gone the complaints reach the session that ENQUEUED it** — the
/// root of its tree, found by walking the store — in the ROOT's sentence: *your branch* is false
/// of a session that did not write the branch, and the root holds the operator's verbs.
#[test]
fn the_session_that_enqueued_it_is_told_when_the_child_is_gone() {
    let (_dir, store) = the_queue("the-root");
    let heard = Arc::new(Heard::default());
    // The child is GONE and the root is live: the ordinary end of a subagent.
    let deliver = a_daemon(&[ROOT, STRANGER], heard.clone());
    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::Reject,
        reasons(),
        &deliver,
        heard.clone(),
        1,
    );

    let went = heard.sent();
    assert_eq!(
        went.len(),
        1,
        "one delivery, moved to the second party: {went:?}"
    );
    assert_eq!(went[0].0, ROOT);
    let text = &went[0].1;
    for asked in [
        // **The root's sentence, naming the child and quoting why it could not be told.**
        "a child you started (`s-root-sub-1`)",
        "could not be told",
        "no session `s-root-sub-1` is live in this daemon",
        "it is yours to move",
        // **And the complaints, whole, with the branch, the tree and the operator's doors.**
        "the retry is not bounded: `loop` can run for ever",
        "the test it added asserts nothing",
        BRANCH,
        WORKTREE,
        "`/queue approve s-root-sub-1`",
        "`/queue rm s-root-sub-1`",
    ] {
        assert!(text.contains(asked), "`{asked}` is missing from: {text}");
    }
    assert!(
        !text.contains("Your branch"),
        "the root is not told its own branch came back: {text}"
    );
    assert!(heard.raised("refusal_undelivered").is_empty());
    assert_eq!(
        store.review_row(ID).unwrap().unwrap().state,
        ReviewState::Rejected,
        "a delivery that moved to the second party does not undo the refusal"
    );
}

/// **A refusal NOBODY can take is raised, and the sentence is TRUE about why** — it quotes both
/// attempts in their own words rather than asserting that a session is gone.
///
/// The two failures are deliberately different kinds: a session that is not live, and a daemon
/// whose bell has closed. *That session is gone* is false of the second.
#[test]
fn a_refusal_nobody_can_take_is_raised_with_both_attempts() {
    let (_dir, store) = the_queue("nobody");
    let heard = Arc::new(Heard::default());
    let tried: Arc<Mutex<Vec<String>>> = Default::default();
    let seen = tried.clone();
    let deliver = move |to: &str, _text: &str| -> Result<String, String> {
        seen.lock().unwrap().push(to.to_string());
        if to == CHILD {
            Err(format!(
                "no session `{to}` is live in this daemon, so there is nothing to say it to"
            ))
        } else {
            Err("the daemon's bell is closed (it is shutting down)".into())
        }
    };
    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::Reject,
        reasons(),
        &deliver,
        heard.clone(),
        1,
    );

    assert_eq!(
        *tried.lock().unwrap(),
        vec![CHILD.to_string(), ROOT.to_string()],
        "the child first, then the root one walk up — and nobody outside the tree"
    );
    let raised = heard.raised("refusal_undelivered");
    assert_eq!(raised.len(), 1, "{raised:?}");
    let said = &raised[0];
    for asked in [
        BRANCH,
        "reached nobody",
        // **Each attempt in its own party's name and its own words.**
        "`s-root-sub-1`: no session `s-root-sub-1` is live in this daemon",
        "`s-root`: the daemon's bell is closed (it is shutting down)",
        // **Where the complaints ARE, and the operator's doors.**
        "The verdict stands and is on the row",
        "`/queue restart s-root-sub-1`",
        "`/queue approve s-root-sub-1`",
        "`/queue rm s-root-sub-1`",
    ] {
        assert!(said.contains(asked), "`{asked}` is missing from: {said}");
    }
    assert!(
        !said.contains("is gone"),
        "the raise must quote the attempts rather than claim why they failed: {said}"
    );
    let r = store.review_row(ID).unwrap().unwrap();
    assert_eq!(
        r.state,
        ReviewState::Rejected,
        "a message that did not go does not undo a judgement"
    );
    assert_eq!(r.reasons, reasons());
}

/// **A child with no session row leaves the root unnamed** rather than guessing one: the store
/// holds the tree, and a row that is gone is a tree nobody can walk. The child is tried, nobody
/// else is, and the operator is told — with the one attempt that was actually made.
#[test]
fn a_child_nobody_can_walk_from_is_tried_alone_and_then_raised() {
    let (_dir, store) = the_queue_with("orphan", false);
    let heard = Arc::new(Heard::default());
    let tried: Arc<Mutex<Vec<String>>> = Default::default();
    let seen = tried.clone();
    let deliver = move |to: &str, _text: &str| -> Result<String, String> {
        seen.lock().unwrap().push(to.to_string());
        Err(format!("no session `{to}` is live in this daemon"))
    };
    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::Reject,
        reasons(),
        &deliver,
        heard.clone(),
        1,
    );
    assert_eq!(
        *tried.lock().unwrap(),
        vec![CHILD.to_string()],
        "one party, named as it is, rather than a second one invented"
    );
    let raised = heard.raised("refusal_undelivered");
    assert_eq!(raised.len(), 1, "{raised:?}");
    assert!(
        raised[0].contains("`s-root-sub-1`: no session"),
        "{}",
        raised[0]
    );
    assert!(
        !raised[0].contains("`s-root`"),
        "the root was never named: {}",
        raised[0]
    );
}

// ===== How often =====

/// **A refusal is delivered ONCE, not on every pass** — and the operator is not raised either.
/// The verdict is taken off the desk once and the row moved conditionally once; the passes after
/// it find a `rejected` row and nothing to say.
#[test]
fn a_refusal_is_delivered_once_not_on_every_pass() {
    let (_dir, store) = the_queue("once");
    let heard = Arc::new(Heard::default());
    let deliver = a_daemon(&[CHILD, ROOT], heard.clone());
    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::Reject,
        reasons(),
        &deliver,
        heard.clone(),
        5,
    );
    assert_eq!(
        heard.sent().len(),
        1,
        "five passes, one message: {:?}",
        heard.sent()
    );
    assert!(heard.raised("refusal_undelivered").is_empty());
}

/// **A SECOND refusal of the same row is a second message, with the SECOND verdict's words.**
///
/// A branch that moves after a refusal goes back to review, so one row can be refused twice — the
/// second time about a branch somebody has since repaired. A door that keyed *told* on the row id
/// alone would swallow the second verdict as a repeat of the first, which is the same defect one
/// round later. Between the two verdicts nothing is said.
#[test]
fn a_second_refusal_of_the_same_row_is_a_second_message() {
    let (_dir, store) = the_queue("twice");
    let heard = Arc::new(Heard::default());
    let deliver = a_daemon(&[CHILD, ROOT], heard.clone());
    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::Reject,
        reasons(),
        &deliver,
        heard.clone(),
        1,
    );
    assert_eq!(heard.sent().len(), 1, "the first refusal is one message");

    // **The branch moved**: the queue sends the row back to review with the old verdict cleared
    // (`RepoQueue`'s own move on a rejected branch whose head changed). Nothing is said.
    assert!(
        store
            .move_review(
                ID,
                ReviewState::Rejected,
                ReviewState::Waiting,
                &Move::new(ME, "the branch moved")
                    .set("head_sha", "head-2")
                    .clear("verdict")
                    .clear("verdict_by")
                    .clear("reviewed_sha"),
                3,
            )
            .unwrap()
    );
    assert_eq!(
        heard.sent().len(),
        1,
        "going back to review is not a refusal"
    );

    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-2",
        Decision::Reject,
        vec!["the fix touched a file the brief did not name".into()],
        &deliver,
        heard.clone(),
        3,
    );
    let went = heard.sent();
    assert_eq!(
        went.len(),
        2,
        "the second refusal is a new thing to say: {went:?}"
    );
    assert_eq!(went[1].0, CHILD);
    assert!(
        went[1]
            .1
            .contains("the fix touched a file the brief did not name"),
        "and it is the SECOND verdict's words: {}",
        went[1].1
    );
    assert!(
        !went[1].1.contains("the retry is not bounded"),
        "the first verdict's reasons are not carried into the second: {}",
        went[1].1
    );
}

// ===== The other verdicts =====

/// **`needs_human` raises for the operator, once, and messages nobody** — it is not a refusal: the
/// branch is not wrong, the reviewer could not tell, and only a person's verb moves it.
#[test]
fn needs_human_raises_for_the_operator_once_and_tells_no_session() {
    let (_dir, store) = the_queue("needs-human");
    let heard = Arc::new(Heard::default());
    let deliver = a_daemon(&[CHILD, ROOT], heard.clone());
    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::NeedsHuman,
        vec!["the brief does not say which behaviour is wanted".into()],
        &deliver,
        heard.clone(),
        4,
    );
    let raised = heard.raised("queue_needs_human");
    assert_eq!(raised.len(), 1, "raised once over four passes: {raised:?}");
    for asked in [
        BRANCH,
        "the brief does not say which behaviour is wanted",
        "`/queue approve s-root-sub-1`",
        "`/queue veto s-root-sub-1 WHY`",
        "`/queue rm s-root-sub-1`",
    ] {
        assert!(
            raised[0].contains(asked),
            "`{asked}` is missing from: {}",
            raised[0]
        );
    }
    assert!(
        heard.sent().is_empty(),
        "no session is messaged: {:?}",
        heard.sent()
    );
    let r = store.review_row(ID).unwrap().unwrap();
    assert_eq!(r.state, ReviewState::CannotDecide, "{r:?}");
    assert_eq!(r.verdict.as_deref(), Some("needs_human"));
}
