//! A reviewer's refusal reaches a LIVE session - the child that wrote the branch, or the
//! session that enqueued it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::{
    MergePriority, Move, NewReview, ReviewKind, ReviewState, SessionRecord, Store,
};
use letibot_tools::builtins::task::{Placement, TaskRunner, TaskSpec, TaskStatus};
use letibot_tools::builtins::verdict::ReviewDesk;
use letibot_tools::gatekeeper::{Decision, LookedAt, ReviewRequest, Submission};

use super::*;
use crate::mergequeue::serve::tests::TempDir;
use crate::mergequeue::settings::QueueSettings;
use crate::sessions::gatekeeper::ReviewerPlacement;

// ===== Fixtures =====

/// The two parties a refusal is between, and a third that has nothing to do with it. The stranger is
/// not decoration: a notice that fires for everybody is a notice nobody reads.
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

/// The reviewer's complaints, in its own words - one long enough that a summary could not be
/// mistaken for it.
fn reasons() -> Vec<String> {
    vec![
        "the retry is not bounded: `loop` can run for ever".to_string(),
        "the test it added asserts nothing".to_string(),
    ]
}

fn session(store: &Store, id: &str, parent: Option<&str>) {
    seated(store, id, parent, "");
}

/// A session row SEATED in a workspace - the fact the refusal's last rung reads to decide *the same
/// workspace* ([`super::super::repositories_of_workspace_cached`] over this folder).
fn seated(store: &Store, id: &str, parent: Option<&str>, workspace: &str) {
    store
        .put_session(&SessionRecord {
            id: id.to_string(),
            title: None,
            model_id: String::new(),
            dialect_sha: String::new(),
            workspace_root: workspace.to_string(),
            owner: String::new(),
            approvers: Vec::new(),
            role: None,
            parent_session_id: parent.map(str::to_string),
        })
        .expect("the session row");
}

/// The queue as the daemon leaves it when a branch is put up for review: a child under the
/// root that ran the `task_start`, a review row whose `session_id` is the CHILD, and a
/// stranger that shares the store. The child's row names its parent on purpose
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

/// A runner whose reviewers are always still running - what is under test is the verdict they
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

/// A daemon that answers by NAME: the sessions in `live` take the message; the rest refuse with the
/// sentence `HarnessTaskRunner::send` gives for a session that is not live.
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

/// A directory that looks like a reviewer's tree - it exists and holds something, which is
/// what [`verdict`] asks of the path a child was given.
fn a_tree(tag: &str) -> TempDir {
    let dir = TempDir::new(tag);
    std::fs::write(dir.0.join("a.txt"), "one\n").expect("a file in the tree");
    dir
}

