//! One letibot session held as an ACP session: the head's obligations, and the fold that turns the
//! daemon's events into `session/update` notifications.
//!
//! A head owes three things (`crates/sessionlog/src/client.rs`): attach with a `since_seq` and take
//! `Resync` as a normal answer, ack after handling a frame, and name an `expected_seq` on every
//! mutating frame. This discharges all three while writing JSON-RPC, which is the shape the bridge
//! has with a collector where a screen would be.
//!
//! One ACP session is one letibot session and one socket, so `session/new` makes a session on the
//! folder's daemon (`letibot_bridge::head::create_session_in`) and attaches to it: the socket a
//! head holds must be its session's from frame one.

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};

use letibot_bridge::daemon;
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::{
    DecisionOption, DeltaTarget, Envelope, FinishReason, OptionKind, SessionEvent,
};
use letibot_sessionlog::protocol::{Ack, Caps, PROTOCOL_VERSION, ServerFrame};
use letibot_transcript::ToolOutcome;
use serde_json::{Value, json};

use crate::acp::{
    AgentCapabilities, CancelNotification, ContentBlock, Implementation, InitializeRequest,
    InitializeResponse, McpCapabilities, NewSessionRequest, NewSessionResponse, PermissionOption,
    PermissionOptionKind, PermissionOutcome, PromptCapabilities, PromptRequest, PromptResponse,
    RequestPermissionRequest, RequestPermissionResponse, SessionNotification, SessionUpdate,
    StopReason, ToolCallContent, ToolCallLocation, ToolCallStatus, ToolCallUpdateRequest, ToolKind,
};
use crate::{context, marks, wire};

/// The handshake kind this head sends. The protocol's vocabulary is `tui`, `remote`, `flowy` and
/// `acp` (`ClientFrame::Attach`), and this is the one that says what it is: an ACP client's turn.
pub const HEAD_KIND: &str = "acp";

/// What the daemon's log calls this head. A person reading `HeadAttached` lines sees it, so it is
/// the binary's name and not a sentence.
pub const IDENTITY: &str = "acp";

/// How long a session creation waits for the daemon's answer. Not a poll interval: the daemon
/// answers a `NewSession` in one round trip, and this is the bound on one that is not answering.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

/// How many calls a session remembers the kind of, so the map cannot grow for ever.
const CALLS: usize = 256;

/// Anything that reaches the event loop. One channel for both directions, because the loop must
/// answer a `session/cancel` while a turn is streaming: two sources, one `recv`.
#[derive(Debug)]
pub enum Event {
    /// One line from the client, as it arrived: parsed in the loop, where a parse error can be
    /// answered.
    Client(String),
    /// One frame from a session's socket.
    Frame {
        session: String,
        inbound: Box<Inbound>,
    },
    /// The client closed its end. The sessions stay up on the daemon's side: a head that leaves
    /// does not end a turn, which is the whole point of a session outliving a head.
    Closed,
}

/// A session this adapter holds.
struct Session {
    /// The daemon's own workspace, for the absolute paths ACP's `diff` and `locations` want.
    workspace: String,
    client: HeadClient,
    seq: u64,
    /// The prompt the client is waiting on, while one runs.
    in_flight: Option<Value>,
    /// What the client was told each call is, so an update names the same kind.
    calls: BTreeMap<String, ToolKind>,
    /// The client asked for this turn to stop: the answer is `cancelled` whatever the turn says.
    cancelled: bool,
    /// Why the connection is gone, once it is.
    closed: Option<String>,
}

/// One decision this adapter has asked the client about, by the request id it used.
struct Pending {
    session: String,
    /// The daemon's own `req_id` for the ask.
    ask: String,
    /// The option to answer with when the client cancels: a decision nobody answers is still a
    /// decision the daemon is waiting for.
    refuse: Option<String>,
}

/// The adapter: the sessions it holds, the requests it has outstanding, and the one writer.
pub struct Adapter<W: Write> {
    out: W,
    sessions: BTreeMap<String, Session>,
    pending: BTreeMap<String, Pending>,
    next_request: u64,
    tx: Sender<Event>,
    rx: Receiver<Event>,
}

