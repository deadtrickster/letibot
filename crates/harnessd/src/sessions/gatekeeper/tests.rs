use super::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{Approval, LeaseClaim, MergePriority, Move, NewReview, ReviewKind};
use letibot_tools::builtins::task::{TaskRunner, TaskSpec, TaskStatus};

use crate::config::Config;
use crate::harness::Parts;
use crate::sessions::{Sessions, StoreSessions};

/// The review-queue session of the fixture's workspace - `s-gatekeeper-<workspace key>`. One
/// per daemon, so a test derives it from its own config: comparing against the old constant would
/// be asserting the defect.
fn queue_of(cfg: &Config) -> String {
    super::session(&cfg.workspace)
}

/// A runner that records what it was asked to start and answers `Running` for ever - a real
/// child needs a model, and what is measured here is whose runner was asked for what.
struct Recording {
    /// `(prompt, role)` per `start`, in order.
    started: Arc<Mutex<Vec<(String, String)>>>,
}

impl TaskRunner for Recording {
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
        let mut g = self.started.lock().unwrap_or_else(|e| e.into_inner());
        g.push((prompt.to_string(), spec.role.clone()));
        Ok(format!("sub-{}", g.len()))
    }

    fn collect(&self, _handle: &str, _timeout: Duration) -> TaskStatus {
        TaskStatus::Running {
            note: None,
            child: None,
        }
    }
}

/// A directory that removes itself.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "letibot-gatekeeper-{tag}-{}-{}",
            std::process::id(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// This test process's daemon identity - the owner the board is read for.
fn me() -> String {
    crate::mergequeue::lease::this_daemon().owner.clone()
}

fn store_with(tag: &str) -> (TempDir, Store, PathBuf) {
    let dir = TempDir::new(tag);
    let ws = dir.path.join("ws");
    std::fs::create_dir_all(&ws).expect("the workspace");
    let ws = std::fs::canonicalize(&ws).expect("the workspace canonicalizes");
    let store = Store::open(&dir.path.join("sessions.db")).expect("the store opens");
    (dir, store, ws)
}

/// The lease on `repo`, held by `owner`.
fn hold(store: &Store, repo: &Path, owner: &str) {
    let claim = LeaseClaim {
        owner: owner.into(),
        pid: 1,
        host: "t".into(),
        build: "t".into(),
    };
    store
        .acquire_queue_lease(
            &repo.display().to_string(),
            &claim,
            (crate::config::now_ns() / 1_000_000) as u64,
            crate::mergequeue::lease::LEASE_TTL_MS,
        )
        .expect("the lease");
}

/// One review row in `repo`, waiting for a reviewer.
fn entry(store: &Store, repo: &Path, id: &str, brief: &str) {
    entry_of(store, repo, id, brief, ReviewKind::Branch);
}

fn entry_of(store: &Store, repo: &Path, id: &str, brief: &str, kind: ReviewKind) {
    store
        .enqueue_review(
            &NewReview {
                id: id.into(),
                repository: repo.display().to_string(),
                target: "main".into(),
                kind,
                session_id: format!("s-{id}"),
                branch: format!("agent/{id}"),
                worktree: None,
                base_sha: "base".into(),
                head_sha: format!("head-{id}"),
                brief: brief.into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: String::new(),
            },
            "test",
            (crate::config::now_ns() / 1_000_000) as u64,
        )
        .expect("the review row");
}

/// The queue's claim, as its thread makes it: `reviewing`, owned by this daemon.
fn claim(store: &Store, id: &str) {
    assert!(
        store
            .move_review(
                id,
                ReviewState::Waiting,
                ReviewState::Reviewing,
                &Move::new(me(), "claimed").set("owner", me()),
                (crate::config::now_ns() / 1_000_000) as u64,
            )
            .expect("the claim")
    );
}

/// An approval, written the way another host writes it - straight into the store.
fn approve(store: &Store, id: &str) {
    let r = store.review_row(id).unwrap().unwrap();
    assert!(
        store
            .approve_review(
                id,
                &Approval {
                    expect: r.state,
                    expect_owner: None,
                    reviewed_sha: r.head_sha,
                    by: "operator:s-x".into(),
                    reasons: vec![],
                    files: vec![],
                    commands: vec![],
                    why: "approved".into(),
                    merge_id: format!("m-{id}"),
                },
                (crate::config::now_ns() / 1_000_000) as u64,
            )
            .unwrap()
    );
}

/// One row per review waiting for a verdict, named by the daemon's run name, the row id and
/// the first line of the ask.
#[test]
fn one_row_per_review_waiting_for_a_verdict() {
    let (_dir, store, ws) = store_with("rows");
    hold(&store, &ws, &me());
    entry(
        &store,
        &ws,
        "m-one",
        "make the reader keep the last record\nand then some",
    );
    entry(&store, &ws, "m-two", "the other branch");
    let rows = queue_rows(&store, &me());
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter()
            .all(|r| r.by == TodoBy::Queue && r.status == TodoStatus::Pending)
    );
    let first = &rows[0].content;
    assert!(
        first.contains("m-one") && first.contains("agent/m-one"),
        "{first}"
    );
    assert!(
        first.contains("make the reader keep the last record"),
        "{first}"
    );
    assert!(!first.contains("and then some"), "{first}");
    assert_eq!(
        rows[1].content,
        "review-agent/m-two (entry m-two): the other branch"
    );
}