/// One reviewer's verdict, handed in and served - the row claimed by this daemon with the
/// reviewer on it, the desk opened for that reviewer, the submission at the desk, and then
/// `passes` passes of [`serve`]
#[allow(clippy::too_many_arguments)]
fn judge(
    store: &Store,
    from: ReviewState,
    reviewer: &str,
    decision: Decision,
    said: Vec<String>,
    deliver: &dyn Fn(&str, &str) -> Result<String, String>,
    live: &[&str],
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
    // A real tree that OUTLIVES the passes, because `verdict` asks whether the tree a reviewer was
    // given is still there - a fixture naming `""` would be asserting a verdict about a tree that
    // does not exist, which is now a WAIT.
    let judged_tree = a_tree("judged-tree");
    desk.opened(
        HOST,
        reviewer,
        ID,
        ReviewRequest {
            brief: "do the work".into(),
            branch: BRANCH.into(),
            base_sha: "base".into(),
            head_sha: HEAD.into(),
            cwd: judged_tree.0.display().to_string(),
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
    let live: Vec<String> = live.iter().map(|s| s.to_string()).collect();
    let live = |id: &str| live.iter().any(|s| s == id);
    // A directory of this helper's own, named as the queue's is - `sweep_clones` refuses any other
    // name, so a fixture cannot be pointed at the box's real clone directory.
    let clones_dir = TempDir::new("review-judge-clones");
    let clones = clones_dir.0.join(CLONES_DIR);
    let pool = Pool {
        reviewer_skills: &[],
        machines: crate::mergequeue::review::hosts::ReviewHosts::new(),
        reviewer_hosts: Some(vec!["local".to_string()]),
        store,
        runner: &Reviewers,
        desk: &desk,
        host: HOST,
        me: ME,
        placement: ReviewerPlacement::Host,
        events: &events,
        warn: &|_: &str, e| events(e),
        deliver,
        live: &live,
        settings: &settings,
        clones,
        slots: &Vec::new,
    };
    for _ in 0..passes {
        serve(&pool).expect("a pass");
    }
}

/// One pass of the reviewers' host over an EXISTING store - what a later pass looks like when the
/// thing under test is what the queue remembers between two of them. `judge` cannot be used for the
/// second pass: it moves the row it is about into `reviewing`, and a rejected row is not there.
fn a_pass(
    store: &Store,
    deliver: &dyn Fn(&str, &str) -> Result<String, String>,
    live: &dyn Fn(&str) -> bool,
    heard: Arc<Heard>,
) -> Served {
    let events = move |e: SessionEvent| {
        if let SessionEvent::Warning { code, detail, .. } = e {
            heard.raised.lock().unwrap().push((code, detail));
        }
    };
    let settings = |_: &Path| Ok(QueueSettings::default());
    let desk = ReviewDesk::new();
    let clones_dir = TempDir::new("review-a-pass-clones");
    let pool = Pool {
        reviewer_skills: &[],
        machines: crate::mergequeue::review::hosts::ReviewHosts::new(),
        reviewer_hosts: Some(vec!["local".to_string()]),
        store,
        runner: &Reviewers,
        desk: &desk,
        host: HOST,
        me: ME,
        placement: ReviewerPlacement::Host,
        events: &events,
        warn: &|_: &str, e| events(e),
        deliver,
        live,
        settings: &settings,
        clones: clones_dir.0.join(CLONES_DIR),
        slots: &Vec::new,
    };
    serve(&pool).expect("a pass")
}

// ===== Who a refusal reaches =====

/// The live session of the branch's workspace the last rung may fall back to, a live session of a
/// DIFFERENT workspace that it must not, and a child of the first that it must not either.
const LIVE: &str = "s-live";
const ELSEWHERE: &str = "s-elsewhere";
const LIVE_CHILD: &str = "s-live-sub-1";

/// The queue when the branch's tree is dead and the workspace still has a session: the same three
/// parties, a real repository - so the queue's own reading of *which repositories a folder is a
/// session of* can answer - and a review row whose `repository` is that repository's key.
fn the_queue_in_a_repository(tag: &str) -> (TempDir, Store, String) {
    let dir = TempDir::new(&format!("refusal-{tag}"));
    let repo = crate::mergequeue::serve::tests::repo(&dir.0);
    let key = crate::mergequeue::git::repository_key(&repo).expect("the repository's key");
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    session(&store, ROOT, None);
    session(&store, CHILD, Some(ROOT));
    store
        .enqueue_review(
            &NewReview {
                id: ID.into(),
                repository: key.clone(),
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
    (dir, store, key)
}

/// The child that wrote the branch is told, and nobody else is - as a message to that live
/// session carrying the reviewer's reasons WHOLE, the branch, the head it judged and the
/// worktree. The negative half is asserted in the same test on purpose
#[test]
fn the_child_that_wrote_the_branch_is_told_the_reviewers_own_words() {
    let (_dir, store) = the_queue("the-child");
    let heard = Arc::new(Heard::default());
    // Every session is live - so what decides the recipient is the ROW's tree, not who could have
    // been reached.
    let deliver = a_daemon(&[CHILD, ROOT, STRANGER], heard.clone());
    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::Reject,
        reasons(),
        &deliver,
        &[CHILD, ROOT, STRANGER],
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
        // The reviewer's OWN words, not a paraphrase: a summary would be this code writing the
        // reviewer's half for it, and the child would be sent to fix something it did not say.
        "the retry is not bounded: `loop` can run for ever",
        "the test it added asserts nothing",
        // The branch, the head the verdict is bound to, and the tree the work is in.
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

    // The verdict is on the row, bound to the head it judged.
    let r = store.review_row(ID).unwrap().expect("the row");
    assert_eq!(r.state, ReviewState::Rejected, "{r:?}");
    assert_eq!(r.verdict.as_deref(), Some("reject"));
    assert_eq!(r.reasons, reasons());
    assert_eq!(r.reviewed_sha.as_deref(), Some(HEAD));
    assert_eq!(r.verdict_by.as_deref(), Some("reviewer:s-gatekeeper-sub-1"));
}

/// When the child's session is gone the complaints reach the session that ENQUEUED it - the root of
/// its tree, found by walking the store - in the ROOT's sentence: *your branch* is false of a session
/// that did not write the branch, and the root holds the operator's verbs.
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
        &[ROOT, STRANGER],
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
        // The root's sentence, naming the child and quoting why it could not be told.
        "a child you started (`s-root-sub-1`)",
        "could not be told",
        "no session `s-root-sub-1` is live in this daemon",
        "it is yours to move",
        // And the complaints, whole, with the branch, the tree and the operator's doors.
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

/// A refusal NOBODY can take is raised, and the sentence is TRUE about why - it quotes both
/// attempts in their own words rather than asserting that a session is gone. The two
/// failures are deliberately different kinds
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
        // No session at all is live in this daemon, so the workspace fallback has no candidate, and
        // the raise must say so rather than name a party it never tried.
        &[],
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
        // Each attempt in its own party's name and its own words.
        "`s-root-sub-1`: no session `s-root-sub-1` is live in this daemon",
        "`s-root`: the daemon's bell is closed (it is shutting down)",
        // Where the complaints ARE, and the operator's doors.
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

/// A child with no session row leaves the root unnamed rather than guessing one: the store holds the
/// tree, and a row that is gone is a tree nobody can walk. The child is tried, nobody else is, and the
/// operator is told - with the one attempt that was actually made.
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
        &[],
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

/// A refusal nobody could take is owed to whoever opens next: the verdict landed with the daemon gone and the child returns. The queue wrote the debt down, so the first session after the door opens takes it once, and the operator hears of the miss once.
#[test]
fn a_refusal_nobody_heard_is_handed_to_the_session_that_opens_later() {
    let (_dir, store) = the_queue("owed");
    let heard = Arc::new(Heard::default());
    let refused = |to: &str, _text: &str| -> Result<String, String> {
        Err(format!(
            "no session `{to}` is live in this daemon, so there is nothing to say it to"
        ))
    };
    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::Reject,
        reasons(),
        &refused,
        &[],
        heard.clone(),
        1,
    );
    assert_eq!(
        heard.raised("refusal_undelivered").len(),
        1,
        "the operator is told about the miss, once"
    );
    assert_eq!(heard.sent().len(), 0, "nobody heard it on the first pass");

    // The child comes back. The door that was closed takes the message now, and the next pass is
    // what the queue owes it.
    let heard_door = heard.clone();
    let open = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flip = open.clone();
    let door = move |to: &str, text: &str| -> Result<String, String> {
        if !flip.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(format!(
                "no session `{to}` is live in this daemon, so there is nothing to say it to"
            ));
        }
        heard_door
            .sent
            .lock()
            .unwrap()
            .push((to.to_string(), text.to_string()));
        Ok(format!("`{to}` was told"))
    };
    open.store(true, std::sync::atomic::Ordering::Relaxed);
    let nobody_live = |_: &str| false;
    let served = a_pass(&store, &door, &nobody_live, heard.clone());
    assert_eq!(
        served.redelivered, 1,
        "the pass handed the owed refusal over"
    );
    let went = heard.sent();
    assert_eq!(went.len(), 1, "{went:?}");
    assert_eq!(
        went[0].0, CHILD,
        "the child that wrote the branch, which is the door the first attempt tried first"
    );
    for asked in [
        "the retry is not bounded: `loop` can run for ever",
        "the test it added asserts nothing",
        BRANCH,
        HEAD,
        "REFUSED",
    ] {
        assert!(
            went[0].1.contains(asked),
            "`{asked}` is missing from: {}",
            went[0].1
        );
    }
    assert_eq!(
        heard.raised("refusal_undelivered").len(),
        1,
        "a debt paid is not news a second time"
    );
    assert_eq!(
        store.review_row(ID).unwrap().unwrap().state,
        ReviewState::Rejected,
        "the delivery does not undo the judgement"
    );

    // And it is paid ONCE: the next pass finds nothing written down.
    let served = a_pass(&store, &door, &nobody_live, heard.clone());
    assert_eq!(served.redelivered, 0);
    assert_eq!(
        heard.sent().len(),
        1,
        "one refusal is one message, however many passes look for a session to take it"
    );
}

/// A refusal is owed for a day, and only while its verdict stands. Two ends of the same rule: a
/// session that opens a week later is not handed a stale complaint about a branch nobody has touched
/// since, and a row a person put back in line is owed nothing at all.
#[test]
fn an_owed_refusal_expires_and_one_whose_row_moved_on_is_forgotten() {
    // A week old: past the day the door looks for.
    let (_dir, store) = the_queue("owed-old");
    let heard = Arc::new(Heard::default());
    let refused = |to: &str, _t: &str| -> Result<String, String> {
        Err(format!("no session `{to}` is live in this daemon"))
    };
    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::Reject,
        reasons(),
        &refused,
        &[],
        heard.clone(),
        1,
    );
    let key = format!("{REFUSAL_OWED}{HEAD}:{ID}");
    assert!(
        store.told(&key).unwrap().is_some(),
        "the miss is written down under the head it judged"
    );
    let week_ago = now_ms().saturating_sub(7 * 24 * 60 * 60 * 1000);
    store.untell_prefix(&key).unwrap();
    store.tell_once(&key, week_ago).unwrap();
    let takes_it = |to: &str, text: &str| Ok(format!("`{to}` took: {}", text.len()));
    let everyone_live = |_: &str| true;
    assert_eq!(
        a_pass(&store, &takes_it, &everyone_live, heard.clone()).redelivered,
        0,
        "a week-old refusal is not owed to anybody"
    );
    assert!(
        store.told(&key).unwrap().is_none(),
        "and the row that said it was owed is gone with it"
    );
    // A key under this prefix that is not one this function wrote is forgotten too, rather than
    // kept for ever by a reader that cannot pay it.
    assert!(store.tell_once("refusal:no-colon", 1).unwrap());
    a_pass(&store, &takes_it, &everyone_live, heard.clone());
    assert!(
        store.told("refusal:no-colon").unwrap().is_none(),
        "a malformed key is dropped"
    );

    // A row a person put back in line: the verdict it was rejected with is not the row's any more.
    let (_dir2, store2) = the_queue("owed-moved");
    let heard2 = Arc::new(Heard::default());
    judge(
        &store2,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::Reject,
        reasons(),
        &refused,
        &[],
        heard2.clone(),
        1,
    );
    assert!(
        store2
            .move_review(
                ID,
                ReviewState::Rejected,
                ReviewState::Waiting,
                &Move::new("operator:s-1", "restarted by a person"),
                3,
            )
            .unwrap()
    );
    let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tally = sent.clone();
    let counting = move |_to: &str, _text: &str| {
        tally.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(String::from("told"))
    };
    assert_eq!(
        a_pass(&store2, &counting, &everyone_live, heard2.clone()).redelivered,
        0,
        "a row that is not rejected is owed nothing"
    );
    assert_eq!(
        sent.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "and nobody was told about it"
    );
    assert!(
        store2.told_with_prefix(REFUSAL_OWED).unwrap().is_empty(),
        "the debt is forgotten once its verdict no longer stands"
    );
}

/// The refusal reaches a live session of the branch's workspace when the branch's own tree
/// is dead - a child that finished hours earlier, a root that exists in the store with
/// nobody attached, and reasons that ended in a card saying `refused, told nobody`.
#[test]
fn a_refusal_reaches_the_live_session_of_the_workspace_when_the_branchs_tree_is_dead() {
    let (dir, store, key) = the_queue_in_a_repository("workspace");
    // Seated in the FOLDER the repository is in, not in the repository: that is how a session is
    // seated on this box, and it is the arm of the workspace rule a path comparison would pass by
    // accident.
    seated(&store, LIVE, None, &dir.0.display().to_string());
    seated(
        &store,
        ELSEWHERE,
        None,
        "/a/workspace/that/is/not/this/repository",
    );
    seated(&store, LIVE_CHILD, Some(LIVE), &key);
    let heard = Arc::new(Heard::default());
    // The child is GONE and the root is dead - the operator's case - and one session of the
    // workspace is live.
    let deliver = a_daemon(&[LIVE, ELSEWHERE, LIVE_CHILD], heard.clone());
    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::Reject,
        reasons(),
        &deliver,
        &[LIVE, ELSEWHERE, LIVE_CHILD],
        heard.clone(),
        1,
    );

    let went = heard.sent();
    assert_eq!(went.len(), 1, "one refusal is one message: {went:?}");
    assert_eq!(
        went[0].0, LIVE,
        "the live session of the branch's workspace, and no other"
    );
    let text = &went[0].1;
    let asked: Vec<String> = vec![
        // Why this session: the author could not be told, and this is the live one here.
        "could not be told".into(),
        "no session `s-root-sub-1` is live in this daemon".into(),
        format!("this session is the live one in {}", dir.0.display()),
        format!("a session of {key}"),
        // The reviewer's own words, whole, with the branch, the head and the tree.
        "the retry is not bounded: `loop` can run for ever".into(),
        "the test it added asserts nothing".into(),
        BRANCH.into(),
        HEAD.into(),
        WORKTREE.into(),
        "REFUSED".into(),
        // And the operator's doors, since the branch is somebody else's.
        "`/queue approve s-root-sub-1`".into(),
        "`/queue rm s-root-sub-1`".into(),
    ];
    for asked in &asked {
        assert!(
            text.contains(asked.as_str()),
            "`{asked}` is missing from: {text}"
        );
    }
    assert!(
        !text.contains("Your branch"),
        "the workspace's session is not told it wrote the branch: {text}"
    );
    assert!(
        heard.raised("refusal_undelivered").is_empty(),
        "somebody took it, so the operator is not raised: {:?}",
        heard.raised("refusal_undelivered")
    );
    assert_eq!(
        store.review_row(ID).unwrap().unwrap().state,
        ReviewState::Rejected,
        "the verdict stands whether or not a message went"
    );
}

