//! A session held rather than run - the bridge's half of the head protocol.
//!
//! `crates/sessionlog/src/client.rs` states a head's three obligations: attach with a
//! `since_seq` and accept `Resync` as a normal answer, ack after handling, and send
//! `expected_seq` on every mutating frame. A screen discharges them while drawing; this
//! does it while collecting, because what the Claude Code bridge needs from a leticode
//! session is not a picture of it but five facts and the door to answer it.
//!
//! ```text
//!   socket ──pump──> frame ──fold──> State {turn, last message, asks, todos, usage}
//!                        │                        │
//!                        └──ack(seq)              └──Condvar──> a caller waiting
//! ```
//!
//! One `Head` per session, one socket per `Head`, and that is what makes an ask
//! reach the bridge at all: the daemon posts a permission card to the heads attached to
//! that session, so a session the bridge is not attached to has nobody to ask - which is
//! the correct answer (`docs/claude-code-bridge.md`: *"one that is left with no head
//! attached refuses gated calls as any session does"*).
//!
//! A head that only watches says so ([`Head::attach_watching`]). The card is
//! published to every attached head and settled by whichever answers first, so attaching a
//! watcher steals nothing from the operator's own screen; what it could do is COUNT as
//! somebody who will answer - `Hub::deciding_heads` is what `Answers::ask` reads before it
//! posts, and a session whose only head is a watcher would then wait out the whole deadline
//! and report a timeout about a question nobody was ever asked. So a watcher declares
//! `can_decide: false`, the daemon refuses any answer from it by name, and this crate
//! refuses to send it one in the first place.
//!
//! Why a condvar and not a poll. `wait` is a blocking verb whose whole value is that
//! it returns the moment something happens; a loop that slept on the state would turn
//! every wake-up into a latency the orchestrator pays for nothing. The fold notifies and
//! the waiter recomputes, which is the same shape the daemon's own hub uses.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use letibot_sessionlog::activity::{Activity, Waiting};
use letibot_sessionlog::client::{ClientError, HeadClient, Inbound, pump};
use letibot_sessionlog::event::{DeltaTarget, Envelope, PrefixCheck, SessionEvent, TodoEntry};
use letibot_sessionlog::protocol::{Ack, Caps, PeekShape, ServerFrame};
use letibot_sessionlog::view::{
    CallState, OpenDecision, Snapshot, SnapshotItem, TurnState as ViewTurnState,
};
use letibot_tools::builtins::task::Placement;
use letibot_transcript::{ToolOutcome, TranscriptItem};
use serde_json::{Value, json};

use crate::daemon::{Daemon, HEAD_KIND, IDENTITY};

/// How long `wait` gives a just-sent prompt to start before it answers with what it
/// sees. Not a poll interval: whether the turn started is read from the daemon's own
/// [`Activity`], not guessed from a clock. See [`Head::wait`].
pub const START_GRACE: Duration = Duration::from_secs(3);

/// How long a head gives a `Switch` to land before it reports that it is on the wrong
/// session. A re-seat is a second `Hello` on the same socket - one round trip - so the
/// wait is for a daemon that is not answering, not for one that is thinking.
const SEAT_PATIENCE: Duration = Duration::from_secs(5);

/// How many progress entries a head keeps, and the number a `since` older than the
/// ring is measured against. Past five hundred an orchestrator is reading history, and
/// the transcript is where history lives.
pub const RING: usize = 500;

/// The longest one-liner an entry carries - the number `crates/ui/DESIGN.md` arrived
/// at for a tool's display target, kept here for the same reason: an entry is a line a
/// reader scans, not a payload.
const LINE: usize = 120;

/// The assistant message's entry gets more room than a tool's line, because the first
/// line of a report is usually the sentence that says what the turn was for.
const MESSAGE_LINE: usize = 160;

/// How many mode answers a head keeps. A caller collects its own the moment it lands; the
/// rest are the tail of a conversation about `allow-all` that nobody is reading.
pub const SETTLED_KEEP: usize = 8;

/// How long a reader waits for the daemon to answer a `Peek`. One round trip: the
/// patience is for a daemon that has stopped talking, not for one that is thinking.
pub const PEEK_PATIENCE: Duration = Duration::from_secs(10);

/// How long a head waits for the daemon's answer to a `/` line it sent. The reply is
/// published on the session's own hub off the worker's queue, and the patience is for a daemon
/// that has stopped talking, not for one that is thinking.
pub const SLASH_PATIENCE: Duration = Duration::from_secs(20);

/// How long `set_model` waits for the daemon's reply to `/models`. The reply comes off the session's
/// worker, which on a host placement opens its remote tree first (an ssh connect and a checkout)
/// and on a busy box queues behind the daemon's other starts, so it is far longer than one round
/// trip. The wait is one deadline, not a retry: `/models` is applied once.
pub const MODEL_PATIENCE: Duration = Duration::from_secs(120);

/// How long `stop` waits for the daemon to ack the close. The ack follows the session's removal,
/// which is quick; a daemon busy past this is reported as not confirmed.
pub const CLOSE_PATIENCE: Duration = Duration::from_secs(30);

/// The answer to one request for a mode confirmation. `answer` is `yes`, `no` or
/// `nobody`; `applied` is a different question, and a `yes` the session cannot carry is
/// `applied: false` with the prerequisite check's words in `said`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeSettled {
    pub name: String,
    pub applied: bool,
    pub answer: String,
    pub said: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskOption {
    pub option_id: String,
    pub label: String,
    pub kind: String,
}

/// A permission ask that is still owed an answer, as the head sees it. `tool` is
/// joined from the `ToolCallProposed` event by `call_id`, not parsed out of
/// [`Self::summary`]: the card's prose has changed wording before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub req_id: String,
    /// `permission` or `question` - the daemon's own two words, and the reason two
    /// different answer frames exist (`Answer` and `AnswerQuestion`).
    pub kind: String,
    pub call_id: Option<String>,
    pub tool: String,
    pub summary: String,
    pub target: String,
    pub access: String,
    pub options: Vec<AskOption>,
    /// Why a rung is not on the ladder, in one line from the gate, or empty when the
    /// ladder is complete. An option list that is silently shorter is one it cannot
    /// explain to whoever it is reporting to.
    pub options_note: String,
    pub choices: Vec<String>,
    pub deadline: Option<u64>,
}

impl Ask {
    fn from_open(d: &OpenDecision) -> Ask {
        Ask {
            req_id: d.req_id.clone(),
            kind: d.kind.clone(),
            call_id: d.call_id.clone(),
            tool: String::new(),
            summary: d.summary.clone(),
            target: d.target.clone(),
            access: d.access.clone(),
            options: d
                .options
                .iter()
                .map(|o| AskOption {
                    option_id: o.option_id.clone(),
                    label: o.label.clone(),
                    kind: option_kind(o.kind),
                })
                .collect(),
            options_note: d.options_note.clone(),
            choices: d.choices.clone(),
            deadline: d.deadline,
        }
    }

    /// The ask as a tool answer: the id the answer frame needs, the call, and the ladder.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ask": self.req_id,
            "kind": self.kind,
            "tool": self.tool,
            "target": self.target,
            "summary": self.summary,
            "access": self.access,
            "call_id": self.call_id,
            "options": self.options.iter().map(|o| serde_json::json!({
                "option": o.option_id,
                "label": o.label,
                "kind": o.kind,
            })).collect::<Vec<_>>(),
            "options_note": self.options_note,
            "choices": self.choices,
            "deadline": self.deadline,
        })
    }
}

fn option_kind(k: letibot_sessionlog::event::OptionKind) -> String {
    use letibot_sessionlog::event::OptionKind as K;
    match k {
        K::AllowOnce => "allow_once",
        K::AllowSession => "allow_session",
        K::AllowAlways => "allow_always",
        K::RejectOnce => "reject_once",
        K::RejectAlways => "reject_always",
    }
    .to_string()
}

/// One line of what a turn is doing, folded from the events so an orchestrator can
/// follow a session without reading its transcript. The kinds are this file's own closed
/// vocabulary: publishing the protocol's event names would change it every version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The position in the session's progress, monotonic for the life of the head and
    /// never reused. A caller passes the last one it saw as `since`.
    pub cursor: u64,
    /// `tool`, `result`, `job`, `turn`, `ask`, `subagent`, `warning`, `failure`, `message`.
    pub kind: &'static str,
    /// The phase or outcome, in the kind's own words: `proposed`, `ok`, `started`,
    /// `ended`, `opened`, `answered`. Empty when the kind has no phase (`message`).
    pub state: &'static str,
    /// What the entry is *about* - a tool's name, a job's handle, an ask's id, a child's
    /// slug. Empty when it is about the turn itself.
    pub name: String,
    /// The event's own code, for a `warning` or a `failure`. Empty otherwise.
    pub code: String,
    /// One line, already clamped: the command, the reason, the first line said.
    pub text: String,
    /// How long a job ran, when the ending carried a duration.
    pub ms: Option<u64>,
    /// When a provider said its limit lifts, Unix millis - the one field of a `provider` entry a
    /// caller can act on without parsing the sentence beside it.
    pub reset_ms: Option<u64>,
}

impl Entry {
    fn of(kind: &'static str, state: &'static str) -> Entry {
        Entry {
            cursor: 0,
            kind,
            state,
            name: String::new(),
            code: String::new(),
            text: String::new(),
            ms: None,
            reset_ms: None,
        }
    }

    fn named(mut self, name: impl Into<String>) -> Entry {
        self.name = name.into();
        self
    }

    fn coded(mut self, code: impl Into<String>) -> Entry {
        self.code = code.into();
        self
    }

    fn saying(mut self, text: String) -> Entry {
        self.text = text;
        self
    }

    fn lasting(mut self, ms: u64) -> Entry {
        self.ms = Some(ms);
        self
    }

    fn lifting_at(mut self, at: u64) -> Entry {
        self.reset_ms = Some(at);
        self
    }

    /// The entry as a tool answer: the fields that are set, and none that are not, because
    /// a ring of five hundred entries with four empty keys each is a payload nobody reads.
    pub fn to_json(&self) -> Value {
        let mut out = json!({ "cursor": self.cursor, "kind": self.kind });
        if !self.state.is_empty() {
            out["state"] = json!(self.state);
        }
        if !self.name.is_empty() {
            out["name"] = json!(self.name);
        }
        if !self.code.is_empty() {
            out["code"] = json!(self.code);
        }
        if !self.text.is_empty() {
            out["text"] = json!(self.text);
        }
        if let Some(ms) = self.ms {
            out["ms"] = json!(ms);
        }
        if let Some(at) = self.reset_ms {
            out["reset_ms"] = json!(at);
        }
        out
    }
}

/// What a `since` cursor and a limit ask of the ring.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub entries: Vec<Entry>,
    /// The cursor to ask from next: the last entry handed over, or the caller's own cursor
    /// when there was nothing new. Never the newest entry when the limit cut the answer
    /// short - a caller that took 50 of 200 has 150 to fetch, not 0.
    pub next: u64,
    /// Entries that fell off the ring before the caller's cursor could reach them. A
    /// caller that sees a non-zero number here knows its view has a hole in it; zero says
    /// the hole is not this ring's.
    pub dropped: u64,
}

/// The bounded ring of progress entries, and the cursors that make it a stream.
#[derive(Debug, Default, Clone)]
pub struct Ring {
    entries: std::collections::VecDeque<Entry>,
    /// The cursor the next entry will be given. One-based, so `since = 0` means *everything
    /// you still hold* and a cursor of 0 is never a position anything was at.
    next: u64,
}

impl Ring {
    fn push(&mut self, mut entry: Entry) {
        self.next += 1;
        entry.cursor = self.next;
        self.entries.push_back(entry);
        while self.entries.len() > RING {
            self.entries.pop_front();
        }
    }

    /// The entries after `since`, at most `limit` of them, plus what a caller has to be
    /// told to read the answer correctly.
    pub fn page(&self, since: u64, limit: usize) -> Page {
        // What fell off the front before this cursor: every entry numbered up to the oldest
        // one still held that the caller never saw.
        let dropped = match self.entries.front() {
            Some(front) => front.cursor.saturating_sub(1).saturating_sub(since),
            None => 0,
        };
        let entries: Vec<Entry> = self
            .entries
            .iter()
            .filter(|e| e.cursor > since)
            .take(limit.max(1))
            .cloned()
            .collect();
        let next = entries.last().map(|e| e.cursor).unwrap_or(since);
        Page {
            entries,
            next,
            dropped,
        }
    }
}

/// The first line of a message, cut to `max` characters. Characters rather than
/// bytes, and the ellipsis counts inside the budget: an entry that overran its own cap to
/// say it was cut would be the cap lying.
fn one_line(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The branch a child was given, when the child's task names one. A review's task
/// carries `- branch: agent/P1` and nothing else on the wire does, so an absent marker
/// yields nothing rather than guessing a branch out of a sentence.
fn branch_of(text: &str) -> Option<&str> {
    text.lines().find_map(|line| {
        let rest = line
            .trim()
            .strip_prefix("- branch:")
            .or_else(|| line.trim().strip_prefix("branch:"))?;
        let branch = rest.trim();
        (!branch.is_empty()).then_some(branch)
    })
}

/// The word a tool's ending is filed under. The three the design names (`ok`, `failed`,
/// `not_run`) plus the four endings that are neither, each with its own word: a `denied`
/// call that reported `failed` would be a gate's decision recorded as the tool's fault.
fn outcome_word(outcome: &ToolOutcome) -> &'static str {
    match outcome {
        ToolOutcome::Ok => "ok",
        ToolOutcome::Abstained { .. } => "abstained",
        ToolOutcome::Failed { .. } => "failed",
        ToolOutcome::Denied { .. } => "denied",
        ToolOutcome::Timeout => "timeout",
        ToolOutcome::NotRun { .. } => "not_run",
        ToolOutcome::Backgrounded { .. } => "backgrounded",
    }
}

