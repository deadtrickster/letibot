//! **`fanout {background: true}`** — the branches start, and ONE handle stands in for them (FB).
//!
//! `docs/remote-hosts.md` § *As built (FB)* is the contract, and this file is the half of it that
//! `docs/remote-hosts.md` § RH8 said could not be done without a composite process host. It can,
//! and this is the smallest honest shape: **one handle in THIS session's job table whose
//! `wait_job` waits on N branches**, each on its own machine through that machine's own process
//! group.
//!
//! # Why not a composite `ProcessHost`
//!
//! A branch is a job of the *host's* table (`RemoteProcesses`), so `jobwatch` — whose whole job is
//! `wait_job` on this session's host — could never settle it. Teaching the seam what a host is
//! (`SpawnRequest.host`, `JobView.host`, a merged `jobs()`, a recorder per host) would fix that and
//! change every caller of it. A [`Aggregate`] is one table beside the jobs instead, and then
//! `jobs`, `job_output`, `job_kill`, `job_wait`, the activity's `waiting on: job …` and the
//! settlement that wakes the model are all the code that was already there.
//!
//! # The three answers this file has to get right
//!
//! * **The settlement is a wake, not a report.** `jobwatch` waits on the handle and hands the model
//!   the state, the numbers and — through [`ProcessHost::settlement_note`] — the table. A fanout's
//!   answer IS its table, so that is the one note a settlement carries, cut to
//!   [`REPORT_LINES`] lines because a wake is read in the middle of a turn.
//! * **`job_output` reads a journal, not a table.** A table is re-rendered as branches settle, so
//!   an offset into it would mean something different a second later; the journal is append-only
//!   (`<host> · started`, then `<host> · exited 0 · 1.2s`), which is what makes paging an honest
//!   operation while the thing runs.
//! * **`job_kill` kills every branch on every host**, through each branch's own process group
//!   (RH1), and each branch's `process` record ends `stopped`. There is no cgroup of ours holding
//!   them and a signal from here would reach nothing.
//!
//! # What this does not survive, said here rather than discovered
//!
//! The table is in memory. A daemon that restarts loses the handle: its watcher was a thread of
//! the old process, so **nothing wakes the model with the table** — `job_output` on the old handle
//! answers *no job called that*, which is a refusal and not a hang, and it claims nothing about
//! work still in flight. The **branches** do survive, because each is a `process` record the
//! supervisor adopts (RH7): they are still killed at their deadline or by a session's close.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::exec::jobs::Capture;
use crate::exec::{Aggregate, JobId, JobState, OutputSlice, ProcessHost, Reaped};
use crate::fleet::{self, Cell, Fate, Fleet, Leg, Running};
use crate::lease::Blocks;
use crate::runtime::Invocation;

use super::fanout::{TOOL, table};

/// **How much of the per-host journal one fanout keeps.** A window is not the point here — the
/// journal is one line per branch, one per ending, and the table at the end — so this bounds a
/// pathological case (a fanout over a thousand hosts) and not a size an ordinary one reaches.
pub const JOURNAL_BYTES: usize = 256 * 1024;

/// **How many lines of the table a settlement carries.** A wake is read in the middle of a turn,
/// and forty rows delivered into one is a table nobody reads; the cut names itself and the whole of
/// it is one `job_output` away.
pub const REPORT_LINES: usize = 24;

