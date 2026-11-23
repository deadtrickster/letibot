//! One worker per job. Lag costs a skipped check, never a delayed turn: one pending request per
//! job (a new trigger replaces the waiting one), a deadline per call, a position stamp that drops
//! a late verdict, and a breaker that pauses a job which keeps missing.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::Job;
use super::conversation::{Asked, Conversation, Question};
use super::feed::Log;
use super::judge::{Judge, Usage};
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
    /// The stateless data: what a fresh call is shown, and all a stateless job sends.
    pub shown: Shown,
    /// A persistent job's question at this trigger; without one the job asks statelessly.
    pub question: Option<Question>,
    reply: Option<mpsc::Sender<Outcome>>,
}

impl Request {
    pub fn new(position: u64, shown: Shown) -> Request {
        Request {
            position,
            shown,
            question: None,
            reply: None,
        }
    }

    pub fn asking(position: u64, shown: Shown, question: Question) -> Request {
        Request {
            question: Some(question),
            ..Request::new(position, shown)
        }
    }
}

/// What makes a job persistent: the rows every job is fed, and how often a question is also asked
/// of a fresh stateless call.
pub struct Persist {
    pub log: Arc<Mutex<Log>>,
    pub fresh_every: u32,
    /// History bytes before compaction.
    pub budget: usize,
    /// The turns the record kept before a restart, resumed at the first question.
    pub resume: Vec<super::resume::Kept>,
}

/// One call to make on the judge, off the worker thread.
type Call = Box<dyn FnOnce(&dyn Judge) -> Result<(String, Option<Usage>), String> + Send>;

