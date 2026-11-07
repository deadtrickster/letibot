//! A daemon restart, made as painless as it can be - what a session was running when its
//! daemon (harnessd) stopped, brought back or reminded when a daemon opens it again.
//!
//! The operator's rule: restore agents and monitors by default and remind about jobs, but past a
//! threshold - *"more than 1 hours after the stop"* - simply remind, saying what was running.
//!
//! # Only a daemon stop triggers it
//!
//! Every record is stamped with the instance that wrote it (`Registry::instance`), so a session
//! opened by the daemon that already holds its records - a head attaching, a `/resume`, a parked
//! child - finds nothing of its own. Each record acted on is re-stamped or closed as it is.
//!
//! # How long the daemon was down
//!
//! From the previous daemon's `daemon_life` row ([`Lifeline`]): an orderly stop wrote its exact
//! time, and after a crash the last heartbeat is the latest moment it is KNOWN to have been alive
//! so the down time can be up to one [`BEAT`] too long and the notice says so.
//!
//! The threshold is measured from the STOP to the moment the session is OPENED, not to
//! the daemon's start: a session opened four hours after the stop would otherwise resume its
//! children on the tree they were on four hours before. When the open is over a minute late the
//! notice names both intervals.
//!
//! With no life row at all the stop is UNKNOWN, and is said to be - a daemon that kept no
//! record did not stop "0s ago", and *"down 0s (estimated from nothing)"* is the wall the operator
//! called hilarious ([`Stopped::Unknown`]).
//!
//! # Under the threshold (`Config::restore_within`, default one hour)
//!
//! * subagents that were mid-task are resumed (`crate::subagent_record`)
//! * monitors are declared again with the call that declared them - same name, condition
//!   owner and TTL - and one that cannot be re-established is reported, not dropped
//! * jobs are NOT re-run (running a command twice can do harm) - the model is reminded of
//!   each ([`job_line`]).
//!
//! # Over it - or not known to be under it
//!
//! Nothing is restored; an unknown down time counts as over the threshold, because restoring is
//! the act that needs the stop to have been recent. The model gets one reminder turn listing what
//! was running and the call that brings each back. The operator's warning is `restart_restored`
//! when something WAS restored and `restart_reminded` otherwise ([`Outcome`]).
//!
//! # And a restart with nothing running is said too
//!
//! As a clock: the model's sense of time went through a stop and a start, and only the daemon
//! knows when ([`nothing_running`]), said once per (session, restart)
//!
//! ```text
//! [restart] the daemon came back at 2026-10-12 08:10:56 UTC (down 1s, stopped at 2026-10-12
//! 08:10:55 UTC): nothing was running
//! ```
//!
//! The two cases take different doors. Something was running ⇒ the wake that continues it
//! carries the notice. Nothing was ⇒ the sentence is for the clock alone, filed into the session
//! and read at the next turn, never a turn of its own - except when the session still OWES
//! something (*"i expected that after restart you will continue"*), where the person is waiting
//! for it to continue and [`Owed`] sends the same sentence through the turn door
//! ([`nothing_running_but`]). A session also gets the turn when its last turn ended within the
//! threshold of the stop, or its last words read as waiting on a restart
//! ([`promises_a_restart`]), so "after you restart I'll verify X" is not left to sit.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{
    JobRecord, Store, StoredMonitor, TodoCondition, TodoItem, TodoNeed, TodoStatus,
};
use letibot_transcript::{Speaker, TranscriptItem};

/// How often a daemon writes its heartbeat - the bound on how wrong a crash's down time can be.
pub const BEAT: Duration = Duration::from_secs(30);

/// The default restore threshold - the operator's *"say more than 1 hours after the stop"*.
pub const RESTORE_WITHIN: Duration = Duration::from_secs(3600);

/// A hub's queued operator prompts, kept in the session store.
pub struct StoreJournal {
    path: PathBuf,
    session_id: String,
    store: Option<Store>,
}

impl StoreJournal {
    /// Attach `hub` to the store at `path`: put back the prompts a dead daemon left queued, and
    /// keep the queue saved from here on. Returns how many were put back.
    pub fn attach(hub: &letibot_sessionlog::Hub, path: &std::path::Path) -> usize {
        let session_id = hub.session_id();
        let store = Store::open(path).ok();
        let stored = store
            .as_ref()
            .and_then(|s| s.queued_prompts(&session_id).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|p| letibot_sessionlog::JournaledPrompt {
                enqueued_ms: p.enqueued_ms,
                head_id: p.head_id,
                identity: p.identity,
                client_request_id: p.client_request_id,
                text: p.text,
                attachments: serde_json::from_str(&p.attachments_json).unwrap_or_default(),
            })
            .collect();
        let journal = StoreJournal {
            path: path.to_path_buf(),
            session_id,
            store,
        };
        hub.journal_prompts(Box::new(journal), stored)
    }
}

impl letibot_sessionlog::PromptJournal for StoreJournal {
    fn save(&mut self, prompts: &[letibot_sessionlog::JournaledPrompt]) -> Result<(), String> {
        if self.store.is_none() {
            self.store = Some(Store::open(&self.path).map_err(|e| e.to_string())?);
        }
        let rows: Vec<_> = prompts
            .iter()
            .map(|p| letibot_tokencore::store::QueuedPrompt {
                enqueued_ms: p.enqueued_ms,
                head_id: p.head_id.clone(),
                identity: p.identity.clone(),
                client_request_id: p.client_request_id.clone(),
                text: p.text.clone(),
                attachments_json: serde_json::to_string(&p.attachments)
                    .unwrap_or_else(|_| "[]".into()),
            })
            .collect();
        self.store
            .as_ref()
            .ok_or_else(|| "no store".to_string())?
            .replace_queued_prompts(&self.session_id, &rows)
            .map_err(|e| e.to_string())
    }
}

/// This daemon's life, written down - the start, a heartbeat every [`BEAT`], and the stop.
/// The stop goes through a connection opened at the START, never a fresh one
pub struct Lifeline {
    store: Option<Store>,
    registry: Arc<Registry>,
}

impl Lifeline {
    pub fn start(path: Option<PathBuf>, registry: Arc<Registry>) -> Lifeline {
        let mut store = None;
        if let Some(p) = &path {
            let instance = registry.instance().to_string();
            let started = registry.started_ms();
            match Store::open(p).and_then(|s| s.life_started(&instance, started).map(|()| s)) {
                Ok(s) => {
                    store = Some(s);
                    // A second HANDLE on the same connection, moved to the heartbeat's thread
                    // `Store::open` shares, so this opens no descriptor of its own.
                    if let Ok(beat) = Store::open(p) {
                        let reg = registry.clone();
                        let _ = std::thread::Builder::new()
                            .name("daemon-heartbeat".into())
                            .spawn(move || heartbeat(beat, reg));
                    }
                }
                Err(e) => eprintln!(
                    "  this daemon's start was not recorded ({e}); a restart after it cannot \
                     say how long it was down"
                ),
            }
        }
        Lifeline { store, registry }
    }

    /// The orderly stop, said with its exact time.
    pub fn stopped(&self) {
        let Some(store) = &self.store else {
            return;
        };
        let now = letibot_sessionlog::registry::now_ms();
        if let Err(e) = store.life_stopped(self.registry.instance(), now) {
            eprintln!("  this daemon's stop was not recorded ({e})");
        }
    }
}

