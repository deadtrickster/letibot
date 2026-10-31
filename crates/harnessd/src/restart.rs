//! **A daemon restart, made as painless as it can be** — what a session was running when its
//! daemon (harnessd) stopped, brought back or reminded when a daemon opens it again.
//!
//! The operator, 2026-10-12, in two steps. First: *"and here is one more thing about restart - we
//! should bring subagents back - i cant afford to wait till they finish. we should record them
//! (session persist anyway) and restart automatically"*. Then, widening it: *"on restoring agents -
//! in fact to make restarts as much painless as possible - by default we restore agents, monitors,
//! and remind the model about jobs. Now, if some threshold passed - say more than 1 hours after
//! the stop, we start to simply remind the model stating that time passed and this is what you wre
//! running and how to restart that."* And the scope of it: *"this is the case for when harnessd
//! stopped of course"*.
//!
//! # Only a daemon stop triggers it
//!
//! Every record this reads is stamped with the daemon instance that wrote it
//! (`Registry::instance`). A session opened by the daemon that already holds its records — a head
//! attaching after hours of idle, a `/resume`, a parked child coming back — finds nothing that is
//! not its own and does nothing. Only records written by a daemon that is no longer running are
//! acted on, and only once: each is re-stamped or closed as it is handled.
//!
//! # How long the daemon was down
//!
//! From the previous daemon's STOP to this daemon's START (`Registry::started_ms`), read from the
//! `daemon_life` row the previous daemon kept ([`Lifeline`]): an orderly stop writes its exact
//! time; after a crash the last heartbeat is the latest moment the daemon is KNOWN to have been
//! alive (it beats every [`BEAT`]), so the down time is an estimate that can be up to one beat too
//! long, and the notice says so.
//!
//! **The threshold is applied to a different interval, and the difference is the point** (R91.8):
//! from the STOP to the moment the session is OPENED ([`Stopped::restores`]). The two coincide for
//! a session the daemon opens as it starts and are hours apart for one opened later — a head
//! attaching, a `/resume`, a picker row — and the operator's rule is about the second: *"if some
//! threshold passed - say more than 1 hours after the stop, we start to simply remind"*. Measuring
//! to the daemon's start let a session opened four hours after the stop resume its children on the
//! tree they were on four hours before. When the open is more than a minute after the daemon's
//! return, the notice names both intervals.
//!
//! **With no life row at all the stop is UNKNOWN, and is said to be.** The operator, pasting the
//! notice the first start on a new build wrote — *"the daemon was down for 0s (stopped at …,
//! estimated from nothing (no record said when); back at …)"* — *"these gray walls are
//! hilarious"*. A daemon that kept no record (a build before this one, or one that crashed before
//! its first heartbeat) did not stop "0s ago"; nobody knows when it stopped, and the notice says
//! exactly that ([`Stopped::Unknown`]).
//!
//! # Under the threshold (`Config::restore_within`, default one hour)
//!
//! * **subagents** that were mid-task are resumed (`crate::subagent_record`);
//! * **monitors** that were live are declared again with the call that declared them — same name,
//!   same condition, same owner, the TTL they were declared with — and one whose condition cannot
//!   be re-established (a job that is gone, a pid that is gone) is reported, not dropped;
//! * **jobs** are NOT re-run — a process cannot be resumed and running a command twice can do harm
//!   — and the model is reminded of each ([`job_line`]).
//!
//! # Over it — or not known to be under it
//!
//! Nothing is restored. **An unknown down time counts as over the threshold**: restoring is the
//! act that needs the stop to have been recent (a subagent resumed hours later works on a tree
//! that moved, a monitor re-armed watches for something long past), and reminding is what is
//! safe however long it was — the model is told what was running and the one call that brings
//! each back. Jobs are only ever reminded, so for them it changes nothing. The model gets one reminder of everything that was running, with the one
//! call that brings each back (`task_resume`, `monitor`, `bash`), and is told to decide what still
//! matters.
//!
//! Either way the reminder is a turn (it is news the model must act on) and the operator sees a
//! warning on the session listing what happened: `restart_restored` when something WAS restored
//! (a subagent resumed or delivered, a monitor re-armed), `restart_reminded` otherwise — a
//! restart under the threshold whose only record was a job restored nothing, and saying
//! *"restored where it could be"* over a list of reminders was wrong ([`Outcome`]).
//!
//! # And a restart with nothing running is said too
//!
//! The operator, 2026-10-30, after one restart had told the model something and the next had said
//! nothing: *"mb always tell agent about restart? with the current time lol"*. That is not a
//! courtesy, it is a clock: the model's sense of time went through a stop and a start, and only
//! the daemon knows when. A session with nothing to restore therefore gets one sentence of its own
//! ([`nothing_running`]), said once per (session, restart):
//!
//! ```text
//! [restart] the daemon came back at 2026-10-12 08:10:56 UTC (down 1s, stopped at 2026-10-12
//! 08:10:55 UTC): nothing was running
//! ```
//!
//! **The return time leads it**, because the time is the whole of what this case is for; what was
//! running is said too, and it is *nothing*. A stop no record dates is still said to be unknown,
//! in this sentence as in the others.
//!
//! **And the two cases take different doors, because they are different news.** Something was
//! running ⇒ there is work to continue, and the wake that continues it carries the notice. Nothing
//! was running ⇒ there is nothing to continue, and the sentence is for the clock alone: it is
//! filed into the session and read at the model's next turn (or at the next step boundary of a
//! turn already running), never as a turn of its own. A generation spent to say *"the daemon came
//! back at 21:28"* is a generation spent on a sentence.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{
    JobRecord, Store, StoredMonitor, TodoCondition, TodoItem, TodoNeed, TodoStatus,
};