impl<W: Write> Adapter<W> {
    pub fn new(out: W) -> Adapter<W> {
        let (tx, rx) = channel();
        Adapter {
            out,
            sessions: BTreeMap::new(),
            pending: BTreeMap::new(),
            next_request: 1,
            tx,
            rx,
        }
    }

    /// Read the client until it closes the pipe, then leave every session where it is.
    pub fn run<R: Read + Send + 'static>(&mut self, input: R) {
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("acp-stdin".into())
            .spawn(move || {
                for line in std::io::BufReader::new(input).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    if tx.send(Event::Client(line)).is_err() {
                        return;
                    }
                }
                let _ = tx.send(Event::Closed);
            })
            .ok();
        while let Ok(event) = self.rx.recv() {
            match event {
                Event::Client(line) => self.line(&line),
                Event::Frame { session, inbound } => self.frame(&session, *inbound),
                Event::Closed => break,
            }
        }
        for session in self.sessions.values_mut() {
            let _ = session.client.detach();
        }
    }

    // -----------------------------------------------------------------------
    // the client's side
    // -----------------------------------------------------------------------

    fn line(&mut self, line: &str) {
        let incoming = match wire::parse(line) {
            Ok(incoming) => incoming,
            Err(malformed) => {
                let id = malformed.id.unwrap_or(Value::Null);
                self.write(wire::error(id, -32700, &malformed.detail));
                return;
            }
        };
        match incoming {
            wire::Incoming::Request { id, method, params } => self.request(id, &method, params),
            wire::Incoming::Notification { method, params } => {
                if method == "session/cancel" {
                    self.cancel(&params);
                }
            }
            wire::Incoming::Response { id, result } => {
                let outcome = serde_json::from_value::<RequestPermissionResponse>(result)
                    .map(|r| r.outcome)
                    .ok();
                self.settle(&id, outcome);
            }
            wire::Incoming::Failure { id, message } => {
                // A client that refuses the method itself has decided nothing, and the daemon
                // would wait its whole deadline for an answer: a refusal is read as a cancel.
                eprintln!("acp: the client refused a permission request: {message}");
                self.settle(&id, None);
            }
        }
    }

    fn request(&mut self, id: Value, method: &str, params: Value) {
        match method {
            "initialize" => self.initialize(id, params),
            "session/new" => self.new_session(id, params),
            "session/prompt" => self.prompt(id, params),
            other => self.write(wire::error(
                id,
                crate::acp::code::METHOD_NOT_FOUND,
                &format!("`{other}` is not a method this agent answers"),
            )),
        }
    }

    fn initialize(&mut self, id: Value, params: Value) {
        let asked: InitializeRequest = match serde_json::from_value(params) {
            Ok(asked) => asked,
            Err(e) => {
                return self.write(wire::error(
                    id,
                    crate::acp::code::INVALID_PARAMS,
                    &format!("initialize: {e}"),
                ));
            }
        };
        // The schema's answer for a client on another version is this agent's own number, and the
        // client decides whether it can go on (`$defs/InitializeResponse`).
        if asked.protocol_version != crate::acp::PROTOCOL_VERSION {
            eprintln!(
                "acp: the client speaks ACP {}, this adapter speaks {}",
                asked.protocol_version,
                crate::acp::PROTOCOL_VERSION
            );
        }
        self.write(wire::result(
            id,
            serde_json::to_value(InitializeResponse {
                protocol_version: crate::acp::PROTOCOL_VERSION,
                agent_capabilities: AgentCapabilities {
                    load_session: false,
                    prompt_capabilities: PromptCapabilities {
                        image: false,
                        audio: false,
                        embedded_context: true,
                    },
                    mcp_capabilities: McpCapabilities {
                        http: false,
                        sse: false,
                    },
                },
                agent_info: Some(Implementation {
                    name: "letibot".into(),
                    title: Some("letibot".into()),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                }),
            })
            .unwrap_or(Value::Null),
        ));
    }

    fn new_session(&mut self, id: Value, params: Value) {
        let asked: NewSessionRequest = match serde_json::from_value(params) {
            Ok(asked) => asked,
            Err(e) => {
                return self.write(wire::error(
                    id,
                    crate::acp::code::INVALID_PARAMS,
                    &format!("session/new: {e}"),
                ));
            }
        };
        match self.open(&asked.cwd) {
            Ok(session) => self.write(wire::result(
                id,
                serde_json::to_value(NewSessionResponse {
                    session_id: session,
                })
                .unwrap_or(Value::Null),
            )),
            Err(why) => self.write(wire::error(id, crate::acp::code::SESSION, &why)),
        }
    }

    /// Make a session on the folder's daemon and attach to it.
    ///
    /// A daemon that is not up is refused and not started: which model, mode and placement a
    /// folder's session runs on is the launcher's to choose (`docs/rano-integration.md` §3.3), and
    /// an adapter that guessed would be a second launcher.
    fn open(&mut self, cwd: &str) -> Result<String, String> {
        let daemon = daemon::resolve(Path::new(cwd))?;
        if !daemon::is_up(&daemon.socket) {
            return Err(format!(
                "no letibot daemon is running for {}: start one there with `letibot`, then ask \
                 again",
                daemon.workspace.display()
            ));
        }
        let workspace = daemon.workspace.display().to_string();
        let session = make_session(&daemon.socket, &workspace)?;
        let (client, hello, reader) = HeadClient::attach(
            &daemon.socket,
            &session,
            0,
            HEAD_KIND,
            IDENTITY,
            Caps {
                can_decide: true,
                row_bodies: true,
                ..Caps::default()
            },
        )
        .map_err(|e| format!("{}: {e}", daemon.socket.display()))?;
        match &hello {
            ServerFrame::Hello {
                protocol_version, ..
            } => {
                if let Some(skew) =
                    letibot_sessionlog::protocol_skew(*protocol_version, PROTOCOL_VERSION)
                {
                    eprintln!("acp: {skew}");
                }
            }
            ServerFrame::Bye { reason } => return Err(format!("the daemon said: {reason}")),
            other => return Err(format!("expected a hello, got {other:?}")),
        }
        let tag = session.clone();
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name(format!("acp-read-{tag}"))
            .spawn(move || {
                let (inner_tx, inner_rx) = channel();
                std::thread::Builder::new()
                    .name(format!("acp-pump-{tag}"))
                    .spawn(move || pump(reader, inner_tx))
                    .ok();
                for inbound in inner_rx {
                    if tx
                        .send(Event::Frame {
                            session: tag.clone(),
                            inbound: Box::new(inbound),
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .map_err(|e| format!("the session's reader did not start: {e}"))?;
        self.sessions.insert(
            session.clone(),
            Session {
                workspace,
                client,
                seq: 0,
                in_flight: None,
                calls: BTreeMap::new(),
                cancelled: false,
                closed: None,
            },
        );
        Ok(session)
    }

    fn prompt(&mut self, id: Value, params: Value) {
        let asked: PromptRequest = match serde_json::from_value(params) {
            Ok(asked) => asked,
            Err(e) => {
                return self.write(wire::error(
                    id,
                    crate::acp::code::INVALID_PARAMS,
                    &format!("session/prompt: {e}"),
                ));
            }
        };
        let Some(session) = self.sessions.get_mut(&asked.session_id) else {
            return self.write(wire::error(
                id,
                crate::acp::code::SESSION,
                &format!("no session `{}` in this adapter", asked.session_id),
            ));
        };
        if session.in_flight.is_some() {
            return self.write(wire::error(
                id,
                crate::acp::code::INVALID_REQUEST,
                "a prompt is still running in this session: ACP asks for one turn at a time, and \
                 the daemon would only queue the second",
            ));
        }
        if let Some(why) = session.closed.clone() {
            return self.write(wire::error(id, crate::acp::code::SESSION, &why));
        }
        let (text, context) = context::split(&asked.prompt);
        // `expected_seq = 0` is the protocol's "I am not claiming a position": this head is a
        // second one on a session a person may be typing into, and a prompt must never be dropped.
        match session
            .client
            .prompt_with_context(0, &text, Vec::new(), context)
        {
            Ok(_) => {
                session.in_flight = Some(id);
                session.cancelled = false;
            }
            Err(e) => self.write(wire::error(
                id,
                crate::acp::code::INTERNAL,
                &format!("the prompt did not reach the daemon: {e}"),
            )),
        }
    }

    /// `session/cancel` is a notification: ACP then expects the prompt's answer to be `cancelled`,
    /// whatever the turn was doing. The daemon's interrupt is the act the TUI's Esc is.
    fn cancel(&mut self, params: &Value) {
        let Ok(asked) = serde_json::from_value::<CancelNotification>(params.clone()) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&asked.session_id) else {
            return;
        };
        session.cancelled = true;
        let _ = session
            .client
            .interrupt(0, "the ACP client cancelled this turn");
    }

    /// The client's answer to a permission request this adapter sent. `None` is a client that
    /// refused the method, which decides nothing and is read as a cancel.
    fn settle(&mut self, id: &Value, outcome: Option<PermissionOutcome>) {
        let Some(pending) = self.pending.remove(&id.to_string()) else {
            // The daemon's own deadline settles a decision too, so an answer to an ask that is no
            // longer open is a race the protocol already allows.
            return;
        };
        let option = match outcome {
            Some(PermissionOutcome::Selected { option_id }) => option_id,
            Some(PermissionOutcome::Cancelled) | None => match pending.refuse {
                Some(refuse) => refuse,
                // A ladder can be all-allow, which is what `allow-all` is: a cancel then has no
                // refusal to send, and stopping the turn is the honest reading of what was asked.
                None => {
                    if let Some(session) = self.sessions.get_mut(&pending.session) {
                        let _ = session.client.interrupt(0, "the ACP client cancelled");
                    }
                    return;
                }
            },
        };
        if let Some(session) = self.sessions.get_mut(&pending.session)
            && let Err(e) = session
                .client
                .answer_with(&pending.ask, &option, None, None)
        {
            eprintln!("acp: the answer did not reach the daemon: {e}");
        }
    }

    // -----------------------------------------------------------------------
    // the daemon's side
    // -----------------------------------------------------------------------

    fn frame(&mut self, session_id: &str, inbound: Inbound) {
        let frame = match inbound {
            Inbound::Frame(frame) => frame,
            // One unreadable line is a fact about two builds, not about the stream: the head
            // carries on, and the person reads the complaint on stderr.
            Inbound::Unreadable(u) => {
                eprintln!("acp: {session_id}: {}", u.said());
                return;
            }
        };
        let env = match frame {
            ServerFrame::Event(env) => env,
            ServerFrame::Bye { reason } => {
                self.write_bye(session_id, &reason);
                return;
            }
            // Every ACP client was handed no history in the first place, so nothing here is stale:
            // the frame asks for a fresh snapshot and this adapter draws none of it.
            ServerFrame::Resync { .. } => return,
            ServerFrame::Rejected { reason, .. } => {
                eprintln!("acp: {session_id}: the daemon refused a frame: {reason}");
                return;
            }
            // A snapshot, a job list, a todo board: this adapter draws none of them.
            _ => return,
        };
        self.fold(session_id, env);
    }

    fn fold(&mut self, session_id: &str, env: Envelope) {
        let out = &mut self.out;
        let pending = &mut self.pending;
        let next = &mut self.next_request;
        let Some(session) = self.sessions.get_mut(session_id) else {
            return;
        };
        session.seq = session.seq.max(env.seq);
        match env.event {
            SessionEvent::Delta { target, text, .. } => {
                let content = ContentBlock::Text { text };
                let one = match target {
                    DeltaTarget::Text => SessionUpdate::AgentMessageChunk { content },
                    DeltaTarget::Reasoning => SessionUpdate::AgentThoughtChunk { content },
                    // A call's arguments stream into the call they belong to, and ACP's
                    // `tool_call` carries the input rather than a stream of it.
                    DeltaTarget::ToolCall => {
                        ack(session);
                        return;
                    }
                };
                update(out, session_id, one);
            }
            SessionEvent::ToolCallProposed {
                call_id,
                name,
                target,
                ..
            } => {
                let kind = kind_of(&name);
                session.calls.insert(call_id.clone(), kind);
                // A session that runs a thousand calls must not grow a map for ever: the asks that
                // are open are the only ones anything looks a kind up for.
                if session.calls.len() > CALLS {
                    session.calls.clear();
                }
                update(
                    out,
                    session_id,
                    SessionUpdate::ToolCall {
                        tool_call_id: call_id,
                        title: title_of(&name, &target),
                        kind,
                        status: ToolCallStatus::InProgress,
                        locations: location_of(&session.workspace, &name, &target)
                            .into_iter()
                            .collect(),
                    },
                );
            }
            SessionEvent::ToolFinished {
                call_id,
                outcome,
                edits,
                ..
            } => {
                // The excerpt's two sides ARE ACP's diff: the region that changed, not the file.
                let content: Vec<ToolCallContent> = edits
                    .iter()
                    .map(|e| marks::edit(e, &session.workspace))
                    .collect();
                let status = match outcome {
                    ToolOutcome::Ok | ToolOutcome::Backgrounded { .. } => ToolCallStatus::Completed,
                    ToolOutcome::Failed { .. }
                    | ToolOutcome::Denied { .. }
                    | ToolOutcome::NotRun { .. }
                    | ToolOutcome::Abstained { .. }
                    | ToolOutcome::Timeout => ToolCallStatus::Failed,
                };
                let locations: Vec<ToolCallLocation> = edits
                    .iter()
                    .map(|e| ToolCallLocation {
                        path: marks::absolute(&session.workspace, &e.path),
                        line: Some(e.after_start as u32),
                    })
                    .collect();
                match session.calls.get(&call_id).copied() {
                    Some(_kind) => update(
                        out,
                        session_id,
                        SessionUpdate::ToolCallUpdate {
                            tool_call_id: call_id,
                            status: Some(status),
                            content: (!content.is_empty()).then_some(content),
                            locations: (!locations.is_empty()).then_some(locations),
                        },
                    ),
                    // A call this head never saw proposed still ran: create its row rather than
                    // updating one the client has never been told about.
                    None => update(
                        out,
                        session_id,
                        SessionUpdate::ToolCall {
                            tool_call_id: call_id.clone(),
                            title: call_id,
                            kind: ToolKind::Other,
                            status,
                            locations,
                        },
                    ),
                }
            }
            SessionEvent::Citations { citations, .. } => {
                for c in &citations {
                    let path = marks::absolute(&session.workspace, &c.path);
                    let cited = marks::Cited {
                        label: &c.label,
                        path: &path,
                        start: c.start,
                        end: c.end,
                    };
                    if let Some(one) = marks::cited(&cited) {
                        update(out, session_id, one);
                    }
                }
            }
            SessionEvent::DecisionRequested {
                req_id,
                kind,
                call_id,
                summary,
                options,
                ..
            } if kind == "permission" => {
                let listed: Vec<PermissionOption> = options.iter().map(permission_option).collect();
                let refuse = options
                    .iter()
                    .find(|o| matches!(o.kind, OptionKind::RejectOnce | OptionKind::RejectAlways))
                    .map(|o| o.option_id.clone());
                let asked = *next;
                *next += 1;
                let request = wire::request(
                    json!(asked),
                    "session/request_permission",
                    serde_json::to_value(RequestPermissionRequest {
                        session_id: session_id.to_string(),
                        tool_call: ToolCallUpdateRequest {
                            tool_call_id: call_id.clone().unwrap_or_else(|| req_id.clone()),
                            title: summary,
                            kind: call_id
                                .as_ref()
                                .and_then(|c| session.calls.get(c).copied())
                                .unwrap_or(ToolKind::Other),
                        },
                        options: listed,
                    })
                    .unwrap_or(Value::Null),
                );
                pending.insert(
                    json!(asked).to_string(),
                    Pending {
                        session: session_id.to_string(),
                        ask: req_id,
                        refuse,
                    },
                );
                write(out, request);
            }
            // A question is the model asking a person something, and ACP v1 has no stable
            // construct for it (`elicitation` is still behind a capability this adapter does not
            // advertise): the session's own screen head is where it is answered.
            SessionEvent::DecisionRequested { req_id, .. } => {
                eprintln!(
                    "acp: {session_id}: the session asked a question (`{req_id}`), which is \
                           answered in letibot and not here"
                );
            }
            SessionEvent::TurnFinished {
                finish_reason,
                ends_turn,
                ..
            } => {
                if ends_turn {
                    let why = match finish_reason {
                        FinishReason::Length => StopReason::MaxTokens,
                        _ => StopReason::EndTurn,
                    };
                    finish(out, session, why);
                }
            }
            SessionEvent::TurnInterrupted { .. } => finish(out, session, StopReason::Cancelled),
            SessionEvent::TurnFailed { error, .. } => {
                if let Some(id) = session.in_flight.take() {
                    write(
                        out,
                        wire::error(
                            id,
                            crate::acp::code::INTERNAL,
                            &format!("the turn failed: {error}"),
                        ),
                    );
                }
            }
            // A warning is not a conversation row (`Hub::warn` reaches heads and no engine), and
            // ACP has no notice a client promised to draw: stderr is where it goes until a client
            // advertises `session.notices`.
            SessionEvent::Warning { code, detail, .. } => {
                eprintln!("acp: {session_id}: {code}: {detail}");
            }
            _ => {}
        }
        ack(session);
    }

    fn write_bye(&mut self, session_id: &str, reason: &str) {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return;
        };
        let said = format!("the daemon closed this session: {reason}");
        session.closed = Some(said.clone());
        if let Some(id) = session.in_flight.take() {
            self.write(wire::error(id, crate::acp::code::SESSION, &said));
        }
    }

    fn write(&mut self, message: Value) {
        write(&mut self.out, message);
    }
}

/// Make a session on a daemon and hand back its id.
///
/// `NewSession` does not switch, so this is a bare connection that creates and leaves: the socket
/// this adapter goes on to hold must be its session's from frame one (the rule `bridge::head`
/// states for the same dance). It attaches under [`IDENTITY`] and not the bridge's, because the
/// daemon records the opening head as the session's origin - and a session a person opened from
/// their editor is theirs, not a package's.
fn make_session(socket: &Path, workspace: &str) -> Result<String, String> {
    let (mut client, hello, reader) = HeadClient::attach(
        socket,
        "",
        0,
        HEAD_KIND,
        IDENTITY,
        Caps {
            can_decide: false,
            ..Caps::default()
        },
    )
    .map_err(|e| format!("{}: {e}", socket.display()))?;
    client.seated_by(&hello);
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name(format!("acp-create-{}", std::process::id()))
        .spawn(move || pump(reader, tx))
        .map_err(|e| format!("the create reader did not start: {e}"))?;
    client
        .new_session("", workspace)
        .map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(format!(
                "the daemon did not answer the new-session request within {PATIENCE:?}"
            ));
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
                return Err(format!(
                    "the daemon did not answer the new-session request within {PATIENCE:?}"
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("the daemon closed the connection".into());
            }
        }
    }
}

/// The head's third obligation: a frame is acked once it has been handled, so a crash costs a
/// duplicate frame and never a silence.
fn ack(session: &mut Session) {
    let _ = session.client.ack(Ack {
        seq: session.seq,
        rendered: 1,
        filtered: 0,
    });
}

/// Answer the prompt whose turn just ended. A turn this adapter did not start is left alone: ACP
/// has no row for a reply nobody asked it for.
fn finish<W: Write>(out: &mut W, session: &mut Session, why: StopReason) {
    let Some(id) = session.in_flight.take() else {
        return;
    };
    let why = if session.cancelled {
        StopReason::Cancelled
    } else {
        why
    };
    let result = serde_json::to_value(PromptResponse { stop_reason: why }).unwrap_or(Value::Null);
    write(out, wire::result(id, result));
}

fn write<W: Write>(out: &mut W, message: Value) {
    let mut line = serde_json::to_string(&message).unwrap_or_else(|_| "{}".into());
    line.push('\n');
    let _ = out.write_all(line.as_bytes());
    let _ = out.flush();
}

fn update<W: Write>(out: &mut W, session_id: &str, update: SessionUpdate) {
    let params = serde_json::to_value(SessionNotification {
        session_id: session_id.to_string(),
        update,
    })
    .unwrap_or(Value::Null);
    write(out, wire::notification("session/update", params));
}

/// The ACP kind a tool of this name is. A name this build has never heard of is `other`, which is
/// ACP's own default.
fn kind_of(name: &str) -> ToolKind {
    match name {
        "read" => ToolKind::Read,
        "write" | "edit" => ToolKind::Edit,
        "bash" => ToolKind::Execute,
        "grep" | "glob" | "outline" | "digest" => ToolKind::Search,
        "web_fetch" | "websearch" | "webfetch" => ToolKind::Fetch,
        "task" | "think" => ToolKind::Think,
        _ => ToolKind::Other,
    }
}

/// A call's one-line title: the tool and the one argument a person reads, which is the pair the
/// head's own tool row draws.
fn title_of(name: &str, target: &str) -> String {
    if target.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {target}")
    }
}

