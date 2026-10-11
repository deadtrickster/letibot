//! **The daemon's own clock over the merge queue's reviews** — `Sessions::serve_reviews`, the
//! pass that reads the store for the reviews the sessions this daemon holds are owed.
//!
//! The defect this file is written against was measured on 2026-10-10, and it was two defects in
//! one shape: **an owed review was served from a bell rather than from the store.**
//!
//! 1. `Harness::serve_reviews` had exactly one caller, the top of `Harness::wake`. A session
//!    inside a turn cannot be woken — the wake queues behind the turn — so a busy session starved
//!    its own queue. Eighteen `Due` rows with `attempts = 0`, none answered, no gatekeeper child
//!    and nothing in the log, for half an hour, while both hosting sessions were busy.
//! 2. A ring is a bell and a bell is per-daemon. A mover in daemon A asking for a review hosted by
//!    a session in daemon B rings A's bell and reaches nobody; and a daemon that restarts between
//!    the ask and the spawn has nothing left to ring, because the ring is in memory and the row is
//!    not. *"restarted pg-noop and nothing moves."*
//!
//! The answer is the one this file asserts: **each daemon reads the store for the reviews its own
//! sessions host, on its own clock** — the store being the only medium two daemons share, so
//! nothing has to reach anything. `Sessions::serve_reviews` is that pass and `Sessions::reviews_due`
//! is its clock; the body is `Harness::serve_reviews`, the same function the wake and the round
//! boundary call, so all three doors make one decision from one body of evidence.
//!
//! What is asserted here, and what the requirement named:
//!
//! * **A `Due` review whose session is idle is spawned by the pass, with no wake involved.** Nothing
//!   rings anything in this file: the row is written, the pass is called, and a gatekeeper is
//!   started with the review's brief and the `gatekeeper` seat.
//! * **The reviewer is a `task` child of the REVIEW QUEUE's session and of no other.** The row names
//!   it (`mergequeue::GatekeeperDoor` writes `gatekeeper::SESSION`), `Harness::serve_reviews` spawns
//!   through that harness's own runner — the same door `task` uses — and the daemon's own first
//!   session is asked to start nothing at all. The child's `parent_session_id` is the queue's
//!   session, its settlement is published on that session's hub, and its approvals ask at that
//!   session's head.
//! * **A review already being served is not spawned twice by a second pass.** The store still says
//!   `Due` — the row is a request and an attempt in flight is the same row — so the guard that
//!   answers this is the session's own review desk, and the assertion is that the second pass
//!   starts nothing and writes nothing.
//! * **A review naming a session this daemon does not hold is left alone.** The row stays as it
//!   was, and no child is started for it: the pass serves what it holds and nothing else.
//! * **A row whose backoff has not elapsed is not asked again** — the other half of idempotence,
//!   and the one the STORE carries (`attempts`/`failed_ms` through `mergequeue::review_retry`).
//!
//! # And the verdict the pass writes is the CHILD's own
//!
//! The pass starts a reviewer and OPENS the review on the daemon's desk
//! (`letibot_tools::builtins::verdict::ReviewDesk`); the reviewer hands its verdict in through the
//! one tool it holds (`review_verdict`), and the next pass writes what it handed in onto the
//! entry's review row. The parent never reads the child's answer for a decision — a reply in prose
//! is a failed attempt, and a child that dies without submitting writes nothing at all. Four tests
//! below are that seam end to end, on the daemon's own door: *a submitted verdict moves the entry
//! and takes the row off*, *a child of another session cannot submit for this queue*, *an answer
//! with no submission is not a verdict*, and *a child that dies without submitting writes
//! nothing*.
//!
//! # What it needs
//!
//! The vocabulary GGUF, because `Harness::open` renders its stable prefix — `compact.rs`'s and
//! `queue_e2e`'s situation exactly, answered the same way: [`present_gguf`] announces a skip when
//! it is not here rather than failing, and `LETIBOT_REQUIRE_APPARATUS=1` refuses that skip. **Not a
//! model**: no round runs here. The child a real runner would start needs one, which is why the
//! harnesses that could be asked to start one are handed a recording runner
//! (`Harness::set_subagents_for_test`) — the measurement is *what the pass asked for, and of
//! which session*, not what a child then did.
//!
//! And **not the live store**: the queue lives in `sessions.db`, and every test here builds its own
//! in a temp directory that removes itself.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{MergeEntry, MergePriority, MergeState, ReviewRecord, Store};
use letibot_tools::builtins::task::{Placement, TaskRunner, TaskSpec, TaskStatus};

use super::*;

