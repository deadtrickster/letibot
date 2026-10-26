//! **Review and merge are two queues** (R61).
//!
//! The operator: *"review is review and merge is merge. separate steps. separate queues."*,
//! *"model reviews stay, yes"*, and *"the merge/review queue is a center-piece and it was never
//! reliable so far."* The v23 queue was one table in which the verdict was a state of the merge
//! row; the review of 2026-10-11 (TODO.md R61) ranked twelve defects in it, and the modules here
//! are written against them:
//!
//! * **[`settings`]** — the per-project `[queue]` table: the target branch (decision 12), the
//!   option to merge via the project's main session when a review cannot run (decision 4), and
//!   what a red target sets in motion (decision 11).
//! * **[`lease`]** — this daemon's identity on the queues; the lease itself is a store row claimed
//!   by compare-and-swap (`Store::acquire_queue_lease`), one per repository (decisions 1, 2).
//! * **[`gate`]** — the target's own `## Merge gate`, run by a [`gate::GateRunner`] (R62's seam)
//!   with a thirty-minute deadline in a killable process group (decision 9).
//! * **[`git`]** — the landing in a DEDICATED worktree (in letibot's data directory, never in
//!   the repository's `.git`), a compare-and-swap on the target's ref, the push, and the fresh
//!   clone a VM reviewer is booted on.
//! * **[`carry`]** — the gitignored inputs a project's gate needs (`[queue] gate_carry`), copied
//!   into the landing tree, and the sentence a failed gate's notice adds when they are missing.
//! * **[`capacity`]** — a reviewer firecode could not place for want of a free VM slot: a wait,
//!   not an attempt; and the words a review that stops for a person says about why.
//! * **[`serve`]** — the queues' own threads: a supervisor per daemon and a worker per
//!   repository whose lease it holds. The review queue's upkeep (deadlines, claims, raises), the
//!   target's health (gated on every change, coalesced — decision 10), and the landings.
//! * **[`review`]** — the reviewers, which run on the review queue's SESSION as `task` children,
//!   their verdicts bound to the head they judged (decision 6), and the enqueue.
//! * **[`verbs`]** — the operator's `/queue` verbs, approve-over among them (decision 5).
//! * **[`desk`]** — the `queue` tool's daemon half: what a session's model can do about its
//!   project's queues, and the target it may only propose (R76 item 2).
//! * **[`notices`]** — what a project's model is told when its target is red, unjudged or timed
//!   out, or a row is parked for a person (R76 item 1).
//! * **[`setup`]** — a gate that could not judge (its own setup failed after the build and the
//!   tests passed) told apart from a target that is red (R76 item 5).
//!
//! The store's half — the tables, the conditional moves, the history, the lease rows, the
//! target's health and the v24 migration — is `letibot_tokencore::store::queues`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::{MergeRow, ReviewRow, Store, StoreError};

pub mod capacity;
pub mod carry;
pub mod desk;
pub mod gate;
pub mod git;
pub mod lease;
pub mod notices;
pub mod review;
pub mod serve;
pub mod settings;
pub mod setup;
pub mod verbs;
pub mod watch;

pub use git::{repo_of_workspace, repo_of_worktree, repo_root};

/// **The clause a row waiting for its target to declare a gate leads with** — the project's
/// session is told to propose one (`Harness::queue_notices`).
pub const WAITING_FOR_GATE: &str = "waiting for a merge gate";

/// **The clause a blocked review leads with once it has been handed to its project's main
/// session** (decision 4's per-project option) — read off the row so the project is told once.
pub const HANDED_TO_PROJECT: &str = "handed to the project's main session";

/// **The root of the session tree `session` is in** — the project's main session, which a
/// refusal falls back to and which decision 4's option hands a blocked review to.
pub fn root_session(store: &Store, session: &str) -> String {
    let mut cur = session.to_string();
    for _ in 0..32 {
        let parent = store.session_parent(&cur).ok().flatten().flatten();
        match parent {
            Some(p) if !p.is_empty() => cur = p,
            _ => break,
        }
    }
    cur
}