/// A beat every [`BEAT`] while the daemon serves - and the stop, the moment it stops
/// serving.
fn heartbeat(store: Store, registry: Arc<Registry>) {
    let mut last = std::time::Instant::now();
    while !registry.is_closed() {
        std::thread::sleep(Duration::from_millis(500));
        if last.elapsed() >= BEAT {
            last = std::time::Instant::now();
            let _ = store.life_beat(registry.instance(), letibot_sessionlog::registry::now_ms());
        }
    }
    let said = store
        .life(registry.instance())
        .ok()
        .flatten()
        .is_some_and(|l| l.stopped_ms.is_some());
    if !said {
        let _ = store.life_stopped(registry.instance(), letibot_sessionlog::registry::now_ms());
    }
}

/// When the previous daemon stopped: a fact, an estimate, or not known at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    At {
        at_ms: u64,
        /// `None` for an orderly stop's own time; otherwise what the time was estimated from.
        estimated_from: Option<&'static str>,
    },
    /// No record says when - the daemon kept no life row. `seen_ms` is the latest moment one
    /// of its records was written (`0` for none): a lower bound, used only to close a monitor whose
    /// TTL ran out before it, never to state a down time.
    Unknown { seen_ms: u64 },
}

impl Stopped {
    /// The stop's time, when it is known (exactly or as an estimate).
    pub fn at_ms(&self) -> Option<u64> {
        match self {
            Stopped::At { at_ms, .. } => Some(*at_ms),
            Stopped::Unknown { .. } => None,
        }
    }

    /// The latest moment the previous daemon is known to have been alive - the stop when it is
    /// known, the last record otherwise.
    pub fn no_earlier_than_ms(&self) -> u64 {
        match self {
            Stopped::At { at_ms, .. } => *at_ms,
            Stopped::Unknown { seen_ms } => *seen_ms,
        }
    }

    /// Whether what it was running is restored - under `threshold` from the stop to
    /// `opened_ms`, the moment the session is being opened and not the daemon's return the harm
    /// the threshold prevents is a stale tree, and that is measured from the stop to the
    pub fn restores(&self, opened_ms: u64, threshold: Duration) -> bool {
        self.at_ms()
            .is_some_and(|at| Duration::from_millis(opened_ms.saturating_sub(at)) < threshold)
    }

    /// The stop of the daemon that went last, among several - the latest known stop, unless
    /// a daemon with no record was seen alive after it (then the last stop is unknown too).
    pub fn latest(stops: impl IntoIterator<Item = Stopped>) -> Stopped {
        let mut known: Option<Stopped> = None;
        let mut seen: Option<u64> = None;
        for s in stops {
            match s {
                Stopped::At { at_ms, .. } => {
                    if known
                        .as_ref()
                        .and_then(Stopped::at_ms)
                        .is_none_or(|k| at_ms > k)
                    {
                        known = Some(s);
                    }
                }
                Stopped::Unknown { seen_ms } => {
                    seen = Some(seen.map_or(seen_ms, |m| m.max(seen_ms)))
                }
            }
        }
        match (known, seen) {
            (Some(k), Some(seen_ms)) if seen_ms > k.no_earlier_than_ms() => {
                Stopped::Unknown { seen_ms }
            }
            (Some(k), _) => k,
            (None, seen) => Stopped::Unknown {
                seen_ms: seen.unwrap_or(0),
            },
        }
    }
}

/// The stop of `instance`, read from its life row; [`Stopped::Unknown`] when it has none
/// `seen_ms` being the latest write among its records.
pub fn stop_of(store: &Store, instance: &str, seen_ms: u64) -> Stopped {
    match store.life(instance).ok().flatten() {
        Some(l) => match l.stopped_ms {
            Some(at) => Stopped::At {
                at_ms: at,
                estimated_from: None,
            },
            None => Stopped::At {
                at_ms: l.beat_ms,
                estimated_from: Some("its last heartbeat (it did not stop in an orderly way)"),
            },
        },
        None => Stopped::Unknown { seen_ms },
    }
}

/// How stale another daemon's heartbeat may be before its records are a gone daemon's: two beats,
/// so one missed beat is not a death and a crashed daemon's rows are not held for long.
pub const ALIVE_WITHIN: Duration = Duration::from_secs(2 * BEAT.as_secs());

/// Whether the daemon that wrote `instance` is still running: no stop recorded, a heartbeat inside
/// [`ALIVE_WITHIN`], and - when the instance names it - a pid that is a live process. A store is
/// shared by every daemon on the box, so another instance's records are not a stop's.
pub fn is_running(store: &Store, instance: &str, now_ms: u64) -> bool {
    let Some(life) = store.life(instance).ok().flatten() else {
        return false;
    };
    if life.stopped_ms.is_some()
        || now_ms.saturating_sub(life.beat_ms) >= ALIVE_WITHIN.as_millis() as u64
    {
        return false;
    }
    match pid_of(instance) {
        // A pid that is gone, or is a zombie, means the record outlived the daemon whatever the
        // clock says - so a crash is not read as a daemon that is still working.
        Some(pid) => letibot_tools::exec::procs::read(pid).is_some_and(|p| p.state != 'Z'),
        None => true,
    }
}

/// The pid an instance was minted with (`d-<pid>-<nanos>-<n>`), when its name carries one.
fn pid_of(instance: &str) -> Option<u32> {
    instance.strip_prefix("d-")?.split('-').next()?.parse().ok()
}

/// What a restart did, which decides the notice's first line and the warning's code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Under the threshold, and at least one thing was brought back.
    Restored,
    /// Under the threshold, but nothing could be or needed to be brought back - only jobs, which
    /// are never re-run, or monitors that could not be re-armed.
    NothingRestorable,
    /// Over the threshold, or the down time is unknown: nothing was even tried.
    Reminded,
    /// There was nothing to try - no subagent, monitor or job was running at the stop. The
    /// sentence is [`nothing_running`]'s, and it is said for the clock alone.
    Nothing,
}

impl Outcome {
    /// The hub warning this outcome is raised as.
    pub fn code(self) -> &'static str {
        match self {
            Outcome::Restored => "restart_restored",
            Outcome::NothingRestorable | Outcome::Reminded => "restart_reminded",
            // Its own word, because it is its own sentence: a head that read this as
            // `restart_reminded` would be told *"reminded"* about a list that does not exist.
            Outcome::Nothing => "restart_nothing",
        }
    }
}