/// A directory that removes itself, because a test that leaks a store per run eventually fills
/// the disk and the run that finds out is not this one.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "letibot-review-pass-{tag}-{}-{}",
            std::process::id(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// **One `start`, as the recording runner saw it**: the prompt, the seat, and where the pass asked
/// for the child to run.
///
/// The placement is here because it is a decision the pass makes FOR a reviewer and for no other
/// child — see `gatekeeper::reviewer_placement` — so *what the pass asked for* is not the whole
/// answer without it. **And `vm_source` is here because it is the other half of that same
/// decision**: a `firecode` placement copies a tree, and which tree it copies is the pass's word
/// (`mergequeue::review_copy_source`) and not the caller's. Every other field of the [`TaskSpec`]
/// is the caller's and has its own test; these two are the pass's own.
#[derive(Debug, Clone)]
struct Asked {
    prompt: String,
    role: String,
    placement: Placement,
    /// The tree a `firecode` placement would copy — `None` is *the child's own workspace*.
    vm_source: Option<PathBuf>,
}

/// **A runner that records what it was asked to start and answers `Running` for ever.**
///
/// A running child is the interesting one: it stays on the review desk, which is
/// the state the second-pass test needs, and it never writes a verdict onto a row.
struct Recording {
    /// One entry per `start`, in order.
    started: Arc<Mutex<Vec<Asked>>>,
}

impl TaskRunner for Recording {
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
        let mut g = self.started.lock().unwrap_or_else(|e| e.into_inner());
        g.push(Asked {
            prompt: prompt.to_string(),
            role: spec.role.clone(),
            placement: spec.placement,
            vm_source: spec.vm_source.clone(),
        });
        Ok(format!("sub-{}", g.len()))
    }

    fn collect(&self, _handle: &str, _timeout: Duration) -> TaskStatus {
        TaskStatus::Running {
            note: None,
            child: None,
        }
    }
}

/// **A runner whose child has ANSWERED, with prose and nothing else.**
///
/// The ending this slice is measured by: the reviewer's turn is over, it never called
/// `review_verdict`, and the queue must record an ATTEMPT rather than read a decision out of what
/// it wrote. `collect` answers `Done` for ever, which is what the settled slot holds.
struct Answered {
    /// One entry per `start`, in order.
    started: Arc<Mutex<Vec<Asked>>>,
    /// What the child "said". Prose, and deliberately carrying the words the old closing block
    /// would have been parsed for — so a build that still read it would pass a weaker test.
    answer: String,
}

impl TaskRunner for Answered {
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
        let mut g = self.started.lock().unwrap_or_else(|e| e.into_inner());
        g.push(Asked {
            prompt: prompt.to_string(),
            role: spec.role.clone(),
            placement: spec.placement,
            vm_source: spec.vm_source.clone(),
        });
        Ok(format!("sub-{}", g.len()))
    }

    fn collect(&self, _handle: &str, _timeout: Duration) -> TaskStatus {
        TaskStatus::Done {
            answer: self.answer.clone(),
            child: None,
        }
    }
}

/// **A runner whose child is GONE** — a daemon that restarted mid-review, or a child that died
/// without a word. `Unknown` is the handle-this-runner-never-minted answer, and it is the fourth
/// ending a review can have: nothing was written, and the row stays for the queue to ask again.
struct Gone {
    /// One entry per `start`, in order.
    started: Arc<Mutex<Vec<Asked>>>,
}

impl TaskRunner for Gone {
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String> {
        let mut g = self.started.lock().unwrap_or_else(|e| e.into_inner());
        g.push(Asked {
            prompt: prompt.to_string(),
            role: spec.role.clone(),
            placement: spec.placement,
            vm_source: spec.vm_source.clone(),
        });
        Ok(format!("sub-{}", g.len()))
    }

    fn collect(&self, _handle: &str, _timeout: Duration) -> TaskStatus {
        TaskStatus::Unknown
    }
}

/// **Everything a test here starts from**: a temp store, the review queue's session, the daemon's
/// own session, one session this daemon does not hold, and the two runners that record the spawns.
///
/// The daemon's own session is the one `Sessions::open_first` opens eagerly; the queue's session is
/// opened by `Sessions::ensure_gatekeeper` and is the one a review is a child of. The third is
/// created in the registry and never opened — a session this daemon knows about and does not hold.
struct Fixture {
    /// Held, not read: dropping it deletes the store.
    _dir: TempDir,
    db: PathBuf,
    cfg: Config,
    registry: Arc<Registry>,
    /// One entry per `start`, in order, asked of the REVIEW QUEUE's session.
    started: Arc<Mutex<Vec<Asked>>>,
    /// **And what the daemon's OWN session was asked to start**, which must stay empty: a review is
    /// a `task` child of the queue's session, not of whatever session the daemon was started with.
    /// A recording runner on that session is the only way to assert the difference — a runner that
    /// is never asked records nothing whether the code is right or the fixture is.
    started_by_the_daemon: Arc<Mutex<Vec<Asked>>>,
    /// The session that hosts the reviews — the queue's own.
    host: String,
    /// A session that exists and is not held.
    elsewhere: String,
}

impl Fixture {
    /// `None` after announcing the skip, when this box has no vocabulary GGUF.
    fn new(tag: &str) -> Option<Self> {
        let gguf = crate::vocab_gguf()?;
        let dir = TempDir::new(tag);
        let db = dir.path().join("sessions.db");
        let mut cfg = Config::for_this_box(dir.path());
        cfg.vocab_gguf = Some(gguf);
        // **A temp store, never the daemon's own**: the queue lives in `sessions.db`, and a test
        // that opened the operator's would be writing rows into a live queue.
        cfg.store = Some(db.clone());
        cfg.session_id = "s-host".into();
        cfg.http_retries = 0;
        // `Registry::new` already hands back the `Arc` the daemon shares.
        let registry = Registry::new();
        registry
            .create(&cfg.session_id, "", Sessions::wiring(&cfg))
            .expect("the held session registers");
        registry
            .create("s-elsewhere", "", Sessions::wiring(&cfg))
            .expect("a session this daemon does not hold registers");
        Some(Self {
            _dir: dir,
            db,
            cfg,
            registry,
            started: Arc::new(Mutex::new(Vec::new())),
            started_by_the_daemon: Arc::new(Mutex::new(Vec::new())),
            // **The review queue's session**, hardcoded (`gatekeeper::SESSION`): the one the door
            // writes on the row and the one the reviewer is a child of.
            host: crate::sessions::gatekeeper::SESSION.to_string(),
            elsewhere: "s-elsewhere".into(),
        })
    }