/// How often a daemon writes its heartbeat — the bound on how wrong a crash's down time can be.
pub const BEAT: Duration = Duration::from_secs(30);

/// The default restore threshold — the operator's *"say more than 1 hours after the stop"*.
pub const RESTORE_WITHIN: Duration = Duration::from_secs(3600);

/// **This daemon's life, written down** — started now, a heartbeat every [`BEAT`] until the
/// registry closes, and the stop: when the registry closes (the heartbeat sees it), and again
/// with its exact time when [`Lifeline::stopped`] is called at the end of the orderly path.
///
/// **The stop is written through a connection opened at the START**, never a fresh one. pg-noop's
/// daemon `d-840951`, stopped on purpose on 2026-10-15, logged *"this daemon's stop was not
/// recorded (store schema is version 30; this build knows 29)"*: the store is shared by every
/// daemon on the box, a newer build had migrated it while this one ran, and `Store::open` refuses
/// a schema newer than its own. The heartbeat, on the connection it opened at the start, had kept
/// beating until 10:37:44 — so the next daemon read a crash, and with the session's other records
/// that came out as a stop nobody could date. A connection opened before the migration still
/// writes `daemon_life`, which no migration changes the shape of.
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
                    // The heartbeat's own connection, opened here and now for the same reason.
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

    /// **The orderly stop**, said with its exact time.
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

/// **A beat every [`BEAT`] while the daemon serves — and the stop, the moment it stops serving.**
///
/// Every way a daemon is stopped on purpose closes its registry first: SIGINT and SIGTERM
/// (`Daemon::catch_signals`), a head's `Stop` frame, the merge queue's stop. What follows the
/// close is bounded but not short — the merge queue joined, the VMs brought down, the children
/// drained, five seconds each at worst — and a head that forces the stop sends SIGKILL five
/// seconds after SIGTERM (`letibot --stop --force`, the head's own force path). A stop written
/// only at the end of that path is lost to the SIGKILL, and the next daemon reads a crash. So the
/// stop is written here as soon as the close is seen, unless the orderly path has already said
/// it; [`Lifeline::stopped`] writes the exact time over it when the path does reach its end.
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