/// The file a call names, when the argument looks like one: ACP clients use it to follow along.
fn location_of(workspace: &str, name: &str, target: &str) -> Option<ToolCallLocation> {
    let reads_a_path = matches!(
        name,
        "read" | "write" | "edit" | "outline" | "grep" | "glob" | "lsp"
    );
    if !reads_a_path || target.is_empty() || target.contains(char::is_whitespace) {
        return None;
    }
    Some(ToolCallLocation {
        path: marks::absolute(workspace, target),
        line: None,
    })
}

/// A letibot rung as the ACP option it is: the two ladders are the same four moves, and the
/// daemon's wider grants (`allow_session`, `allow_always`) are ACP's `allow_always`.
fn permission_option(option: &DecisionOption) -> PermissionOption {
    let kind = match option.kind {
        OptionKind::AllowOnce => PermissionOptionKind::AllowOnce,
        OptionKind::AllowSession | OptionKind::AllowAlways => PermissionOptionKind::AllowAlways,
        OptionKind::RejectOnce => PermissionOptionKind::RejectOnce,
        OptionKind::RejectAlways => PermissionOptionKind::RejectAlways,
    };
    PermissionOption {
        option_id: option.option_id.clone(),
        name: option.label.clone(),
        kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two ladders are the same four moves, and the daemon's wider grants collapse into ACP's
    /// `allow_always`: a session grant shown as `allow_once` would be a lie about its reach.
    #[test]
    fn a_decision_ladder_is_the_acp_permission_ladder() {
        let rung = |kind| PermissionOption {
            option_id: "x".into(),
            name: "x".into(),
            kind,
        };
        let mapped = |kind| {
            permission_option(&DecisionOption {
                option_id: "x".into(),
                label: "x".into(),
                kind,
            })
        };
        assert_eq!(
            mapped(OptionKind::AllowOnce),
            rung(PermissionOptionKind::AllowOnce)
        );
        assert_eq!(
            mapped(OptionKind::AllowSession),
            rung(PermissionOptionKind::AllowAlways)
        );
        assert_eq!(
            mapped(OptionKind::AllowAlways),
            rung(PermissionOptionKind::AllowAlways)
        );
        assert_eq!(
            mapped(OptionKind::RejectOnce),
            rung(PermissionOptionKind::RejectOnce)
        );
        assert_eq!(
            mapped(OptionKind::RejectAlways),
            rung(PermissionOptionKind::RejectAlways)
        );
    }

    /// The kind a call is drawn as, and the one argument a person reads: the same pair the head's
    /// tool row has.
    #[test]
    fn a_tool_name_is_an_acp_kind_and_a_title() {
        assert_eq!(kind_of("read"), ToolKind::Read);
        assert_eq!(kind_of("edit"), ToolKind::Edit);
        assert_eq!(kind_of("bash"), ToolKind::Execute);
        assert_eq!(kind_of("grep"), ToolKind::Search);
        assert_eq!(kind_of("something_new"), ToolKind::Other);
        assert_eq!(title_of("read", "docs/a.md"), "read: docs/a.md");
        assert_eq!(title_of("todo", ""), "todo");
        assert_eq!(
            location_of("/ws", "read", "docs/a.md"),
            Some(ToolCallLocation {
                path: "/ws/docs/a.md".into(),
                line: None
            })
        );
        assert_eq!(
            location_of("/ws", "bash", "git status"),
            None,
            "a command is not a place"
        );
    }
}
