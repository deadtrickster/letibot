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
use letibot_sessionlog::view::{OpenDecision, Snapshot, SnapshotItem, TurnState as ViewTurnState};
use letibot_transcript::TranscriptItem;

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
                ViewTurnState::Finished { .. } => Turn::Idle,
                ViewTurnState::Failed { error, .. } => Turn::Failed(error.clone()),
            },
        };
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
        self.asks = snap.open_decisions.iter().map(Ask::from_open).collect();
        self.asks.retain(|a| !a.req_id.is_empty());
    }

    fn event(&mut self, env: Envelope) {
        match env.event {
            SessionEvent::TurnStarted { turn_id, .. } => {
                self.turn = Turn::Running;
                self.turn_id = turn_id;
                self.streaming.clear();
            }
            SessionEvent::Delta { target, text, .. } => {
                if target == DeltaTarget::Text {
                    self.streaming.push_str(&text);
                }
            }
            SessionEvent::TurnFinished { usage, .. } => {
                self.turn = Turn::Idle;
                if !self.streaming.is_empty() {
                    self.last_row = std::mem::take(&mut self.streaming);
                }
                self.meter = Meter {
                    prompt_tokens: usage.prompt_tokens,
                    cached_tokens: usage.cached_tokens,
                    predicted_tokens: usage.predicted_tokens,
                    cost_micros_usd: usage.cost_micros_usd,
                };
            }
            SessionEvent::TurnInterrupted { .. } => {
                self.turn = Turn::Idle;
                if !self.streaming.is_empty() {
                    self.last_row = std::mem::take(&mut self.streaming);
                }
            }
            SessionEvent::TurnFailed { error, .. } => {
                self.turn = Turn::Failed(error);
                self.streaming.clear();
            }
            SessionEvent::TranscriptContent { item, .. } => {
                if let TranscriptItem::Assistant { text, .. } = item.as_ref()
                    && !text.trim().is_empty()
                {
                    self.last_row = text.clone();
                }
            }
            SessionEvent::ToolCallProposed { call_id, name, .. } => {
                self.calls.insert(call_id, name);
                // A session that runs a thousand calls must not grow a map for ever; the
                // asks that are open are the only ones anything looks a name up for.
                if self.calls.len() > 256 {
                    self.calls.clear();
                }
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
            SessionEvent::DecisionAnswered { req_id, .. } => {
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
        Ok(Head {
            session: session.to_string(),
            worktree: d.workspace.clone(),
            socket: d.socket.clone(),
            shared,
            client,
        })
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
            closed: g.closed.clone(),
            rejection: g.rejection.clone(),
        }
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
    ///   session looks like — idle is a real answer, and the alternative is a `wait` that
    ///   hangs on a session that has nothing to do.
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
                Turn::Idle | Turn::Failed(_) if armed => {
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
            if !armed && now >= grace {
                // Nothing turned: answer with what is here rather than waiting out a
                // timeout that would report a health problem this session does not have.
                return match &g.turn {
                    Turn::Failed(why) => Attention::Failed { why: why.clone() },
                    _ => Attention::TurnEnded {
                        message: g.last_message().to_string(),
                    },
                };
            }
            let until = if armed { deadline } else { deadline.min(grace) };
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

/// The default session a bare attach lands on — what a daemon that has just started opens
/// eagerly, and what a bridge uses to ask for a new one before it holds one of its own.
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
