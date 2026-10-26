//! **The queues' own threads** — a supervisor per daemon that finds the repositories it may serve
//! and claims their leases, and one worker per repository it holds, which keeps the review queue
//! moving, gates the target's tip on every change, and lands approved branches one at a time.
//!
//! Never on a session's worker (R63.3): `c12d044`'s review pass on the shared worker pinned every
//! daemon at 100% and left prompts queued for minutes (2026-10-11). Never ended by a store error
//! (R67 item 12, kept): a pass that fails waits and tries again, and says so on the hubs.
//!
//! # One repository, one worker, one lease
//!
//! Decision 1: *"a lease per repository among that repository's session daemons — one holds it,
//! with a heartbeat; another takes over when it dies"*. Decision 2: *"one queue per repository …
//! one repository's red main does not stop another's landings"*. So the worker is per repository:
//! a long gate in one repository does not hold another's, and losing a lease ends exactly one
//! worker. A daemon serves a repository when the repository is its workspace, under it, or holds
//! it (a daemon seated inside a checkout) — *that repository's session daemons*.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::{
    HealthState, LeaseClaim, LeaseOutcome, MergeRow, MergeState, Move, NewReview, ReviewKind,
    ReviewRow, ReviewState, Store, StoreError, TargetHealth,
};

use super::carry;
use super::gate::{self, GateControl, GateRun, GateRunner, GateVerdict};
use super::git;
use super::lease::{HEARTBEAT_EVERY, LEASE_TTL_MS};
use super::settings::{self, QueueSettings};

/// **A reviewer's time** — an hour from the claim. A reviewer still running then is killed and
/// the attempt counted (R61 defect 10: `Running` was waited on for ever).
pub const REVIEW_DEADLINE_MS: u64 = 60 * 60 * 1000;

/// **How long a claimed review may wait for its reviewer to START** before the queue raises it as
/// a reviewer that cannot start (decision 4) — the review queue's session never picked it up.
pub const REVIEWER_START_GRACE_MS: u64 = 5 * 60 * 1000;

/// **Attempts before a review is `blocked` and raised** — three, and then a person (the v23
/// queue's 3-in-5-minutes then handed the branch to a model to merge without the gate: defect 11).
pub const MAX_REVIEW_ATTEMPTS: u32 = 3;

/// Reviews a repository runs at once.
pub const MAX_REVIEWING: usize = 2;

/// **Where the queues reach sessions** — the review queue's session, a project's main session,
/// and every hub. A trait so the tests record and the daemon rings.
pub trait QueueDoor: Send + Sync {
    /// Publish on every session's log.
    fn announce(&self, event: SessionEvent);
    /// **Publish one repository's alert** — only on the hubs of that repository's sessions (and
    /// the review queue's board), never on every hub: a daemon's other sessions are about other
    /// things (`‼ main RED` reached a session about the box's RSS, 2026-10-12).
    fn announce_to(&self, repository: &str, event: SessionEvent) {
        let _ = repository;
        self.announce(event);
    }
    /// Wake the review queue's session: a claimed review is waiting for its reviewer.
    fn ring_reviewers(&self);
    /// Wake the project session that `session_id` belongs to (its root). What it is told is read
    /// from the store by that session (`Harness::queue_notices`).
    fn ring_project(&self, session_id: &str) -> Result<String, String>;
    /// **Wake the root sessions of the project `repository` is** — a target emergency has no
    /// branch and so no session of its own (R76 item 1). What each is told is read from the store
    /// by that session (`Harness::queue_emergency_notices`), once per red tip or parked row.
    fn ring_repository(&self, repository: &str) -> Result<String, String> {
        Err(format!(
            "this door rings no project session of {repository}"
        ))
    }
    /// **Whether `session_id` is live in this daemon now** — a branch's tree is not removed from
    /// under a session still working in it (R92.24).
    fn session_live(&self, session_id: &str) -> bool {
        let _ = session_id;
        false
    }
    /// **Whether this door has sessions to be asked about at all.** A door that does not — a test
    /// door that holds nothing, a daemon with no sessions — never reads as *the child is gone*:
    /// [`Self::session_live`]'s default `false` is the right answer to *is this tree in use* and
    /// the wrong one to *has the child that was working on this row died*.
    fn sees_sessions(&self) -> bool {
        false
    }
}

/// How a project's `[queue]` settings are read — the project's `leticode.toml` in production.
pub type SettingsFor = Arc<dyn Fn(&Path) -> Result<QueueSettings, String> + Send + Sync>;

/// Where a repository's landing worktree is made: the directory [`git::landing_worktree_in`]
/// puts it under (letibot's data directory in production, a temporary one in the tests).
pub type LandingRootFor = Arc<dyn Fn(&Path) -> Result<PathBuf, String> + Send + Sync>;

/// **What every worker of one daemon shares.**
pub struct QueueEnv {
    pub me: LeaseClaim,
    pub gate: Arc<dyn GateRunner>,
    pub door: Arc<dyn QueueDoor>,
    pub settings: SettingsFor,
    pub landing_root: LandingRootFor,
    pub gate_deadline: Duration,
    pub review_deadline_ms: u64,
    pub reviewer_start_grace_ms: u64,
    /// How long a worker sleeps after a pass with nothing to do.
    pub idle: Duration,
}

impl QueueEnv {
    /// The daemon's own: this daemon's identity, the local gate, thirty minutes, the project's
    /// `leticode.toml`.
    pub fn production(door: Arc<dyn QueueDoor>, landing_root: Option<PathBuf>) -> Self {
        QueueEnv {
            me: super::lease::this_daemon().clone(),
            gate: Arc::new(gate::LocalGate),
            door,
            settings: Arc::new(settings::for_repository),
            landing_root: Arc::new(move |_| {
                landing_root.clone().ok_or_else(|| {
                    "this daemon has no landing root (no letibot data directory), so there is \
                     nowhere outside the repository to land in"
                        .to_string()
                })
            }),
            gate_deadline: gate::GATE_DEADLINE,
            review_deadline_ms: REVIEW_DEADLINE_MS,
            reviewer_start_grace_ms: REVIEWER_START_GRACE_MS,
            idle: Duration::from_secs(2),
        }
    }
}

/// What one pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pass {
    /// Nothing moved.
    Idle,
    /// Something moved; go again at once.
    Worked,
    /// **The lease is not this daemon's any more** — the worker ends.
    LostLease,
}

/// **The worker for one repository** — every pass: heartbeat, the review queue's upkeep, the
/// target's health, one landing.
pub struct RepoQueue {
    store: Store,
    repo: PathBuf,
    key: String,
    env: Arc<QueueEnv>,
    /// **What this worker has already rung a project about** (R76 item 1) — a red tip, a gate
    /// that could not judge, a row parked for a person — so each is rung once per worker, not
    /// every two seconds. A cache of the store's `queue_told` rows (`rung:<repository>:<key>`),
    /// which are what keep it once per tip across a restart (R92.7): a restarted daemon that
    /// re-gates the same red tip neither rings nor resumes anybody about it again.
    rung: std::sync::Mutex<std::collections::HashSet<String>>,
    /// **The `[queue]` error last said** (R92.19) — `queue_settings_unreadable` is raised when
    /// the error is new or changed, not every pass (it was every two seconds, on every hub).
    settings_said: std::sync::Mutex<Option<String>>,
    /// **The one gate this repository is running, if any** (R92 item 8). A gate runs on a thread
    /// of its own and the pass only looks at it — so a thirty-minute gate does not stop the review
    /// queue: claims, reviewer deadlines and capacity holds go on while it runs. One at a time,
    /// so landings stay serialized; taken out of the slot exactly once, so a verdict is applied
    /// exactly once. What a restart needs is in the store already (the `gating` health row and
    /// merge row): a worker that holds no gate for such a row runs it again.
    gate: std::sync::Mutex<Option<InFlight>>,
    /// **Whether the last heartbeat found the lease still this daemon's** — what a running gate's
    /// `alive` reads, since the gate's thread has no store of its own. The pass beats; a lost
    /// lease kills the gate within its next poll, as when the gate beat for itself.
    lease_ok: Arc<AtomicBool>,
}

/// **A gate in flight** — what it is for, so its verdict can be applied when it ends, and the
/// thread running it.
struct InFlight {
    what: GateFor,
    /// The commit under the gate: the target's tip, or the rebased branch.
    sha: String,
    started: Instant,
    /// Set to kill the gate — the daemon is stopping.
    cancel: Arc<AtomicBool>,
    run: Running,
}

enum Running {
    Thread(JoinHandle<GateRun>),
    /// The thread could not be started: its verdict, at once.
    Done(GateRun),
}

impl Running {
    fn finished(&self) -> bool {
        match self {
            Running::Thread(h) => h.is_finished(),
            Running::Done(_) => true,
        }
    }

    fn wait(self) -> GateRun {
        match self {
            Running::Thread(h) => h.join().unwrap_or_else(|_| GateRun {
                steps: vec![],
                verdict: GateVerdict::Infra("the gate's thread panicked".into()),
            }),
            Running::Done(run) => run,
        }
    }
}

/// What a running gate judges — and so how its verdict is applied.
enum GateFor {
    /// The target's tip (decision 10): its health.
    Target {
        health: TargetHealth,
        wt: PathBuf,
        carried: Vec<String>,
    },
    /// A branch rebased onto the target, before the compare-and-swap.
    Land(Box<Landing>),
    /// The install steps, after the landing was pushed (decision 7).
    Install {
        landing: Box<Landing>,
        gate: GateRun,
        pushed: String,
    },
}

/// **One landing between its rebase and its end** — what `land` knew when it started the gate.
struct Landing {
    row: MergeRow,
    tip: String,
    head: String,
    rebased: String,
    wt: PathBuf,
    carried: Vec<String>,
    install: Vec<String>,
}

/// What [`RepoQueue::health`] found.
enum Health {
    /// The target's health now, and whether it was written in this pass.
    Known(TargetHealth, bool),
    /// A gate of the tip was started; its verdict is applied by a later pass.
    Started,
}