/// A duration as a person reads it: `3h12m`, `4m05s`, `12s`.
pub fn span(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// A session opened this long after its daemon came back is told so - otherwise a session
/// nobody looked at for hours reads *"the daemon was down for 5m"* as the whole of the gap.
pub const OPENED_LATE_MS: u64 = 60_000;

/// The first line of every restart notice: how long the daemon was down (or that nobody
/// knows), against which threshold, and what that meant.
pub fn header(
    stop: &Stopped,
    back_ms: u64,
    opened_ms: u64,
    threshold: Duration,
    outcome: Outcome,
) -> String {
    // The empty restart is its own sentence, not this one with an empty list: there is no
    // threshold clause to make, and the one thing it has to say is when the daemon came back.
    if outcome == Outcome::Nothing {
        return nothing_running(stop, back_ms);
    }
    let (at_ms, estimated_from) = match stop {
        Stopped::At {
            at_ms,
            estimated_from,
        } => (*at_ms, *estimated_from),
        Stopped::Unknown { .. } => {
            return "[restart] The daemon restarted (stop time unknown). Nothing was restored \
                    automatically; this is what you were running when it stopped — bring back \
                    only what still matters:"
                .to_string();
        }
    };
    let down = Duration::from_millis(back_ms.saturating_sub(at_ms));
    // what happened leads, and the clock is one bracket - the sentence a reader needs is
    // the same whether the daemon can name its stop time or not. `~` is an inference from a
    // heartbeat rather than a recorded stop.
    let est = if estimated_from.is_some() { "~" } else { "" };
    match outcome {
        Outcome::Restored => format!(
            "[restart] The daemon restarted (down {est}{}). Restored:",
            span(down)
        ),
        Outcome::NothingRestorable => format!(
            "[restart] The daemon restarted (down {est}{}). Nothing could be restored; this is \
             what you were running:",
            span(down)
        ),
        // A late open is the one case whose cause is invisible without the threshold, so the
        // sentence names the window and the gap the session was opened over.
        Outcome::Reminded => format!(
            "[restart] The daemon restarted (down {est}{}). Nothing was restored automatically: a \
             session is restored only within {} of the stop, and this one was opened {} after it. \
             This is what you were running when it stopped — bring back only what still matters:",
            span(down),
            span(threshold),
            span(Duration::from_millis(opened_ms.saturating_sub(at_ms)))
        ),
        // Unreachable - the empty restart returned above - and written out rather than as an
        // `unreachable!` so that a later arm cannot make this function lie.
        Outcome::Nothing => nothing_running(stop, back_ms),
    }
}

/// A restart that had nothing to restore, said anyway - the operator: *"mb always tell
/// agent about restart? with the current time lol"*. It carries the down time and says there was
/// nothing to bring back, which is the whole of the news.
pub fn nothing_running(stop: &Stopped, back_ms: u64) -> String {
    let down = match stop {
        Stopped::At {
            at_ms,
            estimated_from,
        } => format!(
            "down {}{}",
            if estimated_from.is_some() { "~" } else { "" },
            span(Duration::from_millis(back_ms.saturating_sub(*at_ms)))
        ),
        // Not "down 0s": nobody knows when it stopped, and a restart with nothing running is
        // exactly the one with no record to read it from - see the module docs.
        Stopped::Unknown { .. } => "stop time unknown".to_string(),
    };
    format!("[restart] The daemon restarted ({down}). Nothing was running.")
}

/// Whether a session's last row is a turn that was still going when its rows stopped - an
/// `Assistant` with no call after it is a finished turn, and every other row is a turn that
/// never got its answer.
pub fn mid_turn(last: Option<&TranscriptItem>) -> bool {
    match last {
        None => false,
        Some(TranscriptItem::Assistant { tool_calls, .. }) => !tool_calls.is_empty(),
        Some(TranscriptItem::User {
            speaker: Speaker::Agent,
            ..
        }) => false,
        Some(_) => true,
    }
}

/// What a session still owes when its daemon comes back - the reason an empty restart is a
/// TURN and not a row. Every arm is a fact in the store or on the board. None of them is
/// prose
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Owed {
    /// A turn was in flight when the rows stopped - [`mid_turn`].
    pub mid_turn: bool,
    /// Settlements this daemon wrote and nobody has been told about - a child that answered
    /// or a job that ended, into a session nobody was holding
    /// (`Harness::deliver_settlements_left_undelivered`).
    pub settlements: usize,
    /// Rows whose condition the world now meets - the board says the work is due
    /// (`Harness::due_todo_rows`).
    pub due_rows: usize,
    /// Rows still owed - the plan the model wrote, `Pending` or `InProgress`. A row the
    /// operator postponed or struck off is not owed, and a completed one is not either.
    pub plan_rows: usize,
    /// The session was active within the threshold of the stop, or its last message promised
    /// to continue after a restart. It is woken so it can follow through on that.
    pub wake: bool,
}

impl Owed {
    /// Whether there is anything at all - the one question the door is chosen on.
    pub fn any(&self) -> bool {
        self.mid_turn || self.wake || self.settlements + self.due_rows + self.plan_rows > 0
    }

    /// Why this restart is a turn, as the sentence says it - every arm that holds, in the
    /// model's own reading order: the turn it was in, then what the daemon could not deliver
    /// then what the board says.
    pub fn clause(&self) -> String {
        fn rows(n: usize, one: &str, many: &str) -> String {
            match n {
                1 => format!("1 {one}"),
                n => format!("{n} {many}"),
            }
        }
        let mut parts: Vec<String> = Vec::new();
        if self.mid_turn {
            parts.push("a turn was in flight when the rows stopped".into());
        }
        if self.settlements > 0 {
            parts.push(rows(
                self.settlements,
                "a settlement nobody has been told about",
                "settlements nobody has been told about",
            ));
        }
        if self.due_rows > 0 {
            parts.push(rows(
                self.due_rows,
                "a todo row whose condition is met",
                "todo rows whose condition is met",
            ));
        }
        if self.plan_rows > 0 {
            parts.push(rows(
                self.plan_rows,
                "a todo row still owed",
                "todo rows still owed",
            ));
        }
        parts.join(", ")
    }
}

/// A restart with nothing running that still owes work - the same sentence as
/// [`nothing_running`], with the reason it is a TURN rather than a row: a model woken to *nothing
/// was running* alone has no way to know why it is reading it now.
pub fn nothing_running_but(stop: &Stopped, back_ms: u64, owed: &Owed) -> String {
    let mut text = nothing_running(stop, back_ms);
    let work = owed.clause();
    if !work.is_empty() {
        text.push_str(&format!(
            " — but this session has work outstanding ({work}), so this turn is that work \
             continuing and not a notice about it: read the conversation and carry on."
        ));
    }
    if owed.wake {
        text.push_str(WAKE_TAIL);
    }
    text
}

/// What a woken model is told to do about the restart it may have been waiting on.
const WAKE_TAIL: &str = " If you told the operator you would do something once they restarted, \
    do that one thing now and report it. If you were waiting for the operator's answer, keep waiting: \
    a restart is not an answer, so do not decide for them or act on an open question. If nothing \
    was waiting on it, end this turn with no reply.";

/// The restart's own sentence, with a layer's half where one answered (X21): `lifecycle_restart` is
/// asked with the facts, and its `say` becomes the words the model reads before the turn. A layer can
/// reword what a restart means and CANNOT cancel the wake - the turn runs whatever it answers, which
/// is the half of `WAKE_TAIL` no release ever let a project change.
///
/// One act, not four: a sentence is all a caller that composes text can perform. A row, a monitor or a
/// room message belongs to a boundary that still has a harness behind it (`open`, `reseat`, `close`).
pub fn wake_text(
    host: Option<&crate::plugins::PluginHost>,
    hub: &std::sync::Arc<letibot_sessionlog::hub::Hub>,
    session: &str,
    stop: &Stopped,
    back_ms: u64,
    owed: &Owed,
    restore_within: std::time::Duration,
) -> String {
    let mut text = match owed.any() {
        true => nothing_running_but(stop, back_ms, owed),
        false => {
            let now = letibot_sessionlog::event::now_ms();
            let mut text = header(stop, back_ms, now, restore_within, Outcome::Nothing);
            if stop.at_ms().is_some() && !stop.restores(now, restore_within) {
                text.push_str(&format!(
                    " The stop is older than the {} restore window.",
                    span(restore_within)
                ));
            }
            text
        }
    };
    let Some(host) = host else {
        return text;
    };
    let input = letibot_plugin::lifecycle::Input {
        v: letibot_plugin::V,
        event: "restart".into(),
        session: session.into(),
        stale: Default::default(),
        rows: Vec::new(),
        now_ms: Some(letibot_sessionlog::event::now_ms()),
    };
    let Ok(input) = serde_json::to_value(&input) else {
        return text;
    };
    for (verb, args) in host.lifecycle(hub, "restart", &input).acts {
        if verb != "say" {
            continue;
        }
        let said = args
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .trim();
        if !said.is_empty() {
            // Before the release's own words, never instead of them: what owed a turn is this
            // session's fact, and a layer's sentence is what it wants said about it.
            text = format!("{said} {text}");
        }
    }
    text
}

/// What a model says when it has put work off until the operator restarts something. A last
/// message that reads like one of these is waiting on this restart, whatever its age.
pub const RESTART_PROMISES: &[&str] = &[
    "after you restart I will verify the fix",
    "once the daemon is back I will check it",
    "when you restart letibot I'll continue from here",
    "ping me after the rebuild and the restart",
    "after the restart I will run the tests again",
    "restart the daemon and tell me, then I will confirm",
    "I will pick this up when letibot is back up",
    "waiting for you to restart before I carry on",
];

/// The score a sentence has to reach against a probe. Lexical-only scores on the fixture sit
/// 0.48 and up for promises and 0.25 and below for unrelated text; the floor is between, nearer
/// the promises so a stray mention of "restart" does not wake a session.
pub const PROMISE_FLOOR: f64 = 0.4;

/// Whether `text` reads as waiting on a restart: its best sentence against [`RESTART_PROMISES`],
/// with the box's second tier when it has one.
pub fn promises_a_restart(text: &str, tier: Option<&letibot_embeddings::Tier>) -> bool {
    use letibot_tools::similarity::Corpus;
    let probes: Vec<(PathBuf, String)> = RESTART_PROMISES
        .iter()
        .enumerate()
        .map(|(i, p)| (PathBuf::from(format!("probe-{i}")), (*p).to_string()))
        .collect();
    let corpus = Corpus::of_texts(&probes);
    let _ = corpus.fill(tier);
    text.split(['.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .any(|s| {
            corpus
                .best_with(s, tier)
                .is_some_and(|(_, score)| score >= PROMISE_FLOOR)
        })
}

/// The call that declares a monitor again, as the model would write it.
pub fn monitor_call(m: &StoredMonitor) -> String {
    format!("`monitor {}`", m.args_json)
}

/// A monitor re-armed under the threshold, as the model is told.
pub fn monitor_rearmed_line(m: &StoredMonitor) -> String {
    format!(
        "- monitor `{}` was re-armed with the call that declared it ({}); its TTL starts again \
         from now.",
        m.name,
        monitor_call(m)
    )
}

/// A monitor whose re-arming was refused (`why`), as the model is told.
pub fn monitor_not_rearmed_line(m: &StoredMonitor, why: &str) -> String {
    format!(
        "- monitor `{}` could NOT be re-armed — {why}. It was declared with {}; what it watched \
         may be gone.",
        m.name,
        monitor_call(m)
    )
}

/// The named scope a monitor was owned by, when it was - not `turn`, not `session`, not left to
/// the default. Such a scope is something the model set up, and only the stopped daemon knew what
/// it was for or whether anything in it still runs.
pub fn named_owner(m: &StoredMonitor) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&m.args_json).ok()?;
    let owner = v.get("owner")?.as_str()?.trim();
    (!owner.is_empty() && owner != "turn" && owner != "session").then(|| owner.to_string())
}