/// Which state a turn is in, from this head's point of view - the three words
/// `leticode_status` and `wait` are written against.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Turn {
    /// No turn since this head attached, or the last one ended.
    #[default]
    Idle,
    Running,
    /// A turn ended in a failure. Terminal, like [`ViewTurnState::Failed`]: a head that
    /// kept drawing "running" after one is the defect that event exists for.
    Failed(String),
}

/// The tokens and the money of the last turn that reported them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Meter {
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub predicted_tokens: u64,
    /// Micro-USD: `None` on a local server (free) and on a metered model nothing prices  -
    /// *unpriced* and *free* are different and only one of them is a number.
    pub cost_micros_usd: Option<u64>,
}

/// What the daemon said to one `/` line this head sent: the verb's own answer, kept
/// whole. The progress ring keeps one clamped line, and for a `/` reply that line is the
/// echo of the request rather than what it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashSaid {
    /// The line as it was sent, without the `/` (`models deepseek`).
    pub line: String,
    /// The verb's answer, one line per line.
    pub said: String,
    /// Whether the verb acted or refused.
    pub ok: bool,
}

impl SlashSaid {
    /// Split the daemon's `detail` (`/<line>\n<the answer>`) into its two halves, with the verdict
    /// taken from the event's own code.
    fn of(detail: &str, ok: bool) -> SlashSaid {
        let (echo, said) = detail.split_once('\n').unwrap_or((detail, ""));
        SlashSaid {
            line: echo.trim().trim_start_matches('/').to_string(),
            said: said.trim().to_string(),
            ok,
        }
    }
}

/// Everything this head keeps about the session it is attached to. Deliberately not the
/// transcript: the bridge's question is *what do I tell the orchestrator* - the last
/// answer, what is being asked, what is planned.
#[derive(Debug, Default)]
pub struct State {
    pub session: String,
    /// The highest `seq` handled, which is what an `Ack` carries and what a resume would
    /// ask from.
    pub seq: u64,
    pub turn: Turn,
    pub turn_id: String,
    /// The last assistant message: the committed row when one landed, the live stream
    /// while a turn is talking.
    last_row: String,
    streaming: String,
    pub asks: Vec<Ask>,
    pub todos: Vec<TodoEntry>,
    pub meter: Meter,
    /// The rounds this head watched, summed: what the package cost so far. `meter` is
    /// one round, so reading it alone reports only the last round's price. Since attach, which for a package is the whole run.
    pub spent: Meter,
    /// How many rounds `spent` sums.
    pub rounds: u64,
    /// The verdict on the last ending: whether this prompt extended the one before it,
    /// which is what tells a slot that did not hold the history from a prompt of ours that
    /// diverged. `None` is a daemon older than the field.
    pub prefix_check: Option<PrefixCheck>,
    /// The dialect's `template_sha` on the last ending, so a template change reads as a different
    /// dialect rather than as an unexplained cache miss. Empty when the daemon did not say.
    pub dialect_template_sha: String,
    /// The newest assistant message that is a report. A model that reports and is then
    /// woken by its own background jobs says *nothing new in them* and stops, so a report
    /// read only from the last message was lost under it.
    pub last_report: Option<String>,
    /// The calls proposed and not yet finished, joined to their tool's name. Bounded, because a
    /// session's calls are many. Not turn bookkeeping - *is the turn still going* is the
    /// daemon's [`Activity`] now - this is only how an ask names the tool it is about.
    calls: std::collections::HashMap<String, String>,
    /// The children whose start this head has announced, by child session id: what an
    /// entry-per-fact fold needs to stay one line, since `opening`, `running` and a
    /// resync's snapshot are three sayings of one child.
    children: std::collections::HashSet<String>,
    /// Background jobs started and not yet settled, from the `Backgrounded` handle and the
    /// `JobSettled` that names it - `leticode_status`' own list, and nothing else: whether a job
    /// holds the work open is the daemon's [`Activity`] (`waiting_on`), not this set.
    pub jobs_open: std::collections::BTreeSet<String>,
    /// What the DAEMON says this session is waiting on - `PROTOCOL_VERSION` 52. It
    /// replaced four guesses this head made from outside, each a model of the daemon's
    /// state built from events that do not carry it. Folded from `ActivityChanged`.
    pub activity: Activity,
    /// Whether a turn has ever run here, from the snapshot's own turn or from one this
    /// head watched. `wait` needs it to tell *a session between turns* from *a session that
    /// has never run one* - and only the first of those is a turn that ended.
    pub turned: bool,
    /// The compact progress `leticode_events` answers from.
    pub events: Ring,
    /// The rows the last `Peek { shape: Rows }` answered with, and the session they are
    /// rows OF. Kept apart because this head holds no transcript: the window is asked for
    /// when it is needed, and `items_dropped` says where it sits.
    pub peek: Option<(String, Snapshot)>,
    /// How many answers have landed, so a reader can tell a fresh one from the one it was
    /// already holding when it sent its own request.
    pub peeks: u64,
    /// A refused `Peek`, by the session it named - so the wait ends with the daemon's own
    /// sentence instead of timing out about an answer that arrived.
    pub peek_error: Option<(String, String)>,
    /// How the mode confirmations this head asked for ended, by the `client_request_id`
    /// they were asked with. Bounded by [`SETTLED_KEEP`].
    pub mode_settled: Vec<(String, ModeSettled)>,
    /// Request ids of `CloseSession` frames the daemon has acked as done.
    pub close_acks: Vec<String>,
    /// `Some(reason)` when the daemon closed the connection; every later send says so
    /// rather than failing on a dead socket two frames later.
    pub closed: Option<String>,
    /// The daemon's boot failed, and this session will never run anything. A
    /// `Class::Failure` warning published before the hubs close and replayed to every head
    /// that attaches after. Quiet is not an answer when the reason for it is terminal.
    pub boot_failed: Option<String>,
    /// The daemon's answer to the last mutating frame, when it refused one. A refusal is a
    /// real answer (`already stopped`, a stale option) and is worth saying back.
    pub rejection: Option<String>,
    /// The daemon's answer to the last `/` line this head sent, whole. See [`SlashSaid`].
    pub slash: Option<SlashSaid>,
    /// How many such answers have landed, so a caller can tell the one it is waiting for from
    /// the one it was already holding when it asked.
    pub slashes: u64,
    /// What answers this session's turns, as `/models` spells it: the model the last round was
    /// sent to. Empty until a turn has run under this head, which is *not known*, not *local*.
    pub model: String,
    /// The last time the provider itself was in trouble, as the daemon's typed event said it.
    pub last_trouble: Option<Trouble>,
    /// How many such events have landed since this head attached - `wait`'s baseline, so a wait is
    /// answered by the trouble it saw rather than by one that was already true.
    pub troubles: u64,
    /// The sentence for a daemon on another protocol number than this build, naming the older
    /// side and the restart command. `None` when they match.
    pub skew: Option<String>,
    /// Lines from the daemon this build could not read. Skipped, never silent.
    pub unreadable: u64,
}

/// The word a provider trouble is filed under - this file's closed vocabulary, so a head that asks
/// *is the provider stuck* matches four words instead of the daemon's prose. A kind this build does
/// not know reads as `trouble`, and the raw kind stays in the entry's `code`.
fn trouble_word(kind: &str) -> &'static str {
    match kind {
        "rate_limit" => "rate_limit",
        "quota" => "quota",
        "timeout" => "timeout",
        "stall" => "stall",
        _ => "trouble",
    }
}

/// A provider's trouble, kept whole: the free fields of the daemon's typed event, so
/// `leticode_status` can answer *what happened and when does it lift* without re-reading a ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trouble {
    /// `rate_limit`, `quota`, `timeout`, `stall`, or a word this build does not know.
    pub kind: String,
    /// What answers the session, as `/models` spells it.
    pub model: String,
    /// The provider's own words, one line.
    pub detail: String,
    /// Unix millis the provider said its limit lifts, when it said.
    pub reset_ms: Option<u64>,
    /// How many of this kind in a row, this one counted.
    pub count: u32,
    /// Where the daemon's own fallback policy moved the session, when it did.
    pub switched_to: Option<String>,
}

impl Trouble {
    /// The shape `leticode_status` answers with: every field, and `null` where the daemon said nothing,
    /// so a caller can tell `absent` from a field it forgot to read.
    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "model": self.model,
            "detail": self.detail,
            "reset_ms": self.reset_ms,
            "count": self.count,
            "switched_to": self.switched_to,
        })
    }
}

/// One child of this session, as one place or the other reports it - the `Subagent`
/// event, or the same child in a snapshot's `subagents`. Borrowed rather than cloned: the
/// fold is a projection of an event it is handed.
struct Child<'a> {
    id: &'a str,
    slug: &'a str,
    /// `opening`, `running`, `done` or `failed` - the daemon's own four words.
    state: &'a str,
    /// The subtask, whole, on every state.
    task: &'a str,
    /// The subtask's first line on the opening states, the answer's first line on a finish.
    prompt: &'a str,
    /// The child's answer's first line, on a normal finish and nowhere else.
    answer: Option<&'a str>,
}

impl State {
    /// The last assistant message, as a person would read it.
    pub fn last_message(&self) -> &str {
        if self.turn == Turn::Running && !self.streaming.is_empty() {
            &self.streaming
        } else {
            &self.last_row
        }
    }

    /// Whether the work is waiting on a job or a child of its own - the daemon's own
    /// answer ([`Activity`]), which replaced a guess built here from an unfinished todo row
    /// and a `wake_due` flag. Kept so a reader of `leticode_status` asks what it always did.
    pub fn waiting_on_jobs(&self) -> bool {
        self.activity
            .waiting_on()
            .iter()
            .any(|w| matches!(w, Waiting::Job(_) | Waiting::Child(_)))
    }

    /// The words a person reads for what this session waits on - `["job j8032", "monitor
    /// ci"]`, empty for a quiet session.
    pub fn waiting_on(&self) -> Vec<String> {
        self.activity.waiting_labels()
    }

    /// What the last ending measured, as the keys `leticode_status` answers with. Defined here
    /// rather than spelled in the status answer, so a caller and a test read one pair of names.
    /// `null` and an empty string are a daemon older than the fields, which measured nothing.
    pub fn measured_keys(&self) -> Vec<(&'static str, Value)> {
        vec![
            (
                "prefix_check",
                serde_json::to_value(&self.prefix_check).unwrap_or(Value::Null),
            ),
            (
                "dialect_template_sha",
                json!(self.dialect_template_sha.as_str()),
            ),
        ]
    }

