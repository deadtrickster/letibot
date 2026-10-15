//! **A reviewer firecode had no free slot for WAITS; a review that stops says why.**
//!
//! The hub row these tests are written against, on pg-noop's session, 2026-10-12 ~00:56:
//!
//! > `queue_blocked — the review of agent/panic-groupingsets-join
//! > (s-1789919514688401228-sub-1791743034603570463) is BLOCKED after 3 attempts without a
//! > verdict: the reviewer s-gatekeeper-sub-1791759365511364772 failed: placing this session in a
//! > firecode VM: io: firecode up refused (exit status: 1): [firecode] starting a VM and leaving it
//! > up (log: /home/dead/Projects/firecode/runs/up.6DSWx1.log)`
//!
//! The operator: *"investigate"*, then *"fix them now"*. Every test drives the real queue thread's
//! claims ([`RepoQueue::pass`]) and the real reviewers' host ([`serve`]) against a temp repository
//! and a temp store, with a placement that refuses with firecode's own capacity text.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::{
    LeaseOutcome, MergePriority, Move, NewReview, ReviewKind, ReviewState, Store,
};
use letibot_tools::builtins::task::{TaskRunner, TaskSpec, TaskStatus};
use letibot_tools::builtins::verdict::ReviewDesk;
use letibot_tools::gatekeeper::{Decision, LookedAt, Submission};

use super::*;
use crate::mergequeue::capacity::{self, WAITING_FOR_SLOT};
use crate::mergequeue::gate::{self, GreenGate};
use crate::mergequeue::lease::LEASE_TTL_MS;
use crate::mergequeue::serve::tests::{Door, TempDir, branch, commit, env, repo};
use crate::mergequeue::serve::{MAX_REVIEW_ATTEMPTS, Pass, RepoQueue};
use crate::mergequeue::settings::QueueSettings;
use crate::sessions::gatekeeper::ReviewerPlacement;

const HOST: &str = "s-gatekeeper";
const ID: &str = "pg";

/// The capacity refusal of 2026-10-12, as a reviewer's failure carries it.
fn no_slot() -> String {
    capacity::tests::FIRECODE_NO_SLOT.to_string()
}

/// A placement failure that is NOT capacity: firecode is not there.
const NO_FIRECODE: &str =
    "placing this session in a firecode VM: io: running firecode: No such file or directory";

/// **What the next reviewer started will do** — refused for want of a slot, refused for a real
/// reason, or run (and then hand in whatever the test puts at the desk).
#[derive(Clone)]
enum Next {
    NoSlot,
    Broken,
    Runs,
}

/// **A placement with a script**: each `start` takes the next outcome; `collect` answers it.
struct Placer {
    script: Mutex<VecDeque<Next>>,
    started: Mutex<Vec<(String, Next)>>,
}

impl Placer {
    fn new(script: &[Next]) -> Self {
        Placer {
            script: Mutex::new(script.iter().cloned().collect()),
            started: Mutex::new(Vec::new()),
        }
    }
    fn last(&self) -> String {
        self.started.lock().unwrap().last().unwrap().0.clone()
    }
}

impl TaskRunner for Placer {
    fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
        let next = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Next::Runs);
        let mut started = self.started.lock().unwrap();
        let handle = format!("{HOST}-sub-{}", started.len() + 1);
        started.push((handle.clone(), next));
        Ok(handle)
    }
    fn collect(&self, handle: &str, _timeout: std::time::Duration) -> TaskStatus {
        let started = self.started.lock().unwrap();
        match started.iter().find(|(h, _)| h == handle).map(|(_, n)| n) {
            Some(Next::NoSlot) => TaskStatus::Failed {
                why: format!("the session could not be set up: {}", no_slot()),
            },
            Some(Next::Broken) => TaskStatus::Failed {
                why: NO_FIRECODE.to_string(),
            },
            Some(Next::Runs) => TaskStatus::Running {
                note: None,
                child: None,
            },
            None => TaskStatus::Unknown,
        }
    }
}

/// The queue, the reviewers' host and every warning either raised.
struct World {
    tag: String,
    _dir: TempDir,
    repo: PathBuf,
    wt: PathBuf,
    db: PathBuf,
    door: Arc<Door>,
    queue: RepoQueue,
    desk: ReviewDesk,
    placer: Placer,
    raised: Arc<Mutex<Vec<(String, String)>>>,
    /// What the pool's `slots` answers — the holders a long wait names.
    holders: Vec<String>,
}