/// **When the previous daemon stopped**: a fact, an estimate, or not known at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    At {
        at_ms: u64,
        /// `None` for an orderly stop's own time; otherwise what the time was estimated from.
        estimated_from: Option<&'static str>,
    },
    /// **No record says when** — the daemon kept no life row (a build before this one, or a
    /// crash before its first heartbeat). `seen_ms` is the latest moment one of its records was
    /// written (`0` for none): it was alive then, so the stop is no earlier — a lower bound, used
    /// only to close a monitor whose TTL had run out before it, never to state a down time.
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

    /// The latest moment the previous daemon is known to have been alive — the stop when it is
    /// known, the last record otherwise.
    pub fn no_earlier_than_ms(&self) -> u64 {
        match self {
            Stopped::At { at_ms, .. } => *at_ms,
            Stopped::Unknown { seen_ms } => *seen_ms,
        }
    }

    /// **Whether what it was running is restored** — under `threshold` when the down time is
    /// known; never when it is not (see the module docs).
    ///
    /// `opened_ms` is **when the session is being opened**, not when the daemon came up (R91.8).
    /// The two are the same moment for a session the daemon opens as it starts, and they are
    /// hours apart for one opened later — a head attaching to a session after a restart, a
    /// `/resume`, a picker row — and it is the SECOND that the operator's rule is about: *"if
    /// some threshold passed - say more than 1 hours after the stop, we start to simply remind"*.
    /// Measuring stop → daemon start let a session opened four hours after the stop resume its
    /// children, on the tree they were on four hours ago, and the notice said *"the daemon was
    /// down for 5m — under the threshold, so restored"* about a session nobody had looked at
    /// since. The harm the threshold exists to prevent is a stale tree, and that is measured
    /// from the stop to the resume.
    pub fn restores(&self, opened_ms: u64, threshold: Duration) -> bool {
        self.at_ms()
            .is_some_and(|at| Duration::from_millis(opened_ms.saturating_sub(at)) < threshold)
    }

    /// **The stop of the daemon that went last, among several** — the latest known stop, unless
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

/// The stop of `instance`, read from its life row; [`Stopped::Unknown`] when it has none,
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

/// **What a restart did**, which decides the notice's first line and the warning's code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Under the threshold, and at least one thing was brought back.
    Restored,
    /// Under the threshold, but nothing could be or needed to be brought back — only jobs, which
    /// are never re-run, or monitors that could not be re-armed.
    NothingRestorable,
    /// Over the threshold, or the down time is unknown: nothing was even tried.
    Reminded,
    /// **There was nothing to try** — no subagent, monitor or job was running at the stop. The
    /// sentence is [`nothing_running`]'s, and it is said for the clock alone.
    Nothing,
}

