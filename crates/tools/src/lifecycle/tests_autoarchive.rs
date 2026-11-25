//! R276: each auto-archive rule writes its reason, the guards keep a held session in the list,
//! and the backfill's dry run lists exactly what the rules pick.

use std::path::PathBuf;

use letibot_tokencore::store::{
    MergePriority, NewReview, OwnerKind, OwnerRow, OwnerState, ProcState, ProcessRecord,
    ReviewKind, SessionRecord, Store, SubagentRun,
};

use super::autoarchive::{self, Decision, Here, Settled};

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        let p = std::env::temp_dir().join(format!(
            "letibot-autoarchive-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn here() -> Here {
    Here {
        host: "this-box".into(),
        pid: 4_000_000_001,
        proc_root: PathBuf::from("/nonexistent-proc"),
    }
}

fn session(store: &Store, id: &str, parent: Option<&str>, workspace: &str) {
    store
        .put_session(&SessionRecord {
            id: id.into(),
            title: None,
            model_id: "qwen".into(),
            dialect_sha: "d".into(),
            workspace_root: workspace.into(),
            owner: "tester".into(),
            role: None,
            approvers: vec![],
            parent_session_id: parent.map(str::to_string),
        })
        .expect("a session row");
}

fn review_row(store: &Store, id: &str) {
    store
        .enqueue_review(
            &NewReview {
                id: id.into(),
                repository: "/repo".into(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: "s-author".into(),
                branch: format!("agent/{id}"),
                worktree: None,
                base_sha: "b".into(),
                head_sha: "h".into(),
                brief: "the ask".into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: String::new(),
            },
            "test",
            1,
        )
        .expect("the review row");
}

fn moved(store: &Store, row: &str, to: &str, by: &str, why: &str, at: u64) {
    store
        .record_transition("review", row, "/repo", Some("reviewing"), to, by, why, at)
        .expect("a transition");
}

const GK: &str = "s-gatekeeper-abc";

fn reviewer(store: &Store, id: &str, row: &str) {
    session(
        store,
        id,
        Some(GK),
        &format!("/tmp/letibot-review-clones/review-{row}-123-0"),
    );
}

fn package(store: &Store, id: &str, landed: Option<&str>) {
    session(store, id, None, "/repo");
    store.set_session_origin(id, "package").expect("the origin");
    let mut o = OwnerRow::live(id, OwnerKind::Package, Some("/repo".into()), 1);
    o.state = OwnerState::Settled;
    o.landed_sha = landed.map(str::to_string);
    o.settled_ms = Some(2);
    store.put_owner(&o).expect("the owner");
}

fn child_collected(store: &Store, id: &str) {
    session(store, id, Some("s-parent"), "/repo");
    store
        .put_subagent_run(&SubagentRun {
            child_id: id.into(),
            parent_id: "s-parent".into(),
            state: "done".into(),
            prompt: "do it".into(),
            spec_json: "{}".into(),
            instance: "i".into(),
            resumes: 0,
            outcome: Some("done it".into()),
            later_answer: None,
            delivered: false,
            started_ms: 1,
            updated_ms: 1,
        })
        .expect("the run");
    store.deliver_subagent_run(id).expect("delivered");
}

const LATER: i64 = i64::MAX / 2;

fn run(store: &Store, live: autoarchive::LiveFn<'_>, act: bool) -> Vec<Decision> {
    let write = autoarchive::write_to(store);
    autoarchive::sweep(
        store,
        &here(),
        live,
        0,
        LATER,
        act.then_some(&write as autoarchive::ActFn<'_>),
    )
}

fn reason_of(store: &Store, id: &str) -> Option<String> {
    let s = store.session(id).unwrap().expect("the row stays");
    s.archived_ms.and(s.archived_reason)
}

/// Every session kind the rules name, in one store.
fn every_rule(store: &Store) {
    reviewer(store, "s-gatekeeper-abc-sub-1", "row-a");
    review_row(store, "row-a");
    moved(
        store,
        "row-a",
        "approved",
        "reviewer:s-gatekeeper-abc-sub-1",
        "ok",
        u64::MAX / 4,
    );

    reviewer(store, "s-gatekeeper-abc-sub-2", "row-b");
    reviewer(store, "s-gatekeeper-abc-sub-3", "row-b");
    review_row(store, "row-b");
    moved(
        store,
        "row-b",
        "reviewing",
        "q",
        "reviewer s-gatekeeper-abc-sub-2 started on h",
        u64::MAX / 4,
    );
    moved(
        store,
        "row-b",
        "reviewing",
        "q",
        "reviewer s-gatekeeper-abc-sub-3 started on h",
        u64::MAX / 4 + 1,
    );

    session(store, "s-oneshot", None, "/repo");
    store.set_session_origin("s-oneshot", "one_shot").unwrap();
    child_collected(store, "s-child");
    package(store, "s-landed", Some("0123456789abcdef"));
    package(store, "s-dropped", None);
    // The operator's own, and an operator child that has not answered: no rule picks either.
    session(store, "s-mine", None, "/repo");
    session(store, "s-working", Some("s-mine"), "/repo");
}

#[test]
fn each_rule_archives_with_its_reason() {
    let d = Dir::new("rules");
    let store = Store::open_set(&d.0, "aaaaaaaaaaaa").expect("a set");
    every_rule(&store);

    let out = run(&store, &autoarchive::no_daemon, true);
    let want = [
        ("s-gatekeeper-abc-sub-1", "verdict delivered"),
        (
            "s-gatekeeper-abc-sub-2",
            "superseded by s-gatekeeper-abc-sub-3",
        ),
        // The newest reviewer of a row that is waiting again, with no verdict from it.
        ("s-gatekeeper-abc-sub-3", "review abandoned"),
        ("s-oneshot", "one-shot ended"),
        ("s-child", "result collected"),
        ("s-landed", "landed 01234567"),
        ("s-dropped", "abandoned"),
    ];
    for (id, reason) in want {
        assert_eq!(
            reason_of(&store, id).as_deref(),
            Some(reason),
            "{id}: {out:?}"
        );
    }
    for id in ["s-mine", "s-working"] {
        assert_eq!(reason_of(&store, id), None, "{id} was archived: {out:?}");
    }
    assert_eq!(out.len(), want.len(), "{out:?}");
    assert!(
        store
            .list_sessions()
            .unwrap()
            .iter()
            .all(|s| !want.iter().any(|(id, _)| *id == s.id)),
        "the default listing still shows an archived one"
    );
}

#[test]
fn a_head_a_turn_a_job_or_a_writer_keeps_it_in_the_list() {
    let d = Dir::new("guards");
    let mine = Store::open_set(&d.0, "aaaaaaaaaaaa").expect("the daemon's set");
    let other = Store::open_set(&d.0, "bbbbbbbbbbbb").expect("another project");
    child_collected(&mine, "s-headed");
    child_collected(&mine, "s-turning");
    child_collected(&mine, "s-jobbed");
    child_collected(&mine, "s-written");
    child_collected(&mine, "s-free");
    // The job is in ANOTHER project file: the guard reads every one.
    other
        .put_process(&ProcessRecord {
            state: ProcState::Running,
            ..ProcessRecord::starting("s-jobbed", "j1", None, 4_000_000_000)
        })
        .expect("a running job");
    // Another live process holds this one open: pid 1 is always there.
    mine.claim_session_writer("s-written", "this-box/1/0", 1, "this-box", 1, 0)
        .expect("a writer");
    let live = |id: &str| match id {
        "s-headed" => Some(Some("a head is attached".to_string())),
        "s-turning" => Some(Some("a turn is in flight".to_string())),
        _ => None,
    };
    let here = Here {
        proc_root: PathBuf::from("/proc"),
        ..here()
    };
    let write = autoarchive::write_to(&mine);
    let out = autoarchive::sweep(&mine, &here, &live, 0, LATER, Some(&write));
    let by = |id: &str| out.iter().find(|d| d.id == id).expect(id).clone();
    assert_eq!(
        by("s-headed").refused.as_deref(),
        Some("a head is attached")
    );
    assert_eq!(
        by("s-turning").refused.as_deref(),
        Some("a turn is in flight")
    );
    assert_eq!(by("s-jobbed").refused.as_deref(), Some("a job is open"));
    assert_eq!(
        by("s-written").refused.as_deref(),
        Some("held open by pid 1")
    );
    for id in ["s-headed", "s-turning", "s-jobbed", "s-written"] {
        assert_eq!(reason_of(&mine, id), None, "{id} was archived while held");
    }
    assert_eq!(
        reason_of(&mine, "s-free").as_deref(),
        Some("result collected")
    );
}

#[test]
fn the_operators_own_session_is_never_archived_at_settle() {
    let d = Dir::new("operator");
    let store = Store::open_set(&d.0, "aaaaaaaaaaaa").expect("a set");
    session(&store, "s-me", None, "/repo");
    let mut o = OwnerRow::live("s-me", OwnerKind::Session, Some("/repo".into()), 1);
    o.state = OwnerState::Settled;
    store.put_owner(&o).unwrap();
    let write = autoarchive::write_to(&store);
    let d = autoarchive::settle_one(
        &store,
        &here(),
        &autoarchive::no_daemon,
        "s-me",
        LATER,
        Some(&write),
    );
    assert!(d.as_ref().is_none_or(|d| !d.archived), "{d:?}");
    assert_eq!(reason_of(&store, "s-me"), None);
}

#[test]
fn an_archived_session_keeps_its_row_and_comes_back() {
    let d = Dir::new("resume");
    let store = Store::open_set(&d.0, "aaaaaaaaaaaa").expect("a set");
    child_collected(&store, "s-child");
    run(&store, &autoarchive::no_daemon, true);
    assert_eq!(
        reason_of(&store, "s-child").as_deref(),
        Some("result collected")
    );
    assert!(
        store
            .list_sessions_archived(letibot_tokencore::store::Archived::Yes)
            .unwrap()
            .iter()
            .any(|s| s.id == "s-child"),
        "it is listed under archived"
    );
    store.set_archived("s-child", false, None).expect("back");
    assert!(
        store
            .list_sessions()
            .unwrap()
            .iter()
            .any(|s| s.id == "s-child")
    );
}

#[test]
fn the_dry_run_lists_what_the_rules_pick_and_writes_nothing() {
    let d = Dir::new("dry");
    let store = Store::open_set(&d.0, "aaaaaaaaaaaa").expect("a set");
    every_rule(&store);
    let out = run(&store, &autoarchive::no_daemon, false);
    let mut ids: Vec<&str> = out.iter().map(|d| d.id.as_str()).collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "s-child",
            "s-dropped",
            "s-gatekeeper-abc-sub-1",
            "s-gatekeeper-abc-sub-2",
            "s-gatekeeper-abc-sub-3",
            "s-landed",
            "s-oneshot"
        ]
    );
    assert!(out.iter().all(|d| !d.archived && d.refused.is_none()));
    assert!(out[0].line().contains("would archive"), "{}", out[0].line());
    assert!(
        store
            .list_sessions_archived(letibot_tokencore::store::Archived::Yes)
            .unwrap()
            .is_empty(),
        "a dry run wrote an archive"
    );
}

#[test]
fn a_reviewer_whose_row_moved_on_is_abandoned_only_after_the_grace() {
    let d = Dir::new("orphan");
    let store = Store::open_set(&d.0, "aaaaaaaaaaaa").expect("a set");
    reviewer(&store, "s-gatekeeper-abc-sub-9", "row-c");
    review_row(&store, "row-c");
    let created = store
        .session("s-gatekeeper-abc-sub-9")
        .unwrap()
        .unwrap()
        .created_ms;
    let early = autoarchive::sweep(
        &store,
        &here(),
        &autoarchive::no_daemon,
        0,
        created + 1000,
        None,
    );
    assert!(early.is_empty(), "{early:?}");
    let late = autoarchive::sweep(
        &store,
        &here(),
        &autoarchive::no_daemon,
        0,
        created + autoarchive::ORPHAN_GRACE_MS,
        None,
    );
    assert_eq!(late.len(), 1, "{late:?}");
    assert_eq!(late[0].rule, Settled::ReviewAbandoned);
}

#[test]
fn the_daemon_window_leaves_older_facts_to_the_backfill() {
    let d = Dir::new("window");
    let store = Store::open_set(&d.0, "aaaaaaaaaaaa").expect("a set");
    package(&store, "s-old", None);
    let out = autoarchive::sweep(&store, &here(), &autoarchive::no_daemon, 3, LATER, None);
    assert!(
        out.is_empty(),
        "a fact older than the daemon was picked: {out:?}"
    );
}

#[test]
fn a_review_tree_names_its_row() {
    assert_eq!(
        autoarchive::review_row_of_tree(
            "/tmp/letibot-review-clones/review-agent-layout-split-f7c17316-3067287-0"
        )
        .as_deref(),
        Some("agent-layout-split-f7c17316")
    );
    assert_eq!(autoarchive::review_row_of_tree("/repo"), None);
}

/// A package whose worktree is gone is put out of sight, with no owner row and no origin: those
/// are the oldest packages, saved before the column existed. A worktree still on disk is work and
/// stays, and a gone folder that is no worktree stays too - it may be a conversation somebody
/// moved off this disk, and hiding that is not a rule's to do.
#[test]
fn a_gone_worktree_is_archived_and_a_live_or_plain_folder_is_kept() {
    let d = Dir::new("worktree-gone");
    let live = d.0.join("lab/s-1/wt/pkg-live");
    std::fs::create_dir_all(&live).expect("a live worktree");
    let store = Store::open_set(&d.0, "aaaaaaaaaaaa").expect("a set");
    session(&store, "s-live", None, live.to_str().expect("utf-8"));
    session(
        &store,
        "s-gone",
        None,
        d.0.join("gone/s-1/wt/pkg-gone").to_str().expect("utf-8"),
    );
    session(
        &store,
        "s-moved",
        None,
        d.0.join("moved-away").to_str().expect("utf-8"),
    );

    // The backfill's door is the same sweep with no writer: it lists the row first.
    let dry = autoarchive::sweep(&store, &here(), &autoarchive::no_daemon, 0, LATER, None);
    assert!(
        dry.iter()
            .any(|d| d.id == "s-gone" && !d.archived && d.rule == Settled::WorktreeGone),
        "a dry run names the row it would hide: {dry:?}"
    );

    let out = run(&store, &autoarchive::no_daemon, true);
    assert_eq!(
        reason_of(&store, "s-gone").as_deref(),
        Some("worktree gone"),
        "a package whose tree is gone is not a conversation to reopen: {out:?}"
    );
    assert_eq!(
        reason_of(&store, "s-live"),
        None,
        "a worktree still on disk is work: {out:?}"
    );
    assert_eq!(
        reason_of(&store, "s-moved"),
        None,
        "a gone folder that is no worktree may be the operator's: {out:?}"
    );
    assert_eq!(out.len(), 1, "one row was picked: {out:?}");
    assert_eq!(out[0].rule, Settled::WorktreeGone);
}