    /// The store, opened fresh — a connection that did not write the rows.
    fn store(&self) -> Store {
        Store::open(&self.db).expect("the store reopens")
    }

    /// One queue entry, `Waiting`, with the brief the reviewer is given.
    fn entry(&self, id: &str, brief: &str) {
        self.entry_working_in(id, brief, None)
    }

    /// **The same row, with the worktree the branch is checked out in** — which is what says
    /// which repository the review is about: `mergequeue::review_copy_source` reads it, so a
    /// reviewer's VM is booted on the repository rather than on the daemon's workspace.
    fn entry_working_in(&self, id: &str, brief: &str, worktree: Option<&std::path::Path>) {
        let now = (crate::config::now_ns() / 1_000_000) as u64;
        self.store()
            .put_merge_entry(&MergeEntry {
                id: id.into(),
                session_id: self.host.clone(),
                branch: format!("agent/{id}"),
                base_sha: "base".into(),
                priority: MergePriority::Subagent,
                needs: Vec::new(),
                state: MergeState::Waiting,
                brief: brief.into(),
                evidence: String::new(),
                created_ms: now,
                updated_ms: now,
                worktree: worktree.map(|w| w.display().to_string()),
                landed_sha: None,
                gate_steps: Vec::new(),
            })
            .expect("the entry row");
    }

    /// **A repository UNDER this daemon's workspace**, and its path back.
    ///
    /// The fixture's workspace is deliberately not a repository — it is the temp dir — so this is
    /// what makes *the workspace* and *the repository* two different directories, which is the
    /// shape of the operator's own box (`--workspace /home/dead/Projects/letibot`, whose
    /// repository is `/home/dead/Projects/letibot/letibot`) and the whole of what a copy source
    /// has to get right.
    ///
    /// Canonicalized on the way back, because that is what `git rev-parse
    /// --path-format=absolute` reports and the assertion is about the path rather than about a
    /// symlinked `/tmp`.
    fn a_repository_under_the_workspace(&self, name: &str) -> PathBuf {
        let repo = self.cfg.workspace.join(name);
        std::fs::create_dir_all(&repo).expect("the repository's directory");
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q", "-b", "main"])
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git init in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::canonicalize(&repo).expect("the repository canonicalizes")
    }

    /// The request row `mergequeue::GatekeeperDoor` writes: naming the HOST, with no verdict on it.
    /// `attempts`/`failed_ms` are the store's half of idempotence and are the caller's to set.
    fn review(&self, entry_id: &str, host: &str, attempts: u32, failed_ms: Option<u64>) {
        self.store()
            .put_review(&ReviewRecord {
                entry_id: entry_id.into(),
                session_id: host.into(),
                branch: format!("agent/{entry_id}"),
                base_sha: "base".into(),
                asked_ms: (crate::config::now_ns() / 1_000_000) as u64,
                answered_ms: None,
                decision: None,
                attempts,
                failed_ms,
                failure: String::new(),
                reasons: Vec::new(),
                files: Vec::new(),
                commands: Vec::new(),
            })
            .expect("the request row");
    }