/// A monitor owned by a named scope, as the model is told - never re-armed on its own: *"who
/// knows wtf was setup and how relevant it still is"*.
pub fn monitor_scoped_line(m: &StoredMonitor, scope: &str) -> String {
    format!(
        "- monitor `{}` was owned by the scope `{scope}`, which the stopped daemon held — it was \
         NOT re-armed: what that scope was set up for, and whether anything in it still runs, is \
         yours to check (`job_list`). If it still matters, re-arm it with {}, naming an owner \
         that exists now or leaving `owner` out for this session.",
        m.name,
        monitor_call(m)
    )
}

/// A monitor over the threshold, as the model is told.
pub fn monitor_reminded_line(m: &StoredMonitor) -> String {
    format!(
        "- monitor `{}` was watching; re-arm it with {} if it still matters.",
        m.name,
        monitor_call(m)
    )
}

/// The notice the model gets: the header, a line per thing that was running, and - when
/// nothing was brought back - the one sentence of what to do about it.
pub fn notice(header: &str, lines: &[String], outcome: Outcome) -> String {
    // One sentence, and nothing after it: there is no list, and the closing *"nothing above is
    // running now"* would be a sentence about a list that is not there.
    if outcome == Outcome::Nothing {
        return header.to_string();
    }
    let mut text = header.to_string();
    if !lines.is_empty() {
        text.push('\n');
        text.push_str(&lines.join("\n"));
    }
    // a restored restart still leaves every background job dead - its process was the
    // old daemon's - so a job line means the closing is owed whatever the outcome says.
    let any_job = lines.iter().any(|l| l.starts_with("- job "));
    if outcome != Outcome::Restored || any_job {
        text.push_str(
            "\nNothing above is running now. Bring back only what still matters: \
             `task_resume` for a subagent, `monitor` for a watch, `bash` for a job.",
        );
    }
    text
}