/// One refusal is one message: the first party that takes it is the only one told - with
/// the root and a live session of the workspace both able to take it, the root is told and
/// the workspace's session is not asked at all. The negative half is the point
#[test]
fn the_first_party_that_takes_it_is_the_only_one_told() {
    let (_dir, store, key) = the_queue_in_a_repository("first-wins");
    seated(&store, LIVE, None, &key);
    let heard = Arc::new(Heard::default());
    let deliver = a_daemon(&[ROOT, LIVE], heard.clone());
    judge(
        &store,
        ReviewState::Waiting,
        "s-gatekeeper-sub-1",
        Decision::Reject,
        reasons(),
        &deliver,
        &[ROOT, LIVE],
        heard.clone(),
        1,
    );
    let went = heard.sent();
    assert_eq!(went.len(), 1, "one refusal is one message: {went:?}");
    assert_eq!(
        went[0].0, ROOT,
        "the root's rung comes before the workspace's, so the workspace's session is not told"
    );
    assert!(heard.raised("refusal_undelivered").is_empty());
}

/// A refusal nothing can take names every party it tried - the child, the root, and the
/// fact that no live session of the branch's workspace was left. The session row of this
/// workspace that no daemon holds is never even messaged
#[test]
fn a_refusal_nobody_anywhere_can_take_names_every_party_it_tried() {
    let (_dir, store, key) = the_queue_in_a_repository("nobody-at-all");
    seated(&store, LIVE, None, &key);
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
        // Nothing is live - the workspace's session row has nobody attached to it.
        &[],
        heard.clone(),
        1,
    );

    assert_eq!(
        *tried.lock().unwrap(),
        vec![CHILD.to_string(), ROOT.to_string()],
        "the child, then the root — and the dead session of the workspace is never messaged"
    );
    let raised = heard.raised("refusal_undelivered");
    assert_eq!(raised.len(), 1, "{raised:?}");
    let said = &raised[0];
    let asked: Vec<String> = vec![
        BRANCH.into(),
        "reached nobody".into(),
        // Each attempt in its own party's name and its own words.
        "`s-root-sub-1`: no session `s-root-sub-1` is live in this daemon".into(),
        "`s-root`: the daemon's bell is closed (it is shutting down)".into(),
        // And the scan that found nobody left to try.
        format!("no live top-level session of `{key}` is open in this daemon"),
        "The verdict stands and is on the row".into(),
        "`/queue approve s-root-sub-1`".into(),
    ];
    for asked in &asked {
        assert!(
            said.contains(asked.as_str()),
            "`{asked}` is missing from: {said}"
        );
    }
}

// ===== How often =====

/// A refusal is delivered ONCE, not on every pass - and the operator is not raised either. The verdict
/// is taken off the desk once and the row moved conditionally once; the passes after it find a
/// `rejected` row and nothing to say.
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
        &[CHILD, ROOT],
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

/// A SECOND refusal of the same row is a second message, with the SECOND verdict's words. A
/// branch that moves after a refusal goes back to review, so one row can be refused twice -
/// the second time about a branch somebody has since repaired.
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
        &[CHILD, ROOT],
        heard.clone(),
        1,
    );
    assert_eq!(heard.sent().len(), 1, "the first refusal is one message");

    // The branch moved: the queue sends the row back to review with the old verdict cleared. Nothing
    // is said.
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
        &[CHILD, ROOT],
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