    /// The word `leticode_status` answers with. `asking` wins over `running` because a turn
    /// blocked on a card is a turn whose next step is a person's. `running` is read from the
    /// daemon's activity and, for a daemon older than 52, from the round-level turn.
    pub fn word(&self) -> &'static str {
        if !self.asks.is_empty() {
            "asking"
        } else if self.boot_failed.is_some() || matches!(self.turn, Turn::Failed(_)) {
            // A daemon whose boot failed is `failed`, not `idle`: the session has no
            // turn and will never start one, so `idle` is a word for a session that is between
            // turns and this one is not.
            "failed"
        } else if self.turn == Turn::Running
            || self
                .activity
                .waiting_on()
                .iter()
                .any(|w| matches!(w, Waiting::Turn | Waiting::Calls))
        {
            "running"
        } else {
            "idle"
        }
    }

    /// Fold one server frame in. Returns the `seq` to acknowledge, when the frame was an
    /// event with one.
    fn fold(&mut self, frame: ServerFrame) {
        match frame {
            ServerFrame::Hello {
                session_id,
                snapshot,
                protocol_version,
                ..
            } => {
                self.skew = letibot_sessionlog::protocol_skew(
                    protocol_version,
                    letibot_sessionlog::protocol::PROTOCOL_VERSION,
                )
                .map(|s| {
                    format!("{s} Run `letibot --stop` in the daemon's folder, then start again.")
                });
                self.session = session_id;
                if let Some(snap) = snapshot {
                    self.seed(&snap);
                }
            }
            ServerFrame::Event(env) => {
                self.seq = self.seq.max(env.seq);
                self.event(env);
            }
            ServerFrame::Resync { snapshot, .. } => {
                // The daemon demoted this head - it fell too far behind, and its own doc is
                // explicit that this is a normal answer. The frame carries a fresh snapshot,
                // so nothing is stale after it.
                self.seed(&snapshot);
            }
            ServerFrame::Todos { todos, .. } => self.todos = todos,
            ServerFrame::Jobs { jobs, .. } => {
                self.jobs_open = jobs
                    .into_iter()
                    .filter(|j| j.running)
                    .map(|j| j.id)
                    .collect();
            }
            ServerFrame::Rejected {
                client_request_id,
                reason,
                ..
            } => {
                // A refused `Peek` has to wake the reader waiting for rows: the answer is a
                // `Rejected` keyed `peek:<session>`, the only correlation that frame has. Without
                // this the peek would sit out the patience and report a timeout.
                if let Some(on) = client_request_id.strip_prefix("peek:") {
                    self.peek_error = Some((on.to_string(), reason.clone()));
                }
                self.rejection = Some(reason);
            }
            // The rows `leticode_transcript` asked for. Kept together with the session it
            // names, because a reader waiting on it must be sure it is not somebody else's
            // read.
            ServerFrame::Peeked {
                session_id,
                snapshot: Some(snap),
                ..
            } => {
                self.peek = Some((session_id, *snap));
                self.peeks += 1;
            }
            // How a mode confirmation ended (`PROTOCOL_VERSION` 54) - the answer to
            // `leticode_mode`'s ask, from a person's head or from nobody at all.
            ServerFrame::ModeSettled {
                client_request_id,
                name,
                applied,
                answer,
                said,
                ..
            } => {
                if !client_request_id.is_empty() {
                    // Bounded: a session that has been asked a dozen times is a dozen answers a
                    // caller has already collected, and a head lives for the package.
                    if self.mode_settled.len() >= SETTLED_KEEP {
                        self.mode_settled.remove(0);
                    }
                    self.mode_settled.push((
                        client_request_id,
                        ModeSettled {
                            name,
                            applied,
                            answer,
                            said,
                        },
                    ));
                }
            }
            ServerFrame::Accepted {
                client_request_id,
                note,
                ..
            } if note == letibot_sessionlog::protocol::NOTE_SESSION_CLOSED => {
                self.close_acks.push(client_request_id);
            }
            ServerFrame::Bye { reason } => self.closed = Some(reason),
            _ => {}
        }
    }

    /// A snapshot is what a head that attaches late is handed: rows, the turn in flight,
    /// and the cards still open. Everything it carries replaces what this head held, which
    /// is what makes a `Switch` or a resync land correctly.
    fn seed(&mut self, snap: &Snapshot) {
        self.seq = self.seq.max(snap.seq);
        // What the session is waiting on, as the daemon last said it. Seeded before the turn
        // because it is the finer fact: the snapshot's turn state is round-level, and a head that
        // attached between two rounds would read `idle` from it while a call is still running.
        self.activity = snap.activity.clone();
        self.turn = match &snap.turn {
            None => Turn::Idle,
            Some(t) => match &t.state {
                ViewTurnState::Running => Turn::Running,
                ViewTurnState::Interrupted { .. } => Turn::Idle,
                // The round ended and its calls are running: the turn is not over. The activity's
                // own `Calls` says the same thing; this keeps `leticode_status.state` right against
                // a daemon too old to send one.
                ViewTurnState::Finished { .. }
                    if !t
                        .calls
                        .iter()
                        .all(|c| matches!(c.state, CallState::Finished { .. })) =>
                {
                    Turn::Running
                }
                ViewTurnState::Finished { .. } => Turn::Idle,
                ViewTurnState::Failed { error, .. } => Turn::Failed(error.clone()),
            },
        };
        self.turn_id = snap
            .turn
            .as_ref()
            .map(|t| t.turn_id.clone())
            .unwrap_or_default();
        // What answers this session, as the daemon last sent a round there. A head that attaches
        // between turns has no `TurnStarted` to read it from, and `leticode_status.model` would be
        // empty on exactly the session an orchestrator picks up cold.
        if let Some(t) = &snap.turn
            && !t.model.is_empty()
        {
            self.model = t.model.clone();
        }
        // The live turn's text is the newest assistant words there are; a session with no
        // turn in flight falls back to its last committed answer row.
        self.streaming = snap
            .turn
            .as_ref()
            .map(|t| t.text.clone())
            .unwrap_or_default();
        self.last_row = last_assistant(&snap.items).unwrap_or_default();
        self.last_report = snap.items.iter().rev().find_map(|i| match i.item.as_ref() {
            Some(TranscriptItem::Assistant { text, .. })
                if crate::report::parse(text).is_some() =>
            {
                Some(text.clone())
            }
            _ => None,
        });
        // A snapshot with a turn in it, or with words a turn produced, is a session that
        // has run one. This is what `wait` must not mistake for *no turn yet*.
        if snap.turn.is_some() || !self.last_row.is_empty() {
            self.turned = true;
        }
        self.asks = snap.open_decisions.iter().map(Ask::from_open).collect();
        self.asks.retain(|a| !a.req_id.is_empty());
        // The children, as the parent's own view last reported them: a head that attaches
        // mid-review has seen no `Subagent` event, and a child already finished before this head
        // existed is history, not progress.
        for child in &snap.subagents {
            self.child(
                Child {
                    id: &child.session_id,
                    slug: &child.slug,
                    state: &child.state,
                    task: &child.task,
                    prompt: &child.prompt,
                    answer: child.answer.as_deref(),
                },
                true,
            );
        }
    }

    /// One line about a child of this session - its branch and how it ended - from the `Subagent`
    /// event or a snapshot. `from_snapshot` keeps a head from announcing a child that finished
    /// before it attached as news.
    fn child(&mut self, child: Child<'_>, from_snapshot: bool) {
        let name = if child.slug.is_empty() {
            child.id
        } else {
            child.slug
        };
        let branch = branch_of(child.task).or_else(|| {
            // An older daemon's `task` is empty and its `prompt` still carries the words; on a
            // finish that field is the answer's first line, where no placement line exists.
            branch_of(child.prompt)
        });
        match child.state {
            "opening" | "running" => {
                if !self.children.insert(child.id.to_string()) {
                    return;
                }
                let said = match branch {
                    Some(branch) => branch.to_string(),
                    None => one_line(child.task, LINE),
                };
                self.events.push(
                    Entry::of("subagent", "started")
                        .named(name)
                        .saying(one_line(&said, LINE)),
                );
            }
            "done" | "failed" => {
                let known = self.children.remove(child.id);
                if from_snapshot && !known {
                    return;
                }
                // The verdict is the child's own last words: its answer's first line on a
                // normal finish, and - a child that FAILED carries no answer - what it was
                // asked, because the ending has to say which child died.
                let said = child.answer.unwrap_or_default().trim();
                let said = if said.is_empty() { child.prompt } else { said };
                let line = match branch {
                    Some(branch) if !said.trim().is_empty() => format!("{branch} — {said}"),
                    Some(branch) => branch.to_string(),
                    None => said.to_string(),
                };
                let word = if child.state == "done" {
                    "ended"
                } else {
                    "failed"
                };
                self.events.push(
                    Entry::of("subagent", word)
                        .named(name)
                        .saying(one_line(&line, LINE)),
                );
            }
            // A state this build has no word for is not an ending and not a start; the
            // snapshot's own list is what a caller reads for the full picture.
            _ => {}
        }
    }

    fn event(&mut self, env: Envelope) {
        match env.event {
            SessionEvent::TurnStarted { turn_id, model, .. } => {
                self.turn = Turn::Running;
                self.turned = true;
                self.turn_id = turn_id;
                // The model this round was actually sent to, which is the only evidence a head has
                // about where the session's turns go: `leticode_status` answers it.
                if !model.is_empty() {
                    self.model = model;
                }
                self.streaming.clear();
                self.events.push(Entry::of("turn", "started"));
            }
            SessionEvent::Delta { target, text, .. } => {
                if target == DeltaTarget::Text {
                    self.streaming.push_str(&text);
                }
            }
            SessionEvent::TurnFinished {
                usage,
                ends_turn,
                prefix_check,
                dialect_template_sha,
                ..
            } => {
                // Whether this frame ended the PROMPT is the daemon's field: a round that
                // proposed calls is not an ending, and a head that read it as one wrote a report
                // over a turn still running. The metrics are per ROUND and stay so.
                if ends_turn {
                    self.turn = Turn::Idle;
                    self.events.push(Entry::of("turn", "ended"));
                }
                self.turned = true;
                // The ending's own record of what the engine measured. `leticode_status` answers
                // both, and neither is derived here: a check this head recomputed could disagree
                // with the one the turn was actually judged by.
                self.prefix_check = prefix_check;
                self.dialect_template_sha = dialect_template_sha;
                if !self.streaming.is_empty() {
                    self.last_row = std::mem::take(&mut self.streaming);
                }
                self.meter = Meter {
                    prompt_tokens: usage.prompt_tokens,
                    cached_tokens: usage.cached_tokens,
                    predicted_tokens: usage.predicted_tokens,
                    cost_micros_usd: usage.cost_micros_usd,
                };
                self.rounds += 1;
                self.spent.prompt_tokens += usage.prompt_tokens;
                self.spent.cached_tokens += usage.cached_tokens;
                self.spent.predicted_tokens += usage.predicted_tokens;
                // Unpriced stays unpriced until a priced round arrives; a priced round
                // never turns a sum back into `None`.
                if let Some(c) = usage.cost_micros_usd {
                    *self.spent.cost_micros_usd.get_or_insert(0) += c;
                }
            }
            SessionEvent::TurnInterrupted { reason, .. } => {
                // An interrupted turn is over, whatever its calls were left as.
                self.turn = Turn::Idle;
                self.turned = true;
                if !self.streaming.is_empty() {
                    self.last_row = std::mem::take(&mut self.streaming);
                }
                self.events
                    .push(Entry::of("turn", "interrupted").saying(one_line(&reason, LINE)));
            }
            SessionEvent::TurnFailed { error, .. } => {
                self.turn = Turn::Failed(error.clone());
                self.turned = true;
                self.streaming.clear();
                self.events.push(
                    Entry::of("failure", "failed")
                        .coded("turn_failed")
                        .saying(one_line(&error, LINE)),
                );
            }
            // The body rides its announcement when it fits and arrives as `TranscriptContent`
            // when it does not, so a committed row is read from whichever event carries it.
            SessionEvent::TranscriptContent { item, .. }
            | SessionEvent::TranscriptAppended {
                item: Some(item), ..
            } => {
                if let TranscriptItem::Assistant { text, .. } = item.as_ref()
                    && !text.trim().is_empty()
                {
                    self.last_row = text.clone();
                    if crate::report::parse(text).is_some() {
                        self.last_report = Some(text.clone());
                    }
                    // The committed row, and not the stream. A turn's deltas build the
                    // same words the row then carries; an entry per `TurnFinished` would say
                    // them a second time, and one per delta would be a transcript.
                    self.events
                        .push(Entry::of("message", "").saying(one_line(text, MESSAGE_LINE)));
                }
            }
            SessionEvent::ToolCallProposed {
                call_id,
                name,
                target,
                ..
            } => {
                // One entry per call, from the proposal. `ToolStarted` names the same
                // call again with no argument, so folding both would draw every call twice
                // and the second line would be the less useful of the two.
                self.events.push(
                    Entry::of("tool", "proposed")
                        .named(name.clone())
                        .saying(one_line(&target, LINE)),
                );
                self.calls.insert(call_id, name);
                // A session that runs a thousand calls must not grow a map for ever; the
                // asks that are open are the only ones anything looks a name up for.
                if self.calls.len() > 256 {
                    self.calls.clear();
                }
            }
            SessionEvent::ToolFinished {
                call_id, outcome, ..
            } => {
                let tool = self.calls.get(&call_id).cloned().unwrap_or_default();
                // A backgrounded call is the job STARTING: there is no `JobStarted` on
                // the wire, and the handle only exists here and on `JobSettled`.
                if let ToolOutcome::Backgrounded {
                    handle, ran_for_ms, ..
                } = &outcome
                {
                    self.jobs_open.insert(handle.clone());
                    self.events.push(
                        Entry::of("job", "started")
                            .named(handle.clone())
                            .lasting(*ran_for_ms),
                    );
                } else {
                    let text = match &outcome {
                        ToolOutcome::Abstained { reason }
                        | ToolOutcome::Failed { reason }
                        | ToolOutcome::NotRun { why: reason } => one_line(reason, LINE),
                        ToolOutcome::Denied { req_id } => {
                            one_line(&format!("denied: {req_id}"), LINE)
                        }
                        _ => String::new(),
                    };
                    self.events.push(
                        Entry::of("result", outcome_word(&outcome))
                            .named(tool)
                            .saying(text),
                    );
                }
            }
            SessionEvent::JobSettled {
                job,
                state,
                elapsed_ms,
                ..
            } => {
                // Whether this settlement holds the work open is the daemon's - it publishes
                // `Delivery` the moment the result is queued, and `Job(j)` only while the job is
                // one the plan waits on. This head keeps the list a reader sees and nothing else.
                self.jobs_open.remove(&job);
                self.events.push(
                    Entry::of("job", "ended")
                        .named(job)
                        .saying(one_line(&state, LINE))
                        .lasting(elapsed_ms),
                );
            }
            // What the session is waiting on (`PROTOCOL_VERSION` 52) - the whole of the
            // replacement for the four guesses `State`'s `activity` doc names. It is read, never
            // derived: nothing here compares a job against a todo row or times a quiet turn.
            SessionEvent::ActivityChanged { activity } => {
                if activity != self.activity {
                    self.activity = activity.clone();
                    self.events
                        .push(Entry::of("activity", "").saying(one_line(&activity.line(), LINE)));
                }
            }
            SessionEvent::Warning { code, detail, .. } => {
                // A boot that failed is terminal for the whole session, not one turn: no
                // turn can start on a daemon whose session never opened. Recorded so `wait`
                // can answer with it instead of timing out.
                if code == "daemon_start_failed" {
                    self.boot_failed = Some(detail.clone());
                }
                // A `/` verb's reply, kept whole. The ring below keeps one clamped line of it,
                // and for a reply that line is the echo, so a caller that asked a question
                // (`set_model`) would have the request and not the answer.
                if code == "slash" || code == "slash_refused" {
                    self.slash = Some(SlashSaid::of(&detail, code == "slash"));
                    self.slashes += 1;
                }
                self.events.push(
                    Entry::of("warning", "")
                        .coded(code)
                        .saying(one_line(&detail, LINE)),
                );
            }
            // The provider itself, typed. A warning is a sentence and stays one; this is the
            // same fact in the shape an automation branches on, and the last one is what
            // `leticode_status` answers with.
            SessionEvent::ProviderTrouble {
                kind,
                model,
                detail,
                reset_ms,
                count,
                switched_to,
            } => {
                if !model.is_empty() {
                    self.model = model.clone();
                }
                let mut entry = Entry::of("provider", trouble_word(&kind))
                    .named(model.clone())
                    .saying(one_line(&detail, LINE))
                    .coded(kind.clone());
                if let Some(at) = reset_ms {
                    entry = entry.lifting_at(at);
                }
                self.events.push(entry);
                // Where the daemon's own policy moved the session, said as one more line: a caller
                // that reacts to a quota by switching would otherwise be blind to it having happened.
                if let Some(to) = &switched_to {
                    self.events.push(
                        Entry::of("model", "switched")
                            .named(to.clone())
                            .coded(kind.clone()),
                    );
                }
                self.last_trouble = Some(Trouble {
                    kind,
                    model,
                    detail,
                    reset_ms,
                    count,
                    switched_to,
                });
                self.troubles += 1;
            }
            SessionEvent::DecisionRequested {
                req_id,
                kind,
                call_id,
                access,
                summary,
                target,
                options,
                options_note,
                choices,
                deadline,
                ..
            } => {
                let tool = call_id
                    .as_ref()
                    .and_then(|id| self.calls.get(id))
                    .cloned()
                    .unwrap_or_default();
                // The command, path or URL is the one thing being decided about, so it is
                // what the entry says; a card whose target is empty (a `web_search`, a
                // question) falls back to the sentence the gate wrote.
                let said = if target.trim().is_empty() {
                    &summary
                } else {
                    &target
                };
                self.events.push(
                    Entry::of("ask", "opened")
                        .named(req_id.clone())
                        .coded(kind.clone())
                        .saying(one_line(said, LINE)),
                );
                self.asks.retain(|a| a.req_id != req_id);
                self.asks.push(Ask {
                    req_id,
                    kind,
                    call_id,
                    tool,
                    summary,
                    target,
                    access,
                    options: options
                        .iter()
                        .map(|o| AskOption {
                            option_id: o.option_id.clone(),
                            label: o.label.clone(),
                            kind: option_kind(o.kind),
                        })
                        .collect(),
                    options_note: options_note.clone(),
                    choices,
                    deadline,
                });
            }
            SessionEvent::DecisionAnswered {
                req_id,
                outcome,
                by,
                ..
            } => {
                let said = match &outcome {
                    letibot_sessionlog::event::DecisionOutcome::Selected { option_id } => {
                        option_id.clone()
                    }
                    letibot_sessionlog::event::DecisionOutcome::Cancelled => "cancelled".into(),
                    letibot_sessionlog::event::DecisionOutcome::TimedOut => "timed out".into(),
                };
                self.events.push(
                    Entry::of("ask", "answered")
                        .named(req_id.clone())
                        .coded(by.kind.clone())
                        .saying(one_line(&said, LINE)),
                );
                self.asks.retain(|a| a.req_id != req_id);
            }
            SessionEvent::TodosUpdated { todos } => self.todos = todos,
            // A reviewer's start and end, on the parent's own hub - the one thing a head
            // attached to a session that spawns children is told about them, and the reason
            // `leticode_events` on the review queue can say *who is reviewing what* at all.
            SessionEvent::Subagent {
                subagent_id,
                state,
                prompt,
                role: _,
                task,
                model: _,
                answer,
                slug,
            } => self.child(
                Child {
                    id: &subagent_id,
                    slug: &slug,
                    state: &state,
                    task: &task,
                    prompt: &prompt,
                    answer: answer.as_deref(),
                },
                false,
            ),
            _ => {}
        }
    }
}

