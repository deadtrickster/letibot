//! **When a finished job or subagent is told to the model** — R79.
//!
//! The operator: *"can job and agent results come mid turn? i think you guys are getting
//! distracted when this happens"*. They did: a child heard every settlement at its next round
//! boundary, and a root heard each one as a WAKE — a turn of its own — so a model working on X
//! ended its turn and was handed *"job j975 ended"* as the next thing to do. Nothing asked
//! whether the model was waiting for the result.
//!
//! # The operator's decision, 2026-10-11
//!
//! **Queue and batch by default; immediate only for what the work actually waits on.** In this
//! file that is one pure function, [`decide`], over the [`Facts`] of one completion:
//!
//! | the fact | delivery |
//! |---|---|
//! | an emergency (R76's red target) | immediate — [`Why::Emergency`] |
//! | a monitor firing | immediate — [`Why::Monitor`] |
//! | it FAILED, and the plan depends on it (any of the four below but `wake: now`) | immediate — [`Why::DependencyFailed`] |
//! | an open todo row's `when: job` names it | immediate — [`Why::WhenJob`] |
//! | an open todo row `needs` it (job or child) | immediate — [`Why::Needed`] |
//! | a wait asked for it: `job_wait` / `task_result` on it, or the call it ran in was promoted to the background | immediate — [`Why::Awaited`] |
//! | it was started with `wake: "now"` | immediate — [`Why::WakeNow`] |
//! | anything else | **held** |
//!
//! The operator's own messages are not a completion and do not come through here at all: they
//! are the hub's commands, drained first by `HubSteering::try_next` mid-turn and first by the
//! bell between turns, exactly as before.
//!
//! # Where held results go
//!
//! **Holding exists only so a RUNNING turn is not interrupted.** The operator, 2026-10-11, after
//! pg-noop showed a held result reaching an idle model only when they next spoke, under a heading
//! that said *"while you were busy"*: *"jobs were ran for a reason and held for a reason. once turn
//! ends no reason to hold"*. So:
//!
//! * **mid-turn**, held results accumulate per session ([`JobWatchers::hold`]) and nothing is said
//!   at the round boundary (the starvation cap below aside);
//! * **when the turn ends** with anything held, the session's own wake is rung
//!   ([`JobWatchers::turn_ended`]) and `Harness::wake` delivers the held set as **one**
//!   `[meanwhile]` note, a turn of its own, recorded as [`Rule::TurnEnd`] — the same door an
//!   immediate completion between turns takes;
//! * **if another turn starts first** — the operator's prompt (commands drain before wakes), a
//!   parent's message, a plan-check, a continuation after the wall — the batch rides that turn's
//!   start as before ([`Rule::Batch`]; as a harness row just BEFORE the prompt, so the operator's
//!   words stay the last thing read), and the wake that follows finds nothing;
//! * **a result that arrives while the session is idle is not held at all**: the wake its arrival
//!   rang delivers it at once as an ordinary completion, with no `[meanwhile]` heading, recorded
//!   as [`Rule::Idle`].
//!
//! The line between the last two is one fact, *did it arrive before the session's last turn
//! ended* ([`JobWatchers::last_turn_ended`]); see `Deliveries::between_turns`. Every settlement is
//! still published to the hub as it happens (`JobSettled`, the subagent row), so the head's jobs
//! pane is never behind.
//!
//! **Children** follow the same rules at their round boundaries and between their turns. A child
//! that stops with results still held hands them up a level ([`JobWatchers::stop`]), where the
//! parent's rules decide them — as the queue always was.
//!
//! # The starvation cap
//!
//! Turns here run for hours. A turn running with a result held for longer than
//! [`HELD_RESULTS_CAP`] (`Config::held_results_cap`) gets the batch at its next round boundary
//! anyway, as one note. Measured from the longest-held result's ARRIVAL rather than from the
//! turn's start: a turn's age would release every result the moment it arrived once the turn
//! was ten minutes old, which is per-completion delivery again with a delay in front.
//!
//! # The record
//!
//! Every delivery — immediate, batched, delivered at a turn's end, idle, or released by the cap,
//! and every monitor firing — is one
//! JSON line in [`LOG`] (`<workspace>/.letibot/delivery-log.jsonl`, the `notes-offers.jsonl`
//! shape): session, handle, kind, when it arrived, when it was delivered, the rule, and how many
//! were delivered together. Deliberately small and self-contained, so a general metrics store can
//! fold it in by reading the lines; see [`DeliveryLog`].

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use letibot_tokencore::store::{TodoCondition, TodoItem, TodoNeed, TodoStatus};
use letibot_tools::runtime::WantedNow;

use crate::jobwatch::{BackgroundKind, JobCompletion, JobWatchers};