/// A verdict reached elsewhere takes the row off - approved and rejected rows are not
/// waiting for anything.
#[test]
fn a_verdict_reached_elsewhere_takes_the_row_off() {
    let (_dir, store, ws) = store_with("verdict");
    hold(&store, &ws, &me());
    entry(&store, &ws, "m-one", "keep the last record");
    entry(&store, &ws, "m-two", "the other branch");
    assert_eq!(queue_rows(&store, &me()).len(), 2);
    approve(&store, "m-one");
    let rows = queue_rows(&store, &me());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].content.contains("m-two"));
    store
        .move_review(
            "m-two",
            ReviewState::Waiting,
            ReviewState::Rejected,
            &Move::new("r", "no").set("verdict", "reject"),
            1,
        )
        .unwrap();
    assert!(queue_rows(&store, &me()).is_empty());
}

/// A repository whose lease another daemon holds is not on this board - defect 9
/// every daemon served the one global review session, and two would each spawn a reviewer.
#[test]
fn another_daemons_repository_is_not_on_this_board() {
    let (_dir, store, ws) = store_with("served");
    let theirs = ws.parent().expect("a parent").join("other");
    hold(&store, &ws, &me());
    hold(&store, &theirs, "another-daemon");
    entry(&store, &ws, "m-mine", "mine to review");
    entry(&store, &theirs, "m-theirs", "theirs to review");
    let rows = queue_rows(&store, &me());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].content.contains("m-mine"), "{:?}", rows[0]);
}

/// The red target's review comes first on the board, as it does in the queue.
#[test]
fn the_red_targets_review_is_first() {
    let (_dir, store, ws) = store_with("red-first");
    hold(&store, &ws, &me());
    entry(&store, &ws, "m-branch", "a branch");
    entry_of(&store, &ws, "red-main", "main is red", ReviewKind::Target);
    let rows = waiting(&store, &me());
    assert_eq!(rows[0].id, "red-main", "{rows:?}");
}

/// A daemon over a temp store, with the queue's session and a recording runner. `None`
/// after announcing the skip when this box has no vocabulary GGUF.
struct Fixture {
    _dir: TempDir,
    db: PathBuf,
    ws: PathBuf,
    cfg: Config,
    registry: Arc<Registry>,
    started: Arc<Mutex<Vec<(String, String)>>>,
}