/// `needs_human` raises for the operator, once, and messages nobody - it is not a refusal: the branch
/// is not wrong, the reviewer could not tell, and only a person's verb moves it.
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
        &[CHILD, ROOT],
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

// ===== A fix is for the tip that is red NOW =====

/// A red tip's reject opens no fix branch once the target has moved past that tip: the
/// reviewer judged a tree the target no longer has, so a branch cut from it could never
/// land.
#[test]
fn a_reject_against_a_tip_the_target_has_moved_past_opens_no_fix_branch() {
    let dir = TempDir::new("fix-superseded");
    let repo = crate::mergequeue::serve::tests::repo(&dir.0);
    let red = crate::mergequeue::serve::tests::commit(
        &repo,
        "broken",
        "x\n",
        "a commit that breaks the gate",
    );
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    store
        .enqueue_review(
            &NewReview {
                id: "red-main-1".into(),
                repository: repo.display().to_string(),
                target: "main".into(),
                kind: ReviewKind::Target,
                session_id: String::new(),
                branch: "main".into(),
                worktree: None,
                base_sha: red.clone(),
                head_sha: red.clone(),
                brief: "what broke main".into(),
                priority: MergePriority::Urgent,
                fixes: None,
                evidence: "`main` went red".into(),
            },
            "test",
            1,
        )
        .expect("the red tip's review");
    // `main` moves before the reviewer hands its verdict in.
    let moved = crate::mergequeue::serve::tests::commit(&repo, "broken", "y\n", "the tip after it");
    assert!(
        store
            .move_review(
                "red-main-1",
                ReviewState::Waiting,
                ReviewState::Reviewing,
                &Move::new(ME, "claimed")
                    .set("owner", ME)
                    .set("reviewer", "sub-1"),
                2,
            )
            .unwrap()
    );
    let desk = ReviewDesk::new();
    // A real directory that outlives the pass: `verdict` asks whether the reviewer's tree is there.
    let red_tree = a_tree("red-tree");
    desk.opened(
        HOST,
        "sub-1",
        "red-main-1",
        ReviewRequest {
            brief: "what broke main".into(),
            branch: "main".into(),
            base_sha: red.clone(),
            head_sha: red.clone(),
            cwd: red_tree.0.display().to_string(),
        },
    );
    desk.submit(
        "sub-1",
        Submission {
            decision: Decision::Reject,
            reasons: vec!["the commit before it broke the build".into()],
            looked_at: LookedAt {
                files: vec!["AGENTS.md".into()],
                commands: vec!["git log --oneline -20".into()],
            },
        },
    )
    .expect("the verdict is taken at the desk");
    let heard = Arc::new(Heard::default());
    let deliver = a_daemon(&[], heard.clone());
    let events = {
        let heard = heard.clone();
        move |e: SessionEvent| {
            if let SessionEvent::Warning { code, detail, .. } = e {
                heard.raised.lock().unwrap().push((code, detail));
            }
        }
    };
    let settings = |_: &Path| Ok(QueueSettings::default());
    // A directory of this fixture's own, named as the queue's is - a sweep refuses anything else, so a
    // fixture cannot be pointed at `std::env::temp_dir` and delete the operator's real clones.
    let clones = dir.0.join(CLONES_DIR);
    let pool = Pool {
        reviewer_skills: &[],
        machines: crate::mergequeue::review::hosts::ReviewHosts::new(),
        reviewer_hosts: Some(vec!["local".to_string()]),
        store: &store,
        runner: &Reviewers,
        desk: &desk,
        host: HOST,
        me: ME,
        placement: ReviewerPlacement::Host,
        events: &events,
        warn: &|_: &str, e| events(e),
        deliver: &deliver,
        // This fixture holds no session, and its `deliver` refuses every message.
        live: &|_: &str| false,
        settings: &settings,
        clones,
        slots: &Vec::new,
    };
    serve(&pool).expect("a pass");

    let r = store.review_row("red-main-1").unwrap().unwrap();
    assert_eq!(r.state, ReviewState::Rejected, "{r:?}");
    let why = store
        .transitions("review", "red-main-1")
        .unwrap()
        .last()
        .unwrap()
        .why
        .clone();
    assert!(why.starts_with("no fix branch was opened:"), "{why}");
    assert!(why.contains(&red) && why.contains(&moved), "{why}");
    assert!(
        heard.raised("target_red").is_empty(),
        "nothing is raised: the new tip is gated, and reviewed if it is red too — {:?}",
        heard.raised("target_red")
    );
    assert!(
        store
            .review_row("red-main-1")
            .unwrap()
            .unwrap()
            .fixes
            .is_none(),
        "no fixer was recorded on the row"
    );
}

// ===== Where a reviewer works =====

/// What a [`Recorder`] was asked to start: the prompt verbatim, and the spec the spawn carries.
#[derive(Default)]
struct Started {
    items: Mutex<Vec<(String, TaskSpec)>>,
}

impl Started {
    /// The one reviewer that was started - a second one is a failure, not a detail.
    fn one(&self) -> (String, TaskSpec) {
        let items = self.items.lock().unwrap();
        assert_eq!(items.len(), 1, "one reviewer for one row");
        items[0].clone()
    }
}

/// A runner that records what it was handed and answers a handle. Nothing is spawned: what is
/// under test is the TREE a reviewer is given, which is decided before any child exists.
struct Recorder {
    started: Arc<Started>,
}

impl TaskRunner for Recorder {
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
        self.started
            .items
            .lock()
            .unwrap()
            .push((prompt.to_string(), spec.clone()));
        Ok("s-gatekeeper-sub-1".into())
    }
    fn collect(&self, _handle: &str, _timeout: std::time::Duration) -> TaskStatus {
        TaskStatus::Running {
            note: None,
            child: None,
        }
    }
}

