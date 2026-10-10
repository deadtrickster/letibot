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
//!   answers this is the session's own `reviewing` list, and the assertion is that the second pass
//!   starts nothing and writes nothing.
//! * **A review naming a session this daemon does not hold is left alone.** The row stays as it
//!   was, and no child is started for it: the pass serves what it holds and nothing else.
//! * **A row whose backoff has not elapsed is not asked again** — the other half of idempotence,
//!   and the one the STORE carries (`attempts`/`failed_ms` through `mergequeue::review_retry`).
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
use letibot_tokencore::apparatus::present_gguf;
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
/// answer without it. Every other field of the [`TaskSpec`] is the caller's and has its own test;
/// this one is the pass's own.
#[derive(Debug, Clone)]
struct Asked {
    prompt: String,
    role: String,
    placement: Placement,
}

/// **A runner that records what it was asked to start and answers `Running` for ever.**
///
/// A running child is the interesting one: it stays in the harness's `reviewing` list, which is
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
        let gguf = present_gguf()?;
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
                worktree: None,
                landed_sha: None,
                gate_steps: Vec::new(),
            })
            .expect("the entry row");
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
/// first one saw, and the guard that answers it is the session's own `reviewing` list. This is
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

/// **The reviewer is placed where this box can place one, and the state is DECLARED where it
/// cannot — never a silent fallback to the machine the session asked to leave.**
///
/// This is the placement decided FOR a child rather than by the call that spawns it, and it is not
/// a detail: a reviewer on this machine seats `bash`, so every call it makes is a permission card,
/// and a card with no head attached times out and FAILS the review. So the assertion is that what
/// the pass asked for follows the box — `Firecode` here where firecode is installed, `Host` where
/// it is not — and that a session which says *no VM* gets none.
///
/// **The two arms are stated differently on purpose.** The veto is a VALUE the test sets, so that
/// arm holds on any machine; the box's own answer is read from `firecode_available()`, the same
/// call the pass makes, so the assertion follows the box rather than `$FIRECODE_BIN` being raced
/// with a guess about what is installed. The three-way state itself — including *asked for a VM,
/// box cannot give one* — is asserted as pure arms in `gatekeeper`'s own tests, where the box is an
/// argument.
#[test]
fn a_review_child_is_placed_where_the_box_can_place_one() {
    let Some(f) = Fixture::new("placed") else {
        return;
    };
    let parts = Parts::load(&f.cfg).expect("the vocabulary must load");
    f.entry("m-placed", "keep the last record");
    f.review("m-placed", &f.host, 0, None);

    let mut sessions = f.sessions(&parts);
    assert_eq!(sessions.serve_reviews(), 1, "the pass asks");
    let asked = f.asked();
    assert_eq!(asked.len(), 1, "one row, one reviewer: {asked:?}");
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

    // **And a session that states no VM keeps its reviewer here**, whatever the box can do — the
    // behaviour every review had before the placement existed, stated as a value so this arm is
    // asserted on a box with firecode and on one without.
    let Some(mut f) = Fixture::new("placed-host") else {
        return;
    };
    f.cfg.gatekeeper_vm = false;
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
        "a session that states `gatekeeper_vm = false` keeps its reviewer on this machine"
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
/// `reviewing` (there is no child any more), so without the store's `attempts`/`failed_ms` every
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
