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
//! * **A `Due` review whose host is idle is spawned by the pass, with no wake involved.** Nothing
//!   rings anything in this file: the row is written, the pass is called, and a gatekeeper is
//!   started with the review's brief and the `gatekeeper` seat.
//! * **A review already being served is not spawned twice by a second pass.** The store still says
//!   `Due` — the row is a request and an attempt in flight is the same row — so the guard that
//!   answers this is the session's own `reviewing` list, and the assertion is that the second pass
//!   starts nothing and writes nothing.
//! * **A review hosted by a session this daemon does not hold is left alone.** The row stays as it
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
//! one held harness is handed a recording runner (`Harness::set_subagents_for_test`) — the
//! measurement is *what the pass asked for*, not what a child then did.
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
use letibot_tools::builtins::task::{TaskRunner, TaskSpec, TaskStatus};

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

/// **A runner that records what it was asked to start and answers `Running` for ever.**
///
/// A running child is the interesting one: it stays in the harness's `reviewing` list, which is
/// the state the second-pass test needs, and it never writes a verdict onto a row.
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
        TaskStatus::Running { note: None }
    }
}

/// **Everything a test here starts from**: a temp store, one session this daemon holds, one it
/// does not, and the runner that records the spawns.
///
/// The held session is the one `Sessions::open_first` opens eagerly. The other is created in the
/// registry and never opened — a session this daemon knows about and does not hold, which is the
/// state a review's host is in when it belongs to another daemon.
struct Fixture {
    /// Held, not read: dropping it deletes the store.
    _dir: TempDir,
    db: PathBuf,
    cfg: Config,
    registry: Arc<Registry>,
    started: Arc<Mutex<Vec<(String, String)>>>,
    /// The session this daemon holds — the host the pass serves.
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
            host: "s-host".into(),
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

    /// **The daemon**: `Sessions::open_first` over the held session, with the recording runner
    /// installed in the one harness it holds.
    ///
    /// The runner is installed after the open because `Harness::open` builds the real one from the
    /// parts, and a real one would spawn a child that needs a model.
    fn sessions<'p>(&self, parts: &'p Parts) -> Sessions<'p> {
        let mut sessions = Sessions::open_first(parts, self.cfg.clone(), self.registry.clone())
            .expect("the held session opens");
        let held = sessions
            .open
            .get_mut(&self.host)
            .expect("the daemon holds the session it opened");
        held.set_subagents_for_test(Arc::new(Recording {
            started: self.started.clone(),
        }));
        sessions
    }

    /// What the pass asked for, in order.
    fn asked(&self) -> Vec<(String, String)> {
        self.started
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// **A `Due` review whose host is idle is spawned by the pass — and nothing rings anything.**
///
/// This is the defect in its plainest form: the row is in the store, the host has no turn running
/// and no wake coming, and before the pass existed nothing ever looked. The child is the review's
/// own — the brief is the entry's, and the seat is `gatekeeper` — so what is asserted is not *a
/// child started* but *the right child started for the right row*.
#[test]
fn a_due_review_for_an_idle_host_is_spawned_by_the_pass() {
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
    assert_eq!(asked.len(), 1, "one row, one gatekeeper: {asked:?}");
    let (prompt, role) = &asked[0];
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
/// that guard, measured through the door the daemon uses.
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

/// **A review hosted by a session this daemon does not hold is left alone.**
///
/// The whole design in one assertion. A ring is a bell and a bell is per-daemon, so a mover in
/// another daemon cannot reach this one — which is why the pass reads the store, and why it serves
/// only what it holds: two daemons each serving their own rows is the arrangement that needs
/// nothing to reach anything. The row is not touched, and no child is started for it.
#[test]
fn a_review_hosted_elsewhere_is_left_alone() {
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
        asked[0].0.contains("mine to review"),
        "the child started is the HELD session's review: {asked:?}"
    );
    assert!(
        !asked[0].0.contains("theirs to review"),
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