/// **How long a running turn may keep a result held** before the batch is released at its next
/// round boundary — ten minutes, the operator's number (*"a turn running longer than N minutes
/// (10, named, configurable)"*). The default of `Config::held_results_cap`, which is what the
/// harness reads.
pub const HELD_RESULTS_CAP: Duration = Duration::from_secs(10 * 60);

/// Where the delivery record is appended, under the session's workspace.
pub const LOG: &str = ".letibot/delivery-log.jsonl";

/// What finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultKind {
    /// A background job (`bash` background, or a foreground call promoted).
    Job,
    /// A subagent (`task`).
    Child,
    /// A monitor firing.
    Monitor,
}

impl ResultKind {
    pub fn word(self) -> &'static str {
        match self {
            ResultKind::Job => "job",
            ResultKind::Child => "child",
            ResultKind::Monitor => "monitor",
        }
    }

    fn of(c: &JobCompletion) -> ResultKind {
        match c.kind {
            BackgroundKind::Job => ResultKind::Job,
            BackgroundKind::Subagent => ResultKind::Child,
        }
    }
}

/// **Why a result is delivered at once** — the rule that applied, recorded with the delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// An open todo row's `when: job` names it.
    WhenJob,
    /// An open todo row `needs` it.
    Needed,
    /// A wait asked for it (see [`WantedNow::Awaited`]).
    Awaited,
    /// It was started with `wake: "now"`.
    WakeNow,
    /// It failed, and the plan depends on it.
    DependencyFailed,
    /// A monitor fired.
    Monitor,
    /// A target emergency (R76).
    Emergency,
}

impl Why {
    pub fn word(self) -> &'static str {
        match self {
            Why::WhenJob => "when_job",
            Why::Needed => "needed",
            Why::Awaited => "awaited",
            Why::WakeNow => "wake_now",
            Why::DependencyFailed => "dependency_failed",
            Why::Monitor => "monitor",
            Why::Emergency => "emergency",
        }
    }
}

/// The answer [`decide`] gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Deliver at the next round boundary, or as a turn of its own between turns.
    Immediate(Why),
    /// Hold while a turn runs: delivered when it ends (or at the cap, or with the next turn start).
    Held,
}

/// **Everything [`decide`] reads about one result and the session it belongs to** — gathered by
/// [`facts_of`] from the completion, the todo board and the session's wants, so the decision
/// itself reads no state and is asserted without a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    pub kind: ResultKind,
    /// A non-zero exit, a signal, a kill, a job that never ran, a child that failed.
    pub failed: bool,
    /// A target emergency rather than a completion.
    pub emergency: bool,
    /// An open row's `when: job` names this handle.
    pub when_row: bool,
    /// An open row `needs` this job or child.
    pub needed: bool,
    /// What the session said it wants — see [`WantedNow`].
    pub wanted: Option<WantedNow>,
}

impl Facts {
    /// A result with nothing known about it: a routine job that succeeded.
    pub fn routine(kind: ResultKind) -> Facts {
        Facts {
            kind,
            failed: false,
            emergency: false,
            when_row: false,
            needed: false,
            wanted: None,
        }
    }
}

/// **The delivery decision** — the operator's five points, as a function. See the module header
/// for the table it implements; the order of the arms is the order the reasons are recorded in
/// when more than one applies, most specific first.
pub fn decide(f: &Facts) -> Delivery {
    if f.emergency {
        return Delivery::Immediate(Why::Emergency);
    }
    if f.kind == ResultKind::Monitor {
        return Delivery::Immediate(Why::Monitor);
    }
    // **What the plan depends on**: a row's condition, a row's need, or a wait on it. `wake:
    // "now"` is not a dependency — it is the caller asking for timing — so a failure under it is
    // recorded as `wake_now`, not as a broken dependency.
    let depends = f.when_row || f.needed || f.wanted == Some(WantedNow::Awaited);
    if f.failed && depends {
        return Delivery::Immediate(Why::DependencyFailed);
    }
    if f.when_row {
        return Delivery::Immediate(Why::WhenJob);
    }
    if f.needed {
        return Delivery::Immediate(Why::Needed);
    }
    match f.wanted {
        Some(WantedNow::Awaited) => Delivery::Immediate(Why::Awaited),
        Some(WantedNow::WakeNow) => Delivery::Immediate(Why::WakeNow),
        None => Delivery::Held,
    }
}

/// **Did this end badly?** A job that did not exit 0, or a child that answered `failed`.
pub fn failed(c: &JobCompletion) -> bool {
    match c.kind {
        BackgroundKind::Job => c.state != "exited 0",
        BackgroundKind::Subagent => c.state == "failed",
    }
}