fn job_name(what: &str, about: &str, id: &str) -> String {
    let about = if about.trim().is_empty() { id } else { about };
    letibot_tools::exec::job_name(&format!("{what}-{}", about.trim()))
        .unwrap_or_else(|| what.to_string())
}

/// The daemon's name for a review run — the board row's words and the log's.
pub fn review_job_name(r: &ReviewRow) -> String {
    job_name("review", &r.branch, &r.id)
}

/// The daemon's name for a landing's gate run.
pub fn gate_job_name(m: &MergeRow) -> String {
    job_name("gate", &m.branch, &m.id)
}

// ===== A store error is weather, not the end (R67 item 12, kept) =====

/// The first wait after a store error, doubled per consecutive failure up to [`STORE_RETRY_CAP`].
pub const STORE_RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(1);
/// The longest wait between two attempts.
pub const STORE_RETRY_CAP: std::time::Duration = std::time::Duration::from_secs(60);

/// **A run of store errors** — counted, said on the hubs once per reason, and ended out loud.
#[derive(Default)]
pub(crate) struct StoreTrouble {
    failures: u32,
    said: Option<String>,
}

impl StoreTrouble {
    pub(crate) fn failed(
        &mut self,
        doing: &str,
        e: &StoreError,
        door: &dyn serve::QueueDoor,
    ) -> std::time::Duration {
        self.failures = self.failures.saturating_add(1);
        let wait = store_retry_wait(self.failures);
        let why = e.to_string();
        eprintln!(
            "  merge queue: {doing} failed ({why}) — attempt {} failed, trying again in {}s",
            self.failures,
            wait.as_secs()
        );
        if self.said.as_deref() != Some(why.as_str()) {
            door.announce(SessionEvent::Warning {
                code: "merge_queue_store_error".into(),
                detail: format!(
                    "the queues could not read or write their store while {doing}: {why}. Nothing \
                     moved; they keep trying (the wait doubles up to {}s) and say so when they are \
                     serving again.",
                    STORE_RETRY_CAP.as_secs()
                ),
                compaction: None,
            });
            self.said = Some(why);
        }
        wait
    }

    pub(crate) fn passed(&mut self, door: &dyn serve::QueueDoor) {
        if self.said.take().is_some() {
            door.announce(SessionEvent::Warning {
                code: "merge_queue_recovered".into(),
                detail: format!(
                    "the queues are serving again, after {} failed attempt(s) to reach their store.",
                    self.failures
                ),
                compaction: None,
            });
        }
        self.failures = 0;
    }
}

pub(crate) fn store_retry_wait(failures: u32) -> std::time::Duration {
    let mut wait = STORE_RETRY_FIRST;
    for _ in 1..failures {
        wait = (wait * 2).min(STORE_RETRY_CAP);
    }
    wait
}

pub(crate) fn sleep_unless_stopped(wait: std::time::Duration, stop: &AtomicBool) {
    let until = std::time::Instant::now() + wait;
    while !stop.load(Ordering::Relaxed) {
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(std::time::Duration::from_millis(250)));
    }
}

// ===== The snapshot a head's pane draws =====

/// How many moves the snapshot carries.
pub const RECENT_MOVES: usize = 60;

