//! One worker per job. Lag costs a skipped check, never a delayed turn: one pending request per
//! job (a new trigger replaces the waiting one), a deadline per call, a position stamp that drops
//! a late verdict, and a breaker that pauses a job which keeps missing.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::Job;
use super::judge::Judge;
use super::record::Recorder;
use super::verdict::{self, Shown, Verdict};

/// The numbers a runner keeps to. [`Timing::of`] is the daemon's; tests shrink them.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub deadline: Duration,
    /// Median latency past which the job is paused.
    pub budget: Duration,
    /// Misses in a row that pause the job.
    pub misses: u32,
    /// Latencies the median is taken over.
    pub window: usize,
    /// First pause; each failed probe doubles it up to `backoff_max`.
    pub backoff: Duration,
    pub backoff_max: Duration,
}

impl Timing {
    pub fn of(job: Job) -> Timing {
        Timing {
            deadline: job.deadline(),
            budget: job.budget(),
            misses: 3,
            window: 5,
            backoff: Duration::from_secs(60),
            backoff_max: Duration::from_secs(15 * 60),
        }
    }
}

/// One trigger's request: what the shadow is shown, stamped with the transcript position it is about.
pub struct Request {
    pub position: u64,
    pub shown: Shown,
    reply: Option<mpsc::Sender<Outcome>>,
}

impl Request {
    pub fn new(position: u64, shown: Shown) -> Request {
        Request {
            position,
            shown,
            reply: None,
        }
    }
}

/// What became of a request. Only `Verdict` may change anything; every other arm is today's behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Verdict(Verdict),
    /// The reply failed a check and is voided whole.
    Void(String),
    /// The session moved past the position the verdict judged.
    Stale {
        judged: u64,
        now: u64,
    },
    TimedOut,
    Failed(String),
    /// The breaker has the job paused.
    Paused,
    /// A newer trigger replaced this one before it ran.
    Superseded,
}

impl Outcome {
    fn word(&self) -> &'static str {
        match self {
            Outcome::Verdict(v) => v.word(),
            Outcome::Void(_) => "void",
            Outcome::Stale { .. } => "stale",
            Outcome::TimedOut => "timeout",
            Outcome::Failed(_) => "failed",
            Outcome::Paused => "paused",
            Outcome::Superseded => "superseded",
        }
    }
}

/// What the Sessions group shows for a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    On,
    Lagging,
    Paused,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::On => "on",
            Mode::Lagging => "lagging",
            Mode::Paused => "paused",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobState {
    pub mode: Mode,
    pub last: Option<Duration>,
}

impl JobState {
    pub fn line(&self, job: Job) -> String {
        match self.last {
            Some(d) => format!(
                "{} {} {:.1}s",
                job.as_str(),
                self.mode.as_str(),
                d.as_secs_f64()
            ),
            None => format!("{} {}", job.as_str(), self.mode.as_str()),
        }
    }
}

struct Slot {
    pending: Option<Request>,
    coalesced: u32,
    stop: bool,
}

struct Health {
    misses: u32,
    latencies: VecDeque<Duration>,
    last: Option<Duration>,
    paused_until: Option<Instant>,
    backoff: Duration,
}

pub type OnOutcome = Box<dyn Fn(u64, &Outcome) + Send + Sync>;

struct Inner {
    job: Job,
    judge: Arc<dyn Judge>,
    recorder: Arc<dyn Recorder>,
    position: Arc<AtomicU64>,
    timing: Timing,
    system: String,
    source: String,
    slot: Mutex<Slot>,
    cv: Condvar,
    health: Mutex<Health>,
    inflight: Arc<AtomicBool>,
    on_outcome: OnOutcome,
}

pub struct JobRunner {
    inner: Arc<Inner>,
}

impl Drop for JobRunner {
    fn drop(&mut self) {
        // The worker ends at its next wait; a call in flight is left to its own transport deadline.
        self.inner.lock_slot().stop = true;
        self.inner.cv.notify_all();
    }
}