/// The hub warning's detail - the notice's first line and one line per thing ([`said`]).
/// The TUI's `restart_card` reads it as one line.
pub fn warning_detail(notice: &str, said: &[String]) -> String {
    let head = notice.lines().next().unwrap_or("");
    // A restart with nothing in it is one line - no trailing newline, because there is no
    // second line to end.
    if said.is_empty() {
        return head.to_string();
    }
    format!(
        "{head}\n{}",
        said.iter()
            .map(|l| format!("  - {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// What the operator's warning says of each thing - one line each, the daemon's log too.
pub mod said {
    pub fn resumed(name: &str) -> String {
        format!("subagent {name}: resumed")
    }

    pub fn delivering(name: &str) -> String {
        format!("subagent {name}: had finished, its result not yet delivered — delivering it")
    }

    pub fn not_resumed(name: &str, why: &str) -> String {
        format!("subagent {name}: NOT resumed — {why}")
    }

    pub fn subagent_reminded(name: &str) -> String {
        format!("subagent {name}: reminded, not resumed")
    }

    pub fn rearmed(name: &str) -> String {
        format!("monitor `{name}`: re-armed")
    }

    pub fn not_rearmed(name: &str, why: &str) -> String {
        format!("monitor `{name}`: NOT re-armed — {why}")
    }

    pub fn monitor_reminded(name: &str) -> String {
        format!("monitor `{name}`: reminded, not re-armed")
    }

    pub fn monitor_scoped(name: &str, scope: &str) -> String {
        format!("monitor `{name}`: not re-armed — owned by the scope `{scope}`; the model is told")
    }

    pub fn job_settled(handle: &str) -> String {
        format!("job `{handle}`: had ended, its result not yet delivered — said now")
    }

    pub fn job(handle: &str, waiting: usize) -> String {
        if waiting == 0 {
            format!("job `{handle}`: reminded (not re-run)")
        } else {
            format!("job `{handle}`: reminded (not re-run), {waiting} waiting row(s) released")
        }
    }
}

/// What a job row reads when the old daemon's orderly stop ended it - `Registry::close` runs
/// every session's `end_running("the daemon stopping")` before the process exits. Such a job was
/// running at the stop as surely as one a crash left `running`, and is reminded the same way.
pub const STOPPED_WITH_THE_DAEMON: &str = "killed by the daemon stopping";

/// One background job that was running at the stop, as the model is reminded of it.
/// `ran_ms` is how long it had run when the daemon stopped. `waiting` is the open rows that
/// waited on it - named, with what happened to their wait.
pub fn job_line(
    job: &JobRecord,
    slug: Option<&str>,
    cwd: Option<&str>,
    ran_ms: Option<u64>,
    waiting: &[String],
) -> String {
    let mut about: Vec<String> = Vec::new();
    if let Some(s) = slug {
        about.push(format!("slug `{s}`"));
    }
    if let Some(ms) = ran_ms {
        about.push(format!(
            "had run {} when the daemon stopped",
            span(Duration::from_millis(ms))
        ));
    }
    let mut line = format!("- job `{}`", job.handle);
    if !about.is_empty() {
        line.push_str(&format!(" ({})", about.join(", ")));
    }
    // One sentence of why, and the command said once - in the call that runs it again.
    if job.state == STOPPED_WITH_THE_DAEMON {
        // The orderly stop ended it: there is nothing left running to look for.
        line.push_str(
            ": ENDED by the old daemon's orderly stop, unfinished, and NOT run again — its \
             process was the old daemon's.",
        );
    } else {
        line.push_str(
            ": NOT run again — its process was the old daemon's, and a crash may have left it \
             running (check with `pkill` pattern=\"<a fragment of its command>\" \
             action=\"list\", which kills nothing).",
        );
    }
    // Where its output went, read out of the command - the record's own `redirect` was
    // never filled for a running row by the builds before this one, and the operator's job
    // `cd … && … > /tmp/acceptance-child.log 2>&1` was reminded as having written no file.
    let mut files: Vec<String> = job.redirect.iter().cloned().collect();
    for f in letibot_tools::builtins::output_redirect_paths(&job.command) {
        if !files.contains(&f) {
            files.push(f);
        }
    }
    if files.is_empty() {
        line.push_str(" Its output was in the old daemon's memory and is gone.");
    } else {
        let named = files
            .iter()
            .map(|f| format!("`{f}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let survives = if files.len() == 1 {
            "survives — read that"
        } else {
            "survive — read those"
        };
        line.push_str(&format!(" Its output went to {named}, which {survives}."));
    }
    if job.command.is_empty() {
        line.push_str(" Its command was not recorded.");
    } else {
        let mut call = serde_json::json!({"command": job.command, "background": true});
        if let Some(cwd) = cwd {
            call["cwd"] = serde_json::Value::String(cwd.to_string());
        }
        if let Some(s) = slug {
            call["slug"] = serde_json::Value::String(s.to_string());
        }
        line.push_str(&format!(" To run it again: `bash {call}`."));
    }
    if !waiting.is_empty() {
        line.push_str(&format!(
            "\n  Rows that waited on it: {}. Their wait is RELEASED — a job this daemon never ran \
             reads as ended, so they are no longer blocked on it; if they still need its result, \
             run it again and set `wait_on` on the row to the new handle with `todo_write`.",
            waiting.join("; ")
        ));
    }
    line
}

/// One background job that had ENDED at the stop and whose ending never reached the model - held under while a turn ran, or queued, when the daemon went down. The row's own
/// record is all that is left.
pub fn settled_job_line(job: &JobRecord, slug: Option<&str>) -> String {
    let mut line = format!("- job `{}`", job.handle);
    if let Some(s) = slug {
        line.push_str(&format!(" (slug `{s}`)"));
    }
    line.push_str(&format!(
        " had ENDED before the stop — {}, {} byte(s) of output, after {} — and its result never \
         reached you.",
        job.state,
        job.produced,
        span(Duration::from_millis(job.elapsed_ms))
    ));
    let mut files: Vec<String> = job.redirect.iter().cloned().collect();
    for f in letibot_tools::builtins::output_redirect_paths(&job.command) {
        if !files.contains(&f) {
            files.push(f);
        }
    }
    if files.is_empty() {
        line.push_str(" Its output was in the old daemon's memory and is gone.");
    } else {
        let named = files
            .iter()
            .map(|f| format!("`{f}`"))
            .collect::<Vec<_>>()
            .join(", ");
        line.push_str(&format!(" Its output went to {named} — read that."));
    }
    if !job.command.is_empty() {
        line.push_str(&format!(" It ran: `{}`.", job.command));
    }
    line
}

/// The open rows of a board that wait on `handle` (or its slug) - by `when: job` or by a
/// `wait_on` edge - as the reminder names them.
pub fn rows_waiting_on(rows: &[TodoItem], handle: &str, slug: Option<&str>) -> Vec<String> {
    let names = |h: &str| h == handle || slug.is_some_and(|s| s == h);
    rows.iter()
        .filter(|r| matches!(r.status, TodoStatus::Pending | TodoStatus::InProgress))
        .filter(|r| {
            matches!(&r.when, Some(TodoCondition::Job { handle }) if names(handle))
                || r.needs
                    .iter()
                    .any(|n| matches!(n, TodoNeed::Job { handle } if names(handle)))
        })
        .map(|r| format!("`{}`", r.content))
        .collect()
}

/// A monitor declaration as the harness records it - the `monitor` tool, wrapped so a
/// successful declaration is written down with its own arguments, a renewal moves its TTL, and a
/// retirement closes it. Everything else is the tool's own.
pub struct RecordingMonitor {
    pub inner: letibot_tools::builtins::monitor::Monitor,
    pub path: Option<PathBuf>,
    pub session_id: String,
    pub instance: String,
}

impl letibot_tools::Tool for RecordingMonitor {
    fn schema(&self) -> letibot_tools::schema::ToolSchema {
        self.inner.schema()
    }

    fn access_for(&self, args: &serde_json::Value) -> Option<letibot_tools::schema::Access> {
        self.inner.access_for(args)
    }

    fn invoke(
        &self,
        ctx: &mut letibot_tools::runtime::InvokeCtx<'_>,
        args: &serde_json::Value,
    ) -> letibot_tools::runtime::Invocation {
        let out = self.inner.invoke(ctx, args);
        if out.outcome != letibot_transcript::ToolOutcome::Ok {
            return out;
        }
        let (Some(path), Some(name)) = (&self.path, args.get("name").and_then(|v| v.as_str()))
        else {
            return out;
        };
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("watch");
        let wrote = Store::open(path).and_then(|s| match action {
            "retire" => s.end_monitor(&self.session_id, name),
            "renew" => {
                let ttl = ttl_of(args);
                let known = s
                    .monitors_of(&self.session_id)?
                    .into_iter()
                    .find(|m| m.name == name);
                match known {
                    Some(m) => {
                        let mut a: serde_json::Value =
                            serde_json::from_str(&m.args_json).unwrap_or_default();
                        a["ttl_ms"] = serde_json::json!(ttl);
                        s.put_monitor(&self.session_id, name, &a.to_string(), &self.instance, ttl)
                    }
                    None => Ok(()),
                }
            }
            _ => s.put_monitor(
                &self.session_id,
                name,
                &args.to_string(),
                &self.instance,
                ttl_of(args),
            ),
        });
        if let Err(e) = wrote {
            eprintln!(
                "  {} · monitor `{name}` was not recorded ({e}); a restart will not re-arm it",
                self.session_id
            );
        }
        out
    }
}

fn ttl_of(args: &serde_json::Value) -> u64 {
    // A GitHub watch's default is a day, not a monitor's five minutes - recorded as five, a
    // restart would close a live watch as one that had already run out.
    let default = if args.get("github").is_some() {
        letibot_tools::exec::github::GITHUB_DEFAULT_TTL
    } else {
        letibot_tools::exec::monitor::DEFAULT_TTL
    };
    args.get("ttl_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(default.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    #[test]
    fn restart_promises_match_and_other_text_does_not() {
        use super::promises_a_restart;
        for t in [
            "After you restart the daemon I'll verify the migration landed.",
            "Fixed it. Once the daemon is back, ping me and I will rerun the suite",
        ] {
            assert!(promises_a_restart(t, None), "{t}");
        }
        for t in [
            "I fixed the parser bug and added tests for the lexer.",
            "The restart of the cache is not needed here",
            "Done. The branch is merged and the todo list is clear.",
        ] {
            assert!(!promises_a_restart(t, None), "{t}");
        }
    }

    /// A GitHub watch is recorded with its own default life, so a restart inside it re-arms it.
    #[test]
    fn a_github_watch_is_recorded_for_a_day() {
        assert_eq!(
            super::ttl_of(&serde_json::json!({"github": {"url": "u", "until": "checks"}})),
            24 * 3600 * 1000
        );
        assert_eq!(super::ttl_of(&serde_json::json!({"timer_ms": 5})), 300_000);
    }

    /// Only a scope the model named is a named owner - `turn`, `session` and no owner at all
    /// are the daemon's own lifetimes, and a monitor on them is re-armed as before.
    #[test]
    fn a_named_owner_is_the_models_own_scope() {
        let m = |args: &str| StoredMonitor {
            session_id: "s".into(),
            name: "m".into(),
            args_json: args.into(),
            instance: "i".into(),
            ttl_ms: 1,
            live: true,
            declared_ms: 0,
            updated_ms: 0,
        };
        assert_eq!(
            named_owner(&m(r#"{"name":"m","owner":"lubuntu2-soak"}"#)).as_deref(),
            Some("lubuntu2-soak")
        );
        for plain in [
            r#"{"name":"m"}"#,
            r#"{"name":"m","owner":"session"}"#,
            r#"{"name":"m","owner":"turn"}"#,
            r#"{"name":"m","owner":""}"#,
        ] {
            assert_eq!(named_owner(&m(plain)), None, "{plain}");
        }
        let line = monitor_scoped_line(&m(r#"{"name":"m","owner":"x"}"#), "x");
        assert!(
            line.contains("NOT re-armed") && line.contains("`job_list`"),
            "{line}"
        );
    }

    use super::*;

    const AT: u64 = 1_791_806_400_000;

    /// A restart with nothing running is said anyway. The whole sentence is pinned: what
    /// happened and the one clock, and nothing about how it stopped.
    #[test]
    fn an_empty_restart_says_the_daemon_restarted_and_nothing_was_running() {
        let back = AT + 1_000;
        let orderly = Stopped::At {
            at_ms: AT,
            estimated_from: None,
        };
        assert_eq!(
            nothing_running(&orderly, back),
            "[restart] The daemon restarted (down 1s). Nothing was running."
        );
        // A crash: the time is the last heartbeat, and the `~` says so rather than claiming an
        // orderly stop.
        let crash = Stopped::At {
            at_ms: AT,
            estimated_from: Some("its last heartbeat (it did not stop in an orderly way)"),
        };
        assert_eq!(
            nothing_running(&crash, back),
            "[restart] The daemon restarted (down ~1s). Nothing was running."
        );
        // And a stop nobody dated is still said to be unknown - not `down 0s`.
        assert_eq!(
            nothing_running(&Stopped::Unknown { seen_ms: 0 }, back),
            "[restart] The daemon restarted (stop time unknown). Nothing was running."
        );
    }

    /// The empty case is one sentence and nothing else - no list, and none of the closing
    /// *"nothing above is running now"* the reminded notice ends with, because there is no list
    /// for it to be about. Its header comes from `header`, so the two entry points cannot drift.
    #[test]
    fn an_empty_restart_is_one_sentence_with_nothing_after_it() {
        let stop = Stopped::At {
            at_ms: AT,
            estimated_from: None,
        };
        let back = AT + 1_000;
        let text = notice(
            &header(&stop, back, back, RESTORE_WITHIN, Outcome::Nothing),
            &[],
            Outcome::Nothing,
        );
        assert_eq!(text, nothing_running(&stop, back));
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(!text.contains("Nothing above is running now"), "{text}");
        assert!(!text.contains("threshold"), "{text}");
        // And the warning's detail is that one line, with no empty second line under it.
        assert_eq!(warning_detail(&text, &[]), text);
        assert_eq!(Outcome::Nothing.code(), "restart_nothing");
    }

    /// The case with records: what happened first, the clock in one bracket, and the list.
    #[test]
    fn a_restart_with_records_keeps_its_own_sentence() {
        let stop = Stopped::At {
            at_ms: AT,
            estimated_from: None,
        };
        let back = AT + 4_000;
        let h = header(
            &stop,
            back,
            back,
            RESTORE_WITHIN,
            Outcome::NothingRestorable,
        );
        assert_eq!(
            h,
            "[restart] The daemon restarted (down 4s). Nothing could be restored; this is what \
             you were running:"
        );
        let text = notice(
            &h,
            &["- job `j9` was reminded".into()],
            Outcome::NothingRestorable,
        );
        assert!(text.ends_with("`bash` for a job."), "{text}");
    }

    #[test]
    fn the_header_says_what_happened_and_how_long_it_was_down() {
        let stop = Stopped::At {
            at_ms: AT,
            estimated_from: None,
        };
        let back = AT + 5 * 60_000;
        assert!(stop.restores(back, RESTORE_WITHIN));
        let h = header(&stop, back, back, RESTORE_WITHIN, Outcome::Restored);
        assert_eq!(h, "[restart] The daemon restarted (down 5m00s). Restored:");
        let crash = Stopped::At {
            at_ms: AT,
            estimated_from: Some("its last heartbeat (it did not stop in an orderly way)"),
        };
        let late = AT + 2 * 3_600_000;
        assert!(!crash.restores(late, RESTORE_WITHIN));
        let h = header(&crash, late, late, RESTORE_WITHIN, Outcome::Reminded);
        assert!(
            h.starts_with("[restart] The daemon restarted (down ~2h00m)."),
            "{h}"
        );
        assert!(h.contains("Nothing was restored automatically"), "{h}");
        // The threshold decides the outcome; it is not a clause in every sentence.
        assert!(!h.contains("threshold"), "{h}");
    }

    /// The threshold is measured to the OPEN, not to the daemon's return - the session's gap,
    /// not the daemon's - and a reminded notice names the window and the gap it was opened over.
    #[test]
    fn a_session_opened_hours_after_the_stop_is_measured_from_the_open() {
        let stop = Stopped::At {
            at_ms: AT,
            estimated_from: None,
        };
        let back = AT + 5 * 60_000;
        let opened = AT + 4 * 3_600_000;
        assert!(
            stop.restores(back, RESTORE_WITHIN),
            "the daemon's own downtime"
        );
        assert!(
            !stop.restores(opened, RESTORE_WITHIN),
            "the session was opened four hours after the stop, so nothing is restored"
        );
        let h = header(&stop, back, opened, RESTORE_WITHIN, Outcome::Reminded);
        assert_eq!(
            h,
            "[restart] The daemon restarted (down 5m00s). Nothing was restored automatically: a \
             session is restored only within 1h00m of the stop, and this one was opened 4h00m \
             after it. This is what you were running when it stopped — bring back only what still \
             matters:"
        );
        // A late open still under the threshold restores, and says the same thing.
        let soon = AT + 20 * 60_000;
        assert!(stop.restores(soon, RESTORE_WITHIN));
        let h = header(&stop, back, soon, RESTORE_WITHIN, Outcome::Restored);
        assert_eq!(h, "[restart] The daemon restarted (down 5m00s). Restored:");
    }

    /// The window the sentence names is the one in force, not the default: a daemon configured
    /// to 15m says 15m, and the gap is measured to the open.
    #[test]
    fn a_reminded_notice_names_the_configured_window_and_the_gap() {
        let stop = Stopped::At {
            at_ms: AT,
            estimated_from: None,
        };
        let window = Duration::from_secs(15 * 60);
        let h = header(
            &stop,
            AT + 4_000,
            AT + 2 * 3_600_000,
            window,
            Outcome::Reminded,
        );
        assert!(h.contains("only within 15m00s of the stop"), "{h}");
        assert!(h.contains("opened 2h00m after it"), "{h}");
        assert!(
            h.starts_with("[restart] The daemon restarted (down 4s)."),
            "{h}"
        );
    }

    /// No record of the stop is said as no record - *"these gray walls are hilarious"* about
    /// *"down 0s (estimated from nothing)"*. An unknown down time restores nothing: it counts as
    /// over the threshold.
    #[test]
    fn an_unknown_stop_is_said_to_be_unknown_and_restores_nothing() {
        let stop = Stopped::Unknown { seen_ms: 0 };
        let back = AT;
        assert!(!stop.restores(back, RESTORE_WITHIN));
        assert!(!stop.restores(back, Duration::from_secs(u64::MAX / 2_000)));
        assert_eq!(stop.at_ms(), None);
        let h = header(&stop, back, back, RESTORE_WITHIN, Outcome::Reminded);
        assert_eq!(
            h,
            "[restart] The daemon restarted (stop time unknown). Nothing was restored \
             automatically; this is what you were running when it stopped — bring back only what \
             still matters:"
        );
        for wrong in [
            "down for",
            "0s",
            "estimated from nothing",
            "no record said when",
        ] {
            assert!(!h.contains(wrong), "`{wrong}` in {h}");
        }
    }

    /// The latest stop among several daemons - and a daemon with no record seen alive after
    /// the latest known stop makes the stop unknown.
    #[test]
    fn the_latest_stop_is_known_only_when_no_unrecorded_daemon_outlived_it() {
        let at = |ms| Stopped::At {
            at_ms: ms,
            estimated_from: None,
        };
        assert_eq!(Stopped::latest([at(5), at(9), at(7)]), at(9));
        assert_eq!(
            Stopped::latest([at(9), Stopped::Unknown { seen_ms: 3 }]),
            at(9)
        );
        assert_eq!(
            Stopped::latest([at(9), Stopped::Unknown { seen_ms: 12 }]),
            Stopped::Unknown { seen_ms: 12 }
        );
        assert_eq!(
            Stopped::latest(std::iter::empty()),
            Stopped::Unknown { seen_ms: 0 }
        );
    }

    /// Restored is said only when something was - a restart under the threshold whose only
    /// record was a job was raised as `restart_restored`, *"restored where it could be"*, over a
    /// list that restored nothing.
    #[test]
    fn under_the_threshold_with_nothing_restored_the_notice_says_so() {
        let stop = Stopped::At {
            at_ms: AT,
            estimated_from: None,
        };
        let h = header(
            &stop,
            AT + 4_000,
            AT + 4_000,
            RESTORE_WITHIN,
            Outcome::NothingRestorable,
        );
        assert!(h.contains("down 4s"), "{h}");
        assert!(h.contains("Nothing could be restored"), "{h}");
        assert!(!h.contains("has been restored"), "{h}");
        assert_eq!(Outcome::NothingRestorable.code(), "restart_reminded");
        assert_eq!(Outcome::Reminded.code(), "restart_reminded");
        assert_eq!(Outcome::Restored.code(), "restart_restored");
    }

    fn job(command: &str, redirect: Option<&str>) -> JobRecord {
        JobRecord {
            handle: "j7".into(),
            command: command.into(),
            how: String::new(),
            state: "running".into(),
            produced: 0,
            elapsed_ms: 0,
            redirect: redirect.map(str::to_string),
        }
    }

    #[test]
    fn a_job_reminder_names_the_rerun_call_and_where_its_output_went() {
        let line = job_line(
            &job("cargo test > /tmp/t.log", Some("/tmp/t.log")),
            Some("tests"),
            Some("/w"),
            Some(125_000),
            &["`ship it`".into()],
        );
        assert!(line.contains("had run 2m05s"), "{line}");
        assert!(
            line.contains("Its output went to `/tmp/t.log`, which survives"),
            "{line}"
        );
        assert!(
            line.contains(
                r#"`bash {"command":"cargo test > /tmp/t.log","background":true,"cwd":"/w","slug":"tests"}`"#
            ),
            "{line}"
        );
        assert!(line.contains("Their wait is RELEASED"), "{line}");
        // Said once - the command is in the call that runs it again, not repeated before it.
        assert_eq!(line.matches("cargo test").count(), 1, "{line}");
    }

    /// A job the old daemon's orderly stop ended is said to have been ended, not to be
    /// possibly still running - pg-noop's `slt-sweep`, `killed by the daemon stopping`.
    #[test]
    fn a_job_the_stop_ended_is_said_to_be_ended() {
        let mut j = job("bash criterion-slt.sh > run0/out.txt 2>&1", None);
        j.state = STOPPED_WITH_THE_DAEMON.into();
        let line = job_line(&j, Some("slt-sweep"), Some("/w"), Some(468_000), &[]);
        assert!(
            line.contains("ENDED by the old daemon's orderly stop"),
            "{line}"
        );
        assert!(!line.contains("may have left it running"), "{line}");
        assert!(line.contains("`run0/out.txt`, which survives"), "{line}");
        assert!(line.contains("To run it again: `bash {"), "{line}");
    }

    /// The operator's job, whose record had no redirect (a running row written by a build
    /// before this one) - reminded as having *"wrote to no file of its own"* while its log was
    /// the one thing that survived. The reminder reads the command itself.
    #[test]
    fn a_job_reminder_finds_the_redirect_in_a_compound_command() {
        let cmd = "cd /tmp/letibot-scratch-1023699/harness && LETIDB_MODE=memory ./run_acceptance.sh > /tmp/acceptance-child.log 2>&1; echo \"exit=$?\"";
        let line = job_line(&job(cmd, None), None, None, None, &[]);
        assert!(
            line.contains("Its output went to `/tmp/acceptance-child.log`, which survives"),
            "{line}"
        );
        assert!(!line.contains("no file"), "{line}");
        assert!(!line.contains("is gone"), "{line}");

        let two = job_line(&job("a > x.log && b >> y.log", None), None, None, None, &[]);
        assert!(
            two.contains("Its output went to `x.log`, `y.log`, which survive"),
            "{two}"
        );

        let none = job_line(&job("cargo test", None), None, None, None, &[]);
        assert!(
            none.contains("Its output was in the old daemon's memory and is gone."),
            "{none}"
        );
    }

    /// A store in a scratch directory that removes itself.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let p = letibot_sessionlog::testing::scratch_root().join(format!(
                "{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }

        fn store(&self) -> PathBuf {
            self.0.join("sessions.db")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Another daemon is running while its life row says so: no stop, a heartbeat inside
    /// two beats, and a pid that is a live process. A stop, a stale beat and a pid that is gone all
    /// make it a gone daemon - the last one so a crash is not read as a daemon still working.
    #[test]
    fn a_daemon_is_running_only_while_it_stops_beating() {
        let dir = Scratch::new("restart-running");
        let store = Store::open(&dir.store()).unwrap();
        let now = letibot_sessionlog::registry::now_ms();
        // A live daemon: this test's own pid, a heartbeat a moment ago.
        let live = format!("d-{}-1-0", std::process::id());
        store.life_started(&live, now - 90_000).unwrap();
        store.life_beat(&live, now - 1_000).unwrap();
        assert!(
            super::is_running(&store, &live, now),
            "a beating daemon was read as gone, so its children are resumed twice"
        );
        // Its orderly stop: gone, whatever the beat says.
        let stopped = format!("d-{}-2-0", std::process::id());
        store.life_started(&stopped, now - 90_000).unwrap();
        store.life_stopped(&stopped, now - 60_000).unwrap();
        assert!(!super::is_running(&store, &stopped, now));
        // A crash: no stop, and the beat is older than two beats.
        store.life_started(&live, now - 600_000).unwrap();
        store.life_beat(&live, now - 61_000).unwrap();
        assert!(
            !super::is_running(&store, &live, now),
            "a daemon that stopped beating is gone"
        );
        // A beat that is fresh but a pid that is gone - killed hard, and the clock left behind. Pid
        // 0 is nobody's, so this is the one instance whose process cannot be read.
        let dead = "d-0-3-0";
        store.life_started(dead, now - 90_000).unwrap();
        store.life_beat(dead, now).unwrap();
        assert!(!super::is_running(&store, dead, now));
        // No life row at all: nothing says it is running.
        assert!(!super::is_running(&store, "no-instance-at-all", now));
    }

    /// The orderly stop is recorded although a newer build migrated the store meanwhile - the
    /// shared store moved past this build while it ran, and the stop goes through the
    /// connection the start opened, which still writes.
    #[test]
    fn the_stop_is_recorded_after_a_newer_build_migrated_the_store() {
        let dir = Scratch::new("lifeline-newer-schema");
        let registry = Registry::new();
        let life = Lifeline::start(Some(dir.store()), registry.clone());
        let reader = Store::open(&dir.store()).unwrap();
        // A newer daemon, started beside this one, migrates the shared file.
        reader
            .connection()
            .execute(
                "UPDATE schema_version SET version = ?1",
                [letibot_tokencore::store::SCHEMA_VERSION + 1],
            )
            .unwrap();
        reader
            .connection()
            .execute(
                "UPDATE schema_reader_floor SET min_reader = ?1 WHERE only_row = 1",
                [letibot_tokencore::store::SCHEMA_VERSION + 1],
            )
            .unwrap();
        // `open_private`, because a fresh open has to be a fresh CONNECTION: `Store::open`
        // hands back the handle `reader` is writing through, and a shared one never runs `migrate`.
        assert!(
            Store::open_private(&dir.store()).is_err(),
            "a fresh open refuses a store whose floor is past this build — the stop must not \
             depend on one"
        );
        registry.close();
        life.stopped();
        let l = reader
            .life(registry.instance())
            .unwrap()
            .expect("a life row");
        assert!(l.stopped_ms.is_some(), "the stop was not recorded: {l:?}");
    }

    /// A stop the force path cuts short is still a stop - the registry closes first on every
    /// stop asked for (a signal, a head's `Stop`), and a SIGKILL during the drain that follows
    /// never reaches [`Lifeline::stopped`]. The heartbeat writes the stop when it sees the close.
    #[test]
    fn the_stop_is_recorded_when_the_registry_closes_even_if_the_end_never_comes() {
        let dir = Scratch::new("lifeline-close");
        let registry = Registry::new();
        let life = Lifeline::start(Some(dir.store()), registry.clone());
        let reader = Store::open(&dir.store()).unwrap();
        let before = letibot_sessionlog::registry::now_ms();
        registry.close();
        let began = std::time::Instant::now();
        let stopped = loop {
            let l = reader
                .life(registry.instance())
                .unwrap()
                .expect("a life row");
            if let Some(at) = l.stopped_ms {
                break at;
            }
            assert!(
                began.elapsed() < Duration::from_secs(10),
                "the close was never written as the stop"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(stopped >= before, "{stopped} < {before}");
        assert_eq!(
            stop_of(&reader, registry.instance(), 0).at_ms(),
            Some(stopped),
            "the next daemon reads an orderly stop, not a heartbeat estimate"
        );
        // The orderly end, when it does come, says the exact time over it.
        std::thread::sleep(Duration::from_millis(5));
        life.stopped();
        let end = reader
            .life(registry.instance())
            .unwrap()
            .and_then(|l| l.stopped_ms)
            .unwrap();
        assert!(end >= stopped, "{end} < {stopped}");
    }
}

#[cfg(test)]
mod journal_tests {
    use super::*;
    use letibot_sessionlog::hub::OwnWork;
    use letibot_sessionlog::{CommandKind, Hub};

    /// A prompt accepted and not started survives a daemon kill: a new hub on the same store gets
    /// it back, and once taken it is not put back a second time.
    #[test]
    fn a_queued_prompt_survives_a_killed_daemon() {
        let dir = std::env::temp_dir().join(format!("letibot-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.db");
        let first = Hub::new("s-killed");
        assert_eq!(StoreJournal::attach(&first, &path), 0);
        let head = first.attach("tui", "dead", Default::default(), 0);
        first.submit(
            &head.head_id,
            "c1",
            0,
            CommandKind::Prompt {
                text: "carry on after the restart".into(),
                attachments: Vec::new(),
            },
        );
        assert!(first.journal_settled(std::time::Duration::from_secs(30)));
        // kill -9: the first hub is simply gone.
        drop(first);
        let second = Hub::new("s-killed");
        assert_eq!(StoreJournal::attach(&second, &path), 1);
        let OwnWork::Command(c) = second.take_own_work() else {
            panic!("the prompt was not put back");
        };
        assert!(
            matches!(&c.kind, CommandKind::Prompt { text, .. } if text == "carry on after the restart"),
            "{c:?}"
        );
        assert!(second.journal_settled(std::time::Duration::from_secs(30)));
        let third = Hub::new("s-killed");
        assert_eq!(
            StoreJournal::attach(&third, &path),
            0,
            "taken, so not put back again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
