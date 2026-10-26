//! **A session held rather than run** — the bridge's half of the head protocol.
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
//! **One `Head` per session, one socket per `Head`**, and that is what makes an ask
//! reach the bridge at all: the daemon posts a permission card to the heads attached to
//! that session, so a session the bridge is not attached to has nobody to ask — which is
//! the correct answer (`docs/claude-code-bridge.md`: *"one that is left with no head
//! attached refuses gated calls as any session does"*).
//!
//! **Why a condvar and not a poll.** `wait` is a blocking verb whose whole value is that
//! it returns the moment something happens; a loop that slept on the state would turn
//! every wake-up into a latency the orchestrator pays for nothing. The fold notifies and
//! the waiter recomputes, which is the same shape the daemon's own hub uses.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{ClientError, HeadClient, Inbound, pump};
use letibot_sessionlog::event::{DeltaTarget, Envelope, SessionEvent, TodoEntry};
use letibot_sessionlog::protocol::{Ack, Caps, ServerFrame};
use letibot_sessionlog::view::{
    CallState, OpenDecision, Snapshot, SnapshotItem, TurnState as ViewTurnState,
};
use letibot_transcript::{ToolOutcome, TranscriptItem};
use serde_json::{Value, json};

use crate::daemon::{Daemon, HEAD_KIND, IDENTITY};

/// **How long `wait` gives a turn to appear before it answers with what it sees.**
///
/// A prompt is queued the moment it is sent and starts when the daemon's worker reaches
/// it, so a `wait` that attached in the same breath as a `send` can find the session
/// still idle. Answering `0 — the turn ended` there would be a lie about a prompt nobody
/// had run yet, and waiting for ever would turn a healthy session into a timeout; the
/// window is what makes the difference visible without inventing one.
///
/// It is not a poll interval: nothing happens in this time except a turn starting, and a
/// session that has one starts it in milliseconds.
pub const START_GRACE: Duration = Duration::from_secs(3);

/// **How long a turn has to stay quiet before `wait` calls it ended.**
///
/// The engine publishes `TurnStarted`/`TurnFinished` once per ROUND, not per prompt
/// (`Hub::turn_in_flight` says so at length), and the calls a round proposed are what keep
/// the turn going after its `TurnFinished`. [`State`] holds the turn as running until they
/// finish; what it cannot see is the gap between the last call finishing and the next
/// round's `TurnStarted`, where the daemon appends results and opens the next request. The
/// first `wait` the bridge ran on a real package exited `0` ten seconds in, between round
/// one and round two of a brief that had an hour of work left; quiet for this long is what
/// separates *the turn ended* from *the turn is between rounds*.
pub const SETTLE: Duration = Duration::from_secs(10);

/// How long a head gives a `Switch` to land before it reports that it is on the wrong
/// session. A re-seat is a second `Hello` on the same socket — one round trip — so the
/// wait is for a daemon that is not answering, not for one that is thinking.
const SEAT_PATIENCE: Duration = Duration::from_secs(5);

/// **How many progress entries a head keeps**, and the number a `since` older than the
/// ring is measured against. Five hundred is a long turn's worth of calls and jobs: past
/// that an orchestrator is reading history rather than following progress, and the
/// transcript is where history lives.
pub const RING: usize = 500;

/// The longest one-liner an entry carries — the number `crates/ui/DESIGN.md` §4.1 arrived
/// at for a tool's display target, kept here for the same reason: an entry is a line a
/// reader scans, not a payload.
const LINE: usize = 120;

/// The assistant message's entry gets more room than a tool's line, because the first
/// line of a report is usually the sentence that says what the turn was for.
const MESSAGE_LINE: usize = 160;

/// One option a permission ask offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskOption {
    pub option_id: String,
    pub label: String,
    pub kind: String,
}

/// **A permission ask that is still owed an answer**, as the head sees it.
///
/// `tool` is joined from the `ToolCallProposed` event by `call_id` rather than parsed out
/// of [`Self::summary`]: the card's sentence is the daemon's warning prose and it has
/// changed wording before, while the call's name is a fact the log carries in its own
/// field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub req_id: String,
    /// `permission` or `question` — the daemon's own two words, and the reason two
    /// different answer frames exist (`Answer` and `AnswerQuestion`).
    pub kind: String,
    pub call_id: Option<String>,
    pub tool: String,
    pub summary: String,
    pub target: String,
    pub access: String,
    pub options: Vec<AskOption>,
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

