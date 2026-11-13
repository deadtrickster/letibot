//! When a finished job or subagent is told to the model: [`decide`], over the [`Facts`] of
//! one completion. A running turn is not interrupted, so a result is held by default. It is
//! delivered at once only for what the work actually waits on

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use letibot_tokencore::store::{TodoCondition, TodoItem, TodoNeed, TodoStatus};
use letibot_tools::runtime::WantedNow;

use crate::jobwatch::{BackgroundKind, JobCompletion, JobWatchers};

/// How long a running turn may keep a result held before the batch is released at its next round
/// boundary. The default of `Config::held_results_cap`, which is what the harness reads.
pub const HELD_RESULTS_CAP: Duration = Duration::from_secs(10 * 60);

/// The delivery record's file, inside the session's logs directory (`Config::daemon_logs`).
///
/// Never the workspace: a `.letibot/` there would make every child's worktree look dirty.
pub const LOG: &str = "delivery-log.jsonl";

/// What finished. The three come out of the watcher set and are decided by [`decide`]; a room line
/// is not one of them - the room's own ring decides it, and it only rides the same queue
/// (`Held::Room`) so one door can put everything in arrival order.
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

/// A room line a session is owed, as the room's reader composed it: the sentence, the room and
/// chat `seq` behind it (`0` for a queue event or a gap), when it was queued, whether it only
/// passed by, and whether anything rang for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomLine {
    pub seq: u64,
    /// Every chat `seq` this line stands for. Empty for a line that is one message - `seq` is the
    /// whole of it - and the folded ones for a digest, so the door's facts and the wake's report
    /// count the lines the digest took the place of rather than the one line it is.
    pub stands_for: Vec<u64>,
    /// The room the message was said in (`host`, `project/<key>`, `dm`), empty for a queue event
    /// or a note the daemon pushed itself. With `seq` it is what a line IS: a message queued
    /// twice - once by the push and once by a reconnect's replay - is one line.
    pub room: String,
    pub line: String,
    pub arrived: SystemTime,
    /// Only passing-by talk: a turn carrying nothing but these may run cheap.
    pub passing: bool,
    /// Nothing rang for this line, so it is held until a quiet window (`HELD_ROOM_QUIET`) passes
    /// and it is said as one digest. A line that rang a bell is not held: a turn is already coming
    /// for it.
    pub held: bool,
}

impl RoomLine {
    /// The chat messages this line carries: its own `seq`, or every `seq` of a digest.
    pub fn seqs(&self) -> Vec<u64> {
        match self.stands_for.is_empty() {
            true => match self.seq {
                0 => Vec::new(),
                seq => vec![seq],
            },
            false => self.stands_for.clone(),
        }
    }

    /// What makes two lines the same message - `(room, seq)` - or `None` for a line that stands for
    /// no single message at all (a queue event, a folded digest).
    pub fn identity(&self) -> Option<(&str, u64)> {
        match (self.seq, self.stands_for.is_empty()) {
            (0, _) | (_, false) => None,
            (seq, true) => Some((self.room.as_str(), seq)),
        }
    }
}

/// One thing a session is owed, whatever produced it - the same queue for a finished job, a child,
/// a monitor and a room line, so one door says them in the order they happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Held {
    Result(JobCompletion),
    Room(RoomLine),
}

impl Held {
    /// When it reached the session - what the door sorts by.
    pub fn arrived(&self) -> SystemTime {
        match self {
            Held::Result(c) => c.arrived,
            Held::Room(r) => r.arrived,
        }
    }

    pub fn room(&self) -> Option<&RoomLine> {
        match self {
            Held::Room(r) => Some(r),
            Held::Result(_) => None,
        }
    }
}

/// Everything a session is owed, in arrival order: the room's lines keep the order the room's
/// reader queued them in among themselves (which is `seq` order), and a result takes its own place
/// by when it settled. The sort is stable, so two things held at the same instant keep the order
/// they were held in.
pub fn in_arrival_order(room: Vec<RoomLine>, results: Vec<JobCompletion>) -> Vec<Held> {
    let mut all: Vec<Held> = results.into_iter().map(Held::Result).collect();
    all.extend(room.into_iter().map(Held::Room));
    arrivals(all, Held::arrived)
}

