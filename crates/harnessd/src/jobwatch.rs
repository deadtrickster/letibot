//! A background job's end, published the moment it is true.
//!
//! The **start** of a background job needs no watcher: the `bash` call that began
//! it finishes as `ToolOutcome::Backgrounded`, whose `handle` is the job id, and
//! the event stream already carries that to every head. The **end** needs one, and
//! the reason is in the job's own lifetime: a session-scoped job outlives the turn
//! that started it — that is what background is *for* — so it settles when no turn
//! is running, when the only events a hub publishes are the ones the daemon
//! publishes itself. The runtime reaps a job when somebody asks (`job_wait`, or
//! turn-end scope reaping); between turns, nobody asks. Without this, every head's
//! picture of a background job was frozen at "running" forever.
//!
//! # Blocked, not polling
//!
//! §18.1-I12: "no timer, no poll loop" is a property this daemon states about
//! itself, and a watcher that woke every second to ask `jobs()` would break it
//! while adding nothing — the host can block. So this is the monitor-wake shape:
//! **one thread per watched job**, blocked in
//! [`ProcessHost::wait_job`], woken by the job's own settlement, publishing
//! [`SessionEvent::JobSettled`], and done. No background jobs, no threads — the
//! laziness rule `Sessions::arm_wake` keeps, and for the same reason: watchers
//! that accumulate are T24's shape.
//!
//! # Why the handles are weak
//!
//! The thread holds `Weak` references to the host and the hub and upgrades them
//! once per wait chunk. A session that closes takes its backend and its hub with
//! it; a watcher holding strong handles would keep the host — and with it the
//! session-scope cgroup its jobs live in — alive for as long as the thread stayed
//! blocked. The chunk is the shutdown re-check, the same role the bell's deadline
//! plays for the monitor waiter: at most [`WATCH_CHUNK`] after close, the thread
//! wakes, fails to upgrade, and is gone.
//!
//! # Why a thread per job and not one per session
//!
//! One thread per session would have to pick a job to block on and poll the rest —
//! a poll loop again. One thread per job blocks on exactly its own job and ends
//! with it. A session with three long builds has three blocked threads, which is
//! the shape the monitor waiter already puts a session in, and each is woken by
//! the thing it reports.
//!
//! # The head is told and the model is not — which is the whole of R7
//!
//! [`SessionEvent::JobSettled`] has been on the wire since protocol 14 and the jobs
//! pane folds it, so the **head** knows. Nothing in `crates/turn/` or the tools layer
//! reads it, so the **model** — the one process that could act on the job ending —
//! learns nothing, and its only route to a result is `job_wait`. That is the
//! affordance the operator hit: *"as soon as i background a job model does `job_wait`
//! and things block again"*, and it is not discipline, it is the absence of any other
//! way to find out.
//!
//! So a settlement is published twice, to the two readers that need it:
//!
//! 1. [`SessionEvent::JobSettled`] to the hub, as it always was — the durable fact,
//!    the pane's row, every head's picture.
//! 2. A [`JobCompletion`] onto [`JobWatchers::completions`], which the harness drains
//!    in [`crate::harness::Harness::wake`] and **submits to the model as a turn of its
//!    own**, unprompted. That is the hop that was missing.
//!
//! The bell is rung for the second one, because a settlement lands **between turns**,
//! when the worker is blocked in `Registry::next_work` and nothing else would wake it.
//! It is the same bell monitors ring for a firing, and the ordering rule is the same
//! one `Bell::next_any` keeps: a wake has nobody waiting on it, a head that pressed
//! enter does — so a queued prompt is **always** served before a completion.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Bell;
use letibot_tools::exec::{JobId, JobState, ProcessHost, Waited};
use letibot_tools::{ToolEvent, ToolEventSink};
use letibot_transcript::ToolOutcome;

/// How long one blocking wait lasts before the thread re-checks its handles and
/// the stop flag. A job's settlement wakes the wait **immediately** — the condvar
/// under the job's state is what `wait_job` sleeps in — so this bounds only how
/// long a closing session's watcher can linger, never how late a settlement is.
const WATCH_CHUNK: Duration = Duration::from_secs(5);