    /// **The daemon**: `Sessions::open_first` over its own session, the review queue's session
    /// opened beside it, and the recording runners installed in both harnesses.
    ///
    /// The runners are installed after the opens because `Harness::open` builds the real one from
    /// the parts, and a real one would spawn a child that needs a model.
    fn sessions<'p>(&self, parts: &'p Parts) -> Sessions<'p> {
        let mut sessions = Sessions::open_first(parts, self.cfg.clone(), self.registry.clone())
            .expect("the daemon's own session opens");
        // **The review queue's session** — the daemon's own idle arm opens it the same way, on its
        // first pass (`Sessions::ensure_gatekeeper`), and this is the session a review belongs to.
        sessions
            .ensure_gatekeeper()
            .expect("the review queue's session opens");
        sessions
            .open
            .get_mut(&self.host)
            .expect("the review queue's session is open")
            .set_subagents_for_test(Arc::new(Recording {
                started: self.started.clone(),
            }));
        sessions
            .open
            .get_mut(&self.cfg.session_id)
            .expect("the daemon's own session is open")
            .set_subagents_for_test(Arc::new(Recording {
                started: self.started_by_the_daemon.clone(),
            }));
        sessions
    }

    /// What the review queue's session was asked to start, in order.
    fn asked(&self) -> Vec<Asked> {
        self.started
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// **And what the daemon's own session was asked to start** — nothing, in every test here.
    fn asked_by_the_daemon(&self) -> Vec<Asked> {
        self.started_by_the_daemon
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// **A `Due` review is spawned as the REVIEW QUEUE's own `task` child — and nothing rings
/// anything.**
///
/// This is the defect in its plainest form: the row is in the store, the session that reviews it
/// has no turn running and no wake coming, and before the pass existed nothing ever looked. The
/// child is the review's own — the brief is the entry's, and the seat is `gatekeeper` — so what is
/// asserted is not *a child started* but *the right child started, for the right row, by the right
/// session*: the row names the queue's session (`mergequeue::GatekeeperDoor` writes that id), the
/// pass spawns through THAT harness's runner, and the daemon's own session is asked for nothing.
#[test]
fn a_due_review_is_spawned_as_the_queue_sessions_own_child() {
    let Some(f) = Fixture::new("due") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.entry("m-mine", "make the reader keep the last record");
    f.review("m-mine", &f.host, 0, None);

    let mut sessions = f.sessions(&parts);
    // **No wake, no ring, no turn**: the pass is the whole of the trigger, which is what makes
    // this the recovery for a daemon that came up between the ask and the bell.
    let started = sessions.serve_reviews();

    assert_eq!(
        started,
        1,
        "a review this session hosts and nobody is serving is started by the pass: {:?}",
        f.asked()
    );
    let asked = f.asked();
    assert_eq!(asked.len(), 1, "one row, one reviewer: {asked:?}");
    let Asked { prompt, role, .. } = &asked[0];
    assert_eq!(
        role, "gatekeeper",
        "the reviewer runs in the gatekeeper seat — read-only, with `bash` for `git diff`"
    );
    assert!(
        prompt.starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX),
        "the child's title is cut from the brief's first line and the head hides it by that \
         mark, so the prompt has to begin with it: {prompt}"
    );
    assert!(
        prompt.contains("make the reader keep the last record"),
        "the review is given the ENTRY's brief — the reviewer reads the branch against the ask \
         and has no other source for it: {prompt}"
    );
    // **AND IT IS THE QUEUE SESSION'S CHILD, which is the half this slice moves.** The row names
    // `s-gatekeeper` and `Harness::serve_reviews` spawns through `self.subagents` — the same runner
    // the `task` tool uses, whose `base.session_id` is the parent — so *whose runner was asked* is
    // *whose child this is*. The daemon's own session has a runner too, and it was asked for
    // nothing: a review is not a child of whatever session the daemon was started with.
    assert_eq!(
        f.host,
        crate::sessions::gatekeeper::SESSION,
        "the row is the review queue's session's"
    );
    assert!(
        f.asked_by_the_daemon().is_empty(),
        "the daemon's own session was asked to start NOTHING — the review is the queue's child, \
         not the daemon's: {:?}",
        f.asked_by_the_daemon()
    );

    // And the row is still outstanding: the child is running, so nobody has judged anything.
    let row = f
        .store()
        .merge_review("m-mine")
        .expect("reads")
        .expect("the row");
    assert!(
        row.answered_ms.is_none() && row.decision.is_none(),
        "a child that is running is not a verdict: {row:?}"
    );
}

/// **A review already being served is not spawned twice by a second pass.**
///
/// The store cannot tell the two apart — a request row and an attempt in flight are the same row,
/// `decision` NULL and `attempts` 0, and `review_retry` says `Due` for both on purpose, because an
/// outstanding review is re-asked rather than counted. So the second pass sees exactly what the
/// first one saw, and the guard that answers it is the session's own review desk. This is
/// that guard, measured through the door the daemon uses — and it is the guard the queue session's
/// own prompt leans on when it tells its model NOT to start a reviewer of its own.
#[test]
fn a_review_already_being_served_is_not_spawned_twice() {
    let Some(f) = Fixture::new("twice") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.entry("m-mine", "keep the last record");
    f.review("m-mine", &f.host, 0, None);

    let mut sessions = f.sessions(&parts);
    assert_eq!(sessions.serve_reviews(), 1, "the first pass asks");
    assert_eq!(
        sessions.serve_reviews(),
        0,
        "the second pass sees the same row — the store has not changed — and starts nothing: \
         {:?}",
        f.asked()
    );
    assert_eq!(f.asked().len(), 1, "one gatekeeper, not two");

    // **And nothing was written onto the row either.** A pass that re-stamped the row would be
    // the other way to make the second pass quiet, and it would be a lie: the attempt is in
    // flight, not failed, and `asked_ms` is when the review was FIRST asked for.
    let row = f
        .store()
        .merge_review("m-mine")
        .expect("reads")
        .expect("the row");
    assert_eq!(row.attempts, 0, "no attempt has failed: {row:?}");
    assert!(row.failed_ms.is_none(), "so nothing is stamped: {row:?}");
    assert!(row.failure.is_empty(), "and there is no failure to read");
}

/// **The reviewer's VM is booted on the ENTRY's repository, and not on this session's
/// workspace.**
///
/// The defect this is written against is the one that stopped the queue: a session may be seated
/// a level ABOVE its repositories — the operator's own daemon is (`--workspace
/// /home/dead/Projects/letibot`, whose repository is `/home/dead/Projects/letibot/letibot`) — and
/// the copy source was filled from the session's workspace, so a reviewer's VM was booted on a
/// copy of the whole folder: 72.8 GiB of siblings, worktrees and build output, refused by
/// `copy_tree`'s 8 GiB ceiling three times in one night for two branches. A reviewer that cannot
/// start is written as a FAILED ATTEMPT on its row, so nothing in the queue could land.
///
/// **The path is asserted and not the words.** `vm_source` is exactly what `Harness::open` hands
/// `FirecodeSpec::new` (`Config::vm_copy_source`), so this is the value the copy is made from;
/// and the repository is a different directory from the workspace here by construction, because
/// the fixture's workspace is a temp dir that is in no repository at all.
#[test]
fn a_reviewer_copies_the_entries_repository_and_not_the_workspace() {
    let Some(f) = Fixture::new("copy-source") else {
        return;
    };
    let repo = f.a_repository_under_the_workspace("the-repo");
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.entry_working_in("m-in-repo", "keep the last record", Some(&repo));
    f.review("m-in-repo", &f.host, 0, None);

    let mut sessions = f.sessions(&parts);
    assert_eq!(sessions.serve_reviews(), 1, "the pass asks");
    let asked = f.asked();
    assert_eq!(asked.len(), 1, "one row, one reviewer: {asked:?}");
    assert_eq!(
        asked[0].vm_source.as_deref(),
        Some(repo.as_path()),
        "a reviewer reads a branch, so the tree its VM is booted on is the entry's own \
         repository — the one `mergequeue::review_copy_source` resolves from the worktree"
    );
    assert_ne!(
        asked[0].vm_source.as_deref(),
        Some(f.cfg.workspace.as_path()),
        "and NOT the daemon's workspace, which is the folder that repository sits in: copying it \
         is the 72.8 GiB the ceiling refuses, and the review then never starts"
    );
}

/// **The reviewer is placed where this box can place one, and the state is DECLARED where it
/// cannot — never a silent fallback to the machine the session asked to leave.**
///
/// This is the placement decided FOR a child rather than by the call that spawns it, and it is not
/// a detail: a reviewer on this machine seats `bash`, so every call it makes is a permission card,
/// and a card with no head attached times out and FAILS the review. So the assertion is that what
/// the pass asked for follows the box — `Firecode` here where firecode is installed, `Host` where
/// it is not — and that a session which states no VM gets none.
///
/// **The two arms are the two answers a test can give, and the order is the point.** The first
/// fixture ASKS for a VM, by name (`Config::allow_vm_reviewers`), and follows the box's own answer
/// read from `firecode_available()` — the same call the pass makes, so nothing here races
/// `$FIRECODE_BIN` with a guess about what is installed. The second is a fixture built the ORDINARY
/// way, with no line asking for anything, and it must come out `Host` even on a box that could boot
/// a VM: that is the property `Config::for_this_box` carries, and it is what stops a test that
/// drives a real review from booting firecracker over its workspace. The three-way state itself —
/// including *asked for a VM, box cannot give one* — is asserted as pure arms in `gatekeeper`'s own
/// tests, where the box is an argument.
#[test]
fn a_review_child_is_placed_where_the_box_can_place_one() {
    let Some(mut f) = Fixture::new("placed") else {
        return;
    };
    // **This test ASKS for the VM, by name.** A fixture built the ordinary way states no VM, so
    // this is the half that says *a test which means to exercise a VM placement still can*; the
    // other half below is the ordinary fixture, which must come out `Host` whatever this box can
    // boot.
    f.cfg.allow_vm_reviewers();
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.entry("m-placed", "keep the last record");
    f.review("m-placed", &f.host, 0, None);

    let mut sessions = f.sessions(&parts);
    assert_eq!(sessions.serve_reviews(), 1, "the pass asks");
    let asked = f.asked();
    assert_eq!(asked.len(), 1, "one row, one reviewer: {asked:?}");
    // **And the copy source is the child's own workspace here, which is what every build before
    // `vm_source` did.** The entry carries no worktree and the fixture's workspace is in no
    // repository, so nothing better can be named — and the small workspace is exactly the case
    // the old behaviour was right for.
    assert!(
        asked[0].vm_source.is_none(),
        "an entry with no worktree to read a repository from keeps the old copy source — the \
         child's own workspace: {asked:?}"
    );
    match letibot_tools::firecode::firecode_available() {
        Ok(bin) => assert_eq!(
            asked[0].placement,
            Placement::Firecode,
            "firecode is installed at {} — the reviewer runs in a VM, where its `bash` calls are \
             not permission cards",
            bin.display()
        ),
        Err(why) => assert_eq!(
            asked[0].placement,
            Placement::Host,
            "this box cannot boot a VM ({why}), so the reviewer runs here — the declared state, \
             said in the session's own prompt, and not a `Firecode` this box would fail to honour"
        ),
    }

    // **And the ORDINARY fixture — no line asking for a VM — keeps its reviewer here**, whatever
    // this box can do. That is the property the constructor carries: `Config::for_this_box` is what
    // every fixture in this tree is built from, and it states no VM, so a test that drives a real
    // review cannot boot firecracker over its workspace by forgetting a line.
    let Some(f) = Fixture::new("placed-host") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.entry("m-here", "keep the last record");
    f.review("m-here", &f.host, 0, None);

    let mut sessions = f.sessions(&parts);
    assert_eq!(sessions.serve_reviews(), 1, "the pass still asks");
    let asked = f.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(
        asked[0].placement,
        Placement::Host,
        "a fixture built the ordinary way states no VM, so its reviewer stays on this machine"
    );
    assert!(
        asked[0].vm_source.is_none(),
        "and the same small workspace still places as it did: with no repository to name, the \
         copy source stays the child's own workspace — which for a host placement is not used at \
         all: {asked:?}"
    );
}

/// **A review naming a session this daemon does not hold is left alone.**
///
/// The whole design in one assertion. A ring is a bell and a bell is per-daemon, so a mover in
/// another daemon cannot reach this one — which is why the pass reads the store, and why it serves
/// only what it holds: two daemons each serving their own rows is the arrangement that needs
/// nothing to reach anything. The row is not touched, and no child is started for it.
///
/// **The row the door writes names the review queue's session** (`mergequeue::GatekeeperDoor`),
/// which is the case every other test here covers; this one is the row a daemon must NOT serve, and
/// it stays reachable because the pass asks the STORE which session a row names rather than
/// assuming the id.
#[test]
fn a_review_naming_a_session_this_daemon_does_not_hold_is_left_alone() {
    let Some(f) = Fixture::new("elsewhere") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.entry("m-mine", "mine to review");
    f.entry("m-theirs", "theirs to review");
    f.review("m-mine", &f.host, 0, None);
    f.review("m-theirs", &f.elsewhere, 0, None);

    let mut sessions = f.sessions(&parts);
    assert_eq!(
        sessions.serve_reviews(),
        1,
        "the pass serves what this daemon holds and nothing else"
    );
    let asked = f.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].prompt.contains("mine to review"),
        "the child started is the HELD session's review: {asked:?}"
    );
    assert!(
        !asked[0].prompt.contains("theirs to review"),
        "a review hosted by a session this daemon does not hold is not this daemon's to serve — \
         the daemon that holds `{}` serves it: {asked:?}",
        f.elsewhere
    );

    // And the other row is exactly as it was written.
    let theirs = f
        .store()
        .merge_review("m-theirs")
        .expect("reads")
        .expect("the row");
    assert_eq!(theirs.session_id, f.elsewhere, "still hosted elsewhere");
    assert!(theirs.answered_ms.is_none() && theirs.failed_ms.is_none());
    assert_eq!(theirs.attempts, 0, "nobody touched it: {theirs:?}");
}