/// **Start the branches and hand back ONE handle.** The blocking form is
/// [`super::fanout::Fanout::answer`]'s default; this is what `background: true` selects.
pub(super) fn start(
    fleet: Fleet,
    plan: Vec<Leg>,
    held: Vec<letibot_tokencore::store::Lease>,
    blocks: Option<Arc<Blocks>>,
    local: Option<Arc<dyn ProcessHost>>,
    work: &str,
) -> Invocation {
    // **The host is asked for BEFORE the first branch starts.** A session with none has nowhere to
    // keep a handle, and branches running with nobody able to reach them is the silent omission
    // this tool exists to prevent — one step worse for having started them.
    let Some(local) = local else {
        return Invocation::failed(
            "`background: true` and this session has no process host",
            "A fanout that settles in the background is one handle in THIS session's job table, \
             and this session cannot hold processes at all — so the handle would settle nowhere \
             and its table would reach nobody. Drop `background` and the call blocks for its table \
             instead.",
        );
    };
    let handle = JobId::next();
    let job = Arc::new(FanoutJob {
        handle: handle.clone(),
        slug: local.claim_name(TOOL),
        work: work.to_string(),
        declared: fleet.declared(),
        plan,
        fleet,
        blocks,
        leases: held,
        branches: Mutex::new(Vec::new()),
        started: Instant::now(),
        inner: Mutex::new(Inner::default()),
        settled: Condvar::new(),
        capture: Mutex::new(Capture::new(JOURNAL_BYTES)),
    });
    job.start_branches();
    if let Err(why) = local.hold(&handle, Arc::clone(&job) as Arc<dyn Aggregate>) {
        // **Nothing is left running.** The branches started a line ago and this session cannot hold
        // a handle for them, so they are ended the way a refusal ends anything: by their own
        // process groups, with each branch's record saying `stopped`.
        let killed = job.abandon(&why.to_string());
        return Invocation::failed(
            "`background: true` and this session's job table cannot hold a job of jobs",
            format!("{why}\n\n{killed}\nDrop `background` and the same call blocks for its table."),
        );
    }
    let started = job.branches.lock().map(|b| b.len()).unwrap_or(0);
    let hosts: Vec<String> = job.plan.iter().map(|l| l.host.clone()).collect();
    let slug = job.slug.clone();
    job.clone().track();
    Invocation::backgrounded(
        handle.0.clone(),
        Duration::ZERO,
        letibot_transcript::Backgrounding::Asked,
        format!(
            "carry on — the table reaches you on its own when the last branch ends, so there is \
             nothing to wait for. `job_output` with job=\"{handle}\" reads the per-host progress so \
             far; `job_kill` stops every branch, on every host."
        ),
        format!(
            "started `{slug}` in the background: {started} branch(es) on {}.\n  name: {slug} — \
             `job_output`, `job_kill` and `job_wait` take this or `{handle}`\n\n{}\n",
            hosts.join(", "),
            super::DELIVERED,
        ),
    )
}

/// **One backgrounded fanout, as the session's job table holds it** — the [`Aggregate`] behind the
/// handle `fanout {background: true}` answers with.
///
/// # Why the thread lives here
///
/// Somebody has to watch N branches on N machines, and nothing else can: this session's process
/// host knows no `RemoteProcesses`, so its `wait_job` cannot block on a branch. So a thread of the
/// fanout's own polls them the way the blocking loop does — one pass over the branches, a breath
/// between passes — and the handle is what makes that thread's ending visible to everything above:
/// `state`, `output`, `wait` and `kill` are the four questions `jobwatch`, `job_output`, `job_wait`
/// and `job_kill` already ask of a job.
pub struct FanoutJob {
    handle: JobId,
    /// The name it answers to — `fanout`, `fanout-2`, … claimed out of the host's own record.
    slug: String,
    work: String,
    /// What `hosts.toml` declared, for the table's sentence about rows that never ran.
    declared: Vec<String>,
    plan: Vec<Leg>,
    fleet: Fleet,
    blocks: Option<Arc<Blocks>>,
    leases: Vec<letibot_tokencore::store::Lease>,
    /// Every branch this fanout started, in plan order — the ones still running and the ones
    /// already settled. Kept whole so a kill can report what a branch *was* after it ended.
    branches: Mutex<Vec<Running>>,
    started: Instant,
    inner: Mutex<Inner>,
    /// Rung when the last branch settles — what [`Aggregate::wait`] blocks on.
    settled: Condvar,
    /// The journal `job_output` reads. See the module header for why it is append-only.
    capture: Mutex<Capture>,
}

/// What the tracker thread and every reader share.
#[derive(Default)]
struct Inner {
    /// The rows of the branches that have settled, in the order they did.
    done: Vec<Cell>,
    /// The label of every branch that has a fate, so the tracker asks once.
    settled: Vec<String>,
    /// **Who ended it, when somebody did** — `None` until a kill.
    stopped: Option<String>,
    state: Option<JobState>,
    took: Option<Duration>,
    /// The table, once it is over.
    report: String,
}