/// **One background job's end, as the MODEL must be told it.**
///
/// Not [`SessionEvent::JobSettled`], and deliberately not on the wire. The event is
/// the head-facing fact and carries exactly the scalars a pane draws (`job`, `state`,
/// `produced`, `elapsed_ms`); a completion carries one field more — the **command** —
/// because the sentence the model is handed has to say what ended, and the command is
/// only reachable from the live [`JobView`] at the moment it settles. Putting it on
/// the event would be a wire change nobody needs: no head draws a command from a
/// settlement, and the head that has the row already has the command from the tool
/// result that backgrounded it.
///
/// Built by the watcher (which has the [`JobView`]), queued on
/// [`JobWatchers::completions`], and drained by
/// [`crate::harness::Harness::wake`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCompletion {
    /// The job's handle, as the backgrounded result printed it.
    pub job: String,
    /// The command it ran. Empty when the job was already reaped and the view is
    /// gone, which the notice says rather than inventing one.
    pub command: String,
    /// What happened to the process: `exited 0`, `signalled 15`, `killed by job_kill`.
    pub state: String,
    /// Bytes the job wrote, all streams together — the `job_output` denominator.
    pub produced: u64,
    /// Wall time from spawn to settlement.
    pub elapsed_ms: u64,
}

/// The per-session set of watched background jobs.
///
/// Held by the harness, fed by [`JobWatchSink`] as backgrounded results pass
/// through it, and stopped when the session's backend closes.
pub struct JobWatchers {
    host: Weak<dyn ProcessHost>,
    hub: Weak<Hub>,
    /// Settlements the model has not been told yet, in the order they happened.
    /// Drained by [`JobWatchers::take_completions`] on the harness's next wake.
    completions: Arc<Mutex<VecDeque<JobCompletion>>>,
    /// The worker's bell, rung when a completion is queued so a settlement between
    /// turns is not a fact nobody acts on. `None` when there is no daemon — a
    /// harness driven directly by a test has no worker to wake and honestly says so.
    bell: Option<Arc<Bell>>,
    stop: Arc<AtomicBool>,
    /// Jobs with a thread blocked on them, so a re-finished call id cannot start
    /// a second watcher for one job.
    watching: Arc<Mutex<HashSet<String>>>,
    /// Jobs already published, so a late duplicate finish cannot publish a
    /// settlement twice.
    settled: Arc<Mutex<HashSet<String>>>,
}