/// Sort anything by when it arrived, keeping the order it was given in when two arrived at the same
/// instant - the one shape every door's batch is built with.
pub fn arrivals<T>(items: Vec<T>, at: impl Fn(&T) -> SystemTime) -> Vec<T> {
    let mut items = items;
    items.sort_by_key(|i| at(i));
    items
}

/// One line per message. A room line can be queued twice - the live push and a reconnect's replay,
/// a mention and the room it was said in - and the model reads a message once however many paths
/// carried it. The pen drops the second copy at the push; this is the door's own guard, so a batch
/// built from two pens cannot say a message twice either.
pub fn once_each(lines: Vec<RoomLine>) -> Vec<RoomLine> {
    let mut seen: Vec<(String, u64)> = Vec::new();
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        match line.identity() {
            Some((room, seq)) => {
                let key = (room.to_string(), seq);
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                out.push(line);
            }
            None => out.push(line),
        }
    }
    out
}

/// Why a result is delivered at once: the rule that applied, recorded with the delivery.
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
    /// A target emergency.
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

/// Everything [`decide`] reads about one result and the session it belongs to. Gathered by
/// [`facts_of`] from the completion, the todo board and the session's wants, so the
/// decision reads no state and can be asserted without a session.
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
    /// What the session said it wants - see [`WantedNow`].
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

/// The delivery decision, as a function. See the module header for the rules it implements.
/// The order of the arms is the order the reasons are recorded in when more than one
/// applies most specific first.
pub fn decide(f: &Facts) -> Delivery {
    if f.emergency {
        return Delivery::Immediate(Why::Emergency);
    }
    if f.kind == ResultKind::Monitor {
        return Delivery::Immediate(Why::Monitor);
    }
    // What the plan depends on: a row's condition, a row's need, or a wait on it. `wake: "now"`
    // is not a dependency but the caller asking for timing, so a failure under it is `wake_now`.
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
        // `wake: "later"` is a declaration about the WAIT, not about the delivery: the model said
        // it does not need waking for it, so the result is held and batched like any other.
        Some(WantedNow::WakeLater) | None => Delivery::Held,
    }
}

/// Did this end badly? A job that did not exit 0, or a child that answered `failed`.
pub fn failed(c: &JobCompletion) -> bool {
    match c.kind {
        BackgroundKind::Job => c.state != "exited 0",
        BackgroundKind::Subagent => c.state == "failed",
    }
}

/// The one spelling of "a row names this job", for its two callers: the delivery decision
/// here and the activity ([`crate::activity::awaited`]). The slug is part of it because a
/// model may name its job (`bash(slug
pub fn named(handle: &str, job: &str, slug: Option<&str>) -> bool {
    handle == job || slug == Some(handle)
}

/// A row the plan is still working towards: one somebody has not finished or put off.
fn open(row: &TodoItem) -> bool {
    matches!(row.status, TodoStatus::Pending | TodoStatus::InProgress)
}

/// What the session's open plan says about one running thing: the two facts the activity
/// and the delivery decision both need, from one reading of the board. `when_row` is a row
/// whose `when
pub fn plan_says(
    rows: &[TodoItem],
    handle: &str,
    slug: Option<&str>,
    kind: BackgroundKind,
) -> (bool, bool) {
    let (mut when_row, mut needed) = (false, false);
    for row in rows.iter().filter(|r| open(r)) {
        if let Some(TodoCondition::Job { handle: h }) = &row.when
            && named(h, handle, slug)
        {
            when_row = true;
        }
        if row.needs.iter().any(|n| match n {
            TodoNeed::Job { handle: h } => named(h, handle, slug),
            TodoNeed::Child { id } => kind == BackgroundKind::Subagent && id == handle,
            _ => false,
        }) {
            needed = true;
        }
    }
    (when_row, needed)
}

