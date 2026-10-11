//! **A reviewer's refusal reaches a LIVE session — the child that wrote the branch, or the session
//! that enqueued it.**
//!
//! The defect this file exists against, measured three ways on the night of 2026-10-11: the queue
//! put a refusal where the work is — the entry's row is `failed` with the verdict on it, and the
//! pane draws that — the child that did the work may be dead by then, and the session that ran
//! `task_start` was told by a `!` line on the head's screen. A `!` line is not something a model
//! can act on, so an unattended head meant nobody acted at all, and six of that night's entries sat
//! refused while the sessions that enqueued them carried on as if they were fine.
//!
//! `5741101` answered that by ringing the session that enqueued the entry, through
//! `Harness::queue_notices`. `main` then grew [`report_verdict`] (`c05b9c9`), whose refuse door
//! sends the complaints as a MESSAGE — the shape `task_message` already has — to the child that
//! wrote the branch. This file is the two halves read together, and the answer to the question they
//! disagreed about.
//!
//! # Who the refusal must reach
//!
//! **The child that wrote the branch first**, because it is the party that can carry the work on in
//! the worktree and the one the operator's own model of this queue names — *"if it is not happy -
//! it sends a message with complaints to original subagent"*. **And the session that enqueued the
//! entry second**, because a subagent that finished is a session whose turn has ENDED: by the time
//! a reviewer has read its branch, the child is the party most likely to be gone, and the root of
//! its session tree is the model that holds the verbs (`/queue restart|approve|rm`) and the one
//! `5741101` measured as carrying on as if nothing had happened.
//!
//! **One delivery, and the report says which of the two took it** — so *the child has it back* can
//! never be mistaken for *the root was told*, which is a different session with a different thing
//! to do. When NEITHER takes them the complaints are still somewhere, and the sentence names the
//! attempts with each one's own words rather than asserting why they failed.
//!
//! # What it needs
//!
//! A store and the door's two seams, and nothing else: writing queue and session rows and calling
//! [`report_verdict`] with closures for `deliver` and `ask` are pure store work, so no session, no
//! socket and no vocabulary is opened. (The file this replaces needed the GGUF and a real
//! `Harness`, because it asserted through `queue_notices`; the seam it measures now is a function
//! of a store.)

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_harnessd::mergequeue::{Done, Reaches, Reported, complaints_instead, report_verdict};
use letibot_tokencore::store::{MergeEntry, MergePriority, MergeState, SessionRecord, Store};
use letibot_tools::builtins::intent::{AskError, Question, QuestionAnswer};
use letibot_tools::gatekeeper::Decision;