impl Fixture {
    fn new(tag: &str) -> Option<Self> {
        let gguf = crate::vocab_gguf()?;
        let dir = TempDir::new(tag);
        let ws = dir.path.join("ws");
        std::fs::create_dir_all(&ws).expect("the workspace");
        let ws = std::fs::canonicalize(&ws).expect("the workspace canonicalizes");
        // The workspace is a git repository, because a reviewer's tree is made FROM one - it
        // clones the row's repository at its `head_sha`, so a fixture with none would measure the
        // raise, not the review.
        crate::mergequeue::serve::tests::sh(&ws, &["init", "-q", "-b", "main"]);
        letibot_tools::testing::repo_config(&ws);
        std::fs::write(ws.join("a.txt"), "one\n").expect("a file");
        crate::mergequeue::serve::tests::sh(&ws, &["add", "."]);
        crate::mergequeue::serve::tests::sh(&ws, &["commit", "-q", "-m", "base"]);
        let db = dir.path.join("sessions.db");
        let mut cfg = Config::for_this_box(&ws);
        cfg.vocab_gguf = Some(gguf);
        cfg.store = Some(db.clone());
        cfg.session_id = "s-host".into();
        cfg.http_retries = 0;
        // A directory of the fixture's own for the reviewers' clones - `sweep_clones` refuses
        // any other name, and a fixture must never write into (or sweep) the box's own.
        cfg.review_clones = Some(dir.path.join(crate::mergequeue::review::CLONES_DIR));
        let registry = Registry::new();
        registry
            .create(&cfg.session_id, "", Sessions::wiring(&cfg))
            .expect("the daemon's own session registers");
        registry.set_source(StoreSessions::open(&cfg).expect("the store as a source"));
        Some(Self {
            _dir: dir,
            db,
            ws,
            cfg,
            registry,
            started: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn sessions<'p>(&self, parts: &'p Parts) -> Sessions<'p> {
        Sessions::open_first(parts, self.cfg.clone(), self.registry.clone())
            .expect("the daemon opens")
    }

    fn opened<'p>(&self, parts: &'p Parts) -> Sessions<'p> {
        let mut sessions = self.sessions(parts);
        sessions
            .ensure_gatekeeper()
            .expect("the queue's session opens");
        sessions
            .open
            .get_mut(&queue_of(&self.cfg))
            .expect("the queue's session is open")
            .set_subagents_for_test(Arc::new(Recording {
                started: self.started.clone(),
            }));
        sessions
    }

    fn asked(&self) -> Vec<(String, String)> {
        self.started
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn prompt<'p>(&self, sessions: &'p Sessions<'_>) -> &'p str {
        &sessions
            .open
            .get(&queue_of(&self.cfg))
            .expect("the queue's session is open")
            .config()
            .system
    }

    fn store(&self) -> Store {
        Store::open(&self.db).expect("the store reopens")
    }

    /// A review row in this daemon's repository (its workspace), lease held by this daemon - and
    /// the HEAD it names, because the row's head is a real commit on a real branch.
    fn entry(&self, id: &str, brief: &str) -> String {
        let (_wt, base, head) =
            crate::mergequeue::serve::tests::branch(&self.ws, id, &format!("{id}.txt"));
        let s = self.store();
        hold(&s, &self.ws, &me());
        s.enqueue_review(
            &NewReview {
                id: id.into(),
                repository: self.ws.display().to_string(),
                target: "main".into(),
                kind: ReviewKind::Branch,
                session_id: format!("s-{id}"),
                branch: format!("agent/{id}"),
                worktree: None,
                base_sha: base,
                head_sha: head.clone(),
                brief: brief.into(),
                priority: MergePriority::Subagent,
                fixes: None,
                evidence: String::new(),
            },
            "test",
            (crate::config::now_ns() / 1_000_000) as u64,
        )
        .expect("the review row");
        head
    }

    fn board(&self, sessions: &Sessions<'_>) -> Vec<TodoItem> {
        sessions
            .open
            .get(&queue_of(&self.cfg))
            .expect("the gatekeeper session is open")
            .todo_list()
            .into_iter()
            .filter(|r| r.by == TodoBy::Queue)
            .collect()
    }
}

