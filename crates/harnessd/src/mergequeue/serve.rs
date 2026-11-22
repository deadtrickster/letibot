//! The queues' own threads - a supervisor per daemon that finds the repositories it may
//! serve and claims their leases, and one worker per repository it holds: the review queue
//! the gate on every change to the target's tip, and landings one at a time.

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

/// A reviewer's time - an hour from the claim. A reviewer still running then is killed and
/// the attempt counted.
pub const REVIEW_DEADLINE_MS: u64 = 60 * 60 * 1000;

/// How long a claimed review may wait for its reviewer to START before the queue raises it as
/// a reviewer that cannot start - the review queue's session never picked it up.
pub const REVIEWER_START_GRACE_MS: u64 = 5 * 60 * 1000;

/// Attempts before a review is `blocked` and raised - three, and then a person, never a model
/// merging without the gate.
pub const MAX_REVIEW_ATTEMPTS: u32 = 3;

/// Reviews a repository runs at once.
pub const MAX_REVIEWING: usize = 2;

/// The clause a target's health row leads with when its gate could not run - nothing was judged
/// so the row stays `gating` until the retry window is out. Read off the row, so a restart in the
/// middle of the window waits out what is left of it rather than starting the gate again.
pub const UNJUDGED_RETRY: &str = "the gate could not run on this pass";

/// How long a tip whose gate could not run waits before it is gated again - a minute: short
/// enough that a busy host costs the queue a minute, long enough that a machine which is down is not
/// dialled on every pass (the loop goes again at once when it worked).
pub const UNJUDGED_RETRY_MS: u64 = 60 * 1000;

/// Carried by a `gating` row that re-gates a red tip. A re-gate is not a verdict, so the
/// branches behind it stay held: a review claimed in that window had its approval spent the
/// moment the same red came back.
pub const STILL_HALTED: &str = "its review queue stays halted until this gate says green";

/// A target that halts its review queue: red, or gating a tip that was red.
pub fn halts_reviews(h: &TargetHealth) -> bool {
    h.state == HealthState::Red
        || (h.state == HealthState::Gating && h.evidence.contains(STILL_HALTED))
}

/// The clause a merge row leads with when the TRAIN it rode in was red. The `[half TAG]` after
/// it names the half of the bisection the row is in, so a red train cannot re-form whole.
pub const TRAIN_SPLIT: &str = "this branch was in a train whose gate was red";

/// The bisection half a queued row is in - `TRAIN_SPLIT [half 0f2d9550:ab]: ...` is `0f2d9550:ab`.
/// The path is `a`/`b` per split, first half first.
pub fn half_of(evidence: &str) -> Option<String> {
    let rest = evidence
        .strip_prefix(TRAIN_SPLIT)?
        .strip_prefix(" [half ")?;
    let tag = &rest[..rest.find(']')?];
    let (gid, path) = tag.split_once(':')?;
    (!gid.is_empty() && !path.is_empty() && path.chars().all(|c| c == 'a' || c == 'b'))
        .then(|| tag.to_string())
}

/// The half that is gated before this one: `…b` waits for `…a`. `None` for a first half.
fn first_half_of(tag: &str) -> Option<String> {
    tag.strip_suffix('b').map(|t| format!("{t}a"))
}

/// The sentence a row of half `tag` waits in line with.
fn half_sentence(tag: &str, rest: &str) -> String {
    format!("{TRAIN_SPLIT} [half {tag}]: {rest}")
}

/// Where the queues reach sessions - the review queue's session, a project's main session
/// and every hub. A trait so the tests record and the daemon rings.
pub trait QueueDoor: Send + Sync {
    /// Publish on every session's log.
    fn announce(&self, event: SessionEvent);
    /// Publish one repository's alert - only on the hubs of that repository's sessions, never on
    /// every hub: a daemon's other sessions are about other things.
    fn announce_to(&self, repository: &str, event: SessionEvent) {
        let _ = repository;
        self.announce(event);
    }
    /// Wake the review queue's session: a claimed review is waiting for its reviewer.
    fn ring_reviewers(&self);
    /// Wake the project session that `session_id` belongs to (its root). What it is told is read
    /// from the store by that session (`Harness::queue_notices`).
    fn ring_project(&self, session_id: &str) -> Result<String, String>;
    /// Wake the root sessions of the project `repository` is - a target emergency has no branch
    /// and so no session of its own. What each is told is read from the store by that
    /// session (`Harness::queue_emergency_notices`).
    fn ring_repository(&self, repository: &str) -> Result<String, String> {
        Err(format!(
            "this door rings no project session of {repository}"
        ))
    }
    /// Put the words on a session, not only on its bell: a ring says that something happened, this
    /// says what. A door that cannot deliver answers by name, not with a false promise.
    fn deliver_repository(&self, repository: &str, text: &str) -> Result<String, String> {
        let _ = (repository, text);
        Err(format!("this door delivers to no session of {repository}"))
    }
    /// Whether this door puts words on sessions at all - a door with no sessions answers `false`
    /// and the queue takes today's path exactly: the card, and nothing said about nobody reading it.
    /// A door that does deliver answers `true`, and a delivery it could not make is raised.
    fn delivers(&self) -> bool {
        false
    }
    /// Whether `session_id` is live in this daemon now - a branch's tree is not removed from
    /// under a session still working in it, and a reviewed row is not re-seated under a
    /// reviewer that is still going. A session spawned and not yet opened counts: its handle exists.
    fn session_live(&self, session_id: &str) -> bool {
        let _ = session_id;
        false
    }
    /// Whether this door has sessions to be asked about at all. A door that does not never reads
    /// as *the child is gone*: [`Self::session_live`]'s default `false` is the right answer to *is
    /// this tree in use* and the wrong one to *has the child that was working on this row died*.
    fn sees_sessions(&self) -> bool {
        false
    }
}

/// How a project's `[queue]` settings are read - the project's `leticode.toml` in production.
pub type SettingsFor = Arc<dyn Fn(&Path) -> Result<QueueSettings, String> + Send + Sync>;

/// Where a repository's landing worktree is made: the directory [`git::landing_worktree_in`]
/// puts it under (letibot's data directory in production, a temporary one in the tests).
pub type LandingRootFor = Arc<dyn Fn(&Path) -> Result<PathBuf, String> + Send + Sync>;

/// What every worker of one daemon shares.
pub struct QueueEnv {
    pub me: LeaseClaim,
    pub gate: Arc<dyn GateRunner>,
    pub door: Arc<dyn QueueDoor>,
    /// The daemon's review desk - who is reviewing what, and what they have handed in. The
    /// worker asks it one question: whether a reviewer whose session is gone has already handed
    /// its verdict in, because a reviewer's LAST act is the submission and its turn ending.
    pub desk: Option<Arc<letibot_tools::builtins::verdict::ReviewDesk>>,
    pub settings: SettingsFor,
    pub landing_root: LandingRootFor,
    /// The whole run's deadline when `[queue] gate_deadline_minutes` says nothing.
    pub gate_deadline: Duration,
    /// The box's one-workspace-run lock, the file `scripts/gate-at.sh` takes: a gate on this
    /// machine waits for it before its first step and holds it to the last. `None` takes none.
    pub gate_lock: Option<PathBuf>,
    pub review_deadline_ms: u64,
    pub reviewer_start_grace_ms: u64,
    /// How long a worker sleeps after a pass with nothing to do.
    pub idle: Duration,
    /// `--reviewers-at-once`, when the daemon's command line named one - the outermost of the
    /// three places the number comes from (flag, project file, the pool's own capacity).
    pub reviewers_at_once: Option<usize>,
    /// What the machines a `host` reviewer may use can take - this daemon's pool
    /// (`review::hosts::ReviewHosts`), shared with the review pool so the per-host cap counts every
    /// reviewer this daemon has running. Read from the last probe, never by waiting for one.
    pub hosts: Arc<crate::mergequeue::review::hosts::ReviewHosts>,
    /// Which machines that pool is - [`crate::config::Config::reviewer_hosts`], where `None` is
    /// *every host the operator declared, plus this machine*.
    pub reviewer_hosts: Option<Vec<String>>,
    /// The daemon's Claude Code reviewers (`[queue.claude]`) - the `claude -p` processes behind rows.
    /// `None` starts none. This tells a process reviewer from a session one when a row is checked
    /// for a gone reviewer: a process has no entry in the session registry.
    pub claude: Option<Arc<crate::mergequeue::review::claude::Runs>>,
}

impl QueueEnv {
    /// How many reviewers the pool can take right now - the pool's own answer for the machines
    /// this daemon may use, and it never touches the network: a snapshot that is stale is refreshed
    /// on another thread.
    pub fn review_capacity(&self) -> usize {
        self.hosts.capacity(self.reviewer_hosts.as_deref())
    }