/// The last assistant row's text in a snapshot - newest first, and an empty row (a call
/// with no prose) is skipped rather than counted as the last thing said.
fn last_assistant(items: &[SnapshotItem]) -> Option<String> {
    items.iter().rev().find_map(|i| match i.item.as_ref() {
        Some(TranscriptItem::Assistant { text, .. }) if !text.trim().is_empty() => {
            Some(text.clone())
        }
        _ => None,
    })
}

/// A question's answer from the bridge's words: a choice by its exact text or its
/// 1-based number, `free:<words>` for a typed reply, `abstain` for the deliberate no-answer.
/// A note qualifies a choice and is refused on its own, as the daemon refuses it.
fn question_answer(
    option: &str,
    note: Option<&str>,
    choices: &[String],
) -> Result<letibot_sessionlog::question::QuestionAnswer, String> {
    use letibot_sessionlog::question::QuestionAnswer;
    let note = note
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string);
    if option == "abstain" {
        return Ok(QuestionAnswer {
            abstain: true,
            ..QuestionAnswer::default()
        });
    }
    if let Some(words) = option.strip_prefix("free:") {
        return Ok(QuestionAnswer {
            free: Some(words.trim().to_string()),
            ..QuestionAnswer::default()
        });
    }
    let index = choices
        .iter()
        .position(|c| c == option)
        .or_else(|| {
            option
                .parse::<usize>()
                .ok()
                .filter(|n| *n >= 1 && *n <= choices.len())
                .map(|n| n - 1)
        })
        .ok_or_else(|| {
            let listed: Vec<String> = choices
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{}. {c}", i + 1))
                .collect();
            format!(
                "`{option}` is not one of the question's choices; it offers {} — or answer \
                 `free:<words>`, or `abstain`",
                listed.join("; ")
            )
        })?;
    Ok(QuestionAnswer {
        option: Some(index),
        note,
        ..QuestionAnswer::default()
    })
}

/// What a caller waiting on a session learns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attention {
    /// The turn is over. The words are the last assistant message, as it stands.
    TurnEnded { message: String },
    /// The session is stopped on a card and every further step waits for an answer. Boxed
    /// because a card is a whole `DecisionRequested`, and every wake-up that is not a card
    /// would otherwise pay for the size of the one that is.
    Ask(Box<Ask>),
    /// The turn ended in a failure.
    Failed { why: String },
    /// The provider itself is stuck: a rate limit, quota, timeout or stall, as the daemon typed
    /// it. Boxed for [`Attention::Ask`]'s reason. A caller that must wait out a limit reads `reset_ms`.
    Trouble(Box<Trouble>),
    /// The caller's deadline passed with nothing to report.
    Timeout,
}

struct Shared {
    state: Mutex<State>,
    bell: Condvar,
}

/// One attached head, and the session it holds.
pub struct Head {
    pub session: String,
    pub worktree: PathBuf,
    pub socket: PathBuf,
    /// This head is watching and will not touch the session: it never sends a
    /// prompt, never answers a card and never interrupts. Declared as `can_decide: false`,
    /// because the one thing a head must not do to another session is act on it.
    pub watch_only: bool,
    /// Where this session's tools run, from the daemon's own record at attach
    /// ([`crate::daemon::Daemon::placement`]): `host`, `host:NAME`, or `None` when nothing
    /// readable was recorded. Held, not looked up per answer.
    pub placement: Option<Placement>,
    shared: Arc<Shared>,
    /// Shared with the collecting thread because an `Ack` is a client frame: a head that
    /// acked from a second borrow of the socket would be two writers on one stream.
    client: Arc<Mutex<HeadClient>>,
}

impl Head {
    /// Attach as a head and start collecting. `since_seq = 0` asks for the whole
    /// snapshot, which is what a head that has just started needs: the asks already open,
    /// the turn already running and the last answer it missed are all in it.
    pub fn attach(d: &Daemon, session: &str) -> Result<Head, String> {
        Head::attach_with(d, session, false)
    }

    /// Attach to a session somebody else is driving, and only watch it. It gets
    /// everything [`Head::attach`] gives and none of the three verbs that act on it; a card
    /// still reaches `wait`, and `can_decide: false` keeps it from counting as an answerer.
    pub fn attach_watching(d: &Daemon, session: &str) -> Result<Head, String> {
        Head::attach_with(d, session, true)
    }