/// **A row whose backoff has not elapsed is not asked again** — the store's half of idempotence.
///
/// `mergequeue::review_retry` is the one decision the queue's pass and the host's pass share, and
/// this is the arm the in-memory list cannot cover: a failed attempt drops the entry out of
/// the desk (there is no child any more), so without the store's `attempts`/`failed_ms` every
/// pass would start a new gatekeeper for a row the queue has already decided to wait on. The
/// window is `REVIEW_RETRY_BASE_MS`, and the assertion is both halves of it: not now, and again
/// once the wait is over.
#[test]
fn a_row_whose_backoff_has_not_elapsed_is_not_asked_again() {
    let Some(f) = Fixture::new("backoff") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.entry("m-mine", "keep the last record");
    // One attempt that failed, just now — the row a gatekeeper left behind when its provider
    // answered `429`.
    let now = (crate::config::now_ns() / 1_000_000) as u64;
    f.review("m-mine", &f.host, 1, Some(now));

    let mut sessions = f.sessions(&parts);
    assert_eq!(
        sessions.serve_reviews(),
        0,
        "the wait has not elapsed, so the store says `Wait` and the pass starts nothing: {:?}",
        f.asked()
    );
    assert!(f.asked().is_empty());

    // The wait is over: the same row, and the pass asks again — which is what makes the bound a
    // bound rather than a stop.
    let old = now.saturating_sub(crate::mergequeue::REVIEW_RETRY_BASE_MS);
    f.review("m-mine", &f.host, 1, Some(old));
    assert_eq!(
        sessions.serve_reviews(),
        1,
        "a failed attempt is not the end of the road — the queue asks again after the backoff"
    );
}