    /// The daemon's own: this daemon's identity, the local gate, thirty minutes, the project's
    /// `leticode.toml` - the desk its review session opens reviews on, and the pool its reviewers
    /// and workers share.
    pub fn production(
        door: Arc<dyn QueueDoor>,
        landing_root: Option<PathBuf>,
        desk: Arc<letibot_tools::builtins::verdict::ReviewDesk>,
        hosts: Arc<crate::mergequeue::review::hosts::ReviewHosts>,
        reviewers_at_once: Option<usize>,
        reviewer_hosts: Option<Vec<String>>,
        claude: Arc<crate::mergequeue::review::claude::Runs>,
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
            gate_lock: gate::box_gate_lock(),
            review_deadline_ms: REVIEW_DEADLINE_MS,
            reviewer_start_grace_ms: REVIEWER_START_GRACE_MS,
            idle: Duration::from_secs(2),
            reviewers_at_once,
            hosts,
            reviewer_hosts,
            claude: Some(claude),
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
    /// The lease is not this daemon's any more - the worker ends.
    LostLease,
}

/// The worker for one repository - every pass: heartbeat, the review queue's upkeep, the
/// target's health, one landing.
pub struct RepoQueue {
    store: Store,
    repo: PathBuf,
    key: String,
    env: Arc<QueueEnv>,
    /// What this worker has already rung a project about - a red tip, a gate that
    /// could not judge, a row parked for a person - so each is rung once, not every two seconds. A
    /// cache of the store's `queue_told` rows, which are what keep it once per tip across a restart.
    rung: std::sync::Mutex<std::collections::HashSet<String>>,
    /// The `[queue]` error last said - `queue_settings_unreadable` is raised when
    /// the error is new or changed, not every pass (it was every two seconds, on every hub).
    settings_said: std::sync::Mutex<Option<String>>,
    /// The one gate this repository is running, if any. It runs on a thread of its own and the
    /// pass only looks at it, so a thirty-minute gate does not stop the review queue
    gate: std::sync::Mutex<Option<InFlight>>,
    /// Whether the last heartbeat found the lease still this daemon's - what a running gate's
    /// `alive` reads, since the gate's thread has no store of its own. The pass beats; a lost
    /// lease kills the gate within its next poll, as when the gate beat for itself.
    lease_ok: Arc<AtomicBool>,
    /// When each target last fetched `origin`, so a waiting row does not fetch on every pass.
    fetched: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
    /// The process table the in-use rules read: `/proc` in a daemon, a fixture in a test. Whether
    /// a tree is in use must not depend on what another program on the box is doing.
    proc_root: PathBuf,
    /// Each kept row's proof, with the branch head, target head and tree `HEAD` it was made
    /// against: a pass proves a kept tree again only when one of the three moved. Keyed by row,
    /// since a merge row and its review row share one tree and each owes its own note.
    proven: std::sync::Mutex<std::collections::HashMap<String, Heads>>,
    /// How many trees the sweep has proved, for the test that a quiet pass proves none.
    proofs: std::sync::atomic::AtomicUsize,
}

/// What a kept tree's proof was made against: its branch's head, its target's, and its own `HEAD`.
type Heads = (Option<String>, Option<String>, Option<String>);

/// How often a target with rows in line asks `origin` where it is. A refused push fetches at once.
const FETCH_EVERY: Duration = Duration::from_secs(30);

/// One terminal row's hold on a worktree and a branch - the facts the resource sweep needs. A
/// review row and the merge row it was approved into name the same tree, which is examined once.
struct Held<'a> {
    branch: &'a str,
    /// `''` when the row names no tree - then the sweep costs no `git` call at all.
    worktree: &'a str,
    target: &'a str,
    /// The session that works in the tree; `''` when the row names none, which a merge row whose
    /// review row is gone does.
    session: &'a str,
    /// What the record keeps, for the note: `` `landed_sha` 0f2d9550 ``.
    keeps: String,
}

/// What the sweep found at one tree - what went, and what stayed with the reason it did. Two
/// rows that name one tree share one of these, so the `git` work is done once.
#[derive(Clone, Default)]
struct Settled {
    /// What went, as the note says it: `the worktree /x`, `` the branch `agent/y` ``.
    released: Vec<String>,
    /// What stayed: the kind of reason (which is the note's key) and the sentence for a person.
    kept: Option<(&'static str, String)>,
}

/// What a release says on the row's record - what went, when, and what the record still keeps.
/// `None` when nothing went and nothing is kept: the tree and the branch were both already gone.
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

/// A gate in flight - what it is for, so its verdict can be applied when it ends, and the
/// thread running it.
struct InFlight {
    what: GateFor,
    /// The commit under the gate: the target's tip, or the rebased branch.
    sha: String,
    /// This run's name in the store's `gate_live` row, so a stop names the run it was asked about.
    run_id: String,
    started: Instant,
    /// Set to kill the gate - the daemon is stopping, or somebody asked this run to stop.
    cancel: Arc<AtomicBool>,
    /// The words a stop asked for this run carries - `None` for the daemon's own stop, whose
    /// sentence the runner wrote itself.
    stop: Option<String>,
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

/// What a running gate judges - and so how its verdict is applied.
enum GateFor {
    /// The target's tip: its health.
    Target {
        health: TargetHealth,
        wt: PathBuf,
        carried: Vec<String>,
    },
    /// A branch rebased onto the target, before the compare-and-swap.
    Land(Box<Landing>),
    /// Several branches rebased one onto the last, gated once (`[queue] gate_train`).
    Train(Box<Train>),
    /// The install steps, after the landing was pushed.
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

/// One landing between its rebase and its end - what `land` knew when it started the gate.
struct Landing {
    row: MergeRow,
    tip: String,
    head: String,
    rebased: String,
    wt: PathBuf,
    carried: Vec<String>,
    install: Vec<String>,
    /// The bisection half this row was gated as ([`half_of`]); its green is recorded.
    half: Option<String>,
}

/// How a failed push opens in the row; [`Self::landed`] reads it to pick `landed_local`.
const PUSH_FAILED: &str = "the push FAILED";

/// A train - several approved branches, rebased one onto the last, judged by ONE gate run
/// on the final tree (`[queue] gate_train`).
struct Train {
    /// The target tip the train was built on - the first compare-and-swap expects exactly this.
    tip: String,
    /// The cars, oldest first.
    cars: Vec<TrainCar>,
    wt: PathBuf,
    carried: Vec<String>,
    install: Vec<String>,
    /// The bisection half every car is in, `None` for a train of plain rows.
    half: Option<String>,
}

/// One branch in a train - the row, the head it was approved at, and the commit it became.
struct TrainCar {
    row: MergeRow,
    head: String,
    /// The commit this car's branch became on top of the car before it. `''` until it is rebased.
    rebased: String,
}

impl Train {
    /// This car's landing, as `landed` wants it - the tip it was put onto, and the commit it
    /// became. Every fact `landed` writes is true of a car in a train, one car at a time.
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
            half: self.half.clone(),
        }
    }