impl FanoutJob {
    fn journal(&self, line: &str) {
        self.capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("{line}\n").as_bytes());
    }

    /// **Start every branch**, journal the ones that started and the rows of the ones that did not.
    /// A leg that never ran is a row immediately — the blocking form's rule, one call earlier.
    ///
    /// On the caller's thread and not the tracker's, so the call that asked for the background
    /// fanout is the one that knows which branches exist when it answers.
    fn start_branches(&self) {
        self.journal(&format!(
            "fanout · {} planned on {} · work dir `{}` on each host",
            self.plan.len(),
            if self.declared.is_empty() {
                "no declared host".to_string()
            } else {
                self.declared.join(", ")
            },
            self.work
        ));
        let mut started = Vec::with_capacity(self.plan.len());
        let mut never = Vec::new();
        for leg in &self.plan {
            match self.fleet.start(leg) {
                Ok(run) => {
                    // **`started` is the line of a branch that is still running.** The journal is
                    // append-only, so the ending follows this line rather than replacing it, which
                    // is what lets `job_output` read the same window at any moment.
                    self.journal(&format!("  {} · started", cell_label(leg)));
                    started.push(run);
                }
                Err(why) => {
                    let cell = Cell::never(leg, why);
                    self.journal(&format!("  {}", cell.line()));
                    never.push(cell);
                }
            }
        }
        {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            g.settled = never.iter().map(|c| c.label.clone()).collect();
            g.done = never;
        }
        *self.branches.lock().unwrap_or_else(|e| e.into_inner()) = started;
    }

    /// **Watch the branches until the last one ends**, then write the table.
    fn track(self: Arc<Self>) {
        let me = Arc::clone(&self);
        let name = format!("fanout-{}", &self.handle.0[..self.handle.0.len().min(20)]);
        if let Err(e) = std::thread::Builder::new()
            .name(name)
            .spawn(move || me.track_loop())
        {
            // **A fanout nobody is watching is SAID, not left to hang.** The branches are still
            // there and still supervised; what is lost is the parent's settlement, and a handle
            // that never settles is the silent hang this slice exists to refuse.
            eprintln!(
                "letibot: the fanout watcher for `{}` did not start ({e}); its branches run on, \
                 but the per-host table will not be delivered",
                self.handle
            );
            self.abandon(&format!("its watcher could not start: {e}"));
        }
    }

    /// The loop itself — one pass over the branches, so a slow machine costs its own round trip and
    /// not the whole fanout's, and a breath between passes.
    fn track_loop(&self) {
        loop {
            for run in self.snapshot() {
                if self.has_settled(&run.leg.label) {
                    continue;
                }
                match self.fleet.settled(&run) {
                    Some((state, took)) => {
                        // **A row is written in ONE place.** Whoever ended the branch — a kill, a
                        // deadline, the command itself — the fate comes from the flag this fanout
                        // keeps, and the tracker is the only writer of `done`.
                        let fate = match self.stopped_by() {
                            Some(by) => Fate::Stopped { why: by, took },
                            None => Fate::Ended { state, took },
                        };
                        self.settle_one(&run, fate);
                    }
                    None => {
                        // The deadline, enforced here as well as on the record: the supervisor owns
                        // the record's deadline, and this loop must not leave a branch waiting for
                        // a tick it does not control.
                        if run.leg.deadline.is_some_and(|at| Instant::now() >= at) {
                            self.fleet.stop(&run, fleet::EXPIRED);
                            self.settle_one(
                                &run,
                                Fate::Stopped {
                                    why: format!("{} passed", fleet::EXPIRED),
                                    took: run.started.elapsed(),
                                },
                            );
                        }
                    }
                }
            }
            if self
                .snapshot()
                .iter()
                .all(|r| self.has_settled(&r.leg.label))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(120));
        }
        self.finish();
    }

    fn snapshot(&self) -> Vec<Running> {
        self.branches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn has_settled(&self, label: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .settled
            .iter()
            .any(|l| l == label)
    }

    fn stopped_by(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopped
            .clone()
    }

    /// **Write one branch's ending down and journal its row.**
    fn settle_one(&self, run: &Running, fate: Fate) {
        self.fleet.record_end(run, &fate);
        let cell = self.fleet.cell(run, None, fate);
        self.journal(&format!("  {}", cell.line()));
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.settled.push(run.leg.label.clone());
        g.done.push(cell);
    }

    /// **The table, the blocks, and the state** — once, whichever way the fanout ended.
    fn finish(&self) {
        let rows = {
            let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if g.state.is_some() {
                return;
            }
            fleet::cells(&self.plan, &g.done)
        };
        let took = self.started.elapsed();
        // **Every block goes back**, since a branch's containers are the reason it existed. Out of
        // the lock: a release may run an ssh round trip, and holding the state through that would
        // stall every reader of this handle.
        let mut released = Vec::new();
        if let Some(blocks) = &self.blocks {
            for lease in &self.leases {
                released.extend(blocks.release(lease, "the fanout ended"));
            }
        }
        let text = table(
            &self.work,
            &self.declared,
            &self.plan,
            &rows,
            took,
            &released,
        );
        let state = match self.stopped_by() {
            Some(by) => JobState::Killed { by },
            // **The parent's own exit status is the table's reduction**: 0 when every cell exited 0
            // and 1 otherwise. It is the one word a settlement has for *the table is not all
            // green*; the table itself says which cells, and it is in the same message.
            None => JobState::Exited {
                code: if rows.iter().all(|c| c.fate.ok()) {
                    0
                } else {
                    1
                },
            },
        };
        {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if g.state.is_some() {
                return;
            }
            g.state = Some(state);
            g.took = Some(took);
            g.report = text.clone();
        }
        self.journal(&text);
        self.settled.notify_all();
    }

    /// **End it early, before anything watches it** — a run whose handle the session refused, or
    /// one whose watcher thread could not start. Every branch is stopped on its own host and the
    /// table says so; the caller is handed the words.
    fn abandon(&self, why: &str) -> String {
        {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if g.state.is_none() {
                g.stopped = Some(why.to_string());
            }
        }
        for run in self.snapshot() {
            if self.has_settled(&run.leg.label) {
                continue;
            }
            self.fleet.stop(&run, why);
            self.settle_one(
                &run,
                Fate::Stopped {
                    why: why.to_string(),
                    took: run.started.elapsed(),
                },
            );
        }
        self.finish();
        self.report()
    }
}