/// **A verdict the reviewer HANDED IN moves the entry and takes the row off.**
///
/// The whole of this slice in one measurement, and the difference from the test above is the
/// only thing that matters: the child judged, and it said so ITSELF. The pass starts the reviewer
/// and opens the review on the desk; the reviewer hands its verdict in through the one door it
/// holds (`review_verdict` → `ReviewDesk::submit` — the call below is that door's own function,
/// which is what the tool calls with the arguments it salvaged); and the NEXT pass writes it onto
/// the entry's review row through the same `write_outcome` the parent used to call after parsing
/// the child's answer.
///
/// Four things are asserted, and each is one of the requirements: the store's row carries the
/// reviewer's own words (so the queue's gate can act on it), the entry is no longer waiting, the
/// row is off the session's board, and the review is CLOSED — a second verdict from the same
/// child is refused rather than written twice.
#[test]
fn a_submitted_verdict_moves_the_entry_and_takes_the_row_off() {
    use letibot_tools::gatekeeper::{Decision, LookedAt, Submission};
    let Some(f) = Fixture::new("submitted") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.entry("m-mine", "keep the last record");
    f.review("m-mine", &f.host, 0, None);

    let mut sessions = f.sessions(&parts);
    assert_eq!(sessions.serve_reviews(), 1, "the reviewer is started");
    assert_eq!(
        sessions.fill_gatekeeper_board(),
        1,
        "and the entry is a row on the queue's board while nobody has judged it"
    );

    // **The child's own act.** `sub-1` is the handle the recording runner handed back, which is
    // the reviewer's session id — the name its own `review_verdict` call arrives under.
    let verdict = parts
        .reviews
        .submit(
            "sub-1",
            Submission {
                decision: Decision::Accept,
                reasons: vec!["the test covers it".into()],
                looked_at: LookedAt {
                    files: vec!["src/reader.rs".into()],
                    commands: vec!["git diff base...agent/m-mine".into()],
                },
            },
        )
        .expect("a reviewer of this queue may hand its verdict in");
    assert!(
        verdict.render().contains("agent/m-mine") && verdict.render().contains("accept"),
        "and what it gets back is its own verdict, rendered: {}",
        verdict.render()
    );

    // **And the daemon's own pass writes it** — the same door, the same row, the same store.
    assert_eq!(
        sessions.serve_reviews(),
        0,
        "a review in flight starts no second child: {:?}",
        f.asked()
    );
    let row = f
        .store()
        .merge_review("m-mine")
        .expect("reads")
        .expect("the row");
    assert_eq!(row.decision.as_deref(), Some("accept"), "{row:?}");
    assert!(row.answered_ms.is_some(), "and it is stamped as answered");
    assert_eq!(row.reasons, vec!["the test covers it"]);
    assert_eq!(row.files, vec!["src/reader.rs"]);
    assert_eq!(row.commands, vec!["git diff base...agent/m-mine"]);
    assert_eq!(
        row.attempts, 0,
        "a verdict is not a failed attempt: {row:?}"
    );

    // **And the row comes off the board**, because the entry is not waiting for a verdict any
    // more: the board is a view of the queue and the verdict is what moves the entry.
    assert_eq!(
        sessions.fill_gatekeeper_board(),
        0,
        "the verdict is what takes the row off: {:?}",
        sessions
            .open
            .get(f.host.as_str())
            .expect("open")
            .todo_list()
    );

    // **And the review is judged ONCE.** A second call from the same child is refused by name,
    // which is what stops a retry or a change of mind from writing a second judgement.
    let refused = parts
        .reviews
        .submit(
            "sub-1",
            Submission {
                decision: Decision::Reject,
                reasons: vec!["on reflection".into()],
                looked_at: LookedAt::default(),
            },
        )
        .unwrap_err();
    assert!(
        refused.contains("is not reviewing an entry"),
        "the review is closed with its verdict: {refused}"
    );
    assert_eq!(
        f.store()
            .merge_review("m-mine")
            .expect("reads")
            .expect("the row")
            .decision
            .as_deref(),
        Some("accept"),
        "and the FIRST verdict is the one on the row"
    );
}