/// The session is TOLD what its board is - the operator's *"it managed via its prompt and so
/// on"*, the one place a session is managed with nobody at the keyboard. Four facts are asserted
/// rather than the whole text: whose rows they are, the child, its handle, and what takes a row off.
#[test]
fn the_session_is_told_what_its_board_is() {
    let Some(f) = Fixture::new("prompt") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    let sessions = f.opened(&parts);
    let said = f.prompt(&sessions);

    assert!(
        said.contains("You are the review queue"),
        "the session is told what it is: {said}"
    );
    assert!(
        said.contains("A review is a `task` child of YOURS"),
        "and that the reviewer is its own child, which is the half this slice moves: {said}"
    );
    assert!(
        said.contains("task_result") && said.contains("task_message"),
        "and which doors it has on that child: {said}"
    );
    assert!(
        said.contains("do not start one yourself"),
        "and that the queue's own clock starts it, so it must not start a second: {said}"
    );
    assert!(
        said.contains("no field of `todo_write` reaches it"),
        "and that the rows are not its to strike off: {said}"
    );
    assert!(
        said.contains("The verdict is what takes the row off your board"),
        "and what DOES take a row off: {said}"
    );
    // And that the verdict is the CHILD's own act: a session that went looking for one in
    // `task_result`'s payload would be reading exactly what the queue stopped reading.
    assert!(
        said.contains("review_verdict"),
        "and that the reviewer hands its verdict in ITSELF: {said}"
    );
    assert!(
        said.contains("has judged nothing"),
        "and what an answer in prose means: {said}"
    );
    // And it is the session's OWN prompt, not the daemon's - the ordinary sessions are the
    // composed prompt byte for byte, which is what `OwnSession::prompt`'s `None` means.
    assert!(
        !sessions
            .open
            .get(&f.cfg.session_id)
            .expect("the daemon's own session is open")
            .config()
            .system
            .contains("You are the review queue"),
        "the paragraph goes to the queue's session and to no other"
    );
}

/// A reviewer is placed in a VM where this box can boot one, and on this machine where it
/// cannot - and *the box cannot* is a DECLARED state, not a silent fallback. The box is an
/// argument rather than the environment, so no `$FIRECODE_BIN` is raced.
#[test]
fn a_reviewer_is_placed_where_the_box_can_place_one() {
    let here = PathBuf::from("/usr/local/bin/firecode");
    let absent = "there is no `firecode` on this host's PATH (nor $FIRECODE_BIN)".to_string();
    // The box can boot one and the session does not object: the reviewer runs in a VM.
    assert_eq!(
        reviewer_placement(true, Ok(here)).placement(),
        Placement::Firecode,
        "a box that can boot a VM places the reviewer in one: the VM is the boundary, so no \
             `bash` call of its is a permission card"
    );
    // And the box cannot: a state carrying the box's own words, not a bare `Host`.
    let no_vm = reviewer_placement(true, Err(absent.clone()));
    assert_eq!(
        no_vm,
        ReviewerPlacement::NoVm { why: absent },
        "a box that cannot boot one runs the reviewer here and SAYS SO — the reason travels \
             with the state instead of being dropped on the floor"
    );
    assert_eq!(
        no_vm.placement(),
        Placement::Host,
        "and what the spawn carries is this machine — the only placement left"
    );
    // And a project that says `host` is not overridden by a box that could boot one.
    assert_eq!(
        reviewer_placement(false, Ok(PathBuf::from("/usr/bin/firecode"))),
        ReviewerPlacement::Host,
        "the choice is the project's, and a box that could boot a VM does not undo it"
    );
}

/// The session is told WHERE ITS REVIEWERS RUN, and which of the three states it is in - the
/// two `host` states have different consequences. All three arms are asserted on the pure function
/// where the box is an argument and no machine's `$PATH` is consulted.
#[test]
fn the_session_is_told_where_its_reviewers_run() {
    let vm = prompt(&reviewer_placement(
        true,
        Ok(PathBuf::from("/usr/bin/firecode")),
    ));
    assert!(
        vm.contains("You are the review queue"),
        "the constant half is still there, ahead of the placement: {vm}"
    );
    assert!(
        vm.contains("Where your reviewers run: **in a firecode VM**"),
        "a VM placement is said, and said as WHERE ITS REVIEWERS RUN: {vm}"
    );
    let why = "there is no `firecode` on this host's PATH (nor $FIRECODE_BIN)".to_string();
    let none = prompt(&reviewer_placement(true, Err(why.clone())));
    assert!(
        none.contains("NOT in a VM") && none.contains(&why),
        "and a box that cannot boot one is a DECLARED state carrying the box's own words — \
             the difference between this and the arm below is the whole reason the state exists: \
             {none}"
    );
    let host = prompt(&reviewer_placement(
        false,
        Ok(PathBuf::from("/usr/bin/firecode")),
    ));
    assert!(
        host.contains("`[queue] reviewers` says `host`")
            && host.contains("in a CHECKOUT OF THE HEAD THEY JUDGE"),
        "and a project that says `host` is told the choice is why, not the box — and told which \
         tree its reviewers read: {host}"
    );
    assert!(
        host.contains("What that gives up, said plainly")
            && host.contains("it can run anything the operator can"),
        "and the cost of the default is stated where the session reads it, not left to be \
         discovered: {host}"
    );
}