/// **One line of what a turn is doing**, folded from the events so an orchestrator can
/// follow a session without reading its transcript.
///
/// `docs/claude-code-bridge.md` is the contract: *tool calls (name + one line), job starts
/// and ends, failures, asks — never banner or memwatch lines*. The kinds are this file's
/// own closed vocabulary, because a head that let the event names through would be
/// publishing the protocol's shape as an answer and would change it every version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// **The position in the session's progress**, monotonic for the life of the head and
    /// never reused. A caller passes the last one it saw as `since`.
    pub cursor: u64,
    /// `tool`, `result`, `job`, `turn`, `ask`, `warning`, `failure`, `message`.
    pub kind: &'static str,
    /// The phase or outcome, in the kind's own words: `proposed`, `ok`, `started`,
    /// `ended`, `opened`, `answered`. Empty when the kind has no phase (`message`).
    pub state: &'static str,
    /// What the entry is *about* — a tool's name, a job's handle, an ask's id. Empty when
    /// it is about the turn itself.
    pub name: String,
    /// The event's own code, for a `warning` or a `failure`. Empty otherwise.
    pub code: String,
    /// One line, already clamped: the command, the reason, the first line said.
    pub text: String,
    /// How long a job ran, when the ending carried a duration.
    pub ms: Option<u64>,
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
        out
    }
}

/// What a `since` cursor and a limit ask of the ring.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub entries: Vec<Entry>,
    /// The cursor to ask from next: the last entry handed over, or the caller's own cursor
    /// when there was nothing new. Never the newest entry when the limit cut the answer
    /// short — a caller that took 50 of 200 has 150 to fetch, not 0.
    pub next: u64,
    /// Entries that fell off the ring before the caller's cursor could reach them. A
    /// caller that sees a non-zero number here knows its view has a hole in it; zero says
    /// the hole is not this ring's.
    pub dropped: u64,
}

/// **The bounded ring of progress entries**, and the cursors that make it a stream.
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

/// **The first line of a message, cut to `max` characters.**
///
/// Characters rather than bytes, and the ellipsis counts inside the budget: an entry that
/// overran its own cap by one character to say it was cut would be the cap lying.
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

/// Which state a turn is in, from this head's point of view — the three words
/// `leticode_status` and `wait` are written against.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Turn {
    /// No turn since this head attached, or the last one ended.
    #[default]
    Idle,
    Running,
    /// A turn ended in a failure. **Terminal**, like [`ViewTurnState::Failed`]: a head that
    /// kept drawing "running" after one is the defect that event exists for.
    Failed(String),
}

/// The tokens and the money of the last turn that reported them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Meter {
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub predicted_tokens: u64,
    /// Micro-USD: `None` on a local server (free) and on a metered model nothing prices —
    /// *unpriced* and *free* are different and only one of them is a number.
    pub cost_micros_usd: Option<u64>,
}