/// A directory that removes itself, so these fixtures never compete for a name under `/tmp` (the
/// shape `standing_notes.rs` and `prefix_fingerprint.rs` roll for themselves).
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "harnessd-refusal-{name}-{}-{}",
            std::process::id(),
            letibot_harnessd::config::now_ns()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("temp dir");
        TempDir(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **The two parties a refusal is between, and a third that has nothing to do with it.**
///
/// The stranger is not decoration: a notice that fires for everybody is a notice nobody reads, and
/// the routing this file asserts is *the entry's own tree* rather than *every session on the
/// store* — which is exactly what `5741101`'s second test was about, one seam over.
const ROOT: &str = "s-root";
const CHILD: &str = "s-root-sub-1";
const STRANGER: &str = "s-stranger";
const ENTRY: &str = "m-1";
const BRANCH: &str = "agent/the-work";
const WORKTREE: &str = "/tmp/the-work";
const REVIEWER: &str = "s-gatekeeper";

/// The reviewer's complaints, in its own words — one of them long enough that a summary could not
/// be mistaken for it.
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

/// **The queue as the daemon leaves it when a branch is enqueued**: a child under the root that ran
/// the `task_start`, an entry whose `session_id` is the CHILD (`entry_for_finished`'s own shape —
/// `HarnessTaskRunner::enqueue_entry` passes the handle twice), and a stranger session that shares
/// the store and has nothing to do with the entry.
///
/// The child's row names its parent on purpose: the root is only reachable by walking, so a fixture
/// written as its own root would pass whether the walk worked or not.
fn the_queue(name: &str, state: MergeState) -> (TempDir, PathBuf) {
    let dir = TempDir::new(name);
    let db = dir.path().join("sessions.db");
    let store = Store::open(&db).expect("the store");
    session(&store, ROOT, None);
    session(&store, CHILD, Some(ROOT));
    session(&store, STRANGER, None);
    store
        .put_merge_entry(&MergeEntry {
            id: ENTRY.into(),
            session_id: CHILD.into(),
            branch: BRANCH.into(),
            base_sha: "base".into(),
            priority: MergePriority::Subagent,
            needs: Vec::new(),
            state,
            brief: "do the work".into(),
            evidence: String::new(),
            created_ms: 1_000,
            updated_ms: 2_000,
            worktree: Some(WORKTREE.into()),
            landed_sha: None,
            gate_steps: Vec::new(),
        })
        .expect("the entry row");
    (dir, db)
}

/// The messages the door sent, as `(session, text)` — the recording `deliver` seam.
type Sent = Arc<Mutex<Vec<(String, String)>>>;

/// **A daemon that answers by NAME**: the sessions in `live` take the message, the rest refuse with
/// the sentence the real door gives (`HarnessTaskRunner::send`'s own words). A stub that answered
/// by a flag instead would let a door that asked the wrong session pass.
fn a_daemon(live: &[&str], into: Sent) -> impl Fn(&str, &str) -> Result<String, String> {
    let live: Vec<String> = live.iter().map(|s| s.to_string()).collect();
    move |to: &str, text: &str| {
        if !live.iter().any(|s| s == to) {
            return Err(format!(
                "no session `{to}` is live in this daemon, so there is nothing to say it to"
            ));
        }
        into.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((to.to_string(), text.to_string()));
        Ok("the message started a turn in that session".into())
    }
}

/// A head that cannot be reached — the `ask` seam for a door that is not about questions.
fn no_head(_: &Question) -> Result<(QuestionAnswer, String), AskError> {
    Err(AskError::NoHead("no head is attached".into()))
}

fn quiet(_: letibot_sessionlog::SessionEvent) {}

fn messages(sent: &Sent) -> Vec<(String, String)> {
    sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// **The child that wrote the branch is told, and nobody else is** — as a message to that live
/// session, carrying the reviewer's reasons WHOLE, the entry, the branch and the worktree.
///
/// The negative half is asserted in the same test on purpose: the false positive and the false
/// negative are the two ways this feature fails, and a test that asserted only one of them would
/// pass on a door that told every session on the store about every refusal.
#[test]
fn the_child_that_wrote_the_branch_is_told_the_reviewers_own_words() {
    let (_dir, db) = the_queue("the-child", MergeState::Waiting);
    let store = Store::open(&db).expect("the store");
    let sent: Sent = Default::default();
    // Every session is live — so what decides the recipient is the ENTRY's tree and not who could
    // have been reached.
    let deliver = a_daemon(&[CHILD, ROOT, STRANGER], sent.clone());

    let done = report_verdict(
        &store,
        ENTRY,
        &Reported {
            decision: Decision::Reject,
            reasons: reasons(),
            files: vec!["crates/x.rs".into()],
            commands: vec!["git diff base...branch".into()],
        },
        REVIEWER,
        5_000,
        &deliver,
        &no_head,
        &quiet,
    )
    .expect("the refusal");

    let Done::Refused {
        child,
        reached,
        said,
    } = &done
    else {
        panic!("a reject is a refusal: {done:?}")
    };
    assert_eq!(child, CHILD, "a refusal names the child whose work it is");
    assert_eq!(
        reached.as_ref().map(|to| (to.who, to.session.as_str())),
        Some((Reaches::Child, CHILD)),
        "the child is live, so the complaints went to the child: {said}"
    );
    assert!(
        said.contains(CHILD) && said.contains(BRANCH) && said.contains("the child that wrote"),
        "and the report says which party took them: {said}"
    );

    let went = messages(&sent);
    assert_eq!(went.len(), 1, "one refusal is one message: {went:?}");
    assert_eq!(went[0].0, CHILD, "to the child, and not to the reviewer");
    let text = &went[0].1;
    for asked in [
        // **The reviewer's OWN words, not a paraphrase of them.** A summary here would be this code
        // writing the reviewer's half for it, and the child would be sent to fix something the
        // reviewer did not say.
        "the retry is not bounded: `loop` can run for ever",
        "the test it added asserts nothing",
        // **The handle every remedy takes, and the branch and tree the work is in.**
        ENTRY,
        BRANCH,
        WORKTREE,
        "`reject`",
    ] {
        assert!(text.contains(asked), "`{asked}` is missing from: {text}");
    }
    // **And the routing is the entry's tree.** The stranger shares the store and was never a
    // recipient; the root is not asked while the child can take them, because the child is the
    // party that has the worktree.
    assert!(
        !went.iter().any(|(to, _)| to == STRANGER),
        "a session that has nothing to do with the entry must not be told: {went:?}"
    );
    assert!(
        !went.iter().any(|(to, _)| to == ROOT),
        "the root is the FALLBACK, not a second audience: {went:?}"
    );

    // **The verdict is on the row**, so the queue's own gate refuses the entry — the door moved the
    // VERDICT, and the queue's own pass is what parks the entry on it.
    let ruled = store.merge_review(ENTRY).unwrap().expect("the verdict");
    assert_eq!(ruled.decision.as_deref(), Some("reject"));
    assert_eq!(ruled.reasons, reasons());
    assert_eq!(ruled.session_id, REVIEWER, "the reviewer is on the row");
}

/// **When the child's session is gone the complaints reach the session that ENQUEUED the entry** —
/// and the report says WHICH of the two took them.
///
/// This is the half `5741101` measured and `main`'s door did not have: the child that finished is
/// the party most likely to be gone, and the model that enqueued the work is the one that holds the
/// verbs. The message the root gets is the ROOT's sentence and not the child's — *your branch is
/// back with you* is false of a session that did not write the branch.
#[test]
fn the_session_that_enqueued_it_is_told_when_the_child_is_gone() {
    let (_dir, db) = the_queue("the-root", MergeState::Waiting);
    let store = Store::open(&db).expect("the store");
    let sent: Sent = Default::default();
    // The child is GONE and the root is live: the ordinary end of a subagent, and the case the
    // fallback exists for.
    let deliver = a_daemon(&[ROOT, STRANGER], sent.clone());

    let done = report_verdict(
        &store,
        ENTRY,
        &Reported {
            decision: Decision::Reject,
            reasons: reasons(),
            files: Vec::new(),
            commands: Vec::new(),
        },
        REVIEWER,
        5_000,
        &deliver,
        &no_head,
        &quiet,
    )
    .expect("the refusal");

    let Done::Refused {
        child,
        reached,
        said,
    } = &done
    else {
        panic!("a reject is a refusal: {done:?}")
    };
    assert_eq!(
        child, CHILD,
        "the child is named whether or not it was reached"
    );
    assert_eq!(
        reached.as_ref().map(|to| (to.who, to.session.as_str())),
        Some((Reaches::Root, ROOT)),
        "the root took them, and the report says so: {said}"
    );
    // **The sentence says WHICH one it reached**, with the door's own reason for the miss quoted:
    // a report that named only the child would be wrong about who has the ball, and one that named
    // only the root would lose which child is out of reach.
    for asked in [
        CHILD,
        ROOT,
        "could not be told",
        "no session `s-root-sub-1` is live in this daemon",
        "the child that wrote the branch",
        "the root session that enqueued it",
    ] {
        assert!(said.contains(asked), "`{asked}` is missing from: {said}");
    }

    let went = messages(&sent);
    assert_eq!(
        went.len(),
        1,
        "one delivery, moved to the second party: {went:?}"
    );
    assert_eq!(went[0].0, ROOT);
    assert!(
        !went.iter().any(|(to, _)| to == STRANGER),
        "and the fallback is still the entry's own tree: {went:?}"
    );
    let text = &went[0].1;
    for asked in [
        // **The root's sentence, not the child's** — it did not write the branch.
        "of a child you started",
        "is yours to move",
        "The child that wrote it could not be told",
        "no session `s-root-sub-1` is live in this daemon",
        // **And the complaints, whole, with the handle and the tree.**
        "the retry is not bounded: `loop` can run for ever",
        "the test it added asserts nothing",
        ENTRY,
        BRANCH,
        WORKTREE,
        "`/queue restart m-1`",
        "`/queue approve m-1`",
        "`/queue rm m-1`",
    ] {
        assert!(text.contains(asked), "`{asked}` is missing from: {text}");
    }
    assert!(
        !text.contains("Your branch"),
        "the root is not told its own branch came back: {text}"
    );

    // **And the refusal stands** — a delivery that moved to the second party does not undo it.
    let ruled = store.merge_review(ENTRY).unwrap().expect("the verdict");
    assert_eq!(ruled.decision.as_deref(), Some("reject"));
    assert_eq!(ruled.reasons, reasons());
}

/// **A refusal NOBODY can take keeps the complaints somewhere, and the sentence is TRUE about
/// why** — it quotes the two attempts rather than asserting that a session is gone.
///
/// The two failures are deliberately different kinds: a session that is not live, and a daemon
/// whose bell has closed. The sentence this replaced said *that session is gone* about every
/// failure there was — which is false for a closed bell, and false for an entry that left the queue
/// between the write and the delivery, which is the other way this path is reached. A fallback that
/// says a false thing about WHY is worse than one that says only what happened.
#[test]
fn a_refusal_nobody_can_take_says_where_the_complaints_are() {
    let (_dir, db) = the_queue("nobody", MergeState::Waiting);
    let store = Store::open(&db).expect("the store");
    let sent: Sent = Default::default();
    let seen = sent.clone();
    // The child is gone, and the root cannot be reached for a reason of its own.
    let deliver = move |to: &str, text: &str| {
        seen.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((to.to_string(), text.to_string()));
        if to == CHILD {
            Err(format!(
                "no session `{to}` is live in this daemon, so there is nothing to say it to"
            ))
        } else {
            Err("the daemon's bell is closed (it is shutting down)".into())
        }
    };

    let done = report_verdict(
        &store,
        ENTRY,
        &Reported {
            decision: Decision::Reject,
            reasons: reasons(),
            files: Vec::new(),
            commands: Vec::new(),
        },
        REVIEWER,
        5_000,
        &deliver,
        &no_head,
        &quiet,
    )
    .expect("the refusal is still a report and not a failure");

    let Done::Refused {
        child,
        reached,
        said,
    } = &done
    else {
        panic!("a reject is a refusal: {done:?}")
    };
    assert_eq!(child, CHILD, "the child is named even though it is gone");
    assert!(reached.is_none(), "nobody took them: {said}");
    for asked in [
        "The complaints did NOT reach a live session",
        // **Both parties, in order, each in its own name** — so a reader can tell which is which
        // rather than being handed two anonymous failures.
        "`s-root-sub-1`, the child that wrote the branch",
        "`s-root`, the root session that enqueued it",
        // **And each attempt's own words, quoted.**
        "no session `s-root-sub-1` is live in this daemon",
        "the daemon's bell is closed (it is shutting down)",
        // **The three places the complaints ARE.**
        "the verdict stands",
        "the review row of `m-1`",
        "on the entry's own evidence",
        "`/queue restart m-1`",
        "`/queue approve m-1`",
        "`/queue rm m-1`",
    ] {
        assert!(said.contains(asked), "`{asked}` is missing from: {said}");
    }
    assert!(
        !said.contains("is gone"),
        "the fallback must quote the attempts rather than claim why they failed: {said}"
    );

    // **And the door's own sentence is the one the daemon publishes**, so the operator's log and
    // the door's report cannot come to two accounts of one failed delivery.
    let entry = store.merge_entry(ENTRY).unwrap().expect("the entry");
    let missed = vec![
        format!(
            "`{CHILD}`, the child that wrote the branch: no session `{CHILD}` is live in this \
             daemon, so there is nothing to say it to"
        ),
        format!(
            "`{ROOT}`, the root session that enqueued it: the daemon's bell is closed (it is \
             shutting down)"
        ),
    ];
    assert_eq!(
        said,
        &complaints_instead(&entry, &missed),
        "the report and the published sentence are one answer"
    );

    // **And the refusal stands** — a message that did not go does not undo a judgement.
    let ruled = store.merge_review(ENTRY).unwrap().expect("the verdict");
    assert_eq!(ruled.decision.as_deref(), Some("reject"));
    assert_eq!(ruled.reasons, reasons());
}

/// **A SECOND refusal of the same entry is a second message, with the SECOND verdict's words.**
///
/// `/queue restart` clears the review row and the queue asks again, so one entry can be refused
/// twice — the second time about a branch somebody has since repaired. The old notice needed a
/// `refused_told` set keyed by the verdict's `answered_ms` to say the second one at all; a message
/// is an EVENT rather than a derived notice, so the door needs no memory — but the property is
/// still the one worth pinning, because a door that keyed on the entry id alone would swallow the
/// second verdict as a repeat of the first, which is the same defect this whole change closes one
/// round later.
///
/// The negative half is asserted first and in the same test: the restart itself says NOTHING,
/// because between the two verdicts the row is `Awaiting` and there is no refusal to report.
#[test]
fn a_second_refusal_of_the_same_entry_is_a_second_message() {
    let (_dir, db) = the_queue("twice", MergeState::Failed);
    let store = Store::open(&db).expect("the store");
    let sent: Sent = Default::default();
    let deliver = a_daemon(&[CHILD, ROOT, STRANGER], sent.clone());

    let first = report_verdict(
        &store,
        ENTRY,
        &Reported {
            decision: Decision::Reject,
            reasons: reasons(),
            files: Vec::new(),
            commands: Vec::new(),
        },
        REVIEWER,
        5_000,
        &deliver,
        &no_head,
        &quiet,
    )
    .expect("the first refusal");
    assert!(matches!(first, Done::Refused { .. }), "{first:?}");
    assert_eq!(messages(&sent).len(), 1, "the first refusal is one message");

    // **The person restarts it**: the review row goes back to *nobody has answered* and the entry
    // goes back to `waiting`. Nothing is refused now, and nothing is said.
    let restarted = store
        .restart_review(ENTRY, "restarted by the operator", 10)
        .expect("the restart");
    assert!(
        restarted,
        "the fixture must really have restarted the entry"
    );
    let cleared = store.merge_review(ENTRY).unwrap().expect("the row");
    assert!(
        cleared.decision.is_none() && cleared.answered_ms.is_none(),
        "a cleared verdict is not a refusal: {cleared:?}"
    );
    assert_eq!(
        messages(&sent).len(),
        1,
        "the restart itself is not a refusal, so nothing was sent"
    );

    // **And it is refused again**, with a new verdict — a later `answered_ms` is what makes it a
    // new one, and is exactly what a real second verdict carries.
    let second = report_verdict(
        &store,
        ENTRY,
        &Reported {
            decision: Decision::Reject,
            reasons: vec!["the fix touched a file the brief did not name".into()],
            files: Vec::new(),
            commands: Vec::new(),
        },
        REVIEWER,
        20_000,
        &deliver,
        &no_head,
        &quiet,
    )
    .expect("the second refusal");
    assert!(matches!(second, Done::Refused { .. }), "{second:?}");

    let went = messages(&sent);
    assert_eq!(
        went.len(),
        2,
        "the second refusal is a new thing to say: {went:?}"
    );
    assert!(
        went[1]
            .1
            .contains("the fix touched a file the brief did not name"),
        "and it is the SECOND verdict's words, not the first's: {}",
        went[1].1
    );
    assert!(
        !went[1].1.contains("the retry is not bounded"),
        "the first verdict's reasons are not carried into the second refusal: {}",
        went[1].1
    );
    let ruled = store.merge_review(ENTRY).unwrap().expect("the row");
    assert_eq!(
        ruled.answered_ms,
        Some(20_000),
        "the row is the second verdict"
    );
}

/// **The child is the entry's `session_id`, and the root is found by WALKING** — the rule the
/// whole file rests on, asserted on its own so a fixture that made the two the same cannot hide a
/// broken walk.
#[test]
fn the_two_parties_are_the_entrys_own_tree() {
    let (_dir, db) = the_queue("parties", MergeState::Waiting);
    let store = Store::open(&db).expect("the store");
    let entry = store.merge_entry(ENTRY).unwrap().expect("the entry");

    let to = letibot_harnessd::mergequeue::refusal_addressees(&store, &entry);
    let named: Vec<(Reaches, &str)> = to.iter().map(|a| (a.who, a.session.as_str())).collect();
    assert_eq!(
        named,
        vec![(Reaches::Child, CHILD), (Reaches::Root, ROOT)],
        "the child first, then the root one walk up"
    );
    assert!(
        !named.iter().any(|(_, s)| *s == STRANGER),
        "a session that is not in the entry's tree is not a party"
    );

    // **And an entry whose child has no row leaves the root unnamed** rather than guessing one: the
    // store holds the tree, and a row that is gone is a tree nobody can walk. What that costs is
    // said where it lands — the fallback names the parties that were actually tried.
    let mut orphan = entry.clone();
    orphan.session_id = "s-nobody-at-all".into();
    store.put_merge_entry(&orphan).expect("the orphan row");
    let to = letibot_harnessd::mergequeue::refusal_addressees(&store, &orphan);
    assert_eq!(
        to.iter().map(|a| a.session.as_str()).collect::<Vec<_>>(),
        vec!["s-nobody-at-all"],
        "one party, named as it is, rather than a second one invented"
    );
}