/// A session that says `host` reads it in its own prompt - the arm a test can put a real
/// session in on ANY box. The fixture says nothing to get it (`Config::for_this_box` says `host`)
/// so this also asserts that a session a test opens cannot boot a microVM by forgetting a line.
#[test]
fn a_session_that_says_host_is_told_its_reviewers_run_here() {
    let Some(f) = Fixture::new("prompt-host") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    let sessions = f.opened(&parts);
    let said = f.prompt(&sessions);

    assert!(
        said.contains("Where your reviewers run: **on this machine**")
            && said.contains("`[queue] reviewers` says `host`"),
        "the queue's session reads where its reviewers run, and the choice is named as the \
             reason rather than the box: {said}"
    );
}

/// The session is started once, and a second pass or a second daemon does not make a twin.
/// The idle arm calls `ensure_gatekeeper` every thirty seconds, so "once" is a property of the
/// door and not of the caller: `self.open` for this daemon, the registry for the box.
#[test]
fn the_session_is_started_once_and_not_twice() {
    let Some(f) = Fixture::new("once") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    let mut sessions = f.sessions(&parts);
    let held = |reg: &Registry| {
        reg.list()
            .iter()
            .filter(|b| b.session_id == queue_of(&f.cfg))
            .count()
    };

    assert!(
        sessions.ensure_gatekeeper().expect("it starts"),
        "the daemon starts it"
    );
    assert!(
        sessions.open.contains_key(&queue_of(&f.cfg)),
        "and holds a harness for it"
    );
    assert_eq!(held(&f.registry), 1, "one row in the registry, not two");

    // The second pass is the same session, not a second one. This is the common case: the
    // idle arm reaches this line every time the review clock is due.
    assert!(
        !sessions.ensure_gatekeeper().expect("a no-op"),
        "the second pass starts nothing"
    );
    assert_eq!(
        sessions.opened(),
        2,
        "the held session and the queue's, and no third"
    );
    assert_eq!(held(&f.registry), 1, "still one");

    // And a second DAEMON does not make a twin either - the restart, one process along. The
    // first daemon goes first because a session has one writer (`crate::writer`), and the store's
    // name is changed first because a RESUMED session keeps its row's name and a fresh one would not.
    f.store()
        .set_title(&queue_of(&f.cfg), "the queue, renamed")
        .expect("the store takes a name");
    drop(sessions);
    let second = Registry::new();
    second
        .create("s-host-2", "", Sessions::wiring(&f.cfg))
        .expect("the second daemon's own session registers");
    second.set_source(StoreSessions::open(&f.cfg).expect("the store as a source"));
    let mut other = Sessions::open_first(
        &parts,
        Config {
            session_id: "s-host-2".into(),
            ..f.cfg.clone()
        },
        second.clone(),
    )
    .expect("the second daemon opens");
    assert!(other.ensure_gatekeeper().expect("it brings it in"));
    assert_eq!(held(&second), 1, "under the SAME id, resumed");
    assert_eq!(
        held(&f.registry),
        1,
        "and the first daemon's row is untouched"
    );
    assert_eq!(
        second
            .list()
            .iter()
            .find(|b| b.session_id == queue_of(&f.cfg))
            .map(|b| b.title.as_str()),
        Some("the queue, renamed"),
        "RESUMED, not re-made: it came back under the name its own store row holds"
    );

    let stored = f.store().list_sessions().expect("the store lists");
    assert_eq!(
        stored.iter().filter(|s| s.id == queue_of(&f.cfg)).count(),
        1,
        "ONE row in the store: a second daemon resumes the session, it does not mint a twin"
    );
    assert_eq!(
        stored
            .iter()
            .find(|s| s.id == queue_of(&f.cfg))
            .map(|s| s.title.as_deref()),
        Some(Some("the queue, renamed")),
        "and it is the session a person reads in `--sessions`: {stored:?}"
    );
}