impl JobWatchers {
    /// A watcher set for one session's host and hub. Weak by design — see the
    /// module header.
    ///
    /// `bell` is the daemon's worker bell, so a settlement between turns wakes the
    /// model. `None` for a harness with no daemon behind it: the completion is still
    /// queued and the next `wake` would still drain it, but nothing rings to cause
    /// one, which is the truth for a session no worker is serving.
    pub fn new(host: &Arc<dyn ProcessHost>, hub: &Arc<Hub>, bell: Option<Arc<Bell>>) -> Arc<Self> {
        Arc::new(JobWatchers {
            host: Arc::downgrade(host),
            hub: Arc::downgrade(hub),
            completions: Arc::new(Mutex::new(VecDeque::new())),
            bell,
            stop: Arc::new(AtomicBool::new(false)),
            watching: Arc::new(Mutex::new(HashSet::new())),
            settled: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    /// **Take every completion the model has not been told yet.** One drain per
    /// turn: what comes back is what the harness submits, and taking it is what
    /// stops a settlement being delivered twice.
    pub fn take_completions(&self) -> Vec<JobCompletion> {
        let mut g = self.completions.lock().expect("job completions");
        g.drain(..).collect()
    }

    /// **Is this job's completion already on its way to the model?**
    ///
    /// True while a watcher thread is blocked on the job. That thread publishes the
    /// settlement to the hub and queues a [`JobCompletion`], which
    /// [`crate::harness::Harness::wake`] submits as a turn of its own — so from the
    /// moment the watcher exists, the model is going to be told whether it asks or
    /// not. Nothing `job_wait` does can change that, which is exactly why R23 makes
    /// the wait return instead of blocking: the answer is in flight.
    ///
    /// **A settled job is deliberately `false`.** Once the watcher has published, it
    /// is moved from `watching` to `settled` and the thread ends. A `job_wait` on a
    /// job in that state is answered by the caller's own "had already finished"
    /// branch, which is the honest sentence for it — *nothing was waited for*, rather
    /// than *you are about to be told*. Reporting `true` here for a job whose
    /// delivery has already happened would make the two readings one.
    ///
    /// False for a job this session was never watching: a job from another session, a
    /// scope's contents, anything a runtime with no daemon behind it sees. Those
    /// waits keep blocking for exactly as long as they were asked to.
    pub fn delivering(&self, job: &str) -> bool {
        self.watching.lock().expect("job watchers").contains(job)
    }

    /// Stop every watcher this set has spawned. Called when the session's backend
    /// closes: the threads wake within [`WATCH_CHUNK`], fail to upgrade their
    /// handles, and exit without publishing.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Watch one job. Idempotent: a job already watched, or already settled, is
    /// left alone.
    pub fn watch(self: &Arc<Self>, job: String) {
        {
            let mut watching = self.watching.lock().expect("job watchers");
            if watching.contains(&job) || self.settled.lock().expect("job watchers").contains(&job)
            {
                return;
            }
            watching.insert(job.clone());
        }
        let host = Weak::clone(&self.host);
        let hub = Weak::clone(&self.hub);
        let completions = Arc::clone(&self.completions);
        let bell = self.bell.clone();
        let stop = Arc::clone(&self.stop);
        let watching = Arc::clone(&self.watching);
        let settled = Arc::clone(&self.settled);
        let _ = std::thread::Builder::new()
            .name(format!("job-watch-{}", &job[..job.len().min(20)]))
            .spawn(move || watch_one(host, hub, completions, bell, stop, watching, settled, job));
    }
}

#[allow(clippy::too_many_arguments)]
fn watch_one(
    host: Weak<dyn ProcessHost>,
    hub: Weak<Hub>,
    completions: Arc<Mutex<VecDeque<JobCompletion>>>,
    bell: Option<Arc<Bell>>,
    stop: Arc<AtomicBool>,
    watching: Arc<Mutex<HashSet<String>>>,
    settled: Arc<Mutex<HashSet<String>>>,
    job: String,
) {
    loop {
        let (Some(host), Some(hub)) = (host.upgrade(), hub.upgrade()) else {
            // The session is gone — backend, hub, or both. Nothing left that a
            // settlement would be true of.
            break;
        };
        match host.wait_job(&JobId(job.clone()), WATCH_CHUNK) {
            Ok(Waited::Happened { state, .. }) => {
                // The wake's own reading of the state is the truth; the view, if
                // it still exists, adds the numbers a settlement reports. It can
                // be gone already — a scope reap at session end removes jobs —
                // and then the settlement carries the state and no numbers,
                // rather than numbers invented for a process nobody can measure.
                let view = host.job(&JobId(job.clone()));
                let word = state
                    .map(|s: JobState| s.word())
                    .or_else(|| view.as_ref().map(|v| v.state.word()))
                    .unwrap_or_else(|| "gone".to_string());
                let produced = view.as_ref().map(|v| v.produced).unwrap_or(0);
                let elapsed_ms = view
                    .as_ref()
                    .and_then(|v| v.ran_for)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                // The command is read here, from the live view, because this is the
                // last moment it exists: a settlement is delivered between turns, by
                // which time the job is usually reaped and `/proc` no longer says.
                let command = view.as_ref().map(|v| v.command.clone()).unwrap_or_default();
                hub.publish(SessionEvent::JobSettled {
                    job: job.clone(),
                    state: word.clone(),
                    produced,
                    elapsed_ms,
                });
                // **And the model, which the event does not reach.** Queued for the
                // harness's next wake, and the bell rung because a settlement lands
                // between turns, when nothing else is asking the worker for anything.
                completions
                    .lock()
                    .expect("job completions")
                    .push_back(JobCompletion {
                        job: job.clone(),
                        command,
                        state: word,
                        produced,
                        elapsed_ms,
                    });
                if let Some(bell) = &bell {
                    bell.ring_wake(&hub.session_id());
                }
                settled.lock().expect("job watchers").insert(job.clone());
                break;
            }
            // Still running — or never seen, which is a boot window and not a
            // finish. The chunk is the shutdown re-check and nothing else: a
            // settlement wakes the wait at once, so a long-lived job spends its
            // whole life in this arm.
            Ok(Waited::Deadline { .. } | Waited::NeverStarted { .. }) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
            }
            // The job is gone from the host: its scope was reaped with the
            // session, or the host dropped the record. There is no hub left that
            // a settlement would be true of.
            Err(_) => break,
        }
    }
    watching.lock().expect("job watchers").remove(&job);
}

/// A [`ToolEventSink`] that starts a watcher for every job a call leaves behind.
///
/// A decorator, for the same reason the intent encoder's `IntentSink` is one:
/// wiring it is one line at the call site, and forgetting it is visible rather
/// than silent. It adds nothing to what heads see — the start of a background job
/// is already on the wire, the `Finished` event's outcome *is* `Backgrounded`,
/// handle and all — it only tells the daemon's watcher that the job now exists
/// and will settle, which is the half the event stream cannot say.
pub struct JobWatchSink<S> {
    inner: S,
    watch: Option<Arc<JobWatchers>>,
}

impl<S> JobWatchSink<S> {
    /// `None` for `watch` when the session has no process host: nothing can be
    /// backgrounded, so there is nothing to watch.
    pub fn new(inner: S, watch: Option<Arc<JobWatchers>>) -> Self {
        JobWatchSink { inner, watch }
    }
}

impl<S: ToolEventSink> ToolEventSink for JobWatchSink<S> {
    fn emit(&mut self, event: ToolEvent) {
        if let Some(w) = &self.watch
            && let ToolEvent::Finished {
                outcome: ToolOutcome::Backgrounded { handle, .. },
                ..
            } = &event
        {
            w.watch(handle.clone());
        }
        self.inner.emit(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tools::RecordingToolSink;
    use letibot_tools::exec::{HostProcesses, ScopeKind, SpawnRequest};
    use std::time::Instant;

    /// A host over a temp root, the way `exec::host`'s own tests build one. The
    /// box needs a cgroup v2 subtree to delegate; every host-spawning test in
    /// tools already assumes the same.
    fn a_host() -> Arc<HostProcesses> {
        let root = std::env::temp_dir().join(format!("letibot-jobwatch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Arc::new(HostProcesses::new(&root).expect("this box has a cgroup v2 tree"))
    }

    fn spawn_a_short_job(host: &HostProcesses, secs: &str) -> JobId {
        host.spawn(&SpawnRequest {
            command: format!("sleep {secs}"),
            cwd: "/".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
        })
        .expect("spawn")
    }

    /// The one `JobSettled` the hub holds for `job`, if it has arrived yet.
    fn settlement_for(hub: &Hub, job: &str) -> Option<(String, u64, u64)> {
        hub.retained().into_iter().find_map(|e| match e.event {
            SessionEvent::JobSettled {
                job: ref j,
                ref state,
                produced,
                elapsed_ms,
            } if j == job => Some((state.clone(), produced, elapsed_ms)),
            _ => None,
        })
    }

    /// Poll the hub's log for the settlement rather than sleeping a fixed time:
    /// the watcher publishes the moment the job exits, and the test ends at
    /// whichever comes first.
    fn wait_for_settlement(hub: &Hub, job: &str) -> (String, u64, u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(hit) = settlement_for(hub, job) {
                return hit;
            }
            assert!(
                Instant::now() < deadline,
                "no JobSettled for {job} within 10s; log: {:?}",
                hub.retained()
                    .iter()
                    .map(|e| e.event.kind())
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn a_backgrounded_result_through_the_sink_publishes_the_jobs_settlement() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        let hub = Hub::new("s-jobs");
        let host = a_host();
        let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);
        let mut sink = JobWatchSink::new(RecordingToolSink::default(), Some(watchers));

        let id = spawn_a_short_job(&host, "0.3");
        sink.emit(ToolEvent::Finished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: ToolOutcome::Backgrounded {
                handle: id.0.clone(),
                ran_for_ms: 0,
                how: letibot_transcript::Backgrounding::Asked,
                next: "job_wait".into(),
            },
            payload_digest: letibot_tools::payload_digest(""),
            inline_bytes: 0,
            full_bytes: 0,
            spill: None,
            repairs: 0,
            edit: None,
        });

        let (state, _produced, elapsed_ms) = wait_for_settlement(&hub, &id.0);
        assert_eq!(state, "exited 0", "the word a listing shows, not ok/error");
        // The job slept ~300ms; the number comes off the job's own stamps, so it
        // is the real runtime and not the watcher's wait time.
        assert!(
            (250..5_000).contains(&elapsed_ms),
            "elapsed_ms {elapsed_ms} is not the job's runtime"
        );
    }

    #[test]
    fn a_job_watched_twice_and_again_after_settlement_publishes_once() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        let hub = Hub::new("s-jobs");
        let host = a_host();
        let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);

        let id = spawn_a_short_job(&host, "0.2");
        watchers.watch(id.0.clone());
        // A re-finished call id, or a second head reporting the same job, must
        // not start a second watcher: one job, one settlement.
        watchers.watch(id.0.clone());

        wait_for_settlement(&hub, &id.0);
        // And a watch that arrives after the settlement is a no-op too — the
        // settlement is already on the wire and a second one would be a job
        // that died twice.
        watchers.watch(id.0.clone());
        std::thread::sleep(Duration::from_millis(300));

        let count = hub
            .retained()
            .iter()
            .filter(|e| matches!(&e.event, SessionEvent::JobSettled { job, .. } if job == &id.0))
            .count();
        assert_eq!(count, 1, "one job, one settlement");
    }

    /// **R23's question, answered by the one thing that can answer it.**
    ///
    /// `delivering` is what `job_wait` asks before it decides whether to block, so
    /// the two readings it must get right are: **true** while a watcher is live for
    /// that job — the promise is real and the wait would be waiting for something
    /// already in flight — and **false** everywhere else. The false cases are the
    /// whole of "R23 removes one case and only that one": a job this session never
    /// watched keeps blocking, and so does a job whose settlement has already been
    /// published.
    ///
    /// The settled case is the subtle one and is asserted separately from the
    /// never-watched one on purpose. Both are `false`, but for different reasons and
    /// with different consequences: for a settled job `job_wait`'s own "had already
    /// finished" branch answers, which is the honest sentence — *nothing was waited
    /// for* — while for an unwatched job the wait really does block, which is what
    /// the operator's "a deliberate block before a dependent step" depends on.
    #[test]
    fn delivering_is_true_only_while_a_watcher_is_live_for_that_job() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        let hub = Hub::new("s-jobs");
        let host = a_host();
        let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);

        let id = spawn_a_short_job(&host, "30");

        // Before anything watched it: not delivering, so a wait on it blocks.
        assert!(
            !watchers.delivering(&id.0),
            "a job nobody watches must not claim its answer is on the way"
        );

        watchers.watch(id.0.clone());
        assert!(
            watchers.delivering(&id.0),
            "a live watcher IS the promise: the settlement will be submitted to the \
             model whether it asks or not"
        );
        // Per job, not per session. The closure `job_wait` is given takes an id for
        // exactly this reason, and a second id must not ride on the first's watcher.
        assert!(
            !watchers.delivering("j-not-this-one"),
            "the promise is per job; another id must not inherit it"
        );

        // Let it settle: the watcher publishes and ends, so it is no longer
        // delivering — and the wait on it is answered by the caller's own
        // already-finished branch instead.
        host.kill_job(&id).expect("killing the job settles it");
        wait_for_settlement(&hub, &id.0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while watchers.delivering(&id.0) {
            assert!(
                Instant::now() < deadline,
                "the watcher must stop claiming delivery once it has delivered"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !watchers.delivering(&id.0),
            "a settled job is not being delivered any more; it HAS been"
        );
    }

    /// **The arming path, end to end: the sink is what makes `delivering` true.**
    ///
    /// The test above arms the watcher with a direct `watch()` call, which is not how
    /// a session arms it. A real session routes every tool result through
    /// [`JobWatchSink`], and *that* is the composition `harness.rs` depends on when it
    /// wires `job_wait`'s question to the same watcher set: the sink sees the
    /// `Backgrounded` outcome, arms a watcher, and the moment it does, the promise is
    /// real and `job_wait` must stop blocking.
    ///
    /// Asserted in both directions because the `false` half is the one that keeps R23
    /// from being larger than it is: a job no `Backgrounded` result has passed through
    /// is a job with no promise, and a wait on it blocks.
    ///
    /// **No race.** `watch()` inserts into `watching` synchronously, before it spawns
    /// the thread, so `delivering` is true by the time the sink's `emit` returns — which
    /// is before the turn that emitted it has ended, and therefore necessarily before
    /// the model's next call. A test on a clock would be testing the thread scheduler.
    #[test]
    fn the_sink_arms_the_watcher_and_that_is_what_makes_a_wait_decline() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        let hub = Hub::new("s-jobs");
        let host = a_host();
        let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);
        let mut sink = JobWatchSink::new(RecordingToolSink::default(), Some(watchers.clone()));

        let id = spawn_a_short_job(&host, "30");
        // Before any result has passed through the sink: no promise, so a wait blocks.
        assert!(
            !watchers.delivering(&id.0),
            "a job the sink has not seen must not claim its answer is on the way"
        );

        sink.emit(ToolEvent::Finished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: ToolOutcome::Backgrounded {
                handle: id.0.clone(),
                ran_for_ms: 0,
                how: letibot_transcript::Backgrounding::Asked,
                next: "job_wait".into(),
            },
            payload_digest: letibot_tools::payload_digest(""),
            inline_bytes: 0,
            full_bytes: 0,
            spill: None,
            repairs: 0,
            edit: None,
        });

        // Synchronously true, with no sleep: the promise exists from the emit.
        assert!(
            watchers.delivering(&id.0),
            "the sink is what arms the watcher, and arming it is what makes \
             `job_wait` decline to block"
        );

        let _ = host.kill_job(&id);
    }

