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

/// **The clause a target's health row leads with when its gate could not run** — nothing was
/// judged, so nothing is in doubt about the code, and the row stays `gating` until the retry window
/// is out. Read off the row (`health`), so a restart in the middle of the window waits out what is
/// left of it rather than starting the gate again.
pub const UNJUDGED_RETRY: &str = "the gate could not run on this pass";

/// **How long a tip whose gate could not run waits before it is gated again** — a minute.
///
/// The two bounds it sits between: short enough that a host that was merely busy costs the queue a
/// minute and not a `/queue regate`, and long enough that a machine which is down or saturated is
/// not dialled on every pass — the worker's pass loop goes again at once when it worked, so without
/// a window a broken `gate_host` would be one ssh attempt per second, for ever.
pub const UNJUDGED_RETRY_MS: u64 = 60 * 1000;

/// **The clause a merge row leads with when the TRAIN it rode in was red** — its own gate has not
/// run, and it is next in line to be gated alone.
///
/// It is what keeps a red train from becoming a loop: the rows that were in it go back to `queued`
/// with this sentence, and [`RepoQueue::land_train`] will not put a row carrying it into another
/// train. Its own single gate overwrites the evidence whatever it says, so the sentence lasts
/// exactly until the branch has been judged on its own — which is the one thing a red train could
/// not tell anybody.
pub const TRAIN_SPLIT: &str = "this branch was in a train whose gate was red";

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
    /// **Put the words on a session, not only on its bell** — the operator's complaint of
    /// 2026-10-30: *"those red states must reach top session agent or a gatekeeper"*, and a ring
    /// says *that* something happened while this says *what*. A door that cannot deliver answers
    /// by name rather than claiming a delivery it did not make.
    fn deliver_repository(&self, repository: &str, text: &str) -> Result<String, String> {
        let _ = (repository, text);
        Err(format!("this door delivers to no session of {repository}"))
    }
    /// **Whether this door puts words on sessions at all** — a door with no sessions (a test's, a
    /// bench's) answers `false` and the queue takes today's path exactly: the card, and nothing
    /// said about nobody reading it. A door that does deliver answers `true`, and a delivery it
    /// could not make is raised, because *"told nobody"* must never be silent.
    fn delivers(&self) -> bool {
        false
    }
    /// **Whether `session_id` is live in this daemon now** — a branch's tree is not removed from
    /// under a session still working in it (R92.24), and a reviewed row is not re-seated under a
    /// reviewer that is still going. **A session this daemon has spawned and not yet opened
    /// counts**: its handle exists, so it is not gone — see `Registry::announce_spawn`.
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
    /// **The daemon's review desk** — who is reviewing what, and what they have handed in.
    ///
    /// The worker needs it for one question only: whether a reviewer whose session is gone has
    /// already handed its verdict in, because a reviewer's LAST act is the submission and its turn
    /// ending, and a session whose turn has ended is not live. Without it the worker called that
    /// child *gone* and re-seated a row that had just been judged — the verdict then reached a pass
    /// that found the row was no longer its reviewer's, and was discarded (measured 2026-10-29: nine
    /// reviewers, eighteen `reviewing` transitions, no `approved`). `None` is a queue whose daemon
    /// has no desk to ask (a fixture that serves rows without reviewers), and it answers as it did
    /// before: no verdict is waiting that anybody could see.
    pub desk: Option<Arc<letibot_tools::builtins::verdict::ReviewDesk>>,
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
    /// `leticode.toml` — and the desk its review session opens reviews on.
    pub fn production(
        door: Arc<dyn QueueDoor>,
        landing_root: Option<PathBuf>,
        desk: Arc<letibot_tools::builtins::verdict::ReviewDesk>,
    ) -> Self {
        QueueEnv {
            me: super::lease::this_daemon().clone(),
            gate: Arc::new(gate::LocalGate),
            door,
            desk: Some(desk),
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

/// **One terminal row's hold on a worktree and a branch** — the facts the resource sweep needs,
/// whichever queue the row is in. A review row and the merge row it was approved into name the
/// same tree, so the tree is examined once and both records get the note.
struct Held<'a> {
    branch: &'a str,
    /// `''` when the row names no tree — then the sweep costs no `git` call at all.
    worktree: &'a str,
    target: &'a str,
    /// The session that works in the tree; `''` when the row names none, which a merge row whose
    /// review row is gone does.
    session: &'a str,
    /// What the record keeps, for the note: `` `landed_sha` 0f2d9550 ``.
    keeps: String,
}

/// **What the sweep found at one tree** — what went, and what stayed with the reason it did. Two
/// rows that name one tree share one of these, so the `git` work is done once and both records say
/// the same thing happened.
#[derive(Clone, Default)]
struct Settled {
    /// What went, as the note says it: `the worktree /x`, `` the branch `agent/y` ``.
    released: Vec<String>,
    /// What stayed: the kind of reason (which is the note's key) and the sentence for a person.
    kept: Option<(&'static str, String)>,
}

/// **What a release says on the row's record** — one sentence, in the row's own terms: what went,
/// when, and what the record still keeps. `None` when nothing went and nothing is kept, which is
/// the row whose tree and branch were both already gone.
fn release_note(found: &Settled, keeps: &str, now_ms: u64) -> Option<String> {
    let at = letibot_claude::handover::iso_ts(now_ms);
    let kept = found.kept.as_ref().map(|(_, why)| why.as_str());
    match (found.released.is_empty(), kept) {
        (true, None) => None,
        (false, None) => Some(format!(
            "resources released {at}: {}; the record keeps {keeps}",
            found.released.join(" and ")
        )),
        (false, Some(why)) => Some(format!(
            "resources released {at}: {}; resources kept: {why}",
            found.released.join(" and ")
        )),
        (true, Some(why)) => Some(format!("resources kept {at}: {why}")),
    }
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
    /// **Several branches rebased one onto the last, gated once** (`[queue] gate_train`).
    Train(Box<Train>),
    /// The install steps, after the landing was pushed (decision 7).
    Install {
        landing: Box<Landing>,
        gate: GateRun,
        pushed: String,
    },
    /// The install steps of a TRAIN, after its last car was pushed.
    InstallTrain {
        train: Box<Train>,
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

/// **A train** — several approved branches, rebased one onto the last, judged by ONE gate run on
/// the final tree (`[queue] gate_train`).
///
/// The cars are in the order they were rebased and are to be landed: each one's `rebased` is a
/// commit on top of the previous car's, so `tip → car 1 → car 2 → …` is a chain of single commits
/// and the target moves once per car. That is what keeps a train bisectable: every row's
/// `landed_sha` is the tip at its own landing, exactly as a lone landing's is.
///
/// It lives in the gate's slot, on this worker, for the length of the gate — so a daemon that dies
/// mid-train leaves its rows `gating` with no gate, which is what `adopt_orphans` already puts back
/// in line. Nothing about a train needs the store to remember it.
struct Train {
    /// The target tip the train was built on — the first compare-and-swap expects exactly this.
    tip: String,
    /// The cars, oldest first.
    cars: Vec<TrainCar>,
    wt: PathBuf,
    carried: Vec<String>,
    install: Vec<String>,
}

/// **One branch in a train** — the row, the head it was approved at, and the commit it became.
struct TrainCar {
    row: MergeRow,
    head: String,
    /// The commit this car's branch became on top of the car before it. `''` until it is rebased.
    rebased: String,
}

impl Train {
    /// **This car's landing, as `landed` wants it** — the tip it was put onto, and the commit it
    /// became. `landed` writes the row, the target's health at that commit, and retires the
    /// branch; every one of those is true of a car in a train, one car at a time.
    fn landing(&self, i: usize) -> Landing {
        let car = &self.cars[i];
        let tip = match i {
            0 => self.tip.clone(),
            n => self.cars[n - 1].rebased.clone(),
        };
        Landing {
            row: car.row.clone(),
            tip,
            head: car.head.clone(),
            rebased: car.rebased.clone(),
            wt: self.wt.clone(),
            carried: self.carried.clone(),
            install: self.install.clone(),
        }
    }

    /// The branches in the order they were rebased — `agent/a`, `agent/b` — for a sentence.
    fn branches(&self) -> Vec<String> {
        self.cars.iter().map(|c| c.row.branch.clone()).collect()
    }

    /// **The order, written out** — `1. `agent/a` (m-a) at 0f2d9550` — what a red train hands the
    /// reviewer and every one of its rows.
    fn order(&self) -> String {
        self.cars
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}. `{}` ({}) at {}", i + 1, c.row.branch, c.row.id, c.head))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// **What a row that landed in a train says about the train** — `, in a train of 3 (car 2)`. `''`
/// for a lone landing, whose sentence is the one it has always been.
fn train_note(n: usize, i: usize) -> String {
    format!(", in a train of {n} (car {})", i + 1)
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
        // Beside the other sweeps, and after them: a row that went terminal in this pass has its
        // resources released in the same one.
        self.release_terminal_resources()?;
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
            worked |= self.land_next(&target, &h, &settings)?;
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

    /// **Said before, in this worker?** — the in-memory half of [`Self::first_time`], for a
    /// question asked once per row per pass: a settled row must cost a hash lookup, not a store
    /// query and two `git` calls.
    fn said(&self, key: &str) -> bool {
        self.rung
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(key)
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
        gate::watch(&wt, &self.key);
        let cancel = Arc::new(AtomicBool::new(false));
        let lease = self.lease_ok.clone();
        let runner = self.env.gate.clone();
        let deadline = Instant::now() + self.env.gate_deadline;
        let budget = self.env.gate_deadline;
        let stop = cancel.clone();
        let short: String = sha.chars().take(8).collect();
        let run = std::thread::Builder::new()
            .name(format!("gate-{short}"))
            .spawn(move || {
                let alive = move || lease.load(Ordering::Relaxed);
                let ctl = GateControl {
                    deadline,
                    budget,
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
            GateFor::Train(train) => self.finish_train(train, run),
            GateFor::Install {
                landing,
                gate,
                pushed,
            } => self.finish_install(landing, gate, pushed, run),
            GateFor::InstallTrain {
                train,
                gate,
                pushed,
            } => self.finish_install_train(*train, gate, pushed, run),
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
                    // child that finished, died, or never came back through a restart — **or one
                    // that is still OPENING**, which is why that door counts a spawn this daemon
                    // has announced as alive; a reviewer is placed in a VM and its open is a
                    // clone and a boot). The reviewers' host collects a child that ends while
                    // the daemon is up; this is the door for one whose host will never look
                    // again.
                    let gone = self.env.door.sees_sessions()
                        && r.reviewer
                            .as_deref()
                            .is_some_and(|c| !self.env.door.session_live(c));
                    let late = r.deadline_ms.is_some_and(|d| now > d);
                    // **A reviewer that has handed its verdict in is not gone, whatever its
                    // session says** — and the two questions are asked of the same two seconds.
                    // A reviewer's last act is the submission (`review_verdict`, the daemon's
                    // desk) and its turn ending; a session whose turn has ended is not live. So
                    // `gone` alone called a reviewer that had just judged *"not alive … it
                    // finished without handing a verdict in"* and re-seated the row — and the
                    // verdict then reached the pass below (`review::serve`), which found the row
                    // was no longer its reviewer's and threw the judgement away (measured
                    // 2026-10-29: nine reviewers, eighteen `reviewing` transitions, not one
                    // `approved`). The desk is PEEKED here, never taken: taking belongs to the
                    // pass that can write it, and that pass is the one that decides a reviewer's
                    // fate next. Nothing is deferred that has not judged — a reviewer that died
                    // with nothing at the desk keeps its attempt, its re-seat and its honest
                    // sentence — and a deferral cannot last: the daemon's own clock serves the
                    // reviews this daemon holds (`sessions::REVIEW_PASS_EVERY`), so a verdict
                    // waiting here is written by the next review pass whatever the row says.
                    let handed_in = r.reviewer.as_deref().is_some_and(|c| {
                        self.env.desk.as_ref().is_some_and(|d| d.verdict_waiting(c))
                    });
                    if never_started {
                        let why = format!(
                            "no reviewer started for `{}` within {} s of the claim — the review \
                             queue's session did not pick it up. A reviewer that cannot start is \
                             raised (decision 4), not retried in silence.",
                            r.branch,
                            self.env.reviewer_start_grace_ms / 1000
                        );
                        moved |= self.block(r, &why, settings)?;
                    } else if (gone || late) && !handed_in {
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

    /// **A terminal row keeps its record and gives back its resources** — the sweep.
    ///
    /// MEASURED 2026-10-29 on this box, and it is why this exists: `review_queue` held **5
    /// `approved`** and **52 `withdrawn`** rows that name a `worktree`, and **nine of those
    /// directories were still there**; `~/Projects/letibot/worktrees` was **36 GB — 32 GB of it
    /// ONE review's tree** (a checkout, and whatever a build had left inside it);
    /// `~/Projects/stroppy-io/worktrees` was another **5.9 GB**. The only retirement was
    /// [`Self::landed`]'s, and it fires only when the branch's head is still the head the row was
    /// cut at and no live session holds the tree — so every `rejected`, `cannot_decide` and
    /// superseded `withdrawn` row kept its tree for ever, a landing done by hand (`git merge
    /// --ff-only`, which the operator does when the queue is slow) never entered that path at all,
    /// and a branch that had moved since the row was cut was left in silence. A person cleared
    /// eight of them by hand and took that directory from 36 GB to 95 MB: that is a sweep's job.
    ///
    /// **Terminal is the store's own answer** ([`ReviewState::is_open`], [`MergeState::is_open`]),
    /// not a list of names: a `blocked` review is a row a person's `restart` moves, so its tree is
    /// still the queue's business — and a blocked or rejected branch carries work the target does
    /// not have, which keeps it in any case (below). And **an `approved` review whose landing is
    /// still open is not terminal for the sweep**: `approved` is the REVIEW queue's terminal, the
    /// merge queue has not started with it, and a landing with no branch cannot rebase. So the
    /// work is released when the work is over, not when the first of the two queues is.
    ///
    /// **The record stays.** The row keeps its branch, its `base_sha`/`head_sha`/`reviewed_sha`/
    /// `landed_sha`, its verdict and its reasons, and the release is written where a reader of the
    /// row finds it — a move of the row's RECORD (`from == to`, [`Store::record_transition`]) that
    /// says what went, when, and which sha the row still keeps. It is deliberately not a move of
    /// the ROW: that rewrites `state_ms`, and a finished row's `state_ms` is what the pane's done
    /// list is keyed on (`ui::panes::queue::RECENT_MS` is a day, so a two-day-old `withdrawn` row
    /// would come back as just-finished) and what a project's ring is keyed on
    /// ([`Self::ring_the_projects`], so a `cannot_decide` row would wake its project again).
    ///
    /// **What goes, and what stays**, in three rules:
    ///
    /// * a tree a live session works in is kept whatever else is true (R92.24) — and released by a
    ///   later pass once that session has ended, which is the part a person used to do by hand;
    /// * **the COMMITS decide, and never the row's state** — see the invariant below. A branch goes
    ///   only when its tip is provably on the target (`git merge-base --is-ancestor`), which is what
    ///   a landing is whether the queue made it or a person did, and a tree goes only under the same
    ///   proof about the commit its own `HEAD` names;
    /// * anything else is kept — a branch that carries work, a tree whose `HEAD` carries work, a tree
    ///   with uncommitted changes, a tree that is not this repository's, and anything whose proof
    ///   could not be taken — said on the row's record and raised where a person reads it, because
    ///   it may be the only copy of that work.
    ///
    /// # The invariant
    ///
    /// **Nothing in the queue may delete a ref, or a worktree, whose commits are not provably on the
    /// target.** Not a terminal review row, not a `blocked` landing, not a person's withdrawal: a row
    /// is terminal in TWO queues for different reasons (`approved` is the REVIEW queue's end and says
    /// nothing about the target), so a row's state can say *the queue is done with this* and never
    /// *this work is safe*. The proof is [`git::ref_proof`] for a branch and [`git::commit_proof`] for
    /// a worktree's `HEAD`, and it is three-valued: `On` and `Gone` permit the removal, `Unproven`
    /// does not. A proof that could not be taken — a repository or a ref git will not read, a target
    /// that cannot be named — is `Unproven`, and the safe side of *I could not tell* is keeping.
    ///
    /// MEASURED 2026-10-29: this used to be `branch_head(..).is_none() => may go`, which folded
    /// *the ref is not there* together with *git could not tell*, and the worktree went with it —
    /// `git worktree remove --force` is unrecoverable for a commit no ref holds.
    ///
    /// Cheap: a row that names no worktree costs no `git` call at all, a tree that two rows name
    /// is examined once, and a row the sweep has settled is not looked at again — `first_time`, so
    /// not after a restart either.
    fn release_terminal_resources(&self) -> Result<(), StoreError> {
        let reviews = self.store.review_rows(Some(&self.key))?;
        let merges = self.store.merge_rows(Some(&self.key))?;
        // A merge row names no session of its own; the review row that opened it does, and a row
        // whose review row is gone names none — nothing can be asked about it, and nothing is.
        let session_of = |review_id: &str| {
            reviews
                .iter()
                .find(|r| r.id == review_id)
                .map(|r| r.session_id.as_str())
                .unwrap_or("")
        };
        let landing_open = |review_id: &str| {
            merges
                .iter()
                .any(|m| m.review_id == review_id && m.state.is_open())
        };
        // One examination per (tree, target) per pass: a review row and the merge row it was
        // approved into name the same worktree, branch AND target, and the tree is one thing. The
        // TARGET is in the key because the decision is about a branch's commits *on a target* —
        // two rows naming one tree but different targets are two questions, and a cached verdict
        // must never answer a question it was not asked.
        let mut decided: HashMap<(String, String, String), Settled> = HashMap::new();
        for m in &merges {
            if m.state.is_open() || m.worktree.is_none() {
                continue;
            }
            let keeps = match m.landed_sha.as_deref().filter(|s| !s.is_empty()) {
                Some(landed) => format!("`landed_sha` {landed}"),
                None => format!("`approved_sha` {}", m.approved_sha),
            };
            let held = Held {
                branch: &m.branch,
                worktree: m.worktree.as_deref().unwrap_or(""),
                target: &m.target,
                session: session_of(&m.review_id),
                keeps,
            };
            self.release_one("merge", &m.id, m.state.as_str(), &held, &mut decided)?;
        }
        for r in &reviews {
            if r.state.is_open() || r.worktree.is_none() || landing_open(&r.id) {
                continue;
            }
            let keeps = match r.reviewed_sha.as_deref().filter(|s| !s.is_empty()) {
                Some(judged) => format!(
                    "`head_sha` {} (judged {judged}), `base_sha` {}",
                    r.head_sha, r.base_sha
                ),
                None => format!("`head_sha` {}, `base_sha` {}", r.head_sha, r.base_sha),
            };
            let held = Held {
                branch: &r.branch,
                worktree: r.worktree.as_deref().unwrap_or(""),
                target: &r.target,
                session: &r.session_id,
                keeps,
            };
            self.release_one("review", &r.id, r.state.as_str(), &held, &mut decided)?;
        }
        Ok(())
    }

    /// **What the sweep did with one row's tree**, and where it says so.
    ///
    /// Three keys, and they answer three questions. `released:<row>` is *there is nothing more to
    /// do here* — written once the tree and the branch are both gone (or were never there), so a
    /// settled row costs a hash lookup and no `git` call for the rest of the daemon's life. A row
    /// whose BRANCH is kept is not settled — the target may move and take it — so it is looked at
    /// again on the next pass, and `noted:<row>:<kind>` is what keeps its note to once. And
    /// `kept:<tree>` is *a person has been told about this tree*, which is shared with the two
    /// paths that keep a tree themselves ([`Self::landed`], [`Self::supersede_fix_branches`]): one
    /// tree is one piece of news, however many rows name it and whoever says it first.
    fn release_one(
        &self,
        queue: &str,
        id: &str,
        state: &str,
        held: &Held<'_>,
        decided: &mut HashMap<(String, String, String), Settled>,
    ) -> Result<(), StoreError> {
        let settled = format!("released:{}:{queue}:{id}:{state}", self.key);
        if held.worktree.is_empty() || self.said(&settled) {
            return Ok(());
        }
        let tree = (
            held.worktree.to_string(),
            held.branch.to_string(),
            held.target.to_string(),
        );
        let found = match decided.get(&tree) {
            Some(found) => found.clone(),
            None => {
                let found = self.settle_tree(held);
                decided.insert(tree, found.clone());
                found
            }
        };
        let now = gate::now_ms();
        let Some(note) = release_note(&found, &held.keeps, now) else {
            // Nothing was there: the tree and the branch are both gone already.
            self.first_time(&settled)?;
            return Ok(());
        };
        let key = match &found.kept {
            Some((kind, _)) => format!("noted:{}:{queue}:{id}:{state}:{kind}", self.key),
            None => settled.clone(),
        };
        if !self.first_time(&key)? {
            return Ok(());
        }
        let me = self.me().to_string();
        self.store
            .record_transition(queue, id, &self.key, Some(state), state, &me, &note, now)?;
        if let Some((_, why)) = &found.kept
            && self.first_time(&format!("kept:{}:{}", self.key, held.worktree))?
        {
            // Said where a person reads it (R92.4), not only in the daemon's log — once per tree.
            self.raise(
                "queue_branch_kept",
                format!("the queue's sweep of `{}`'s resources: {why}", held.branch),
            );
        }
        Ok(())
    }

    /// **The decision about one tree** — what may go, and what stays with the reason it does.
    ///
    /// **The commits decide, and the tree follows them.** That is the rule this queue already
    /// applies to a superseded fix (`git::retire_if_empty`: *a branch that carries nothing may go,
    /// a branch that carries the fixer's work and its tree are kept*), and one queue may not hold
    /// two rules for one tree: a sweep that removed the tree of a branch the mover had just said it
    /// was keeping would leave two notes on one row saying opposite things. So a branch the target
    /// already has carries nothing and takes its tree with it, and a branch that carries work keeps
    /// both — the tree is where that work is continued, and it is the only copy of anything the
    /// branch does not hold.
    ///
    /// **Two proofs, and the second one is the tree's own.** The branch's tip is proved on the
    /// target ([`git::ref_proof`]) *and* the commit the worktree's `HEAD` names is proved on it
    /// ([`git::commit_proof`]), because a tree can hold a commit no ref of the row names — a
    /// detached head, a branch the row does not name — and `git worktree remove --force` is
    /// unrecoverable for it. Either proof failing (or not being takeable) keeps both.
    ///
    /// A tree a live session works in is kept whatever the proofs say (R92.24), and released by a
    /// later pass once that session has ended.
    fn settle_tree(&self, held: &Held<'_>) -> Settled {
        let mut found = Settled::default();
        let wt = Path::new(held.worktree);
        let there = wt.exists();
        if there && !held.session.is_empty() && self.env.door.session_live(held.session) {
            found.kept = Some((
                "live",
                format!(
                    "the worktree {} and `{}` are kept: the session `{}` that works in it is \
                     still live, and the sweep releases them once it has ended",
                    wt.display(),
                    held.branch,
                    held.session
                ),
            ));
            return found;
        }
        let with_tree = if there {
            format!(" (its worktree {} is kept with it)", wt.display())
        } else {
            String::new()
        };
        if let Some((kind, why)) = self.branch_may_go(held) {
            found.kept = Some((kind, format!("{why}{with_tree}")));
            return found;
        }
        // **The tree's own commits, under the same proof.** A tree is removed whole, so a commit
        // it holds and no ref does (a detached head, a branch the row does not name) is gone with
        // it — kept here unless it is provably on the target.
        if there {
            match git::head_of(wt) {
                Ok(head) => {
                    let what = format!("the worktree {}'s HEAD ({head})", wt.display());
                    if let Some((_, why)) =
                        git::commit_proof(&self.repo, &head, held.target, &what).unproven()
                    {
                        // The kind is `tree` because the TREE is what is kept: the sentence says
                        // which proof refused it, and `noted:<row>:tree` is what keeps it said once.
                        found.kept = Some(("tree", why));
                        return found;
                    }
                }
                Err(why) => {
                    found.kept = Some((
                        "tree",
                        format!(
                            "the worktree {} is kept: the commit its HEAD names could not be read \
                             ({why}), so whether it is on `{}` could not be told",
                            wt.display(),
                            held.target
                        ),
                    ));
                    return found;
                }
            }
        }
        if there {
            match git::remove_worktree(&self.repo, wt, held.branch) {
                Ok(()) => found
                    .released
                    .push(format!("the worktree {}", wt.display())),
                Err(why) => {
                    // A tree with work in it, when the branch itself is safe: the branch may not
                    // carry what was never committed.
                    found.kept = Some(("tree", why));
                    return found;
                }
            }
        }
        if git::branch_head(&self.repo, held.branch).is_some() {
            match git::delete_branch(&self.repo, held.branch) {
                Ok(()) => found.released.push(format!("the branch `{}`", held.branch)),
                Err(why) => found.kept = Some(("branch", why)),
            }
        }
        found
    }

    /// **Whether a terminal row's branch may go** — `None` when the proof says its commits are on
    /// the target (or the ref is not there at all), else the kind of reason it stays and the
    /// sentence for a person.
    ///
    /// The test is the one a landing makes: *does the target already have everything on this
    /// branch?* (`git merge-base --is-ancestor`). That is a landing by the queue and a landing by
    /// hand alike, and it is what the operator's own `git merge --ff-only` mornings were.
    ///
    /// **`None` means one of two things and never a third.** Either the tip is provably on the
    /// target, or git says there is no such ref — permission to delete what is left, because a ref
    /// that is not there holds no commit. *Git could not tell* is [`git::Proof::Unproven`], and it
    /// keeps.
    fn branch_may_go(&self, held: &Held<'_>) -> Option<(&'static str, String)> {
        let what = format!("the branch `{}`", held.branch);
        git::ref_proof(&self.repo, held.branch, held.target, &what).unproven()
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
                // Said where a person reads it (R92.4), not only in the daemon's log — and once
                // per tree, which is the key the resource sweep reads too ([`Self::release_one`]).
                let key = format!("kept:{}:{wt}", self.key);
                if self.first_time(&key)? {
                    self.raise(
                        "queue_branch_kept",
                        format!("after withdrawing a superseded fix branch: {kept}"),
                    );
                }
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
        // **A gate that could not run waits out its retry window.** The row is `gating` and carries
        // the sentence that stopped it, so the tip is neither judged nor blocked — but the host is
        // not dialled on every pass either: the window is what keeps a machine that is down or busy
        // from being asked once a second. `Gating` with a plain *gating* evidence is a gate that is
        // on its thread right now and has nothing to wait for.
        if let Some(h) = self.store.target_health(&self.key, target)?
            && h.sha == tip
            && h.state == HealthState::Gating
            && h.evidence.starts_with(UNJUDGED_RETRY)
            && now.saturating_sub(h.checked_ms) < UNJUDGED_RETRY_MS
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
            // **A gate that could not run is not a verdict about the code, and it is not a halt.**
            // The tip goes back to `gating` with the sentence that stopped it, and the next pass
            // after the retry window gates it again: a machine that was slow, busy or briefly gone
            // must not stop the queue landing anything, which is what `blocked` does.
            //
            // A step KILLED at the gate's memory cap is the exception, and stays `blocked`: that
            // one ran, so re-running it with the same cap kills it the same way, and the backoff in
            // `verbs::regate_at_start` is what keeps it from being a loop.
            GateVerdict::Infra(w) if !w.starts_with(gate::GATE_KILLED) => {
                return self.unjudged(h, w, run.steps);
            }
            GateVerdict::Infra(w) => (HealthState::Blocked, w, run.steps),
            GateVerdict::Stopped(_) => {
                // Judged nothing: the row stays `gating`, and the next pass (here or in the next
                // lease holder) gates it again.
                return Ok(());
            }
        };
        self.judged(&mut h, state, evidence, steps)
    }

    /// **A gate that could not run** — the tip stays UNJUDGED, the queue keeps moving, and the row
    /// says both. `HealthState::Blocked` is for a tip nobody can judge at all (no declared gate, a
    /// landing tree that cannot be made); a host that did not answer in time is not that, and
    /// treating it as that is what halted both queues on 2026-10-30.
    fn unjudged(
        &self,
        mut h: TargetHealth,
        why: String,
        steps: Vec<letibot_tokencore::store::MergeGateStep>,
    ) -> Result<(), StoreError> {
        let me = self.me().to_string();
        h.state = HealthState::Gating;
        h.evidence = format!(
            "{UNJUDGED_RETRY}: {why}\n\nNothing was judged about the code — the tip is UNJUDGED, \
             not red, and the queue is not halted by it. It is gated again on the next pass once \
             {} s have passed.",
            UNJUDGED_RETRY_MS / 1000
        );
        h.gate_steps = steps;
        h.checked_ms = gate::now_ms();
        self.store.set_target_health(&h, &me)?;
        self.announce_move("target", &h.target, "gating", &h.evidence);
        // Once per tip (`alarm` is deduped on the store), so a host that is down for an hour is one
        // warning and not sixty.
        self.alarm(
            &h,
            "target_unjudged",
            super::notices::target_unjudged(&h, &self.key),
        )?;
        Ok(())
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
            let words = super::notices::target_red_warning(h, &self.key, !fresh);
            self.raise("target_red", words.clone());
            // **And on a session, in the same words.** A ring wakes the repository's roots and
            // leaves what happened to the store; the operator's report of 2026-10-30 — *"those red
            // states must reach top session agent or a gatekeeper"* — is that the sentence itself
            // has to reach a session, because a session that is merely woken cannot act on what it
            // was not told. The card stays the record; this is the copy somebody can act on.
            if self.env.door.delivers()
                && let Err(why) = self.env.door.deliver_repository(&self.key, &words)
            {
                self.raise(
                    "target_red",
                    format!(
                        "…and no session was told about it: {why}. The card is the record; the next \
                         session of {} that wakes reads it",
                        self.key
                    ),
                );
            }
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
    ///
    /// `[queue] gate_train` above 1 makes the taking a TRAIN ([`Self::land_train`]): up to that
    /// many approved rows, rebased one onto the last, judged by ONE gate run. At the default `1`
    /// nothing here differs from the queue that never had a train — not the order, not the
    /// sentences, not the number of gates.
    fn land_next(
        &self,
        target: &str,
        h: &TargetHealth,
        settings: &QueueSettings,
    ) -> Result<bool, StoreError> {
        let mut queued: Vec<MergeRow> = self
            .store
            .merge_rows(Some(&self.key))?
            .into_iter()
            .filter(|m| m.state == MergeState::Queued && m.target == target)
            // **A row whose own gate could not run waits out its window.** It went back in line
            // with the sentence that stopped it (`land`'s Infra arm), and without this it would be
            // taken again on the very next pass — one ssh attempt per second at a host that is
            // down, and a queue that never settles.
            .filter(|m| {
                !(m.evidence.starts_with(UNJUDGED_RETRY)
                    && gate::now_ms().saturating_sub(m.state_ms) < UNJUDGED_RETRY_MS)
            })
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
        let eligible: Vec<MergeRow> = queued.into_iter().filter(|m| allowed(m)).collect();
        // **The train** — and only when every branch at the front of the line is a plain one: a
        // fix lands alone (it is the one thing that may move a red target), and a row that has
        // just come out of a red train is gated on its own first (see [`TRAIN_SPLIT`]). So the
        // train is always exactly the front of the line, never a re-ordering of it.
        if settings.gate_train > 1
            && h.state == HealthState::Green
            && current
            && eligible.len() > 1
            && eligible
                .iter()
                .all(|m| m.fixes.is_none() && !m.evidence.starts_with(TRAIN_SPLIT))
            && self.land_train(target, settings.gate_train, &eligible)?
        {
            return Ok(true);
        }
        let Some(row) = eligible.into_iter().next() else {
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
        // **The base the branch was cut from**, as the store remembers it: the tip this row was last
        // rebased onto, and the base its review recorded. Both survive the target being rewritten —
        // which is what tells the branch's own commits from the old target's, and what a plain
        // `git rebase` gets wrong once the merge base is a commit on the old target.
        let cut_from = self
            .store
            .review_row(&row.review_id)
            .ok()
            .flatten()
            .map(|r| r.base_sha)
            .unwrap_or_default();
        let bases = [row.onto_sha.clone().unwrap_or_default(), cut_from];
        let rebased = match git::rebase_in(&wt, &head, &tip, &target, &bases) {
            Ok(sha) => sha,
            Err(e) => {
                let why = e.sentence(&row.branch, &target, &tip);
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

    // ===== The train =====

    /// **One gate for several branches** — `[queue] gate_train` (`N > 1`).
    ///
    /// The order is DERIVED, not guessed: the queue's own order (priority, then the time the
    /// approval made the row — FIFO within a priority), lifted so that a branch cut from another
    /// branch in the train comes after it ([`Self::train_order`]). The cars are rebased one onto
    /// the last in the landing worktree, and the target's gate runs ONCE on the final tree.
    ///
    /// `false` means *no train was taken, and no row was written*: fewer than two branches can be
    /// in one, so the caller's single path takes the first row exactly as it always did. A train
    /// of one is not a train.
    fn land_train(
        &self,
        target: &str,
        most: usize,
        candidates: &[MergeRow],
    ) -> Result<bool, StoreError> {
        let me = self.me().to_string();
        let Some(tip) = git::branch_head(&self.repo, target) else {
            return Ok(false);
        };
        // What can be a car, asked before anything is written: the branch is still where its
        // approval left it (a branch that moved goes back to review, by the single path), and it
        // carries something the target has not got (a branch already on the target settles).
        let mut could: Vec<MergeRow> = Vec::new();
        for row in self.train_order(candidates) {
            let Some(head) = git::branch_head(&self.repo, &row.branch) else {
                continue;
            };
            if head != row.approved_sha {
                continue;
            }
            if git::is_ancestor(&self.repo, &head, &tip).unwrap_or(false) {
                continue;
            }
            could.push(row);
            if could.len() == most {
                break;
            }
        }
        if could.len() < 2 {
            return Ok(false);
        }
        // Taken in order. A row a person moved between the read above and the take is dropped
        // rather than judged: the take is a compare-and-swap on `queued`.
        let mut cars: Vec<TrainCar> = Vec::new();
        for row in could {
            let why = format!(
                "taken by {me}: car {} of a train of up to {most} onto `{target}` at {tip}",
                cars.len() + 1
            );
            if !self.merge_to(
                &row,
                MergeState::Queued,
                MergeState::Gating,
                Move::new(&me, why).set("owner", &me),
            )? {
                continue;
            }
            let row = self.store.merge_row(&row.id)?.unwrap_or(row);
            let head = row.approved_sha.clone();
            cars.push(TrainCar {
                row,
                head,
                rebased: String::new(),
            });
        }
        if cars.len() < 2 {
            // Nothing to ride with: put back what was taken, and let the single path take it.
            let why = "nothing to ride with: the rest of the train was taken out of line, so this \
                       branch is gated on its own";
            for c in &cars {
                self.gating_to(&c.row, MergeState::Queued, Move::new(&me, why))?;
            }
            return Ok(false);
        }
        let root = match (self.env.landing_root)(&self.repo) {
            Ok(r) => r,
            Err(e) => {
                self.train_park(&cars, MergeState::Blocked, &e, &[])?;
                self.raise(
                    "queue_blocked",
                    format!("a train onto `{target}` cannot land: {e}"),
                );
                return Ok(true);
            }
        };
        let wt = match git::landing_worktree_in(&root, &self.repo, &tip) {
            Ok(wt) => wt,
            Err(e) => {
                self.train_park(&cars, MergeState::Blocked, &e, &[])?;
                self.raise(
                    "queue_blocked",
                    format!("a train onto `{target}` cannot land: {e}"),
                );
                return Ok(true);
            }
        };
        // **The rebases, in order** — each car onto the last, in ONE worktree, so the final HEAD
        // is the tree the gate will judge. A car that will not rebase is parked `conflict` and
        // left out; the train is what is left.
        let mut at = tip.clone();
        let mut kept: Vec<TrainCar> = Vec::new();
        let mut lost = 0usize;
        for c in cars {
            let bases = [
                c.row.onto_sha.clone().unwrap_or_default(),
                self.base_of(&c.row),
            ];
            match git::rebase_in(&wt, &c.head, &at, target, &bases) {
                Ok(sha) => {
                    at = sha.clone();
                    kept.push(TrainCar { rebased: sha, ..c });
                }
                Err(e) => {
                    lost += 1;
                    let why = e.sentence(&c.row.branch, target, &tip);
                    self.raise(
                        "merge_conflict",
                        format!("{why} (it was in a train onto `{target}` at {tip})"),
                    );
                    self.gating_to(
                        &c.row,
                        MergeState::Conflict,
                        Move::new(&me, why).set("onto_sha", tip.clone()),
                    )?;
                }
            }
        }
        if kept.len() < 2 {
            // A train is two or more: what is left goes back in line and is gated alone.
            let why = format!(
                "the train lost {} of its cars to a rebase conflict, so this branch is gated on \
                 its own",
                lost
            );
            for c in &kept {
                self.gating_to(&c.row, MergeState::Queued, Move::new(&me, &why))?;
            }
            return Ok(true);
        }
        let (steps, install) = match gate::declared_gate(&self.repo, &tip) {
            Ok(Some(s)) => s,
            Ok(None) => {
                let why = format!(
                    "{}: `{target}` declares no gate at {tip}",
                    super::WAITING_FOR_GATE
                );
                for c in &kept {
                    self.gating_to(&c.row, MergeState::Queued, Move::new(&me, &why))?;
                }
                return Ok(true);
            }
            Err(e) => {
                self.raise(
                    "queue_blocked",
                    format!("a train onto `{target}` was not gated: {e}"),
                );
                self.train_park(&kept, MergeState::Blocked, &e, &[])?;
                return Ok(true);
            }
        };
        // Each row says where it was put and what is about to judge it, so a row read on its own
        // is a whole sentence about a train and not a fragment of one.
        let mut prev = tip.clone();
        for (i, c) in kept.iter().enumerate() {
            let why = format!(
                "car {} of {}: rebased onto {prev} as {}; the train's gate runs once on the final \
                 tree ({} step(s), {} gate)",
                i + 1,
                kept.len(),
                c.rebased,
                steps.len(),
                self.env.gate.name()
            );
            self.merge_to(
                &c.row,
                MergeState::Gating,
                MergeState::Gating,
                Move::new(&me, why)
                    .owned_by(&me)
                    .set("onto_sha", prev.clone())
                    .set("rebased_sha", c.rebased.clone()),
            )?;
            prev = c.rebased.clone();
        }
        // The gitignored inputs the project names, copied in AFTER the rebases (a fresh landing
        // tree has none), exactly as the single landing does.
        let carried = self.gate_carry();
        if let Err(e) = carry::carry(&self.repo, &wt, &carried) {
            self.raise(
                "queue_blocked",
                format!("a train onto `{target}` was not gated: {e}"),
            );
            self.train_park(&kept, MergeState::Blocked, &e, &[])?;
            return Ok(true);
        }
        self.start_gate(
            GateFor::Train(Box::new(Train {
                tip: tip.clone(),
                cars: kept,
                wt: wt.clone(),
                carried,
                install,
            })),
            wt,
            steps,
            &at,
        );
        Ok(true)
    }

    /// **The order a train's cars go in** — the queue's own order, and a branch cut from another
    /// branch in the train after it.
    ///
    /// The first half is [`Self::land_next`]'s comparator (priority, then the time the approval
    /// made the row, then its id), so a train lands its branches in the order they would have
    /// landed one by one. The second half is the one fact a row records and a clock cannot:
    /// [`Self::stacks_on`]. The sort is stable, so unrelated rows keep their FIFO.
    fn train_order(&self, rows: &[MergeRow]) -> Vec<MergeRow> {
        let mut rows: Vec<MergeRow> = rows.to_vec();
        rows.sort_by_key(|m| (m.priority.rank(), m.created_ms, m.id.clone()));
        let n = rows.len();
        let bases: Vec<String> = rows.iter().map(|m| self.base_of(m)).collect();
        // `stacks[c][p]`: car `c` was cut from car `p`'s own work, so `p` is rebased first.
        let mut stacks = vec![vec![false; n]; n];
        for c in 0..n {
            for p in 0..n {
                if c != p {
                    stacks[c][p] = self.stacks_on(&bases[c], &rows[p], &bases[p]);
                }
            }
        }
        let mut order: Vec<usize> = Vec::with_capacity(n);
        let mut left: Vec<usize> = (0..n).collect();
        while !left.is_empty() {
            // The first row whose parents are all placed. A cycle — two branches each cut from the
            // other's work, which only a rewritten branch can produce — keeps the first of them,
            // because the queue's order is a better guess than no order at all.
            let pick = left
                .iter()
                .position(|&c| !left.iter().any(|&p| p != c && stacks[c][p]))
                .unwrap_or(0);
            order.push(left.remove(pick));
        }
        order.into_iter().map(|i| rows[i].clone()).collect()
    }

    /// **Was `child` cut from `parent`'s own work?** — the child's recorded base against the
    /// parent's head and the base the parent itself was cut from.
    ///
    /// The child's base is on the parent's head and NOT on the parent's base: the commits between
    /// them are the parent's own, so the child is a branch of that branch and rebasing the child
    /// first would replay it without its parent. Two branches cut from one tip are not stacked
    /// (their bases are equal), which is what keeps a queue of unrelated branches in FIFO order.
    fn stacks_on(&self, child_base: &str, parent: &MergeRow, parent_base: &str) -> bool {
        if child_base.is_empty() || parent.approved_sha.is_empty() {
            return false;
        }
        if child_base == parent.approved_sha {
            return true;
        }
        if !git::is_ancestor(&self.repo, child_base, &parent.approved_sha).unwrap_or(false) {
            return false;
        }
        // A base nobody recorded could be anything, so the parent goes first: the safe side.
        parent_base.is_empty()
            || !git::is_ancestor(&self.repo, child_base, parent_base).unwrap_or(false)
    }

    /// **The base the row's branch was cut from**, as the store remembers it — the base its review
    /// recorded, and failing that the tip it was last rebased onto.
    fn base_of(&self, m: &MergeRow) -> String {
        self.store
            .review_row(&m.review_id)
            .ok()
            .flatten()
            .map(|r| r.base_sha)
            .filter(|b| !b.is_empty())
            .or_else(|| m.onto_sha.clone())
            .unwrap_or_default()
    }

    /// **Put every car still this gate's back in `Queued`**, with one sentence and the gate's steps.
    fn train_back(
        &self,
        cars: &[TrainCar],
        why: &str,
        steps: &[letibot_tokencore::store::MergeGateStep],
    ) -> Result<(), StoreError> {
        self.train_move(cars, MergeState::Queued, why, steps)
    }

    /// **Park every car still this gate's in `to`** — `blocked`, `timed_out`.
    fn train_park(
        &self,
        cars: &[TrainCar],
        to: MergeState,
        why: &str,
        steps: &[letibot_tokencore::store::MergeGateStep],
    ) -> Result<(), StoreError> {
        self.train_move(cars, to, why, steps)
    }

    /// The one move [`Self::train_back`] and [`Self::train_park`] are — a car a person has already
    /// taken out of the train is not moved, whatever this gate said.
    fn train_move(
        &self,
        cars: &[TrainCar],
        to: MergeState,
        why: &str,
        steps: &[letibot_tokencore::store::MergeGateStep],
    ) -> Result<(), StoreError> {
        let me = self.me().to_string();
        let json = serde_json::to_string(steps).unwrap_or_else(|_| "[]".into());
        for c in cars {
            let ours = self
                .store
                .merge_row(&c.row.id)?
                .is_some_and(|r| r.state == MergeState::Gating);
            if !ours {
                continue;
            }
            self.gating_to(
                &c.row,
                to,
                Move::new(&me, why).set("gate_steps_json", json.clone()),
            )?;
        }
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
        // **The person may have taken the row out while its gate ran** (`/queue rm` on a `gating`
        // row, `verbs::withdraw_merge`). A landing whose row is no longer `gating` does not land,
        // whatever its gate said — and its verdict is not news about a row nobody wants. The row
        // is read once, here, because everything below writes to the repository.
        if !self
            .store
            .merge_row(&row.id)?
            .is_some_and(|r| r.state == MergeState::Gating)
        {
            return Ok(());
        }
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
            GateVerdict::Infra(w) if !w.starts_with(gate::GATE_KILLED) => {
                // **The branch was not judged, and nothing about it is in doubt** — so it goes back
                // in line rather than being parked `blocked` behind a host that was slow to
                // answer. `land_next` keeps it out of the next passes until its window is out.
                //
                // Said ONCE per row and tip, not once per attempt: the window re-gates the row
                // every minute, and a host down for an hour must not be sixty identical warnings.
                let once = self.first_time(&format!(
                    "unjudged:{}:{}:{}",
                    self.key, row.id, row.approved_sha
                ))?;
                if once {
                    self.raise(
                        "queue_blocked",
                        format!(
                            "`{}` was not judged: {w}. Nothing lands until its gate can run, and it \
                             goes back in line and is gated again",
                            row.branch
                        ),
                    );
                }
                self.gating_to(
                    row,
                    MergeState::Queued,
                    Move::new(
                        &me,
                        format!(
                            "{UNJUDGED_RETRY}: {w}\n\nNothing was judged about the branch, so it \
                             goes back in line and is gated again"
                        ),
                    )
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
        self.landed(&landing, run, steps, pushed_text, install_note, "")
    }

    /// **The verdict of a train's gate** — every car lands when it passed, and the whole train
    /// dissolves when it did not.
    ///
    /// A train is ONE tree, so it is judged as one: a car a person took out of line while the gate
    /// ran takes the whole train with it, and the cars that are still this gate's go back in line.
    /// Nothing has moved at that point — a train is judged before its first compare-and-swap.
    fn finish_train(&self, train: Box<Train>, run: GateRun) -> Result<(), StoreError> {
        let n = train.cars.len();
        let mut taken_out: Option<String> = None;
        for c in &train.cars {
            if !self
                .store
                .merge_row(&c.row.id)?
                .is_some_and(|r| r.state == MergeState::Gating)
            {
                taken_out = Some(c.row.branch.clone());
                break;
            }
        }
        if let Some(branch) = taken_out {
            let why = format!(
                "the train of {n} was dissolved before it landed: `{branch}` was taken out of line \
                 while the train's gate ran, so nothing of the train landed and this branch goes \
                 back in line"
            );
            self.train_back(&train.cars, &why, &run.steps)?;
            return Ok(());
        }
        match run.verdict.clone() {
            GateVerdict::Passed => self.land_the_train(train, run),
            GateVerdict::Failed(w) if super::setup::setup_failure(&run.steps).is_some() => {
                // The branches were never judged: their gate's own setup failed after the build
                // and the tests passed. Parked as `blocked` — `/queue restart` once the gate is
                // fixed — never `failed`, which says a branch is bad.
                let setup = super::setup::setup_failure(&run.steps).unwrap_or_default();
                self.raise(
                    "queue_blocked",
                    format!("a train of {n} was not judged: {setup}"),
                );
                self.train_park(
                    &train.cars,
                    MergeState::Blocked,
                    &format!("{setup}\n\n{w}"),
                    &run.steps,
                )?;
                Ok(())
            }
            GateVerdict::Failed(w) => {
                let w = self.with_carry_hint(w, &train.wt, &train.carried);
                self.red_train(&train, w, &run.steps)
            }
            GateVerdict::TimedOut(w) => {
                self.raise(
                    "queue_blocked",
                    format!("the gate of a train of {n} timed out: {w}"),
                );
                self.train_park(&train.cars, MergeState::TimedOut, &w, &run.steps)?;
                Ok(())
            }
            GateVerdict::Stopped(w) => {
                let why = format!("{w}; it goes back in line and is gated again");
                self.train_back(&train.cars, &why, &run.steps)?;
                Ok(())
            }
            GateVerdict::Infra(w) if !w.starts_with(gate::GATE_KILLED) => {
                // **Not judged, and nothing about the branches is in doubt** — so they go back in
                // line rather than being parked `blocked` behind a host that was slow to answer.
                // Said once per train's tip, not once per attempt: the window re-gates it every
                // minute, and a host down for an hour must not be sixty identical warnings.
                if self.first_time(&format!("train-unjudged:{}:{}", self.key, train.tip))? {
                    self.raise(
                        "queue_blocked",
                        format!(
                            "a train of {n} was not judged: {w}. Nothing lands until its gate can \
                             run, and its branches go back in line and are gated again"
                        ),
                    );
                }
                let why = format!(
                    "{UNJUDGED_RETRY}: {w}\n\nNothing was judged about the train, so its branches \
                     go back in line and are gated again"
                );
                self.train_back(&train.cars, &why, &run.steps)?;
                Ok(())
            }
            GateVerdict::Infra(w) => {
                let said = if w.starts_with(gate::GATE_KILLED) {
                    format!("the gate of a train of {n} was killed: {w}")
                } else {
                    format!("the gate of a train of {n} could not run: {w}")
                };
                self.raise("queue_blocked", said);
                self.train_park(&train.cars, MergeState::Blocked, &w, &run.steps)?;
                Ok(())
            }
        }
    }

    /// **Every car lands** — one commit each, in the order it was rebased.
    ///
    /// The target moves once per car (`tip → car 1 → car 2 → …`), each move a real
    /// compare-and-swap, so the history a person's bisect walks is the tree the gate judged and
    /// every row's `landed_sha` is the tip at its own landing. The push is once, at the end: the
    /// target is one branch, and a push of its last tip carries every commit before it.
    fn land_the_train(&self, train: Box<Train>, run: GateRun) -> Result<(), StoreError> {
        let me = self.me().to_string();
        let target = train.cars[0].row.target.clone();
        let n = train.cars.len();
        let rows_json = |steps: &[letibot_tokencore::store::MergeGateStep]| {
            serde_json::to_string(steps).unwrap_or_else(|_| "[]".into())
        };
        // The gate ran on its own thread while the pass went on: the lease is asked again before
        // anything is written into the repository.
        if !self.beat().unwrap_or(true) {
            let why = "this daemon lost the repository's lease, so it may not land; the train goes \
                       back in line and is gated again";
            self.train_back(&train.cars, why, &run.steps)?;
            return Ok(());
        }
        let mut prev = train.tip.clone();
        let mut landed = 0usize;
        let mut stopped: Option<String> = None;
        for (i, c) in train.cars.iter().enumerate() {
            if let Err(e) = git::update_ref_cas(&self.repo, &target, &c.rebased, &prev) {
                stopped = Some(format!(
                    "`{target}` moved while the train's gate ran (it was {prev}): {e}. The train \
                     stopped at car {} of {n}, and the branches from there on go back in line and \
                     are rebased onto the new tip and gated again",
                    i + 1
                ));
                break;
            }
            if let Err(e) = git::follow_checkouts(&self.repo, &target, &prev, &c.rebased) {
                let undone = git::update_ref_cas(&self.repo, &target, &prev, &c.rebased);
                stopped = Some(format!(
                    "{e}. Car {} of {n} was {}: `{target}` is at {prev} as before, and the \
                     branches from there on go back in line",
                    i + 1,
                    if undone.is_ok() {
                        "undone"
                    } else {
                        "NOT undone (the ref moved again)"
                    }
                ));
                break;
            }
            prev = c.rebased.clone();
            landed = i + 1;
        }
        if let Some(why) = stopped {
            // **A ref moved is a landing**, so the cars that are on the target say so — a row left
            // `gating` about a commit the target holds would be a lie. The rest go back in line.
            // Nothing is pushed: the train never reached its last car.
            let pushed = format!("nothing was pushed: the train stopped after car {landed} of {n}");
            self.raise(
                "queue_blocked",
                format!("a train onto `{target}` stopped: {why}"),
            );
            for i in 0..landed {
                self.landed(
                    &train.landing(i),
                    run.clone(),
                    run.steps.clone(),
                    pushed.clone(),
                    String::new(),
                    &train_note(n, i),
                )?;
            }
            for c in &train.cars[landed..] {
                if self
                    .store
                    .merge_row(&c.row.id)?
                    .is_some_and(|r| r.state == MergeState::Gating)
                {
                    self.gating_to(
                        &c.row,
                        MergeState::Queued,
                        Move::new(&me, &why).set("gate_steps_json", rows_json(&run.steps)),
                    )?;
                }
            }
            return Ok(());
        }
        // **Push, then install** (decision 7) — once, on the last car's tip.
        let pushed = git::push(&self.repo, &target, &prev);
        let (pushed_text, may_install) = match &pushed {
            git::Pushed::Yes(s) => (s.clone(), true),
            git::Pushed::NoRemote => ("no `origin` to push to".to_string(), true),
            git::Pushed::Failed(e) => (format!("the push FAILED: {e}"), false),
        };
        if !train.install.is_empty() && may_install {
            let wt = train.wt.clone();
            let install = train.install.clone();
            self.start_gate(
                GateFor::InstallTrain {
                    train,
                    gate: run,
                    pushed: pushed_text,
                },
                wt,
                install,
                &prev,
            );
            return Ok(());
        }
        let mut install_note = String::new();
        if !may_install {
            // R92.33: a failed push is said whether or not the project has an install step.
            install_note = " Not installed: the push failed, and nothing is installed that is not \
                            where it was pushed (decision 7)."
                .into();
            self.raise(
                "queue_push_failed",
                format!(
                    "a train of {n} landed on `{target}` locally but {pushed_text}.{install_note}"
                ),
            );
        }
        for i in 0..n {
            self.landed(
                &train.landing(i),
                run.clone(),
                run.steps.clone(),
                pushed_text.clone(),
                install_note.clone(),
                &train_note(n, i),
            )?;
        }
        Ok(())
    }

    /// **The verdict of a train's install**, after its last car was pushed (decision 7) — the same
    /// arm as a lone landing's, written on every car's row.
    fn finish_install_train(
        &self,
        train: Train,
        gate: GateRun,
        pushed: String,
        inst: GateRun,
    ) -> Result<(), StoreError> {
        let mut all_steps = gate.steps.clone();
        all_steps.extend(inst.steps);
        let n = train.cars.len();
        let install_note = match inst.verdict {
            GateVerdict::Passed => " Installed after the push.".into(),
            other => {
                let w = format!("{other:?}");
                // Landed, so nothing is blocked: its own code (R92.33).
                self.raise(
                    "queue_install_failed",
                    format!("a train of {n} landed but its install did not: {w}"),
                );
                format!(" The install did not complete: {w}")
            }
        };
        for i in 0..n {
            self.landed(
                &train.landing(i),
                gate.clone(),
                all_steps.clone(),
                pushed.clone(),
                install_note.clone(),
                &train_note(n, i),
            )?;
        }
        Ok(())
    }

    /// **A train's gate was red** — the train dissolves, and the gatekeeper is asked which branch
    /// to fix or drop.
    ///
    /// Nothing was landed and the target never moved: a train is judged BEFORE its first
    /// compare-and-swap, so the tip is still the one the train was built on. What is left behind,
    /// row by row:
    ///
    /// * **one review row** (`train-<target>-<sha>`) — the train's own. It names every branch in
    ///   the order it was rebased, the failing step and the end of its output, and it is a
    ///   `Branch` row about a branch the queue CREATED at the train's final commit
    ///   (`agent/train-…`), so a reviewer started on it works in the exact tree the gate judged —
    ///   the only tree in which the failure can be reproduced — and `git log` reaches every car's
    ///   own commits from there.
    /// * **every merge row back to `queued`**, its evidence leading with [`TRAIN_SPLIT`] and
    ///   carrying the gate's steps. Each is gated ON ITS OWN next: a row carrying that sentence is
    ///   not put into another train, so the queue cannot re-form this train and fail it again.
    /// * **the review rows untouched** — each is still `approved`, bound to the head it judged.
    fn red_train(
        &self,
        train: &Train,
        words: String,
        steps: &[letibot_tokencore::store::MergeGateStep],
    ) -> Result<(), StoreError> {
        let target = train.cars[0].row.target.clone();
        let n = train.cars.len();
        let at = train.cars[n - 1].rebased.clone();
        let short: String = at.chars().take(8).collect();
        let branches = train.branches();
        let order = train.order();
        // **The train's own tree, as a branch** — the tree the gate judged, at a ref a reviewer can
        // be given. It is the queue's, it is disposable, and it is named for the commit it is at.
        let branch = format!("agent/train-{}-{short}", target.replace('/', "-"));
        if let Err(e) = git::git(&self.repo, &["branch", "-f", &branch, &at]) {
            // Not fatal: the review row is still the record, and its brief says the tree may be
            // gone.
            eprintln!("  merge queue: the train's tree could not be kept as `{branch}`: {e}");
        }
        let id = format!("train-{}-{short}", target.replace('/', "-"));
        let failing = steps
            .iter()
            .find(|s| s.outcome == letibot_tokencore::store::MergeGateOutcome::Failed)
            .map(|s| s.command.clone())
            .unwrap_or_default();
        let brief = format!(
            "A train of {n} branches was rebased onto `{target}` at {tip} and gated ONCE, together, \
             and that gate is RED. The branches, in the order they were rebased:\n\n{order}\n\nThe \
             tree the gate judged is the branch `{branch}` at {at}, which is this row's tree: \
             reproduce there, and `git log`/`git show` reach each branch's own commits from it.\n\n\
             Decide which branch to fix or drop{step}. The failing output's end:\n\n{out}",
            tip = train.tip,
            step = if failing.is_empty() {
                String::new()
            } else {
                format!(" at `{failing}`")
            },
            out = super::gate::for_the_model(&words),
        );
        let fresh = self.store.enqueue_review(
            &NewReview {
                id: id.clone(),
                repository: self.key.clone(),
                target: target.clone(),
                kind: ReviewKind::Branch,
                session_id: String::new(),
                branch: branch.clone(),
                worktree: None,
                base_sha: train.tip.clone(),
                head_sha: at.clone(),
                brief,
                priority: letibot_tokencore::store::MergePriority::Urgent,
                fixes: None,
                evidence: format!(
                    "the gate of a train of {n} ({}) was red at {at}",
                    branches.join(", ")
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
                "the train's red gate goes to review: which branch to fix or drop",
            );
        }
        // **The rows go back in line, one at a time** — and the sentence is what keeps them out of
        // the next train, so the queue cannot re-form this one and fail it again.
        let why = format!(
            "{TRAIN_SPLIT}: the train of {n} ({}) onto `{target}` at {tip} failed its gate, so this \
             branch is gated on its own next. The train's evidence is the review row `{id}`",
            branches.join("`, `"),
            tip = train.tip,
        );
        self.train_back(&train.cars, &why, steps)?;
        self.raise(
            "merge_failed",
            format!(
                "the gate of a train of {n} onto `{target}` at {tip} is RED — nothing landed and \
                 the branches ({}) are back in line, each to be gated on its own. Which of them to \
                 fix or drop is the review row `{id}`'s question.\n\nThe failing output's end:\n\n{}",
                branches.join("`, `"),
                super::gate::for_the_model(&words),
                tip = train.tip,
            ),
        );
        Ok(())
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
        self.landed(&landing, gate, all_steps, pushed, install_note, "")
    }

    /// **Landed** — the row, the target's health at the landed tree, the branch retired.
    ///
    /// `train_note` is what a row that rode in a train adds to its own sentence (`, in a train of
    /// 3 (car 2)`, [`train_note`]); it is `""` for a lone landing, whose sentence is the one it has
    /// always been.
    fn landed(
        &self,
        landing: &Landing,
        run: GateRun,
        all_steps: Vec<letibot_tokencore::store::MergeGateStep>,
        pushed_text: String,
        install_note: String,
        train_note: &str,
    ) -> Result<(), StoreError> {
        let me = self.me().to_string();
        let repo = &self.repo;
        let row = &landing.row;
        let target = row.target.clone();
        let (tip, head, rebased) = (&landing.tip, &landing.head, &landing.rebased);
        let rows_json = |steps: &[letibot_tokencore::store::MergeGateStep]| {
            serde_json::to_string(steps).unwrap_or_else(|_| "[]".into())
        };
        let why = format!(
            "landed on `{target}` at {rebased} (onto {tip}){train_note}; {pushed_text}.{install_note}"
        );
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
            // **The queue's own landing is the proof, and it is taken here too** — the branch goes
            // only when the sha the compare-and-swap put on the target is still on it. A landing
            // is often a REBASE, so the branch's own commits are not ancestors of the target; the
            // commit they became is (`landed_sha`, `rebased`, on the target by the CAS above), and
            // that is what makes this the same fact the sweep proves. A target rewritten past the
            // landing — a force-push, a reset — makes the branch the only copy of the work again,
            // and it is kept and said.
            let proved = git::commit_proof(
                repo,
                rebased,
                &target,
                &format!("the branch `{}` (landed at {rebased})", row.branch),
            );
            let kept = match &live {
                Some(s) => Err(format!(
                    "the worktree {} and `{}` are kept: the session `{s}` that works in it is \
                     still live. Remove them once it has ended (`git worktree remove` and \
                     `git branch -D`).",
                    row.worktree.as_deref().unwrap_or("?"),
                    row.branch
                )),
                None => match proved {
                    git::Proof::On => {
                        git::retire_branch(repo, &row.branch, row.worktree.as_deref())
                    }
                    other => Err(other.unproven().map(|(_, why)| why).unwrap_or_else(|| {
                        format!("`{}` is kept: {rebased} is not on `{target}`", row.branch)
                    })),
                },
            };
            if let Err(kept) = kept {
                // Said where a person reads it (R92.4), not only in the daemon's log — and once
                // per tree, which is the key the resource sweep reads too ([`Self::release_one`]):
                // one tree is one piece of news, whoever says it.
                let key = format!(
                    "kept:{}:{}",
                    self.key,
                    row.worktree.as_deref().unwrap_or("")
                );
                if self.first_time(&key)? {
                    self.raise(
                        "queue_branch_kept",
                        format!("after landing `{}`: {kept}", row.branch),
                    );
                }
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
    /// **The repositories last said not to be queue targets** (2026-10-26) — the SET, sorted, and
    /// not one entry per repository as each was met: the line the log carries is one sentence
    /// about the folder, and it is written again only when this set has changed since it was
    /// written ([`not_targets_to_say`]).
    not_target_said: std::sync::Mutex<Vec<String>>,
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
        let mut not_targets: Vec<(String, String)> = Vec::new();
        for repo in self.repositories(store)? {
            let key = repo.display().to_string();
            if workers.contains_key(&key) {
                continue;
            }
            // **A repository whose target declares no `Merge gate` is not a queue target**
            // (2026-10-26): it takes no lease and gets no health row, so no head counts it
            // unjudged and no hub is warned about it. It is asked every pass, so the repository
            // that gains the section starts being served on the next one. What is said about
            // them is ONE line, at the end of the pass — see [`Self::say_not_a_targets`].
            if let Some(target) = self.refusal(&repo) {
                not_targets.push((key, target));
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
        self.say_not_a_targets(&not_targets);
        Ok(())
    }

    /// **Every lease whose holder is gone is released, with the history saying why** — see
    /// [`super::lease::gone`] for the two facts (an expired heartbeat, and a pid that is not
    /// running on this host) and why neither alone would do. Returns how many were released.
    ///
    /// Every pass, over the whole store rather than over this daemon's repositories: a lease the
    /// supervisor would never pass on is exactly the one that stays stale for ever.
    ///
    /// The HISTORY keeps each lease's own reason — that is the row the queue pane draws, one per
    /// repository, where a repository each is what a reader wants. The terminal gets ONE line for
    /// the set ([`stale_leases_line`]): on 2026-10-26 a one-shot that had died holding six
    /// repositories under one project folder left six *"gave up the lease of …"* lines in the
    /// operator's next daemon, which is the same per-repository noise as the not-a-target sweep.
    fn sweep_leases(&self, store: &Store) -> Result<usize, StoreError> {
        let now = gate::now_ms();
        let expired_before = now.saturating_sub(LEASE_TTL_MS);
        let mut released: Vec<(String, String)> = Vec::new();
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
                let age = now.saturating_sub(lease.heartbeat_ms) / 1000;
                released.push((
                    lease.repository.clone(),
                    if lease.host == self.env.me.host {
                        format!(
                            "pid {} is not running on this box (its heartbeat was {age} s old)",
                            lease.pid
                        )
                    } else {
                        format!(
                            "its holder on {} stopped heartbeating {age} s ago",
                            lease.host
                        )
                    },
                ));
            }
        }
        if !released.is_empty() {
            eprintln!("{}", stale_leases_line(&released));
        }
        Ok(released.len())
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

    /// **One line — in the daemon's own log, never on a hub — for the repositories this pass found
    /// not to be queue targets**, and nothing at all when the set is the one already said.
    ///
    /// The daemon started in the foreground writes its log to the operator's terminal, so a line
    /// per repository arrived interleaved with the banner: fifteen of them, twice, around the
    /// `harnessd: session` header (2026-10-26). A folder whose repositories never asked for a
    /// queue is the ordinary case of one project folder, not fifteen pieces of news, and the
    /// question a person asks it is *why is nothing landing here* — so the line accounts for the
    /// folder and is repeated only when the set changes.
    fn say_not_a_targets(&self, not_targets: &[(String, String)]) {
        let mut said = self
            .not_target_said
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some((now, line)) = not_targets_to_say(&said, &self.workspace, not_targets) else {
            return;
        };
        *said = now;
        eprintln!("{line}");
    }

    /// **The repositories this daemon may serve** — every one its workspace is a session of
    /// ([`super::repositories_of_workspace`]): the checkout it is in, or each repository of a
    /// one-project folder of them. A repository with open rows and no session daemon of its own is
    /// NOT taken by a daemon that happens to sit above it: its queue waits, rows and all, for a
    /// daemon seated in it, and the first one that starts there takes the lease and re-gates what
    /// was judged (`regate_at_start`).
    ///
    /// The singular form of this rule is what the startup line used to read while this pass read
    /// the plural one — see [`super::serving_line`], which is now the same call.
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

/// **What the daemon's log owes the operator about the repositories it will not serve** — either
/// the one line and the set to remember, or `None` when this set is the one already said.
///
/// The state transition is its own function so the *said once* half is a test rather than a claim
/// about `eprintln!`: the daemon started in the foreground writes into the operator's terminal.
pub(crate) fn not_targets_to_say(
    said: &[String],
    workspace: &Path,
    not_targets: &[(String, String)],
) -> Option<(Vec<String>, String)> {
    let mut pairs: Vec<&(String, String)> = not_targets.iter().collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let keys: Vec<String> = pairs.iter().map(|(k, _)| k.clone()).collect();
    if said == keys.as_slice() {
        return None;
    }
    Some((keys, not_a_target_line(workspace, not_targets)))
}

/// **The one line** — the folder, how many of its repositories never asked for a queue, and which
/// ones, because the question it answers is *why is nothing landing here* and the answer is a set.
///
/// One line for the whole folder and not one per repository (2026-10-26): fifteen of these, said
/// twice each around the `harnessd: session` header, buried the banner they were interleaved with.
/// A *nobody asked* line per repository is the ordinary case of a project folder, not news.
pub(crate) fn not_a_target_line(workspace: &Path, not_targets: &[(String, String)]) -> String {
    let mut pairs: Vec<&(String, String)> = not_targets.iter().collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let names: Vec<String> = pairs.iter().map(|(k, _)| repo_name(k)).collect();
    match pairs.as_slice() {
        // Nothing is refused any more: the section is the whole of the opt-in, so this is what a
        // folder looks like after the repositories that wanted a queue committed one, and it is
        // worth the one line because the previous line is still in the log above it.
        [] => format!(
            "  queues: every repository under {} declares a `Merge gate` now — each of them is \
             served when this daemon can take its lease",
            workspace.display()
        ),
        [_] => format!(
            "  queues: 1 repository under {} declares no `Merge gate` — {} is not queued (the \
             section in its AGENTS.md is the whole of what opts a repository in; its target \
             branch is `{}`)",
            workspace.display(),
            names[0],
            pairs[0].1
        ),
        many => format!(
            "  queues: {} repositories under {} declare no `Merge gate` — none of them is queued \
             ({}). The section in a repository's AGENTS.md is the whole of what opts it in",
            many.len(),
            workspace.display(),
            names.join(", ")
        ),
    }
}

/// **One line for the leases a pass gave up** — the same fold as [`not_a_target_line`] and for the
/// same reason: the terminal is the daemon's log when it is started in the foreground, and a
/// project folder's repositories come in sets. The rows keep each lease's own reason.
pub(crate) fn stale_leases_line(stale: &[(String, String)]) -> String {
    let names: Vec<String> = stale.iter().map(|(repo, _)| repo_name(repo)).collect();
    let mut holders: Vec<&str> = stale.iter().map(|(_, h)| h.as_str()).collect();
    holders.sort_unstable();
    holders.dedup();
    let whose = match holders.as_slice() {
        [one] => (*one).to_string(),
        many => format!(
            "{} holders, all gone — the first was {}",
            many.len(),
            many[0]
        ),
    };
    match names.as_slice() {
        [one] => format!("  queues: gave up the lease of {one}: {whose}"),
        many => format!(
            "  queues: gave up {} stale leases ({}): {whose}",
            many.len(),
            many.join(", ")
        ),
    }
}

/// A repository key as the log names it: the last path component, because the line already names
/// the folder it lives in.
fn repo_name(key: &str) -> String {
    Path::new(key)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| key.to_string())
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