/// **Both queues, as the wire carries them** — every row, the leases, the targets' health and
/// the last moves. A table that could not be read is said in `unreadable` and is never an empty
/// list (R61 defect 12).
pub fn snapshot(store: &Store) -> letibot_sessionlog::event::QueueSnapshot {
    use letibot_sessionlog::event as w;
    let mut out = w::QueueSnapshot {
        at_ms: gate::now_ms(),
        ..Default::default()
    };
    let mut unread: Vec<String> = Vec::new();
    match store.review_rows(None) {
        Ok(rows) => {
            out.reviews = rows
                .into_iter()
                .map(|r| w::QueueReview {
                    id: r.id,
                    repository: r.repository,
                    target: r.target,
                    kind: r.kind.as_str().into(),
                    session_id: r.session_id,
                    branch: r.branch,
                    state: r.state.as_str().into(),
                    owner: r.owner,
                    reviewer: r.reviewer,
                    head_sha: r.head_sha,
                    reviewed_sha: r.reviewed_sha,
                    verdict: r.verdict,
                    verdict_by: r.verdict_by,
                    attempts: r.attempts,
                    failure: r.failure,
                    reasons: r.reasons,
                    evidence: r.evidence,
                    deadline_ms: r.deadline_ms,
                    fixes: r.fixes,
                    created_ms: r.created_ms,
                    state_ms: r.state_ms,
                    brief: r.brief,
                })
                .collect()
        }
        Err(e) => unread.push(format!("the review queue: {e}")),
    }
    match store.merge_rows(None) {
        Ok(rows) => {
            out.merges = rows
                .into_iter()
                .map(|m| w::QueueMerge {
                    id: m.id,
                    review_id: m.review_id,
                    repository: m.repository,
                    target: m.target,
                    branch: m.branch,
                    state: m.state.as_str().into(),
                    owner: m.owner,
                    approved_sha: m.approved_sha,
                    approved_by: m.approved_by,
                    rebased_sha: m.rebased_sha,
                    landed_sha: m.landed_sha,
                    pushed: m.pushed,
                    evidence: m.evidence,
                    gate_steps: m.gate_steps.iter().map(wire_gate_step).collect(),
                    fixes: m.fixes,
                    created_ms: m.created_ms,
                    state_ms: m.state_ms,
                })
                .collect()
        }
        Err(e) => unread.push(format!("the merge queue: {e}")),
    }
    match store.queue_leases() {
        Ok(ls) => {
            out.leases = ls
                .into_iter()
                .map(|l| w::QueueLeaseView {
                    repository: l.repository,
                    owner: l.owner,
                    pid: l.pid,
                    host: l.host,
                    build: l.build,
                    heartbeat_ms: l.heartbeat_ms,
                    acquired_ms: l.acquired_ms,
                })
                .collect()
        }
        Err(e) => unread.push(format!("the leases: {e}")),
    }
    match store.target_healths() {
        Ok(hs) => {
            out.targets = hs
                .into_iter()
                .map(|h| w::TargetHealthView {
                    repository: h.repository,
                    target: h.target,
                    sha: h.sha,
                    state: h.state.as_str().into(),
                    checked_ms: h.checked_ms,
                    evidence: h.evidence,
                    gate_steps: h.gate_steps.iter().map(wire_gate_step).collect(),
                })
                .collect()
        }
        Err(e) => unread.push(format!("the targets' health: {e}")),
    }
    match store.recent_transitions(RECENT_MOVES) {
        Ok(ts) => {
            out.recent = ts
                .into_iter()
                .map(|t| w::QueueMove {
                    queue: t.queue,
                    entry_id: t.entry_id,
                    repository: t.repository,
                    from: t.from_state,
                    to: t.to_state,
                    at_ms: t.at_ms,
                    by: t.by,
                    why: t.why,
                })
                .collect()
        }
        Err(e) => unread.push(format!("the history: {e}")),
    }
    match store.archived_v23_queue() {
        Ok(a) => out.archived = a.len(),
        Err(e) => unread.push(format!("the v23 archive: {e}")),
    }
    if !unread.is_empty() {
        out.unreadable = Some(format!(
            "could not be read, so the pane is NOT showing it (empty here means unread, not \
             empty): {}",
            unread.join("; ")
        ));
    }
    out
}

/// One gate step, as the wire spells it.
pub fn wire_gate_step(
    s: &letibot_tokencore::store::MergeGateStep,
) -> letibot_sessionlog::event::MergeGateStep {
    use letibot_sessionlog::event::MergeGateOutcome as W;
    use letibot_tokencore::store::MergeGateOutcome as S;
    letibot_sessionlog::event::MergeGateStep {
        command: s.command.clone(),
        outcome: match s.outcome {
            S::Passed => W::Passed,
            S::Failed => W::Failed,
            S::NotRun => W::NotRun,
            S::NoGate => W::NoGate,
            S::TimedOut => W::TimedOut,
            S::Killed => W::Killed,
        },
        output: s.output.clone(),
        started_ms: s.started_ms,
        elapsed_ms: s.elapsed_ms,
    }
}