impl World {
    fn new(tag: &str, script: &[Next]) -> Self {
        let dir = TempDir::new(&format!("cap-{tag}"));
        let repo = repo(&dir.0);
        let (wt, base, head) = branch(&repo, ID, "pg.txt");
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        store
            .enqueue_review(
                &NewReview {
                    id: ID.into(),
                    repository: repo.display().to_string(),
                    target: "main".into(),
                    kind: ReviewKind::Branch,
                    session_id: format!("s-{ID}"),
                    branch: format!("agent/{ID}"),
                    worktree: Some(wt.display().to_string()),
                    base_sha: base,
                    head_sha: head,
                    brief: "fix the two panics".into(),
                    priority: MergePriority::Subagent,
                    fixes: None,
                    evidence: "enqueued".into(),
                },
                "test",
                gate::now_ms(),
            )
            .unwrap();
        let door = Arc::new(Door::default());
        let e = env(Arc::new(GreenGate), door.clone(), tag);
        assert!(matches!(
            store
                .acquire_queue_lease(
                    &repo.display().to_string(),
                    &e.me,
                    gate::now_ms(),
                    LEASE_TTL_MS
                )
                .unwrap(),
            LeaseOutcome::Held { .. }
        ));
        let queue = RepoQueue::new(store, repo.clone(), e);
        World {
            tag: tag.to_string(),
            _dir: dir,
            repo,
            wt,
            db,
            door,
            queue,
            desk: ReviewDesk::new(),
            placer: Placer::new(script),
            raised: Arc::new(Mutex::new(Vec::new())),
            holders: Vec::new(),
        }
    }
    /// This daemon's lease identity — the owner `env` names (`serve::tests::me`).
    fn me(&self) -> String {
        format!("test/{}", self.tag)
    }
    fn store(&self) -> Store {
        Store::open(&self.db).unwrap()
    }
    fn row(&self) -> letibot_tokencore::store::ReviewRow {
        self.store().review_row(ID).unwrap().unwrap()
    }
    /// One pass of the queue's own thread: deadlines, claims.
    fn queue_pass(&self) {
        let stop = AtomicBool::new(false);
        assert_ne!(self.queue.settle(&stop).unwrap(), Pass::LostLease);
    }
    /// One pass of the reviewers' host.
    fn serve(&self) {
        let store = self.store();
        let raised = self.raised.clone();
        let events = move |e: SessionEvent| {
            if let SessionEvent::Warning { code, detail, .. } = e {
                raised.lock().unwrap().push((code, detail));
            }
        };
        let deliver = |_: &str, _: &str| -> Result<String, String> { Ok("told".into()) };
        let settings = |_: &Path| Ok(QueueSettings::default());
        let me = self.me();
        let pool = Pool {
            store: &store,
            runner: &self.placer,
            desk: &self.desk,
            host: HOST,
            me: &me,
            placement: ReviewerPlacement::Host,
            events: &events,
            warn: &|_: &str, e| events(e),
            deliver: &deliver,
            settings: &settings,
            clones: std::env::temp_dir(),
            slots: &|| self.holders.clone(),
        };
        serve(&pool).unwrap();
    }
    /// Claim, start a reviewer, and collect what it did: one reviewer's whole life but its verdict.
    fn one_reviewer(&self) {
        self.queue_pass();
        assert_eq!(self.row().state, ReviewState::Reviewing, "{:?}", self.row());
        self.serve(); // started
        self.serve(); // collected
    }
    /// **Time passes** for a capacity wait: its not-before time is moved into the past, as a
    /// note on the waiting row (which ends no wait).
    fn time_passes(&self) {
        let r = self.row();
        assert_eq!(r.state, ReviewState::Waiting);
        assert!(
            self.store()
                .move_review(
                    ID,
                    ReviewState::Waiting,
                    ReviewState::Waiting,
                    &Move::new("test", "time passes").set("deadline_ms", 1u64),
                    gate::now_ms(),
                )
                .unwrap()
        );
    }
    fn hand_in(&self, decision: Decision, reasons: &[&str]) {
        let child = self.placer.last();
        self.desk
            .submit(
                &child,
                Submission {
                    decision,
                    reasons: reasons.iter().map(|s| s.to_string()).collect(),
                    looked_at: LookedAt {
                        files: vec![],
                        commands: vec![],
                    },
                },
            )
            .expect("the verdict is taken at the desk");
        self.serve();
    }
    fn raised(&self, code: &str) -> Vec<String> {
        let mut all: Vec<String> = self
            .raised
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _)| c == code)
            .map(|(_, d)| d.clone())
            .collect();
        all.extend(self.door.warnings(code));
        all
    }
    fn move_branch(&self, n: usize) {
        commit(
            &self.wt,
            "pg.txt",
            &format!("answer {n}\n"),
            &format!("answer {n}"),
        );
        let _ = &self.repo;
    }
}