    fn attach_with(d: &Daemon, session: &str, watch_only: bool) -> Result<Head, String> {
        let caps = Caps {
            can_decide: !watch_only,
            row_bodies: true,
            ..Caps::default()
        };
        let (client, hello, reader) =
            HeadClient::attach(&d.socket, session, 0, HEAD_KIND, IDENTITY, caps)
                .map_err(|e| said(&d.socket, e))?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                session: session.to_string(),
                ..State::default()
            }),
            bell: Condvar::new(),
        });
        let client = Arc::new(Mutex::new(client));
        {
            let mut g = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            g.fold(hello);
        }
        collect(reader, shared.clone(), client.clone());
        // The todo board is not in the snapshot (`ClientFrame::ListTodos` says why: a head
        // attaching fresh has no `TodosUpdated` to replay), so it is asked for once.
        {
            let mut c = client.lock().unwrap_or_else(|e| e.into_inner());
            let _ = c.list_todos();
            // Nor are the running jobs, and `wait` attaches fresh every time: without the
            // list, a turn that ended to wait on a job would read as the work being done.
            let _ = c.list_jobs();
        }
        let head = Head {
            session: session.to_string(),
            worktree: d.workspace.clone(),
            socket: d.socket.clone(),
            watch_only,
            // The daemon's own answer, read when it was resolved. A head that attached to a
            // daemon whose record it could not read says `None` - *not known* - rather than
            // inventing this box.
            placement: d.placement.clone(),
            shared,
            client,
        };
        head.seated_on(session)?;
        Ok(head)
    }

    /// The head is on the session it was asked for, or this says so. The first real
    /// `leticode_start` sent a brief into one session while `leticode_status` watched
    /// another, so a difference is corrected with `Switch` rather than assumed away.
    fn seated_on(&self, want: &str) -> Result<(), String> {
        if want.is_empty() {
            // A bare attach asks for the daemon's own choice, so there is nothing to check
            // and nothing to correct.
            return Ok(());
        }
        let mut on = {
            let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            g.session.clone()
        };
        if on == want {
            return Ok(());
        }
        let mut c = self.client.lock().unwrap_or_else(|e| e.into_inner());
        // `since_seq = 0` on a switch asks for the whole snapshot, which is what a head that
        // has just attached needs from the session it has just moved to.
        c.switch(want, 0).map_err(|e| e.to_string())?;
        drop(c);
        let deadline = Instant::now() + SEAT_PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            on = g.session.clone();
            let closed = g.closed.clone();
            if on == want {
                return Ok(());
            }
            // A connection the daemon has closed is not one a later frame will arrive on,
            // so the wait ends with the daemon's own reason rather than the patience running
            // out five seconds later.
            if let Some(why) = closed {
                return Err(format!(
                    "the daemon was asked for session `{want}` and closed the connection \
                     instead: {why}"
                ));
            }
            if left.is_zero() {
                break;
            }
            let (next, _) = self
                .shared
                .bell
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner());
            // The guard is one turn of this loop's business; waiting again under it would
            // keep the fold's thread out of the state it is trying to write.
            drop(next);
        }
        Err(format!(
            "the daemon seated this head on session `{on}` when `{want}` was asked for, and \
             did not move it to `{want}` within {SEAT_PATIENCE:?}"
        ))
    }

    /// A copy of everything this head holds - the shape `leticode_status` answers with.
    pub fn snapshot(&self) -> State {
        let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        State {
            session: g.session.clone(),
            seq: g.seq,
            turn: g.turn.clone(),
            turn_id: g.turn_id.clone(),
            last_row: g.last_row.clone(),
            streaming: g.streaming.clone(),
            asks: g.asks.clone(),
            todos: g.todos.clone(),
            meter: g.meter,
            spent: g.spent,
            rounds: g.rounds,
            // Carried: `leticode_status` answers both, and neither is a payload.
            prefix_check: g.prefix_check.clone(),
            dialect_template_sha: g.dialect_template_sha.clone(),
            last_report: g.last_report.clone(),
            calls: g.calls.clone(),
            children: g.children.clone(),
            jobs_open: g.jobs_open.clone(),
            activity: g.activity.clone(),
            turned: g.turned,
            events: g.events.clone(),
            // Not carried across: the peeked rows and the mode answers are read by the two
            // verbs that ask for them ([`Head::rows`], [`Head::ask_mode`]), and a transcript
            // cloned on every status poll would be work nobody asked for.
            peek: None,
            peeks: g.peeks,
            peek_error: None,
            mode_settled: Vec::new(),
            close_acks: Vec::new(),
            closed: g.closed.clone(),
            boot_failed: g.boot_failed.clone(),
            rejection: g.rejection.clone(),
            // The last `/` reply is read by the one verb that asked (`set_model`), which holds the
            // whole `said`; a status copy carries none of it, on the same argument as `peek`.
            slash: None,
            slashes: g.slashes,
            // Carried, unlike the peek: `leticode_status` answers both, and neither is a payload.
            model: g.model.clone(),
            last_trouble: g.last_trouble.clone(),
            troubles: g.troubles,
            skew: g.skew.clone(),
            unreadable: g.unreadable,
        }
    }

    /// The progress since a cursor: the entries, the cursor to ask from next, and how
    /// many fell off the ring before the caller could reach them. Taken under the state's
    /// own lock, because a ring cloned on every status poll is work nobody asked for.
    pub fn events(&self, since: u64, limit: usize) -> Page {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .events
            .page(since, limit)
    }

    /// The asks still owed an answer, oldest first.
    pub fn asks(&self) -> Vec<Ask> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .asks
            .clone()
    }

    /// A line to the running session: queued behind a running turn, as a head's own
    /// prompt is (the protocol's `Prompt` is a follow-up item, never a rejection).
    pub fn prompt(&self, text: &str) -> Result<String, String> {
        self.may_write()?;
        let mut c = self.said_client()?;
        // `expected_seq = 0` is what the protocol reads as *I am not claiming a position*,
        // and `Hub::submit` accepts a prompt on a stale seq anyway. A bridge is a second
        // head on a session a person may be typing into, so it must never drop what it was told.
        c.prompt(0, text).map_err(|e| e.to_string())
    }

    /// Answer a card, by an option the card itself offered. The check is the bridge's as
    /// well as the daemon's, because the two failures differ: an option the ask never had is
    /// refused here with the list, and one the daemon no longer wants arrives as a `Rejected`.
    pub fn answer(&self, ask: &str, option: &str, note: Option<&str>) -> Result<(), String> {
        // Before the ask is looked for. A caller that tried to answer a watched session's
        // card must be told the bridge is not holding it, not that the card is gone - the
        // card is very likely still open, and somebody else is going to settle it.
        self.may_write()?;
        let (kind, offered, choices): (String, Vec<String>, Vec<String>) = {
            let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(found) = g.asks.iter().find(|a| a.req_id == ask) else {
                let open: Vec<String> = g.asks.iter().map(|a| a.req_id.clone()).collect();
                return Err(format!(
                    "no ask `{ask}` is open on this session; it holds {}",
                    if open.is_empty() {
                        "none".to_string()
                    } else {
                        open.join(", ")
                    }
                ));
            };
            (
                found.kind.clone(),
                found.options.iter().map(|o| o.option_id.clone()).collect(),
                found.choices.clone(),
            )
        };
        // A question carries the model's own choices, and is answered with
        // `AnswerQuestion`: `option` names a choice by its exact words or its 1-based number,
        // `free:<words>` is a typed reply, `abstain` the deliberate no-answer.
        if kind == "question" {
            let answer =
                question_answer(option, note, &choices).map_err(|e| format!("ask `{ask}`: {e}"))?;
            let mut c = self.said_client()?;
            return c
                .answer_question(ask, answer)
                .map(|_| ())
                .map_err(|e| e.to_string());
        }
        // A question is not a permission, and the two have different frames. Sending an
        // `Answer` for a question would be refused by the daemon for a reason that names
        // neither - so it is refused here, saying what could answer it.
        if kind != "permission" {
            return Err(format!(
                "ask `{ask}` is a {kind}: only permissions and questions can be answered from \
                 the bridge"
            ));
        }
        if !offered.iter().any(|o| o == option) {
            return Err(format!(
                "`{option}` is not an option of ask `{ask}`; it offers {}",
                offered.join(", ")
            ));
        }
        let mut c = self.said_client()?;
        c.answer_with(ask, option, None, note)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Interrupt whatever is running, then close the session here and leave. The rows stay on disk
    /// for any head, and the close is what settles a placement: a detach alone leaves the daemon
    /// holding the harness.
    pub fn stop(&self) -> Result<(), String> {
        self.stop_within(CLOSE_PATIENCE)
    }

    /// [`Head::stop`] with the wait named. Returns once the daemon acked the close, and an `Err`
    /// naming the wait when it did not, so a caller never reports a close that was not seen.
    pub fn stop_within(&self, patience: Duration) -> Result<(), String> {
        self.may_write()?;
        let id = {
            let mut c = self.said_client()?;
            let who = c.identity().to_string();
            c.interrupt(0, "the bridge was asked to stop this session")
                .map_err(|e| e.to_string())?;
            c.close_session(
                0,
                &who,
                "the bridge was asked to stop it, so its placement could come home",
            )
            .map_err(|e| e.to_string())?
        };
        let deadline = Instant::now() + patience;
        loop {
            let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if g.close_acks.contains(&id) {
                drop(g);
                // A daemon that shuts down with its last session ends this connection with a
                // goodbye right behind the ack: the close was seen, and there is nothing to detach.
                let Ok(mut c) = self.said_client() else {
                    return Ok(());
                };
                return match c.detach() {
                    Ok(()) => Ok(()),
                    Err(_) if self.has_closed() => Ok(()),
                    Err(e) => Err(e.to_string()),
                };
            }
            if let Some(why) = &g.rejection {
                return Err(format!("the daemon refused the close: {why}"));
            }
            // The hub closes with the session and the daemon ends this connection with a `Bye`,
            // which can overtake the ack: either way the session is closed.
            if g.closed.is_some() {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!(
                    "the close was sent but the daemon did not ack it within {patience:?}, so the \
                     session may not be closed yet"
                ));
            }
            let (next, _) = self
                .shared
                .bell
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner());
            drop(next);
        }
    }

    /// Ask the daemon to move this session to an approval point. Queued like a prompt
    /// and effective from the next call, so a `start` sends it before the brief. `consented:
    /// false` always: a bridge is not a person, and `allow-all` needs one.
    pub fn set_mode(&self, mode: &str) -> Result<(), String> {
        self.may_write()?;
        let mut c = self.said_client()?;
        c.set_mode(0, mode, false)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Put this session on the model the caller named: the daemon's `/models` verb, which
    /// is per SESSION. It writes the session's own row, so a restart restores it; a refusal
    /// is the answer, not a silence, because a brief must not run on a model nobody asked for.
    pub fn set_model(&self, want: &str) -> Result<String, String> {
        self.set_model_within(want, MODEL_PATIENCE)
    }

    /// [`Head::set_model`] with the wait named, so the silence it reports is testable.
    pub fn set_model_within(&self, want: &str, patience: Duration) -> Result<String, String> {
        self.may_write()?;
        let line = format!("models {}", want.trim());
        let before = {
            let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            g.slashes
        };
        {
            let mut c = self.said_client()?;
            c.slash(0, &line).map_err(|e| e.to_string())?;
        }
        let deadline = Instant::now() + patience;
        loop {
            let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if g.slashes > before
                && let Some(said) = g.slash.clone()
                && said.line == line
            {
                return if said.ok {
                    Ok(said.said)
                } else {
                    Err(said.said)
                };
            }
            if let Some(why) = &g.closed {
                return Err(format!("the daemon closed this connection: {why}"));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!(
                    "the daemon is up and kept this connection, but its session did not answer \
                     `/{line}` within {patience:?}: the session's worker was still opening (a \
                     host placement's first open, or a busy box). The request was sent once and \
                     may still apply; ask `leticode_status` before repeating it"
                ));
            }
            let (next, _) = self
                .shared
                .bell
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner());
            drop(next);
        }
    }

    /// The session's own rows, as the daemon serves them to a head - `leticode_transcript`.
    /// `Peek { shape: Rows }` is *what an attach to this session would be handed*, and it is
    /// asked every time: this head holds no transcript, so a caller's cursor is the daemon's own.
    pub fn rows(&self, session: &str, patience: Duration) -> Result<Snapshot, String> {
        let (before, was, closed) = {
            let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            (g.peeks, g.peek_error.clone(), g.closed.clone())
        };
        if let Some(why) = closed {
            return Err(format!("the daemon closed this connection: {why}"));
        }
        {
            let mut c = self.said_client()?;
            c.peek(session, PeekShape::Rows)
                .map_err(|e| format!("asking the daemon for `{session}`'s rows: {e}"))?;
        }
        let deadline = Instant::now() + patience;
        loop {
            let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            // A refusal, said rather than waited out. The daemon mints `peek:<session>` as
            // the id of a refused peek, and the fold keeps it - so a session this daemon does
            // not hold answers with its own sentence instead of a timeout ten seconds later.
            if let Some((on, why)) = g.peek_error.clone()
                && on == session
                && g.peek_error != was
            {
                return Err(format!("the read of `{session}`'s rows was refused: {why}"));
            }
            if g.peeks > before
                && let Some((on, snap)) = &g.peek
                && on == session
            {
                return Ok(snap.clone());
            }
            if let Some(why) = &g.closed {
                return Err(format!("the daemon closed this connection: {why}"));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let (next, _) = self
                .shared
                .bell
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner());
            drop(next);
        }
        Err(format!(
            "the daemon did not answer the read of `{session}`'s rows within {patience:?} — the \
             socket is up, so this is a daemon that is not serving that session"
        ))
    }

    /// Ask the daemon to put a confirmation in front of a person - `leticode_mode`.
    /// The one route to `allow-all` this bridge has, and deliberately not a set: the daemon
    /// lets any head attached to the session EXCEPT this one answer, so the answer is a person's.
    pub fn ask_mode(&self, name: &str, patience: Duration) -> Result<ModeSettled, String> {
        self.may_write()?;
        // What this head had already been refused, so a refusal from BEFORE the ask is not read
        // as the answer to it. The tools are called one at a time by one client, so the only
        // `Rejected` that can arrive between the send and the answer is this request's.
        let refused_before = {
            let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            g.rejection.clone()
        };
        let id = {
            let mut c = self.said_client()?;
            // `expected_seq = 0` for `prompt`'s reason: a request is not a position, and a
            // bridge must never be the head that loses a race to a person's typing.
            c.mode_confirm(0, name).map_err(|e| e.to_string())?
        };
        let deadline = Instant::now() + patience;
        loop {
            let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, settled)) = g.mode_settled.iter().find(|(on, _)| on == &id) {
                return Ok(settled.clone());
            }
            // A refusal is the answer. The daemon writes a `Rejected` on this connection
            // when nobody is attached to ask, and the sentence names the command that opens a
            // head - the caller's next move - so it is returned as itself.
            if let Some(why) = g.rejection.clone()
                && g.rejection != refused_before
            {
                return Err(why);
            }
            if let Some(why) = &g.closed {
                return Err(format!("the daemon closed this connection: {why}"));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let (next, _) = self
                .shared
                .bell
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner());
            drop(next);
        }
        Err(format!(
            "the `{name}` confirmation was put in front of the session's heads and nobody \
             answered within {patience:?}. Nothing was changed: the request is still open on the \
             daemon, and an answer that arrives later applies the point with nobody listening"
        ))
    }

    /// The sentence a watching head answers every verb with, and `Ok` for a head that holds
    /// its session. It names both facts - this head attached read-only, and the head that does
    /// hold the session is somebody else's - because `refused` alone explains nothing.
    fn may_write(&self) -> Result<(), String> {
        if !self.watch_only {
            return Ok(());
        }
        Err(format!(
            "`{}` is watched, not held: this bridge attached to it with `watch_only` (the \
             default), so it sends no prompt, answers no card, changes no approval point and \
             interrupts nothing — the head that holds the session is somebody else's, and a \
             second one acting on it would be the bridge deciding for the operator. Attach \
             with `watch_only: false` if you mean to drive it",
            self.session
        ))
    }

    /// The client, or the reason a frame cannot be sent: a connection the daemon closed
    /// answers every send with the closing sentence rather than with a dead-socket error
    /// that names nothing.
    fn has_closed(&self) -> bool {
        let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        g.closed.is_some()
    }

    fn said_client(&self) -> Result<std::sync::MutexGuard<'_, HeadClient>, String> {
        let closed = {
            let g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            g.closed.clone()
        };
        match closed {
            Some(why) => Err(format!("the daemon closed this connection: {why}")),
            None => Ok(self.client.lock().unwrap_or_else(|e| e.into_inner())),
        }
    }

    /// Block until the session needs somebody, or the deadline passes. An open ask wins
    /// over everything, then a failure, then the daemon's own [`Activity`] saying the session
    /// is quiet. A session that has never run a turn is never answered as ended.
    pub fn wait(&self, deadline: Instant) -> Attention {
        self.wait_with(deadline, false)
    }

    /// [`Head::wait`], choosing whether a `stall` ends it. A stall is a round that is slow with its
    /// request still open, so by default it is ridden out: a stall that lasts becomes a `timeout`
    /// trouble when the transport gives up, and that one always ends the wait.
    pub fn wait_with(&self, deadline: Instant, exit_on_stall: bool) -> Attention {
        let mut g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let baseline = match &g.turn {
            Turn::Failed(why) => Some(why.clone()),
            _ => None,
        };
        // Only a trouble this wait saw. A provider that was rate limited ten minutes ago is not
        // news to a wait that started a minute ago, and answering with it would wake an orchestrator
        // for a session that has since answered.
        let mut troubles = g.troubles;
        // Whether this wait saw the session working. Turn and calls only: `delivery` says
        // a turn is OWED, which is not one having run. A job or a child counts too, because
        // its settlement wakes the model and this wait will see the turn it starts.
        let mut armed = g.activity.waiting_on().iter().any(|w| {
            matches!(
                w,
                Waiting::Turn | Waiting::Calls | Waiting::Job(_) | Waiting::Child(_)
            )
        });
        let grace = Instant::now() + START_GRACE;
        loop {
            if let Some(a) = g.asks.first() {
                return Attention::Ask(Box::new(a.clone()));
            }
            // A daemon whose boot failed is not waited on. It is not a failure of
            // THIS wait's turn, so the baseline rule below does not cover it, and the session
            // can never produce what this loop waits for.
            if let Some(why) = &g.boot_failed {
                return Attention::Failed { why: why.clone() };
            }
            // And a connection the daemon closed is the end of the wait: nothing else will
            // ever arrive on it. A wait that ignored `closed` sat on a dead socket until its
            // deadline, which is the hour two packages' orchestrator spent.
            if let Some(why) = &g.closed {
                return Attention::Failed {
                    why: format!("the daemon closed this connection: {why}"),
                };
            }
            if let Turn::Failed(why) = &g.turn
                && baseline.as_deref() != Some(why.as_str())
            {
                return Attention::Failed { why: why.clone() };
            }
            // The provider is stuck, and this wait has not said so yet. Asked after the failure
            // because a turn that died of the trouble says why in one sentence a caller can act on;
            // asked before quiet, because a session told the provider is rate limited is not *done*.
            if g.troubles > troubles
                && let Some(t) = &g.last_trouble
            {
                if exit_on_stall || t.kind != "stall" {
                    return Attention::Trouble(Box::new(t.clone()));
                }
                troubles = g.troubles;
            }
            let now = Instant::now();
            if g.activity.is_quiet() {
                if armed {
                    // Quiet is the answer: nothing is running, nothing is owed, nobody was
                    // asked. That is what the orchestrator needs, and it is a fact the daemon
                    // stated rather than a window this head sat out.
                    return Attention::TurnEnded {
                        message: g.last_message().to_string(),
                    };
                }
                if g.turned && now >= grace {
                    // Nothing ran during this wait, and something has run before: this session
                    // was already between turns when the caller got here, so the honest answer
                    // is the last thing it said.
                    return Attention::TurnEnded {
                        message: g.last_message().to_string(),
                    };
                }
            } else if g.activity.waiting_on().iter().any(|w| {
                matches!(
                    w,
                    Waiting::Turn | Waiting::Calls | Waiting::Job(_) | Waiting::Child(_)
                )
            }) {
                armed = true;
            }
            if now >= deadline {
                return Attention::Timeout;
            }
            // A session that has never turned waits the whole deadline: there is no grace
            // question to ask about a session with no turn to have ended.
            let until = if armed || !g.turned {
                deadline
            } else {
                deadline.min(grace)
            };
            let wait = until.saturating_duration_since(now);
            let (next, _) = self
                .shared
                .bell
                .wait_timeout(g, wait)
                .unwrap_or_else(|e| e.into_inner());
            g = next;
        }
    }
}

