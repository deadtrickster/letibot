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

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::SessionEvent;
use letibot_tools::exec::{JobId, JobState, ProcessHost, Waited};
use letibot_tools::{ToolEvent, ToolEventSink};
use letibot_transcript::ToolOutcome;

/// How long one blocking wait lasts before the thread re-checks its handles and
/// the stop flag. A job's settlement wakes the wait **immediately** — the condvar
/// under the job's state is what `wait_job` sleeps in — so this bounds only how
/// long a closing session's watcher can linger, never how late a settlement is.
const WATCH_CHUNK: Duration = Duration::from_secs(5);

/// The per-session set of watched background jobs.
///
/// Held by the harness, fed by [`JobWatchSink`] as backgrounded results pass
/// through it, and stopped when the session's backend closes.
pub struct JobWatchers {
    host: Weak<dyn ProcessHost>,
    hub: Weak<Hub>,
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
    pub fn new(host: &Arc<dyn ProcessHost>, hub: &Arc<Hub>) -> Arc<Self> {
        Arc::new(JobWatchers {
            host: Arc::downgrade(host),
            hub: Arc::downgrade(hub),
            stop: Arc::new(AtomicBool::new(false)),
            watching: Arc::new(Mutex::new(HashSet::new())),
            settled: Arc::new(Mutex::new(HashSet::new())),
        })
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
        let stop = Arc::clone(&self.stop);
        let watching = Arc::clone(&self.watching);
        let settled = Arc::clone(&self.settled);
        let _ = std::thread::Builder::new()
            .name(format!("job-watch-{}", &job[..job.len().min(20)]))
            .spawn(move || watch_one(host, hub, stop, watching, settled, job));
    }
}

fn watch_one(
    host: Weak<dyn ProcessHost>,
    hub: Weak<Hub>,
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
                hub.publish(SessionEvent::JobSettled {
                    job: job.clone(),
                    state: word,
                    produced,
                    elapsed_ms,
                });
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
        let hub = Hub::new("s-jobs");
        let host = a_host();
        let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub);
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
        let hub = Hub::new("s-jobs");
        let host = a_host();
        let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub);

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
}