/// A call's answer: `Err(())` is the deadline passing.
type Got = Result<Result<(String, Option<Usage>), String>, ()>;

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
    /// The permission job's second question, and where it came from: `Some` for that job alone.
    /// Which one is sent is the CALLER's answer (`Shown::oracle`), never a marker in the data.
    asking: Option<(String, String)>,
    slot: Mutex<Slot>,
    cv: Condvar,
    health: Mutex<Health>,
    inflight: Arc<AtomicBool>,
    on_outcome: OnOutcome,
    persist: Option<Persist>,
    convo: Mutex<Conversation>,
    asked: AtomicU64,
    /// The newest verdict's word and when it came, for the operator's view of the shadow.
    said: Mutex<Option<(Instant, &'static str)>>,
    /// The watched session's lines, one per job transition: the model stopped answering, and it
    /// answered again. Shared by every runner, drained by the harness at its next publish; each
    /// carries the warning code it is published under.
    notices: Arc<Mutex<Vec<(&'static str, String)>>>,
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
    /// A worker for `job`. `system` is the prompt (builtin or the project's), `source` says which,
    /// and `asking` is the second question a job may be asked (permission's alone).
    #[allow(clippy::too_many_arguments)] // each is a separate seam the tests replace
    pub fn start(
        job: Job,
        judge: Arc<dyn Judge>,
        recorder: Arc<dyn Recorder>,
        position: Arc<AtomicU64>,
        timing: Timing,
        system: String,
        source: String,
        asking: Option<(String, String)>,
        on_outcome: OnOutcome,
    ) -> JobRunner {
        Self::launch(
            job,
            judge,
            recorder,
            position,
            timing,
            system,
            source,
            asking,
            on_outcome,
            None,
            Arc::new(Mutex::new(Vec::new())),
        )
    }

    /// [`JobRunner::start`] whose notices go to `notices`, the queue a whole [`Shadow`] shares: one
    /// session line per job transition, drained by the harness at its next publish.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sharing(
        job: Job,
        judge: Arc<dyn Judge>,
        recorder: Arc<dyn Recorder>,
        position: Arc<AtomicU64>,
        timing: Timing,
        system: String,
        source: String,
        asking: Option<(String, String)>,
        on_outcome: OnOutcome,
        persist: Option<Persist>,
        notices: Arc<Mutex<Vec<(&'static str, String)>>>,
    ) -> JobRunner {
        Self::launch(
            job, judge, recorder, position, timing, system, source, asking, on_outcome, persist,
            notices,
        )
    }

    /// [`JobRunner::start`] as a running conversation fed from `persist.log`.
    #[allow(clippy::too_many_arguments)] // each is a separate seam the tests replace
    pub fn start_persistent(
        job: Job,
        judge: Arc<dyn Judge>,
        recorder: Arc<dyn Recorder>,
        position: Arc<AtomicU64>,
        timing: Timing,
        system: String,
        source: String,
        asking: Option<(String, String)>,
        on_outcome: OnOutcome,
        persist: Persist,
    ) -> JobRunner {
        Self::launch(
            job,
            judge,
            recorder,
            position,
            timing,
            system,
            source,
            asking,
            on_outcome,
            Some(persist),
            Arc::new(Mutex::new(Vec::new())),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn launch(
        job: Job,
        judge: Arc<dyn Judge>,
        recorder: Arc<dyn Recorder>,
        position: Arc<AtomicU64>,
        timing: Timing,
        system: String,
        source: String,
        asking: Option<(String, String)>,
        on_outcome: OnOutcome,
        persist: Option<Persist>,
        notices: Arc<Mutex<Vec<(&'static str, String)>>>,
    ) -> JobRunner {
        let budget = persist
            .as_ref()
            .map_or(super::conversation::BUDGET, |p| p.budget);
        let mut convo = Conversation::new(budget);
        if let Some(p) = &persist {
            convo.resume(p.resume.clone());
        }
        let inner = Arc::new(Inner {
            job,
            judge,
            recorder,
            position,
            timing,
            system,
            source,
            asking,
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
            persist,
            convo: Mutex::new(convo),
            asked: AtomicU64::new(0),
            said: Mutex::new(None),
            notices,
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

    /// The newest verdict's word, and when it came.
    pub fn said(&self) -> Option<(Instant, &'static str)> {
        *self.inner.said.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Inner {
    fn lock_slot(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_health(&self) -> std::sync::MutexGuard<'_, Health> {
        self.health.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The prompt this request is judged with, and where it came from. The permission job asks
    /// three questions and the CALLER says which one this is: a `Shown` marked `block` is
    /// automode's, a `Shown` marked `oracle` is an authorisation brief. Nothing in the data
    /// selects a prompt.
    ///
    /// The block question is the builtin and takes no project override: it is the harness's own
    /// policy, and a project that could reword it could reword what automode blocks for.
    fn system_for(&self, shown: &Shown) -> (&str, &str) {
        if shown.block {
            return (crate::shadow::prompts::PERMISSION_BLOCK, "builtin");
        }
        match &self.asking {
            Some((prompt, source)) if shown.oracle => (prompt, source),
            _ => (&self.system, &self.source),
        }
    }

    fn finish(&self, req: Request, outcome: Outcome) {
        if let Outcome::Verdict(v) = &outcome {
            *self.said.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), v.word()));
        }
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
            let fresh = self.sampled(&req).then(|| req.shown.clone());
            let outcome = self.exchange(&req, coalesced);
            self.breaker(probe, &outcome);
            self.publish_state();
            let position = req.position;
            self.finish(req, outcome.clone());
            // After the verdict acted: the comparison never delays what the session is told.
            if let Some(shown) = fresh {
                self.fresh_check(position, &shown, &outcome);
            }
        }
    }

    /// Whether this persistent question is one of the sampled ones a fresh call also answers.
    fn sampled(&self, req: &Request) -> bool {
        let Some(p) = &self.persist else {
            return false;
        };
        if req.question.is_none() || p.fresh_every == 0 {
            return false;
        }
        let n = self.asked.fetch_add(1, Ordering::SeqCst) + 1;
        n.is_multiple_of(u64::from(p.fresh_every))
    }

    /// One call under the deadline. A call still running from before makes this one a miss.
    fn call(&self, f: Call) -> Got {
        if self.inflight.swap(true, Ordering::SeqCst) {
            return Ok(Err(
                "the previous call is still running past its deadline".to_string()
            ));
        }
        let (tx, rx) = mpsc::channel();
        let (judge, busy) = (self.judge.clone(), self.inflight.clone());
        std::thread::spawn(move || {
            let r = f(&*judge);
            busy.store(false, Ordering::SeqCst);
            let _ = tx.send(r);
        });
        rx.recv_timeout(self.timing.deadline).map_err(|_| ())
    }

    /// One call and its record: the prompt sent, the reply, and every check's pass or reject.
    fn exchange(&self, req: &Request, coalesced: u32) -> Outcome {
        if let (Some(p), Some(q)) = (&self.persist, &req.question) {
            return self.exchange_persistent(req, q, p, coalesced);
        }
        let user = super::prompts::fence(&req.shown.data);
        let started = Instant::now();
        let (system, source) = self.system_for(&req.shown);
        let (system, source) = (system.to_string(), source.to_string());
        let (sys, u) = (system.clone(), user.clone());
        let got = self.call(Box::new(move |j| j.judge(&sys, &u).map(|r| (r, None))));
        let took = started.elapsed();
        // What was sent, so an empty reply is reported with the size that explains it.
        let mut shown = req.shown.clone();
        shown.prompt_bytes = self.system.len() + user.len();
        let (reply, checks, outcome) = self.settle(req.position, got, took, &shown);
        let body = format!(
            "model: {}\nprompt: {}\nposition: #{}\ncoalesced before it: {coalesced}\n\n\
             --- system\n{}\n--- user\n{user}\n--- reply\n{reply}\n--- checks\n{}\n--- outcome\n{}\n",
            self.judge.label(),
            source,
            req.position,
            system,
            checks.join("\n"),
            self.said(&outcome),
        );
        self.record(req.position, took, &outcome, body, None);
        outcome
    }

    /// The persistent turn: the new rows and the question on top of the running conversation.
    fn exchange_persistent(
        &self,
        req: &Request,
        q: &Question,
        p: &Persist,
        coalesced: u32,
    ) -> Outcome {
        let asked: Asked = {
            let log = p.log.lock().unwrap_or_else(|e| e.into_inner());
            self.convo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .ask(&log, q)
        };
        if let Some(note) = &asked.resumed {
            self.recorder
                .row(&format!("resume · {} · {note}", self.job.as_str()), "");
        }
        if let Some(note) = &asked.compacted {
            self.recorder
                .row(&format!("compacted · {} · {note}", self.job.as_str()), "");
        }
        let system = format!("{}{}", self.system, super::conversation::addendum(self.job));
        let started = Instant::now();
        let (sys, msgs) = (system.clone(), asked.messages.clone());
        let got = self.call(Box::new(move |j| j.converse(&sys, &msgs)));
        let took = started.elapsed();
        let mut shown = asked.shown.clone();
        shown.shortlist = req.shown.shortlist.clone();
        shown.call = req.shown.call.clone();
        shown.prompt_bytes =
            system.len() + asked.messages.iter().map(|m| m.text.len()).sum::<usize>();
        let usage = match &got {
            Ok(Ok((_, u))) => *u,
            _ => None,
        };
        let (reply, checks, outcome) = self.settle(req.position, got, took, &shown);
        let kept = (!matches!(outcome, Outcome::TimedOut | Outcome::Failed(_))).then(|| {
            self.convo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .commit(&asked, &reply)
        });
        let tokens = match usage {
            Some(u) => format!(
                "tokens: prompt {}, cached {}\n",
                u.prompt_tokens, u.cached_tokens
            ),
            None => String::new(),
        };
        // The system prompt is the same for every turn of one conversation: shown at its first.
        let sys = match asked.turn {
            1 => format!("--- system\n{system}\n"),
            n => format!(
                "--- system\nunchanged since turn 1 ({n} turns, {} messages)\n",
                asked.messages.len()
            ),
        };
        let body = format!(
            "model: {}\nprompt: {}\nmode: persistent, turn {}\nposition: #{}\ncoalesced before it: {coalesced}\n{tokens}\n\
             {sys}--- new rows and question\n{}\n--- reply\n{reply}\n--- checks\n{}\n--- outcome\n{}\n",
            self.judge.label(),
            self.source,
            asked.turn,
            req.position,
            asked.tail,
            checks.join("\n"),
            self.said(&outcome),
        );
        self.record(req.position, took, &outcome, body, kept.as_ref());
        outcome
    }

    /// The same question, asked of a fresh stateless call, and whether it agrees. Recorded, and
    /// never acted on; the breaker does not count it.
    fn fresh_check(&self, position: u64, shown: &Shown, persistent: &Outcome) {
        let user = super::prompts::fence(&shown.data);
        let (system, u) = (self.system_for(shown).0.to_string(), user.clone());
        let started = Instant::now();
        let got = self.call(Box::new(move |j| j.judge(&system, &u).map(|r| (r, None))));
        let took = started.elapsed();
        let mut shown = shown.clone();
        shown.prompt_bytes = self.system.len() + user.len();
        let shown = &shown;
        let (reply, checks, fresh) = match got {
            Ok(Ok((reply, _))) => {
                let c = verdict::check(self.job, &reply, shown);
                let o = match c.verdict {
                    Ok(v) => Outcome::Verdict(v),
                    Err(why) => Outcome::Void(why),
                };
                (reply, c.log, o)
            }
            Ok(Err(e)) => (String::new(), Vec::new(), Outcome::Failed(e)),
            Err(()) => (String::new(), Vec::new(), Outcome::TimedOut),
        };
        let (a, b) = (persistent.word(), fresh.word());
        let said = match agree(persistent, &fresh) {
            Some(true) => format!("agrees · {a}"),
            Some(false) => format!("DISAGREES · persistent {a}, fresh {b}"),
            None => format!("no comparison · persistent {a}, fresh {b}"),
        };
        self.recorder.row(
            &format!(
                "fresh check · {} · {said} · {:.1}s · at #{position}",
                self.job.as_str(),
                took.as_secs_f64()
            ),
            &format!(
                "persistent: {}\nfresh: {}\n\n--- user\n{user}\n--- reply\n{reply}\n--- checks\n{}\n",
                self.said(persistent),
                self.said(&fresh),
                checks.join("\n")
            ),
        );
    }

    /// A reply read against `shown`, and the health it leaves.
    fn settle(
        &self,
        position: u64,
        got: Got,
        took: Duration,
        shown: &Shown,
    ) -> (String, Vec<String>, Outcome) {
        match got {
            Ok(Ok((reply, _))) => {
                let checked = verdict::check(self.job, &reply, shown);
                let now = self.position.load(Ordering::SeqCst);
                let outcome = match checked.verdict {
                    Ok(_) if self.job.acts() && now > position => Outcome::Stale {
                        judged: position,
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
            Err(()) => {
                let mut h = self.lock_health();
                h.misses += 1;
                h.last = Some(took);
                (String::new(), Vec::new(), Outcome::TimedOut)
            }
        }
    }

    fn said(&self, outcome: &Outcome) -> String {
        match outcome {
            Outcome::Stale { judged, now } => {
                format!("dropped as stale: judged #{judged}, the session is at #{now}")
            }
            Outcome::Void(why) => format!("voided: {why}"),
            Outcome::TimedOut => format!("no answer inside {:?}", self.timing.deadline),
            Outcome::Failed(e) => format!("the call failed: {e}"),
            Outcome::Verdict(v) => format!("{v:?}"),
            other => other.word().to_string(),
        }
    }

    /// The exchange's row. The title is what `record::last_turn_end` reads after a restart.
    /// A kept turn's line goes last, after any watched text, so a resume can trust it.
    fn record(
        &self,
        position: u64,
        took: Duration,
        outcome: &Outcome,
        body: String,
        kept: Option<&super::resume::Kept>,
    ) {
        let title = format!(
            "{} · {} · {:.1}s · {} · at #{}",
            self.job.as_str(),
            outcome.word(),
            took.as_secs_f64(),
            self.judge.label(),
            position
        );
        // The promise, apart, for a reader that acts on the record later (the restart check).
        let mut body = body;
        if let Outcome::Verdict(Verdict::TurnEnd {
            reminder: verdict::Reminder::Owed { promise, .. },
            ..
        }) = outcome
        {
            body.push_str(&format!("--- promise\n{promise}\n"));
        }
        if let Some(k) = kept {
            body.push_str(&k.line());
        }
        self.recorder.row(&title, &body);
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
                // And the watched session hears that the shadow is looking again, once.
                self.say(
                    "shadow_answering",
                    format!(
                        "the shadow's `{job}` job is answering again: `{}` replied in {:.1}s.",
                        self.judge.label(),
                        took.as_secs_f64()
                    ),
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
            // Edge-triggered: the session hears that this job went blind once, not once per
            // miss. That job's own log row says the same thing in its own words.
            let blind = h.paused_until.is_none();
            h.paused_until = Some(Instant::now() + wait);
            h.misses = 0;
            h.latencies.clear();
            drop(h);
            self.paused_row(&why, wait);
            if blind {
                self.say(
                    "shadow_not_answering",
                    format!(
                        "the shadow's `{}` job got no answer from `{}` ({why}), so it is paused \
                         for {}s; meanwhile {}.",
                        self.job.as_str(),
                        self.judge.label(),
                        wait.as_secs(),
                        self.job.fallback()
                    ),
                );
            }
        }
    }

    /// One line for the watched session, which the harness publishes as a warning at its next
    /// publish: the shadow's own record is a log nobody reads while it is silent.
    fn say(&self, code: &'static str, line: String) {
        self.notices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((code, line));
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

/// Whether a persistent and a fresh outcome say the same: the same verdict word and, for during,
/// the same offers. `None` when either has no verdict to compare.
pub fn agree(a: &Outcome, b: &Outcome) -> Option<bool> {
    let (Outcome::Verdict(x), Outcome::Verdict(y)) = (a, b) else {
        return None;
    };
    let offers = |v: &Verdict| match v {
        Verdict::During { offers, .. } => {
            let mut n: Vec<String> = offers.iter().map(|o| o.name.clone()).collect();
            n.sort();
            n
        }
        _ => Vec::new(),
    };
    Some(x.word() == y.word() && offers(x) == offers(y))
}

/// The jobs that are on, for one watched session, and the position they are stamped against.
pub struct Shadow {
    runners: BTreeMap<Job, JobRunner>,
    position: Arc<AtomicU64>,
    /// The rows every persistent job is fed; `None` for a stateless shadow.
    log: Option<Arc<Mutex<Log>>>,
    /// The watched session's lines, shared by every runner: see [`Shadow::take_notices`].
    notices: Arc<Mutex<Vec<(&'static str, String)>>>,
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
        let notices: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let mut said = cfg.disclose(watched_model);
        let budget = super::conversation::BUDGET;
        let log = cfg
            .persistent()
            .then(|| Arc::new(Mutex::new(Log::new(2 * budget))));
        let bodies = match &log {
            Some(_) => recorder.bodies(),
            None => Vec::new(),
        };
        for job in cfg.enabled() {
            let model = cfg.model_for(job, watched_model).unwrap_or_default();
            let (system, source) = super::prompts::load(workspace, job);
            said.push(format!("{} prompt: {source}", job.as_str()));
            // The permission job's second question rides beside the first: one job, two prompts,
            // and the caller picks. Its own source line says which file it came from.
            let asking = super::prompts::builtin_asking(job).map(|_| {
                let (text, from) = super::prompts::load_asking(workspace, job);
                said.push(format!("{} asking prompt: {from}", job.as_str()));
                (text, from)
            });
            let (judge, timing, on) = (judge_for(job, &model), timing_for(job), on_outcome(job));
            let r = match &log {
                Some(log) => JobRunner::sharing(
                    job,
                    judge,
                    recorder.clone(),
                    position.clone(),
                    timing,
                    system,
                    source,
                    asking,
                    on,
                    Some(Persist {
                        log: log.clone(),
                        fresh_every: cfg.fresh_every(),
                        budget,
                        resume: super::resume::kept_of(bodies.iter().map(String::as_str), job),
                    }),
                    notices.clone(),
                ),
                None => JobRunner::sharing(
                    job,
                    judge,
                    recorder.clone(),
                    position.clone(),
                    timing,
                    system,
                    source,
                    asking,
                    on,
                    None,
                    notices.clone(),
                ),
            };
            runners.insert(job, r);
        }
        recorder.row(
            &format!("shadow started for a session on {watched_model}"),
            &said.join("\n"),
        );
        Some(Shadow {
            runners,
            position,
            log,
            notices,
        })
    }

    /// What a job said for the WATCHED session since the last drain, with the warning code each
    /// line is published under: one per transition into and out of blindness. Drained by the
    /// harness's publish - the shadow's own record is a log nobody reads while it is silent.
    pub fn take_notices(&self) -> Vec<(&'static str, String)> {
        std::mem::take(&mut *self.notices.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Whether the jobs are running conversations that need the watched rows fed.
    pub fn persistent(&self) -> bool {
        self.log.is_some()
    }

    /// Append the watched session's new rows, with their authors. A stateless shadow drops them.
    pub fn feed(&self, rows: Vec<super::feed::Row>) {
        if let Some(log) = &self.log {
            log.lock().unwrap_or_else(|e| e.into_inner()).push(rows);
        }
    }

    /// The operator's most recent own words in the watched session, as the feed holds them with
    /// their author. `None` for a stateless shadow, or one whose window holds none of them.
    ///
    /// The permission question is "did the operator ask for this?", and the gate's own trail can be
    /// empty of the operator's lines (a resumed session, a trail seeded from a store that kept only
    /// our side of it). The feed is the same rows the operator and the model see, so it is asked
    /// here rather than answering "(none)".
    pub fn operator_words(&self) -> Option<String> {
        let log = self.log.as_ref()?;
        let log = log.lock().unwrap_or_else(|e| e.into_inner());
        let mut last = None;
        for row in log
            .since(log.start())
            .filter(|r| r.author == super::words::Author::Operator)
        {
            last = Some(row.text.clone());
        }
        last
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

    /// Queue `job`'s check with the persistent question beside the stateless data. A stateless
    /// shadow asks the data alone.
    pub fn submit_asking(&self, job: Job, shown: Shown, question: Question) -> bool {
        let Some(r) = self.runners.get(&job) else {
            return false;
        };
        let req = match self.persistent() {
            true => Request::asking(self.position(), shown, question),
            false => Request::new(self.position(), shown),
        };
        r.submit(req);
        true
    }

    /// The bounded wait for `job`'s verdict. `None` when the job is off.
    pub fn ask(&self, job: Job, shown: Shown) -> Option<Outcome> {
        self.runners
            .get(&job)
            .map(|r| r.ask(Request::new(self.position(), shown)))
    }

    /// Which judge answers `job`, in its own words. `None` when the job is off.
    pub fn label(&self, job: Job) -> Option<String> {
        self.runners.get(&job).map(|r| r.inner.judge.label())
    }

    pub fn states(&self) -> Vec<(Job, JobState)> {
        self.runners.iter().map(|(j, r)| (*j, r.state())).collect()
    }

    /// The newest verdict of any job, as `job: word`.
    pub fn last_verdict(&self) -> Option<String> {
        self.runners
            .iter()
            .filter_map(|(j, r)| r.said().map(|(at, w)| (at, *j, w)))
            .max_by_key(|(at, _, _)| *at)
            .map(|(_, j, w)| format!("{}: {w}", j.as_str()))
    }
}