// ===== The production door, and the startup =====

/// **The daemon's door**: every hub for announcements, the bell for the review queue's session
/// and a project's main session.
pub struct RegistryDoor {
    pub registry: Arc<letibot_sessionlog::registry::Registry>,
    pub store_path: std::path::PathBuf,
}

impl serve::QueueDoor for RegistryDoor {
    fn announce(&self, event: SessionEvent) {
        self.registry.broadcast(event);
    }

    fn announce_to(&self, repository: &str, event: SessionEvent) {
        publish_for_repository(&self.registry, Some(&self.store_path), repository, event);
    }

    fn ring_reviewers(&self) {
        let host = crate::sessions::gatekeeper::SESSION;
        if !self.registry.bell().is_closed() && self.registry.get(host).is_some() {
            self.registry.bell().ring_wake(host);
        }
    }

    fn ring_project(&self, session_id: &str) -> Result<String, String> {
        let store = Store::open(&self.store_path).map_err(|e| e.to_string())?;
        let host = root_session(&store, session_id);
        if host.is_empty() {
            return Err("the row names no session".into());
        }
        if self.registry.get(&host).is_none() {
            match self.registry.resumable(&host) {
                Some(brief) => {
                    self.registry
                        .create(&host, &brief.title, brief.wiring)
                        .map_err(|e| format!("`{host}` could not be resumed: {e}"))?;
                }
                None => {
                    return Err(format!(
                        "the project session `{host}` is in neither this daemon nor the store"
                    ));
                }
            }
        }
        if self.registry.bell().is_closed() {
            return Err("the daemon is shutting down".into());
        }
        self.registry.bell().ring_wake(&host);
        Ok(format!("the project session `{host}` was rung"))
    }

    fn session_live(&self, session_id: &str) -> bool {
        self.registry.get(session_id).is_some()
    }

    fn ring_repository(&self, repository: &str) -> Result<String, String> {
        let store = Store::open(&self.store_path).map_err(|e| e.to_string())?;
        if self.registry.bell().is_closed() {
            return Err("the daemon is shutting down".into());
        }
        let roots = self.roots_of(&store, repository);
        if roots.is_empty() {
            return Err(format!(
                "no root session of {repository} is open in this daemon; the next one that wakes \
                 there reads it"
            ));
        }
        for r in &roots {
            self.registry.bell().ring_wake(r);
        }
        Ok(format!("rang {}", roots.join(", ")))
    }
}

impl RegistryDoor {
    /// **The root sessions this daemon holds whose workspace is `repository`** — not the review
    /// queue's own session, and not a subagent (its root is the one that hears).
    fn roots_of(&self, store: &Store, repository: &str) -> Vec<String> {
        self.registry
            .held_ids()
            .into_iter()
            .filter(|id| id != crate::sessions::gatekeeper::SESSION)
            .filter(|id| self.registry.parent_of(id).is_none())
            .filter(|id| !matches!(store.session_parent(id), Ok(Some(Some(p))) if !p.is_empty()))
            .filter(|id| {
                store.session_workspace(id).ok().flatten().is_some_and(|w| {
                    repositories_of_workspace(Path::new(&w))
                        .iter()
                        .any(|r| r == repository)
                })
            })
            .collect()
    }
}