impl Outcome {
    /// The hub warning this outcome is raised as.
    pub fn code(self) -> &'static str {
        match self {
            Outcome::Restored => "restart_restored",
            Outcome::NothingRestorable | Outcome::Reminded => "restart_reminded",
            // **Its own word**, because it is its own sentence: a head that read this as
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

/// **A session opened this long after its daemon came back is told so** — the restart notice's
/// clock is otherwise the daemon's, and a session nobody looked at for hours would read *"the
/// daemon was down for 5m"* as if that were the whole of the gap.
pub const OPENED_LATE_MS: u64 = 60_000;

/// **The first line of every restart notice**: how long the daemon was down (or that nobody
/// knows), against which threshold, and what that meant.
///
/// `back_ms` is when the daemon came back; `opened_ms` is when the session is being opened, which
/// is what the threshold is applied to (R91.8 — see [`Stopped::restores`]). When the two are far
/// enough apart to matter, the same sentence names both, so the clause *"under the … restore
/// threshold"* can never be read as a claim about a session that was away for hours.
pub fn header(
    stop: &Stopped,
    back_ms: u64,
    opened_ms: u64,
    threshold: Duration,
    outcome: Outcome,
) -> String {
    // **The empty restart is its own sentence**, not this one with an empty list: there is no
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
            return format!(
                "[restart] the daemon restarted at {}; when the previous one stopped is unknown \
                 (it kept no record — a build before this one, or a crash before its first \
                 heartbeat). An unknown down time counts as longer than the {} restore threshold, \
                 so NOTHING was restored automatically. This is what you were running; decide \
                 what still matters and bring back only that:",
                crate::subagent_record::utc(back_ms),
                span(threshold)
            );
        }
    };
    let down = Duration::from_millis(back_ms.saturating_sub(at_ms));
    let waited = Duration::from_millis(opened_ms.saturating_sub(at_ms));
    let estimate = match estimated_from {
        Some(from) => format!(", estimated from {from}"),
        None => String::new(),
    };
    // **The open is the moment the threshold was measured at** (R91.8), so when it is later than
    // the daemon's own return the sentence says both — the fact the person is living through
    // (`opened`) and the fact the daemon's row holds (`back`).
    let opened = match opened_ms.saturating_sub(back_ms) >= OPENED_LATE_MS {
        true => format!("; this session is opened {} after the stop", span(waited)),
        false => String::new(),
    };
    let when = format!(
        "stopped at {}{estimate}; back at {}{opened}",
        crate::subagent_record::utc(at_ms),
        crate::subagent_record::utc(back_ms)
    );
    match outcome {
        Outcome::Restored => format!(
            "[restart] the daemon was down for {} ({when}) — under the {} restore threshold, so \
             what this session was running has been restored where it could be:",
            span(down),
            span(threshold)
        ),
        Outcome::NothingRestorable => format!(
            "[restart] the daemon was down for {} ({when}) — under the {} restore threshold, but \
             nothing could be restored automatically. This is what you were running:",
            span(down),
            span(threshold)
        ),
        Outcome::Reminded => format!(
            "[restart] the daemon was down for {} ({when}) — longer than the {} restore \
             threshold, so NOTHING was restored automatically. This is what you were running \
             when it stopped; decide what still matters and bring back only that:",
            span(down),
            span(threshold)
        ),
        // Unreachable — the empty restart returned above — and written out rather than as an
        // `unreachable!()` so that a later arm cannot make this function lie.
        Outcome::Nothing => nothing_running(stop, back_ms),
    }
}

/// **A restart that had nothing to restore, said anyway** — the operator, 2026-10-30: *"mb always
/// tell agent about restart? with the current time lol"*.
///
/// The return time leads it because the time IS this case: the model's sense of how long it was
/// away is the one fact only the daemon has, and the rest of the sentence is what was running —
/// nothing. A stop no record dates is said to be unknown, here as everywhere else ([`Stopped`]).
pub fn nothing_running(stop: &Stopped, back_ms: u64) -> String {
    let down = match stop {
        Stopped::At {
            at_ms,
            estimated_from,
        } => {
            let span = span(Duration::from_millis(back_ms.saturating_sub(*at_ms)));
            let estimate = match estimated_from {
                Some(from) => format!(", estimated from {from}"),
                None => String::new(),
            };
            format!(
                "down {span}, stopped at {}{estimate}",
                crate::subagent_record::utc(*at_ms)
            )
        }
        // **Not "down 0s"**: nobody knows when it stopped, and a restart with nothing running is
        // exactly the one with no record to read it from — see the module docs.
        Stopped::Unknown { .. } => {
            "down unknown, and no record says when the previous one stopped".to_string()
        }
    };
    format!(
        "[restart] the daemon came back at {} ({down}): nothing was running",
        crate::subagent_record::utc(back_ms)
    )
}

/// **The call that declares a monitor again**, as the model would write it.
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

/// **The named scope a monitor was owned by**, when it was — not `turn`, not `session`, not left
/// to the default. Such a scope is something the model set up (a background job started under
/// that name) and only the daemon that stopped knew about it, so a restart cannot tell what it
/// was for or whether what ran in it still runs.
pub fn named_owner(m: &StoredMonitor) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&m.args_json).ok()?;
    let owner = v.get("owner")?.as_str()?.trim();
    (!owner.is_empty() && owner != "turn" && owner != "session").then(|| owner.to_string())
}