/// The facts of one completion against this session's board. `wanted` is the session's own
/// record ([`JobWatchers::wanted`]). A completion handed up from a stopped child is decided
/// by the same reading, and is normally held: nothing above it waits on a grandchild's job.
pub fn facts_of(c: &JobCompletion, rows: &[TodoItem], wanted: Option<WantedNow>) -> Facts {
    let mut f = Facts::routine(ResultKind::of(c));
    f.failed = failed(c);
    f.wanted = wanted;
    let (when_row, needed) = plan_says(rows, &c.job, c.slug.as_deref(), c.kind);
    f.when_row = when_row;
    f.needed = needed;
    f
}

/// The sort at a boundary: everything held plus everything newly queued, each decided again
/// against the board as it is now, so a row added since a result was held promotes it.
pub fn sort(
    w: &JobWatchers,
    rows: &[TodoItem],
    skip: impl Fn(&JobCompletion) -> bool,
) -> Vec<(JobCompletion, Why)> {
    let mut all = w.take_held_to_resort();
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

/// Has the longest-held result waited past the cap?
pub fn starved(w: &JobWatchers, cap: Duration, now: SystemTime) -> bool {
    w.oldest_held()
        .is_some_and(|t| now.duration_since(t).unwrap_or(Duration::ZERO) >= cap)
}

/// How a delivery happened, as the record names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Immediate(Why),
    /// Held, and handed over with the next turn the model took anyway.
    Batch,
    /// Held, and released mid-turn by the starvation cap.
    Starvation,
    /// Held while a turn ran, and delivered as a turn of its own the moment that turn ended.
    /// Distinct from [`Rule::Batch`], which is the same batch riding a turn that started for
    /// another reason before the turn-end wake was served.
    TurnEnd,
    /// Arrived while the session was idle, so it was never held and arrives as an ordinary
    /// completion with no `[meanwhile]` heading.
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

/// The `[meanwhile]` note's first line. The `[job]` / `[task]` groups under it are
/// `completion_notices`' own, so the head's compact job rows read them as they always did.
/// It says what the results are and not which rule delivered them
pub fn meanwhile_heading(_n: usize, _rule: Rule, _cap: Duration) -> String {
    MEANWHILE.to_string()
}

/// The opening word of a held batch - what the head's fold and every test that reads the batch
/// apart from the rest of a delivery looks for.
pub const MEANWHILE: &str = "[meanwhile] Finished while you were working:";

/// How long an idle session may hold room lines nothing rang for before they are said as one
/// digest turn. `Config`'s default, and `[room] held_quiet_seconds` overrides it.
pub const HELD_ROOM_QUIET: Duration = Duration::from_secs(10 * 60);

/// The heading a digest of held room lines opens with - the same `[meanwhile]` word a batch of
/// held results opens with, because it is the same thing: what arrived while the session was busy
/// elsewhere, said in one row when it stops to look.
pub fn meanwhile_room(n: usize) -> String {
    match n {
        1 => format!("{MEANWHILE_ROOM} 1 room message while you were idle:"),
        n => format!("{MEANWHILE_ROOM} {n} room messages while you were idle:"),
    }
}

/// The opening of [`meanwhile_room`], which is what a head keys its fold on.
pub const MEANWHILE_ROOM: &str = "[meanwhile]";

/// The append-only delivery record: one JSON line per delivered result, under the session's logs.
///
/// ```text
/// {"session":"s-1","handle":"j12","kind":"job","arrived_at_ms":…,"delivered_at_ms":…,
///  "waited_ms":…,"rule":"batch","batch":3}
/// ```
///
/// `batch` is how many results reached the model in the same note. A write that fails is dropped
/// the record is measurement, and a delivery must never fail because its measurement could not be
/// written. A log with no path records nothing.
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
    /// The record for one session, in its logs directory (`Config::daemon_logs`).
    pub fn under(logs: &Path, session: &str) -> DeliveryLog {
        DeliveryLog {
            path: Some(logs.join(LOG)),
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
        // The metrics row for each, from this one record site, so every rule and door reaches
        // `metrics.db` whether or not this record has a file to write.
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