/// A claimed review is a `task` child of THIS session, bound to the head the claim read
/// (decision 6), and the row says which child is on it; a second pass starts no second child
/// the verdict takes the row off.
#[test]
fn a_claimed_review_is_this_sessions_child_and_its_row_says_so() {
    let Some(f) = Fixture::new("child") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    let head = f.entry("m-one", "make the reader keep the last record");
    let mut sessions = f.opened(&parts);
    assert_eq!(sessions.fill_gatekeeper_board(), 1);
    assert_eq!(f.board(&sessions)[0].status, TodoStatus::Pending);
    assert_eq!(
        sessions.serve_reviews(),
        0,
        "nothing is started for a row the queue has not claimed"
    );

    claim(&f.store(), "m-one");
    assert_eq!(sessions.serve_reviews(), 1, "{:?}", f.asked());
    let asked = f.asked();
    let (prompt, role) = &asked[0];
    assert_eq!(role, "gatekeeper");
    assert!(
        prompt.starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX),
        "{prompt}"
    );
    assert!(
        prompt.contains("make the reader keep the last record"),
        "{prompt}"
    );
    assert!(
        prompt.contains(&format!("...{head}")),
        "the reviewer reads the HEAD the claim bound, not the branch as it is: {prompt}"
    );
    let row = f.store().review_row("m-one").unwrap().unwrap();
    assert_eq!(row.reviewer.as_deref(), Some("sub-1"), "{row:?}");
    let during = f.board(&sessions);
    assert_eq!(during[0].status, TodoStatus::InProgress, "{during:?}");
    assert_eq!(
        during[0].needs,
        vec![TodoNeed::Child { id: "sub-1".into() }]
    );

    assert_eq!(sessions.serve_reviews(), 0, "no second child for one row");
    assert_eq!(f.asked().len(), 1);

    // The daemon's own session serves nothing: reviewers are the queue session's.
    assert_eq!(
        sessions
            .open
            .get_mut(&f.cfg.session_id)
            .expect("open")
            .serve_reviews(),
        0
    );
}

/// A review queue per daemon, and the id is the workspace's. The id was the constant
/// `s-gatekeeper` in every daemon while they share ONE store, so two daemons opened one transcript
/// .
#[test]
fn two_workspaces_get_two_review_sessions_and_one_workspace_keeps_its_own() {
    let a = queue_of(&Config::for_this_box("/tmp/letibot-review-a"));
    let b = queue_of(&Config::for_this_box("/tmp/letibot-review-b"));
    assert_ne!(a, b, "two daemons must not share one transcript");
    assert_eq!(
        a,
        queue_of(&Config::for_this_box("/tmp/letibot-review-a")),
        "the same folder's daemon comes back to the same session after a restart"
    );
    assert_eq!(a, format!("s-gatekeeper-{}", &a["s-gatekeeper-".len()..]));
    assert!(
        letibot_sessionlog::is_gatekeeper_session(&a),
        "the id is recognised as a review queue's by shape: {a}"
    );
    assert!(
        letibot_sessionlog::is_gatekeeper_session("s-gatekeeper"),
        "and so is the constant every daemon shared before, which a store still holds"
    );
    // A reviewer child of one is NOT one of these - `hubs_of_repository` and `roots_of` filter
    // on this predicate, and a child admitted as "the queue's board" would be a publish into a
    // subagent's hub.
    assert!(
        !letibot_sessionlog::is_gatekeeper_session(&format!("{a}-sub-1791")),
        "a reviewer's id is its parent's plus a suffix, and it is not the queue"
    );
}

