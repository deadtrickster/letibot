//! What a session tells its model between turns: a fired monitor, an ended background job or subagent, and the rule for holding a batch until a break can carry it.
//!
//! The decision is `crate::delivery`'s, pure and tested there. This file reads the session's facts and words them.

use super::*;
use letibot_tools::exec::monitor::{Fired, Firing};

/// One monitor's news in one delivery: what it last said, and how many of its firings the line
/// stands for. A type rather than a `Vec<String>` so the fold is a thing a test can assert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FiringLine {
    pub name: String,
    pub said: String,
    /// Firings this line stands for BESIDES the one whose words it carries.
    pub folded: usize,
    /// Whether this line is a monitor's ENDING rather than something it saw.
    pub ending: bool,
}

impl FiringLine {
    fn words(&self) -> String {
        match self.folded {
            0 => self.said.clone(),
            1 => format!("{} (and one firing since)", self.said),
            n => format!("{} (and {n} firings since)", self.said),
        }
    }
}

/// A burst of firings folded to one line per monitor: the newest words and the count.
/// Folded per name and kind in first-seen order, so one chatty monitor cannot push out another's news, and an ending keeps its own line (*and nothing is watching now*).
pub(crate) fn fold_firings(fired: &[Firing]) -> Vec<FiringLine> {
    let mut out: Vec<FiringLine> = Vec::new();
    for f in fired {
        let Some(said) = said_by(f) else { continue };
        let same = out
            .iter_mut()
            .find(|l| l.name == f.name && l.ending == !f.fired.happened());
        match same {
            Some(line) => {
                line.said = said;
                line.folded += 1;
            }
            None => out.push(FiringLine {
                name: f.name.clone(),
                said,
                folded: 0,
                ending: !f.fired.happened(),
            }),
        }
    }
    out
}

/// What one firing says, in the model's words, or `None` for the two endings that are the model's
/// own act or nobody's business.
fn said_by(f: &Firing) -> Option<String> {
    match &f.fired {
        Fired::Fired { why, .. } => Some(format!("[monitor] `{}` fired: {why}.", f.name)),
        Fired::Expired { .. } => Some(format!("[monitor] `{}` expired without firing.", f.name)),
        Fired::Cancelled { by, .. } if by != MODEL_RETIRE => {
            Some(format!("[monitor] `{}` was retired: {by}.", f.name))
        }
        Fired::Cancelled { .. } | Fired::OwnerEnded { .. } => None,
    }
}

