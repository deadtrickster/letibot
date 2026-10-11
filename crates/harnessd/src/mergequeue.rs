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
                store
                    .session_workspace(id)
                    .ok()
                    .flatten()
                    .and_then(|w| repo_of_workspace(Path::new(&w)))
                    .and_then(|r| repo_of_worktree(&r))
                    .is_some_and(|r| r.display().to_string() == repository)
            })
            .collect()
    }
}

/// **Start the queues for a daemon** — `None` with no `--store` (the queues live in the store).
pub fn spawn_for(
    cfg: &crate::config::Config,
    stop: Arc<AtomicBool>,
    registry: Arc<letibot_sessionlog::registry::Registry>,
) -> Option<JoinHandle<()>> {
    let store_path = cfg.store.clone()?;
    let env = serve::QueueEnv::production(
        Arc::new(RegistryDoor {
            registry,
            store_path: store_path.clone(),
        }),
        cfg.landing_root.clone(),
    );
    let service = serve::QueueService::new(store_path, &cfg.workspace, env);
    match service.spawn(stop) {
        Ok(h) => {
            eprintln!(
                "  queues: serving the repositories under {} whose lease this daemon can take",
                cfg.workspace.display()
            );
            Some(h)
        }
        Err(e) => {
            eprintln!("  queues: not served — the thread did not start: {e}");
            None
        }
    }
}

/// Whether `path` is inside `workspace` or holds it — a repository this daemon may serve.
pub fn serves(workspace: &Path, repo: &Path) -> bool {
    let ws = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    repo.starts_with(&ws) || ws.starts_with(repo)
}
