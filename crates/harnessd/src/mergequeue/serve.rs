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
use super::gate::{self, GateControl, GateRunner, GateVerdict};
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
    /// every two seconds.
    rung: std::sync::Mutex<std::collections::HashSet<String>>,
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
        }
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

    /// **A raise** — said on every hub, under a code a head can style: `queue_blocked` (high) and
    /// `target_red` (emergency).
    fn raise(&self, code: &str, detail: String) {
        eprintln!("  merge queue: {code}: {detail}");
        self.env.door.announce(SessionEvent::Warning {
            code: code.into(),
            detail,
            compaction: None,
        });
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
        self.store
            .heartbeat_queue_lease(&self.key, self.me(), gate::now_ms())
    }

    /// **One pass.**
    pub fn pass(&self, stop: &AtomicBool) -> Result<Pass, StoreError> {
        if !self.beat()? {
            return Ok(Pass::LostLease);
        }
        let settings = match (self.env.settings)(&self.repo) {
            Ok(s) => s,
            Err(why) => {
                // A `[queue]` that does not parse is said, and the queue runs on the defaults
                // rather than on a guess at what the file meant.
                self.raise(
                    "queue_settings_unreadable",
                    format!(
                        "the queue settings for {} could not be read: {why}",
                        self.key
                    ),
                );
                QueueSettings::default()
            }
        };
        let mut worked = self.review_upkeep(&settings)?;
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
            let (health, gated) = self.health(&target, stop)?;
            worked |= gated;
            if stop.load(Ordering::Relaxed) {
                return Ok(Pass::Worked);
            }
            if let Some(h) = health {
                if gated && h.state == HealthState::Red {
                    // The red tip was just put up for review: claim it in this pass, not the
                    // next — *"taken first, as soon as a reviewer is free"*.
                    self.review_upkeep(&settings)?;
                }
                worked |= self.land_next(&target, &h, stop)?;
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
            if !self
                .rung
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key.clone())
            {
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

    // ===== The review queue's upkeep =====

    /// Deadlines, the rows that need no reviewer any more, the claims, and the raises. Returns
    /// whether anything moved.
    fn review_upkeep(&self, settings: &QueueSettings) -> Result<bool, StoreError> {
        let now = gate::now_ms();
        let me = self.me().to_string();
        let mut moved = false;
        // **A rewritten target supersedes the reviews of its old tips** (R76 item 4) — before
        // anything else reads them, so a stale one is neither claimed nor raised again.
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
                moved |= self.supersede_if_rewritten(&r)?;
            }
        }
        let rows = self.store.review_rows(Some(&self.key))?;
        let target_default = settings::target_for(&self.repo, settings);

        for r in &rows {
            match r.state {
                // **A reviewer past its deadline, or one that never started** — the attempt is
                // counted, and the third is a raise rather than a fourth (defect 10).
                ReviewState::Reviewing if r.owner == me => {
                    let never_started = r.reviewer.is_none()
                        && now.saturating_sub(r.state_ms) > self.env.reviewer_start_grace_ms;
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
                    } else if late {
                        let attempts = r.attempts + 1;
                        let why = format!(
                            "the reviewer {} was still at it at its deadline and was stopped — attempt \
                             {attempts} of {MAX_REVIEW_ATTEMPTS}",
                            r.reviewer.as_deref().unwrap_or("?")
                        );
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
        // first, as soon as a reviewer is free"*), then priority, then age. Other reviews go on
        // while the target is red (the relaxed rule).
        let rows = self.store.review_rows(Some(&self.key))?;
        let busy = rows
            .iter()
            .filter(|r| r.state == ReviewState::Reviewing)
            .count();
        let mut waiting: Vec<&ReviewRow> = rows
            .iter()
            .filter(|r| r.state == ReviewState::Waiting)
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
            let target = if r.target.is_empty() {
                target_default.clone().unwrap_or_default()
            } else {
                r.target.clone()
            };
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

    /// **The review of a red tip the target no longer holds** (R76 item 4). An amend, a
    /// force-push or a reset leaves the target's tip NOT a descendant of the head the review was
    /// opened on — or the head is gone altogether (pg-noop, 2026-10-11: `red-main-ec3cb01e8a6f`
    /// sat `blocked` on *"fatal: unable to read tree"* after `main` was amended from `ec3cb01` to
    /// `62b0296`). Such a review judges a history nobody has; it is withdrawn, saying so, and the
    /// new tip is gated by the health pass like any change of the target. A tip that merely moved
    /// FORWARD keeps its review: the red may still be in it.
    fn supersede_if_rewritten(&self, r: &ReviewRow) -> Result<bool, StoreError> {
        if r.target.is_empty() || r.head_sha.is_empty() {
            return Ok(false);
        }
        let Some(tip) = git::branch_head(&self.repo, &r.target) else {
            return Ok(false);
        };
        if tip == r.head_sha {
            return Ok(false);
        }
        let exists = git::rev_parse(&self.repo, &r.head_sha).is_some();
        if exists && git::is_ancestor(&self.repo, &r.head_sha, &tip).unwrap_or(true) {
            return Ok(false);
        }
        let me = self.me().to_string();
        let why = format!(
            "superseded: the target was rewritten from {} to {tip}{} — `{}` no longer holds the \
             tip this review was opened on, so it judges nothing; the new tip is gated instead",
            r.head_sha,
            if exists {
                ""
            } else {
                " (that commit no longer exists)"
            },
            r.target
        );
        let mut mv = Move::new(&me, why).set("owner", "").clear("deadline_ms");
        if r.state == ReviewState::Reviewing {
            mv = mv.owned_by(r.owner.clone());
        }
        self.review_to(r, ReviewState::Withdrawn, mv)
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
                format!(
                    "the review of `{}` (`{}`) cannot run and is BLOCKED: {why} {}",
                    r.branch,
                    r.id,
                    if settings.merge_via_session {
                        "This project merges such a branch via its main session, which is told next."
                    } else {
                        "Nothing lands it until a person acts: `/queue restart ID` asks again, \
                         `/queue approve ID` approves it over the reviewer (the merge gate still \
                         runs)."
                    }
                ),
            );
        }
        Ok(ok)
    }

    // ===== The target's health =====

    /// **Gate the target's tip if it changed** (decision 10) — coalesced: whatever the tip is
    /// NOW is what is gated, so a burst of commits is one run. Returns the health and whether a
    /// gate ran.
    fn health(
        &self,
        target: &str,
        stop: &AtomicBool,
    ) -> Result<(Option<TargetHealth>, bool), StoreError> {
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
            return Ok((Some(h), false));
        };
        if let Some(h) = self.store.target_health(&self.key, target)?
            && h.sha == tip
            && h.state != HealthState::Gating
        {
            return Ok((Some(h), false));
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
                    let alive = || self.beat().unwrap_or(true);
                    let ctl = GateControl {
                        deadline: Instant::now() + self.env.gate_deadline,
                        stop,
                        alive: &alive,
                    };
                    let run = self.env.gate.run(&wt, &steps, &ctl);
                    match run.verdict {
                        GateVerdict::Passed => (
                            HealthState::Green,
                            format!(
                                "`{target}` is green at {tip} ({} gate)",
                                self.env.gate.name()
                            ),
                            run.steps,
                        ),
                        // **R76 item 5**: a gate that failed AFTER its build and tests passed
                        // could not judge the code — its own setup failed. The tip stays
                        // unjudged (`blocked`: nothing lands), and nobody is sent to find what
                        // in the code broke it.
                        GateVerdict::Failed(w) => {
                            let w = self.with_carry_hint(w, &wt, &carried);
                            match super::setup::setup_failure(&run.steps) {
                                Some(setup) => {
                                    (HealthState::Blocked, format!("{setup}\n\n{w}"), run.steps)
                                }
                                None => (HealthState::Red, w, run.steps),
                            }
                        }
                        GateVerdict::TimedOut(w) => (HealthState::TimedOut, w, run.steps),
                        GateVerdict::Infra(w) => (HealthState::Blocked, w, run.steps),
                        GateVerdict::Stopped(_) => {
                            // Judged nothing: the next pass (here or in the next lease holder)
                            // gates it again.
                            return Ok((None, true));
                        }
                    }
                }
            },
        };
        h.state = state;
        h.evidence = evidence;
        h.gate_steps = steps;
        h.checked_ms = gate::now_ms();
        self.store.set_target_health(&h, &me)?;
        self.announce_move("target", target, state.as_str(), &h.evidence);
        match state {
            HealthState::Red => self.target_went_red(&h)?,
            HealthState::Green => self.target_is_green(&h)?,
            HealthState::Blocked if h.evidence.starts_with(super::setup::GATE_SETUP_FAILED) => {
                // High, not emergency: the code was never judged red. Nothing lands until the
                // gate can judge it, which is said in the same breath.
                self.raise(
                    "gate_setup_failed",
                    format!(
                        "`{}` at {} in {} is UNJUDGED, not red: {} Nothing lands on it until its \
                         gate can judge it — fix the gate (or the project's `[queue] target`), then \
                         `/queue regate {}`.",
                        h.target,
                        h.sha,
                        self.key,
                        h.evidence.lines().next().unwrap_or_default(),
                        h.target
                    ),
                );
            }
            _ => {}
        }
        Ok((Some(h), true))
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
            out = h.evidence,
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
            self.raise(
                "target_red",
                format!(
                    "EMERGENCY: `{}` is RED at {} in {}. The merge queue lands nothing until it is \
                     green again (a fix branch excepted); the red tip is first in review; \
                     enqueueing and other reviews go on. {}",
                    h.target, h.sha, self.key, h.evidence
                ),
            );
        }
        Ok(())
    }

    /// Green again: the open reviews of earlier red tips are withdrawn.
    fn target_is_green(&self, h: &TargetHealth) -> Result<(), StoreError> {
        for r in self.store.review_rows(Some(&self.key))? {
            if r.kind == ReviewKind::Target && r.target == h.target && r.state.is_open() {
                let why = format!("`{}` is green again at {}", h.target, h.sha);
                let mut mv = Move::new(self.me(), why);
                if r.state == ReviewState::Reviewing {
                    mv = mv.owned_by(r.owner.clone()).set("owner", "");
                }
                self.review_to(&r, ReviewState::Withdrawn, mv)?;
            }
        }
        Ok(())
    }

    // ===== The landing =====

    /// **Take the next approved branch for `target` and land it** — only while the target is
    /// green at its current tip, or, while it is red, only a branch opened to fix it.
    fn land_next(
        &self,
        target: &str,
        h: &TargetHealth,
        stop: &AtomicBool,
    ) -> Result<bool, StoreError> {
        let mut queued: Vec<MergeRow> = self
            .store
            .merge_rows(Some(&self.key))?
            .into_iter()
            .filter(|m| m.state == MergeState::Queued && m.target == target)
            .collect();
        if queued.is_empty() {
            return Ok(false);
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
        self.land(&row, stop)?;
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
    fn land(&self, row: &MergeRow, stop: &AtomicBool) -> Result<(), StoreError> {
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
                self.gating_to(
                    row,
                    MergeState::Conflict,
                    Move::new(
                        &me,
                        format!(
                            "`{}` does not rebase onto `{target}` at {tip}: {e}",
                            row.branch
                        ),
                    )
                    .set("onto_sha", tip.clone()),
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
        let alive = || self.beat().unwrap_or(true);
        let ctl = GateControl {
            deadline: Instant::now() + self.env.gate_deadline,
            stop,
            alive: &alive,
        };
        let run = self.env.gate.run(&wt, &steps, &ctl);
        let rows_json = |steps: &[letibot_tokencore::store::MergeGateStep]| {
            serde_json::to_string(steps).unwrap_or_else(|_| "[]".into())
        };
        match run.verdict {
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
                self.raise(
                    "queue_blocked",
                    format!("the gate of `{}` could not run: {w}", row.branch),
                );
                self.gating_to(
                    row,
                    MergeState::Blocked,
                    Move::new(&me, w).set("gate_steps_json", rows_json(&run.steps)),
                )?;
                return Ok(());
            }
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
        let mut all_steps = run.steps.clone();
        let mut install_note = String::new();
        if !install.is_empty() {
            if may_install {
                let alive = || self.beat().unwrap_or(true);
                let ctl = GateControl {
                    deadline: Instant::now() + self.env.gate_deadline,
                    stop,
                    alive: &alive,
                };
                let inst = self.env.gate.run(&wt, &install, &ctl);
                all_steps.extend(inst.steps);
                install_note = match inst.verdict {
                    GateVerdict::Passed => " Installed after the push.".into(),
                    other => {
                        let w = format!("{other:?}");
                        self.raise(
                            "queue_blocked",
                            format!("`{}` landed but its install did not: {w}", row.branch),
                        );
                        format!(" The install did not complete: {w}")
                    }
                };
            } else {
                install_note = " Not installed: the push failed, and nothing is installed that is \
                                not where it was pushed (decision 7)."
                    .into();
                self.raise(
                    "queue_blocked",
                    format!(
                        "`{}` landed on `{target}` locally but {pushed_text}",
                        row.branch
                    ),
                );
            }
        }
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
        self.store.set_target_health(
            &TargetHealth {
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
            },
            &me,
        )?;
        if git::branch_head(&repo, &row.branch).as_deref() == Some(head.as_str())
            && let Err(kept) = git::retire_branch(&repo, &row.branch, row.worktree.as_deref())
        {
            eprintln!("  merge queue: after landing `{}`: {kept}", row.branch);
        }
        Ok(())
    }
}

/// **A target's health a project has to hear about** (R76 item 1): red, timed out, or unjudged
/// because the gate's own setup failed. `blocked` for want of a declared gate has its own notice
/// (`Harness::queue_notices`), and the other blocked causes are the queue's own (a worktree that
/// could not be made) and are raised on the hubs.
pub fn emergency_health(h: &TargetHealth) -> bool {
    match h.state {
        HealthState::Red | HealthState::TimedOut => true,
        HealthState::Blocked => h.evidence.starts_with(super::setup::GATE_SETUP_FAILED),
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
}

impl QueueService {
    pub fn new(store_path: PathBuf, workspace: &Path, env: QueueEnv) -> Self {
        QueueService {
            store_path,
            workspace: std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf()),
            env: Arc::new(env),
        }
    }

    /// **Whether this daemon is one of `repo`'s session daemons** — the repository is its
    /// workspace, under it, or holds it.
    pub fn serves(&self, repo: &Path) -> bool {
        repo.starts_with(&self.workspace) || self.workspace.starts_with(repo)
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
        self.resolve_carried(store)?;
        for repo in self.repositories(store)? {
            let key = repo.display().to_string();
            if workers.contains_key(&key) {
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

    /// The repositories with open rows this daemon serves, and its own.
    fn repositories(&self, store: &Store) -> Result<Vec<PathBuf>, StoreError> {
        let mut keys: Vec<String> = store
            .review_rows(None)?
            .into_iter()
            .filter(|r| r.state.is_open())
            .map(|r| r.repository)
            .chain(
                store
                    .merge_rows(None)?
                    .into_iter()
                    .filter(|m| m.state.is_open())
                    .map(|m| m.repository),
            )
            .filter(|k| !k.is_empty())
            .collect();
        if let Some(own) =
            git::repo_of_workspace(&self.workspace).and_then(|r| git::repository_key(&r))
        {
            keys.push(own);
        }
        keys.sort();
        keys.dedup();
        Ok(keys
            .into_iter()
            .map(PathBuf::from)
            .filter(|p| self.serves(p) && p.exists())
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
    let _ = q
        .store
        .release_queue_lease(&key, &env.me.owner, gate::now_ms());
}

#[cfg(test)]
pub(crate) mod tests;