    /// The branches in the order they were rebased - `agent/a`, `agent/b` - for a sentence.
    fn branches(&self) -> Vec<String> {
        self.cars.iter().map(|c| c.row.branch.clone()).collect()
    }
}

/// The `told` key that says half `tag` of a bisection in `repo` landed green.
fn half_green_key(repo: &str, tag: &str) -> String {
    format!("half-green:{repo}:{tag}")
}

/// What a row that landed in a train says about the train - `, in a train of 3 (car 2)`. `''`
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
            fetched: Default::default(),
            proc_root: PathBuf::from("/proc"),
            proven: Default::default(),
            proofs: Default::default(),
        }
    }

    /// This queue against a process table a caller names, so a test's answer about a tree does not
    /// depend on what is running on the box.
    pub fn with_proc_root(mut self, root: PathBuf) -> Self {
        self.proc_root = root;
        self
    }

    /// How many trees the resource sweep has proved since this worker started.
    #[cfg(test)]
    pub(crate) fn proofs_run(&self) -> usize {
        self.proofs.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// What the in-use rules read about this box: its name, this pid, and the process table.
    pub(crate) fn here(&self) -> letibot_tools::lifecycle::autoarchive::Here {
        letibot_tools::lifecycle::autoarchive::Here {
            host: letibot_tools::lifecycle::box_id(),
            pid: std::process::id(),
            proc_root: self.proc_root.clone(),
        }
    }

    /// Is a fetch of `target` due? One per target per [`FETCH_EVERY`], shared by the two steps that
    /// need origin's tip, so a pass that both follows and settles fetches once.
    fn fetch_due(&self, target: &str) -> bool {
        let mut at = self.fetched.lock().unwrap_or_else(|e| e.into_inner());
        if at.get(target).is_some_and(|t| t.elapsed() < FETCH_EVERY) {
            return false;
        }
        at.insert(target.to_string(), std::time::Instant::now());
        true
    }

    /// Follow `origin` when it moved ahead of the local target - another machine landed there - so
    /// the next landing rebases onto it and its push is a fast-forward. Only with rows in line.
    fn follow_origin(&self, target: &str) -> Result<bool, StoreError> {
        let waiting = self
            .store
            .merge_rows(Some(&self.key))?
            .iter()
            .any(|m| m.state == MergeState::Queued && m.target == target);
        if !waiting || !self.fetch_due(target) {
            return Ok(false);
        }
        match git::follow_origin(&self.repo, target) {
            Ok(moved) => Ok(moved),
            Err(e) => {
                eprintln!("  merge queue: `{target}` could not follow origin: {e}");
                Ok(false)
            }
        }
    }

    /// A row that landed locally has a dead end no verb reached: its push failed, and nothing moved
    /// it again. Each fetch settles it `landed` once its own commit is found on `origin`'s target -
    /// the push went through later, or a person pushed it by hand.
    fn settle_landed_local(&self, target: &str) -> Result<bool, StoreError> {
        let rows: Vec<MergeRow> = self
            .store
            .merge_rows(Some(&self.key))?
            .into_iter()
            .filter(|m| m.state == MergeState::LandedLocal && m.target == target)
            .collect();
        if rows.is_empty() || !self.fetch_due(target) {
            return Ok(false);
        }
        let theirs = match git::fetch_target(&self.repo, target) {
            Ok(Some(theirs)) => theirs,
            Ok(None) => return Ok(false),
            Err(e) => {
                eprintln!("  merge queue: `origin/{target}` could not be read: {e}");
                return Ok(false);
            }
        };
        let me = self.me().to_string();
        let mut moved = false;
        for m in rows {
            let Some(landed) = m.landed_sha.as_deref().filter(|s| !s.is_empty()) else {
                continue;
            };
            if !git::is_ancestor(&self.repo, landed, &theirs).unwrap_or(false) {
                continue;
            }
            moved |= self.merge_to(
                &m,
                MergeState::LandedLocal,
                MergeState::Landed,
                Move::new(
                    &me,
                    format!(
                        "the push was found on origin: `{landed}` is on `origin/{target}` (at \
                         {theirs}), so this landing is no longer local only"
                    ),
                )
                .set(
                    "pushed",
                    format!("pushed to `origin/{target}` (found at {theirs})"),
                ),
            )?;
        }
        Ok(moved)
    }

    /// A push refused because origin moved: the local target goes back to origin's tip and the
    /// rows go back in line, to be rebased onto it. `None` when origin did not move (or cannot be
    /// read), and the landing stays local as before. The rows' commits live on their branches.
    fn refused_push(
        &self,
        rows: &[&MergeRow],
        target: &str,
        landed: &str,
        said: &str,
        steps: &[letibot_tokencore::store::MergeGateStep],
    ) -> Result<Option<()>, StoreError> {
        let Ok(Some(theirs)) = git::fetch_target(&self.repo, target) else {
            return Ok(None);
        };
        if git::is_ancestor(&self.repo, &theirs, landed).unwrap_or(true) {
            return Ok(None);
        }
        if git::undo_landing(&self.repo, target, landed, &theirs).is_err() {
            return Ok(None);
        }
        if let Err(e) = git::follow_checkouts(&self.repo, target, landed, &theirs) {
            let _ = git::update_ref_cas(&self.repo, target, landed, &theirs);
            eprintln!("  merge queue: `{target}` could not go back to origin's {theirs}: {e}");
            return Ok(None);
        }
        let me = self.me().to_string();
        let steps = serde_json::to_string(steps).unwrap_or_else(|_| "[]".into());
        for row in rows {
            let why = format!(
                "{said}. `origin/{target}` moved to {theirs}, so `{target}` was put back there \
                 and this branch goes back in line, to be rebased onto it and gated again"
            );
            self.gating_to(
                row,
                MergeState::Queued,
                Move::new(&me, why).set("gate_steps_json", steps.clone()),
            )?;
        }
        Ok(Some(()))
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

    /// A pass, and the passes that apply every gate it started - for the tests that are about
    /// what a verdict does, not about the pass going on meanwhile.
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

    /// Why the landings wait - `gate running on <sha> for Nm`, while a gate runs.
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

    /// A raise - said on the hubs of THIS repository's sessions (and the queue's board), under
    /// a code a head can style: `queue_blocked` (high) and `target_red` (emergency).
    fn raise(&self, code: &str, detail: String) {
        eprintln!("  merge queue: {code}: {detail}");
        self.env
            .door
            .announce_to(&self.key, SessionEvent::warning(code, detail));
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

    /// Am I still the owner? `false` ends the worker. A store error is not an answer either
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

    /// Why this daemon may not land now, or `None`. A store error is a refusal: a lease that
    /// cannot be confirmed is not held.
    fn lease_refusal(&self) -> Option<String> {
        match self.beat() {
            Ok(true) => None,
            Ok(false) => {
                Some("this daemon lost the repository's lease, so it may not land".to_string())
            }
            Err(e) => Some(format!(
                "the repository's lease could not be confirmed ({e}), so this daemon may not land"
            )),
        }
    }

    /// One pass. It never waits for a gate: it starts one, or applies the verdict of one that
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
                // A `[queue]` that does not parse is said once per error, not once per pass - and
                // the queue runs on the defaults rather than on a guess at what the file meant.
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
        // The verdict of a gate that ended, applied once - before the upkeep, so the review of a
        // tip that just went red is claimed in this pass, first, as soon as a reviewer is free.
        let mut worked = self.reap_gate()?;
        worked |= self.mind_gate()?;
        worked |= self.adopt_orphans()?;
        worked |= self.review_upkeep(&settings)?;
        worked |= self.settle_landed_by_hand()?;
        // Beside the other sweeps, and after them: a row that went terminal in this pass has its
        // resources released in the same one.
        self.release_terminal_resources()?;
        // And the debris no row names: a registration for a directory that is gone, a tree of a
        // checkout that is not there any more. Nothing reaped these, so they accumulated per
        // killed or refused child, for ever.
        self.reap_worktree_debris();
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
            // A landed-local row is terminal, and its target is still the queue's business: the
            // settle below reads origin for it.
            .filter(|m| m.state.is_open() || m.state == MergeState::LandedLocal)
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
            worked |= self.follow_origin(&target)?;
            worked |= self.settle_landed_local(&target)?;
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

    /// A target emergency reaches a MODEL, not only the heads: `raise` is a warning on every
    /// hub, which heads draw and no model reads (pg-noop's agent never heard its `main` was
    /// red).
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
        // A landing that stopped wakes the tree that enqueued it, as a job's end wakes its session.
        for m in self.store.merge_rows(Some(&self.key))? {
            if !super::notices::wakes_owner(m.state) {
                continue;
            }
            let who = self
                .store
                .review_row(&m.review_id)?
                .map(|r| r.session_id)
                .filter(|s| !s.is_empty());
            due.push((
                format!("merge:{}:{}:{}", m.id, m.state.as_str(), m.state_ms),
                who,
            ));
        }
        // A target that cannot land wakes every tree with a row behind it, once per tip and per
        // stop of its fix: the rows themselves never move, so nothing else would.
        for b in super::behind::behind(&self.store, Some(&self.key), gate::now_ms())? {
            let mut roots: Vec<String> = b
                .waiting
                .iter()
                .map(|w| super::root_session(&self.store, &w.session))
                .collect();
            roots.sort();
            roots.dedup();
            for root in roots {
                due.push((format!("{}:{root}", b.key()), Some(root)));
            }
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

    /// Said before? `true` the first time `key` is asked about - by this worker or any
    /// daemon's before it - and records it.
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

    /// Said before, in this worker? - the in-memory half of [`Self::first_time`], for a question
    /// asked once per row per pass: a settled row must cost a hash lookup, not a store query.
    fn said(&self, key: &str) -> bool {
        self.rung
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(key)
    }

    /// Forget what was said under `prefix` - the state it was about is over. Answers how many
    /// keys the store held: more than none means it had been said.
    fn forget(&self, prefix: &str) -> Result<usize, StoreError> {
        self.rung
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|k| !k.starts_with(prefix));
        self.store.untell_prefix(prefix)
    }

    /// A target alarm, raised once per tip and cause - not again when a restarted daemon re-gates
    /// the same tip to the same verdict, and again once the target has been green in between.
    fn alarm(&self, h: &TargetHealth, code: &str, detail: String) -> Result<bool, StoreError> {
        let key = self.alarm_key(&h.target, &h.sha, code);
        if self.first_time(&key)? {
            self.raise(code, detail);
            return Ok(true);
        }
        Ok(false)
    }

    /// The key a target alarm is said under: the tip it is about, and the cause.
    fn alarm_key(&self, target: &str, sha: &str, code: &str) -> String {
        format!("alarm:{}:{target}:{sha}:{code}", self.key)
    }

    /// The targets of this repository that are RED now - the ones whose queues are halted.
    fn red_targets(&self) -> Result<std::collections::HashSet<String>, StoreError> {
        Ok(self
            .store
            .target_healths()?
            .into_iter()
            .filter(|h| h.repository == self.key && halts_reviews(h))
            .map(|h| h.target)
            .collect())
    }

    // ===== The gate in flight =====

    /// Start a gate on its own thread - under the same controls as ever: the deadline from
    /// now, the daemon's stop (through `cancel`), and the lease (through [`Self::lease_ok`]).
    fn start_gate(&self, what: GateFor, wt: PathBuf, steps: Vec<String>, sha: &str) {
        gate::watch(&wt, &self.key);
        let cancel = Arc::new(AtomicBool::new(false));
        let lease = self.lease_ok.clone();
        let settings = (self.env.settings)(&self.repo).ok();
        let mut gate_in = settings.as_ref().map(|s| s.gate_in).unwrap_or_default();
        let budget = settings
            .as_ref()
            .and_then(|s| s.gate_deadline)
            .unwrap_or(self.env.gate_deadline);
        if let Some(why) = firecode_only_step(&self.repo, &steps) {
            eprintln!(
                "  merge queue: {}: {why}; the gate runs in firecode",
                self.key
            );
            gate_in = super::settings::GateIn::Firecode;
        }
        let runner = super::gate_firecode::runner_for(gate_in, &self.env.gate);
        // Where it runs and where its build goes, read before the runner is moved into its thread.
        let host = runner.name();
        let dir = build_dir(&host, &wt);
        let lock = self
            .env
            .gate_lock
            .clone()
            .filter(|_| host.starts_with("local"));
        let stop = cancel.clone();
        let short: String = sha.chars().take(8).collect();
        let run = std::thread::Builder::new()
            .name(format!("gate-{short}"))
            .spawn(move || {
                let alive = move || lease.load(Ordering::Relaxed);
                // The deadline starts once the lock is held: the wait is the box's, not the gate's.
                let _held = match lock {
                    Some(path) => match gate::wait_for_box_lock(&path, &stop, &alive) {
                        Ok(file) => Some(file),
                        Err(run) => return run,
                    },
                    None => None,
                };
                let deadline = Instant::now() + budget;
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
        let started_ms = gate::now_ms();
        let (kind, target, entry) = gate_subject(&what);
        let run_id = format!("{sha}-{started_ms}");
        // Published before the thread can finish: no reader sees a gate running unpublished, and no
        // stop can name a run that is not there yet.
        let published = self.store.gate_began(&letibot_tokencore::store::LiveGate {
            repository: self.key.clone(),
            run_id: run_id.clone(),
            kind: kind.to_string(),
            target,
            entry,
            sha: sha.to_string(),
            host,
            dir,
            step: 0,
            of_step: 0,
            command: String::new(),
            step_ms: 0,
            started_ms,
            stop_by: String::new(),
            stop_why: String::new(),
        });
        if let Err(e) = published {
            eprintln!(
                "  merge queue: {}: the running gate was not published: {e}",
                self.key
            );
        }
        *slot = Some(InFlight {
            what,
            sha: sha.to_string(),
            run_id,
            started: Instant::now(),
            cancel,
            stop: None,
            run,
        });
    }

    /// Kill the running gate (the daemon is stopping); its verdict, `stopped`, is still applied.
    fn cancel_gate(&self) {
        if let Some(g) = self.slot().as_ref() {
            g.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// What a gate in flight has to be told on every pass: the step it is on, so a reader in another
    /// process can say what it is doing, and a stop somebody asked for, which kills it here and now.
    fn mind_gate(&self) -> Result<bool, StoreError> {
        if !self.gate_in_flight() {
            return Ok(false);
        }
        let (run_id, asked) = {
            let slot = self.slot();
            let Some(g) = slot.as_ref() else {
                return Ok(false);
            };
            (g.run_id.clone(), g.stop.is_some())
        };
        let mut worked = false;
        // The step, from the runner's own record - both machines write it as they start a step, so a
        // local gate and a host gate publish the same fact.
        if let Some(step) = gate::running(&self.key) {
            self.store.gate_step_at(
                &self.key,
                &run_id,
                step.step,
                step.of,
                &step.command,
                step.started_ms,
            )?;
            worked = true;
        }
        if asked {
            return Ok(worked);
        }
        // A stop asked for THIS run. A request that names a run which has ended is left where it is:
        // the row it is on is gone with the run, and the next gate starts with no stop at all.
        let Some(live) = self.store.live_gate(&self.key)? else {
            return Ok(worked);
        };
        if !live.stopping() || live.run_id != run_id {
            return Ok(worked);
        }
        let words = format!("{STOPPED} by {}: {}", live.stop_by, live.stop_why);
        let mut slot = self.slot();
        if let Some(g) = slot.as_mut() {
            g.stop = Some(words.clone());
            g.cancel.store(true, Ordering::Relaxed);
        }
        eprintln!("  merge queue: {}: {words}; the gate is killed", self.key);
        Ok(true)
    }

    /// Apply the verdict of a gate that ended - taken out of the slot first, so it is applied
    /// once whatever the application does (including start the install's gate).
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
        self.gate_over(&g.run_id);
        self.apply_gate(g.what, with_stop(run, g.stop))?;
        Ok(true)
    }

    /// Nothing is running here any more, so the published row goes - before the verdict is applied,
    /// which may start the install's gate and publish its own.
    fn gate_over(&self, run_id: &str) {
        if let Err(e) = self.store.gate_ended(&self.key, run_id) {
            eprintln!(
                "  merge queue: {}: the finished gate's row was not taken down: {e}",
                self.key
            );
        }
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

    /// A `gating` landing this worker runs no gate for - its worker ended mid-gate, so it goes
    /// back in line and is gated again. Never while a gate is in flight: that row is the gate's.
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

    /// The worker is ending - the running gate is killed, waited for, and its verdict
    /// applied (a stopped gate puts its row back in line), so nothing is left `gating` behind.
    pub fn wind_down(&self) {
        self.cancel_gate();
        let g = self.slot().take();
        if let Some(g) = g {
            let run = g.run.wait();
            self.gate_over(&g.run_id);
            if let Err(e) = self.apply_gate(g.what, with_stop(run, g.stop)) {
                eprintln!(
                    "  merge queue: {}: the stopped gate's verdict was not written: {e}",
                    self.key
                );
            }
            // An install started by that verdict is stopped too.
            if let Some(next) = self.slot().take() {
                next.cancel.store(true, Ordering::Relaxed);
                let run = next.run.wait();
                self.gate_over(&next.run_id);
                if let Err(e) = self.apply_gate(next.what, with_stop(run, next.stop)) {
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
        // A target review is about ONE tip - before anything else reads them. Every move of the
        // tip supersedes, not only a rewrite
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
                // A reviewer past its deadline, or one that never started - the attempt is
                // counted, and the third is a raise rather than a fourth.
                ReviewState::Reviewing if r.owner == me => {
                    let never_started = r.reviewer.is_none()
                        && now.saturating_sub(r.state_ms) > self.env.reviewer_start_grace_ms;
                    // A reviewer child that is not alive in this daemon, as after a restart. A Claude
                    // reviewer is a process with no session: its run in the table is its liveness.
                    let is_process = r
                        .reviewer
                        .as_deref()
                        .is_some_and(|c| self.env.claude.as_ref().is_some_and(|runs| runs.owns(c)));
                    let gone = self.env.door.sees_sessions()
                        && !is_process
                        && r.reviewer
                            .as_deref()
                            .is_some_and(|c| !self.env.door.session_live(c));
                    // A reviewer on no desk was lost with a daemon or a gatekeeper: the review pass
                    // reopens its stored session, so it is given the start grace to do so first.
                    let gone = gone
                        && (self.env.desk.as_ref().is_none_or(|d| d.is_open_for(&r.id))
                            || now.saturating_sub(r.state_ms) > self.env.reviewer_start_grace_ms);
                    let late = r.deadline_ms.is_some_and(|d| now > d);
                    // A reviewer that has handed its verdict in is not gone, whatever its session says: its
                    // last act is the submission and its turn ending, so `gone` alone would re-seat a row that
                    // has just judged and throw the verdict away. The desk is PEEKED, never taken.
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
                // A verdict that answered a head the branch has since moved past is no verdict on
                // the branch.
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

        // Decision 4's option: a blocked review of a project that opted in is handed to the
        // project's main session to gate and merge itself - once, read off the row.
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

        // The claims - the target's red tip first, then priority, then age. A red target halts its
        // review queue.
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
        // Reviews this queue runs at once for its placement ([`super::capacity::review_limit`]): `vm`
        // keeps the VM-slot count, `host` takes the project's number, else the pool's.
        let limit = super::capacity::review_limit(
            settings.reviewers,
            self.env.reviewers_at_once.or(settings.reviewers_at_once),
            self.env.review_capacity(),
        );
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
        // The row's deadline IS the reviewer's time limit. A project reviewing with headless
        // `claude -p` runs sets it with `[queue.claude] timeout_seconds`; everyone else gets the
        // queue's own hour, which is what a letibot reviewer has always had.
        let review_limit_ms = settings
            .claude
            .as_ref()
            .map(|c| (c.timeout.as_millis() as u64).max(1))
            .unwrap_or(self.env.review_deadline_ms);
        let mut claimed = 0;
        for r in waiting.into_iter().take(limit.saturating_sub(busy)) {
            let head = match r.kind {
                ReviewKind::Target => Some(r.head_sha.clone()),
                ReviewKind::Branch => git::branch_head(&self.repo, &r.branch),
            };
            let Some(head) = head else {
                continue;
            };
            let target = target_of(r);
            // The reason it waited outlives the claim. A capacity wait is a row's whole story
            // for an hour, and writing the claim over it left a pane with no reason on it at all.
            let waited = r.failure.trim();
            let why = match waited.is_empty() {
                true => format!("claimed for review at {head} by {me}"),
                false => format!(
                    "claimed for review at {head} by {me}, after waiting: {}",
                    super::capacity::gist(waited)
                ),
            };
            if self.review_to(
                r,
                ReviewState::Reviewing,
                Move::new(&me, why)
                    .set("owner", &me)
                    .set("head_sha", head)
                    .set("target", target)
                    .clear("reviewer")
                    .set("deadline_ms", now + review_limit_ms)
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

    /// A review nobody needs to run - the branch is gone, carries nothing, or is already on
    /// the target (a project that landed a blocked branch by hand).
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

    /// A merge row whose branch is already on its target settles - landed by hand, or by the
    /// project's main session after a handover. The commit on the target answers the common case;
    /// a merge that rewrote every sha leaves the branch off it with all its patches there.
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
            // The cheap rung first - an ancestor check is 12 ms where `git cherry` over a target
            // thousands of commits ahead is 0.3 s, measured - and the two agree wherever the
            // commit is on the target (`mergequeue/git/tests.rs` holds them together).
            let patches_only = !git::is_ancestor(&self.repo, &head, &tip).unwrap_or(false);
            let why = if patches_only {
                match git::unlanded_patches(&self.repo, &tip, &head) {
                    Ok(0) => format!(
                        "`{}` is already on `{}` (at {tip}): every patch it approved is on the \
                         target under another commit, so its work landed outside the queue and \
                         there is nothing to merge",
                        m.branch, m.target
                    ),
                    // A branch that still carries work waits its turn, whatever else is owed.
                    _ => continue,
                }
            } else {
                format!(
                    "`{}` is already on `{}` (at {tip}): it landed some other way, so \
                     there is nothing to merge",
                    m.branch, m.target
                )
            };
            if !self.merge_to(&m, m.state, MergeState::Withdrawn, Move::new(&me, why))? {
                continue;
            }
            moved = true;
            if patches_only {
                // Said rather than swallowed: the row goes without the queue gating it, and the
                // author is the one who landed it - they should see that it was noticed.
                self.raise(
                    "queue_already_on_target",
                    format!(
                        "the queue settled `{}`'s row for `{}` at {tip}: every patch it approved \
                         is already on the target under another commit, so the branch was merged \
                         outside the queue and nothing is left to land",
                        m.branch, m.target
                    ),
                );
            }
        }
        Ok(moved)
    }

    /// The debris no row names: a registration whose directory is gone and a tree whose `.git` names a missing gitdir, left by a killed, refused or elsewhere-landed child. Neither can hold a commit.
    /// The tree says so in the daemon's log; a sweep that took nothing says nothing.
    fn reap_worktree_debris(&self) {
        let here = self.here();
        let in_use =
            |dir: &Path| letibot_tools::lifecycle::in_use::why_kept(dir, Some(&self.store), &here);
        match git::reap_worktrees(&self.repo, std::time::SystemTime::now(), &in_use) {
            Ok(reaped) if reaped.took_anything() || !reaped.in_use.is_empty() => {
                eprintln!("  worktree sweep: {}: {}", self.key, reaped.words());
            }
            Ok(_) => {}
            // Said rather than swallowed: a repository this build cannot read is the one place the
            // sweep is blind, and silence there looks exactly like nothing to do.
            Err(why) => eprintln!("  worktree sweep: {}: {why}", self.key),
        }
    }

    /// A terminal row keeps its record and gives back its resources - the sweep. Terminal is
    /// the store's own answer ([`ReviewState::is_open`]), and an `approved` review whose
    /// landing is still open is NOT terminal for the sweep
    fn release_terminal_resources(&self) -> Result<(), StoreError> {
        let reviews = self.store.review_rows(Some(&self.key))?;
        let merges = self.store.merge_rows(Some(&self.key))?;
        // A merge row names no session of its own; the review row that opened it does, and a row
        // whose review row is gone names none - nothing can be asked about it, and nothing is.
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
        // One examination per (tree, target) per pass: a review row and the merge row it became name
        // the same worktree and branch, but the decision is about commits *on a target*, so two rows
        // naming one tree and different targets are two questions.
        let mut decided: HashMap<(String, String, String), Settled> = HashMap::new();
        // **The branch heads are read only when a row will be asked about them.** The sweep used to
        // spawn `git for-each-ref` over every `refs/heads` on every pass — the idle daemon's
        // remaining spin, measured at ~13 spawns/s before `default_target` was cached and this
        // was the next in line. A pass with no terminal row holding a worktree has nobody to ask,
        // and the map is `None` until somebody is.
        let asks_about_a_tree = merges
            .iter()
            .any(|m| !m.state.is_open() && m.worktree.is_some());
        let heads = if asks_about_a_tree {
            letibot_tools::git::ref_heads(&self.repo)
        } else {
            Default::default()
        };
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
            self.release_one(
                "merge",
                &m.id,
                m.state.as_str(),
                &held,
                &heads,
                &mut decided,
            )?;
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
            self.release_one(
                "review",
                &r.id,
                r.state.as_str(),
                &held,
                &heads,
                &mut decided,
            )?;
        }
        Ok(())
    }

    /// What the sweep did with one row's tree, and where it says so. Three keys.
    /// `released:<row>` is *there is nothing more to do here*, so a settled row costs a hash
    /// lookup for the rest of the daemon's life
    fn release_one(
        &self,
        queue: &str,
        id: &str,
        state: &str,
        held: &Held<'_>,
        heads: &HashMap<String, String>,
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
        // Kept on a proof, and nothing the proof read has moved: the same answer, already said.
        let now_heads: Heads = (
            heads.get(held.branch).cloned(),
            heads.get(held.target).cloned(),
            letibot_tools::git::tree_head_quick(Path::new(held.worktree), heads),
        );
        let unmoved = !heads.is_empty()
            && self
                .proven
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&settled)
                .is_some_and(|was| *was == now_heads);
        if unmoved {
            return Ok(());
        }
        let found = match decided.get(&tree) {
            Some(found) => found.clone(),
            None => {
                let found = self.settle_tree(held);
                decided.insert(tree.clone(), found.clone());
                found
            }
        };
        {
            let mut proven = self.proven.lock().unwrap_or_else(|e| e.into_inner());
            match &found.kept {
                // A live session or process is a fact about now, not about commits: asked again.
                Some((kind, _)) if *kind != "live" => {
                    proven.insert(settled.clone(), now_heads);
                }
                _ => {
                    proven.remove(&settled);
                }
            }
        }
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
            // Said where a person reads it, not only in the daemon's log - once per tree.
            self.raise(
                "queue_branch_kept",
                format!("the queue's sweep of `{}`'s resources: {why}", held.branch),
            );
        }
        Ok(())
    }

    /// The decision about one tree - what may go, and what stays with the reason it does. The
    /// commits decide, and the tree follows them - the rule `git::retire_if_empty` already
    /// applies to a superseded fix, because one queue may not hold two rules for one tree
    fn settle_tree(&self, held: &Held<'_>) -> Settled {
        self.proofs
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        // Another daemon's session, or any process with its cwd in the tree: the queue's own
        // registry above sees only this daemon's sessions.
        if there
            && let Some(why) =
                letibot_tools::lifecycle::in_use::why_kept(wt, Some(&self.store), &self.here())
        {
            found.kept = Some((
                "live",
                format!(
                    "the worktree {} and `{}` are kept: {why}",
                    wt.display(),
                    held.branch
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
        // The tree's own commits, under the same proof. A tree is removed whole, so a commit it
        // holds and no ref does is gone with it - kept here unless it is provably on the target.
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

    /// Whether a terminal row's branch may go - `None` when the proof says its commits are on
    /// the target, else the kind of reason it stays and the sentence for a person. `None` means
    /// one of two things and never a third
    fn branch_may_go(&self, held: &Held<'_>) -> Option<(&'static str, String)> {
        let what = format!("the branch `{}`", held.branch);
        git::ref_proof(&self.repo, held.branch, held.target, &what).unproven()
    }

    /// The review of a tip the target no longer has. A newer tip never closed the older rows
    /// so each got its own reviewer, each was rejected, and each opened a fix branch for a red
    /// `main` had already left.
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

    /// A branch opened to fix a red tip the target has moved past. Decision 11 has the red
    /// tip's reviewer open one, and it is the only branch a red target lands - so a fix cut
    /// from a tip that is no longer the target's can never land.
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
            // A fix is a row in either queue - the review of the fixer's branch, or the landing an
            // approval put behind it. A row being GATED is left to its gate: `land` rebases onto the
            // tip as it is then, so it judges a tree the target really has.
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
                git::retire_if_empty(
                    &self.repo,
                    &tip,
                    &r.branch,
                    Some(wt),
                    Some(&self.store),
                    &self.here(),
                )
            };
            if let Err(kept) = kept {
                // Said where a person reads it, not only in the daemon's log - and once
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

    /// Raise a review that cannot run - `blocked`, at high severity.
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

    /// Gate the target's tip if it changed - coalesced: whatever the tip is NOW is
    /// what is gated, so a burst of commits is one run. Returns the health, or that a gate started.
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
            // Nothing lands on a branch that is not there, and the row is the only place this fact
            // was written: raised once, so a person is told rather than left to read the pane.
            self.alarm(
                &h,
                "target_unjudged",
                super::notices::target_missing(&h, &self.key),
            )?;
            return Ok(Health::Known(h, false));
        };
        // The target is back: what was said about it being missing is not true any more, so a
        // second disappearance is news again - and it need not have been green in between.
        self.forget(&self.alarm_key(target, "", "target_unjudged"))?;
        if let Some(h) = self.store.target_health(&self.key, target)?
            && h.sha == tip
            && h.state != HealthState::Gating
        {
            return Ok(Health::Known(h, false));
        }
        // A gate that could not run waits out its retry window: the row is `gating` and carries the
        // sentence that stopped it, so the tip is neither judged nor blocked and a host that is down is
        // not dialled once a second. `Gating` with plain *gating* evidence is a gate on its thread now.
        for (clause, window) in [
            (UNJUDGED_RETRY, UNJUDGED_RETRY_MS),
            (STOPPED, STOPPED_RETRY_MS),
        ] {
            if let Some(h) = self.store.target_health(&self.key, target)?
                && h.sha == tip
                && h.state == HealthState::Gating
                && h.evidence.starts_with(clause)
                && now.saturating_sub(h.checked_ms) < window
            {
                return Ok(Health::Known(h, false));
            }
        }
        let was_red = self
            .store
            .target_health(&self.key, target)?
            .is_some_and(|old| old.sha == tip && halts_reviews(&old));
        let mut h = TargetHealth {
            repository: self.key.clone(),
            target: target.into(),
            sha: tip.clone(),
            state: HealthState::Gating,
            checked_ms: now,
            evidence: match was_red {
                true => format!("gating `{target}`'s tip; {STILL_HALTED}"),
                false => format!("gating `{target}`'s tip"),
            },
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

    /// The verdict of the target's gate.
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
            // A gate that failed AFTER its build and tests passed could not judge the
            // code: its own setup failed. The tip stays unjudged and nobody is sent to the code.
            GateVerdict::Failed(w) => {
                let w = self.with_carry_hint(w, wt, carried);
                match super::setup::setup_failure(&run.steps) {
                    Some(setup) => (HealthState::Blocked, format!("{setup}\n\n{w}"), run.steps),
                    None => (HealthState::Red, w, run.steps),
                }
            }
            GateVerdict::TimedOut(w) => (HealthState::TimedOut, w, run.steps),
            // A gate that could not run is not a verdict about the code, and it is not a halt: the tip
            // goes back to `gating` and is gated again after the window, because a machine that was
            // slow or briefly gone must not stop the queue landing anything.
            GateVerdict::Infra(w) if !w.starts_with(gate::GATE_KILLED) => {
                return self.unjudged(h, w, run.steps);
            }
            GateVerdict::Infra(w) => (HealthState::Blocked, w, run.steps),
            GateVerdict::Stopped(w) => {
                // Judged nothing: the tip stays `gating` and is gated again - after the window, when
                // somebody stopped it on purpose, so the stop is a fact the row carries and not a
                // restart dressed as one.
                return self.stopped_health(h, &target, w, run.steps);
            }
        };
        self.judged(&mut h, state, evidence, steps)
    }

    /// The health row of a tip whose gate was stopped: gating, un-judged, and saying who stopped it.
    fn stopped_health(
        &self,
        mut h: TargetHealth,
        target: &str,
        why: String,
        steps: Vec<letibot_tokencore::store::MergeGateStep>,
    ) -> Result<(), StoreError> {
        let me = self.me().to_string();
        // A tip that was red keeps its queue halted: the stop judged nothing about the code.
        let halted = halts_reviews(&h);
        h.state = HealthState::Gating;
        h.evidence = match why.starts_with(STOPPED) {
            true => format!(
                "{why}; nothing was judged, and the tip is gated again in {} minutes' time",
                STOPPED_RETRY_MS / 60_000
            ),
            false => format!("{why}; nothing was judged, and it is gated again"),
        };
        if halted {
            h.evidence = format!("{}; {STILL_HALTED}", h.evidence);
        }
        h.gate_steps = steps;
        h.checked_ms = gate::now_ms();
        self.store.set_target_health(&h, &me)?;
        self.announce_move("target", target, "gating", &h.evidence);
        Ok(())
    }

    /// A gate that could not run - the tip stays UNJUDGED, the queue keeps moving, and the row says
    /// both. `HealthState::Blocked` is for a tip nobody can judge at all; a host that did not answer in
    /// time is not that, and treating it as that would halt both queues.
    fn unjudged(
        &self,
        mut h: TargetHealth,
        why: String,
        steps: Vec<letibot_tokencore::store::MergeGateStep>,
    ) -> Result<(), StoreError> {
        let me = self.me().to_string();
        let was_red = halts_reviews(&h);
        h.state = HealthState::Gating;
        let halt = match was_red {
            true => format!("the tip was red before this gate, so {STILL_HALTED}"),
            false => "the queue is not halted by it".to_string(),
        };
        h.evidence = format!(
            "{UNJUDGED_RETRY}: {why}\n\nNothing was judged about the code — the tip is UNJUDGED, \
             not red, and {halt}. It is gated again on the next pass once {} s have passed.",
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

    /// Write a target's verdict and say what it means.
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
                // High, not emergency: the step was killed before it judged the code, and the words
                // say which and how to give it room. Counted, so each retry comes less often.
                self.store
                    .tell_again(&killed_key(&self.key, &h.target, &h.sha), gate::now_ms())?;
                self.alarm(h, "gate_killed", super::notices::gate_killed(h, &self.key))?;
            }
            HealthState::TimedOut => {
                // : nothing lands until it is gated again, so it is said.
                self.alarm(
                    h,
                    "target_timed_out",
                    super::notices::target_timed_out(h, &self.key),
                )?;
            }
            HealthState::Blocked if !h.evidence.starts_with(super::WAITING_FOR_GATE) => {
                // : the gate could not be read, the landing tree could not be made ready, a
                // step could not be spawned - the tip is unjudged and nothing lands.
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

    /// The landing worktree at `at`, with the project's `gate_carry` copied in - the tree and
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

    /// Red target: an emergency, the red tip first in review, and the operator told
    /// . Enqueue is never refused and other reviews go on; the
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
            // The same sha red a second time - it was green in between, its review row
            // was withdrawn then, and the `INSERT OR IGNORE` above left it so. It goes back to review.
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
            // And on a session, in the same words - a ring wakes the repository's roots and leaves
            // what happened to the store, and a session that is merely woken cannot act on what it was
            // not told. The card stays the record; this is the copy somebody can act on.
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
        // A red target halts BOTH queues : what was approved and waits to land goes
        // back to review now, not when it would have landed.
        self.halt_landings(&h.target)?;
        Ok(())
    }

    /// Every branch approved for `target` and waiting to land goes back to review - its approval
    /// was of a change onto a tip that is now red, so it is spent
    /// . A branch opened to fix the red tip keeps its place: it is the one thing that may move.
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

    /// Green again: the reviews of earlier red tips are withdrawn - open ones, and the ones
    /// parked as `cannot_decide` or ended `rejected`, which a later verdict could otherwise turn into
    /// an EMERGENCY on a green target - and a raised alarm is closed where it was raised.
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
        self.forget(&format!(
            "rung:{}:behind:{}:{}:",
            self.key, self.key, h.target
        ))?;
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

    /// Take the next approved branch for `target` and land it - only while the target is green
    /// at its current tip, or, while it is red, only a branch opened to fix it.
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
            // A row whose own gate could not run waits out its window - it went back in line with
            // the sentence that stopped it, and without this it would be taken on the next pass: one
            // ssh attempt per second at a host that is down, and a queue that never settles.
            .filter(|m| {
                let now = gate::now_ms();
                let since = now.saturating_sub(m.state_ms);
                if m.evidence.starts_with(UNJUDGED_RETRY) {
                    return since >= UNJUDGED_RETRY_MS;
                }
                // A stopped gate is not somebody's accident: the row waits, so stopping it was not
                // the same as restarting it.
                !(m.evidence.starts_with(STOPPED) && since < STOPPED_RETRY_MS)
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
            // While the target is red the queue lands nothing but the fix itself.
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
        if self.settle_halves(&eligible)? {
            return Ok(true);
        }
        // A fix lands alone. Then a first half of a red train's bisection, before any plain row:
        // its verdict is what the rest of that train waits for.
        let mut half: Option<String> = None;
        let mut group: Vec<MergeRow> = Vec::new();
        match eligible.first() {
            Some(m) if m.fixes.is_some() => group.push(m.clone()),
            _ => {
                let first_half = eligible
                    .iter()
                    .filter_map(|m| half_of(&m.evidence))
                    .find(|t| first_half_of(t).is_none());
                if let Some(t) = first_half {
                    group = eligible
                        .iter()
                        .filter(|m| half_of(&m.evidence).as_deref() == Some(t.as_str()))
                        .cloned()
                        .collect();
                    half = Some(t);
                } else {
                    group = eligible
                        .iter()
                        .filter(|m| m.fixes.is_none() && half_of(&m.evidence).is_none())
                        .cloned()
                        .collect();
                }
            }
        }
        let most = match &half {
            Some(_) => group.len(),
            None => settings.gate_train,
        };
        if most > 1
            && h.state == HealthState::Green
            && current
            && group.len() > 1
            && group.iter().all(|m| m.fixes.is_none())
            && self.land_train(target, most, &group, half.clone())?
        {
            return Ok(true);
        }
        let Some(row) = group.into_iter().next() else {
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
        self.land(&row, half)?;
        Ok(true)
    }

    /// The second halves of a bisection, once their first half has a verdict. First half green:
    /// the second holds the red, so it is split again without a gate, or parked when it is one
    /// row. First half red or gone: the second half is plain rows again.
    fn settle_halves(&self, eligible: &[MergeRow]) -> Result<bool, StoreError> {
        let me = self.me().to_string();
        let mut halves: std::collections::BTreeMap<String, Vec<MergeRow>> = Default::default();
        for m in eligible {
            if let Some(t) = half_of(&m.evidence) {
                halves.entry(t).or_default().push(m.clone());
            }
        }
        let mut moved = false;
        for (tag, rows) in &halves {
            let Some(first) = first_half_of(tag) else {
                continue;
            };
            if halves.contains_key(&first) {
                continue;
            }
            let green = self
                .store
                .told(&half_green_key(&self.key, &first))?
                .is_some();
            if !green {
                let why = format!(
                    "the half `{first}` gated before this one was red or left the line, so this \
                     branch is back in line as a plain one"
                );
                for m in rows {
                    moved |= self.merge_to(
                        m,
                        MergeState::Queued,
                        MergeState::Queued,
                        Move::new(&me, &why),
                    )?;
                }
            } else if let [m] = rows.as_slice() {
                let why = format!(
                    "the train it rode in was red and the half `{first}` before it landed green \
                     without it, so this branch is the one that failed the gate (half `{tag}`)"
                );
                self.raise("merge_failed", super::notices::merge_failed(m, &why));
                moved |= self.merge_to(
                    m,
                    MergeState::Queued,
                    MergeState::Failed,
                    Move::new(&me, why).set("owner", ""),
                )?;
            } else {
                let rows = self.train_order(rows);
                let k = rows.len().div_ceil(2);
                for (i, m) in rows.iter().enumerate() {
                    let sub = format!("{tag}{}", if i < k { 'a' } else { 'b' });
                    let why = half_sentence(
                        &sub,
                        &format!(
                            "half `{first}` landed green, so the red is in `{tag}` and it is split \
                             without a gate"
                        ),
                    );
                    moved |= self.merge_to(
                        m,
                        MergeState::Queued,
                        MergeState::Queued,
                        Move::new(&me, why),
                    )?;
                }
            }
        }
        Ok(moved)
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

    /// The landing, in the dedicated worktree, with a compare-and-swap on the target.
    fn land(&self, row: &MergeRow, half: Option<String>) -> Result<(), StoreError> {
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
        // Decision 6: the verdict is bound to the head it judged. A different head carrying
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
        // The base the branch was cut from, as the store remembers it - the tip this row was last
        // rebased onto, and the base its review recorded. Both survive the target being rewritten
        // which is what a plain `git rebase` gets wrong once the merge base is on the old target.
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
                // : said, where the repository's sessions hear it - a parked row nobody is
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
                half,
            })),
            wt,
            steps,
            &rebased,
        );
        Ok(())
    }

    // ===== The train =====

    /// One gate for several branches - `[queue] gate_train` (`N > 1`). The order is DERIVED
    /// not guessed: the queue's own (priority, then FIFO), lifted so a branch cut from another
    /// branch in the train comes after it ([`Self::train_order`]).
    fn land_train(
        &self,
        target: &str,
        most: usize,
        candidates: &[MergeRow],
        half: Option<String>,
    ) -> Result<bool, StoreError> {
        let me = self.me().to_string();
        let Some(tip) = git::branch_head(&self.repo, target) else {
            return Ok(false);
        };
        // What can be a car, asked before anything is written: the branch is still where its approval
        // left it, and it carries something the target has not got (a branch already there settles).
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
        // The rebases, in order - each car onto the last, in ONE worktree, so the final HEAD is the
        // tree the gate judges. A car that will not rebase is parked `conflict` and the train is the rest.
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
                half,
            })),
            wt,
            steps,
            &at,
        );
        Ok(true)
    }

    /// The order a train's cars go in - the queue's own order, and a branch cut from another
    /// branch in the train after it. The first half is [`Self::land_next`]'s comparator, so a
    /// train lands in the order its branches would have landed one by one
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
            // The first row whose parents are all placed. A cycle - two branches each cut from the
            // other's work - keeps the first of them: the queue's order beats no order at all.
            let pick = left
                .iter()
                .position(|&c| !left.iter().any(|&p| p != c && stacks[c][p]))
                .unwrap_or(0);
            order.push(left.remove(pick));
        }
        order.into_iter().map(|i| rows[i].clone()).collect()
    }

    /// Was `child` cut from `parent`'s own work? - the child's recorded base against the
    /// parent's head and the base the parent itself was cut from. The child's base is on the
    /// parent's head and NOT on the parent's base
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

    /// The base the row's branch was cut from, as the store remembers it - the base its review
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

    /// Put every car still this gate's back in `Queued`, with one sentence and the gate's steps.
    fn train_back(
        &self,
        cars: &[TrainCar],
        why: &str,
        steps: &[letibot_tokencore::store::MergeGateStep],
    ) -> Result<(), StoreError> {
        self.train_move(cars, MergeState::Queued, why, steps)
    }

    /// Park every car still this gate's in `to` - `blocked`, `timed_out`.
    fn train_park(
        &self,
        cars: &[TrainCar],
        to: MergeState,
        why: &str,
        steps: &[letibot_tokencore::store::MergeGateStep],
    ) -> Result<(), StoreError> {
        self.train_move(cars, to, why, steps)
    }

    /// The one move [`Self::train_back`] and [`Self::train_park`] are - a car a person has already
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

    /// The verdict of a landing's gate - and, when it passed, the compare-and-swap, the push
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
        // The person may have taken the row out while its gate ran (`/queue rm` on a `gating`
        // row). Such a row does not land, whatever its gate said, and its verdict is not news. The row
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
                // The branch was never judged: the gate's own setup failed after the build and tests
                // passed. `blocked`, never `failed` - which says the branch is bad.
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
                let wait = match w.starts_with(STOPPED) {
                    true => format!(
                        " and waits {} minutes before it is gated again",
                        STOPPED_RETRY_MS / 60_000
                    ),
                    false => String::new(),
                };
                self.gating_to(
                    row,
                    MergeState::Queued,
                    Move::new(&me, format!("{w}; it goes back in line{wait}"))
                        .set("gate_steps_json", rows_json(&run.steps)),
                )?;
                return Ok(());
            }
            GateVerdict::Infra(w) if !w.starts_with(gate::GATE_KILLED) => {
                // The branch was not judged, and nothing about it is in doubt - so it goes back in line
                // rather than being parked behind a host that was slow to answer, and `land_next` keeps it
                // out of the next passes until its window is out.
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
                // A step killed at the gate's memory cap RAN and was killed - "could not run" is the
                // wrong sentence, and the verb that gates a branch row again is `restart`.
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
        // before anything is written into the repository - a daemon that no longer holds it must
        // not land into it.
        if let Some(why) = self.lease_refusal() {
            self.gating_to(
                row,
                MergeState::Queued,
                Move::new(
                    &me,
                    format!("{why}; it goes back in line and is gated again"),
                )
                .set("gate_steps_json", rows_json(&run.steps)),
            )?;
            return Ok(());
        }
        // The compare-and-swap. The target moved during the gate → rebase and gate again
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
            if git::undo_landing(&repo, &target, &rebased, &tip).is_ok() {
                let why = format!("{e}. The landing was undone: `{target}` is at {tip} as before.");
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
            // The target holds the commit and cannot be put back: the row says landed, and the
            // checkout is the thing that needs a person.
            self.raise(
                "queue_blocked",
                format!(
                    "`{}` landed on `{target}` at {rebased} but a checkout did not follow and the \
                     landing could not be undone (the ref moved again): {e}",
                    row.branch
                ),
            );
        }
        // Push, then install. Nothing is installed that was not pushed: a repository with no
        // `origin` has no built tree to install from, and an install step that ran anyway would
        // put a binary in place on this box while saying the landing went out.
        let pushed = git::push(&repo, &target, &rebased);
        let (pushed_text, may_install) = match &pushed {
            git::Pushed::Yes(s) => (s.clone(), true),
            git::Pushed::NoRemote => ("no `origin` to push to".to_string(), false),
            git::Pushed::Failed(e) => (format!("{PUSH_FAILED}: {e}"), false),
        };
        if !may_install
            && self
                .refused_push(&[row], &target, &rebased, &pushed_text, &run.steps)?
                .is_some()
        {
            return Ok(());
        }
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
                let why = if pushed == git::Pushed::NoRemote {
                    "there is no `origin` to push to"
                } else {
                    "the push failed"
                };
                install_note = format!(
                    " Not installed: {why}, and nothing is installed that is not where it was \
                     pushed (decision 7)."
                );
            }
        }
        if matches!(pushed, git::Pushed::Failed(_)) {
            // : a failed push is said whether or not the project has an install step.
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

    /// The verdict of a train's gate - every car lands when it passed, and the whole train
    /// dissolves when it did not. A train is ONE tree, so it is judged as one: a car a person
    /// took out of line while the gate ran takes the whole train with it.
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
                // The branches were never judged: the gate's own setup failed after the build and the
                // tests passed. `blocked`, never `failed` - which says a branch is bad.
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
                let wait = match w.starts_with(STOPPED) {
                    true => format!(
                        " and waits {} minutes before it is gated again",
                        STOPPED_RETRY_MS / 60_000
                    ),
                    false => String::new(),
                };
                let why = format!("{w}; it goes back in line{wait}");
                self.train_back(&train.cars, &why, &run.steps)?;
                Ok(())
            }
            GateVerdict::Infra(w) if !w.starts_with(gate::GATE_KILLED) => {
                // Not judged, and nothing about the branches is in doubt - so they go back in line
                // rather than being parked behind a host that was slow to answer. Said once per train's
                // tip, not once per attempt: a host down for an hour must not be sixty warnings.
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

    /// Every car lands - one commit each, in the order it was rebased. The target moves once, to
    /// the final tip the gate judged, so the history a bisect walks is that tree and every row's
    /// `landed_sha` is its own commit on the way there.
    fn land_the_train(&self, train: Box<Train>, run: GateRun) -> Result<(), StoreError> {
        let target = train.cars[0].row.target.clone();
        let n = train.cars.len();
        // The gate ran on its own thread while the pass went on: the lease is asked again before
        // anything is written into the repository.
        if let Some(why) = self.lease_refusal() {
            let why = format!("{why}; the train goes back in line and is gated again");
            self.train_back(&train.cars, &why, &run.steps)?;
            return Ok(());
        }
        // One compare-and-swap to the gated final tip: the target is never on a tip the gate did not
        // judge, and a failure leaves every car in line.
        let prev = train.cars[n - 1].rebased.clone();
        let stopped = match git::update_ref_cas(&self.repo, &target, &prev, &train.tip) {
            Err(e) => Some(format!(
                "`{target}` moved while the train's gate ran (it was {}): {e}. Nothing of the \
                 train landed, and its branches go back in line and are rebased onto the new tip \
                 and gated again",
                train.tip
            )),
            Ok(()) => match git::follow_checkouts(&self.repo, &target, &train.tip, &prev) {
                Ok(()) => None,
                Err(e) => {
                    if git::undo_landing(&self.repo, &target, &prev, &train.tip).is_ok() {
                        Some(format!(
                            "{e}. The train's landing was undone: `{target}` is at {} as before, \
                             and its branches go back in line",
                            train.tip
                        ))
                    } else {
                        // The target holds the train and cannot be put back: it stands as landed.
                        self.raise(
                            "queue_blocked",
                            format!(
                                "a train onto `{target}` landed at {prev} but a checkout did not \
                                 follow and the landing could not be undone (the ref moved \
                                 again): {e}"
                            ),
                        );
                        None
                    }
                }
            },
        };
        if let Some(why) = stopped {
            self.raise(
                "queue_blocked",
                format!("a train onto `{target}` stopped: {why}"),
            );
            self.train_back(&train.cars, &why, &run.steps)?;
            return Ok(());
        }
        // Push, then install - once, on the last car's tip.
        let pushed = git::push(&self.repo, &target, &prev);
        let (pushed_text, may_install) = match &pushed {
            git::Pushed::Yes(s) => (s.clone(), true),
            git::Pushed::NoRemote => ("no `origin` to push to".to_string(), false),
            git::Pushed::Failed(e) => (format!("{PUSH_FAILED}: {e}"), false),
        };
        let cars: Vec<&MergeRow> = (0..n).map(|i| &train.cars[i].row).collect();
        if !may_install
            && self
                .refused_push(&cars, &target, &prev, &pushed_text, &run.steps)?
                .is_some()
        {
            return Ok(());
        }
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
        if train.install.is_empty() && !may_install {
            let why = if pushed == git::Pushed::NoRemote {
                "there is no `origin` to push to"
            } else {
                "the push failed"
            };
            install_note = format!(
                " Not installed: {why}, and nothing is installed that is not where it was pushed \
                 (decision 7)."
            );
        }
        if matches!(pushed, git::Pushed::Failed(_)) {
            // A failed push is said whether or not the project has an install step.
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

    /// The verdict of a train's install, after its last car was pushed - the same
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
                // Landed, so nothing is blocked: its own code.
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

    /// A train's gate was red - nothing landed and the target never moved. The train is split
    /// in two halves, first half gated first, so the failing row is found in about log2(n) gates
    /// (the bors/merge-train bisection). The failing row is parked `failed` like a lone branch.
    fn red_train(
        &self,
        train: &Train,
        words: String,
        steps: &[letibot_tokencore::store::MergeGateStep],
    ) -> Result<(), StoreError> {
        let target = train.cars[0].row.target.clone();
        let n = train.cars.len();
        let at = train.cars[n - 1].rebased.clone();
        let branches = train.branches();
        let tag = match &train.half {
            Some(t) => t.clone(),
            // A later train can rebuild the same tree; the clock keeps the verdicts apart.
            None => format!(
                "{}.{}:",
                at.chars().take(8).collect::<String>(),
                gate::now_ms()
            ),
        };
        let k = n.div_ceil(2);
        let (first, second) = train.cars.split_at(k);
        for (cars, side) in [(first, 'a'), (second, 'b')] {
            let half = format!("{tag}{side}");
            let why = half_sentence(
                &half,
                &format!(
                    "the train of {n} (`{}`) onto `{target}` at {tip} failed its gate at {at}, so \
                     it is bisected and this half is gated {when}",
                    branches.join("`, `"),
                    tip = train.tip,
                    when = if side == 'a' {
                        "next"
                    } else {
                        "after the first"
                    },
                ),
            );
            self.train_back(cars, &why, steps)?;
        }
        self.raise(
            "merge_failed",
            format!(
                "the gate of a train of {n} onto `{target}` at {tip} is RED. Nothing landed, and the \
                 branches (`{}`) are bisected to find the one that failed.\n\nThe failing output's \
                 end:\n\n{}",
                branches.join("`, `"),
                super::gate::for_the_model(&words),
                tip = train.tip,
            ),
        );
        Ok(())
    }

    /// The verdict of the install that followed a pushed landing.
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
                // Landed, so nothing is blocked: its own code.
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

    /// Landed - the row, the target's health at the landed tree, the branch retired.
    /// `train_note` is what a row that rode in a train adds (`, in a train of 3 (car 2)`); `""`
    /// for a lone landing, whose sentence is the one it has always been.
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
        // The machine is in the sentence a person reads after the landing: where a gate ran is
        // the whole of the fact a host gate adds, and the health row says it already.
        let why = format!(
            "landed on `{target}` at {rebased} (onto {tip}, {gate} gate){train_note}; \
             {pushed_text}.{install_note}",
            gate = self.env.gate.name()
        );
        let state = if pushed_text.starts_with(PUSH_FAILED) {
            MergeState::LandedLocal
        } else {
            MergeState::Landed
        };
        self.gating_to(
            row,
            state,
            Move::new(&me, why)
                .set("landed_sha", rebased.clone())
                .set("pushed", pushed_text)
                .set("gate_steps_json", rows_json(&all_steps)),
        )?;
        // The second half of this bisection reads it: green here puts the red in the other half.
        if let Some(t) = &landing.half {
            self.store
                .tell_once(&half_green_key(&self.key, t), gate::now_ms())?;
        }
        // And the owner of that branch: its work is on the target, the proof the lifecycle's
        // landed grace rests on (`landed_sha`). Only a recorded owner is written.
        if let Err(e) = letibot_tools::lifecycle::set_landed(&self.store, &row.owner, rebased) {
            self.raise(
                "queue_owner_landing",
                format!(
                    "`{}` landed at {rebased}, and its owner `{}` could not be told: {e}",
                    row.branch, row.owner
                ),
            );
        }
        // The gate passed on exactly this tree, so the target is green at it - not gated again. Through
        // `target_is_green`, like any green : a pending review of a red tip is withdrawn and an
        // alarm that stood is said to be over.
        let green = TargetHealth {
            repository: self.key.clone(),
            target: target.clone(),
            sha: rebased.clone(),
            state: HealthState::Green,
            checked_ms: gate::now_ms(),
            // The machine, as the gated-tip path says it: a green tip's row is where a reader
            // checks what judged it, and a landing is one more way a tip gets green.
            evidence: format!(
                "the queue's own gate passed on this tree (`{}`, {} gate)",
                row.branch,
                self.env.gate.name()
            ),
            gate_steps: run.steps,
        };
        self.store.set_target_health(&green, &me)?;
        self.target_is_green(&green)?;
        if git::branch_head(repo, &row.branch).as_deref() == Some(head.as_str()) {
            // A child still running in that tree keeps it : its session is held by
            // this daemon, and removing its cwd under it leaves every later call failing ENOENT.
            let live = self
                .store
                .review_row(&row.review_id)?
                .map(|r| r.session_id)
                .filter(|s| !s.is_empty() && self.env.door.session_live(s));
            // The queue's own landing is the proof, and it is taken here too - the branch goes only
            // when the sha the compare-and-swap put on the target is still on it. A landing is often a
            // REBASE, so the branch's own commits are not ancestors of the target
            let proved = git::commit_proof(
                repo,
                rebased,
                &target,
                &format!("the branch `{}` (landed at {rebased})", row.branch),
            );
            // And another daemon's session, or a process cwd'd inside, which this registry cannot see.
            let in_use = row
                .worktree
                .as_deref()
                .map(Path::new)
                .filter(|wt| wt.exists())
                .and_then(|wt| {
                    letibot_tools::lifecycle::in_use::why_kept(wt, Some(&self.store), &self.here())
                });
            let kept = match (&live, in_use) {
                (Some(s), _) => Err(format!(
                    "the worktree {} and `{}` are kept: the session `{s}` that works in it is \
                     still live. Remove them once it has ended (`git worktree remove` and \
                     `git branch -D`).",
                    row.worktree.as_deref().unwrap_or("?"),
                    row.branch
                )),
                (None, Some(why)) => Err(format!(
                    "the worktree {} and `{}` are kept: {why}. Remove them once nothing uses \
                     them (`git worktree remove` and `git branch -D`).",
                    row.worktree.as_deref().unwrap_or("?"),
                    row.branch
                )),
                (None, None) => match proved {
                    git::Proof::On => {
                        git::retire_branch(repo, &row.branch, row.worktree.as_deref())
                    }
                    other => Err(other.unproven().map(|(_, why)| why).unwrap_or_else(|| {
                        format!("`{}` is kept: {rebased} is not on `{target}`", row.branch)
                    })),
                },
            };
            if let Err(kept) = kept {
                // Said where a person reads it, and once per tree - the key the resource sweep
                // reads too ([`Self::release_one`]): one tree is one piece of news, whoever says it.
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

/// Why a repository's landings wait while its gate runs - `gate running on <sha> for Nm`.
pub fn gate_running_words(sha: &str, for_ms: u64) -> String {
    let short: String = sha.chars().take(10).collect();
    format!("gate running on {short} for {}m", for_ms / 60_000)
}

/// The clause a stopped gate's row leads with. A row that carries it waits its window before it is
/// gated again (`STOPPED_RETRY_MS`): whoever stopped it did so to get the box back, and taking the
/// row on the very next pass would undo that.
pub const STOPPED: &str = "its gate was stopped";

/// How long a stopped row or target waits before the gate is taken again - ten minutes: long enough
/// that a stop is felt and a person can act on it, short enough that a forgotten one does not hold
/// the queue for ever.
pub const STOPPED_RETRY_MS: u64 = 10 * 60 * 1000;

/// The verdict a stopped run carries. The runners' own sentence is about their kill (`the daemon is
/// stopping`); a person's or a model's stop replaces it, and that sentence is what the row records.
fn with_stop(run: GateRun, stop: Option<String>) -> GateRun {
    match (stop, run.verdict) {
        (Some(why), GateVerdict::Stopped(_)) => GateRun {
            steps: run.steps,
            verdict: GateVerdict::Stopped(why),
        },
        (_, verdict) => GateRun {
            steps: run.steps,
            verdict,
        },
    }
}

/// What a gate is for, as its published row says it: the kind of subject, the target, and the rows
/// (comma-joined for a train, empty for a target's own tip).
fn gate_subject(what: &GateFor) -> (&'static str, String, String) {
    fn of_train(t: &Train) -> (&'static str, String, String) {
        let ids: Vec<&str> = t.cars.iter().map(|c| c.row.id.as_str()).collect();
        (
            "train",
            t.cars
                .first()
                .map(|c| c.row.target.clone())
                .unwrap_or_default(),
            ids.join(","),
        )
    }
    match what {
        GateFor::Target { health, .. } => ("target", health.target.clone(), String::new()),
        GateFor::Land(l) => ("land", l.row.target.clone(), l.row.id.clone()),
        GateFor::Train(t) => of_train(t),
        GateFor::Install { landing, .. } => {
            ("land", landing.row.target.clone(), landing.row.id.clone())
        }
        GateFor::InstallTrain { train, .. } => of_train(train),
    }
}

/// Where a gate's build goes on the machine that runs it, which is where `status` reads the room left.
/// `local` is this box's `<worktree>/target`; a declared host builds under its root, at the same
/// `<repo>-<hex>` directory the local landing uses.
fn build_dir(host: &str, wt: &Path) -> String {
    let name = host.strip_suffix(" (firecode)").unwrap_or(host);
    if name == "local" {
        return gate::gate_target_dir(wt).display().to_string();
    }
    let root = letibot_tools::remote::Hosts::load()
        .get(name)
        .map(|h| letibot_tools::remote::expand_root(&h.root));
    match root {
        Some(root) => format!(
            "{}/gate/{}/target",
            root.trim_end_matches('/'),
            super::gate_host::dir_name(wt)
        ),
        // A host nobody declares any more: the sentence `status` prints still names where it runs.
        None => String::new(),
    }
}

/// The `queue_told` key that counts a target tip's gate kills (`""` for the sha: every tip's).
pub fn killed_key(repository: &str, target: &str, sha: &str) -> String {
    format!("killed:{repository}:{target}:{sha}")
}

/// A target's health a project has to hear about : red, timed out, or unjudged because
/// the gate's own setup failed. `blocked` for want of a declared gate has its own notice, and the other
/// blocked causes are the queue's own and are raised on the hubs.
pub fn emergency_health(h: &TargetHealth) -> bool {
    match h.state {
        HealthState::Red | HealthState::TimedOut => true,
        HealthState::Blocked => super::setup::unjudged(&h.evidence),
        _ => false,
    }
}

// ===== The supervisor =====

/// The daemon's queue service - finds the repositories this daemon may serve, claims their leases
/// runs a worker per lease held, and on stop joins them (their gates killed, their leases released).
pub struct QueueService {
    store_path: PathBuf,
    workspace: PathBuf,
    env: Arc<QueueEnv>,
    /// The repositories last said not to be queue targets - the SET, sorted: the line
    /// the log carries is one sentence about the folder, written again only when the set has changed.
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

    /// Whether this daemon is one of `repo`'s session daemons - its workspace is the checkout, under
    /// it, or one of its worktrees ([`super::serves`]). A workspace that merely contains it is not.
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
        // A lease whose holder is gone is given up by whoever passes, before anything
        // is served: the rows the pane drew as *served by* were daemons dead for weeks, on repositories
        // no daemon claims - so `acquire`'s takeover, which needs a claimant, never reached them.
        self.sweep_leases(store)?;
        self.resolve_carried(store)?;
        let mut not_targets: Vec<(String, String)> = Vec::new();
        for repo in self.repositories(store)? {
            let key = repo.display().to_string();
            if workers.contains_key(&key) {
                continue;
            }
            // A repository whose target declares no `Merge gate` is not a queue target
            // : no lease, no health row, so no head counts it unjudged. It is asked every
            // pass, and what is said about them is ONE line at the end of it ([`Self::say_not_a_targets`]).
            if let Some(target) = self.refusal(&repo) {
                not_targets.push((key, target));
                continue;
            }
            match store.acquire_queue_lease(&key, &self.env.me, gate::now_ms(), LEASE_TTL_MS)? {
                LeaseOutcome::Held { previous } => {
                    let recovered = store.recover_queue(
                        &key,
                        &self.env.me.owner,
                        gate::now_ms(),
                        self.env.review_deadline_ms,
                    )?;
                    // : a tip whose last verdict was red, blocked or timed out is
                    // gated again by the daemon that takes the queue - a restart is a re-gate.
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
                        self.env.door.announce(SessionEvent::warning(
                            "queue_wrong_build",
                            format!(
                                "this daemon serves NO queue: the store's schema is {theirs} and this \
                                 build knows {mine}. A daemon whose build does not match the store \
                                 never takes a queue (R61). Restart it on the current build."
                            ),
                        ));
                    }
                }
            }
        }
        self.say_not_a_targets(&not_targets);
        Ok(())
    }

    /// Every lease whose holder is gone is released, with the history saying why - see
    /// [`super::lease::gone`] for the two facts and why neither alone would do. Every pass
    /// over the WHOLE store rather than over this daemon's repositories
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

    /// Why `repo` is not a queue target, if it is not one - the target branch it lands on, when
    /// that branch's `AGENTS.md` has no `Merge gate` section. `None` means *serve it*
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

    /// One line - in the daemon's own log, never on a hub - for the repositories this pass
    /// found not to be queue targets, and nothing when the set is the one already said.
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

    /// The repositories this daemon may serve - every one its workspace is a session of
    /// ([`super::repositories_of_workspace`]): the checkout it is in, or each repository of a
    /// one-project folder of them.
    fn repositories(&self, _store: &Store) -> Result<Vec<PathBuf>, StoreError> {
        // Every repository of the workspace - one, or each of a one-org project folder's.
        Ok(super::repositories_of_workspace(&self.workspace)
            .into_iter()
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .collect())
    }

    /// Rows carried over from v23 name no repository yet - the daemon that serves where the
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

/// What the daemon's log owes the operator about the repositories it will not serve -
/// either the one line and the set to remember, or `None` when this set is the one already
/// said.
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

/// The one line - the folder, how many of its repositories never asked for a queue, and which
/// ones, because the question it answers is *why is nothing landing here* and the answer is a set.
/// One line for the folder, not one per repository : fifteen of those buried the banner.
pub(crate) fn not_a_target_line(workspace: &Path, not_targets: &[(String, String)]) -> String {
    let mut pairs: Vec<&(String, String)> = not_targets.iter().collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let names: Vec<String> = pairs.iter().map(|(k, _)| repo_name(k)).collect();
    match pairs.as_slice() {
        // Nothing is refused any more: the section is the whole of the opt-in, so this is what a folder
        // looks like after the repositories that wanted a queue committed one, and it is worth the line.
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

/// One line for the leases a pass gave up - the same fold as [`not_a_target_line`] and for the same
/// reason: the terminal is the daemon's log when it runs in the foreground. The rows keep each reason.
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

/// One repository's worker - passes until the lease is lost or the daemon stops; a store
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
                env.door.announce(SessionEvent::warning(
                    "queue_lease_lost",
                    format!(
                        "this daemon no longer holds the queues of {key} (its heartbeat was \
                         missed, or the store changed schema); another daemon serves them now"
                    ),
                ));
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

/// The first gate step that runs a `[boundary] firecode_only` program, said as a reason. Such a
/// step never runs on a host, whatever `gate_in` says.
pub(crate) fn firecode_only_step(repo: &Path, steps: &[String]) -> Option<String> {
    let only = crate::perimeter::load(repo).ok()?.firecode_only;
    if only.is_empty() {
        return None;
    }
    use letibot_tools::firecode_route::{Script, examine, script_of};
    let read = |p: &str| match std::fs::read(repo.join(p)) {
        Ok(b) => script_of(Ok(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Script::Absent,
        Err(e) => Script::Unreadable(e.to_string()),
    };
    steps.iter().find_map(|step| {
        examine(step, &only, &read)
            .hit
            .map(|h| format!("step `{step}` runs `{}`, which is firecode_only", h.program))
    })
}

#[cfg(test)]
pub(crate) mod tests;