/// Whether a row's handle names this completion — by the id, or by the name the caller gave it
/// (`TodoNeed::Job`'s handle is whatever the model wrote, and `job_output` takes either).
fn names(handle: &str, c: &JobCompletion) -> bool {
    handle == c.job || c.slug.as_deref() == Some(handle)
}

/// A row the plan is still working towards: one somebody has not finished or put off.
fn open(row: &TodoItem) -> bool {
    matches!(row.status, TodoStatus::Pending | TodoStatus::InProgress)
}

/// **The facts of one completion against this session's board.** `wanted` is the session's own
/// record ([`JobWatchers::wanted`]); a completion handed up from a stopped child is decided by the
/// same reading, and is normally held — nothing above it waits on a grandchild's job.
pub fn facts_of(c: &JobCompletion, rows: &[TodoItem], wanted: Option<WantedNow>) -> Facts {
    let mut f = Facts::routine(ResultKind::of(c));
    f.failed = failed(c);
    f.wanted = wanted;
    for row in rows.iter().filter(|r| open(r)) {
        if let Some(TodoCondition::Job { handle }) = &row.when
            && names(handle, c)
        {
            f.when_row = true;
        }
        if row.needs.iter().any(|n| match n {
            TodoNeed::Job { handle } => names(handle, c),
            TodoNeed::Child { id } => c.kind == BackgroundKind::Subagent && *id == c.job,
            _ => false,
        }) {
            f.needed = true;
        }
    }
    f
}

/// **The sort, at a boundary**: everything held plus everything newly queued, each decided again
/// against the board as it is NOW — a row added since a result was held, or a `job_wait` on it,
/// promotes it — with the immediates returned and the rest held again.
///
/// `skip` drops a completion that is nobody's news (a gatekeeper's, whose verdict is already on
/// the queue's row — see `Harness::wake`).
pub fn sort(
    w: &JobWatchers,
    rows: &[TodoItem],
    skip: impl Fn(&JobCompletion) -> bool,
) -> Vec<(JobCompletion, Why)> {
    let mut all = w.take_held();
    all.extend(w.take_completions());
    let mut now = Vec::new();
    let mut keep = Vec::new();
    for c in all {
        if skip(&c) {
            continue;
        }
        match decide(&facts_of(&c, rows, w.wanted(&c.job))) {
            Delivery::Immediate(why) => {
                w.delivered(&c.job);
                now.push((c, why));
            }
            Delivery::Held => keep.push(c),
        }
    }
    w.hold(keep);
    now
}

/// **Has the longest-held result waited past the cap?**
pub fn starved(w: &JobWatchers, cap: Duration, now: SystemTime) -> bool {
    w.oldest_held()
        .is_some_and(|t| now.duration_since(t).unwrap_or(Duration::ZERO) >= cap)
}

/// **How a delivery happened**, as the record names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Immediate(Why),
    /// Held, and handed over with the next turn the model took anyway.
    Batch,
    /// Held, and released mid-turn by the starvation cap.
    Starvation,
    /// **Held while a turn ran, and delivered the moment that turn ENDED, as a turn of its own**
    /// (the operator, 2026-10-11: *"jobs were ran for a reason and held for a reason. once turn
    /// ends no reason to hold"*). Distinct from [`Rule::Batch`], which is the same batch riding a
    /// turn that started for another reason before the turn-end wake was served.
    TurnEnd,
    /// **Arrived while the session was idle, so it was never held** — delivered at once, as an
    /// ordinary completion with no `[meanwhile]` heading (same decision).
    Idle,
}

impl Rule {
    pub fn word(self) -> String {
        match self {
            Rule::Immediate(why) => format!("immediate:{}", why.word()),
            Rule::Batch => "batch".into(),
            Rule::Starvation => "starvation".into(),
            Rule::TurnEnd => "turn_end".into(),
            Rule::Idle => "idle".into(),
        }
    }
}

/// **The `[meanwhile]` note's first line** — what the model is told about why these results
/// arrive together and late. The `[job]` / `[task]` groups under it are `completion_notices`'
/// own, unchanged, so the head's compact job rows read them as they always did.
///
/// [`Rule::TurnEnd`] says what is true of a batch delivered the moment a turn ended: the results
/// came in while the model was working and were held so it could finish. [`Rule::Idle`] has no
/// heading in practice — an idle arrival is an ordinary completion — and is worded as the batch
/// only so the function stays total.
pub fn meanwhile_heading(n: usize, rule: Rule, cap: Duration) -> String {
    let what = if n == 1 {
        "1 result".to_string()
    } else {
        format!("{n} results")
    };
    let (it, they) = if n == 1 {
        ("it", "it was")
    } else {
        ("them", "they were")
    };
    match rule {
        Rule::TurnEnd => format!(
            "[meanwhile] {what} arrived while you were working — nothing in your plan was waiting \
             on {it}, so {they} held until your turn ended instead of interrupting you."
        ),
        Rule::Starvation => format!(
            "[meanwhile] {what} held for over {} while this turn ran — nothing in your plan waits \
             on them; here they are so they do not wait for ever. Carry on with what you were \
             doing unless one of them changes it.",
            minutes(cap)
        ),
        _ => format!(
            "[meanwhile] {what} arrived while you were busy — nothing in your plan was waiting on \
             them, so they were held for this break instead of interrupting you."
        ),
    }
}