/// **Start the queues for a daemon** — `None` with no `--store` (the queues live in the store),
/// and `None` under `--no-queue` ([`crate::config::Config::serve_queues`]).
///
/// The watcher is spawned in both cases and the supervisor only without the flag: a daemon that
/// serves no queue still has heads that must redraw when another daemon moves a row (`watch`),
/// and it is the claiming of a lease, the gating of a target and the landing of a branch that
/// `--no-queue` takes away. A package worker's daemon is one of the repository's session daemons
/// by the queue's own rule, so it would take the lease and tell its model to fix `main`
/// (2026-10-26); it must not.
pub fn spawn_for(
    cfg: &crate::config::Config,
    stop: Arc<AtomicBool>,
    registry: Arc<letibot_sessionlog::registry::Registry>,
) -> Option<JoinHandle<()>> {
    let store_path = cfg.store.clone()?;
    let env = serve::QueueEnv::production(
        Arc::new(RegistryDoor {
            registry: registry.clone(),
            store_path: store_path.clone(),
        }),
        cfg.landing_root.clone(),
    );
    // **Every daemon hears every repository's moves** (R92 item 5): the worker below announces
    // only what THIS daemon moves, so a head attached here would never re-read a queue another
    // daemon serves. The watcher reads the shared store's history and says the rest.
    {
        let registry = registry.clone();
        if let Err(e) = watch::spawn(
            store_path.clone(),
            lease::this_daemon().owner.clone(),
            Arc::new(move |e| {
                registry.broadcast(e);
            }),
            stop.clone(),
        ) {
            eprintln!("  queues: other daemons' moves will not be heard here: {e}");
        }
    }
    if !cfg.serve_queues {
        // The line the daemon's log carries, which is where an operator asks why nothing lands
        // from this daemon: the flag, why it exists, and what still works here.
        eprintln!(
            "  queues: this daemon serves NO queue (--no-queue) — it neither claims a merge-queue \
             lease nor gates a target nor lands a branch, so a package worker cannot be set to \
             fixing a `main` nobody asked it about; other daemons' moves are still relayed to its \
             heads"
        );
        return None;
    }
    let service = serve::QueueService::new(store_path, &cfg.workspace, env);
    match service.spawn(stop) {
        Ok(h) => {
            eprintln!(
                "  queues: serving {} when this daemon can take its lease",
                repository_of_workspace(&cfg.workspace)
                    .map(|r| format!("the repository {r}"))
                    .unwrap_or_else(|| format!(
                        "nothing — {} is a session of no repository (it may contain some; their \
                         queues are served by daemons seated in them)",
                        cfg.workspace.display()
                    ))
            );
            Some(h)
        }
        Err(e) => {
            eprintln!("  queues: not served — the thread did not start: {e}");
            None
        }
    }
}

/// **Why this daemon's queues still need it, if they do** — the merge queue's half of the idle
/// exit (`Sessions::idle_exit_due`). A repository whose lease this daemon holds and that has a row
/// in line (queued or gating to land, waiting or under review) is work this daemon is serving, and
/// exiting would leave it until some other daemon took the lease. A BLOCKED row does not hold it:
/// that row waits on a person, and the lease's next holder re-gates it at start (R76 item 3), which
/// is what a restart already is. `None` when the store cannot be read either — an unreadable store
/// is not work, and the worker loop says so on its own.
pub fn busy_here(store_path: &Path) -> Option<String> {
    use letibot_tokencore::store::{MergeState, ReviewState};
    let store = Store::open(store_path).ok()?;
    let me = &lease::this_daemon().owner;
    for l in store.queue_leases().ok()? {
        if &l.owner != me {
            continue;
        }
        let key = l.repository.as_str();
        let landing = store
            .merge_rows(Some(key))
            .ok()?
            .into_iter()
            .any(|m| matches!(m.state, MergeState::Queued | MergeState::Gating));
        let reviewing = store
            .review_rows(Some(key))
            .ok()?
            .into_iter()
            .any(|r| matches!(r.state, ReviewState::Waiting | ReviewState::Reviewing));
        if landing || reviewing {
            return Some(format!("the merge queue of {key} has work in line"));
        }
    }
    None
}

/// **The repository a workspace is a session OF** — the queues' key (the canonical main
/// checkout) of the repository [`repo_of_workspace`] reads the workspace as: the checkout it is
/// in (a worktree counts as its repository), a same-named child (`~/Projects/letibot` means
/// `~/Projects/letibot/letibot`), or its only repository child. `None` for a directory that merely
/// CONTAINS repositories — the operator's home, with five checkouts under it, is a session of none
/// of them.
pub fn repository_of_workspace(workspace: &Path) -> Option<String> {
    repo_of_workspace(workspace).and_then(|r| git::repository_key(&r))
}