/// **A child of some OTHER session cannot submit for this queue.**
///
/// The desk answers one question — *is this session a reviewer of an entry this daemon is waiting
/// for a verdict on* — and it is open only to the children the queue's own session started. A
/// session that was never asked is refused BY NAME, nothing is recorded, and the pass that runs
/// afterwards writes nothing: a verdict that reached a queue from a session the queue never asked
/// would be a judgement nobody commissioned.
///
/// **And the reviewer that WAS asked is unaffected** — the refused call closes nothing, so the
/// same pass still writes the real reviewer's verdict. That is the other half of the same fact:
/// the door is narrow, and it is not narrow enough to shut the child it was opened for.
#[test]
fn a_child_of_another_session_cannot_submit_for_this_queue() {
    use letibot_tools::gatekeeper::{Decision, LookedAt, Submission};
    let Some(f) = Fixture::new("foreign") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    // **A child of a session that is not this queue's** — registered under the session this
    // daemon does not hold, which is the fixture's "somebody else".
    let stranger = "s-elsewhere-sub-1";
    f.registry
        .create_under(
            stranger,
            "a child of another tree",
            Sessions::wiring(&f.cfg),
            Some(f.elsewhere.clone()),
        )
        .expect("the stranger's session registers");

    f.entry("m-mine", "keep the last record");
    f.review("m-mine", &f.host, 0, None);
    let mut sessions = f.sessions(&parts);
    assert_eq!(
        sessions.serve_reviews(),
        1,
        "the queue's own reviewer is started"
    );

    let accept = |why: &str| Submission {
        decision: Decision::Accept,
        reasons: vec![why.into()],
        looked_at: LookedAt::default(),
    };
    let refused = parts
        .reviews
        .submit(stranger, accept("looks fine from over here"))
        .unwrap_err();
    assert!(
        refused.contains(stranger) && refused.contains("is not reviewing an entry"),
        "the refusal names the session and what is wrong: {refused}"
    );
    assert!(
        refused.contains("No verdict was recorded"),
        "and says nothing was recorded: {refused}"
    );

    // **Nothing was written, and nothing moved.** The stranger's word did not become a verdict.
    sessions.serve_reviews();
    let row = f
        .store()
        .merge_review("m-mine")
        .expect("reads")
        .expect("the row");
    assert!(
        row.decision.is_none() && row.answered_ms.is_none() && row.attempts == 0,
        "a submission from a session the queue never asked left the row exactly as it was: {row:?}"
    );
    assert_eq!(
        sessions.fill_gatekeeper_board(),
        1,
        "and the entry is still waiting for a verdict, so its row is still on the board"
    );

    // **And the reviewer that was asked still can.**
    parts
        .reviews
        .submit("sub-1", accept("the test covers it"))
        .expect("the queue's own reviewer may still hand its verdict in");
    sessions.serve_reviews();
    assert_eq!(
        f.store()
            .merge_review("m-mine")
            .expect("reads")
            .expect("the row")
            .decision
            .as_deref(),
        Some("accept"),
        "and that one is written"
    );
}