/// A head that nobody holds lets go of the daemon. The collecting threads keep the socket
/// alive, so dropping the last handle without shutting it down would leave the daemon with a
/// head it believes is watching and a socket that never reaches EOF - and `shutdown` joins.
impl Drop for Head {
    fn drop(&mut self) {
        if let Ok(mut c) = self.client.lock() {
            let _ = c.shut_down();
        }
    }
}

/// Read frames off the socket forever, folding them and acking what was handled. The reader is
/// `pump`, which hands over a line this build cannot read instead of ending the connection: a
/// bridge that exited on the first unknown frame would look like a daemon that stopped answering.
fn collect(
    reader: letibot_sessionlog::wire::FrameReader<std::os::unix::net::UnixStream>,
    shared: Arc<Shared>,
    client: Arc<Mutex<HeadClient>>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(format!("mcp-read-{}", std::process::id()))
        .spawn(move || pump(reader, tx))
        .ok();
    std::thread::Builder::new()
        .name(format!("mcp-fold-{}", std::process::id()))
        .spawn(move || {
            while let Ok(inbound) = rx.recv() {
                let ack = match inbound {
                    Inbound::Frame(frame) => {
                        let seq = if let ServerFrame::Event(ref env) = frame {
                            Some(env.seq)
                        } else {
                            None
                        };
                        {
                            let mut g = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                            g.fold(frame);
                        }
                        shared.bell.notify_all();
                        seq
                    }
                    // The connection stays up and the next frame is very likely readable, so
                    // the line is skipped and counted for `leticode_status`.
                    Inbound::Unreadable(_) => {
                        let mut g = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                        g.unreadable += 1;
                        None
                    }
                };
                if let Some(seq) = ack {
                    let mut c = client.lock().unwrap_or_else(|e| e.into_inner());
                    // Acked after the fold, which is the head's obligation: a crash then
                    // costs a duplicate frame and never a silence.
                    if c.ack(Ack {
                        seq,
                        rendered: 1,
                        filtered: 0,
                    })
                    .is_err()
                    {
                        return;
                    }
                }
            }
        })
        .ok();
}

/// The sentence a refused attach gets: the daemon's own reason when it gave one, and the
/// socket it came from either way, because the usual cause is looking in the wrong folder.
fn said(socket: &Path, e: ClientError) -> String {
    match e {
        ClientError::Refused(reason) => format!(
            "the daemon on {} refused the attach: {reason}",
            socket.display()
        ),
        other => format!("attaching to {}: {other}", socket.display()),
    }
}

/// The session a bare attach lands on - the daemon's own choice, which for a daemon that has
/// just started is the one session it opened eagerly. A bridge that started the daemon uses
/// this rather than minting a second one: one brief is one conversation.
pub fn open_default(d: &Daemon) -> Result<String, String> {
    let (mut client, hello, _reader) = HeadClient::attach(
        &d.socket,
        "",
        0,
        HEAD_KIND,
        IDENTITY,
        Caps {
            can_decide: false,
            ..Caps::default()
        },
    )
    .map_err(|e| said(&d.socket, e))?;
    let id = match hello {
        ServerFrame::Hello { session_id, .. } => session_id,
        other => return Err(format!("expected Hello, got {other:?}")),
    };
    let _ = client.detach();
    Ok(id)
}

/// Make a session in this daemon and hand back its id. `NewSession` does not switch, so
/// this is a control connection that creates and leaves, and the head that will hold the
/// session attaches afterwards: the socket a head holds must be that session's from frame one.
pub fn create_session(d: &Daemon, title: &str) -> Result<String, String> {
    create_session_in(d, title, &d.workspace.display().to_string())
}

