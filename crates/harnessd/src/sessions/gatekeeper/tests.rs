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

/// **The review-queue session of the fixture's workspace** — `s-gatekeeper-<workspace key>`.
///
/// One per daemon now, so a test that names it derives it from the config it built rather than from
/// a constant every daemon on the box once shared; a test that compared against the constant would
/// be asserting the defect.
fn queue_of(cfg: &Config) -> String {
    super::session(&cfg.workspace)
}

/// **A runner that records what it was asked to start and answers `Running` for ever** — the
/// child a real runner would start needs a model, and what is measured here is *whose runner
/// was asked for what*.
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

/// This test process's daemon identity — the owner the board is read for.
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

/// An approval, written the way another host writes it — straight into the store.
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

/// **One row per review waiting for a verdict, named by the daemon's run name, the row id and
/// the first line of the ask.**
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

/// **A verdict reached elsewhere takes the row off** — approved and rejected rows are not
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

/// **A repository whose lease another daemon holds is not on this board** — R61 defect 9:
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

/// **The red target's review comes first on the board, as it does in the queue.**
#[test]
fn the_red_targets_review_is_first() {
    let (_dir, store, ws) = store_with("red-first");
    hold(&store, &ws, &me());
    entry(&store, &ws, "m-branch", "a branch");
    entry_of(&store, &ws, "red-main", "main is red", ReviewKind::Target);
    let rows = waiting(&store, &me());
    assert_eq!(rows[0].id, "red-main", "{rows:?}");
}

/// **A daemon over a temp store**, with the queue's session and a recording runner. `None`
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
        // **The workspace is a git repository, because a reviewer's tree is made FROM one.**
        // `mergequeue::review::start_reviewer` clones the row's repository at its `head_sha` for
        // both placements (a reviewer judges a checkout of the head, never the tree the operator
        // has out), so a fixture with no repository would measure *a reviewer that could not
        // start* — the raise, not the review.
        crate::mergequeue::serve::tests::sh(&ws, &["init", "-q", "-b", "main"]);
        crate::mergequeue::serve::tests::sh(&ws, &["config", "user.email", "q@test"]);
        crate::mergequeue::serve::tests::sh(&ws, &["config", "user.name", "queue test"]);
        std::fs::write(ws.join("a.txt"), "one\n").expect("a file");
        crate::mergequeue::serve::tests::sh(&ws, &["add", "."]);
        crate::mergequeue::serve::tests::sh(&ws, &["commit", "-q", "-m", "base"]);
        let db = dir.path.join("sessions.db");
        let mut cfg = Config::for_this_box(&ws);
        cfg.vocab_gguf = Some(gguf);
        cfg.store = Some(db.clone());
        cfg.session_id = "s-host".into();
        cfg.http_retries = 0;
        // **A directory of the fixture's own** for the reviewers' clones — `sweep_clones` refuses
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

    /// A review row in this daemon's repository (its workspace), lease held by this daemon — and
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

/// **The session is TOLD what its board is** — the operator's *"it managed via its prompt and
/// so on"*, which is the one place a session can be managed without a person at the keyboard.
///
/// Four facts the paragraph has to carry, and each is asserted rather than the whole text: the
/// rows are the queue's, a review is a `task` child of THIS session, the row names the child's
/// handle, and the verdict — not `todo_write` — is what takes a row off. A prompt that lost one
/// of them would be a session guessing at exactly the thing this slice moved.
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
    // **And that the verdict is the CHILD's own act**, which is the half this slice adds: the
    // session is told the door the reviewer holds, and that a reply in prose is not a verdict —
    // because a session that went looking for one in `task_result`'s payload would be reading
    // exactly what the queue stopped reading.
    assert!(
        said.contains("review_verdict"),
        "and that the reviewer hands its verdict in ITSELF: {said}"
    );
    assert!(
        said.contains("has judged nothing"),
        "and what an answer in prose means: {said}"
    );
    // **And it is the session's OWN prompt, not the daemon's** — the ordinary sessions are the
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