/// **An answer with no submission is not a verdict.**
///
/// The reviewer's turn ended and it never called `review_verdict`. What the queue records is a
/// FAILED ATTEMPT — `decision` NULL, so `mergequeue::review_gate` does not read a word here as a
/// refusal; `answered_ms` NULL, so the review is still outstanding; the sentence saying what
/// happened; and the attempt counted, so the queue's own bound retries it rather than parking the
/// entry for ever. **The prose it wrote is not read for a decision**, and the fixture's prose
/// carries the exact words the old closing block would have been parsed for: a build that still
/// read it would land this branch, and this test would fail.
#[test]
fn an_answer_with_no_submission_is_not_a_verdict() {
    let Some(f) = Fixture::new("prose") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.entry("m-mine", "keep the last record");
    f.review("m-mine", &f.host, 0, None);

    let mut sessions = f.sessions(&parts);
    // The child that answers in prose instead of handing a verdict in.
    sessions
        .open
        .get_mut(&f.host)
        .expect("the review queue's session is open")
        .set_subagents_for_test(Arc::new(Answered {
            started: f.started.clone(),
            answer: "I read the diff and the test covers it.\n\nverdict: accept\n\
                     reasons: - the test covers it\nfiles: src/reader.rs"
                .into(),
        }));
    assert_eq!(sessions.serve_reviews(), 1, "the reviewer is started");
    // The child has answered by the time the next pass looks.
    assert_eq!(
        sessions.serve_reviews(),
        0,
        "and no second child is started"
    );

    let row = f
        .store()
        .merge_review("m-mine")
        .expect("reads")
        .expect("the row");
    assert!(
        row.decision.is_none(),
        "an answer in prose is not a verdict — `review_gate` reads any word here as a refusal: \
         {:?}",
        row.decision
    );
    assert!(
        row.answered_ms.is_none(),
        "the review is still outstanding: nothing judged the branch"
    );
    assert_eq!(row.attempts, 1, "the attempt is counted");
    assert!(row.failed_ms.is_some(), "and it is stamped");
    assert!(
        row.failure.contains("review_verdict") && row.failure.contains("sub-1"),
        "the sentence names the door that was not used and the child that did not use it: {:?}",
        row.failure
    );
    assert!(
        !row.reasons.iter().any(|r| r.contains("the test covers it")),
        "and nothing the child wrote was copied onto the row as evidence: {:?}",
        row.reasons
    );
    // **And the row STAYS on the board**: the entry is still waiting for a verdict, and the
    // queue's own clock is what asks again (boundedly, by `attempts`/`failed_ms`).
    assert_eq!(
        sessions.fill_gatekeeper_board(),
        1,
        "still waiting for a verdict"
    );
}

/// **A child that dies without submitting writes NOTHING** — the fourth ending, and the one that
/// must not be mistaken for either of the other three.
///
/// A handle this runner no longer knows is a child that is gone: a daemon restarted mid-review,
/// or a child whose thread ended without a word. It is not a judgement (no `decision`) and it is
/// not a failed attempt either (`attempts` stays 0 — nothing was spent), so the entry is exactly
/// as it was: the row stays on the board and the queue starts a new reviewer for it. That is what
/// makes a restart survivable rather than a review that can never be finished.
#[test]
fn a_child_that_dies_without_submitting_writes_nothing() {
    let Some(f) = Fixture::new("gone") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.entry("m-mine", "keep the last record");
    f.review("m-mine", &f.host, 0, None);

    let mut sessions = f.sessions(&parts);
    sessions
        .open
        .get_mut(&f.host)
        .expect("the review queue's session is open")
        .set_subagents_for_test(Arc::new(Gone {
            started: f.started.clone(),
        }));
    assert_eq!(sessions.serve_reviews(), 1, "the reviewer is started");
    // The child is gone by the time the next pass looks, so the review is dropped and nothing is
    // written — and the pass after that starts a REPLACEMENT, because the entry still waits.
    assert_eq!(sessions.serve_reviews(), 1, "a gone child is replaced");

    let row = f
        .store()
        .merge_review("m-mine")
        .expect("reads")
        .expect("the row");
    assert!(
        row.decision.is_none() && row.answered_ms.is_none(),
        "a child that died is not a judgement: {row:?}"
    );
    assert_eq!(
        row.attempts, 0,
        "and it is not a failed attempt either — nothing was spent, so nothing is stamped: {row:?}"
    );
    assert!(
        row.failure.is_empty(),
        "there is no failure to read: {row:?}"
    );
    assert_eq!(
        sessions.fill_gatekeeper_board(),
        1,
        "and the row is exactly where it was"
    );
}

/// **The clock is armed, so the pass is reached at all** — the property the whole change is for.
///
/// The pass is only worth anything if the daemon actually runs it: the worker parks on a condvar
/// until a deadline it was given, and a daemon whose only clock was the plan-check would never
/// look at the store. So the deadline exists from open, it is due at once (a daemon that started
/// between a review's ask and its wake has nothing else), and spending it arms the next one.
#[test]
fn the_daemon_has_a_clock_for_the_store_it_reads() {
    let Some(f) = Fixture::new("clock") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    let mut sessions = f.sessions(&parts);

    assert!(
        sessions.next_idle_at().is_some(),
        "**a daemon that holds a session has a deadline to read the store at** — without it the \
         worker sleeps until somebody presses enter, which is exactly how a review asked for by \
         another daemon (or by a daemon that then restarted) came to sit unspawned"
    );
    assert!(
        sessions.reviews_due(),
        "armed at open to now: the first idle pass looks, rather than waiting a window"
    );
    assert!(
        !sessions.reviews_due(),
        "and spending it arms the next — a pass that took longer than the interval does not fire \
         again at once"
    );
    assert!(
        sessions.next_idle_at().is_some(),
        "the clock never stands down: a review is asked for by another daemon's thread and the \
         only thing the two share is the store"
    );
}