/// A reviewer on this machine works in a checkout of the head it judges, and NOT at
/// `main`'s tip: a reviewer that judges the wrong tree is worse than a slow one.
#[test]
fn a_reviewer_on_this_machine_works_in_a_checkout_of_the_head_it_judges() {
    use crate::mergequeue::serve::tests::{branch, commit, repo, sh};

    let dir = TempDir::new("review-checkout");
    let repo = repo(&dir.0);
    let (_wt, base, head) = branch(&repo, "the-work", "b.txt");
    // The tip moves past the head after the branch was cut, so a tree at `main`'s tip and a tree at
    // `head_sha` differ in a file this test reads.
    let tip = commit(&repo, "a.txt", "two\n", "main moves on");
    assert_ne!(head, tip, "the fixture needs two different commits");

    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    store
        .enqueue_review(
            &NewReview {
                id: "the-review".into(),
                repository: repo.display().to_string(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: CHILD.into(),
                branch: "agent/the-work".into(),
                worktree: None,
                base_sha: base.clone(),
                head_sha: head.clone(),
                brief: "do the work".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            1,
        )
        .expect("the review row");
    assert!(
        store
            .move_review(
                "the-review",
                ReviewState::Waiting,
                ReviewState::Reviewing,
                &Move::new(ME, "claimed").set("owner", ME),
                2,
            )
            .unwrap(),
        "the fixture must really have claimed the row"
    );

    let started = Arc::new(Started::default());
    let runner = Recorder {
        started: started.clone(),
    };
    let desk = ReviewDesk::new();
    let heard = Arc::new(Heard::default());
    let deliver = a_daemon(&[], heard.clone());
    let events = |_: SessionEvent| {};
    let settings = |_: &Path| Ok(QueueSettings::default());
    let clones = dir.0.join(CLONES_DIR);
    let pool = Pool {
        store: &store,
        runner: &runner,
        desk: &desk,
        host: HOST,
        me: ME,
        placement: ReviewerPlacement::Host,
        events: &events,
        warn: &|_: &str, _e| {},
        deliver: &deliver,
        // This fixture holds no session, and its `deliver` refuses every message.
        live: &|_: &str| false,
        settings: &settings,
        clones: clones.clone(),
        reviewer_skills: &[],
        machines: crate::mergequeue::review::hosts::ReviewHosts::new(),
        reviewer_hosts: Some(vec!["local".to_string()]),
        slots: &Vec::new,
    };
    assert_eq!(
        serve(&pool).expect("a pass").started,
        1,
        "the claimed row's reviewer is started"
    );

    let (prompt, spec) = started.one();
    let tree = spec
        .worktree
        .as_ref()
        .expect("a host reviewer is given a tree of its own")
        .path
        .clone();
    let tree = Path::new(&tree);
    assert_ne!(
        tree,
        repo.as_path(),
        "a reviewer must not read the operator's checkout"
    );
    assert!(
        tree.starts_with(&clones),
        "the checkout is the queue's own, under {}: {}",
        clones.display(),
        tree.display()
    );
    assert_eq!(
        sh(tree, &["rev-parse", "HEAD"]),
        head,
        "the reviewer's working directory is at the head the row names"
    );
    assert_ne!(
        sh(tree, &["rev-parse", "HEAD"]),
        tip,
        "and NOT at main's tip"
    );
    assert_eq!(
        std::fs::read_to_string(tree.join("a.txt")).expect("the file main moved"),
        "one\n",
        "the tree holds the branch's bytes, not the tip's"
    );
    assert!(
        tree.join("b.txt").exists(),
        "and the file the branch added: {}",
        tree.display()
    );
    assert!(
        prompt.contains(&tree.display().to_string()),
        "the prompt sends the reviewer's `bash` to the same tree it works in: {prompt}"
    );
}

/// A project that asks for VMs still gets one: the isolated arm, which no test of its own
/// was left guarding when the host became the default. Nothing is booted.
#[test]
fn a_project_that_asks_for_vms_gets_its_reviewer_placed_in_one() {
    use crate::mergequeue::serve::tests::{branch, repo, sh};

    let dir = TempDir::new("review-vm");
    let repo = repo(&dir.0);
    let (_wt, base, head) = branch(&repo, "the-work", "b.txt");
    // The project says it in the file the daemon reads: so what is under test is the answer the daemon
    // itself would act on, not a placement this test builds and then believes.
    std::fs::write(repo.join("leticode.toml"), "[queue]\nreviewers = \"vm\"\n")
        .expect("the project file");
    let cfg = crate::config::Config::for_the_daemon(&repo);
    assert_eq!(
        cfg.reviewers,
        crate::config::Reviewers::Vm,
        "the daemon's own constructor reads the project's answer"
    );
    // This box can boot one; a box that cannot is `ReviewerPlacement::NoVm`, asserted in
    // `gatekeeper`'s tests, where the box is an argument rather than a race on `$FIRECODE_BIN`.
    let placement = crate::sessions::gatekeeper::reviewer_placement(
        cfg.reviewers == crate::config::Reviewers::Vm,
        Ok(PathBuf::from("/usr/bin/firecode")),
    );
    assert_eq!(
        placement,
        ReviewerPlacement::Vm,
        "a project that asks for the isolation is placed in a VM, not quietly on the host"
    );

    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    store
        .enqueue_review(
            &NewReview {
                id: "the-review".into(),
                repository: repo.display().to_string(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: CHILD.into(),
                branch: "agent/the-work".into(),
                worktree: None,
                base_sha: base.clone(),
                head_sha: head.clone(),
                brief: "do the work".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            1,
        )
        .expect("the review row");
    assert!(
        store
            .move_review(
                "the-review",
                ReviewState::Waiting,
                ReviewState::Reviewing,
                &Move::new(ME, "claimed").set("owner", ME),
                2,
            )
            .unwrap(),
        "the fixture must really have claimed the row"
    );

    let started = Arc::new(Started::default());
    let runner = Recorder {
        started: started.clone(),
    };
    let desk = ReviewDesk::new();
    let deliver = |_: &str, _: &str| -> Result<String, String> { Err("nobody here".into()) };
    let events = |_: SessionEvent| {};
    let settings = |_: &Path| Ok(QueueSettings::default());
    let clones = dir.0.join(CLONES_DIR);
    let pool = Pool {
        store: &store,
        runner: &runner,
        desk: &desk,
        host: HOST,
        me: ME,
        placement,
        events: &events,
        warn: &|_: &str, _e| {},
        deliver: &deliver,
        live: &|_: &str| false,
        settings: &settings,
        clones: clones.clone(),
        slots: &Vec::new,
        reviewer_skills: &[],
        machines: crate::mergequeue::review::hosts::ReviewHosts::new(),
        reviewer_hosts: Some(vec!["local".to_string()]),
    };
    assert_eq!(
        serve(&pool).expect("a pass").started,
        1,
        "the claimed row's reviewer is started"
    );

    let (prompt, spec) = started.one();
    assert_eq!(
        spec.placement,
        Placement::Firecode,
        "what the spawn carries is a VM, which is the whole of what `[queue] reviewers` chose"
    );
    let source = spec
        .vm_source
        .as_ref()
        .expect("a VM is booted on the clone of the head the row names");
    assert!(
        source.starts_with(&clones),
        "and on the queue's own clone, under {}: {}",
        clones.display(),
        source.display()
    );
    assert_eq!(
        sh(source, &["rev-parse", "HEAD"]),
        head,
        "the clone the VM copies is at the head the row names"
    );
    assert!(
        spec.worktree.is_none(),
        "a VM works on its own copy, so there is no worktree in this machine for it to hold"
    );
    // `cwd` is empty for this arm: inside the VM the clone is the guest's project, and an absolute
    // path from here names nothing there.
    let open = desk.open(HOST);
    assert_eq!(open.len(), 1, "one reviewer, one desk entry");
    assert!(
        open[0].req.cwd.is_empty(),
        "the reviewer's `cwd` names no path outside the VM: {:?}",
        open[0].req.cwd
    );
    assert!(
        !prompt.contains(&source.display().to_string()),
        "and the prompt sends it nowhere absolute either: {prompt}"
    );
}

/// A clone nobody is on any more is removed by the next pass, and a live row's is kept -
/// the assertion that would have caught hundreds of megabytes of never-removed clones (see
/// [`sweep_clones`]). The row that matters most is the WITHDRAWN one
#[test]
fn a_clone_no_row_is_on_is_removed_and_a_live_rows_is_kept() {
    let dir = TempDir::new("review-sweep");
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    for (id, state) in [
        ("withdrawn", ReviewState::Withdrawn),
        ("waiting", ReviewState::Waiting),
        ("reviewing", ReviewState::Reviewing),
    ] {
        store
            .enqueue_review(
                &NewReview {
                    id: id.into(),
                    repository: "/the/repository".into(),
                    target: "main".into(),
                    kind: ReviewKind::Branch,
                    session_id: format!("s-{id}"),
                    branch: format!("agent/{id}"),
                    worktree: None,
                    base_sha: "base".into(),
                    head_sha: "head".into(),
                    brief: format!("do {id}"),
                    priority: MergePriority::Subagent,
                    fixes: None,
                    evidence: "enqueued".into(),
                },
                "test",
                1,
            )
            .expect("the review row");
        if state != ReviewState::Waiting {
            assert!(
                store
                    .move_review(
                        id,
                        ReviewState::Waiting,
                        state,
                        &Move::new(ME, "moved")
                            .set("owner", ME)
                            .set("reviewer", "sub-x"),
                        2,
                    )
                    .unwrap(),
                "{id} must really be {state:?}"
            );
        }
    }
    let clones = dir.0.join(CLONES_DIR);
    for name in [
        "review-withdrawn",
        "review-waiting",
        "review-reviewing",
        // Never a row of this store: what a daemon that died left behind.
        "review-s-gone-sub-1",
        // And a name that is not a review's, which the sweep must not touch.
        "something-else",
    ] {
        let p = clones.join(name);
        std::fs::create_dir_all(&p).expect("a directory");
        std::fs::write(p.join("a.txt"), "x").expect("a file in it");
    }
    // A FILE called like a clone is not a directory and is left alone.
    std::fs::write(clones.join("review-a-file"), "x").expect("a file");

    let started = Arc::new(Started::default());
    let runner = Recorder {
        started: started.clone(),
    };
    let desk = ReviewDesk::new();
    let heard = Arc::new(Heard::default());
    let deliver = a_daemon(&[], heard.clone());
    let events = |_: SessionEvent| {};
    let settings = |_: &Path| Ok(QueueSettings::default());
    let pool = Pool {
        store: &store,
        runner: &runner,
        desk: &desk,
        host: HOST,
        me: ME,
        placement: ReviewerPlacement::Host,
        events: &events,
        warn: &|_: &str, _e| {},
        deliver: &deliver,
        // This fixture holds no session, and its `deliver` refuses every message.
        live: &|_: &str| false,
        settings: &settings,
        clones: clones.clone(),
        reviewer_skills: &[],
        machines: crate::mergequeue::review::hosts::ReviewHosts::new(),
        reviewer_hosts: Some(vec!["local".to_string()]),
        slots: &Vec::new,
    };
    // One sweep, with the grace named as zero: the row rule is this test's subject, and the grace is
    // the next one's.
    sweep_clones_with(&pool, std::time::Duration::ZERO);

    assert!(
        !clones.join("review-withdrawn").exists(),
        "a withdrawn row's clone is gone: nothing else will ever remove it"
    );
    assert!(
        !clones.join("review-s-gone-sub-1").exists(),
        "and a dead daemon's is gone too — its row is on no store"
    );
    assert!(
        clones.join("review-waiting").exists(),
        "a waiting row's clone is kept: its reviewer is still to be started"
    );
    assert!(
        clones.join("review-reviewing").exists(),
        "and a reviewing row's is kept: a reviewer is in it"
    );
    assert!(
        clones.join("something-else").exists(),
        "a directory that is not a review's is never touched"
    );
    assert!(
        clones.join("review-a-file").is_file(),
        "a file is not a directory, and is left alone"
    );
}

// ===== A tree that is still being written =====

/// Make `path` look `secs` seconds old - the sweep's grace is read off the tree's own mtime, so a
/// test that needs an old tree has to say so.
fn age(path: &Path, secs: u64) {
    let at = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
    let f = std::fs::File::open(path).expect("the tree");
    f.set_times(std::fs::FileTimes::new().set_modified(at))
        .expect("its mtime");
}

/// A live row's trees are NEVER swept, however old; a dead row's go once they are old - the
/// rule that makes the sweep safe ([`sweep_clones`]), asserted apart from the store's own
/// words about a row.
#[test]
fn a_live_rows_trees_are_never_swept_and_a_dead_rows_go_once_they_are_old() {
    let dir = TempDir::new("review-grace");
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    for (id, state) in [
        ("gone", ReviewState::Withdrawn),
        ("live", ReviewState::Reviewing),
    ] {
        store
            .enqueue_review(
                &NewReview {
                    id: id.into(),
                    repository: "/the/repository".into(),
                    target: "main".into(),
                    kind: ReviewKind::Branch,
                    session_id: format!("s-{id}"),
                    branch: format!("agent/{id}"),
                    worktree: None,
                    base_sha: "base".into(),
                    head_sha: "head".into(),
                    brief: "do the work".into(),
                    priority: MergePriority::Subagent,
                    fixes: None,
                    evidence: "enqueued".into(),
                },
                "test",
                1,
            )
            .expect("the review row");
        if state != ReviewState::Waiting {
            assert!(
                store
                    .move_review(
                        id,
                        ReviewState::Waiting,
                        state,
                        &Move::new(ME, "moved")
                            .set("owner", ME)
                            .set("reviewer", "sub-x"),
                        2,
                    )
                    .unwrap(),
                "{id} must really be {state:?}"
            );
        }
    }
    let clones = dir.0.join(CLONES_DIR);
    let pid = std::process::id();
    let tree = |name: &str| {
        let p = clones.join(name);
        std::fs::create_dir_all(&p).expect("a directory");
        std::fs::write(p.join("a.txt"), "x").expect("a file in it");
        p
    };
    // One attempt's tree of the withdrawn row, that tree being made, and one made before trees were
    // per-attempt; then the same three for a row that is LIVE.
    let complete = tree(&format!("review-gone-{pid}-0"));
    let writing = tree(&format!(".review-gone-{pid}-1.tmp"));
    let legacy = tree("review-gone");
    let live_tree = tree(&format!("review-live-{pid}-2"));
    let live_writing = tree(&format!(".review-live-{pid}-3.tmp"));
    // And one nobody's row names at all.
    let orphan = tree(&format!("review-nobody-{pid}-4"));

    let started = Arc::new(Started::default());
    let runner = Recorder {
        started: started.clone(),
    };
    let desk = ReviewDesk::new();
    let heard = Arc::new(Heard::default());
    let deliver = a_daemon(&[], heard.clone());
    let events = |_: SessionEvent| {};
    let settings = |_: &Path| Ok(QueueSettings::default());
    let pool = Pool {
        store: &store,
        runner: &runner,
        desk: &desk,
        host: HOST,
        me: ME,
        placement: ReviewerPlacement::Host,
        events: &events,
        warn: &|_: &str, _e| {},
        deliver: &deliver,
        settings: &settings,
        // Every session this fixture names counts as live: these two tests are about a
        // verdict and a deadline, not about liveness.
        live: &|_: &str| true,
        clones: clones.clone(),
        reviewer_skills: &[],
        machines: crate::mergequeue::review::hosts::ReviewHosts::new(),
        reviewer_hosts: Some(vec!["local".to_string()]),
        slots: &Vec::new,
    };
    // The REAL grace, and every tree YOUNG. Nothing of the dead row's goes yet - a clone may still be
    // being written - and nothing of the live row's goes ever.
    sweep_clones(&pool);
    for p in [
        &complete,
        &writing,
        &legacy,
        &live_tree,
        &live_writing,
        &orphan,
    ] {
        assert!(
            p.exists(),
            "a tree younger than the grace is kept: {}",
            p.display()
        );
    }

    // A day later. The dead row's trees and the orphan go; the LIVE row's stay, however old - its
    // previous reviewer may be standing in one of them right now.
    for p in [
        &complete,
        &writing,
        &legacy,
        &live_tree,
        &live_writing,
        &orphan,
    ] {
        age(p, 24 * 3600);
    }
    sweep_clones(&pool);
    for p in [&complete, &writing, &legacy, &orphan] {
        assert!(
            !p.exists(),
            "a dead row's tree, and one no row names, is reclaimed: {}",
            p.display()
        );
    }
    for p in [&live_tree, &live_writing] {
        assert!(
            p.exists(),
            "a LIVE row's trees are never swept — not its attempt's tree, not one being made: {}",
            p.display()
        );
    }
}

/// The row a tree belongs to is read off its name, in all three spellings - the parse the sweep rests
/// on, asserted on its own, because a name that parses as the wrong row is a tree swept for another
/// row's state.
#[test]
fn a_trees_name_names_its_row() {
    for (name, want) in [
        ("review-m-mine-4711-0", Some("m-mine")),
        // A row id that ends in numbers: the token is stripped from the END.
        (
            "review-s-root-sub-1793298319066201000-4711-12",
            Some("s-root-sub-1793298319066201000"),
        ),
        (".review-m-mine-4711-0.tmp", Some("m-mine")),
        // A tree made before trees were per-attempt.
        ("review-red-main-1700000000", Some("red-main-1700000000")),
        ("review-m-mine", Some("m-mine")),
        // Not a review's at all.
        ("something-else", None),
        ("review-", Some("")),
    ] {
        assert_eq!(review_id(name), want, "{name}");
    }
}

/// A real clone with the sweep running against it, and the row NOT live, is not disturbed -
/// the interleaving that matters: `git clone` writing a tree while the review pass sweeps
/// the directory it is writing into.
#[test]
fn the_sweep_does_not_disturb_a_clone_it_meets() {
    use crate::mergequeue::serve::tests::{branch, repo, sh};

    let dir = TempDir::new("review-sweep-clone");
    let repo = repo(&dir.0);
    let (_wt, base, head) = branch(&repo, "the-work", "b.txt");
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    store
        .enqueue_review(
            &NewReview {
                id: "cloning".into(),
                repository: repo.display().to_string(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: "s-cloning".into(),
                branch: "agent/the-work".into(),
                worktree: None,
                base_sha: base,
                head_sha: head.clone(),
                brief: "do the work".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            1,
        )
        .expect("the review row");
    assert!(
        store
            .move_review(
                "cloning",
                ReviewState::Waiting,
                ReviewState::Withdrawn,
                &Move::new(ME, "a person vetoed it while the clone was being made"),
                2,
            )
            .unwrap(),
        "the row must really be withdrawn"
    );
    let clones = dir.0.join(CLONES_DIR);
    std::fs::create_dir_all(&clones).expect("the clone directory");
    let started = Arc::new(Started::default());
    let runner = Recorder {
        started: started.clone(),
    };
    let desk = ReviewDesk::new();
    let heard = Arc::new(Heard::default());
    let deliver = a_daemon(&[], heard.clone());
    let events = |_: SessionEvent| {};
    let settings = |_: &Path| Ok(QueueSettings::default());
    let pool = Pool {
        store: &store,
        runner: &runner,
        desk: &desk,
        host: HOST,
        me: ME,
        placement: ReviewerPlacement::Host,
        events: &events,
        warn: &|_: &str, _e| {},
        deliver: &deliver,
        settings: &settings,
        // Every session this fixture names counts as live: these two tests are about a
        // verdict and a deadline, not about liveness.
        live: &|_: &str| true,
        clones: clones.clone(),
        reviewer_skills: &[],
        machines: crate::mergequeue::review::hosts::ReviewHosts::new(),
        reviewer_hosts: Some(vec!["local".to_string()]),
        slots: &Vec::new,
    };
    let stem = clones.join("review-cloning");
    let clone = {
        let (repo, stem, head) = (repo.clone(), stem.clone(), head.clone());
        std::thread::spawn(move || git::clone_branch(&repo, "agent/the-work", &head, &stem))
    };
    // Sweep until the clone is done, the way another daemon's pass would: every millisecond, with
    // the daemon's own grace.
    let mut sweeps = 0;
    while !clone.is_finished() {
        sweep_clones(&pool);
        sweeps += 1;
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let tree = clone.join().expect("the clone thread").unwrap_or_else(|e| {
        panic!("the clone was disturbed by the sweep after {sweeps} of them: {e}")
    });
    sweep_clones(&pool);
    assert!(
        tree.exists(),
        "a tree younger than the grace survives the pass that follows its own clone"
    );
    assert_eq!(
        sh(&tree, &["rev-parse", "HEAD"]),
        head,
        "the tree is the head the row names"
    );
    assert_eq!(
        sh(&tree, &["status", "--porcelain"]),
        "",
        "and it is complete: no half-written clone was left at the row's name"
    );
    let names: Vec<String> = std::fs::read_dir(&clones)
        .expect("the clone directory")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names.len(),
        1,
        "one tree, and no debris beside it: {names:?}"
    );
    assert!(
        names[0].starts_with("review-cloning-") && !names[0].ends_with(".tmp"),
        "{names:?}"
    );
}

// ===== A tree that could not be made =====

/// A clone that fails is a WAIT, not an attempt, and the next pass tries again - the three
/// attempts are for a review that cannot RUN, and a tree that could not be made says
/// nothing about the branch (see [`Pool::wait_for_tree`]).
#[test]
fn a_clone_that_fails_is_a_wait_and_the_next_pass_tries_again() {
    use crate::mergequeue::serve::tests::{repo, sh};

    let dir = TempDir::new("review-tree-wait");
    let repo = repo(&dir.0);
    // The head the row names is a real commit, and the branch it names exists - until the moment
    // the first clone is asked for.
    let head = sh(&repo, &["rev-parse", "main"]);
    sh(&repo, &["branch", "agent/the-work", &head]);
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    store
        .enqueue_review(
            &NewReview {
                id: "the-review".into(),
                repository: repo.display().to_string(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: CHILD.into(),
                branch: "agent/the-work".into(),
                worktree: None,
                base_sha: head.clone(),
                head_sha: head.clone(),
                brief: "do the work".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            1,
        )
        .expect("the review row");
    let claim = |store: &Store| {
        assert!(
            store
                .move_review(
                    "the-review",
                    ReviewState::Waiting,
                    ReviewState::Reviewing,
                    &Move::new(ME, "claimed").set("owner", ME).set("failure", ""),
                    2,
                )
                .unwrap(),
            "the fixture must really have claimed the row"
        );
    };
    claim(&store);
    // The branch goes away between the claim and the clone: `git clone --branch` refuses it.
    sh(&repo, &["branch", "-D", "agent/the-work"]);

    let started = Arc::new(Started::default());
    let runner = Recorder {
        started: started.clone(),
    };
    let desk = ReviewDesk::new();
    let heard = Arc::new(Heard::default());
    let deliver = a_daemon(&[], heard.clone());
    let events = {
        let heard = heard.clone();
        move |e: SessionEvent| {
            if let SessionEvent::Warning { code, detail, .. } = e {
                heard.raised.lock().unwrap().push((code, detail));
            }
        }
    };
    let settings = |_: &Path| Ok(QueueSettings::default());
    let clones = dir.0.join(CLONES_DIR);
    let pool = Pool {
        store: &store,
        runner: &runner,
        desk: &desk,
        host: HOST,
        me: ME,
        placement: ReviewerPlacement::Host,
        events: &events,
        warn: &|_: &str, e| events(e),
        deliver: &deliver,
        settings: &settings,
        live: &|_: &str| true,
        clones: clones.clone(),
        reviewer_skills: &[],
        machines: crate::mergequeue::review::hosts::ReviewHosts::new(),
        reviewer_hosts: Some(vec!["local".to_string()]),
        slots: &Vec::new,
    };
    assert_eq!(serve(&pool).expect("a pass").started, 0, "no reviewer");

    let row = store.review_row("the-review").unwrap().unwrap();
    assert_eq!(row.state, ReviewState::Waiting, "{row:?}");
    assert_eq!(row.attempts, 0, "no attempt was spent on it: {row:?}");
    assert!(row.reviewer.is_none(), "{row:?}");
    assert!(
        row.failure.starts_with(WAITING_FOR_TREE),
        "the row says it is waiting for its tree: {}",
        row.failure
    );
    assert!(
        row.failure.contains("git clone of `agent/the-work`"),
        "and carries the clone's own words: {}",
        row.failure
    );
    assert!(
        capacity::held_until(&row.failure, row.deadline_ms, super::now_ms()),
        "and the queue's claim is held until its not-before time: {row:?}"
    );
    assert!(
        heard.raised("queue_blocked").is_empty(),
        "nothing is raised: this is not a review that could not run — {:?}",
        heard.raised("queue_blocked")
    );
    assert!(
        started.items.lock().unwrap().is_empty(),
        "and no reviewer was asked to start"
    );

    // The next pass, once the branch is back: the same row, the same head, a reviewer.
    sh(&repo, &["branch", "agent/the-work", &head]);
    claim(&store);
    assert_eq!(
        serve(&pool).expect("a pass").started,
        1,
        "a reviewer starts"
    );
    let (prompt, spec) = started.one();
    let tree = Path::new(
        &spec
            .worktree
            .as_ref()
            .expect("a host reviewer is given a tree of its own")
            .path,
    );
    assert_eq!(
        sh(tree, &["rev-parse", "HEAD"]),
        head,
        "in a clone of the head the row names"
    );
    assert!(prompt.contains(&tree.display().to_string()), "{prompt}");
    let row = store.review_row("the-review").unwrap().unwrap();
    assert_eq!(row.state, ReviewState::Reviewing, "{row:?}");
    assert_eq!(row.attempts, 0, "still no attempt spent: {row:?}");
    assert!(row.reviewer.is_some(), "{row:?}");
}

/// A reviewer whose tree went away mid-review does not get to answer `cannot_decide` about
/// the branch A reviewer that has read two files and run two `git` commands, then finds
/// every later call failing at spawn (a workspace with 0 entries), hands in `needs_human` -
#[test]
fn a_verdict_about_a_tree_that_went_away_is_a_wait_and_says_so() {
    use crate::mergequeue::serve::tests::{branch, repo};

    let dir = TempDir::new("review-tree-gone");
    let repo = repo(&dir.0);
    let (_wt, base, head) = branch(&repo, "the-work", "b.txt");
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    store
        .enqueue_review(
            &NewReview {
                id: "the-review".into(),
                repository: repo.display().to_string(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: CHILD.into(),
                branch: "agent/the-work".into(),
                worktree: None,
                base_sha: base,
                head_sha: head.clone(),
                brief: "do the work".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: "enqueued".into(),
            },
            "test",
            1,
        )
        .expect("the review row");
    assert!(
        store
            .move_review(
                "the-review",
                ReviewState::Waiting,
                ReviewState::Reviewing,
                &Move::new(ME, "claimed").set("owner", ME),
                2,
            )
            .unwrap(),
        "the fixture must really have claimed the row"
    );

    let started = Arc::new(Started::default());
    let runner = Recorder {
        started: started.clone(),
    };
    let desk = ReviewDesk::new();
    let heard = Arc::new(Heard::default());
    let deliver = a_daemon(&[], heard.clone());
    let events = |_: SessionEvent| {};
    let settings = |_: &Path| Ok(QueueSettings::default());
    let clones = dir.0.join(CLONES_DIR);
    let pool = Pool {
        store: &store,
        runner: &runner,
        desk: &desk,
        host: HOST,
        me: ME,
        placement: ReviewerPlacement::Host,
        events: &events,
        warn: &|_: &str, _e| {},
        deliver: &deliver,
        settings: &settings,
        // Every session this fixture names counts as live: these two tests are about a
        // verdict and a deadline, not about liveness.
        live: &|_: &str| true,
        clones: clones.clone(),
        reviewer_skills: &[],
        machines: crate::mergequeue::review::hosts::ReviewHosts::new(),
        reviewer_hosts: Some(vec!["local".to_string()]),
        slots: &Vec::new,
    };
    assert_eq!(
        serve(&pool).expect("a pass").started,
        1,
        "a reviewer starts"
    );
    let (_, spec) = started.one();
    let tree = spec
        .worktree
        .as_ref()
        .expect("a host reviewer is given a tree of its own")
        .path
        .clone();
    // The machinery eats the tree - what four reviewers met, from the other side.
    std::fs::remove_dir_all(&tree).expect("the tree is there to remove");
    // And the reviewer, having read a fraction of an artifact, says it cannot decide.
    desk.submit(
        "s-gatekeeper-sub-1",
        Submission {
            decision: Decision::NeedsHuman,
            reasons: vec!["every tool call failed; I could not read the code".into()],
            looked_at: LookedAt {
                files: vec!["a.txt".into()],
                commands: vec!["git diff base...head".into()],
            },
        },
    )
    .expect("the desk takes the submission");

    serve(&pool).expect("a pass");
    let row = store.review_row("the-review").unwrap().unwrap();
    assert_eq!(
        row.state,
        ReviewState::Waiting,
        "the review waits for a tree, and nothing was decided: {row:?}"
    );
    assert!(
        row.verdict.is_none(),
        "no verdict was written about the branch: {row:?}"
    );
    assert_eq!(row.attempts, 0, "and no attempt was spent: {row:?}");
    assert!(
        row.failure.starts_with(WAITING_FOR_TREE),
        "the row says what it is waiting for: {}",
        row.failure
    );
    assert!(
        row.failure.contains("the review environment went away")
            && row.failure.contains(&tree)
            && row.failure.contains(&head),
        "and says the ENVIRONMENT went away, naming the tree and the head: {}",
        row.failure
    );
    assert!(
        heard.raised("queue_blocked").is_empty(),
        "nothing is raised: this is our machinery, not the branch — {:?}",
        heard.raised("queue_blocked")
    );
}