impl JobRunner {
    /// A worker for `job`. `system` is the prompt (builtin or the project's), `source` says which.
    #[allow(clippy::too_many_arguments)] // each is a separate seam the tests replace
    pub fn start(
        job: Job,
        judge: Arc<dyn Judge>,
        recorder: Arc<dyn Recorder>,
        position: Arc<AtomicU64>,
        timing: Timing,
        system: String,
        source: String,
        on_outcome: OnOutcome,
    ) -> JobRunner {
        let inner = Arc::new(Inner {
            job,
            judge,
            recorder,
            position,
            timing,
            system,
            source,
            slot: Mutex::new(Slot {
                pending: None,
                coalesced: 0,
                stop: false,
            }),
            cv: Condvar::new(),
            health: Mutex::new(Health {
                misses: 0,
                latencies: VecDeque::new(),
                last: None,
                paused_until: None,
                backoff: timing.backoff,
            }),
            inflight: Arc::new(AtomicBool::new(false)),
            on_outcome,
        });
        let worker = inner.clone();
        std::thread::Builder::new()
            .name(format!("shadow-{}", job.as_str()))
            .spawn(move || worker.run())
            .expect("a shadow worker thread");
        inner.publish_state();
        JobRunner { inner }
    }

    /// Queue a request without waiting. The one already waiting, if any, is superseded.
    pub fn submit(&self, req: Request) {
        let old = {
            let mut s = self.inner.lock_slot();
            let old = s.pending.replace(req);
            if old.is_some() {
                s.coalesced += 1;
            }
            old
        };
        self.inner.cv.notify_all();
        if let Some(old) = old {
            self.inner.finish(old, Outcome::Superseded);
        }
    }

    /// Queue a request and wait for it, never past the deadline: the permission job's one bounded
    /// wait. A timeout here is `TimedOut`, which the caller reads as ask.
    pub fn ask(&self, mut req: Request) -> Outcome {
        let (tx, rx) = mpsc::channel();
        req.reply = Some(tx);
        self.submit(req);
        rx.recv_timeout(self.inner.timing.deadline + Duration::from_millis(50))
            .unwrap_or(Outcome::TimedOut)
    }

    pub fn state(&self) -> JobState {
        self.inner.state()
    }
}