/// **Capacity is a wait, not an attempt**: the row goes back to `waiting` (never `blocked`) with
/// no attempt counted, is NOT claimed again before its time, says so ONCE in the routine register
/// however many times it is refused, and is reviewed when a slot frees.
#[test]
fn a_reviewer_with_no_free_vm_slot_waits_uncounted_and_is_reviewed_when_a_slot_frees() {
    let w = World::new(
        "waits",
        &[Next::NoSlot, Next::NoSlot, Next::NoSlot, Next::Runs],
    );
    w.one_reviewer();
    let r = w.row();
    assert_eq!(r.state, ReviewState::Waiting, "{r:?}");
    assert_eq!(r.attempts, 0, "no attempt is counted");
    assert!(r.failure.starts_with(WAITING_FOR_SLOT), "{}", r.failure);
    assert!(
        r.failure.contains("4 of 4 VM slots are busy"),
        "{}",
        r.failure
    );
    let held_for = r.deadline_ms.unwrap().saturating_sub(gate::now_ms());
    assert!(
        (50_000..=60_000).contains(&held_for),
        "the first wait is a minute: {held_for}"
    );

    // Not claimed before its time — firecode is not asked every pass.
    w.queue_pass();
    w.queue_pass();
    assert_eq!(w.row().state, ReviewState::Waiting);
    assert_eq!(w.placer.started.lock().unwrap().len(), 1);

    // Refused twice more, a slot frees, and the third reviewer runs and judges.
    for _ in 0..2 {
        w.time_passes();
        w.one_reviewer();
        assert_eq!(w.row().state, ReviewState::Waiting, "{:?}", w.row());
    }
    let held_for = w.row().deadline_ms.unwrap().saturating_sub(gate::now_ms());
    assert!(
        held_for > 4 * 60_000,
        "the third wait is five minutes: {held_for}"
    );
    w.time_passes();
    w.one_reviewer();
    assert_eq!(w.row().state, ReviewState::Reviewing);
    w.hand_in(Decision::Accept, &["the patch is right"]);
    let r = w.row();
    assert_eq!(r.state, ReviewState::Approved, "{r:?}");
    assert_eq!(r.attempts, 0);

    let waiting = w.raised("queue_waiting_for_vm");
    assert_eq!(
        waiting.len(),
        1,
        "ONE routine line, not one per retry: {waiting:?}"
    );
    assert!(
        waiting[0].contains("waiting for a free VM slot: 4 of 4 VM slots are busy"),
        "{}",
        waiting[0]
    );
    assert!(
        waiting[0].contains("no attempt is counted"),
        "{}",
        waiting[0]
    );
    assert!(
        w.raised("queue_blocked").is_empty(),
        "a shortage is not a block"
    );
    assert!(
        w.raised("queue_vm_slots_exhausted").is_empty(),
        "nor a long wait"
    );
}

/// **A long wait raises once**, with firecode's own lines for raising the ceiling.
#[test]
fn a_wait_past_thirty_minutes_raises_once_with_firecodes_sudo_lines() {
    let w = World::new("long", &[Next::NoSlot, Next::NoSlot, Next::NoSlot]);
    // The wait began 31 minutes ago: a claim and a capacity refusal written then.
    let then = gate::now_ms() - 31 * 60_000;
    let s = w.store();
    let me = w.me();
    assert!(
        s.move_review(
            ID,
            ReviewState::Waiting,
            ReviewState::Reviewing,
            &Move::new(&me, "claimed").set("owner", me.clone()),
            then,
        )
        .unwrap()
    );
    assert!(
        s.move_review(
            ID,
            ReviewState::Reviewing,
            ReviewState::Waiting,
            &Move::new(&me, format!("{WAITING_FOR_SLOT} for `agent/pg`: …"))
                .owned_by(me.clone())
                .set("owner", "")
                .set("deadline_ms", 1u64)
                .set("failure", format!("{WAITING_FOR_SLOT} for `agent/pg`: …")),
            then + 1,
        )
        .unwrap()
    );
    w.one_reviewer();
    assert_eq!(w.row().state, ReviewState::Waiting);
    w.time_passes();
    w.one_reviewer();
    assert_eq!(w.row().state, ReviewState::Waiting);
    assert_eq!(w.row().attempts, 0);

    let long = w.raised("queue_vm_slots_exhausted");
    assert_eq!(long.len(), 1, "raised once: {long:?}");
    for said in [
        "VM SLOTS EXHAUSTED",
        "has waited 31 min",
        "4 of 4 VM slots are busy",
        "`sudo ip tuntap add dev fccode5 mode tap user $(id -u)`",
        "`sudo ip addr add 172.16.5.1/30 dev fccode5 && sudo ip link set fccode5 up`",
    ] {
        assert!(long[0].contains(said), "`{said}` missing from: {}", long[0]);
    }
    assert!(
        w.raised("queue_waiting_for_vm").is_empty(),
        "the wait was already announced"
    );
    assert!(w.raised("queue_blocked").is_empty());
}