/// Every tool name this build seats anywhere, so a backticked word in the prompt can be told from
/// prose. The roles' own lists, plus what the daemon registers beside a role rather than in one.
fn known_tools() -> std::collections::BTreeSet<String> {
    use letibot_tools::runtime::roles;
    let mut all = std::collections::BTreeSet::new();
    for r in [
        roles::orchestrator(),
        roles::coder(),
        roles::leticode(),
        roles::m1_orchestrator(),
        roles::review_queue(),
        roles::m3_researcher(),
        roles::planner(),
        roles::m2_coder(),
        roles::m2_runner(),
        roles::gatekeeper(),
        roles::notes_keeper(),
    ] {
        all.extend(r.tools);
    }
    for t in [
        "flowy",
        "web_search",
        "web_fetch",
        "review_verdict",
        "digest",
        "decisions",
    ] {
        all.insert(t.into());
    }
    all
}

/// The prompt is written for the seat it is given to. It told the queue's session to `task_result`
/// and `task_message` its reviewers while that session was seated M1's role, which holds neither -
/// a session managed by its prompt, told to call two tools it does not have.
#[test]
fn every_tool_the_queue_prompt_names_is_on_the_seat_that_serves_it() {
    use letibot_tools::runtime::roles;
    let queue = roles::review_queue();
    let reviewer = roles::gatekeeper();
    let known = known_tools();
    let text = prompt(&ReviewerPlacement::Host);
    let named: std::collections::BTreeSet<String> = text
        .split('`')
        .skip(1)
        .step_by(2)
        .map(|w| w.split_whitespace().next().unwrap_or("").to_string())
        .filter(|w| known.contains(w))
        .collect();
    let delta = || {
        format!(
            "\n  queue {}: {:?}\n  reviewer {}: {:?}\n  named: {:?}",
            queue.name, queue.tools, reviewer.name, reviewer.tools, named
        )
    };
    assert!(
        named.contains("task_result") && named.contains("todo_write"),
        "the scan found no tool names at all, so it proves nothing{}",
        delta()
    );
    // `task` is the one word on neither seat: a review IS the child `task` would start, and the
    // sentence that says so forbids starting one. Everything else the prompt names is a seat's.
    let described_only = ["task"];
    let lacks: Vec<&String> = named
        .iter()
        .filter(|t| {
            !queue.tools.contains(t)
                && !reviewer.tools.contains(t)
                && !described_only.contains(&t.as_str())
        })
        .collect();
    assert!(
        lacks.is_empty(),
        "the queue's prompt prescribes {lacks:?} and neither of the two seats holds them{}",
        delta()
    );

    // The pair it prescribes for the queue itself, on the queue's seat and nowhere else.
    for tool in ["task_result", "task_message"] {
        assert!(
            queue.tools.iter().any(|t| t == tool),
            "the queue's prompt prescribes `{tool}` and its seat lacks it{}",
            delta()
        );
        assert!(
            !reviewer.tools.iter().any(|t| t == tool),
            "and the reviewer's seat must not have it - a reader that collects is a second \
             author{}",
            delta()
        );
    }
    // And the two it names as the reviewer's own, which the queue must not hold either.
    for tool in ["review_verdict", "bash"] {
        assert!(
            reviewer.tools.iter().any(|t| t == tool),
            "the prompt names `{tool}` as the reviewer's door{}",
            delta()
        );
        assert!(
            !queue.tools.iter().any(|t| t == tool),
            "and the queue's seat must not hold it{}",
            delta()
        );
    }
}