impl Aggregate for FanoutJob {
    fn command(&self) -> String {
        // **There is no single command line here**, and naming the first branch's would be a claim
        // about all of them — so the row's command column is the plan.
        format!(
            "fanout: {} branch(es) on {}",
            self.plan.len(),
            self.plan
                .iter()
                .map(|l| l.host.clone())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    fn slug(&self) -> Option<String> {
        Some(self.slug.clone())
    }

    fn state(&self) -> JobState {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .state
            .clone()
            .unwrap_or(JobState::Running)
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn ran_for(&self) -> Option<Duration> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).took
    }

    fn produced(&self) -> u64 {
        self.capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .produced()
    }

    fn since_last_output(&self) -> Option<Duration> {
        self.capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .since_last()
    }

    fn output(&self, from: u64, limit: usize) -> OutputSlice {
        self.capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .slice(from, limit)
    }

    fn wait(&self, within: Duration) -> Option<JobState> {
        let began = Instant::now();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(state) = &g.state {
                return Some(state.clone());
            }
            let left = within.saturating_sub(began.elapsed());
            if left.is_zero() {
                return None;
            }
            let (held, _) = self
                .settled
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner());
            g = held;
        }
    }

    fn kill(&self, by: &str) -> Vec<Reaped> {
        let branches = self.snapshot();
        let mut out = Vec::new();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for run in &branches {
            // **Only the branches that are still there** — `observed` is what `job_kill` prints as
            // *what was running*, and a settled branch counted here would be a kill claiming to
            // have found work that had already ended.
            if g.settled.iter().any(|l| l == &run.leg.label) {
                continue;
            }
            // `comm` carries the label and the host because a process name is not readable from
            // here: this box has no `/proc` entry for a process on another machine, and inventing
            // one would be a number that named somebody else's.
            out.push(Reaped {
                pid: self.fleet.view(run).map(|v| v.pid).unwrap_or(0),
                comm: format!("{}@{}", run.leg.label, run.leg.host),
                cmdline: run.leg.command.clone(),
            });
            // Each branch through its own host and its own process group (RH1) — there is no
            // cgroup of ours holding them, and a signal from here would reach nothing.
            self.fleet.stop(run, by);
        }
        if g.state.is_none() {
            g.stopped = Some(by.to_string());
        }
        drop(g);
        // **And the branches' endings follow from the same fact**, one pass of the tracker later:
        // the tracker is the only row writer, so a kill cannot write two rows for one branch.
        out
    }

    fn report(&self) -> String {
        let text = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .report
            .clone();
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() <= REPORT_LINES {
            return text;
        }
        let rest = lines.len() - REPORT_LINES;
        format!(
            "{}\n… and {rest} more line(s) — `job_output` reads the whole table",
            lines[..REPORT_LINES].join("\n")
        )
    }
}

/// The name a branch's journal line opens with — `fake1 on fake1`, `local (local)` — the same words
/// [`Cell::line`] uses, so a branch that is running and the same branch once it has ended are two
/// lines a reader follows rather than two spellings of one host.
fn cell_label(leg: &Leg) -> String {
    if leg.host == fleet::LOCAL {
        format!("{} (local)", leg.label)
    } else {
        format!("{} on {}", leg.label, leg.host)
    }
}