/// **Another row of the same branch, already in a capacity wait** — enqueued beside `pg`, its
/// wait begun `ago_min` minutes ago (and raised as long when `raised`), and held back so the queue
/// does not claim it. What a red branch looks like: one row per red tip, all for one branch.
fn a_sibling_waiting(w: &World, ago_min: u64, raised: bool) {
    let s = w.store();
    let first = w.row();
    s.enqueue_review(
        &NewReview {
            id: "pg-red-2".into(),
            repository: first.repository.clone(),
            target: "main".into(),
            kind: ReviewKind::Branch,
            session_id: first.session_id.clone(),
            branch: first.branch.clone(),
            worktree: first.worktree.clone(),
            base_sha: first.base_sha.clone(),
            head_sha: first.head_sha.clone(),
            brief: "the red tip".into(),
            priority: MergePriority::Subagent,
            fixes: None,
            evidence: "enqueued".into(),
        },
        "test",
        gate::now_ms(),
    )
    .unwrap();
    let then = gate::now_ms() - ago_min * 60_000;
    let me = w.me();
    assert!(
        s.move_review(
            "pg-red-2",
            ReviewState::Waiting,
            ReviewState::Reviewing,
            &Move::new(&me, "claimed").set("owner", me.clone()),
            then,
        )
        .unwrap()
    );
    let why = format!(
        "{WAITING_FOR_SLOT} for `agent/pg`: …{}",
        if raised {
            format!("; {}", capacity::SLOTS_EXHAUSTED)
        } else {
            String::new()
        }
    );
    assert!(
        s.move_review(
            "pg-red-2",
            ReviewState::Reviewing,
            ReviewState::Waiting,
            &Move::new(&me, why.clone())
                .owned_by(me.clone())
                .set("owner", "")
                .set("deadline_ms", gate::now_ms() + 3_600_000)
                .set("failure", why),
            then + 1,
        )
        .unwrap()
    );
}

/// **A wait is the branch's, said once while any of its rows is in it** — pg-noop's
/// `feat/memory-engine`, 2026-10-15: one row per red tip, and each new row drew its own "waiting
/// for a VM slot" (16:10, 16:16, 16:34, 16:48, …) and its own "VM slots exhausted" half an hour
/// later, for one wait that never ended. A new row joining a wait its branch is already in says
/// nothing new; when the branch's wait passes half an hour it is raised once — and the raise
/// names what holds each slot.
#[test]
fn a_new_row_of_a_branch_already_waiting_does_not_say_it_again() {
    let mut w = World::new("branch", &[Next::NoSlot]);
    w.holders = vec![
        "fccode1: firecracker pid 861706, run firecode-20261014-191759-812644 started \
         2026-10-14 19:17:59; its owner `s-1-sub-2` is recorded `running` under daemon \
         `d-404553-1-0`, which is gone — being reaped"
            .into(),
    ];
    a_sibling_waiting(&w, 31, false);
    w.one_reviewer();
    assert_eq!(w.row().state, ReviewState::Waiting, "{:?}", w.row());
    assert!(
        w.raised("queue_waiting_for_vm").is_empty(),
        "the branch was already waiting: {:?}",
        w.raised("queue_waiting_for_vm")
    );
    let long = w.raised("queue_vm_slots_exhausted");
    assert_eq!(long.len(), 1, "the branch's wait is 31 min old: {long:?}");
    for said in [
        "has waited 31 min",
        "What holds the slots: (1) fccode1: firecracker pid 861706",
        "under daemon `d-404553-1-0`, which is gone — being reaped",
    ] {
        assert!(long[0].contains(said), "`{said}` missing from: {}", long[0]);
    }
}

/// **…and a branch whose wait was already raised is not raised again by its next row.**
#[test]
fn a_branch_whose_wait_was_raised_is_not_raised_again_by_a_new_row() {
    let w = World::new("raised", &[Next::NoSlot]);
    a_sibling_waiting(&w, 45, true);
    w.one_reviewer();
    assert_eq!(w.row().state, ReviewState::Waiting, "{:?}", w.row());
    assert!(w.raised("queue_waiting_for_vm").is_empty());
    assert!(
        w.raised("queue_vm_slots_exhausted").is_empty(),
        "{:?}",
        w.raised("queue_vm_slots_exhausted")
    );
}