    /// **R7: the settlement reaches the MODEL, not only the heads.**
    ///
    /// `JobSettled` has gone to the hub since protocol 14 and the jobs pane folds it,
    /// so every head knows. Nothing read it back into a turn, so the model's only route
    /// to a result was `job_wait` — which is the operator's complaint, and this is the
    /// hop that answers it. What is asserted is the two things the event cannot carry:
    /// that a completion is queued for the harness at all, and that it carries the
    /// **command** (read off the live view, which the durable event does not have and
    /// which is gone by the time a settlement is delivered).
    #[test]
    fn a_settled_job_is_queued_for_the_model_with_the_command_the_event_omits() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        let hub = Hub::new("s-jobs");
        let host = a_host();
        let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);

        let id = spawn_a_short_job(&host, "0.2");
        watchers.watch(id.0.clone());
        wait_for_settlement(&hub, &id.0);

        let done = watchers.take_completions();
        assert_eq!(done.len(), 1, "one job, one completion for the model");
        assert_eq!(done[0].job, id.0);
        assert_eq!(done[0].state, "exited 0");
        assert!(
            done[0].command.contains("sleep 0.2"),
            "the command must reach the model: {:?}",
            done[0].command
        );
        // **Taken, not left to arrive again.** The harness drains once per wake; a
        // second drain is empty, which is what stops a settlement being delivered as
        // two turns.
        assert!(
            watchers.take_completions().is_empty(),
            "a completion is taken once, not left for the next wake"
        );
    }

    /// **R7's floor rule, structurally: a completion never jumps a queued human line.**
    ///
    /// A job ending is not urgent in the operator's sense — nothing is waiting on it but
    /// the model — so it must not displace a person who pressed enter. That is not a
    /// check anywhere in this file; it is `Bell::next_any`'s order, where commands drain
    /// before wakes. Asserted here for the wake a job rings, in the shape the registry's
    /// own `a_wake_is_served_after_every_queued_command` asserts for a monitor's.
    #[test]
    fn a_jobs_completion_wakes_the_worker_with_no_command_behind_it_and_after_one_that_is() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

        let r = Registry::new();
        let hub = r
            .create("s-jobs", "", SessionWiring::default())
            .expect("the session registers");
        // `create` rings an open, and an open is drained first by design.
        assert!(matches!(r.next_work(), Some(Work::Open(_))));

        let host = a_host();
        // The watcher is given the daemon's own bell, which is what makes a settlement
        // between turns wake the worker rather than sit until something else asks.
        let watchers = JobWatchers::new(
            &(host.clone() as Arc<dyn ProcessHost>),
            &hub,
            Some(Arc::clone(r.bell())),
        );
        let id = spawn_a_short_job(&host, "0.2");
        watchers.watch(id.0.clone());
        wait_for_settlement(&hub, &id.0);

        // **The operator types while the job is ending.** Their line is in the hub's
        // queue before the wake is drained; it must come out of `next_work` first.
        let head = hub.attach("tui", "dead", Default::default(), 0);
        hub.submit(
            &head.head_id,
            "c1",
            0,
            letibot_sessionlog::CommandKind::Prompt {
                text: "answer me first".into(),
            },
        );
        match r.next_work() {
            Some(Work::Command(id, cmd)) => {
                assert_eq!(id, "s-jobs");
                assert!(matches!(
                    cmd.kind,
                    letibot_sessionlog::CommandKind::Prompt { .. }
                ));
            }
            // `Work` is not `Debug` — it carries a `QueuedCommand` — so the failure
            // names the expectation rather than the value.
            _ => panic!("the operator's line must be served before the job's completion"),
        }
        match r.next_work() {
            Some(Work::Woken(id)) => assert_eq!(id, "s-jobs", "the job's completion wakes it"),
            _ => panic!("the completion must arrive as a wake, after the operator's line"),
        }
    }
}