/// The seat a review-queue session resolves is its own, from the session rather than from a choice
/// a person made: `Seat::Orchestrator` is what the queue records, and M1's role is not what it gets.
#[test]
fn the_queue_seat_resolves_to_the_queue_role() {
    use letibot_tools::runtime::roles;
    let cfg = Config::for_this_box("/tmp/letibot-review-queue-seat");
    let queue = Config {
        session_id: queue_of(&cfg),
        ..cfg.clone()
    };
    assert!(is_the_queue_session(&queue), "{}", queue.session_id);
    assert_eq!(queue.seat, SEAT, "the queue's recorded seat is unchanged");
    let seat = crate::harness::base_role_for_seat(SEAT, &queue);
    let expected = roles::review_queue();
    let delta = || {
        format!(
            "\n  resolved {}: {:?}\n  expected {}: {:?}\n  m1 {}: {:?}",
            seat.name,
            seat.tools,
            expected.name,
            expected.tools,
            roles::m1_orchestrator().name,
            roles::m1_orchestrator().tools
        )
    };
    assert_eq!(
        seat.tools,
        expected.tools,
        "the queue's session is seated the queue's role, not M1's{}",
        delta()
    );
    for tool in ["task_result", "task_message"] {
        assert!(
            seat.tools.iter().any(|t| t == tool),
            "and `{tool}` is why: its prompt prescribes it{}",
            delta()
        );
    }
    for tool in ["task", "task_start", "review_verdict", "bash"] {
        assert!(
            !seat.tools.iter().any(|t| t == tool),
            "while `{tool}` is not on it{}",
            delta()
        );
    }

    // The narrowing is the SESSION's, not the seat's: a session next door in the same workspace
    // and the reviewer children of the queue are both ordinary.
    let ordinary = Config {
        session_id: format!("s-{}", std::process::id()),
        ..cfg.clone()
    };
    assert_eq!(
        crate::harness::base_role_for_seat(SEAT, &ordinary).tools,
        roles::m1_orchestrator().tools,
        "an ordinary orchestrator session still seats M1{}",
        delta()
    );
    let reviewer = Config {
        session_id: format!("{}-sub-1791", queue.session_id),
        ..cfg.clone()
    };
    assert!(
        !is_the_queue_session(&reviewer),
        "a reviewer child is not the queue: {}",
        reviewer.session_id
    );
    // Its own seat, `bash` behind the daemon's flag as `base_role_for_seat` filters it: the queue's
    // check must not have taken the reviewer's seat over.
    let mut expected = roles::gatekeeper().tools;
    if !reviewer.allow_bash {
        expected.retain(|t| t != "bash");
    }
    assert_eq!(
        crate::harness::base_role_for_seat(crate::config::Seat::Gatekeeper, &reviewer).tools,
        expected,
        "and its own seat is untouched{}",
        delta()
    );
}

/// The seat's room context names it by its own handle, and Claude Code's names appear only as
/// other principals it must not use.
#[test]
fn the_gatekeepers_room_context_never_claims_claude_codes_name() {
    let handle = room_handle(Path::new("/work/stroppy"));
    assert_eq!(handle, "gatekeeper@stroppy");
    let said = prompt_as(&ReviewerPlacement::Host, &handle);
    assert!(said.contains("`gatekeeper@stroppy`"), "{said}");
    assert!(
        !said.contains("@HANDLE@") && !said.contains("<your"),
        "{said}"
    );
    assert!(!said.contains("<project"), "no placeholder is left: {said}");
    for line in said.lines().flat_map(|l| l.split(". ")) {
        if line.contains("claude-code") {
            assert!(
                line.contains("other principals") || line.contains("never sign"),
                "claude-code read as self: {line}"
            );
        }
    }
    assert!(!said.contains("claude-code here"), "{said}");
}

/// A `review_brief` layer's paragraph reaches the queue's own prompt, and nothing else does: the
/// host's protocol stays whole, because the row mechanics and the operator's verbs are not a
/// project's words to replace (`docs/plugin-extension-points-wide.md` X18).
#[test]
fn the_queue_prompt_takes_a_layers_added_words_and_keeps_the_hosts_own() {
    let placement = ReviewerPlacement::Host;
    let native = prompt(&placement);
    // A layer adding a rule: appended, with the host's text untouched up to it.
    let adding = letibot_plugin::review::Brief {
        brief_add: Some("Every review of this project names the flaky test it saw.".into()),
        ..Default::default()
    };
    let shaped = prompt_with(&placement, "gatekeeper", Some(&adding));
    assert!(
        shaped.starts_with(&native),
        "the host's prompt is whole, and the layer's words come after it: {shaped}"
    );
    assert!(
        shaped.ends_with("Every review of this project names the flaky test it saw."),
        "{shaped}"
    );
    // A layer REPLACING the body is ignored here: the queue's prompt is the protocol, and a project
    // that replaced it would be telling the queue to run reviews it has no verbs for.
    let replacing = letibot_plugin::review::Brief::saying("Anything the reviewer says is fine.");
    assert_eq!(
        prompt_with(&placement, "gatekeeper", Some(&replacing)),
        native
    );
    // And no layer at all is the native prompt, byte for byte.
    assert_eq!(prompt_with(&placement, "gatekeeper", None), native);
}