/// **A real placement failure still counts** — and a capacity wait between them does not; the
/// block names every counted attempt by firecode's or the error's own words, and says they were
/// placement, not a judgement.
#[test]
fn a_real_placement_failure_still_counts_and_the_block_names_each_attempt() {
    let w = World::new(
        "real",
        &[Next::Broken, Next::NoSlot, Next::Broken, Next::Broken],
    );
    w.one_reviewer();
    assert_eq!(w.row().attempts, 1);
    assert_eq!(w.row().state, ReviewState::Waiting);
    w.one_reviewer(); // no slot: waits, uncounted
    assert_eq!(w.row().attempts, 1);
    w.time_passes();
    w.one_reviewer();
    assert_eq!(w.row().attempts, 2);
    w.one_reviewer();
    let r = w.row();
    assert_eq!(r.state, ReviewState::Blocked, "{r:?}");
    assert_eq!(r.attempts, MAX_REVIEW_ATTEMPTS);

    let blocked = w.raised("queue_blocked");
    assert_eq!(blocked.len(), 1, "{blocked:?}");
    let b = &blocked[0];
    for said in [
        "is BLOCKED: 3 attempts to review it at",
        "ended without a verdict — no reviewer judged the branch",
        "(1) the reviewer `s-gatekeeper-sub-1` failed: placing this session in a firecode VM: io: running firecode: No such file or directory",
        "(3) the reviewer `s-gatekeeper-sub-4` failed",
        "could not be PLACED",
        "`/queue restart pg`",
        "`/queue approve pg`",
    ] {
        assert!(b.contains(said), "`{said}` missing from: {b}");
    }
    assert!(
        !b.contains("sub-2"),
        "the capacity wait is not an attempt: {b}"
    );
    assert!(
        r.failure.contains("3 attempts"),
        "the row says it too: {}",
        r.failure
    );
}

/// **Three `could not decide` at three heads**: the stop says THAT — how many, at which heads,
/// the reviewer's decisive `BUT …` sentence, and the operator's verbs — and not an earlier
/// placement failure, which is not why it stopped.
#[test]
fn three_could_not_decide_verdicts_are_named_with_their_heads_and_the_decisive_reason() {
    let w = World::new("undecided", &[Next::Broken]);
    w.one_reviewer(); // a real placement failure on the first head — history, not the cause
    assert_eq!(w.row().attempts, 1);
    let mut heads = Vec::new();
    for n in 1..=3 {
        w.one_reviewer();
        assert_eq!(w.row().state, ReviewState::Reviewing);
        heads.push(w.row().head_sha.clone());
        w.hand_in(
            Decision::NeedsHuman,
            &[
                "The two root causes are the right ones and both patches are correct in \
                 themselves.",
                "BUT the ask's acceptance cannot be met on this base, and the branch's own \
                 measurements say it was not met. On main the memory engine cannot hold rows.",
            ],
        );
        assert_eq!(w.row().state, ReviewState::CannotDecide);
        assert_eq!(
            w.row().attempts,
            if n == 1 { 1 } else { 0 },
            "verdicts are not attempts"
        );
        if n < 3 {
            w.move_branch(n);
            w.queue_pass(); // the moved branch goes back to review (and is claimed again)
            assert_ne!(w.row().state, ReviewState::CannotDecide, "{:?}", w.row());
        }
    }
    let said = w.raised("queue_needs_human");
    assert_eq!(said.len(), 3);
    let last = &said[2];
    let short: Vec<String> = heads.iter().map(|h| capacity::short(h)).collect();
    for want in [
        "the review of `agent/pg` (`pg`) stops for a person: the reviewer could not decide at".to_string(),
        "\"BUT the ask's acceptance cannot be met on this base, and the branch's own measurements say it was not met.\"".to_string(),
        format!("3 × could not decide on this branch (at {})", short.join(", ")),
        "`/queue approve pg` approves it over the reviewer".to_string(),
        "`/queue veto pg WHY`".to_string(),
        "`/queue rm pg` withdraws it".to_string(),
    ] {
        assert!(last.contains(&want), "`{want}` missing from: {last}");
    }
    assert!(
        !last.contains("firecode") && !last.contains("placing"),
        "{last}"
    );
    assert!(
        !said[0].contains("×"),
        "the first one is not counted as a series: {}",
        said[0]
    );
    assert!(w.raised("queue_blocked").is_empty());
}