/// The same, with the tree the session is about named: a review's session stands in the pull
/// request's own worktree, which is not the daemon's workspace.
pub fn create_session_in(d: &Daemon, title: &str, workspace: &str) -> Result<String, String> {
    // A daemon always opens one session eagerly, so a bare attach lands somewhere; a daemon
    // that holds none yet is a build this cannot create a session on, and says so by name.
    let (mut client, hello, reader) = HeadClient::attach(
        &d.socket,
        "",
        0,
        HEAD_KIND,
        IDENTITY,
        Caps {
            can_decide: false,
            ..Caps::default()
        },
    )
    .map_err(|e| said(&d.socket, e))?;
    client.seated_by(&hello);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(format!("mcp-create-{}", std::process::id()))
        .spawn(move || pump(reader, tx))
        .ok();
    client
        .new_session(title, workspace)
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("the daemon did not answer the new-session request within 30s".into());
        }
        match rx.recv_timeout(left) {
            Ok(Inbound::Frame(ServerFrame::Sessions { created, .. })) => {
                let _ = client.detach();
                return created
                    .ok_or_else(|| "the daemon answered a new-session with no id".to_string());
            }
            Ok(Inbound::Frame(ServerFrame::Bye { reason })) => {
                return Err(format!("the daemon closed the connection: {reason}"));
            }
            Ok(_) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return Err("the daemon did not answer the new-session request within 30s".into());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("the daemon closed the connection".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A daemon on an older protocol number is named in the status, and a line this build cannot
    /// read is counted there instead of vanishing.
    #[test]
    fn skew_and_unreadable_lines_reach_the_status() {
        use std::io::{BufRead, BufReader, Write};
        let socket = letibot_sessionlog::testing::scratch_root()
            .join(format!("bridge-skew-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut lines = BufReader::new(stream.try_clone().expect("clone")).lines();
            let _attach = lines.next();
            let hello = ServerFrame::Hello {
                protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION - 1,
                session_id: "s".into(),
                head_id: "h1".into(),
                dropped: 0,
                snapshot: Some(Box::new(
                    letibot_sessionlog::view::SessionView::new(
                        "s",
                        letibot_sessionlog::view::ViewBounds::default(),
                    )
                    .snapshot(0, 0),
                )),
                resumed_from: None,
                scrubbed: letibot_sessionlog::ScrubReport::default(),
                wiring: letibot_sessionlog::SessionWiring::default(),
                sessions: Vec::new(),
            };
            let _ = writeln!(stream, "{}", serde_json::to_string(&hello).unwrap());
            let _ = writeln!(stream, "{{\"frame\":\"from_the_future\"}}");
            for _ in lines {}
        });
        let d = crate::daemon::Daemon {
            workspace: socket.clone(),
            key: "k".into(),
            socket,
            log: Default::default(),
            store: Default::default(),
            placement: None,
            started: false,
        };
        let h = Head::attach(&d, "s").expect("attach");
        let end = Instant::now() + Duration::from_secs(10);
        while h.snapshot().unreadable == 0 {
            assert!(
                Instant::now() < end,
                "the unreadable line was never counted"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let s = h.snapshot();
        assert_eq!(s.unreadable, 1);
        let skew = s.skew.expect("a daemon one protocol behind is named");
        assert!(
            skew.contains("OLDER") && skew.contains("letibot --stop"),
            "{skew}"
        );
    }

    /// A close the daemon never acks is reported with the wait, not as a closed session.
    #[test]
    fn a_close_nobody_acks_is_reported_as_not_confirmed() {
        let socket = letibot_sessionlog::testing::scratch_root()
            .join(format!("bridge-noack-{}.sock", std::process::id()));
        let hub = letibot_sessionlog::hub::Hub::new("s");
        let server = letibot_sessionlog::server::serve(hub, socket.clone()).expect("bind");
        let d = crate::daemon::Daemon {
            workspace: socket.clone(),
            key: "k".into(),
            socket,
            log: Default::default(),
            store: Default::default(),
            placement: None,
            started: false,
        };
        let h = Head::attach(&d, "s").expect("attach");
        // No time to be acked: the answer says so rather than claiming a close.
        let why = h
            .stop_within(Duration::ZERO)
            .expect_err("no time to be acked");
        assert!(why.contains("did not ack"), "{why}");
        server.shutdown();
    }

    /// A daemon that accepts the attach and never answers `/models`: the silence is reported with
    /// the wait that ran out and what that means, and the real wait is longer than one round trip.
    #[test]
    fn a_silent_session_is_reported_with_the_wait_and_the_cause() {
        let socket = letibot_sessionlog::testing::scratch_root()
            .join(format!("bridge-silent-{}.sock", std::process::id()));
        let server = letibot_sessionlog::server::serve(
            letibot_sessionlog::hub::Hub::new("s"),
            socket.clone(),
        )
        .expect("bind");
        let d = crate::daemon::Daemon {
            workspace: socket.clone(),
            key: "k".into(),
            socket,
            log: Default::default(),
            store: Default::default(),
            placement: None,
            started: false,
        };
        let h = Head::attach(&d, "s").expect("attach");
        let why = h
            .set_model_within("glm", Duration::from_millis(300))
            .expect_err("nobody answers");
        assert!(
            why.contains("300ms") && why.contains("still opening") && why.contains("once"),
            "{why}"
        );
        assert!(
            MODEL_PATIENCE >= Duration::from_secs(60),
            "a host's first open fits"
        );
        server.shutdown();
    }

    /// A question is answered by its choice, its number, typed words, or an abstention.
    #[test]
    fn a_question_is_answered_by_choice_number_words_or_abstention() {
        let choices = vec!["Lift it".to_string(), "Stop".to_string()];
        let by_text = question_answer("Stop", None, &choices).expect("a choice by its words");
        assert_eq!(by_text.option, Some(1));
        let by_number = question_answer("1", Some("and say why"), &choices).expect("by number");
        assert_eq!(by_number.option, Some(0));
        assert_eq!(by_number.note.as_deref(), Some("and say why"));
        let free = question_answer("free:neither", None, &choices).expect("typed words");
        assert_eq!(free.free.as_deref(), Some("neither"));
        assert!(
            question_answer("abstain", None, &choices)
                .expect("abstain")
                .abstain
        );
        let refused = question_answer("3", None, &choices).expect_err("no third choice");
        assert!(
            refused.contains("1. Lift it") && refused.contains("2. Stop"),
            "{refused}"
        );
    }
    use letibot_sessionlog::testing;
    use letibot_transcript::Backgrounding;

    /// One recorded event, as the fold receives it: the log's own envelope, never a
    /// hand-built shape - a head that folded something the daemon does not send would pass
    /// every test here and fail on the first real session.
    fn env(seq: u64, event: SessionEvent) -> Envelope {
        Envelope {
            session_id: "s-recorded".into(),
            seq,
            ts: 1_700_000_000_000 + seq,
            event,
        }
    }

    fn finished(call_id: &str, outcome: ToolOutcome) -> SessionEvent {
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: call_id.into(),
            outcome,
            payload_digest: "fnv1a:0000000000000000".into(),
            inline_bytes: 0,
            full_bytes: 0,
            spill: None,
            repairs: 0,
            edit: None,
        }
    }

    fn settled(job: &str, state: &str, elapsed_ms: u64) -> SessionEvent {
        SessionEvent::JobSettled {
            job: job.into(),
            state: state.into(),
            produced: 4096,
            elapsed_ms,
        }
    }

    fn assistant(text: &str) -> SessionEvent {
        SessionEvent::TranscriptContent {
            item_id: "tr.9".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: text.into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        }
    }

    /// A snapshot as the daemon cuts one: events folded by the view this crate does not own.
    /// Used where a test needs what a head that attached LATE was handed, which is the one path
    /// the fold alone cannot exercise.
    fn snapshot_of(events: Vec<SessionEvent>) -> Snapshot {
        let mut view = letibot_sessionlog::view::SessionView::new(
            "s-recorded",
            letibot_sessionlog::view::ViewBounds::default(),
        );
        for (i, event) in events.into_iter().enumerate() {
            view.apply(&env(i as u64, event));
        }
        view.snapshot(0, 0)
    }

    /// A round's end is not the turn's, and the head no longer has to work that out. The
    /// engine publishes `TurnFinished` per round and the round's calls run after it; what this
    /// reads now is the daemon's `Activity`, quiet only when there is nothing left.
    #[test]
    fn the_daemons_activity_says_what_the_rounds_events_cannot() {
        let mut state = State::default();
        let mut seq = 0;
        let mut feed = |state: &mut State, event: SessionEvent| {
            seq += 1;
            state.fold(ServerFrame::Event(env(seq, event)));
        };
        let activity = |waiting_on: Vec<Waiting>| SessionEvent::ActivityChanged {
            activity: Activity::of(waiting_on),
        };
        feed(&mut state, testing::turn_started("t1"));
        feed(&mut state, activity(vec![Waiting::Turn]));
        assert_eq!(state.word(), "running");

        // The round ended and its calls are running: still busy, and the round's own
        // `TurnFinished` below does not change that.
        feed(&mut state, activity(vec![Waiting::Calls]));
        feed(&mut state, testing::turn_finished("t1"));
        assert_eq!(
            state.word(),
            "running",
            "the round's calls are still to run"
        );

        // The next round starts, and the whole turn ends quiet.
        feed(&mut state, activity(vec![Waiting::Turn]));
        assert_eq!(state.word(), "running");
        feed(&mut state, testing::turn_finished("t1"));
        feed(&mut state, activity(Vec::new()));
        assert_eq!(state.word(), "idle", "nothing left: the turn is over");
        assert!(state.activity.is_quiet());

        // Published only when it changes is the daemon's, so a repeated value is not news
        // here either: the ring gains no second entry for a value it already held.
        let before = state.events.page(0, RING).next;
        feed(&mut state, activity(Vec::new()));
        assert_eq!(
            state.events.page(0, RING).next,
            before,
            "an unchanged value says nothing"
        );
    }

    /// A turn that ended to wait on its own job is not the end of the work, and whether a
    /// job holds the work open is the DAEMON's answer now. The job the model PARKED with
    /// `wake: "later"` is not something the work waits on and must not hold a later turn open.
    #[test]
    fn a_job_the_work_waits_on_keeps_the_session_busy_and_a_server_does_not() {
        let mut state = State::default();
        let mut seq = 0;
        let mut feed = |state: &mut State, event: SessionEvent| {
            seq += 1;
            state.fold(ServerFrame::Event(env(seq, event)));
        };
        let activity = |waiting_on: Vec<Waiting>| SessionEvent::ActivityChanged {
            activity: Activity::of(waiting_on),
        };
        feed(&mut state, testing::turn_started("t1"));
        feed(
            &mut state,
            testing::proposed_on("t1", "c1", "bash", "cargo test"),
        );
        feed(
            &mut state,
            finished(
                "c1",
                ToolOutcome::Backgrounded {
                    handle: "j9".into(),
                    ran_for_ms: 900,
                    how: Backgrounding::Asked,
                    next: "job_output j9".into(),
                },
            ),
        );
        feed(&mut state, testing::turn_finished("t1"));
        // An open job the model PARKED (`bash {background: true, wake: "later"}`), which is
        // what a dev server left running is: the daemon says the session is quiet, and this head
        // says exactly what the daemon said.
        feed(&mut state, activity(Vec::new()));
        assert_eq!(state.word(), "idle");
        assert!(state.jobs_open.contains("j9"), "the job is still running");
        assert!(
            !state.waiting_on_jobs(),
            "a job the model parked holds nothing open"
        );

        // The work waits on it now. The daemon re-reads the board and says `job j9`.
        feed(&mut state, activity(vec![Waiting::Job("j9".into())]));
        assert!(state.waiting_on_jobs(), "the plan depends on j9");
        assert_eq!(state.waiting_on(), vec!["job j9".to_string()]);

        // It settled, and the turn its settlement wakes has not started: the daemon says
        // `delivery`, which is the window QC's `wait` exited in.
        feed(&mut state, settled("j9", "exited 0", 60_000));
        assert!(state.jobs_open.is_empty(), "its settlement closes it");
        feed(&mut state, activity(vec![Waiting::Delivery]));
        assert!(!state.activity.is_quiet(), "the wake is owed");
        assert!(
            !state.waiting_on_jobs(),
            "the job is over; what is left is the turn it wakes"
        );

        feed(&mut state, testing::turn_started("t1"));
        feed(&mut state, activity(vec![Waiting::Turn]));
        assert_eq!(state.word(), "running", "the wake started");
    }

    /// A head that attaches mid-turn is not blind: the value travels in the snapshot, so a
    /// `wait` that attached as the brief started sees `turn` rather than an idle session.
    #[test]
    fn a_snapshot_carries_the_activity_it_was_taken_with() {
        let mut state = State::default();
        let mut snap = snapshot_of(testing::one_of_each());
        snap.activity = Activity::of(vec![Waiting::Calls, Waiting::Job("j8032".into())]);
        state.seed(&snap);
        assert_eq!(state.word(), "running");
        assert!(state.waiting_on_jobs());
        assert_eq!(
            state.waiting_on(),
            vec!["calls".to_string(), "job j8032".to_string()]
        );
    }

    /// A round's finish is not the turn's: the state says *over* only on the frame the daemon marked
    /// as the prompt's end, so a bridge that reported on every round reported over running work.
    #[test]
    fn a_rounds_finish_does_not_end_the_turn_until_the_daemon_marks_it() {
        let mut state = State::default();
        let mut seq = 0;
        let mut feed = |state: &mut State, event: SessionEvent| {
            seq += 1;
            state.fold(ServerFrame::Event(env(seq, event)));
        };
        feed(&mut state, testing::turn_started("t1"));
        feed(&mut state, testing::turn_finished_round("t1"));
        assert_eq!(
            state.rounds, 1,
            "the round is counted, and its figures with it"
        );
        assert_eq!(
            state.turn,
            Turn::Running,
            "a round that proposed calls is not the turn's end"
        );
        feed(&mut state, testing::turn_finished("t1"));
        assert_eq!(state.turn, Turn::Idle, "the marked end is");
    }

    /// A row that rode its own announcement is read from it: the daemon sends the body no second time
    /// to a head that declared `row_bodies`, so the bridge's last message must come from the append.
    #[test]
    fn a_row_on_its_announcement_is_the_last_message() {
        let mut state = State::default();
        state.fold(ServerFrame::Event(env(
            1,
            SessionEvent::TranscriptAppended {
                item_id: "tr.1".into(),
                kind: "assistant".into(),
                ledger_head: String::new(),
                item: Some(Box::new(TranscriptItem::Assistant {
                    text: "done, all green".into(),
                    tool_calls: Vec::new(),
                    truncated: false,
                })),
                body_bytes: 15,
            },
        )));
        assert_eq!(state.last_message(), "done, all green");
    }

    /// The report outlives the chatter after it, and the spend is the package's. A run that
    /// reported, was woken by its own jobs, said *nothing new in them* and stopped: the
    /// report must still be there, and the cost must be every round's, not the last one's.
    #[test]
    fn the_report_and_the_spend_survive_the_rounds_after_them() {
        let mut state = State::default();
        let mut seq = 0;
        let mut feed = |state: &mut State, event: SessionEvent| {
            seq += 1;
            state.fold(ServerFrame::Event(env(seq, event)));
        };
        let priced = |micros| match testing::turn_finished("t1") {
            SessionEvent::TurnFinished {
                turn_id,
                finish_reason,
                mut usage,
                timings,
                ends_turn,
                prefix_check,
                dialect_template_sha,
            } => {
                usage.cost_micros_usd = Some(micros);
                SessionEvent::TurnFinished {
                    turn_id,
                    finish_reason,
                    usage,
                    timings,
                    ends_turn,
                    prefix_check,
                    dialect_template_sha,
                }
            }
            _ => unreachable!(),
        };
        let report = "## What I built\n\nthe pane\n\n## Test results\n\nall green";
        feed(&mut state, testing::turn_started("t1"));
        feed(&mut state, assistant(report));
        feed(&mut state, priced(1_500));
        feed(&mut state, testing::turn_started("t1"));
        feed(&mut state, assistant("Nothing new in those jobs."));
        feed(&mut state, priced(250));

        assert_eq!(state.last_message(), "Nothing new in those jobs.");
        assert_eq!(state.last_report.as_deref(), Some(report));
        assert_eq!(state.rounds, 2);
        assert_eq!(state.spent.cost_micros_usd, Some(1_750));
        assert_eq!(state.spent.prompt_tokens, 2_000);
        assert_eq!(
            state.meter.cost_micros_usd,
            Some(250),
            "the meter is one round"
        );
    }

    /// The two facts reach `leticode_status`: which check judged the last ending and
    /// which dialect rendered it. Without the check an orchestrator reads zero reuse as a prompt
    /// that diverged; a daemon older than the fields answers `null` and an empty string.
    #[test]
    fn an_endings_prefix_check_and_dialect_reach_the_status() {
        let mut state = State::default();
        let mut seq = 0;
        let mut feed = |state: &mut State, event: SessionEvent| {
            seq += 1;
            state.fold(ServerFrame::Event(env(seq, event)));
        };
        feed(&mut state, testing::turn_started("t1"));
        feed(
            &mut state,
            SessionEvent::TurnFinished {
                turn_id: "t1".into(),
                finish_reason: letibot_sessionlog::event::FinishReason::Eos,
                usage: Default::default(),
                timings: Default::default(),
                ends_turn: true,
                prefix_check: Some(letibot_sessionlog::event::PrefixCheck::Held {
                    expected_cached_min: 12_340,
                    cached: 12_300,
                    shortfall: 40,
                }),
                dialect_template_sha: "3f2a9c".into(),
            },
        );
        let measured: std::collections::BTreeMap<&str, Value> =
            state.measured_keys().into_iter().collect();
        assert_eq!(
            measured.get("dialect_template_sha"),
            Some(&json!("3f2a9c")),
            "{measured:?}"
        );
        assert_eq!(
            measured.get("prefix_check"),
            Some(&json!({"held": {
                "expected_cached_min": 12_340,
                "cached": 12_300,
                "shortfall": 40,
            }})),
            "the verdict is the daemon's, in its own shape: {measured:?}"
        );

        // A daemon older than the two fields: the keys are present and say nothing was measured,
        // which is a different statement from a check that ran.
        let old: std::collections::BTreeMap<&str, Value> =
            State::default().measured_keys().into_iter().collect();
        assert_eq!(old.get("prefix_check"), Some(&Value::Null), "{old:?}");
        assert_eq!(old.get("dialect_template_sha"), Some(&json!("")), "{old:?}");
    }

    /// A recorded turn folds into the compact entries `leticode_events` answers with. The
    /// sequence is a turn as this daemon actually publishes one, with the interactive noise
    /// (deltas, prompt progress, token counts) interleaved where the daemon puts it.
    #[test]
    fn a_recorded_turn_folds_into_compact_entries() {
        let mut state = State::default();
        let mut seq = 0;
        let mut feed = |state: &mut State, event: SessionEvent| {
            seq += 1;
            state.fold(ServerFrame::Event(env(seq, event)));
        };
        feed(&mut state, testing::turn_started("t1"));
        feed(&mut state, testing::progress("t1"));
        feed(&mut state, testing::tokens_generated("t1", 42));
        feed(&mut state, testing::delta("t1", "thinking about it"));
        feed(
            &mut state,
            testing::proposed_on("t1", "c1", "bash", "cargo test --offline -p letibot-bridge"),
        );
        feed(&mut state, finished("c1", ToolOutcome::Ok));
        feed(
            &mut state,
            testing::requested("r1", "bash wants exec access"),
        );
        feed(&mut state, testing::answered("r1", "deny"));
        feed(
            &mut state,
            testing::proposed_on("t1", "c2", "bash", "cargo test --workspace"),
        );
        feed(
            &mut state,
            finished(
                "c2",
                ToolOutcome::Backgrounded {
                    handle: "j7".into(),
                    ran_for_ms: 1_200,
                    how: Backgrounding::Asked,
                    next: "job_output j7".into(),
                },
            ),
        );
        feed(&mut state, settled("j7", "exited 0", 30_400));
        feed(
            &mut state,
            testing::warn("the prompt moved, so this session is re-seated"),
        );
        feed(
            &mut state,
            assistant("## What I built\n\nthe ring\n\n## Test results\n\nall green"),
        );
        feed(&mut state, testing::turn_finished("t1"));

        let page = state.events.page(0, 50);
        let said: Vec<(&str, &str, &str, &str)> = page
            .entries
            .iter()
            .map(|e| (e.kind, e.state, e.name.as_str(), e.text.as_str()))
            .collect();
        assert_eq!(
            said,
            vec![
                ("turn", "started", "", ""),
                (
                    "tool",
                    "proposed",
                    "bash",
                    "cargo test --offline -p letibot-bridge"
                ),
                ("result", "ok", "bash", ""),
                ("ask", "opened", "r1", "bash wants exec access"),
                ("ask", "answered", "r1", "deny"),
                ("tool", "proposed", "bash", "cargo test --workspace"),
                ("job", "started", "j7", ""),
                ("job", "ended", "j7", "exited 0"),
                (
                    "warning",
                    "",
                    "",
                    "the prompt moved, so this session is re-seated"
                ),
                ("message", "", "", "## What I built"),
                ("turn", "ended", "", ""),
            ],
            "the fold's whole vocabulary: {said:?}"
        );
        // Deltas, prompt progress and token counts are `status`'s business and not the
        // ring's: an entry per delta would be the transcript.
        assert!(
            page.entries
                .iter()
                .all(|e| !e.text.contains("thinking about it")),
            "a delta is not an entry: {said:?}"
        );
        // The cursors are one per entry, in order, starting at 1.
        assert_eq!(
            page.entries.iter().map(|e| e.cursor).collect::<Vec<_>>(),
            (1..=11).collect::<Vec<u64>>()
        );
        assert_eq!(page.next, 11);
        assert_eq!(page.dropped, 0);
        // And the facts the entry carries beyond its words: a job's duration, a card's
        // kind, and the message clamped to its first line.
        let job = page
            .entries
            .iter()
            .find(|e| e.kind == "job")
            .expect("a job");
        assert_eq!(job.ms, Some(1_200));
        let ask = page
            .entries
            .iter()
            .find(|e| e.kind == "ask")
            .expect("an ask");
        assert_eq!(ask.code, "permission");
        let answered = page
            .entries
            .iter()
            .find(|e| e.state == "answered")
            .expect("an answer");
        assert_eq!(answered.code, "human", "who settled it, from the event");
        let message = page
            .entries
            .iter()
            .find(|e| e.kind == "message")
            .expect("a message");
        assert_eq!(
            message.text, "## What I built",
            "the first line, and only it"
        );

        // The same turn through the tool's own JSON: the fields that are set, and no others.
        let json = message.to_json();
        assert_eq!(json["kind"], "message");
        assert_eq!(json["cursor"], 10);
        assert!(json.get("state").is_none(), "{json}");
        assert!(json.get("name").is_none(), "{json}");
        let ended = page.entries.last().expect("the last entry").to_json();
        assert_eq!(ended["state"], "ended");
        assert_eq!(page.entries[7].to_json()["ms"], 30_400);
    }

    /// A tool's ending says what happened to it, and a failure carries the reason.
    #[test]
    fn a_failed_call_and_a_failed_turn_are_named() {
        let mut state = State::default();
        state.event(env(
            1,
            testing::proposed_on("t1", "c1", "bash", "git push origin main"),
        ));
        state.event(env(
            2,
            finished(
                "c1",
                ToolOutcome::Failed {
                    reason: "exit 128: no configured push destination".into(),
                },
            ),
        ));
        state.event(env(
            3,
            SessionEvent::TurnFailed {
                turn_id: "t1".into(),
                error: "the provider refused round 3".into(),
                partial_kept: false,
            },
        ));
        let page = state.events.page(0, 50);
        assert_eq!(page.entries[1].state, "failed");
        assert_eq!(
            page.entries[1].name, "bash",
            "the call's name, joined by id"
        );
        assert_eq!(
            page.entries[1].text,
            "exit 128: no configured push destination"
        );
        assert_eq!(page.entries[2].kind, "failure");
        assert_eq!(page.entries[2].code, "turn_failed");
        assert_eq!(page.entries[2].text, "the provider refused round 3");
        assert!(state.turned, "a failed turn is a turn that ran");
    }

    /// A reviewer's start and end are two lines, and they name the branch. A reviewer
    /// is a child session, and the only thing a head attached to the parent is told about it is
    /// `SessionEvent::Subagent` - so this is what `leticode_events` on `s-gatekeeper` can say.
    #[test]
    fn a_reviewers_start_and_end_are_one_line_each() {
        let task = "You are the gatekeeper. A subagent has finished work on a branch.\n\n\
                    The ask, verbatim:\n\nkeep the last record\n\nThe placement:\n\
                    - branch: agent/P1\n- head:   abc123\n- base:   def456\n";
        let mut state = State::default();
        let mut seq = 0;
        let mut child = |state: &mut State, child_state: &str, said: &str, answer: Option<&str>| {
            seq += 1;
            state.fold(ServerFrame::Event(env(
                seq,
                SessionEvent::Subagent {
                    subagent_id: "s-sub-1".into(),
                    state: child_state.into(),
                    prompt: said.into(),
                    role: "gatekeeper".into(),
                    task: task.into(),
                    model: String::new(),
                    answer: answer.map(str::to_string),
                    slug: "keep-the-last-record".into(),
                },
            )));
        };
        child(&mut state, "opening", "You are the gatekeeper.", None);
        // The second state of the same child is not a second start: `opening` then `running`
        // is one reviewer beginning, and two entries would say it began twice.
        child(&mut state, "running", "You are the gatekeeper.", None);
        child(
            &mut state,
            "done",
            "accepted: the reader keeps the last record",
            Some("accepted: the reader keeps the last record"),
        );
        let page = state.events.page(0, 50);
        let said: Vec<(&str, &str, &str, &str)> = page
            .entries
            .iter()
            .map(|e| (e.kind, e.state, e.name.as_str(), e.text.as_str()))
            .collect();
        assert_eq!(
            said,
            vec![
                ("subagent", "started", "keep-the-last-record", "agent/P1"),
                (
                    "subagent",
                    "ended",
                    "keep-the-last-record",
                    "agent/P1 — accepted: the reader keeps the last record",
                ),
            ],
            "one line per reviewer per phase: {said:?}"
        );

        // A child that FAILED carries no answer, so the line falls back to what it was asked
        // - an ending that named no branch and said nothing would be a row nobody can act on.
        seq += 1;
        state.fold(ServerFrame::Event(env(
            seq,
            SessionEvent::Subagent {
                subagent_id: "s-sub-2".into(),
                state: "failed".into(),
                prompt: "You are the gatekeeper.".into(),
                role: "gatekeeper".into(),
                task: task.into(),
                model: String::new(),
                answer: None,
                slug: String::new(),
            },
        )));
        let last = state.events.page(0, 50).entries.pop().expect("an ending");
        assert_eq!(last.kind, "subagent");
        assert_eq!(last.state, "failed");
        assert_eq!(
            last.name, "s-sub-2",
            "a child with no slug is named by its id"
        );
        assert_eq!(last.text, "agent/P1 — You are the gatekeeper.");
    }

    /// A reviewer that is already running when the head attaches is announced from the
    /// snapshot, and a resync completes the ones that ended while it was demoted. Attaching
    /// mid-review is the case: the snapshot's `subagents` is the only place they exist.
    #[test]
    fn a_child_already_running_is_read_from_the_snapshot() {
        let hub = letibot_sessionlog::hub::Hub::new("s-gatekeeper");
        let published = |id: &str, state: &str, answer: Option<&str>| SessionEvent::Subagent {
            subagent_id: id.into(),
            state: state.into(),
            prompt: "You are the gatekeeper.".into(),
            role: "gatekeeper".into(),
            task: "The placement:\n- branch: agent/B6\n".into(),
            model: String::new(),
            answer: answer.map(str::to_string),
            slug: id.into(),
        };
        // One reviewer in flight and one that finished before this head existed.
        hub.publish(published("sub-running", "running", None));
        hub.publish(published("sub-old", "done", Some("accepted")));
        let snap = hub
            .attach("remote", "probe", Caps::default(), 0)
            .snapshot
            .expect("a snapshot");

        let mut state = State::default();
        state.seed(&snap);
        let said: Vec<(String, String)> = state
            .events
            .page(0, 50)
            .entries
            .iter()
            .map(|e| (e.name.clone(), e.text.clone()))
            .collect();
        assert_eq!(
            said,
            vec![("sub-running".to_string(), "agent/B6".to_string())],
            "the child still running is news; the one that ended before the attach is history"
        );

        // The reviewer finishes while this head is demoted (a `Resync` carries a fresh
        // snapshot): the ending must not be lost just because the event was never delivered.
        hub.publish(published("sub-running", "done", Some("rejected: no test")));
        let again = hub
            .attach("remote", "probe", Caps::default(), 0)
            .snapshot
            .expect("a snapshot");
        state.seed(&again);
        let entries = state.events.page(0, 50).entries;
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert_eq!(entries[1].state, "ended");
        assert_eq!(entries[1].text, "agent/B6 — rejected: no test");
        // And nothing is announced twice: the child is off the running list now.
        state.seed(&again);
        assert_eq!(state.events.page(0, 50).entries.len(), 2);
    }

    /// A line is clamped, counted in characters, and the ellipsis is inside the budget:
    /// a cap of 120 that yields 121 characters is not a cap.
    #[test]
    fn a_long_line_is_cut_within_its_budget() {
        let long = "cargo test --workspace --all-targets ".repeat(20);
        let line = one_line(&long, LINE);
        assert_eq!(line.chars().count(), LINE);
        assert!(line.ends_with('…'), "{line}");
        let text = "first line\nsecond line";
        assert_eq!(one_line(text, LINE), "first line");
        assert_eq!(one_line("\n\n  padded  \nmore", LINE), "padded");
        // Fewer characters than the cap stay untouched, and a wide character counts once.
        assert_eq!(one_line("日本語", 5), "日本語");
    }

    /// A cursor older than the ring says what it dropped, and the cursors go on past it:
    /// a ring that reused a cursor would let a caller ask for what it already has and never
    /// learn that it missed anything.
    #[test]
    fn a_cursor_older_than_the_ring_reports_what_it_dropped() {
        let mut ring = Ring::default();
        for i in 0..(RING + 10) {
            ring.push(Entry::of("tool", "proposed").named(format!("call-{i}")));
        }
        let page = ring.page(0, RING);
        assert_eq!(page.entries.len(), RING);
        assert_eq!(page.entries[0].cursor, 11, "the first ten fell off");
        assert_eq!(page.entries[0].name, "call-10");
        assert_eq!(page.next, RING as u64 + 10);
        assert_eq!(page.dropped, 10, "ten entries are gone from this view");
        // A caller that is inside the ring has lost nothing.
        assert_eq!(ring.page(20, RING).dropped, 0);
        assert_eq!(ring.page(RING as u64 + 10, RING).dropped, 0);
        // And nothing to say: the cursor comes back unchanged, so a poll loop keeps its
        // place rather than resetting to zero.
        let none = ring.page(RING as u64 + 10, RING);
        assert!(none.entries.is_empty(), "{none:?}");
        assert_eq!(none.next, RING as u64 + 10);
    }

    /// A limit pages, and `next` is where the caller got to - not the newest cursor, or
    /// a caller that took the first fifty of two hundred would be told there was nothing
    /// else.
    #[test]
    fn a_limit_pages_from_the_cursor_it_was_given() {
        let mut ring = Ring::default();
        for i in 0..10 {
            ring.push(Entry::of("turn", "started").named(format!("t{i}")));
        }
        let first = ring.page(0, 3);
        assert_eq!(first.entries.len(), 3);
        assert_eq!(first.next, 3);
        let second = ring.page(first.next, 3);
        assert_eq!(
            second.entries.iter().map(|e| e.cursor).collect::<Vec<_>>(),
            vec![4, 5, 6]
        );
        assert_eq!(second.next, 6);
        // The default page is fifty entries; a limit of zero is one entry rather than none,
        // because an answer that cannot advance a cursor is a caller that loops for ever.
        assert_eq!(ring.page(0, 0).entries.len(), 1);
    }
}