impl RepoQueue {
    pub fn new(store: Store, repo: PathBuf, env: Arc<QueueEnv>) -> Self {
        let key = repo.display().to_string();
        RepoQueue {
            store,
            repo,
            key,
            env,
            rung: Default::default(),
            settings_said: Default::default(),
            gate: Default::default(),
            lease_ok: Arc::new(AtomicBool::new(true)),
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<InFlight>> {
        self.gate.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether a gate of this repository is running (or ended and not yet applied).
    pub fn gate_in_flight(&self) -> bool {
        self.slot().is_some()
    }

    /// Whether the running gate has ended, so the next pass has a verdict to apply.
    pub fn gate_finished(&self) -> bool {
        self.slot().as_ref().is_some_and(|g| g.run.finished())
    }

    /// **A pass, and the passes that apply every gate it started** — what one pass was before
    /// gates ran on their own thread. For the tests that are about what a gate's verdict does,
    /// not about the pass going on meanwhile.
    #[cfg(test)]
    pub(crate) fn settle(&self, stop: &AtomicBool) -> Result<Pass, StoreError> {
        loop {
            let p = self.pass(stop)?;
            if p == Pass::LostLease || !self.gate_in_flight() {
                return Ok(p);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// **Why the landings wait** — `gate running on <sha> for Nm`, while a gate runs.
    pub fn gate_running(&self) -> Option<String> {
        self.slot()
            .as_ref()
            .map(|g| gate_running_words(&g.sha, g.started.elapsed().as_millis() as u64))
    }

    pub fn repository(&self) -> &str {
        &self.key
    }

    fn me(&self) -> &str {
        &self.env.me.owner
    }

    fn announce_move(&self, queue: &str, id: &str, to: &str, why: &str) {
        self.env.door.announce(SessionEvent::QueueMoved {
            queue: queue.into(),
            id: id.into(),
            to: to.into(),
            why: why.into(),
        });
    }

    /// **A raise** — said on the hubs of THIS repository's sessions (and the queue's board), under
    /// a code a head can style: `queue_blocked` (high) and `target_red` (emergency).
    fn raise(&self, code: &str, detail: String) {
        eprintln!("  merge queue: {code}: {detail}");
        self.env.door.announce_to(
            &self.key,
            SessionEvent::Warning {
                code: code.into(),
                detail,
                compaction: None,
            },
        );
    }

    fn review_to(&self, r: &ReviewRow, to: ReviewState, mv: Move) -> Result<bool, StoreError> {
        let why = mv.why.clone();
        let ok = self
            .store
            .move_review(&r.id, r.state, to, &mv, gate::now_ms())?;
        if ok {
            self.announce_move("review", &r.id, to.as_str(), &why);
        }
        Ok(ok)
    }

    fn merge_to(
        &self,
        m: &MergeRow,
        from: MergeState,
        to: MergeState,
        mv: Move,
    ) -> Result<bool, StoreError> {
        let why = mv.why.clone();
        let ok = self
            .store
            .move_merge(&m.id, from, to, &mv, gate::now_ms())?;
        if ok {
            self.announce_move("merge", &m.id, to.as_str(), &why);
        }
        Ok(ok)
    }

    /// **Am I still the owner?** `false` ends the worker. A store error is not an answer either
    /// way: the caller retries the pass.
    pub fn beat(&self) -> Result<bool, StoreError> {
        let ours = self
            .store
            .heartbeat_queue_lease(&self.key, self.me(), gate::now_ms())?;
        if !ours {
            self.lease_ok.store(false, Ordering::Relaxed);
        }
        Ok(ours)
    }

    /// **One pass.** It never waits for a gate: it starts one, or applies the verdict of one that
    /// ended, and goes on with the review queue either way.
    pub fn pass(&self, stop: &AtomicBool) -> Result<Pass, StoreError> {
        if stop.load(Ordering::Relaxed) {
            self.cancel_gate();
        }
        if !self.beat()? {
            return Ok(Pass::LostLease);
        }
        let settings = match (self.env.settings)(&self.repo) {
            Ok(s) => {
                *self.settings_said.lock().unwrap_or_else(|e| e.into_inner()) = None;
                s
            }
            Err(why) => {
                // A `[queue]` that does not parse is said — once per error, not once per pass —
                // and the queue runs on the defaults rather than on a guess at what the file
                // meant.
                let mut said = self.settings_said.lock().unwrap_or_else(|e| e.into_inner());
                if said.as_deref() != Some(why.as_str()) {
                    *said = Some(why.clone());
                    drop(said);
                    self.raise(
                        "queue_settings_unreadable",
                        format!(
                            "the queue settings for {} could not be read: {why}",
                            self.key
                        ),
                    );
                }
                QueueSettings::default()
            }
        };
        // The verdict of a gate that ended, applied once — before the upkeep, so the review of a
        // tip that just went red is claimed in this pass (*"taken first, as soon as a reviewer is
        // free"*).
        let mut worked = self.reap_gate()?;
        worked |= self.adopt_orphans()?;
        worked |= self.review_upkeep(&settings)?;
        worked |= self.settle_landed_by_hand()?;
        if self.gate_in_flight() {
            // One gate per repository: the health of every target and the next landing wait for
            // it; the review queue above did not.
            self.ring_the_projects()?;
            return Ok(if worked { Pass::Worked } else { Pass::Idle });
        }
        let mut targets: Vec<String> = self
            .store
            .merge_rows(Some(&self.key))?
            .into_iter()
            .filter(|m| m.state.is_open())
            .map(|m| m.target)
            .collect();
        if let Some(t) = settings::target_for(&self.repo, &settings) {
            targets.push(t);
        }
        targets.sort();
        targets.dedup();
        for target in targets {
            if stop.load(Ordering::Relaxed) {
                return Ok(Pass::Worked);
            }
            let h = match self.health(&target)? {
                Health::Started => {
                    worked = true;
                    break;
                }
                Health::Known(h, written) => {
                    worked |= written;
                    h
                }
            };
            if stop.load(Ordering::Relaxed) {
                return Ok(Pass::Worked);
            }
            worked |= self.land_next(&target, &h)?;
            if self.gate_in_flight() {
                break;
            }
        }
        self.ring_the_projects()?;
        Ok(if worked { Pass::Worked } else { Pass::Idle })
    }

    /// **A target emergency reaches a MODEL, not only the heads** (R76 item 1). `raise` is a
    /// warning on every hub, which heads draw and no model reads: pg-noop's agent never heard its
    /// `main` was red. So every pass rings the project that has to act — the repository's root
    /// sessions for the target's own trouble (red, unjudged, timed out; a red tip's review blocked
    /// or undecided), the branch's root for its review parked for a person — once per tip or per
    /// parked row. The words are the session's to read off the store
    /// (`Harness::queue_emergency_notices`), so a ring that arrives twice says nothing twice.
    fn ring_the_projects(&self) -> Result<(), StoreError> {
        let mut due: Vec<(String, Option<String>)> = Vec::new();
        for h in self.store.target_healths()? {
            if h.repository == self.key && emergency_health(&h) {
                due.push((
                    format!("health:{}:{}:{}", h.target, h.sha, h.state.as_str()),
                    None,
                ));
            }
        }
        for r in self.store.review_rows(Some(&self.key))? {
            if !matches!(r.state, ReviewState::Blocked | ReviewState::CannotDecide)
                || r.evidence.starts_with(super::HANDED_TO_PROJECT)
            {
                continue;
            }
            let who = match r.kind {
                ReviewKind::Target => None,
                ReviewKind::Branch => Some(r.session_id.clone()),
            };
            due.push((
                format!("row:{}:{}:{}", r.id, r.state.as_str(), r.state_ms),
                who,
            ));
        }
        for (key, session) in due {
            if !self.first_time(&format!("rung:{}:{key}", self.key))? {
                continue;
            }
            let rang = match &session {
                Some(s) if !s.is_empty() => self.env.door.ring_project(s),
                _ => self.env.door.ring_repository(&self.key),
            };
            if let Err(why) = rang {
                eprintln!("  merge queue: {key}: no project session was rung: {why}");
            }
        }
        Ok(())
    }

    /// **Said before?** `true` the first time `key` is asked about — by this worker or any
    /// daemon's before it (the store's `queue_told`, R92.7) — and records it.
    fn first_time(&self, key: &str) -> Result<bool, StoreError> {
        if !self
            .rung
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string())
        {
            return Ok(false);
        }
        self.store.tell_once(key, gate::now_ms())
    }

    /// **Forget what was said under `prefix`** — the state it was about is over. Answers how many
    /// keys the store held: more than none means it had been said.
    fn forget(&self, prefix: &str) -> Result<usize, StoreError> {
        self.rung
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|k| !k.starts_with(prefix));
        self.store.untell_prefix(prefix)
    }

    /// **A target alarm, raised once per tip and cause** — not again when a restarted daemon
    /// re-gates the same tip to the same verdict (R92.7), and again once the target has been
    /// green in between (R92.31: [`Self::target_is_green`] forgets them).
    fn alarm(&self, h: &TargetHealth, code: &str, detail: String) -> Result<bool, StoreError> {
        let key = format!("alarm:{}:{}:{}:{code}", self.key, h.target, h.sha);
        if self.first_time(&key)? {
            self.raise(code, detail);
            return Ok(true);
        }
        Ok(false)
    }

    /// **The targets of this repository that are RED now** — the ones whose queues are halted
    /// (R92.18).
    fn red_targets(&self) -> Result<std::collections::HashSet<String>, StoreError> {
        Ok(self
            .store
            .target_healths()?
            .into_iter()
            .filter(|h| h.repository == self.key && h.state == HealthState::Red)
            .map(|h| h.target)
            .collect())
    }

    // ===== The gate in flight =====

    /// **Start a gate on its own thread** — under the same controls as ever: the deadline from
    /// now, the daemon's stop (through `cancel`), and the lease (through [`Self::lease_ok`]).
    fn start_gate(&self, what: GateFor, wt: PathBuf, steps: Vec<String>, sha: &str) {
        let cancel = Arc::new(AtomicBool::new(false));
        let lease = self.lease_ok.clone();
        let runner = self.env.gate.clone();
        let deadline = Instant::now() + self.env.gate_deadline;
        let stop = cancel.clone();
        let short: String = sha.chars().take(8).collect();
        let run = std::thread::Builder::new()
            .name(format!("gate-{short}"))
            .spawn(move || {
                let alive = move || lease.load(Ordering::Relaxed);
                let ctl = GateControl {
                    deadline,
                    stop: &stop,
                    alive: &alive,
                };
                runner.run(&wt, &steps, &ctl)
            });
        let run = match run {
            Ok(h) => Running::Thread(h),
            Err(e) => Running::Done(GateRun {
                steps: vec![],
                verdict: GateVerdict::Infra(format!("the gate's thread could not start: {e}")),
            }),
        };
        let mut slot = self.slot();
        if let Some(old) = slot.take() {
            // Never two at once; a second start is a bug, and the first must not run unwatched.
            old.cancel.store(true, Ordering::Relaxed);
            eprintln!(
                "  merge queue: {}: a gate was started while one ran on {}; that one was stopped",
                self.key, old.sha
            );
        }
        *slot = Some(InFlight {
            what,
            sha: sha.to_string(),
            started: Instant::now(),
            cancel,
            run,
        });
    }

    /// Kill the running gate (the daemon is stopping); its verdict, `stopped`, is still applied.
    fn cancel_gate(&self) {
        if let Some(g) = self.slot().as_ref() {
            g.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// **Apply the verdict of a gate that ended** — taken out of the slot first, so it is applied
    /// once whatever the application does (including start the install's gate). Returns whether
    /// there was one.
    fn reap_gate(&self) -> Result<bool, StoreError> {
        let done = {
            let mut slot = self.slot();
            if slot.as_ref().is_some_and(|g| g.run.finished()) {
                slot.take()
            } else {
                None
            }
        };
        let Some(g) = done else {
            return Ok(false);
        };
        let run = g.run.wait();
        self.apply_gate(g.what, run)?;
        Ok(true)
    }

    fn apply_gate(&self, what: GateFor, run: GateRun) -> Result<(), StoreError> {
        match what {
            GateFor::Target {
                health,
                wt,
                carried,
            } => self.finish_health(health, &wt, &carried, run),
            GateFor::Land(landing) => self.finish_land(landing, run),
            GateFor::Install {
                landing,
                gate,
                pushed,
            } => self.finish_install(landing, gate, pushed, run),
        }
    }

    /// **A `gating` landing this worker runs no gate for** — its worker ended mid-gate (or a store
    /// error cut the application short), so it goes back in line and is gated again, as
    /// `recover_queue` does for another daemon's. Never while a gate is in flight: that row is
    /// the gate's.
    fn adopt_orphans(&self) -> Result<bool, StoreError> {
        if self.gate_in_flight() {
            return Ok(false);
        }
        let me = self.me().to_string();
        let mut moved = false;
        for m in self.store.merge_rows(Some(&self.key))? {
            if m.state == MergeState::Gating && m.owner == me {
                moved |= self.gating_to(
                    &m,
                    MergeState::Queued,
                    Move::new(
                        &me,
                        "its gate is not running here any more (the queue's worker restarted \
                         mid-gate), so it goes back in line and is rebased and gated again",
                    ),
                )?;
            }
        }
        Ok(moved)
    }

    /// **The worker is ending** — the running gate is killed, waited for, and its verdict
    /// applied (a stopped gate puts its row back in line), so nothing is left `gating` behind.
    pub fn wind_down(&self) {
        self.cancel_gate();
        let g = self.slot().take();
        if let Some(g) = g {
            let run = g.run.wait();
            if let Err(e) = self.apply_gate(g.what, run) {
                eprintln!(
                    "  merge queue: {}: the stopped gate's verdict was not written: {e}",
                    self.key
                );
            }
            // An install started by that verdict is stopped too.
            if let Some(next) = self.slot().take() {
                next.cancel.store(true, Ordering::Relaxed);
                let run = next.run.wait();
                if let Err(e) = self.apply_gate(next.what, run) {
                    eprintln!(
                        "  merge queue: {}: the stopped install's verdict was not written: {e}",
                        self.key
                    );
                }
            }
        }
    }

    // ===== The review queue's upkeep =====

    /// Deadlines, the rows that need no reviewer any more, the claims, and the raises. Returns
    /// whether anything moved.
    fn review_upkeep(&self, settings: &QueueSettings) -> Result<bool, StoreError> {
        let now = gate::now_ms();
        let me = self.me().to_string();
        let mut moved = false;
        // **A target review is about ONE tip** (the operator, 2026-10-26) — before anything else
        // reads them, so a stale one is neither claimed nor raised again. Every move of the tip
        // supersedes, not only a rewrite: R76 item 4 kept a review whose tip had merely moved
        // FORWARD, and four `red-main-<sha>` rows were open at once by lunchtime for tips `main`
        // had already left — each with a reviewer child, each rejected, each opening a fix branch
        // for a red that was no longer there.
        for r in self.store.review_rows(Some(&self.key))? {
            if r.kind == ReviewKind::Target
                && matches!(
                    r.state,
                    ReviewState::Waiting
                        | ReviewState::Reviewing
                        | ReviewState::Blocked
                        | ReviewState::CannotDecide
                )
            {
                moved |= self.supersede_older_tip(&r)?;
            }
        }
        // And the branches those tips opened (`red_target = "fix"`), which can never land once
        // the tip they were cut from is not the target's.
        moved |= self.supersede_fix_branches()?;
        let rows = self.store.review_rows(Some(&self.key))?;
        let target_default = settings::target_for(&self.repo, settings);

        for r in &rows {
            match r.state {
                // **A reviewer past its deadline, or one that never started** — the attempt is
                // counted, and the third is a raise rather than a fourth (defect 10).
                ReviewState::Reviewing if r.owner == me => {
                    let never_started = r.reviewer.is_none()
                        && now.saturating_sub(r.state_ms) > self.env.reviewer_start_grace_ms;
                    // **A reviewer child that is not alive in this daemon** — the operator's
                    // half of the report: *"after a daemon restart two rows stayed reviewing
                    // with reviewer children that no longer existed until their deadline"*.
                    // Liveness is the session registry's (`QueueDoor::session_live`: a subagent
                    // is a real registered session, so a handle the registry does not hold is a
                    // child that finished, died, or never came back through a restart). The
                    // reviewers' host collects a child that ends while the daemon is up; this is
                    // the door for one whose host will never look again.
                    let gone = self.env.door.sees_sessions()
                        && r.reviewer
                            .as_deref()
                            .is_some_and(|c| !self.env.door.session_live(c));
                    let late = r.deadline_ms.is_some_and(|d| now > d);
                    if never_started {
                        let why = format!(
                            "no reviewer started for `{}` within {} s of the claim — the review \
                             queue's session did not pick it up. A reviewer that cannot start is \
                             raised (decision 4), not retried in silence.",
                            r.branch,
                            self.env.reviewer_start_grace_ms / 1000
                        );
                        moved |= self.block(r, &why, settings)?;
                    } else if gone || late {
                        let attempts = r.attempts + 1;
                        let child = r.reviewer.as_deref().unwrap_or("?");
                        let why = if gone {
                            format!(
                                "the reviewer {child} is not alive in this daemon any more (it \
                                 finished without handing a verdict in, died, or a restart left the \
                                 row claimed), so the row is re-seated now rather than waiting out \
                                 its deadline — attempt {attempts} of {MAX_REVIEW_ATTEMPTS}"
                            )
                        } else {
                            format!(
                                "the reviewer {child} was still at it at its deadline and was stopped \
                                 — attempt {attempts} of {MAX_REVIEW_ATTEMPTS}"
                            )
                        };
                        if attempts >= MAX_REVIEW_ATTEMPTS {
                            moved |= self.block(r, &why, settings)?;
                        } else {
                            moved |= self.review_to(
                                r,
                                ReviewState::Waiting,
                                Move::new(&me, why.clone())
                                    .owned_by(&me)
                                    .set("owner", "")
                                    .clear("reviewer")
                                    .clear("deadline_ms")
                                    .set("attempts", attempts)
                                    .set("failure", why),
                            )?;
                        }
                    }
                }
                // **A verdict that answered a head the branch has since moved past is no verdict
                // on the branch** (decision 6, and defect 5's other half: `queue-spin`'s reject
                // outlived the commits that answered it).
                ReviewState::Rejected | ReviewState::CannotDecide
                    if r.kind == ReviewKind::Branch =>
                {
                    if let (Some(judged), Some(head)) = (
                        r.reviewed_sha.as_deref(),
                        git::branch_head(&self.repo, &r.branch),
                    ) && judged != head
                    {
                        moved |= self.review_to(
                            r,
                            ReviewState::Waiting,
                            Move::new(
                                &me,
                                format!(
                                    "`{}` moved after the verdict (it judged {judged}, the branch is \
                                     at {head}), so the new head goes back to review",
                                    r.branch
                                ),
                            )
                            .set("head_sha", head)
                            .clear("verdict")
                            .clear("verdict_by")
                            .clear("reviewed_sha")
                            .set("attempts", 0u32)
                            .set("failure", ""),
                        )?;
                    }
                }
                ReviewState::Waiting | ReviewState::Blocked if r.kind == ReviewKind::Branch => {
                    moved |= self.settle_if_needless(r)?;
                }
                _ => {}
            }
        }

        // **Decision 4's option**: a blocked review of a project that opted in is handed to the
        // project's main session to gate and merge itself — once, read off the row.
        if settings.merge_via_session {
            for r in self.store.review_rows(Some(&self.key))? {
                if r.state == ReviewState::Blocked
                    && r.kind == ReviewKind::Branch
                    && !r.evidence.starts_with(super::HANDED_TO_PROJECT)
                {
                    let told = self.env.door.ring_project(&r.session_id);
                    let why = match &told {
                        Ok(how) => format!(
                            "{}: the review could not run, and this project merges such a branch \
                             via its main session (`[queue] merge_via_session`). {how}",
                            super::HANDED_TO_PROJECT
                        ),
                        Err(e) => format!(
                            "{}: the review could not run and this project merges such a branch via \
                             its main session — but nobody could be told: {e}",
                            super::HANDED_TO_PROJECT
                        ),
                    };
                    moved |= self.review_to(&r, ReviewState::Blocked, Move::new(&me, why))?;
                }
            }
        }

        // **The claims** — the target's red tip first (*"main itself goes to review … taken
        // first, as soon as a reviewer is free"*), then priority, then age. **A red target halts
        // its review queue** (the operator, R92.18): while it is red only its red tip and a
        // branch opened to fix it are claimed; every other branch for it waits, and one claimed
        // but not yet started goes back to waiting.
        let red = self.red_targets()?;
        let target_of = |r: &ReviewRow| {
            if r.target.is_empty() {
                target_default.clone().unwrap_or_default()
            } else {
                r.target.clone()
            }
        };
        let halted = |r: &ReviewRow| {
            r.kind == ReviewKind::Branch && r.fixes.is_none() && red.contains(&target_of(r))
        };
        for r in self.store.review_rows(Some(&self.key))? {
            if r.state == ReviewState::Reviewing
                && r.owner == me
                && r.reviewer.is_none()
                && halted(&r)
            {
                let why = format!(
                    "held: `{}` is RED, and a red target halts its review queue — `{}` is \
                     reviewed once it is green again",
                    target_of(&r),
                    r.branch
                );
                moved |= self.review_to(
                    &r,
                    ReviewState::Waiting,
                    Move::new(&me, why)
                        .owned_by(&me)
                        .set("owner", "")
                        .clear("deadline_ms"),
                )?;
            }
        }
        let rows = self.store.review_rows(Some(&self.key))?;
        let busy = rows
            .iter()
            .filter(|r| r.state == ReviewState::Reviewing)
            .count();
        // A row waiting for a free VM slot is held until its not-before time (`capacity`): no
        // attempt was spent on it, and claiming it every pass would only ask firecode again.
        let mut waiting: Vec<&ReviewRow> = rows
            .iter()
            .filter(|r| r.state == ReviewState::Waiting)
            .filter(|r| !super::capacity::held_until(&r.failure, r.deadline_ms, now))
            .filter(|r| !halted(r))
            .collect();
        waiting.sort_by_key(|r| {
            (
                r.kind != ReviewKind::Target,
                r.priority.rank(),
                r.created_ms,
                r.id.clone(),
            )
        });
        let mut claimed = 0;
        for r in waiting.into_iter().take(MAX_REVIEWING.saturating_sub(busy)) {
            let head = match r.kind {
                ReviewKind::Target => Some(r.head_sha.clone()),
                ReviewKind::Branch => git::branch_head(&self.repo, &r.branch),
            };
            let Some(head) = head else {
                continue;
            };
            let target = target_of(r);
            let why = format!("claimed for review at {head} by {me}");
            if self.review_to(
                r,
                ReviewState::Reviewing,
                Move::new(&me, why)
                    .set("owner", &me)
                    .set("head_sha", head)
                    .set("target", target)
                    .clear("reviewer")
                    .set("deadline_ms", now + self.env.review_deadline_ms)
                    .set("failure", ""),
            )? {
                claimed += 1;
            }
        }
        if claimed > 0 {
            self.env.door.ring_reviewers();
            moved = true;
        }
        Ok(moved)
    }

    /// **A review nobody needs to run** — the branch is gone, carries nothing, or is already on
    /// the target (decision 13's *"checked and re-enqueued if its branch is unmerged"*, and a
    /// project that landed a blocked branch by hand).
    fn settle_if_needless(&self, r: &ReviewRow) -> Result<bool, StoreError> {
        let me = self.me().to_string();
        let Some(head) = git::branch_head(&self.repo, &r.branch) else {
            return self.review_to(
                r,
                ReviewState::Withdrawn,
                Move::new(
                    &me,
                    format!(
                        "`{}` no longer exists in {}, so there is nothing to review",
                        r.branch, self.key
                    ),
                ),
            );
        };
        if !r.base_sha.is_empty()
            && git::commits_over(&self.repo, &r.base_sha, &head).ok() == Some(0)
        {
            return self.review_to(
                r,
                ReviewState::Withdrawn,
                Move::new(
                    &me,
                    format!(
                        "nothing was committed on `{}`: it carries 0 commits over its base {}, so \
                         there is nothing to review. Its worktree is left as it is.",
                        r.branch, r.base_sha
                    ),
                ),
            );
        }
        let Some(target) = (!r.target.is_empty()).then(|| r.target.clone()) else {
            return Ok(false);
        };
        let Some(tip) = git::branch_head(&self.repo, &target) else {
            return Ok(false);
        };
        if git::is_ancestor(&self.repo, &head, &tip).unwrap_or(false) {
            return self.review_to(
                r,
                ReviewState::Withdrawn,
                Move::new(
                    &me,
                    format!(
                        "`{}` is already on `{target}` (at {tip}): it landed some other way, so \
                         there is nothing to review or merge",
                        r.branch
                    ),
                ),
            );
        }
        Ok(false)
    }

    /// **A merge row whose branch is already on its target settles** (R92.28) — landed by hand,
    /// or by the project's main session after a handover. `land_next` notices this only when it
    /// TAKES the row, which it never does while the target is red, unjudged or blocked, and never
    /// for a `blocked` row at all: such a row stayed parked for ever. A row being gated is left to
    /// its gate. The approved head is what is looked for (the branch may be gone by now), and the
    /// row is withdrawn saying where it was found — the queue did not land it, so `landed` would
    /// claim a push and a cleanup that never ran.
    fn settle_landed_by_hand(&self) -> Result<bool, StoreError> {
        let me = self.me().to_string();
        let mut moved = false;
        for m in self.store.merge_rows(Some(&self.key))? {
            if !matches!(m.state, MergeState::Queued | MergeState::Blocked) {
                continue;
            }
            let head = if m.approved_sha.is_empty() {
                match git::branch_head(&self.repo, &m.branch) {
                    Some(h) => h,
                    None => continue,
                }
            } else {
                m.approved_sha.clone()
            };
            let Some(tip) = git::branch_head(&self.repo, &m.target) else {
                continue;
            };
            if git::is_ancestor(&self.repo, &head, &tip).unwrap_or(false) {
                moved |= self.merge_to(
                    &m,
                    m.state,
                    MergeState::Withdrawn,
                    Move::new(
                        &me,
                        format!(
                            "`{}` is already on `{}` (at {tip}): it landed some other way, so \
                             there is nothing to merge",
                            m.branch, m.target
                        ),
                    ),
                )?;
            }
        }
        Ok(moved)
    }

    /// **The review of a tip the target no longer has** (R76 item 4, and the operator's ruling of
    /// 2026-10-26: *"a target review row is about ONE tip"*). Yesterday's four `red-main-<sha>`
    /// rows for `e51847ed`, `f35d19c0`, `54e5a372` and `0158b957` were one morning's tips of one
    /// branch: a newer tip never closed the older rows, so each got its own reviewer, each was
    /// rejected, and each opened a fix branch for a red `main` had already left.
    ///
    /// So **every move of the tip withdraws the reviews of the tips before it**, whatever the new
    /// tip's verdict: the health pass gates it, and if it is red its own row is the one review
    /// anybody acts on. That is wider than R76 item 4, which withdrew only a tip the target had
    /// been REWRITTEN past (an amend, a force-push, a reset — a tip whose history nobody has) and
    /// kept a review whose tip had merely moved forward, on the argument that *the red may still be
    /// in it*. It usually is — and the fix for it is the reviewer of the tip that is red NOW.
    ///
    /// **The row is moved; its reviewer is stopped by the host that started it.**
    /// [`super::review::serve`] kills a reviewer whose row is no longer `reviewing` under it, which
    /// is exactly how the deadline path stops one — so a withdrawal is all this side has to do.
    ///
    /// Nothing is deleted: the row keeps its state, its verdict and its history, with the sha it
    /// judged and the sha that superseded it in that history.
    fn supersede_older_tip(&self, r: &ReviewRow) -> Result<bool, StoreError> {
        if r.target.is_empty() || r.head_sha.is_empty() {
            return Ok(false);
        }
        let Some(tip) = git::branch_head(&self.repo, &r.target) else {
            // A target git cannot name is not a tip that moved; the health pass says so.
            return Ok(false);
        };
        if tip == r.head_sha {
            return Ok(false);
        }
        let me = self.me().to_string();
        let why = format!(
            "superseded by {tip}: a target review is about ONE tip, and `{}`'s has moved from {} \
             to {tip} — this review judges a tree the target no longer has, so {tip} is what is \
             gated now and, if it is red, the only review of `{}` anybody acts on",
            r.target, r.head_sha, r.target
        );
        let mut mv = Move::new(&me, why).set("owner", "").clear("deadline_ms");
        if r.state == ReviewState::Reviewing {
            mv = mv.owned_by(r.owner.clone());
        }
        self.review_to(r, ReviewState::Withdrawn, mv)
    }

    /// **A branch opened to fix a red tip the target has moved past.** Decision 11 has the red
    /// tip's reviewer open one, and it is the only branch a red target lands — so a fix cut from a
    /// tip that is no longer the target's is a branch that can never land. This box opened four of
    /// them in one morning, each with its own worktree and its own fixer child, and every one ended
    /// empty: `main` had moved past the tip whose reviewer asked for it.
    ///
    /// It is withdrawn like the review that opened it — *superseded by* the tip that replaced it —
    /// together with any landing it had queued. Its worktree and branch go **only when there is
    /// nothing in them to lose** ([`git::retire_if_empty`]), and a tree still held by a live
    /// session is never removed from under it (R92.24). Either way a person is told where the tree
    /// is, because a fixer's tree is the only copy of whatever it had not committed.
    fn supersede_fix_branches(&self) -> Result<bool, StoreError> {
        let me = self.me().to_string();
        let mut moved = false;
        let landings = self.store.merge_rows(Some(&self.key))?;
        for r in self.store.review_rows(Some(&self.key))? {
            if r.kind != ReviewKind::Branch {
                continue;
            }
            let Some(red) = r.fixes.clone() else {
                continue;
            };
            // **A fix is a row in either queue** — the review of the fixer's branch, or the
            // landing an approval put behind it — and it is superseded if either is still open. A
            // row being GATED is left to its gate: `land` rebases onto the tip as it is then and
            // gates the result, so it judges a tree the target really has.
            let open_landing: Vec<&MergeRow> = landings
                .iter()
                .filter(|m| {
                    m.review_id == r.id
                        && matches!(m.state, MergeState::Queued | MergeState::Blocked)
                })
                .collect();
            if !r.state.is_open() && open_landing.is_empty() {
                continue;
            }
            let Some(tip) = git::branch_head(&self.repo, &r.target) else {
                continue;
            };
            if tip == red {
                continue;
            }
            let why = format!(
                "superseded by {tip}: this branch was opened to fix the red tip {red}, and `{}`'s \
                 tip has moved to {tip} — a fix is the only branch a red target lands, and this one \
                 was cut from a tip the target no longer has, so it could never land",
                r.target
            );
            let mut done = false;
            if r.state.is_open() {
                done |= self.review_to(&r, ReviewState::Withdrawn, Move::new(&me, why.clone()))?;
            }
            for m in open_landing {
                done |= self.merge_to(
                    m,
                    m.state,
                    MergeState::Withdrawn,
                    Move::new(&me, why.clone()),
                )?;
            }
            if !done {
                // Somebody else moved it first: its history is that move's to write.
                continue;
            }
            moved = true;
            let Some(wt) = r.worktree.as_deref() else {
                continue;
            };
            let live = !r.session_id.is_empty() && self.env.door.session_live(&r.session_id);
            let kept = if live {
                Err(format!(
                    "the worktree {wt} and `{}` are kept: the session `{}` that works in it is \
                     still live",
                    r.branch, r.session_id
                ))
            } else {
                git::retire_if_empty(&self.repo, &tip, &r.branch, Some(wt))
            };
            if let Err(kept) = kept {
                // Said where a person reads it (R92.4), not only in the daemon's log.
                self.raise(
                    "queue_branch_kept",
                    format!("after withdrawing a superseded fix branch: {kept}"),
                );
            }
        }
        Ok(moved)
    }

    /// **Raise a review that cannot run** (decision 4) — `blocked`, at high severity.
    fn block(
        &self,
        r: &ReviewRow,
        why: &str,
        settings: &QueueSettings,
    ) -> Result<bool, StoreError> {
        let me = self.me().to_string();
        let ok = self.review_to(
            r,
            ReviewState::Blocked,
            Move::new(&me, why)
                .owned_by(&me)
                .set("owner", "")
                .set("failure", why)
                .set("attempts", r.attempts + 1),
        )?;
        if ok {
            self.raise(
                "queue_blocked",
                super::notices::review_cannot_run(r, why, settings.merge_via_session),
            );
        }
        Ok(ok)
    }

    // ===== The target's health =====

    /// **Gate the target's tip if it changed** (decision 10) — coalesced: whatever the tip is
    /// NOW is what is gated, so a burst of commits is one run. Returns the health, or that a gate
    /// of the tip was started (its verdict is [`Self::finish_health`]'s).
    fn health(&self, target: &str) -> Result<Health, StoreError> {
        let me = self.me().to_string();
        let now = gate::now_ms();
        let Some(tip) = git::branch_head(&self.repo, target) else {
            let h = TargetHealth {
                repository: self.key.clone(),
                target: target.into(),
                sha: String::new(),
                state: HealthState::Blocked,
                checked_ms: now,
                evidence: format!("`{target}` does not exist in {}", self.key),
                gate_steps: vec![],
            };
            let changed = self
                .store
                .target_health(&self.key, target)?
                .is_none_or(|old| old.state != h.state || old.evidence != h.evidence);
            if changed {
                self.store.set_target_health(&h, &me)?;
            }
            return Ok(Health::Known(h, false));
        };
        if let Some(h) = self.store.target_health(&self.key, target)?
            && h.sha == tip
            && h.state != HealthState::Gating
        {
            return Ok(Health::Known(h, false));
        }
        let mut h = TargetHealth {
            repository: self.key.clone(),
            target: target.into(),
            sha: tip.clone(),
            state: HealthState::Gating,
            checked_ms: now,
            evidence: format!("gating `{target}`'s tip"),
            gate_steps: vec![],
        };
        self.store.set_target_health(&h, &me)?;
        self.announce_move("target", target, "gating", &h.evidence);
        let declared = gate::declared_gate(&self.repo, &tip);
        let (state, evidence, steps) = match declared {
            Ok(None) => (
                HealthState::Blocked,
                format!(
                    "{}: `{target}` has no `Merge gate` section in AGENTS.md at {tip}, so nothing \
                     can be judged and nothing lands. The project's agent can propose one \
                     (`merge_gate`) for the operator to choose.",
                    super::WAITING_FOR_GATE
                ),
                vec![gate::no_gate_row("no `Merge gate` section on the target")],
            ),
            Err(e) => (
                HealthState::Blocked,
                format!("the gate could not be read from `{target}` at {tip}: {e}"),
                vec![],
            ),
            Ok(Some((steps, _install))) => match self.landing_at(&tip) {
                Err(e) => (
                    HealthState::Blocked,
                    format!("the landing worktree could not be made ready at {tip}: {e}"),
                    vec![],
                ),
                Ok((wt, carried)) => {
                    self.start_gate(
                        GateFor::Target {
                            health: h,
                            wt: wt.clone(),
                            carried,
                        },
                        wt,
                        steps,
                        &tip,
                    );
                    return Ok(Health::Started);
                }
            },
        };
        self.judged(&mut h, state, evidence, steps)?;
        Ok(Health::Known(h, true))
    }

    /// **The verdict of the target's gate.**
    fn finish_health(
        &self,
        mut h: TargetHealth,
        wt: &Path,
        carried: &[String],
        run: GateRun,
    ) -> Result<(), StoreError> {
        let target = h.target.clone();
        let tip = h.sha.clone();
        let (state, evidence, steps) = match run.verdict {
            GateVerdict::Passed => (
                HealthState::Green,
                format!(
                    "`{target}` is green at {tip} ({} gate)",
                    self.env.gate.name()
                ),
                run.steps,
            ),
            // **R76 item 5**: a gate that failed AFTER its build and tests passed could not judge
            // the code — its own setup failed. The tip stays unjudged (`blocked`: nothing lands),
            // and nobody is sent to find what in the code broke it.
            GateVerdict::Failed(w) => {
                let w = self.with_carry_hint(w, wt, carried);
                match super::setup::setup_failure(&run.steps) {
                    Some(setup) => (HealthState::Blocked, format!("{setup}\n\n{w}"), run.steps),
                    None => (HealthState::Red, w, run.steps),
                }
            }
            GateVerdict::TimedOut(w) => (HealthState::TimedOut, w, run.steps),
            GateVerdict::Infra(w) => (HealthState::Blocked, w, run.steps),
            GateVerdict::Stopped(_) => {
                // Judged nothing: the row stays `gating`, and the next pass (here or in the next
                // lease holder) gates it again.
                return Ok(());
            }
        };
        self.judged(&mut h, state, evidence, steps)
    }

    /// **Write a target's verdict** and say what it means.
    fn judged(
        &self,
        h: &mut TargetHealth,
        state: HealthState,
        evidence: String,
        steps: Vec<letibot_tokencore::store::MergeGateStep>,
    ) -> Result<(), StoreError> {
        let me = self.me().to_string();
        let target = h.target.clone();
        h.state = state;
        h.evidence = evidence;
        h.gate_steps = steps;
        h.checked_ms = gate::now_ms();
        self.store.set_target_health(h, &me)?;
        self.announce_move("target", &target, state.as_str(), &h.evidence);
        match state {
            HealthState::Red => self.target_went_red(h)?,
            HealthState::Green => self.target_is_green(h)?,
            HealthState::Blocked if h.evidence.starts_with(super::setup::GATE_SETUP_FAILED) => {
                // High, not emergency: the code was never judged red. Nothing lands until the
                // gate can judge it, which is said in the same breath.
                self.alarm(
                    h,
                    "gate_setup_failed",
                    super::notices::gate_setup_failed(h, &self.key),
                )?;
            }
            HealthState::Blocked if h.evidence.starts_with(gate::GATE_KILLED) => {
                // High, not emergency, for the same reason: the step was killed before it
                // judged the code — at the gate's memory cap, or by a signal — and the words
                // say which and how to give it room. Counted, so a daemon taking the queue
                // retries it less often each time (`verbs::regate_at_start`, R92.25).
                self.store
                    .tell_again(&killed_key(&self.key, &h.target, &h.sha), gate::now_ms())?;
                self.alarm(h, "gate_killed", super::notices::gate_killed(h, &self.key))?;
            }
            HealthState::TimedOut => {
                // R92.27: nothing lands until it is gated again, so it is said.
                self.alarm(
                    h,
                    "target_timed_out",
                    super::notices::target_timed_out(h, &self.key),
                )?;
            }
            HealthState::Blocked if !h.evidence.starts_with(super::WAITING_FOR_GATE) => {
                // R92.27: the gate could not be read, the landing tree could not be made ready,
                // a step could not be spawned — the tip is unjudged and nothing lands. (A target
                // with no declared gate has its own notice, `Harness::queue_notices`.)
                self.alarm(
                    h,
                    "target_unjudged",
                    super::notices::target_unjudged(h, &self.key),
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    /// **The landing worktree at `at`, with the project's `gate_carry` copied in** — the tree and
    /// what was carried.
    fn landing_at(&self, at: &str) -> Result<(PathBuf, Vec<String>), String> {
        let root = (self.env.landing_root)(&self.repo)?;
        let wt = git::landing_worktree_in(&root, &self.repo, at)?;
        let carried = self.gate_carry();
        carry::carry(&self.repo, &wt, &carried)?;
        Ok((wt, carried))
    }

    /// The project's `[queue] gate_carry`. A `[queue]` that does not parse was raised by the
    /// pass that read it; here it is the default, nothing carried.
    fn gate_carry(&self) -> Vec<String> {
        (self.env.settings)(&self.repo)
            .map(|s| s.gate_carry)
            .unwrap_or_default()
    }

    /// A failed gate's words, with [`carry::hint`]'s sentence as their last paragraph when the
    /// landing tree lacks ignored directories the checkout has and nothing is carried.
    fn with_carry_hint(&self, words: String, wt: &Path, carried: &[String]) -> String {
        match carry::hint(&self.repo, wt, carried) {
            Some(h) => format!("{words}\n\n{h}"),
            None => words,
        }
    }

    /// **Red target: an emergency, the red tip first in review, and the operator told**
    /// (decisions 10, 11; R61 item 4). Enqueue is never refused and other reviews go on; the
    /// merge queue lands nothing but a fix.
    fn target_went_red(&self, h: &TargetHealth) -> Result<(), StoreError> {
        let short: String = h.sha.chars().take(12).collect();
        let id = format!("red-{}-{short}", h.target.replace('/', "-"));
        let failing = h
            .gate_steps
            .iter()
            .find(|s| s.outcome == letibot_tokencore::store::MergeGateOutcome::Failed)
            .map(|s| s.command.clone())
            .unwrap_or_default();
        let base = git::rev_parse(&self.repo, &format!("{}^", h.sha)).unwrap_or_default();
        let brief = format!(
            "`{target}` is RED at {sha} in {repo}: its own merge gate fails{step}. Find what in the \
             target's recent history broke it, and say so with the evidence. The failing output's \
             end:\n\n{out}",
            target = h.target,
            sha = h.sha,
            repo = self.key,
            step = if failing.is_empty() {
                String::new()
            } else {
                format!(" at `{failing}`")
            },
            out = super::gate::for_the_model(&h.evidence),
        );
        let fresh = self.store.enqueue_review(
            &NewReview {
                id: id.clone(),
                repository: self.key.clone(),
                target: h.target.clone(),
                kind: ReviewKind::Target,
                session_id: String::new(),
                branch: h.target.clone(),
                worktree: None,
                base_sha: base,
                head_sha: h.sha.clone(),
                brief,
                priority: letibot_tokencore::store::MergePriority::Urgent,
                fixes: None,
                evidence: format!(
                    "`{}` went red at {}: the red tip is reviewed first",
                    h.target, h.sha
                ),
            },
            self.me(),
            gate::now_ms(),
        )?;
        if fresh {
            self.announce_move(
                "review",
                &id,
                "waiting",
                "the red target goes to review first",
            );
        }
        let news = self.first_time(&format!(
            "alarm:{}:{}:{}:target_red",
            self.key, h.target, h.sha
        ))?;
        if news && !fresh {
            // **The same sha red a second time** (R92.31) — it was green in between (a gate that
            // reads the machine, a re-gate): its review row was withdrawn then, and the
            // `INSERT OR IGNORE` above left it so. It goes back to review, and it is said.
            if let Some(r) = self.store.review_row(&id)?
                && r.state == ReviewState::Withdrawn
            {
                self.review_to(
                    &r,
                    ReviewState::Waiting,
                    Move::new(
                        self.me(),
                        format!(
                            "`{}` is RED again at {} (it was green in between): the red tip is \
                             reviewed first again",
                            h.target, h.sha
                        ),
                    )
                    .set("owner", "")
                    .clear("reviewer")
                    .clear("deadline_ms")
                    .clear("verdict")
                    .clear("verdict_by")
                    .clear("reviewed_sha")
                    .set("attempts", 0u32)
                    .set("failure", ""),
                )?;
            }
        }
        if news {
            self.raise(
                "target_red",
                super::notices::target_red_warning(h, &self.key, !fresh),
            );
        }
        // **A red target halts BOTH queues** (R92.18): what was approved and waits to land goes
        // back to review now, not when it would have landed.
        self.halt_landings(&h.target)?;
        Ok(())
    }

    /// **Every branch approved for `target` and waiting to land goes back to review** — its
    /// approval was of a change onto a tip that is now red, so it is spent (the operator, R92.18:
    /// *red main halts both queues*). A branch opened to fix the red tip keeps its place: it is
    /// the one thing that may move.
    fn halt_landings(&self, target: &str) -> Result<bool, StoreError> {
        let me = self.me().to_string();
        let mut moved = false;
        for m in self.store.merge_rows(Some(&self.key))? {
            if m.state != MergeState::Queued || m.target != target || m.fixes.is_some() {
                continue;
            }
            let why = format!(
                "`{target}` is RED, and a red target halts both queues: `{}`'s approval is spent \
                 and it goes back to review, to be reviewed again against the tip once `{target}` \
                 is green",
                m.branch
            );
            if !self.merge_to(
                &m,
                MergeState::Queued,
                MergeState::Gating,
                Move::new(&me, why.clone()).set("owner", &me),
            )? {
                continue;
            }
            let head =
                git::branch_head(&self.repo, &m.branch).unwrap_or_else(|| m.approved_sha.clone());
            if self
                .store
                .send_back_to_review(&m.id, &me, &head, &me, &why, gate::now_ms())?
            {
                self.announce_move("merge", &m.id, "moved", &why);
                self.announce_move("review", &m.review_id, "waiting", &why);
                moved = true;
            } else {
                // The review row was not `approved` (a person moved it): the row keeps its place.
                let m = self.store.merge_row(&m.id)?.unwrap_or(m);
                self.gating_to(
                    &m,
                    MergeState::Queued,
                    Move::new(&me, "back in line: its review could not be reopened"),
                )?;
            }
        }
        Ok(moved)
    }

    /// **Green again**: the reviews of earlier red tips are withdrawn — open ones, and the ones
    /// parked as `cannot_decide` or ended `rejected`, which a later verdict could otherwise turn
    /// into an EMERGENCY on a green target (R92.29) — and, when an alarm about the target had been
    /// raised, that it is over is said once, where the alarm was (R92.6).
    fn target_is_green(&self, h: &TargetHealth) -> Result<(), StoreError> {
        for r in self.store.review_rows(Some(&self.key))? {
            if r.kind == ReviewKind::Target
                && r.target == h.target
                && (r.state.is_open()
                    || matches!(r.state, ReviewState::CannotDecide | ReviewState::Rejected))
            {
                let why = format!("`{}` is green again at {}", h.target, h.sha);
                let mut mv = Move::new(self.me(), why);
                if r.state == ReviewState::Reviewing {
                    mv = mv.owned_by(r.owner.clone()).set("owner", "");
                }
                self.review_to(&r, ReviewState::Withdrawn, mv)?;
            }
        }
        let alarms = self.forget(&format!("alarm:{}:{}:", self.key, h.target))?;
        self.forget(&format!("rung:{}:health:{}:", self.key, h.target))?;
        self.forget(&killed_key(&self.key, &h.target, ""))?;
        if alarms > 0 {
            self.raise("target_green", super::notices::green_again(h, &self.key));
            if let Err(why) = self.env.door.ring_repository(&self.key) {
                eprintln!("  merge queue: green again: no project session was rung: {why}");
            }
        }
        Ok(())
    }

    // ===== The landing =====

    /// **Take the next approved branch for `target` and land it** — only while the target is
    /// green at its current tip, or, while it is red, only a branch opened to fix it.
    fn land_next(&self, target: &str, h: &TargetHealth) -> Result<bool, StoreError> {
        let mut queued: Vec<MergeRow> = self
            .store
            .merge_rows(Some(&self.key))?
            .into_iter()
            .filter(|m| m.state == MergeState::Queued && m.target == target)
            .collect();
        if queued.is_empty() {
            return Ok(false);
        }
        if h.state == HealthState::Red && self.halt_landings(target)? {
            // Approved while red (a reviewer that was already running): back to review.
            return Ok(true);
        }
        let tip_now = git::branch_head(&self.repo, target);
        let current = tip_now.as_deref() == Some(h.sha.as_str());
        let allowed = |m: &MergeRow| match h.state {
            HealthState::Green => current,
            // *"while main is red the merge queue lands nothing"* — but the fix itself has to.
            HealthState::Red => m.fixes.is_some(),
            _ => false,
        };
        queued.sort_by_key(|m| {
            (
                m.fixes.is_none(),
                m.priority.rank(),
                m.created_ms,
                m.id.clone(),
            )
        });
        let Some(row) = queued.into_iter().find(|m| allowed(m)) else {
            return Ok(false);
        };
        let me = self.me().to_string();
        if !self.merge_to(
            &row,
            MergeState::Queued,
            MergeState::Gating,
            Move::new(&me, format!("taken by {me}: rebasing onto `{target}`")).set("owner", &me),
        )? {
            return Ok(false);
        }
        let row = self.store.merge_row(&row.id)?.unwrap_or(row);
        self.land(&row)?;
        Ok(true)
    }

    fn gating_to(&self, row: &MergeRow, to: MergeState, mv: Move) -> Result<bool, StoreError> {
        let me = self.me().to_string();
        self.merge_to(
            row,
            MergeState::Gating,
            to,
            mv.owned_by(&me).set("owner", ""),
        )
    }

    /// **The landing, in the dedicated worktree, with a compare-and-swap on the target.**
    fn land(&self, row: &MergeRow) -> Result<(), StoreError> {
        let me = self.me().to_string();
        let repo = self.repo.clone();
        let target = row.target.clone();
        let Some(tip) = git::branch_head(&repo, &target) else {
            self.gating_to(
                row,
                MergeState::Blocked,
                Move::new(
                    &me,
                    format!("the target `{target}` does not exist in {}", self.key),
                ),
            )?;
            return Ok(());
        };
        let Some(head) = git::branch_head(&repo, &row.branch) else {
            self.gating_to(
                row,
                MergeState::Blocked,
                Move::new(
                    &me,
                    format!("`{}` no longer exists in {}", row.branch, self.key),
                ),
            )?;
            return Ok(());
        };
        // **Decision 6**: the verdict is bound to the head it judged. A different head carrying
        // the same change (a rebase) is not a move; any other difference is.
        if head != row.approved_sha {
            let same = match (
                git::change_id(&repo, &tip, &head),
                git::change_id(&repo, &tip, &row.approved_sha),
            ) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            };
            if !same {
                let why = format!(
                    "`{}` moved after its approval (approved {}, now {head}): the change is not the \
                     one that was judged, so it goes back to review",
                    row.branch, row.approved_sha
                );
                if self
                    .store
                    .send_back_to_review(&row.id, &me, &head, &me, &why, gate::now_ms())?
                {
                    self.announce_move("merge", &row.id, "moved", &why);
                    self.announce_move("review", &row.review_id, "waiting", &why);
                }
                return Ok(());
            }
        }
        if git::is_ancestor(&repo, &head, &tip).unwrap_or(false) {
            self.gating_to(
                row,
                MergeState::Landed,
                Move::new(
                    &me,
                    format!(
                        "`{}` is already on `{target}` at {tip}: nothing to rebase and nothing to gate",
                        row.branch
                    ),
                )
                .set("landed_sha", tip.clone()),
            )?;
            return Ok(());
        }
        let landing_root = match (self.env.landing_root)(&repo) {
            Ok(r) => r,
            Err(e) => {
                self.raise(
                    "queue_blocked",
                    format!("`{}` cannot land: {e}", row.branch),
                );
                self.gating_to(row, MergeState::Blocked, Move::new(&me, e))?;
                return Ok(());
            }
        };
        let wt = match git::landing_worktree_in(&landing_root, &repo, &tip) {
            Ok(wt) => wt,
            Err(e) => {
                self.raise(
                    "queue_blocked",
                    format!("`{}` cannot land: {e}", row.branch),
                );
                self.gating_to(row, MergeState::Blocked, Move::new(&me, e))?;
                return Ok(());
            }
        };
        let rebased = match git::rebase_in(&wt, &head, &tip) {
            Ok(sha) => sha,
            Err(e) => {
                let why = format!(
                    "`{}` does not rebase onto `{target}` at {tip}: {e}",
                    row.branch
                );
                // R92.28: said, where the repository's sessions hear it — a parked row nobody is
                // told about is a branch that never lands.
                self.raise("merge_conflict", super::notices::merge_conflict(row, &why));
                self.gating_to(
                    row,
                    MergeState::Conflict,
                    Move::new(&me, why).set("onto_sha", tip.clone()),
                )?;
                return Ok(());
            }
        };
        let (steps, install) = match gate::declared_gate(&repo, &tip) {
            Ok(Some(s)) => s,
            Ok(None) => {
                // Not the branch's fault: back in line until the target declares a gate.
                self.gating_to(
                    row,
                    MergeState::Queued,
                    Move::new(
                        &me,
                        format!(
                            "{}: `{target}` declares no gate at {tip}",
                            super::WAITING_FOR_GATE
                        ),
                    ),
                )?;
                return Ok(());
            }
            Err(e) => {
                self.raise(
                    "queue_blocked",
                    format!(
                        "`{}` cannot land: the gate could not be read from `{target}` at {tip}: {e}",
                        row.branch
                    ),
                );
                self.gating_to(row, MergeState::Blocked, Move::new(&me, e))?;
                return Ok(());
            }
        };
        self.merge_to(
            row,
            MergeState::Gating,
            MergeState::Gating,
            Move::new(
                &me,
                format!(
                    "rebased onto {tip} as {rebased}; running `{target}`'s gate ({} step(s), {} gate)",
                    steps.len(),
                    self.env.gate.name()
                ),
            )
            .owned_by(&me)
            .set("onto_sha", tip.clone())
            .set("rebased_sha", rebased.clone()),
        )?;
        // The gitignored inputs the project names, copied in AFTER the rebase (whose checkout
        // would not touch them, but a fresh landing tree has none).
        let carried = self.gate_carry();
        if let Err(e) = carry::carry(&repo, &wt, &carried) {
            self.raise(
                "queue_blocked",
                format!("`{}` was not gated: {e}", row.branch),
            );
            self.gating_to(row, MergeState::Blocked, Move::new(&me, e))?;
            return Ok(());
        }
        self.start_gate(
            GateFor::Land(Box::new(Landing {
                row: row.clone(),
                tip: tip.clone(),
                head,
                rebased: rebased.clone(),
                wt: wt.clone(),
                carried,
                install,
            })),
            wt,
            steps,
            &rebased,
        );
        Ok(())
    }

    /// **The verdict of a landing's gate** — and, when it passed, the compare-and-swap, the push
    /// and the install.
    fn finish_land(&self, landing: Box<Landing>, run: GateRun) -> Result<(), StoreError> {
        let me = self.me().to_string();
        let repo = self.repo.clone();
        let row = &landing.row;
        let target = row.target.clone();
        let (tip, rebased, wt, carried) = (
            landing.tip.clone(),
            landing.rebased.clone(),
            landing.wt.clone(),
            landing.carried.clone(),
        );
        let rows_json = |steps: &[letibot_tokencore::store::MergeGateStep]| {
            serde_json::to_string(steps).unwrap_or_else(|_| "[]".into())
        };
        match run.verdict.clone() {
            GateVerdict::Passed => {}
            GateVerdict::Failed(w) if super::setup::setup_failure(&run.steps).is_some() => {
                // The branch was never judged: its gate's own setup failed after the build and
                // the tests passed (R76 item 5). Parked as `blocked` — `/queue restart` once the
                // gate is fixed — never `failed`, which says the branch is bad.
                let setup = super::setup::setup_failure(&run.steps).unwrap_or_default();
                self.raise(
                    "queue_blocked",
                    format!("`{}` was not judged: {setup}", row.branch),
                );
                self.gating_to(
                    row,
                    MergeState::Blocked,
                    Move::new(&me, format!("{setup}\n\n{w}"))
                        .set("gate_steps_json", rows_json(&run.steps)),
                )?;
                return Ok(());
            }
            GateVerdict::Failed(w) => {
                let w = self.with_carry_hint(w, &wt, &carried);
                self.raise("merge_failed", super::notices::merge_failed(row, &w));
                self.gating_to(
                    row,
                    MergeState::Failed,
                    Move::new(&me, w).set("gate_steps_json", rows_json(&run.steps)),
                )?;
                return Ok(());
            }
            GateVerdict::TimedOut(w) => {
                self.raise(
                    "queue_blocked",
                    format!("the gate of `{}` timed out: {w}", row.branch),
                );
                self.gating_to(
                    row,
                    MergeState::TimedOut,
                    Move::new(&me, w).set("gate_steps_json", rows_json(&run.steps)),
                )?;
                return Ok(());
            }
            GateVerdict::Stopped(w) => {
                self.gating_to(
                    row,
                    MergeState::Queued,
                    Move::new(&me, format!("{w}; it goes back in line and is gated again"))
                        .set("gate_steps_json", rows_json(&run.steps)),
                )?;
                return Ok(());
            }
            GateVerdict::Infra(w) => {
                // A step killed at the gate's memory cap RAN and was killed — "could not run" was
                // the wrong sentence, and the verb that gates a branch row again is `restart`
                // (R92.25).
                let said = if w.starts_with(gate::GATE_KILLED) {
                    super::notices::merge_gate_killed(row, &w)
                } else {
                    format!("the gate of `{}` could not run: {w}", row.branch)
                };
                self.raise("queue_blocked", said);
                self.gating_to(
                    row,
                    MergeState::Blocked,
                    Move::new(&me, w).set("gate_steps_json", rows_json(&run.steps)),
                )?;
                return Ok(());
            }
        }
        // The gate ran on its own thread while the pass went on: the lease is asked again
        // before anything is written into the repository — a daemon that no longer holds it must
        // not land into it.
        if !self.beat().unwrap_or(true) {
            self.gating_to(
                row,
                MergeState::Queued,
                Move::new(
                    &me,
                    "this daemon lost the repository's lease, so it may not land; it goes back in \
                     line and is gated again",
                )
                .set("gate_steps_json", rows_json(&run.steps)),
            )?;
            return Ok(());
        }
        // **The compare-and-swap.** The target moved during the gate → rebase and gate again,
        // not fail (requirement 3).
        if let Err(e) = git::update_ref_cas(&repo, &target, &rebased, &tip) {
            self.gating_to(
                row,
                MergeState::Queued,
                Move::new(
                    &me,
                    format!(
                        "`{target}` moved during the gate (it was {tip}): {e}. It goes back in line \
                         and is rebased onto the new tip and gated again."
                    ),
                )
                .set("gate_steps_json", rows_json(&run.steps)),
            )?;
            return Ok(());
        }
        if let Err(e) = git::follow_checkouts(&repo, &target, &tip, &rebased) {
            let undone = git::update_ref_cas(&repo, &target, &tip, &rebased);
            let why = format!(
                "{e}. The landing was {}: `{target}` is at {tip} as before.",
                if undone.is_ok() {
                    "undone"
                } else {
                    "NOT undone (the ref moved again)"
                }
            );
            self.raise(
                "queue_blocked",
                format!("`{}` could not land: {why}", row.branch),
            );
            self.gating_to(
                row,
                MergeState::Blocked,
                Move::new(&me, why).set("gate_steps_json", rows_json(&run.steps)),
            )?;
            return Ok(());
        }
        // **Push, then install** (decision 7).
        let pushed = git::push(&repo, &target, &rebased);
        let (pushed_text, may_install) = match &pushed {
            git::Pushed::Yes(s) => (s.clone(), true),
            git::Pushed::NoRemote => ("no `origin` to push to".to_string(), true),
            git::Pushed::Failed(e) => (format!("the push FAILED: {e}"), false),
        };
        let mut install_note = String::new();
        if !landing.install.is_empty() {
            if may_install {
                let install = landing.install.clone();
                self.start_gate(
                    GateFor::Install {
                        landing,
                        gate: run,
                        pushed: pushed_text,
                    },
                    wt,
                    install,
                    &rebased,
                );
                return Ok(());
            } else {
                install_note = " Not installed: the push failed, and nothing is installed that is \
                                not where it was pushed (decision 7)."
                    .into();
            }
        }
        if !may_install {
            // R92.33: a failed push is said whether or not the project has an install step.
            self.raise(
                "queue_push_failed",
                format!(
                    "`{}` landed on `{target}` locally but {pushed_text}.{install_note}",
                    row.branch
                ),
            );
        }
        let steps = run.steps.clone();
        self.landed(&landing, run, steps, pushed_text, install_note)
    }

    /// **The verdict of the install** that followed a pushed landing (decision 7).
    fn finish_install(
        &self,
        landing: Box<Landing>,
        gate: GateRun,
        pushed: String,
        inst: GateRun,
    ) -> Result<(), StoreError> {
        let mut all_steps = gate.steps.clone();
        all_steps.extend(inst.steps);
        let install_note = match inst.verdict {
            GateVerdict::Passed => " Installed after the push.".into(),
            other => {
                let w = format!("{other:?}");
                // Landed, so nothing is blocked: its own code (R92.33).
                self.raise(
                    "queue_install_failed",
                    format!(
                        "`{}` landed but its install did not: {w}",
                        landing.row.branch
                    ),
                );
                format!(" The install did not complete: {w}")
            }
        };
        self.landed(&landing, gate, all_steps, pushed, install_note)
    }

    /// **Landed** — the row, the target's health at the landed tree, the branch retired.
    fn landed(
        &self,
        landing: &Landing,
        run: GateRun,
        all_steps: Vec<letibot_tokencore::store::MergeGateStep>,
        pushed_text: String,
        install_note: String,
    ) -> Result<(), StoreError> {
        let me = self.me().to_string();
        let repo = &self.repo;
        let row = &landing.row;
        let target = row.target.clone();
        let (tip, head, rebased) = (&landing.tip, &landing.head, &landing.rebased);
        let rows_json = |steps: &[letibot_tokencore::store::MergeGateStep]| {
            serde_json::to_string(steps).unwrap_or_else(|_| "[]".into())
        };
        let why =
            format!("landed on `{target}` at {rebased} (onto {tip}); {pushed_text}.{install_note}");
        self.gating_to(
            row,
            MergeState::Landed,
            Move::new(&me, why)
                .set("landed_sha", rebased.clone())
                .set("pushed", pushed_text)
                .set("gate_steps_json", rows_json(&all_steps)),
        )?;
        // The gate passed on exactly this tree, so the target is green at it — not gated again.
        // Through `target_is_green`, like any green (R92.29): a red tip's review still pending
        // is withdrawn, and an alarm that stood is said to be over.
        let green = TargetHealth {
            repository: self.key.clone(),
            target: target.clone(),
            sha: rebased.clone(),
            state: HealthState::Green,
            checked_ms: gate::now_ms(),
            evidence: format!(
                "the queue's own gate passed on this tree (`{}`)",
                row.branch
            ),
            gate_steps: run.steps,
        };
        self.store.set_target_health(&green, &me)?;
        self.target_is_green(&green)?;
        if git::branch_head(repo, &row.branch).as_deref() == Some(head.as_str()) {
            // **A child still running in that tree keeps it** (R92.24): its session is held by
            // this daemon, and removing its cwd under it leaves every later call failing ENOENT.
            let live = self
                .store
                .review_row(&row.review_id)?
                .map(|r| r.session_id)
                .filter(|s| !s.is_empty() && self.env.door.session_live(s));
            let kept = match &live {
                Some(s) => Err(format!(
                    "the worktree {} and `{}` are kept: the session `{s}` that works in it is \
                     still live. Remove them once it has ended (`git worktree remove` and \
                     `git branch -D`).",
                    row.worktree.as_deref().unwrap_or("?"),
                    row.branch
                )),
                None => git::retire_branch(repo, &row.branch, row.worktree.as_deref()),
            };
            if let Err(kept) = kept {
                // Said where a person reads it (R92.4), not only in the daemon's log.
                self.raise(
                    "queue_branch_kept",
                    format!("after landing `{}`: {kept}", row.branch),
                );
            }
        }
        Ok(())
    }
}

impl Drop for RepoQueue {
    /// A worker dropped mid-gate does not leave its gate running unwatched.
    fn drop(&mut self) {
        self.wind_down();
    }
}

/// **Why a repository's landings wait while its gate runs** — `gate running on <sha> for Nm`.
pub fn gate_running_words(sha: &str, for_ms: u64) -> String {
    let short: String = sha.chars().take(10).collect();
    format!("gate running on {short} for {}m", for_ms / 60_000)
}

/// The `queue_told` key that counts a target tip's gate kills (`""` for the sha: every tip's).
pub fn killed_key(repository: &str, target: &str, sha: &str) -> String {
    format!("killed:{repository}:{target}:{sha}")
}

/// **A target's health a project has to hear about** (R76 item 1): red, timed out, or unjudged
/// because the gate's own setup failed. `blocked` for want of a declared gate has its own notice
/// (`Harness::queue_notices`), and the other blocked causes are the queue's own (a worktree that
/// could not be made) and are raised on the hubs.
pub fn emergency_health(h: &TargetHealth) -> bool {
    match h.state {
        HealthState::Red | HealthState::TimedOut => true,
        HealthState::Blocked => super::setup::unjudged(&h.evidence),
        _ => false,
    }
}

// ===== The supervisor =====

/// **The daemon's queue service** — finds the repositories this daemon may serve, claims their
/// leases, runs a worker per lease held, and on stop joins them (their gates killed, their leases
/// released).
pub struct QueueService {
    store_path: PathBuf,
    workspace: PathBuf,
    env: Arc<QueueEnv>,
    /// **The repositories already said not to be queue targets** (2026-10-26) — one line per
    /// repository per daemon, not one every two seconds: a folder whose repositories never asked
    /// for a queue is the ordinary case, not news.
    not_target_said: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl QueueService {
    pub fn new(store_path: PathBuf, workspace: &Path, env: QueueEnv) -> Self {
        QueueService {
            store_path,
            workspace: std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf()),
            env: Arc::new(env),
            not_target_said: Default::default(),
        }
    }

    /// **Whether this daemon is one of `repo`'s session daemons** — its workspace is a session of
    /// that repository ([`super::serves`]): the checkout, under it, or one of its worktrees. A
    /// workspace that merely contains the repository (`/home/dead`) is not.
    pub fn serves(&self, repo: &Path) -> bool {
        super::serves(&self.workspace, repo)
    }

    pub fn spawn(self, stop: Arc<AtomicBool>) -> std::io::Result<JoinHandle<()>> {
        std::thread::Builder::new()
            .name("queues".into())
            .spawn(move || self.run(&stop))
    }

    /// The supervisor's loop. Returns when `stop` is set and every worker has ended.
    pub fn run(&self, stop: &AtomicBool) {
        let stop_all = Arc::new(AtomicBool::new(false));
        let mut workers: HashMap<String, JoinHandle<()>> = HashMap::new();
        let mut said_schema = false;
        let mut trouble = super::StoreTrouble::default();
        let mut store: Option<Store> = None;
        while !stop.load(Ordering::Relaxed) {
            workers.retain(|_, h| !h.is_finished());
            if store.is_none() {
                match Store::open(&self.store_path) {
                    Ok(s) => store = Some(s),
                    Err(e) => {
                        let wait = trouble.failed("opening the store", &e, &*self.env.door);
                        super::sleep_unless_stopped(wait, stop);
                        continue;
                    }
                }
            }
            let s = store.as_ref().expect("opened above");
            match self.supervise(s, &mut workers, &stop_all, &mut said_schema) {
                Ok(()) => trouble.passed(&*self.env.door),
                Err(e) => {
                    let wait = trouble.failed("finding the queues to serve", &e, &*self.env.door);
                    super::sleep_unless_stopped(wait, stop);
                    continue;
                }
            }
            super::sleep_unless_stopped(Duration::from_secs(2), stop);
        }
        stop_all.store(true, Ordering::Relaxed);
        for (_, h) in workers {
            let _ = h.join();
        }
    }

    fn supervise(
        &self,
        store: &Store,
        workers: &mut HashMap<String, JoinHandle<()>>,
        stop_all: &Arc<AtomicBool>,
        said_schema: &mut bool,
    ) -> Result<(), StoreError> {
        // **A lease whose holder is gone is given up by whoever passes** (2026-10-26), and it is
        // done before anything is served: the rows the operator's pane drew as *served by* were
        // daemons dead for weeks, on repositories no running daemon claims — so `acquire`'s
        // takeover, which needs a claimant, never reached them.
        self.sweep_leases(store)?;
        self.resolve_carried(store)?;
        for repo in self.repositories(store)? {
            let key = repo.display().to_string();
            if workers.contains_key(&key) {
                continue;
            }
            // **A repository whose target declares no `Merge gate` is not a queue target**
            // (2026-10-26): it takes no lease and gets no health row, so no head counts it
            // unjudged and no hub is warned about it. It is asked every pass, so the repository
            // that gains the section starts being served on the next one.
            if let Some(target) = self.refusal(&repo) {
                self.say_not_a_target(&key, &target);
                continue;
            }
            match store.acquire_queue_lease(&key, &self.env.me, gate::now_ms(), LEASE_TTL_MS)? {
                LeaseOutcome::Held { previous } => {
                    let recovered =
                        store.recover_queue(&key, &self.env.me.owner, gate::now_ms())?;
                    // **R76 item 3**: a tip whose last verdict was red, blocked or timed out is
                    // gated again by the daemon that takes the queue — a restart is a re-gate.
                    match super::verbs::regate_at_start(store, &key, &self.env.me.owner) {
                        Ok(t) if !t.is_empty() => {
                            eprintln!("  merge queue: {key}: re-gating {}", t.join(", "))
                        }
                        Ok(_) => {}
                        Err(e) => eprintln!("  merge queue: {key}: no re-gate at start: {e}"),
                    }
                    eprintln!(
                        "  merge queue: serving {key}{}{}",
                        previous
                            .map(|p| format!(" (took over from {}, {})", p.owner, p.build))
                            .unwrap_or_default(),
                        if recovered > 0 {
                            format!(
                                "; {recovered} row(s) its previous owner held went back in line"
                            )
                        } else {
                            String::new()
                        }
                    );
                    let env = self.env.clone();
                    let path = self.store_path.clone();
                    let stop = stop_all.clone();
                    let name = format!(
                        "queue-{}",
                        repo.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    );
                    let handle = std::thread::Builder::new()
                        .name(name)
                        .spawn(move || work(path, repo, env, &stop));
                    match handle {
                        Ok(h) => {
                            workers.insert(key, h);
                        }
                        Err(e) => {
                            eprintln!("  merge queue: the worker for {key} did not start: {e}");
                            let _ =
                                store.release_queue_lease(&key, &self.env.me.owner, gate::now_ms());
                        }
                    }
                }
                LeaseOutcome::HeldBy(_) => {}
                LeaseOutcome::WrongSchema {
                    store: theirs,
                    mine,
                } => {
                    if !*said_schema {
                        *said_schema = true;
                        self.env.door.announce(SessionEvent::Warning {
                            code: "queue_wrong_build".into(),
                            detail: format!(
                                "this daemon serves NO queue: the store's schema is {theirs} and this \
                                 build knows {mine}. A daemon whose build does not match the store \
                                 never takes a queue (R61). Restart it on the current build."
                            ),
                            compaction: None,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// **Every lease whose holder is gone is released, with the history saying why** — see
    /// [`super::lease::gone`] for the two facts (an expired heartbeat, and a pid that is not
    /// running on this host) and why neither alone would do. Returns how many were released.
    ///
    /// Every pass, over the whole store rather than over this daemon's repositories: a lease the
    /// supervisor would never pass on is exactly the one that stays stale for ever.
    fn sweep_leases(&self, store: &Store) -> Result<usize, StoreError> {
        let now = gate::now_ms();
        let expired_before = now.saturating_sub(LEASE_TTL_MS);
        let mut released = 0;
        for lease in store.queue_leases()? {
            let Some(why) = super::lease::gone(&lease, &self.env.me, now) else {
                continue;
            };
            if store.release_stale_lease(
                &lease.repository,
                &lease.owner,
                &self.env.me.owner,
                &why,
                expired_before,
                now,
            )? {
                eprintln!(
                    "  merge queue: gave up the lease of {}: {why}",
                    lease.repository
                );
                released += 1;
            }
        }
        Ok(released)
    }

    /// **Why `repo` is not a queue target, if it is not one** — the target branch it lands on,
    /// when that branch's `AGENTS.md` has no `Merge gate` section (2026-10-26: *the queue is the
    /// repositories that asked for it*). `None` means *serve it* — either the gate is there, or it
    /// could not be read (a git that failed is the queue's own trouble, and the health row says so
    /// at `unjudged`, where it belongs).
    ///
    /// The question is asked of the target's TIP, exactly as [`RepoQueue::health`] asks it, so the
    /// repository that commits a gate is served without anything else being said twice.
    fn refusal(&self, repo: &Path) -> Option<String> {
        // A `[queue]` that does not parse falls back to the defaults here as it does in the
        // worker, which raises `queue_settings_unreadable` itself once it serves the repository.
        let settings = (self.env.settings)(repo).unwrap_or_default();
        let target = settings::target_for(repo, &settings)?;
        let tip = git::branch_head(repo, &target)?;
        match gate::declared_gate(repo, &tip) {
            Ok(None) => Some(target),
            Ok(Some(_)) | Err(_) => None,
        }
    }

    /// Say once — in the daemon's own log, never on a hub — that a repository is not served.
    fn say_not_a_target(&self, key: &str, target: &str) {
        let mut said = self
            .not_target_said
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if said.insert(key.to_string()) {
            eprintln!(
                "  merge queue: {key} is not a queue target: `{target}` has no `Merge gate` \
                 section in AGENTS.md, so it never asked for a queue (nothing is claimed, gated \
                 or counted for it; the section is the whole of what opts a repository in)"
            );
        }
    }

    /// **The one repository this daemon serves** — the one its workspace is a session of, when it
    /// is one. A repository with open rows and no session daemon of its own is NOT taken by a
    /// daemon that happens to sit above it: its queue waits, rows and all, for a daemon seated in
    /// it, and the first one that starts there takes the lease and re-gates what was judged
    /// (`regate_at_start`).
    fn repositories(&self, _store: &Store) -> Result<Vec<PathBuf>, StoreError> {
        // **Every repository of the workspace** — one, or each of a one-org project folder's.
        Ok(super::repositories_of_workspace(&self.workspace)
            .into_iter()
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .collect())
    }

    /// **Rows carried over from v23 name no repository yet** — the daemon that serves where the
    /// row's worktree (or its project's workspace) is fills it in, once, conditionally.
    fn resolve_carried(&self, store: &Store) -> Result<(), StoreError> {
        for r in store.review_rows(None)? {
            if !r.repository.is_empty() || !r.state.is_open() {
                continue;
            }
            let repo = r
                .worktree
                .as_deref()
                .map(Path::new)
                .filter(|p| p.exists())
                .and_then(git::repo_of_worktree)
                .or_else(|| {
                    let root = super::root_session(store, &r.session_id);
                    store
                        .session_workspace(&root)
                        .ok()
                        .flatten()
                        .and_then(|w| git::repo_of_workspace(Path::new(&w)))
                        .and_then(|p| git::repo_of_worktree(&p))
                });
            let Some(repo) = repo else { continue };
            if !self.serves(&repo) {
                continue;
            }
            let settings = (self.env.settings)(&repo).unwrap_or_default();
            let target = settings::target_for(&repo, &settings).unwrap_or_default();
            let head = git::branch_head(&repo, &r.branch).unwrap_or_default();
            let why = format!(
                "carried-over row resolved: repository {}, target `{target}`",
                repo.display()
            );
            store.move_review(
                &r.id,
                r.state,
                r.state,
                &Move::new(&self.env.me.owner, why)
                    .set("repository", repo.display().to_string())
                    .set("target", target)
                    .set("head_sha", head),
                gate::now_ms(),
            )?;
        }
        Ok(())
    }
}

/// **One repository's worker** — passes until the lease is lost or the daemon stops; a store
/// error waits and retries; on stop the lease is released so a successor need not wait it out.
fn work(store_path: PathBuf, repo: PathBuf, env: Arc<QueueEnv>, stop: &AtomicBool) {
    let key = repo.display().to_string();
    let mut trouble = super::StoreTrouble::default();
    let q = loop {
        match Store::open(&store_path) {
            Ok(s) => break RepoQueue::new(s, repo.clone(), env.clone()),
            Err(e) => {
                let wait = trouble.failed("opening the store", &e, &*env.door);
                super::sleep_unless_stopped(wait, stop);
                if stop.load(Ordering::Relaxed) {
                    return;
                }
            }
        }
    };
    let mut last_beat = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match q.pass(stop) {
            Ok(Pass::LostLease) => {
                env.door.announce(SessionEvent::Warning {
                    code: "queue_lease_lost".into(),
                    detail: format!(
                        "this daemon no longer holds the queues of {key} (its heartbeat was \
                         missed, or the store changed schema); another daemon serves them now"
                    ),
                    compaction: None,
                });
                return;
            }
            Ok(Pass::Worked) => {
                trouble.passed(&*env.door);
                last_beat = Instant::now();
            }
            Ok(Pass::Idle) => {
                trouble.passed(&*env.door);
                let until = Instant::now() + env.idle;
                while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                    if q.gate_finished() {
                        // A verdict to apply: the next pass, now.
                        break;
                    }
                    if last_beat.elapsed() >= HEARTBEAT_EVERY {
                        last_beat = Instant::now();
                        if let Ok(false) = q.beat() {
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let wait = trouble.failed("serving the queues", &e, &*env.door);
                super::sleep_unless_stopped(wait, stop);
            }
        }
    }
    // The running gate is killed and its verdict applied while the lease is still held.
    q.wind_down();
    let _ = q
        .store
        .release_queue_lease(&key, &env.me.owner, gate::now_ms());
}

#[cfg(test)]
pub(crate) mod tests;