impl Inner {
    fn lock_slot(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_health(&self) -> std::sync::MutexGuard<'_, Health> {
        self.health.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn finish(&self, req: Request, outcome: Outcome) {
        (self.on_outcome)(req.position, &outcome);
        if let Some(tx) = req.reply {
            let _ = tx.send(outcome);
        }
    }

    fn state(&self) -> JobState {
        let h = self.lock_health();
        let mode = if h.paused_until.is_some() {
            Mode::Paused
        } else if h.misses > 0 || h.last.is_some_and(|d| d > self.timing.budget) {
            Mode::Lagging
        } else {
            Mode::On
        };
        JobState { mode, last: h.last }
    }

    fn publish_state(&self) {
        let line = self.state().line(self.job);
        self.recorder.state(self.job, &line);
    }

    fn median(h: &Health) -> Option<Duration> {
        let mut v: Vec<Duration> = h.latencies.iter().copied().collect();
        v.sort();
        v.get(v.len() / 2).copied()
    }

    fn run(self: Arc<Self>) {
        loop {
            let (req, coalesced) = {
                let mut s = self.lock_slot();
                while s.pending.is_none() && !s.stop {
                    s = self.cv.wait(s).unwrap_or_else(|e| e.into_inner());
                }
                if s.stop {
                    return;
                }
                let n = std::mem::take(&mut s.coalesced);
                (s.pending.take().expect("a pending request"), n)
            };
            let probe = {
                let h = self.lock_health();
                match h.paused_until {
                    Some(t) if Instant::now() < t => {
                        drop(h);
                        self.finish(req, Outcome::Paused);
                        continue;
                    }
                    Some(_) => true,
                    None => false,
                }
            };
            let outcome = self.exchange(&req, coalesced);
            self.breaker(probe, &outcome);
            self.publish_state();
            self.finish(req, outcome);
        }
    }

    /// One call and its record: the prompt sent, the reply, and every check's pass or reject.
    fn exchange(&self, req: &Request, coalesced: u32) -> Outcome {
        let user = super::prompts::fence(&req.shown.data);
        let started = Instant::now();
        let got = if self.inflight.swap(true, Ordering::SeqCst) {
            // The last call is still running past its deadline: this one is a miss, not a pile-up.
            Ok(Err(
                "the previous call is still running past its deadline".to_string()
            ))
        } else {
            let (tx, rx) = mpsc::channel();
            let (judge, system, u, busy) = (
                self.judge.clone(),
                self.system.clone(),
                user.clone(),
                self.inflight.clone(),
            );
            std::thread::spawn(move || {
                let r = judge.judge(&system, &u);
                busy.store(false, Ordering::SeqCst);
                let _ = tx.send(r);
            });
            rx.recv_timeout(self.timing.deadline)
        };
        let took = started.elapsed();
        let (reply, checks, outcome) = match got {
            Ok(Ok(reply)) => {
                let checked = verdict::check(self.job, &reply, &req.shown);
                let now = self.position.load(Ordering::SeqCst);
                let outcome = match checked.verdict {
                    Ok(_) if self.job.acts() && now > req.position => Outcome::Stale {
                        judged: req.position,
                        now,
                    },
                    Ok(v) => Outcome::Verdict(v),
                    Err(why) => Outcome::Void(why),
                };
                let mut h = self.lock_health();
                h.misses = 0;
                h.last = Some(took);
                h.latencies.push_back(took);
                while h.latencies.len() > self.timing.window {
                    h.latencies.pop_front();
                }
                (reply, checked.log, outcome)
            }
            Ok(Err(e)) => {
                self.lock_health().misses += 1;
                (String::new(), Vec::new(), Outcome::Failed(e))
            }
            Err(_) => {
                let mut h = self.lock_health();
                h.misses += 1;
                h.last = Some(took);
                (String::new(), Vec::new(), Outcome::TimedOut)
            }
        };
        let said = match &outcome {
            Outcome::Stale { judged, now } => {
                format!("dropped as stale: judged #{judged}, the session is at #{now}")
            }
            Outcome::Void(why) => format!("voided: {why}"),
            Outcome::TimedOut => format!("no answer inside {:?}", self.timing.deadline),
            Outcome::Failed(e) => format!("the call failed: {e}"),
            Outcome::Verdict(v) => format!("{v:?}"),
            other => other.word().to_string(),
        };
        let title = format!(
            "{} · {} · {:.1}s · {} · at #{}",
            self.job.as_str(),
            outcome.word(),
            took.as_secs_f64(),
            self.judge.label(),
            req.position
        );
        let body = format!(
            "model: {}\nprompt: {}\nposition: #{}\ncoalesced before it: {coalesced}\n\n\
             --- system\n{}\n--- user\n{user}\n--- reply\n{reply}\n--- checks\n{}\n--- outcome\n{said}\n",
            self.judge.label(),
            self.source,
            req.position,
            self.system,
            checks.join("\n"),
        );
        // The promise, apart, for a reader that acts on the record later (the restart check).
        let mut body = body;
        if let Outcome::Verdict(Verdict::TurnEnd {
            reminder: verdict::Reminder::Owed { promise, .. },
            ..
        }) = &outcome
        {
            body.push_str(&format!("--- promise\n{promise}\n"));
        }
        self.recorder.row(&title, &body);
        outcome
    }

    /// Pause after `misses` in a row or a median over budget; a probe after the backoff resumes
    /// the job or doubles the backoff. Each change is one visible row.
    fn breaker(&self, probe: bool, outcome: &Outcome) {
        let answered = !matches!(outcome, Outcome::TimedOut | Outcome::Failed(_));
        let mut h = self.lock_health();
        let job = self.job.as_str();
        if probe {
            if answered {
                h.paused_until = None;
                h.backoff = self.timing.backoff;
                h.latencies.clear();
                let took = h.last.unwrap_or_default();
                drop(h);
                self.recorder.row(
                    &format!(
                        "{job} · resumed · probe answered in {:.1}s",
                        took.as_secs_f64()
                    ),
                    "",
                );
            } else {
                h.backoff = (h.backoff * 2).min(self.timing.backoff_max);
                let wait = h.backoff;
                h.paused_until = Some(Instant::now() + wait);
                drop(h);
                self.paused_row("the probe missed too", wait);
            }
            return;
        }
        let why = if h.misses >= self.timing.misses {
            Some(format!("{} misses in a row", h.misses))
        } else if h.latencies.len() >= self.timing.window
            && Self::median(&h).is_some_and(|m| m > self.timing.budget)
        {
            Some(format!(
                "median latency {:.1}s over the {:.1}s budget",
                Self::median(&h).unwrap_or_default().as_secs_f64(),
                self.timing.budget.as_secs_f64()
            ))
        } else {
            None
        };
        if let Some(why) = why {
            let wait = h.backoff;
            h.paused_until = Some(Instant::now() + wait);
            h.misses = 0;
            h.latencies.clear();
            drop(h);
            self.paused_row(&why, wait);
        }
    }

    fn paused_row(&self, why: &str, wait: Duration) {
        self.recorder.row(
            &format!(
                "{} · paused · {why}; probing again in {}s; meanwhile {}",
                self.job.as_str(),
                wait.as_secs(),
                self.job.fallback()
            ),
            "",
        );
    }
}

/// The jobs that are on, for one watched session, and the position they are stamped against.
pub struct Shadow {
    runners: BTreeMap<Job, JobRunner>,
    position: Arc<AtomicU64>,
}

impl Shadow {
    /// Runners for every job `cfg` turns on, or `None` when none is: off by default costs nothing.
    pub fn start(
        cfg: &super::config::ShadowConfig,
        watched_model: &str,
        workspace: Option<&std::path::Path>,
        recorder: Arc<dyn Recorder>,
        judge_for: &dyn Fn(Job, &str) -> Arc<dyn Judge>,
        timing_for: &dyn Fn(Job) -> Timing,
        on_outcome: &dyn Fn(Job) -> OnOutcome,
    ) -> Option<Shadow> {
        if !cfg.any_on() {
            return None;
        }
        let position = Arc::new(AtomicU64::new(0));
        let mut runners = BTreeMap::new();
        let mut said = cfg.disclose(watched_model);
        for job in cfg.enabled() {
            let model = cfg.model_for(job, watched_model).unwrap_or_default();
            let (system, source) = super::prompts::load(workspace, job);
            said.push(format!("{} prompt: {source}", job.as_str()));
            runners.insert(
                job,
                JobRunner::start(
                    job,
                    judge_for(job, &model),
                    recorder.clone(),
                    position.clone(),
                    timing_for(job),
                    system,
                    source,
                    on_outcome(job),
                ),
            );
        }
        recorder.row(
            &format!("shadow started for a session on {watched_model}"),
            &said.join("\n"),
        );
        Some(Shadow { runners, position })
    }

    /// The watched transcript grew to `n` rows: a verdict stamped earlier is now stale.
    pub fn advance(&self, n: u64) {
        self.position.fetch_max(n, Ordering::SeqCst);
    }

    pub fn position(&self) -> u64 {
        self.position.load(Ordering::SeqCst)
    }

    pub fn is_on(&self, job: Job) -> bool {
        self.runners.contains_key(&job)
    }

    /// Queue `job`'s check; `false` when the job is off.
    pub fn submit(&self, job: Job, shown: Shown) -> bool {
        match self.runners.get(&job) {
            Some(r) => {
                r.submit(Request::new(self.position(), shown));
                true
            }
            None => false,
        }
    }

    /// The bounded wait for `job`'s verdict. `None` when the job is off.
    pub fn ask(&self, job: Job, shown: Shown) -> Option<Outcome> {
        self.runners
            .get(&job)
            .map(|r| r.ask(Request::new(self.position(), shown)))
    }

    pub fn states(&self) -> Vec<(Job, JobState)> {
        self.runners.iter().map(|(j, r)| (*j, r.state())).collect()
    }
}