/// Everything this head keeps about the session it is attached to.
///
/// Deliberately not the transcript: a head that drew one would be the TUI, and the bridge's
/// question is *what do I tell the orchestrator* — the last answer, what is being asked,
/// what is planned.
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
    /// `call_id` → the tool's name, joined into an ask. Bounded: a session's calls are
    /// many and this only has to answer for cards that are still open.
    calls: std::collections::HashMap<String, String>,
    /// The calls proposed and not yet finished. A round's `TurnFinished` with any of these
    /// open is a turn that goes on: its calls run, and the next round reads their results.
    in_flight: std::collections::HashSet<String>,
    /// When the turn last went idle, so `wait` can tell a turn that ended from a gap
    /// between rounds (see [`SETTLE`]).
    pub idle_since: Option<Instant>,
    /// Whether the round in view has finished generating: calls that finish while it is
    /// still writing are not the turn's last.
    round_done: bool,
    /// **Whether a turn has ever run here**, from the snapshot's own turn or from one this
    /// head watched. `wait` needs it to tell *a session between turns* from *a session that
    /// has never run one* — and only the first of those is a turn that ended.
    pub turned: bool,
    /// The compact progress `leticode_events` answers from.
    pub events: Ring,
    /// `Some(reason)` when the daemon closed the connection; every later send says so
    /// rather than failing on a dead socket two frames later.
    pub closed: Option<String>,
    /// The daemon's answer to the last mutating frame, when it refused one. A refusal is a
    /// real answer (`already stopped`, a stale option) and is worth saying back.
    pub rejection: Option<String>,
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

    fn go_idle(&mut self) {
        self.turn = Turn::Idle;
        self.idle_since = Some(Instant::now());
    }

    /// The word `leticode_status` answers with. `asking` wins over `running` because a
    /// turn blocked on a card is a turn whose next step is a person's.
    pub fn word(&self) -> &'static str {
        if !self.asks.is_empty() {
            "asking"
        } else {
            match self.turn {
                Turn::Running => "running",
                Turn::Idle => "idle",
                Turn::Failed(_) => "failed",
            }
        }
    }

    /// Fold one server frame in. Returns the `seq` to acknowledge, when the frame was an
    /// event with one.
    fn fold(&mut self, frame: ServerFrame) {
        match frame {
            ServerFrame::Hello {
                session_id,
                snapshot,
                ..
            } => {
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
                // The daemon demoted this head — it fell too far behind, and its own doc is
                // explicit that this is a normal answer and never an error. The frame
                // carries a fresh snapshot, so nothing is stale after it: the same seeding
                // an attach does, and the connection stays up.
                self.seed(&snapshot);
            }
            ServerFrame::Todos { todos, .. } => self.todos = todos,
            ServerFrame::Rejected { reason, .. } => self.rejection = Some(reason),
            ServerFrame::Bye { reason } => self.closed = Some(reason),
            _ => {}
        }
    }

    /// A snapshot is what a head that attaches late is handed: rows, the turn in flight,
    /// and the cards still open. Everything it carries replaces what this head held, which
    /// is what makes a `Switch` or a resync land correctly.
    fn seed(&mut self, snap: &Snapshot) {
        self.seq = self.seq.max(snap.seq);
        self.turn = match &snap.turn {
            None => Turn::Idle,
            Some(t) => match &t.state {
                ViewTurnState::Running => Turn::Running,
                ViewTurnState::Interrupted { .. } => Turn::Idle,
                // The round ended and its calls are running: the turn is not over.
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
        self.in_flight = snap
            .turn
            .iter()
            .flat_map(|t| &t.calls)
            .filter(|c| !matches!(c.state, CallState::Finished { .. }))
            .map(|c| c.call_id.clone())
            .collect();
        self.idle_since = (self.turn != Turn::Running).then(Instant::now);
        self.round_done = snap
            .turn
            .as_ref()
            .is_none_or(|t| !matches!(t.state, ViewTurnState::Running));
        self.turn_id = snap
            .turn
            .as_ref()
            .map(|t| t.turn_id.clone())
            .unwrap_or_default();
        // The live turn's text is the newest assistant words there are; a session with no
        // turn in flight falls back to its last committed answer row.
        self.streaming = snap
            .turn
            .as_ref()
            .map(|t| t.text.clone())
            .unwrap_or_default();
        self.last_row = last_assistant(&snap.items).unwrap_or_default();
        // **A snapshot with a turn in it, or with words a turn produced, is a session that
        // has run one.** This is what `wait` must not mistake for *no turn yet*.
        if snap.turn.is_some() || !self.last_row.is_empty() {
            self.turned = true;
        }
        self.asks = snap.open_decisions.iter().map(Ask::from_open).collect();
        self.asks.retain(|a| !a.req_id.is_empty());
    }

    fn event(&mut self, env: Envelope) {
        match env.event {
            SessionEvent::TurnStarted { turn_id, .. } => {
                self.turn = Turn::Running;
                self.idle_since = None;
                self.round_done = false;
                self.turned = true;
                self.turn_id = turn_id;
                self.streaming.clear();
                self.events.push(Entry::of("turn", "started"));
            }
            SessionEvent::Delta { target, text, .. } => {
                if target == DeltaTarget::Text {
                    self.streaming.push_str(&text);
                }
            }
            SessionEvent::TurnFinished { usage, .. } => {
                // A round that proposed calls hands the turn to them; only a round that left
                // nothing running is the turn going idle.
                self.round_done = true;
                if self.in_flight.is_empty() {
                    self.go_idle();
                }
                self.turned = true;
                if !self.streaming.is_empty() {
                    self.last_row = std::mem::take(&mut self.streaming);
                }
                self.meter = Meter {
                    prompt_tokens: usage.prompt_tokens,
                    cached_tokens: usage.cached_tokens,
                    predicted_tokens: usage.predicted_tokens,
                    cost_micros_usd: usage.cost_micros_usd,
                };
                self.events.push(Entry::of("turn", "ended"));
            }
            SessionEvent::TurnInterrupted { reason, .. } => {
                // An interrupted turn is over, whatever its calls were left as.
                self.in_flight.clear();
                self.go_idle();
                self.turned = true;
                if !self.streaming.is_empty() {
                    self.last_row = std::mem::take(&mut self.streaming);
                }
                self.events
                    .push(Entry::of("turn", "interrupted").saying(one_line(&reason, LINE)));
            }
            SessionEvent::TurnFailed { error, .. } => {
                self.in_flight.clear();
                self.idle_since = Some(Instant::now());
                self.turn = Turn::Failed(error.clone());
                self.turned = true;
                self.streaming.clear();
                self.events.push(
                    Entry::of("failure", "failed")
                        .coded("turn_failed")
                        .saying(one_line(&error, LINE)),
                );
            }
            SessionEvent::TranscriptContent { item, .. } => {
                if let TranscriptItem::Assistant { text, .. } = item.as_ref()
                    && !text.trim().is_empty()
                {
                    self.last_row = text.clone();
                    // **The committed row, and not the stream.** A turn's deltas build the
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
                // **One entry per call, from the proposal.** `ToolStarted` names the same
                // call again with no argument, so folding both would draw every call twice
                // and the second line would be the less useful of the two.
                self.events.push(
                    Entry::of("tool", "proposed")
                        .named(name.clone())
                        .saying(one_line(&target, LINE)),
                );
                self.in_flight.insert(call_id.clone());
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
                // A backgrounded call has finished as far as the round is concerned: the
                // model gets its handle and goes on.
                if self.in_flight.remove(&call_id)
                    && self.in_flight.is_empty()
                    && self.turn == Turn::Running
                    && self.round_done
                {
                    self.go_idle();
                }
                // **A backgrounded call is the job STARTING**: there is no `JobStarted` on
                // the wire, and the handle only exists here and on `JobSettled`.
                if let ToolOutcome::Backgrounded {
                    handle, ran_for_ms, ..
                } = &outcome
                {
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
                self.events.push(
                    Entry::of("job", "ended")
                        .named(job)
                        .saying(one_line(&state, LINE))
                        .lasting(elapsed_ms),
                );
            }
            SessionEvent::Warning { code, detail, .. } => {
                self.events.push(
                    Entry::of("warning", "")
                        .coded(code)
                        .saying(one_line(&detail, LINE)),
                );
            }
            SessionEvent::DecisionRequested {
                req_id,
                kind,
                call_id,
                access,
                summary,
                target,
                options,
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
            _ => {}
        }
    }
}

/// The last assistant row's text in a snapshot — newest first, and an empty row (a call
/// with no prose) is skipped rather than counted as the last thing said.
fn last_assistant(items: &[SnapshotItem]) -> Option<String> {
    items.iter().rev().find_map(|i| match i.item.as_ref() {
        Some(TranscriptItem::Assistant { text, .. }) if !text.trim().is_empty() => {
            Some(text.clone())
        }
        _ => None,
    })
}

/// What a caller waiting on a session learns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attention {
    /// The turn is over. The words are the last assistant message, as it stands.
    TurnEnded { message: String },
    /// The session is stopped on a card and every further step waits for an answer.
    ///
    /// Boxed because a card is a whole `DecisionRequested` — the sentence, the ladder, the
    /// model's own choices — and an unboxed one would make every wake-up that is not a
    /// card pay for the size of the one that is (the argument `Hello.snapshot` makes one
    /// frame over).
    Ask(Box<Ask>),
    /// The turn ended in a failure.
    Failed { why: String },
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
    shared: Arc<Shared>,
    /// Shared with the collecting thread because an `Ack` is a client frame: a head that
    /// acked from a second borrow of the socket would be two writers on one stream.
    client: Arc<Mutex<HeadClient>>,
}

impl Head {
    /// **Attach as a head and start collecting.**
    ///
    /// `since_seq = 0` asks for the whole snapshot, which is what a head that has just
    /// started needs: the asks already open, the turn already running and the last answer
    /// it missed are all in it.
    pub fn attach(d: &Daemon, session: &str) -> Result<Head, String> {
        let caps = Caps {
            can_decide: true,
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
        }
        let head = Head {
            session: session.to_string(),
            worktree: d.workspace.clone(),
            socket: d.socket.clone(),
            shared,
            client,
        };
        head.seated_on(session)?;
        Ok(head)
    }

    /// **The head is on the session it was asked for, or this says so.**
    ///
    /// The first real `leticode_start` sent a brief into the daemon's own first session
    /// while `leticode_status` watched the new, empty one — the answer named one session and
    /// the work happened in another, which is the worst shape a reply can have because
    /// everything in it is true except what it is about. So the id the daemon seated this
    /// connection on is compared with the id asked for, and a difference is corrected with
    /// `Switch` rather than assumed away: the socket *is* the head's claim about which
    /// session it speaks for.
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

    /// A copy of everything this head holds — the shape `leticode_status` answers with.
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
            calls: g.calls.clone(),
            in_flight: g.in_flight.clone(),
            idle_since: g.idle_since,
            round_done: g.round_done,
            turned: g.turned,
            events: g.events.clone(),
            closed: g.closed.clone(),
            rejection: g.rejection.clone(),
        }
    }

    /// **The progress since a cursor**: the entries, the cursor to ask from next, and how
    /// many fell off the ring before the caller could reach them.
    ///
    /// Taken under the state's own lock rather than through [`Self::snapshot`], because a
    /// ring of five hundred entries cloned on every status poll would be work nobody asked
    /// for; the page is what the caller wanted.
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
        let mut c = self.said_client()?;
        // `expected_seq = 0` is what the protocol reads as *I am not claiming a position*,
        // and `Hub::submit` accepts a prompt on a stale seq anyway. A bridge is a second
        // head on a session a person may be typing into, so it must never be the head that
        // loses the race and drops what it was told.
        c.prompt(0, text).map_err(|e| e.to_string())
    }

    /// **Answer a card**, by an option the card itself offered.
    ///
    /// The check is the bridge's as well as the daemon's, because the two failures are
    /// different: an option the ask never had is a caller's mistake and is refused here
    /// with the list; an option the ask had and the daemon no longer wants is the daemon's
    /// word, and arrives as a `Rejected` frame.
    pub fn answer(&self, ask: &str, option: &str, note: Option<&str>) -> Result<(), String> {
        let (kind, offered): (String, Vec<String>) = {
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
            )
        };
        // **A question is not a permission, and the two have different frames.** Sending an
        // `Answer` for a question would be refused by the daemon for a reason that names
        // neither — so it is refused here, saying what could answer it.
        if kind != "permission" {
            return Err(format!(
                "ask `{ask}` is a {kind}, not a permission: a question is answered by a person \
                 in a TUI (it carries the model's own choices, and `AnswerQuestion` is the frame \
                 for it, which this build does not send yet)"
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

    /// Interrupt whatever is running and leave; the session stays on disk for any head.
    pub fn stop(&self) -> Result<(), String> {
        let mut c = self.said_client()?;
        c.interrupt(0, "the bridge was asked to stop this session")
            .map_err(|e| e.to_string())?;
        c.detach().map_err(|e| e.to_string())
    }

    /// **Ask the daemon to move this session to an approval point.**
    ///
    /// Queued like a prompt, and it takes effect from the next call — so a `start` sends
    /// it before the brief and the brief's first tool call already runs under it.
    ///
    /// `consented: false` always. The one point that needs a person's confirmation is
    /// `allow-all` on a box with no structural boundary, and a bridge is not a person: it
    /// can ask for the point and the daemon answers with the question, which is the honest
    /// answer to a machine.
    pub fn set_mode(&self, mode: &str) -> Result<(), String> {
        let mut c = self.said_client()?;
        c.set_mode(0, mode, false)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// The client, or the reason a frame cannot be sent: a connection the daemon closed
    /// answers every send with the closing sentence rather than with a dead-socket error
    /// that names nothing.
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

    /// **Block until the session needs somebody**, or the deadline passes.
    ///
    /// The precedence is the whole of `wait`'s meaning:
    ///
    /// * an **open ask** wins over everything — the turn is stopped on it, so waiting past
    ///   it would be waiting for a person who was never told;
    /// * a **failure** is terminal and is reported as one;
    /// * otherwise the turn this head watched run ends;
    /// * and a turn that never appears inside [`START_GRACE`] is answered with what the
    ///   session looks like — but **only on a session that has run a turn before**.
    ///   `idle is a real answer` is true of a session between turns and false of one that
    ///   has never had one: the first real `leticode_start` was followed by a `wait` that
    ///   exited `0 — the turn ended` in three seconds on a session no prompt had reached,
    ///   and the orchestrator read that as the brief having finished. A session that has
    ///   never turned has nothing to report as ended, so the only honest answers there are
    ///   a turn starting and ending, an ask, a failure, or the timeout saying so.
    ///
    /// **A failure that was already there when this head attached does not count**: a
    /// `wait` fired after a `send` must not report the previous turn's death as this
    /// one's.
    pub fn wait(&self, deadline: Instant) -> Attention {
        let mut g = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let baseline = match &g.turn {
            Turn::Failed(why) => Some(why.clone()),
            _ => None,
        };
        let mut armed = g.turn == Turn::Running;
        let grace = Instant::now() + START_GRACE;
        loop {
            if let Some(a) = g.asks.first() {
                return Attention::Ask(Box::new(a.clone()));
            }
            match &g.turn {
                Turn::Failed(why) if baseline.as_deref() != Some(why.as_str()) => {
                    return Attention::Failed { why: why.clone() };
                }
                Turn::Running => armed = true,
                // Ended means quiet for [`SETTLE`]: a round's end is not the turn's, and the
                // next round's `TurnStarted` re-arms this before the window runs out.
                Turn::Idle | Turn::Failed(_)
                    if armed && g.idle_since.is_none_or(|t| t.elapsed() >= SETTLE) =>
                {
                    return Attention::TurnEnded {
                        message: g.last_message().to_string(),
                    };
                }
                _ => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Attention::Timeout;
            }
            if !armed && g.turned && now >= grace {
                // Nothing turned, and something has turned before: answer with what is here
                // rather than waiting out a timeout that would report a health problem this
                // session does not have.
                return match &g.turn {
                    Turn::Failed(why) => Attention::Failed { why: why.clone() },
                    _ => Attention::TurnEnded {
                        message: g.last_message().to_string(),
                    },
                };
            }
            // A session that has never turned waits the whole deadline: there is no grace
            // question to ask about a session with no turn to have ended.
            let until = match g.idle_since {
                Some(t) if armed && g.turn != Turn::Running => deadline.min(t + SETTLE),
                _ if armed || !g.turned => deadline,
                _ => deadline.min(grace),
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

/// **A head that nobody holds lets go of the daemon.**
///
/// The collecting threads keep the socket alive — they own the reader and the writer — so
/// dropping the last handle without shutting the connection down would leave the daemon
/// with a head it believes is watching and a socket that never reaches EOF. That is not
/// only untidy: `Daemon::shutdown` joins its connection threads, so a bridge that went
/// away quietly is a daemon that cannot stop.
impl Drop for Head {
    fn drop(&mut self) {
        if let Ok(mut c) = self.client.lock() {
            let _ = c.shut_down();
        }
    }
}

/// Read frames off the socket forever, folding them and acking what was handled.
///
/// The reader is `pump`, which hands over a line this build cannot read instead of ending
/// the connection: two builds a version apart is a fact about them and not about the
/// stream, and a bridge that exited on the first unknown frame would look like a daemon
/// that stopped answering.
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
                    // A line this build cannot read. The connection is still up and the
                    // next frame is very likely readable, so it is counted nowhere and
                    // nothing else happens.
                    Inbound::Unreadable(_) => None,
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

/// The session a bare attach lands on — the daemon's own choice, which for a daemon that
/// has just started is the one session it opened eagerly.
///
/// **A bridge that started the daemon uses this session rather than minting a second one.**
/// One brief is one conversation, and the reply that names one session while the work
/// happens in another is the defect that put this function here: the first real
/// `leticode_start` answered with a session id it had just created, and the brief ran in the
/// daemon's own first one, so `leticode_status` watched an empty session report `idle` while
/// a turn was happening one session over.
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

/// **Make a session in this daemon and hand back its id.**
///
/// `NewSession` answers with the whole list and the id it minted, and it deliberately does
/// not switch — so this is a control connection that creates and leaves, and the head that
/// will hold the session attaches to it afterwards. Two connections rather than a `Switch`
/// because a `Switch` re-seats this head on the same socket, and the socket the bridge
/// holds for a session must be that session's from the first frame.
pub fn create_session(d: &Daemon, title: &str) -> Result<String, String> {
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
        .new_session(title, &d.workspace.display().to_string())
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
    use letibot_sessionlog::testing;
    use letibot_transcript::Backgrounding;

    /// One recorded event, as the fold receives it: the log's own envelope, never a
    /// hand-built shape — a head that folded something the daemon does not send would pass
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

    /// **A round's end is not the turn's.** The engine publishes `TurnFinished` per round,
    /// and the round's calls run after it: the first `wait` on a real package woke its
    /// caller between rounds one and two of an hour-long brief. The turn stays running
    /// while a call is open, goes idle only when the last finishes after its round did,
    /// and the next round's start takes the idle stamp back.
    #[test]
    fn a_turn_runs_through_its_rounds_calls() {
        let mut state = State::default();
        let mut seq = 0;
        let mut feed = |state: &mut State, event: SessionEvent| {
            seq += 1;
            state.fold(ServerFrame::Event(env(seq, event)));
        };
        feed(&mut state, testing::turn_started("t1"));
        feed(
            &mut state,
            testing::proposed_on("t1", "c1", "read", "docs/plan.md"),
        );
        feed(
            &mut state,
            testing::proposed_on("t1", "c2", "bash", "git status"),
        );
        feed(&mut state, testing::turn_finished("t1"));
        assert_eq!(
            state.word(),
            "running",
            "the round's calls are still to run"
        );
        assert!(state.idle_since.is_none());

        feed(&mut state, finished("c1", ToolOutcome::Ok));
        assert_eq!(state.word(), "running", "one call is still open");
        feed(&mut state, finished("c2", ToolOutcome::Ok));
        assert_eq!(state.word(), "idle", "the gap before the next round");
        assert!(state.idle_since.is_some(), "wait settles from this stamp");

        feed(&mut state, testing::turn_started("t1"));
        assert_eq!(state.word(), "running");
        assert!(
            state.idle_since.is_none(),
            "the next round re-arms the turn"
        );

        // A call that finishes while its round is still writing is not the turn's last.
        feed(&mut state, testing::proposed_on("t1", "c3", "read", "a.rs"));
        feed(&mut state, finished("c3", ToolOutcome::Ok));
        assert_eq!(state.word(), "running", "the round is still generating");
        feed(&mut state, testing::turn_finished("t1"));
        assert_eq!(
            state.word(),
            "idle",
            "a round that left nothing open ends idle"
        );
    }

    /// **A recorded turn folds into the compact entries `leticode_events` answers with.**
    ///
    /// The sequence is a turn as this daemon actually publishes one: a start, a call, a
    /// result, a card and its answer, a backgrounded call and its settlement, a warning, an
    /// answer row, and an end — with the interactive noise (deltas, prompt progress, token
    /// counts) interleaved where the daemon puts it.
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

    /// **A tool's ending says what happened to it**, and a failure carries the reason.
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

    /// **A line is clamped, counted in characters**, and the ellipsis is inside the budget:
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

    /// **A cursor older than the ring says what it dropped**, and the cursors go on past it:
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

    /// **A limit pages, and `next` is where the caller got to** — not the newest cursor, or
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