/// The delivery the model is handed: one line a monitor, folded.
pub(crate) fn monitor_notice(fired: &[letibot_tools::exec::monitor::Firing]) -> Option<String> {
    let lines: Vec<String> = fold_firings(fired).iter().map(|l| l.words()).collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The row a delivery becomes: a `Notice`, so nothing has to remember to attribute it. A `User`
/// row's `speaker` defaults to the operator, and a row written without one reads as their words.
pub(crate) fn notice_row(
    kind: letibot_transcript::NoticeKind,
    text: impl Into<String>,
) -> TranscriptItem {
    TranscriptItem::Notice {
        kind,
        text: text.into(),
    }
}

/// The firings `monitor_notice` was composed from, as the facts the `notice` hook is handed, so a
/// layer can build a different sentence out of the same news rather than reword this one.
pub(crate) fn monitor_facts(fired: &[letibot_tools::exec::monitor::Firing]) -> serde_json::Value {
    // Folded, because a layer handed twenty rows for one monitor would write twenty sentences where
    // the composed line writes one - and the count is what tells it the burst happened.
    let rows: Vec<serde_json::Value> = fold_firings(fired)
        .into_iter()
        .map(|l| {
            let f = fired.iter().rev().find(|f| {
                f.name == l.name && said_by(f).is_some() && f.fired.happened() != l.ending
            });
            serde_json::json!({
                "name": l.name,
                "watch": f.map(|f| f.watch.clone()),
                "ended": f.map(|f| f.fired.word()),
                "said": l.said,
                "folded": l.folded,
                "ending": l.ending,
            })
        })
        .collect();
    serde_json::json!({"fired": rows})
}

/// How many ended monitors the pane lists - the newest. A session that ran a repeating watch
/// all day would otherwise ship its whole history with every job-table refill.
pub const MONITORS_ENDED_SHOWN: usize = 20;

/// What reaps a monitor, as a person says it - `session`, `turn`, or a named scope's own name;
/// the scope's id (`session.s0`) is the daemon's, not the reader's.
pub(crate) fn owner_words(s: &letibot_tools::exec::scope::ScopeId) -> String {
    use letibot_tools::exec::scope::ScopeKind;
    match s.kind {
        ScopeKind::Session => "session".into(),
        ScopeKind::Turn => "turn".into(),
        ScopeKind::Explicit => format!("scope `{}`", s.name),
    }
}

/// Who armed a monitor, as a person says it - `turn pr-watch` was the harness on a
/// `gh pr create`, `turn restart` a restart re-arming its stored call; a model's own turn is
/// said as it is.
pub(crate) fn declared_words(by: &str) -> String {
    match by {
        "turn pr-watch" => "the harness, when the pull request was opened".into(),
        "turn restart" => "a restart, from the call that armed it".into(),
        other => other.to_string(),
    }
}

/// [`Harness::monitor_entries`] over its inputs, so the rows are assertable without a session.
pub fn monitor_entries_of(
    live: &[std::sync::Arc<letibot_tools::exec::monitor::Monitor>],
    firings: &[letibot_tools::exec::monitor::Firing],
    now_ms: u64,
) -> Vec<letibot_sessionlog::protocol::MonitorEntry> {
    use letibot_tools::exec::monitor::Fired;
    let fired_count = |name: &str| {
        firings
            .iter()
            .filter(|f| f.name == name && matches!(f.fired, Fired::Fired { .. }))
            .count() as u32
    };
    let mut out: Vec<letibot_sessionlog::protocol::MonitorEntry> = live
        .iter()
        .map(|m| letibot_sessionlog::protocol::MonitorEntry {
            name: m.name.clone(),
            watching: m.watch.describe(),
            owner: owner_words(&m.owner),
            declared_by: declared_words(&m.declared_by),
            repeat: m.repeats(),
            live: true,
            ends_at_ms: now_ms + m.remaining().map_or(0, |r| r.as_millis() as u64),
            fired: fired_count(&m.name),
            ended: String::new(),
        })
        .collect();
    // The last event of each name that is no longer live is how it ended; a name armed again
    // later is live and listed above, and its earlier end is history the pane does not repeat.
    let mut ended: Vec<&letibot_tools::exec::monitor::Firing> = Vec::new();
    for f in firings.iter().rev() {
        if live.iter().any(|m| m.name == f.name) || ended.iter().any(|e| e.name == f.name) {
            continue;
        }
        // A repeating monitor's firing is not its end; its end is the record that took it.
        ended.push(f);
        if ended.len() == MONITORS_ENDED_SHOWN {
            break;
        }
    }
    ended.reverse();
    out.extend(
        ended
            .into_iter()
            .map(|f| letibot_sessionlog::protocol::MonitorEntry {
                name: f.name.clone(),
                watching: f.watch.clone(),
                owner: String::new(),
                declared_by: declared_words(&f.declared_by),
                repeat: false,
                live: false,
                ends_at_ms: 0,
                fired: fired_count(&f.name),
                ended: f.fired.word(),
            }),
    );
    out
}

#[cfg(test)]
mod monitor_rows {
    use super::*;
    use letibot_tools::exec::monitor::{DEFAULT_TTL, Monitors, Watch};
    use letibot_tools::exec::scope::{ScopeId, ScopeKind};

    /// The monitors pane's rows - a live repeating watch is one row with its firing count and
    /// its deadline; a one-shot that fired and one the operator retired are ended rows saying how,
    /// in the order they ended.
    #[test]
    fn live_monitors_come_first_and_ended_ones_say_how_they_ended() {
        let dir = std::env::temp_dir().join(format!("letibot-monrows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let scope = ScopeId {
            kind: ScopeKind::Session,
            name: "s".into(),
            path: dir.clone(),
        };
        let ms = Arc::new(Monitors::new());
        let watch = |f: &str| Watch::Path(dir.join(f));
        ms.declare(
            "tick",
            scope.clone(),
            watch("a"),
            None,
            "turn 1",
            DEFAULT_TTL,
            true,
        )
        .unwrap();
        ms.declare(
            "once",
            scope.clone(),
            watch("b"),
            None,
            "turn 2",
            DEFAULT_TTL,
            false,
        )
        .unwrap();
        ms.declare(
            "gone",
            scope.clone(),
            watch("c"),
            None,
            "turn 3",
            DEFAULT_TTL,
            false,
        )
        .unwrap();
        std::fs::write(dir.join("a"), b"1").unwrap();
        std::fs::write(dir.join("b"), b"1").unwrap();
        assert_eq!(ms.tick().len(), 2, "both watched files changed");
        ms.retire("gone", OPERATOR_RETIRE).unwrap();

        let now = 1_000_000;
        let rows = monitor_entries_of(&ms.list(), &ms.firings(), now);
        let names: Vec<(&str, bool)> = rows.iter().map(|r| (r.name.as_str(), r.live)).collect();
        assert_eq!(
            names,
            vec![("tick", true), ("once", false), ("gone", false)]
        );
        let tick = &rows[0];
        assert!(tick.repeat && tick.fired == 1, "{tick:?}");
        assert!(
            tick.ends_at_ms > now && tick.ends_at_ms <= now + 300_000,
            "{tick:?}"
        );
        assert!(
            tick.ended.is_empty() && tick.watching.contains('a'),
            "{tick:?}"
        );
        assert!(rows[1].ended.starts_with("FIRED"), "{:?}", rows[1]);
        assert_eq!(rows[1].fired, 1);
        assert!(
            rows[2].ended.starts_with("retired by the operator"),
            "{:?}",
            rows[2]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Who `monitor`'s own `retire` op says retired a watch (`builtins::monitor`) - the model's act,
/// which [`monitor_notice`] does not announce back to it.
const MODEL_RETIRE: &str = "the model";

/// Who a retire from the monitors pane says retired a watch - announced to the model, unlike its
/// own ([`MODEL_RETIRE`]).
pub(crate) const OPERATOR_RETIRE: &str = "the operator";

/// The completion of a background job, as the sentence the model is handed: a labelled notice in the
/// harness's own voice. It names the job, its command, how it ended and where its output is, and never
/// inlines the output, which is one `job_output` away. `mine` says whose job it was.
pub(crate) fn completion_notice(done: &[JobCompletion], mine: bool) -> String {
    let mut s = String::from("[job] ");
    match (mine, done.len() == 1) {
        (true, true) => s.push_str("a job you backgrounded has ended:\n"),
        (true, false) => s.push_str(&format!(
            "{} jobs you backgrounded have ended:\n",
            done.len()
        )),
        (false, true) => s.push_str("a job a session below this one started has ended:\n"),
        (false, false) => s.push_str(&format!(
            "{} jobs a session below this one started have ended:\n",
            done.len()
        )),
    }
    for c in done {
        // no byte count (a number with nothing to do), and the command only for a job
        // ANOTHER session started - the model wrote its own one call ago, and echoing it back in
        // full was most of the notice.
        let whose = if mine {
            String::new()
        } else {
            let command = if c.command.is_empty() {
                "command not recorded"
            } else {
                c.command.as_str()
            };
            format!(": {command} (started by `{}`)", c.owner)
        };
        s.push_str(&format!(
            "  - `{}` {} after {}{}\n",
            letibot_tools::exec::job_label(c.slug.as_deref(), &c.job),
            c.state,
            human_secs(c.elapsed_ms),
            whose,
        ));
        if let Some(m) = &c.memory {
            s.push_str(&format!("    memory: {m}\n"));
        }
        // The one job whose result IS its output: a fanout's per-host table, carried in `detail`
        // rather than read from a stream, because the job has usually been reaped by now.
        if !c.detail.is_empty() {
            for line in c.detail.lines() {
                s.push_str(&format!("    {line}\n"));
            }
        }
    }
    if mine {
        // the closing named `job_wait` on every completion, said "you do not need to wait"
        // to a model that had just waited, and gave `job="…"` for an id it knew. It names the door.
        s.push_str(&format!("{JOB_CLOSING}{}.", handles_as("job", done)));
    } else {
        // the paragraph this replaces spent its length explaining why the reader is
        // being told, and named no starter.
        s.push_str(&stopped_closing(done, "output is gone."));
    }
    s
}

/// `(Started by `s-a`, which has stopped; its output is gone.)` - the closing a settlement
/// handed up from a session that has stopped carries, with one clause per starter the batch has.
fn stopped_closing(done: &[JobCompletion], tail: &str) -> String {
    let mut owners: Vec<&str> = Vec::new();
    for c in done {
        if !owners.contains(&c.owner.as_str()) {
            owners.push(&c.owner);
        }
    }
    let names: Vec<String> = owners.iter().map(|o| format!("`{o}`")).collect();
    let (verb, their) = if owners.len() > 1 {
        ("have stopped", "their")
    } else {
        ("has stopped", "its")
    };
    format!(
        "(Started by {}, which {verb}; {their} {tail})",
        names.join(" and ")
    )
}

/// The same sentence for a subagent, and the same rule: a subagent settles through the job channel
/// because it IS one, and only the verb differs, since a child is read with `task_result`. A
/// handed-up child is still collectable here, because every level shares the tree's handle list.
pub(crate) fn subagent_notice(done: &[JobCompletion], mine: bool) -> String {
    let mut s = String::from("[task] ");
    match (mine, done.len() == 1) {
        (true, true) => s.push_str("a subagent you started has finished:\n"),
        (true, false) => s.push_str(&format!(
            "{} subagents you started have finished:\n",
            done.len()
        )),
        (false, true) => s.push_str(
            "a subagent a session below this one started has \
             finished:\n",
        ),
        (false, false) => s.push_str(&format!(
            "{} subagents a session below this one started have finished:\n",
            done.len()
        )),
    }
    for c in done {
        let whose = if mine {
            String::new()
        } else {
            format!(" (started by `{}`)", c.owner)
        };
        // The child by its name, `fix-login · s-…-sub-3` - the job rule, and the same
        // spelling `task` printed when it started the child. A child nobody named (a settlement
        // from a daemon before names) is its bare handle, byte for byte what this always said.
        let label = letibot_tools::builtins::task::subagent_label(c.slug.as_deref(), &c.job);
        if c.detail.is_empty() {
            s.push_str(&format!("  - `{label}` {}{}\n", c.state, whose));
        } else {
            s.push_str(&format!(
                "  - `{label}` {}: {}{}\n",
                c.state, c.detail, whose
            ));
        }
    }
    if mine {
        // "calling `task_result` would only hold you … Read what it said with
        // `task_result`" said don't and do in one breath, with `task="…"` for a known handle.
        s.push_str(&format!("{TASK_CLOSING}{}.", handles_as("task", done)));
    } else {
        s.push_str(&stopped_closing(
            done,
            &format!(
                "answer is still here: `task_result` {}.",
                handles_as("task", done)
            ),
        ));
    }
    s
}

/// The closing a `[job]` notice for this session's own jobs ends with, up to its handles - the
/// words the head's fold and job card recognise (`tui`'s `PROMISES` and `CLOSINGS`).
pub const JOB_CLOSING: &str = "Output: `job_output` ";
/// The same for a `[task]` notice of this session's own children.
pub const TASK_CLOSING: &str = "Full answer: `task_result` ";

/// `job="j1", job="j2"` - the real handles, never a placeholder.
pub(crate) fn handles_as(field: &str, done: &[JobCompletion]) -> String {
    done.iter()
        .map(|c| format!("{field}=\"{}\"", c.job))
        .collect::<Vec<_>>()
        .join(", ")
}
/// One channel, two nouns: a job and a subagent arrive through the same queue and ring the same bell,
/// and are told apart here rather than in two queues, because two queues would be the second mechanism
/// the ruling avoided. `owner != me` is what says whose a settlement was.
pub(crate) fn completion_notices(
    done: Vec<JobCompletion>,
    me: &str,
    rows: &[TodoItem],
) -> Vec<String> {
    // Mine first, then what was handed up - so a session reading one wake reads its
    // own settlements before its children's, and the order of the two groups cannot be
    // mistaken for one list.
    let (theirs, mine): (Vec<JobCompletion>, Vec<JobCompletion>) =
        done.into_iter().partition(|c| c.owner != me);
    let mut out = Vec::new();
    for (batch, mine) in [(mine, true), (theirs, false)] {
        let (tasks, jobs): (Vec<JobCompletion>, Vec<JobCompletion>) = batch
            .into_iter()
            .partition(|c| c.kind == BackgroundKind::Subagent);
        if !jobs.is_empty() {
            let mut s = completion_notice(&jobs, mine);
            s.push_str(&waiting_on_these(&jobs, rows));
            out.push(s);
        }
        // A later turn of a child already reported is its own sentence: it is not the child
        // finishing, so a heading saying so over it would be false. No plan line under it, since no
        // row waits on the same child twice.
        let (again, tasks): (Vec<JobCompletion>, Vec<JobCompletion>) =
            tasks.into_iter().partition(|c| c.later);
        if !tasks.is_empty() {
            let mut s = subagent_notice(&tasks, mine);
            s.push_str(&waiting_on_these(&tasks, rows));
            out.push(s);
        }
        if !again.is_empty() {
            out.push(later_turn_notice(&again, mine));
        }
    }
    out
}

/// One line per later turn of a child the model has already been told about, named the way every
/// sentence names a child (`slug · handle`), with the first line of the new answer. `mine` decides
/// whose child it is: a later turn handed up from a session below is not one "you started".
pub(crate) fn later_turn_notice(done: &[JobCompletion], mine: bool) -> String {
    let mut s = String::from("[task] ");
    match (mine, done.len() == 1) {
        (true, true) => s.push_str("a subagent you started has answered again on a later turn:\n"),
        (true, false) => s.push_str(&format!(
            "{} subagents you started have answered again on later turns:\n",
            done.len()
        )),
        (false, true) => s.push_str(
            "a subagent a session below this one started has answered again on a later turn:\n",
        ),
        (false, false) => s.push_str(&format!(
            "{} subagents a session below this one started have answered again on later turns:\n",
            done.len()
        )),
    }
    for c in done {
        let label = letibot_tools::builtins::task::subagent_label(c.slug.as_deref(), &c.job);
        s.push_str(&format!("  - `{label}` answered again: {}\n", c.detail));
    }
    s.push_str(&format!("{TASK_CLOSING}{}.", handles_as("task", done)));
    s
}

/// The plan rows that wait on these completions, as one line under the notice, or nothing when no row
/// does: a row attached with `wait_on` is the plan's own statement that this ending is what it waited
/// for. One line, opening with [`WAITING_ON_THIS`] or [`WAITING_ON_THESE`], naming open rows only.
pub(crate) fn waiting_on_these(done: &[JobCompletion], rows: &[TodoItem]) -> String {
    use letibot_tokencore::store::{TodoNeed, TodoStatus};
    let waits = |row: &TodoItem, c: &JobCompletion| {
        row.needs.iter().any(|n| match n {
            TodoNeed::Job { handle } => {
                c.kind == BackgroundKind::Job
                    && (*handle == c.job || c.slug.as_deref() == Some(handle.as_str()))
            }
            TodoNeed::Child { id } => c.kind == BackgroundKind::Subagent && *id == c.job,
            _ => false,
        })
    };
    let one = done.len() == 1;
    let named: Vec<String> = rows
        .iter()
        .filter(|r| matches!(r.status, TodoStatus::Pending | TodoStatus::InProgress))
        .filter_map(|r| {
            let on: Vec<&JobCompletion> = done.iter().filter(|c| waits(r, c)).collect();
            if on.is_empty() {
                return None;
            }
            let text = letibot_tools::builtins::todo::said_short(&r.content);
            let whose = letibot_tools::builtins::todo::whose_row(&r.by);
            Some(if one {
                format!("\"{text}\" ({whose})")
            } else {
                let handles: Vec<String> = on.iter().map(|c| format!("`{}`", c.job)).collect();
                format!("\"{text}\" ({whose}, on {})", handles.join(", "))
            })
        })
        .collect();
    if named.is_empty() {
        return String::new();
    }
    let verb = match (one, done[0].kind) {
        (true, BackgroundKind::Job) => "it has ended",
        (true, BackgroundKind::Subagent) => "it has finished",
        (false, BackgroundKind::Job) => "they have ended",
        (false, BackgroundKind::Subagent) => "they have finished",
    };
    let them = if named.len() == 1 { "it" } else { "them" };
    format!(
        "\n{}{} — update {them} with `todo_write` now that {verb}.",
        if one {
            WAITING_ON_THIS
        } else {
            WAITING_ON_THESE
        },
        named.join(", ")
    )
}

/// The endings these rows wait on that the model has not digested yet, added to `jobs`: the half of
/// `running_jobs` that reads the watcher set's read record. A job already called running is skipped,
/// because running is its own quiet and the grace is for after.
pub(crate) fn graced_endings(
    mut jobs: letibot_tools::builtins::todo::RunningJobs,
    rows: &[TodoItem],
    w: &JobWatchers,
    cap: std::time::Duration,
    now: std::time::SystemTime,
) -> letibot_tools::builtins::todo::RunningJobs {
    use letibot_tokencore::store::TodoNeed;
    for row in rows {
        for need in &row.needs {
            let handle = match need {
                TodoNeed::Job { handle } if !jobs.is_running(handle) => handle,
                TodoNeed::Child { id } => id,
                _ => continue,
            };
            if let Some(left) = w.grace_left(handle, cap, now) {
                jobs = jobs.digesting(handle.clone(), left);
            }
        }
    }
    jobs
}

/// The opening words of [`waiting_on_these`]' line for one completion - what the head's fold
/// (`tui`'s `folded_notice`) and job card recognise the line by.
pub const WAITING_ON_THIS: &str = "Waiting on this: ";
/// The same for a group of several completions.
pub const WAITING_ON_THESE: &str = "Waiting on these: ";

/// The held batch, as one note: the `[meanwhile]` line, then the settlements in
/// [`completion_notices`]' own words - unchanged, so the head's compact job rows read the groups
/// under the heading exactly as they read a wake's.
pub(crate) fn meanwhile_notice(
    batch: Vec<JobCompletion>,
    me: &str,
    rule: crate::delivery::Rule,
    cap: std::time::Duration,
    rows: &[TodoItem],
) -> String {
    let mut parts = vec![crate::delivery::meanwhile_heading(batch.len(), rule, cap)];
    parts.extend(completion_notices(batch, me, rows));
    parts.join("\n\n")
}

/// The delivery, for the two places that hand a result to the model: the round boundary and a
/// turn's start. The decision is `crate::delivery::decide`'s, pure and asserted there; this reads the
/// session and turns the decision into notices, holding no state of its own.
#[derive(Clone)]
pub(crate) struct Deliveries {
    watch: Arc<JobWatchers>,
    /// The plan the decision reads. `None` in a test with no board.
    todos: Option<Arc<TodoBoard>>,
    log: Arc<crate::delivery::DeliveryLog>,
    cap: std::time::Duration,
    me: String,
}

/// What a between-turns read hands a turn's start: the notices said now (immediates and idle
/// arrivals, in `completion_notices`' words) and the `[meanwhile]` batch, apart so a wake can
/// keep the batch last.
pub(crate) struct BetweenTurns {
    pub(crate) now: Vec<String>,
    pub(crate) batch: Option<String>,
}

impl Deliveries {
    pub(crate) fn new(
        watch: Arc<JobWatchers>,
        todos: Option<Arc<TodoBoard>>,
        log: crate::delivery::DeliveryLog,
        cap: std::time::Duration,
        me: &str,
    ) -> Self {
        Deliveries {
            watch,
            todos,
            log: Arc::new(log),
            cap,
            me: me.to_string(),
        }
    }

    /// What must be said now: the sort, its immediates recorded and worded. Everything else
    /// is left held.
    pub(crate) fn immediates(&self, skip: impl Fn(&JobCompletion) -> bool) -> Vec<String> {
        let rows = self
            .todos
            .as_ref()
            .map(|t| t.snapshot())
            .unwrap_or_default();
        let now = crate::delivery::sort(&self.watch, &rows, skip);
        if now.is_empty() {
            return Vec::new();
        }
        self.log.immediates(&now, std::time::SystemTime::now());
        let done: Vec<JobCompletion> = now.into_iter().map(|(c, _)| c).collect();
        // Delivered, and recorded as such - the fact the plan check's grace reads
        // (`JobWatchers::grace_left`).
        self.watch.read_now(&done);
        completion_notices(done, &self.me, &rows)
    }

    /// The board the notices name rows from - empty in a test with no board.
    fn rows(&self) -> Vec<TodoItem> {
        self.todos
            .as_ref()
            .map(|t| t.snapshot())
            .unwrap_or_default()
    }

    /// The held batch, taken and recorded, as one `[meanwhile]` note - or `None` when nothing
    /// is held.
    pub(crate) fn batch(&self, rule: crate::delivery::Rule) -> Option<String> {
        let held = self.watch.take_held();
        if held.is_empty() {
            return None;
        }
        self.log
            .completions(&held, rule, std::time::SystemTime::now());
        self.watch.read_now(&held);
        Some(meanwhile_notice(
            held,
            &self.me,
            rule,
            self.cap,
            &self.rows(),
        ))
    }

    /// A round boundary inside a running turn: the immediates, and the batch only when the
    /// longest-held result has waited past the cap.
    pub(crate) fn at_round_boundary(&self) -> Option<String> {
        let mut parts = self.immediates(|_| false);
        if crate::delivery::starved(&self.watch, self.cap, std::time::SystemTime::now())
            && let Some(b) = self.batch(crate::delivery::Rule::Starvation)
        {
            parts.push(b);
        }
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }

    /// A turn the model is taking anyway: immediate items, what arrived while idle, and the batch held during the last turn (`Rule::Batch` rides this turn).
    /// Test-only: doors use [`Deliveries::at_turn_start_unread`] because a door's row can still fail.
    #[cfg(test)]
    pub(crate) fn at_turn_start(&self) -> Option<String> {
        let b = self.between_turns(|_| false, crate::delivery::Rule::Batch);
        let mut parts = b.now;
        parts.extend(b.batch);
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }

    /// [`Deliveries::at_turn_start`] for a door whose submit can still fail: the words, and the
    /// results they say, for a caller that marks them only once its row is in.
    pub(crate) fn at_turn_start_unread(&self) -> (Option<String>, Vec<JobCompletion>) {
        let (b, told) = self.between_turns_unread(|_| false, crate::delivery::Rule::Batch);
        let mut parts = b.now;
        parts.extend(b.batch);
        ((!parts.is_empty()).then(|| parts.join("\n\n")), told)
    }

    /// Everything owed between turns: holding exists only so a running turn is not interrupted, so nothing stays held.
    /// Arrivals during work are the batch under `held_rule`, arrivals while idle an ordinary `Rule::Idle` completion.
    /// Test-only, for [`Deliveries::at_turn_start`]'s reason.
    #[cfg(test)]
    pub(crate) fn between_turns(
        &self,
        skip: impl Fn(&JobCompletion) -> bool,
        held_rule: crate::delivery::Rule,
    ) -> BetweenTurns {
        let (out, told) = self.between_turns_unread(skip, held_rule);
        self.read(&told);
        out
    }

    /// `between_turns` without marking anything read: the sentences and the results they say,
    /// for a caller that has not yet put them in front of the model. It marks them with `read` once it
    /// has, or hands them back with `put_back` when the turn never opened.
    pub(crate) fn between_turns_unread(
        &self,
        skip: impl Fn(&JobCompletion) -> bool,
        held_rule: crate::delivery::Rule,
    ) -> (BetweenTurns, Vec<JobCompletion>) {
        // Taken means owed until the turn that says it begins - marked before the take,
        // because `pending` and `held` are what a reader waits on and the take is what clears
        // them. Without it the activity read quiet between the drain and the clock.
        if self.watch.pending() || self.watch.held_count() > 0 {
            self.watch.took_a_settlement();
        }
        let rows = self.rows();
        let immediate = crate::delivery::sort(&self.watch, &rows, skip);
        let held = self.watch.take_held();
        let ended = self.watch.last_turn_ended();
        let (busy, idle): (Vec<JobCompletion>, Vec<JobCompletion>) = held
            .into_iter()
            .partition(|c| ended.is_some_and(|e| c.arrived <= e));
        let at = std::time::SystemTime::now();
        if !immediate.is_empty() {
            self.log.immediates(&immediate, at);
        }
        if !idle.is_empty() {
            self.log.completions(&idle, crate::delivery::Rule::Idle, at);
        }
        let mut told: Vec<JobCompletion> = immediate.into_iter().map(|(c, _)| c).collect();
        told.extend(idle);
        let mut said = told.clone();
        let now = if told.is_empty() {
            Vec::new()
        } else {
            completion_notices(told, &self.me, &rows)
        };
        said.extend(busy.iter().cloned());
        let batch = (!busy.is_empty()).then(|| {
            self.log.completions(&busy, held_rule, at);
            meanwhile_notice(busy, &self.me, held_rule, self.cap, &rows)
        });
        if said.is_empty() {
            // Nothing was said after all (a gatekeeper's settlement is nobody's news here), so
            // there is no turn owed and nothing for a reader to wait on.
            self.watch.turn_began();
        }
        (BetweenTurns { now, batch }, said)
    }

    /// These results are in front of the model now - the read record the plan check's grace
    /// reads, and a subagent's `delivered` in the store.
    pub(crate) fn read(&self, told: &[JobCompletion]) {
        if !told.is_empty() {
            self.watch.read_now(told);
        }
    }

    /// These results were never told - the turn that was to carry them did not open - so
    /// they are held again, for the next break to say.
    pub(crate) fn put_back(&self, untold: Vec<JobCompletion>) {
        if untold.is_empty() {
            return;
        }
        self.watch.hold(untold);
        // The ring these arrived with was spent by the turn that could not say them, so the wake
        // that makes the next break happen has to be rung here or the session waits for ever.
        self.watch.a_batch_is_owed();
    }

    pub(crate) fn log_firings(&self, fired: &[letibot_tools::exec::monitor::Firing]) {
        self.log.firings(fired, std::time::SystemTime::now());
    }
}

/// A duration in the words a settlement reads with: `4.4s`, `1m12s`.
pub(crate) fn human_secs(ms: u64) -> String {
    let secs = ms as f64 / 1000.0;
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}