/// **Every repository a workspace is a session of**, as the queues key them — the one
/// [`repository_of_workspace`] names, or each repository of a folder that is one project of
/// several (`git::repos_of_workspace`: children sharing an `origin` owner). What a daemon serves,
/// whose alarms a session hears, and which sessions are a repository's roots.
pub fn repositories_of_workspace(workspace: &Path) -> Vec<String> {
    git::repos_of_workspace(workspace)
        .iter()
        .filter_map(|r| git::repository_key(r))
        .collect()
}

/// **[`repository_of_workspace`], remembered for a minute** — a head asks for the queues on
/// every move (R92 item 5), and each answer names the asking session's repository; the answer
/// for one workspace does not change between two moves, and git need not be asked each time.
pub fn repository_of_workspace_cached(workspace: &Path) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    type Memo = Mutex<HashMap<std::path::PathBuf, (Instant, Option<String>)>>;
    static MEMO: OnceLock<Memo> = OnceLock::new();
    const KEEP: Duration = Duration::from_secs(60);
    let memo = MEMO.get_or_init(Default::default);
    if let Some((at, r)) = memo
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(workspace)
        && at.elapsed() < KEEP
    {
        return r.clone();
    }
    let r = repository_of_workspace(workspace);
    memo.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(workspace.to_path_buf(), (Instant::now(), r.clone()));
    r
}

/// **[`repositories_of_workspace`], remembered for a minute** — see the single form below. — a head asks for the queues on
/// every move (R92 item 5), and each answer names the asking session's repository; the answer
/// for one workspace does not change between two moves, and git need not be asked each time.
pub fn repositories_of_workspace_cached(workspace: &Path) -> Vec<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    type Memo = Mutex<HashMap<std::path::PathBuf, (Instant, Vec<String>)>>;
    static MEMO: OnceLock<Memo> = OnceLock::new();
    const KEEP: Duration = Duration::from_secs(60);
    let memo = MEMO.get_or_init(Default::default);
    if let Some((at, r)) = memo
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(workspace)
        && at.elapsed() < KEEP
    {
        return r.clone();
    }
    let r = repositories_of_workspace(workspace);
    memo.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(workspace.to_path_buf(), (Instant::now(), r.clone()));
    r
}

/// **Whether a daemon seated at `workspace` is one of `repo`'s session daemons** — its workspace
/// is that repository (or under it, or one of its worktrees). An ancestor directory that merely
/// contains the repository is NOT: on 2026-10-12 a daemon seated at `/home/dead` took letibot's
/// lease, gated its `main`, and showed `‼ main RED` to a session about the box's RSS, because
/// the old rule was *the repository is under the workspace, or holds it*.
pub fn serves(workspace: &Path, repo: &Path) -> bool {
    let key = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    repositories_of_workspace(workspace)
        .iter()
        .any(|mine| Path::new(mine) == key)
}

/// **The hubs a repository's queue alert goes to** — the sessions this registry holds whose
/// workspace is a session of `repository` ([`repository_of_workspace`]), and the review queue's
/// own session, whose board IS the queue. Never every hub: a red `main` is news to the project's
/// sessions and the queue's board, and noise everywhere else.
pub fn hubs_of_repository(
    registry: &letibot_sessionlog::registry::Registry,
    store: Option<&Store>,
    repository: &str,
) -> Vec<String> {
    registry
        .held_ids()
        .into_iter()
        .filter(|id| {
            id == crate::sessions::gatekeeper::SESSION
                || store
                    .and_then(|s| s.session_workspace(id).ok().flatten())
                    .is_some_and(|w| {
                        repositories_of_workspace(Path::new(&w))
                            .iter()
                            .any(|r| r == repository)
                    })
        })
        .collect()
}

/// Publish `event` on [`hubs_of_repository`]'s hubs only. Answers how many it reached.
pub fn publish_for_repository(
    registry: &letibot_sessionlog::registry::Registry,
    store_path: Option<&Path>,
    repository: &str,
    event: SessionEvent,
) -> usize {
    let store = store_path.and_then(|p| Store::open(p).ok());
    let mut reached = 0;
    for id in hubs_of_repository(registry, store.as_ref(), repository) {
        if let Some(hub) = registry.get(&id) {
            hub.publish(event.clone());
            reached += 1;
        }
    }
    reached
}