/// **A reviewer is placed in a VM where this box can boot one, and on this machine where it
/// cannot — and *the box cannot* is a DECLARED state, not a silent fallback.**
///
/// The box is an ARGUMENT here rather than the environment, so every arm holds on whatever
/// machine runs this and `$FIRECODE_BIN` is not raced — the discipline `firecode.rs` states for
/// its own `bin` field. Three arms, and the third is the point: *the session said no* and *the
/// box could not* are two different states, and a catch-all `_ => Host` is exactly how they
/// become one thing that neither a reader nor the session can tell apart.
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
    // **And the box cannot: a state carrying the box's own words**, not a bare `Host`.
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
    // **And a project that says `host` is not overridden by a box that could boot one.**
    assert_eq!(
        reviewer_placement(false, Ok(PathBuf::from("/usr/bin/firecode"))),
        ReviewerPlacement::Host,
        "the choice is the project's, and a box that could boot a VM does not undo it"
    );
}

/// **The session is told WHERE ITS REVIEWERS RUN**, and told which of the three states it is
/// in — because the two `host` states have different consequences for the session reading them.
///
/// `the_session_is_told_what_its_board_is` asserts the constant half. This is the half that
/// cannot be a constant, so all three arms are asserted on the pure function — where the box is
/// an argument and no machine's `$PATH` is consulted — and the arm a real session can be put in
/// without racing the box is asserted on the session itself below.
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

/// **A session that says `host` reads it in its own prompt** — the arm a test can put a real
/// session in on ANY box, because `host` is the default whatever `firecode_available()` says. The
/// other two arms are the pure function's, above.
///
/// **The fixture says nothing to get this.** It is built the ordinary way
/// (`Config::for_this_box`), which says `host` — so this is also the assertion that a session a
/// test opens cannot boot a microVM over its workspace by forgetting a line. A test that means
/// to exercise the VM arm asks for one by name (`Config::allow_vm_reviewers`), and the VM arm's
/// own words are asserted on the pure function above, where the box is an argument rather than a
/// `$FIRECODE_BIN` race.
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

/// **The session is started once, and a second pass or a second daemon does not make a twin.**
///
/// The idle arm calls `ensure_gatekeeper` on every pass it reads the store — every thirty
/// seconds — so "once" has to be a property of the door and not of the caller. It is two
/// checks: `self.open` for this daemon, and the store (through the registry) for the box.
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

    // **The second pass is the same session, not a second one.** This is the common case: the
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

    // **And a second DAEMON does not make a twin either** — the restart, one process along: a
    // fresh registry over the same store, which finds the session on disk and RESUMES it.
    //
    // **The first daemon GOES FIRST, and that is now a condition of the test rather than a
    // convenience.** A session has one writer (`crate::writer`, and
    // `sessions::writer_tests::a_second_session_cannot_write_one_transcript` proves it): a second
    // daemon opening one a LIVE process holds is refused, which is the whole of what that guard
    // is for. What this half measures is the restart — the first daemon gone, its claim given
    // back with its harnesses — and that is what a restart is.
    //
    // **The name is changed in the store first, and that is what makes this an assertion rather
    // than a coincidence**: a session made fresh would carry the hardcoded `TITLE`, and one
    // RESUMED carries the name its own row holds. The count alone could not tell the two apart —
    // `put_session` is skipped for a row that is already there, so a twin would leave one row too.
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

/// **A claimed review is a `task` child of THIS session, bound to the head the claim read**
/// (decision 6), and the row says which child is on it; a second pass starts no second child;
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

    // **The daemon's own session serves nothing**: reviewers are the queue session's.
    assert_eq!(
        sessions
            .open
            .get_mut(&f.cfg.session_id)
            .expect("open")
            .serve_reviews(),
        0
    );
}

/// **A review queue per daemon, and the id is the workspace's** — the defect this replaced, at
/// the level of the id.
///
/// The id used to be the constant `s-gatekeeper` in every daemon on every box, while every daemon
/// on the box shares ONE store — so the operator's daemon and the project daemon each opened their
/// own copy of the same session and both appended to one transcript (MEASURED 2026-10-26:
/// `s-gatekeeper#t0`, `transcript_item seq must be the next one`). Two folders are two daemons and
/// now two sessions; the same folder is the same session across a restart, which is what keeps its
/// board and its review rows.
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
    // **A reviewer child of one is NOT one of these** — `hubs_of_repository` and `roots_of` filter
    // on this predicate, and a child admitted as "the queue's board" would be a publish into a
    // subagent's hub.
    assert!(
        !letibot_sessionlog::is_gatekeeper_session(&format!("{a}-sub-1791")),
        "a reviewer's id is its parent's plus a suffix, and it is not the queue"
    );
}