/// The cap in the words the note reads with: `10 min`, `90s`, `300ms`.
fn minutes(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 && s % 60 == 0 {
        format!("{} min", s / 60)
    } else if s > 0 {
        format!("{s}s")
    } else {
        format!("{}ms", d.as_millis())
    }
}

/// **The append-only delivery record** — one JSON line per delivered result, under the
/// workspace ([`LOG`]).
///
/// ```text
/// {"session":"s-1","handle":"j12","kind":"job","arrived_at_ms":…,"delivered_at_ms":…,
///  "waited_ms":…,"rule":"batch","batch":3}
/// ```
///
/// `batch` is how many results reached the model in the same note. A write that fails is
/// dropped, as `notes-offers.jsonl`'s is: the record is measurement, and a delivery must never
/// fail because its measurement could not be written. `None` path: a harness with no workspace
/// to write under records nothing.
#[derive(Debug, Clone)]
pub struct DeliveryLog {
    path: Option<PathBuf>,
    session: String,
}

/// One delivery, as recorded.
pub struct Delivered<'a> {
    pub handle: &'a str,
    pub kind: ResultKind,
    pub arrived: SystemTime,
    pub delivered: SystemTime,
    pub rule: Rule,
    pub batch: usize,
}

fn ms(t: SystemTime) -> u128 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

impl DeliveryLog {
    /// The record for one session, under its workspace.
    pub fn under(workspace: &Path, session: &str) -> DeliveryLog {
        DeliveryLog {
            path: Some(workspace.join(LOG)),
            session: session.to_string(),
        }
    }

    /// A record that writes nothing.
    pub fn none() -> DeliveryLog {
        DeliveryLog {
            path: None,
            session: String::new(),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Append one line per delivery.
    pub fn record(&self, rows: &[Delivered<'_>]) {
        // The metrics row for each (TODO R81) — here, R79's one record site, so every rule and
        // door reaches `metrics.db` whether or not this record has a file to write.
        crate::instruments::delivered_all(&self.session, rows);
        let Some(path) = &self.path else {
            return;
        };
        if rows.is_empty() {
            return;
        }
        let mut text = String::new();
        for d in rows {
            let line = serde_json::json!({
                "session": self.session,
                "handle": d.handle,
                "kind": d.kind.word(),
                "arrived_at_ms": ms(d.arrived) as u64,
                "delivered_at_ms": ms(d.delivered) as u64,
                "waited_ms": d.delivered.duration_since(d.arrived).unwrap_or_default().as_millis() as u64,
                "rule": d.rule.word(),
                "batch": d.batch,
            });
            text.push_str(&line.to_string());
            text.push('\n');
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = f.write_all(text.as_bytes());
        }
    }

    /// Record a set of completions delivered together under one rule.
    pub fn completions(&self, done: &[JobCompletion], rule: Rule, at: SystemTime) {
        let rows: Vec<Delivered<'_>> = done
            .iter()
            .map(|c| Delivered {
                handle: &c.job,
                kind: ResultKind::of(c),
                arrived: c.arrived,
                delivered: at,
                rule,
                batch: done.len(),
            })
            .collect();
        self.record(&rows);
    }

    /// Record immediates delivered together, each under its own reason.
    pub fn immediates(&self, done: &[(JobCompletion, Why)], at: SystemTime) {
        let rows: Vec<Delivered<'_>> = done
            .iter()
            .map(|(c, why)| Delivered {
                handle: &c.job,
                kind: ResultKind::of(c),
                arrived: c.arrived,
                delivered: at,
                rule: Rule::Immediate(*why),
                batch: done.len(),
            })
            .collect();
        self.record(&rows);
    }

    /// Record monitor firings delivered together.
    pub fn firings(&self, fired: &[letibot_tools::exec::monitor::Firing], at: SystemTime) {
        let rows: Vec<Delivered<'_>> = fired
            .iter()
            .map(|f| Delivered {
                handle: &f.name,
                kind: ResultKind::Monitor,
                arrived: f.at,
                delivered: at,
                rule: Rule::Immediate(Why::Monitor),
                batch: fired.len(),
            })
            .collect();
        self.record(&rows);
    }
}

#[cfg(test)]
mod tests;