/// **A monitor owned by a named scope, as the model is told** — never re-armed on its own: the
/// operator, *"if scopes are involved we should tell model, who knows wtf was setup and how
/// relevant it still is"*.
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

/// **The notice the model gets**: the header, a line per thing that was running, and — when
/// nothing was brought back — the one sentence of what to do about it.
pub fn notice(header: &str, lines: &[String], outcome: Outcome) -> String {
    // **One sentence, and nothing after it**: there is no list, and the closing *"nothing above is
    // running now"* would be a sentence about a list that is not there.
    if outcome == Outcome::Nothing {
        return header.to_string();
    }
    let mut text = header.to_string();
    if !lines.is_empty() {
        text.push('\n');
        text.push_str(&lines.join("\n"));
    }
    if outcome != Outcome::Restored {
        text.push_str(
            "\nNothing above is running now. Bring back only what still matters: \
             `task_resume` for a subagent, `monitor` for a watch, `bash` for a job.",
        );
    }
    text
}

/// **The hub warning's detail** — the notice's first line and one line per thing ([`said`]).
/// The TUI's `restart_card` reads it as one line.
pub fn warning_detail(notice: &str, said: &[String]) -> String {
    let head = notice.lines().next().unwrap_or("");
    // **A restart with nothing in it is one line** — no trailing newline, because there is no
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

/// **What the operator's warning says of each thing** — one line each, the daemon's log too.
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

/// **What a job row reads when the old daemon's orderly stop ended it** — `Registry::close` runs
/// every session's `end_running("the daemon stopping")` before the process exits, and the row
/// says `killed by` that. Such a job was running at the stop as surely as one a crash left
/// `running`, and is reminded the same way ([`job_line`]).
pub const STOPPED_WITH_THE_DAEMON: &str = "killed by the daemon stopping";

/// **One background job that was running at the stop, as the model is reminded of it.**
///
/// `ran_ms` is how long it had run when the daemon stopped. `waiting` is the open rows that waited
/// on it — named, with what happened to their wait.
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
    // **One sentence of why**, and the command said once — in the call that runs it again.
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
    // **Where its output went, read out of the command** — the record's own `redirect` was
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

/// **One background job that had ENDED at the stop and whose ending never reached the model**
/// (TODO.md R91.5) — held under R79 while a turn ran, or queued, when the daemon went down. The
/// row's own record is all that is left: its state, its size, where its output went if anywhere.
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

/// **The open rows of a board that wait on `handle` (or its slug)** — by `when: job` or by a
/// `wait_on` edge — as the reminder names them.
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

/// **A monitor declaration as the harness records it** — the `monitor` tool, wrapped so a
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
    // **A GitHub watch's default is a day**, not a monitor's five minutes — recorded as five, a
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

    /// **A GitHub watch is recorded with its own default life**, so a restart inside it re-arms it.
    #[test]
    fn a_github_watch_is_recorded_for_a_day() {
        assert_eq!(
            super::ttl_of(&serde_json::json!({"github": {"url": "u", "until": "checks"}})),
            24 * 3600 * 1000
        );
        assert_eq!(super::ttl_of(&serde_json::json!({"timer_ms": 5})), 300_000);
    }

    /// **Only a scope the model named is a named owner** — `turn`, `session` and no owner at all
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

    /// **A restart with nothing running is said anyway, and the time leads it** — the operator,
    /// 2026-10-30: *"mb always tell agent about restart? with the current time lol"*. The whole
    /// sentence is pinned, because the return time is the one fact the case exists for and a
    /// fragment test would let it go missing.
    #[test]
    fn an_empty_restart_says_when_the_daemon_came_back() {
        let back = AT + 1_000;
        let orderly = Stopped::At {
            at_ms: AT,
            estimated_from: None,
        };
        assert_eq!(
            nothing_running(&orderly, back),
            "[restart] the daemon came back at 2026-10-12 12:00:01 UTC (down 1s, stopped at \
             2026-10-12 12:00:00 UTC): nothing was running"
        );
        // A crash: the time is the last heartbeat, and the sentence says so rather than claiming
        // an orderly stop.
        let crash = Stopped::At {
            at_ms: AT,
            estimated_from: Some("its last heartbeat (it did not stop in an orderly way)"),
        };
        assert_eq!(
            nothing_running(&crash, back),
            "[restart] the daemon came back at 2026-10-12 12:00:01 UTC (down 1s, stopped at \
             2026-10-12 12:00:00 UTC, estimated from its last heartbeat (it did not stop in an \
             orderly way)): nothing was running"
        );
        // And a stop nobody dated is still said to be unknown — not `down 0s`.
        assert_eq!(
            nothing_running(&Stopped::Unknown { seen_ms: 0 }, back),
            "[restart] the daemon came back at 2026-10-12 12:00:01 UTC (down unknown, and no \
             record says when the previous one stopped): nothing was running"
        );
    }

    /// **The empty case is one sentence and nothing else** — no list, and none of the closing
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
        // **And the warning's detail is that one line**, with no empty second line under it.
        assert_eq!(warning_detail(&text, &[]), text);
        assert_eq!(Outcome::Nothing.code(), "restart_nothing");
    }

    /// **The case with records is untouched** — same opening, same threshold clause, same list.
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
        assert!(
            h.starts_with("[restart] the daemon was down for 4s (stopped at "),
            "{h}"
        );
        assert!(h.contains("back at 2026-10-12 12:00:04 UTC"), "{h}");
        let text = notice(
            &h,
            &["- job `j9` was reminded".into()],
            Outcome::NothingRestorable,
        );
        assert!(text.ends_with("`bash` for a job."), "{text}");
    }

    #[test]
    fn the_header_says_how_long_and_which_side_of_the_threshold() {
        let stop = Stopped::At {
            at_ms: AT,
            estimated_from: None,
        };
        let back = AT + 5 * 60_000;
        assert!(stop.restores(back, RESTORE_WITHIN));
        let h = header(&stop, back, back, RESTORE_WITHIN, Outcome::Restored);
        assert!(h.contains("down for 5m00s"), "{h}");
        assert!(h.contains("under the 1h00m restore threshold"), "{h}");
        let crash = Stopped::At {
            at_ms: AT,
            estimated_from: Some("its last heartbeat (it did not stop in an orderly way)"),
        };
        let late = AT + 2 * 3_600_000;
        assert!(!crash.restores(late, RESTORE_WITHIN));
        let h = header(&crash, late, late, RESTORE_WITHIN, Outcome::Reminded);
        assert!(h.contains("down for 2h00m"), "{h}");
        assert!(h.contains("estimated from its last heartbeat"), "{h}");
        assert!(h.contains("NOTHING was restored"), "{h}");
    }

    /// **The threshold is measured to the OPEN, not to the daemon's return** (R91.8) — the
    /// operator's *"more than 1 hours after the stop"*, which is the session's gap and not the
    /// daemon's. A daemon that came back five minutes after the stop and a session nobody opened
    /// for four hours: under the hour by the daemon's clock, four hours late by the session's,
    /// and the child on a tree nothing has touched since.
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
        assert!(h.contains("down for 5m00s"), "{h}");
        assert!(
            h.contains("; this session is opened 4h00m after the stop"),
            "the sentence has to name the interval the threshold used: {h}"
        );
        assert!(h.contains("longer than the 1h00m restore threshold"), "{h}");
        assert!(h.contains("NOTHING was restored"), "{h}");
        // A late open still under the threshold says when it was, and still restores.
        let soon = AT + 20 * 60_000;
        assert!(stop.restores(soon, RESTORE_WITHIN));
        let h = header(&stop, back, soon, RESTORE_WITHIN, Outcome::Restored);
        assert!(
            h.contains("this session is opened 20m00s after the stop"),
            "{h}"
        );
        assert!(h.contains("under the 1h00m restore threshold"), "{h}");
        // And the ordinary case — opened as the daemon starts — has no such clause at all.
        let h = header(&stop, back, back, RESTORE_WITHIN, Outcome::Restored);
        assert!(!h.contains("this session is opened"), "{h}");
    }

    /// **No record of the stop is said as no record** — the operator, of *"the daemon was down
    /// for 0s (stopped at 2026-10-12 08:10:56 UTC, estimated from nothing (no record said
    /// when); back at 2026-10-12 08:10:56 UTC)"*: *"these gray walls are hilarious"*. And an
    /// unknown down time restores nothing: it counts as over the threshold.
    #[test]
    fn an_unknown_stop_is_said_to_be_unknown_and_restores_nothing() {
        let stop = Stopped::Unknown { seen_ms: 0 };
        let back = AT;
        assert!(!stop.restores(back, RESTORE_WITHIN));
        assert!(!stop.restores(back, Duration::from_secs(u64::MAX / 2_000)));
        assert_eq!(stop.at_ms(), None);
        let h = header(&stop, back, back, RESTORE_WITHIN, Outcome::Reminded);
        assert!(
            h.starts_with("[restart] the daemon restarted at 2026-"),
            "{h}"
        );
        assert!(
            h.contains(
                "when the previous one stopped is unknown (it kept no record — a build before \
                 this one, or a crash before its first heartbeat)"
            ),
            "{h}"
        );
        assert!(
            h.contains("An unknown down time counts as longer than the 1h00m restore threshold"),
            "{h}"
        );
        assert!(h.contains("NOTHING was restored automatically"), "{h}");
        for wrong in [
            "down for",
            "0s",
            "estimated from nothing",
            "no record said when",
        ] {
            assert!(!h.contains(wrong), "`{wrong}` in {h}");
        }
    }

    /// **The latest stop among several daemons** — and a daemon with no record seen alive after
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

    /// **Restored is said only when something was** — a restart under the threshold whose only
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
        assert!(h.contains("down for 4s"), "{h}");
        assert!(
            h.contains("under the 1h00m restore threshold, but nothing could be restored"),
            "{h}"
        );
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
        // **Said once** — the command is in the call that runs it again, not repeated before it.
        assert_eq!(line.matches("cargo test").count(), 1, "{line}");
    }

    /// **A job the old daemon's orderly stop ended is said to have been ended**, not to be
    /// possibly still running — pg-noop's `slt-sweep`, `killed by the daemon stopping`.
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

    /// **The operator's job, whose record had no redirect** (a running row written by a build
    /// before this one) — reminded as having *"wrote to no file of its own"* while its log was
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

    /// **The orderly stop is recorded although a newer build migrated the store meanwhile** —
    /// pg-noop's `d-840951`, stopped on purpose on 2026-10-15, logged *"this daemon's stop was
    /// not recorded (store schema is version 30; this build knows 29)"*, and the next daemon read
    /// a crash. The store every daemon on the box shares moved to a newer schema while this one
    /// ran; the stop goes through the connection the start opened, which still writes.
    ///
    /// **What makes a fresh open refuse is the store's READER FLOOR, and the fixture raises it on
    /// purpose** (2026-10-26). A bare version bump is now the additive case — a newer build that
    /// only added something leaves the floor where it was, and an older daemon goes on serving
    /// (`letibot_tokencore`'s `store::floor`). The migration this test stands for is the other
    /// kind: it took away something an older reader reads, so it moved the floor past this build
    /// and a fresh open is refused — which is exactly the state the stop has to survive.
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
        assert!(
            Store::open(&dir.store()).is_err(),
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

    /// **A stop the force path cuts short is still a stop** — the registry closes first on every
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
