//! Fan-out, attach, resync, and the serialized command queue.
//!
//! This is where §13.2's model and §13.2b's mechanics become one object.
//!
//! # The rules, and where each one is
//!
//! - **The daemon is the only reader of the model stream.** Nothing here reads a
//!   socket; the turn engine pushes into [`Hub::publish`]. Several heads share one
//!   authoritative reader (§13.2b's `waiterlock.go` lesson) because there is
//!   structurally only one.
//! - **Non-blocking fan-out.** [`Hub::publish`] takes the lock, appends, and pushes
//!   into each head's bounded queue. A full queue **demotes that head to resync**;
//!   it never blocks and never drops silently. One stalled client cannot stall a
//!   turn.
//! - **Snapshot and register under one lock.** [`Hub::attach`] appends
//!   `HeadAttached`, cuts the snapshot and installs the subscriber in a single
//!   critical section. *"The missing byte is usually the prompt."*
//! - **Idle is quiet, not unwatched.** Nothing in this file counts heads to decide
//!   whether to keep working. Detaching the last head is a `HeadDetached` event and
//!   nothing else.
//! - **Resync is a normal outcome, never an error.** It is a delivery variant, not
//!   an `Err`.
//! - **TCP close is detach, never abort.** [`Hub::detach`] does not touch any turn.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use letibot_transcript::TranscriptItem;

use crate::cursor::{Batch, ReadMark};
use crate::event::{Envelope, SessionEvent};
use crate::log::{LogBounds, SessionLog};
use crate::protocol::{
    Ack, Caps, REJECT_READ_ONLY, REJECT_STALE_SEQ, REJECT_UNKNOWN_DECISION, ServerFrame,
};
use crate::scrub::{ScrubReport, StoredProjection, scrub_replay};
use crate::view::{SessionView, Snapshot, ViewBounds};

/// What a head gets from one wait.
#[derive(Debug, Clone, PartialEq)]
pub enum Delivery {
    /// Events, in seq order, contiguous with the last delivery.
    Events(Batch),
    /// **Frames that are not the record** — the pane's byte stream, and anything else that
    /// belongs to *this connection, now* rather than to the session's log.
    ///
    /// A second variant rather than a second channel, because the pump in
    /// `server.rs` owns one ordering and two sources of frames on one socket need one
    /// ordering. **Delivered ahead of [`Delivery::Events`]**, and that is the point of the
    /// variant: a screen program's redraw must not sit behind a batch of a thousand deltas,
    /// and a keystroke that arrives after the row it was answering is not a keystroke.
    ///
    /// Nothing here is ever appended to the log, replayed, or scrubbed. See
    /// [`crate::protocol::ServerFrame::TermOutput`] for the argument.
    Frames(Vec<ServerFrame>),
    /// This head fell behind, or asked. **Not an error.** Reset to the snapshot and
    /// continue from `snapshot.seq + 1`.
    Resync {
        reason: String,
        dropped: u64,
        snapshot: Box<Snapshot>,
        scrubbed: ScrubReport,
    },
    /// The daemon is shutting down, **and everything queued for this head has
    /// already been delivered.**
    ///
    /// The second half is a guarantee rather than an observation: a head that is
    /// told `daemon_stopping` and then `Bye` must actually receive both, in that
    /// order, or the one sentence explaining why the session went away is lost to a
    /// race. See `next_batch`, where honouring `closed` before the queue did exactly
    /// that.
    Closed,
}

/// The answer to ATTACH.
#[derive(Debug, Clone, PartialEq)]
pub struct Attached {
    pub head_id: String,
    pub dropped: u64,
    /// `Some` for a snapshot attach or a demoted resume.
    pub snapshot: Option<Snapshot>,
    /// `Some` for a resume served from the scrollback.
    pub resumed_from: Option<u64>,
    /// What the replay scrub stripped. The daemon's half of "report what was
    /// filtered".
    pub scrubbed: ScrubReport,
    /// The gap, already scrubbed, for a resume. Empty for a snapshot attach.
    pub backlog: Vec<Envelope>,
}

/// **The daemon's own name in a submit.**
///
/// A name no head can have, because no head is minted with a NUL in it (a seat's id comes from
/// the `Hello` this hub mints), and the vocabulary `harnessd` already used for *"a caller that
/// is not a head at all"* when it tried to stop a subagent — where the string alone was not
/// enough, because `submit` has to know it. See the arm in [`Hub::submit`]: this name is
/// admitted for an [`CommandKind::Interrupt`] and a relayed [`CommandKind::Message`], and for
/// nothing else.
pub const DAEMON_SUBMITTER: &str = "\0daemon";

/// **How many unread pane frames one head may hold before the oldest is dropped.**
///
/// A bound rather than a queue that grows: the pane's bytes are produced by a *process* — a
/// program redrawing as fast as it likes — and an unbounded queue would be a program that can
/// make the daemon hold a gigabyte by painting. The number is generous for a screen and small
/// against a runaway: a full repaint of a 200×50 pane is about 4 KB, so this is several hundred
/// frames of slack and about a megabyte of JSON in the worst case.
const MAX_SIDE: usize = 256;

/// A mutating command, after validation, waiting for the session's single command
/// worker. §13.2: *"Commands are serialized on a per-session queue."*
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedCommand {
    pub head_id: String,
    pub identity: String,
    pub client_request_id: String,
    /// The log's head seq when the command was accepted.
    pub at_seq: u64,
    pub kind: CommandKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandKind {
    Prompt {
        text: String,
    },
    /// Compact the session: one summary turn, then the history is replaced by
    /// that summary through a transcript fork. Serialized on the queue like a
    /// prompt — it *is* a turn — and accepted on a stale `expected_seq` for the
    /// same reason a prompt is: a head that asked while the screen moved still
    /// meant it.
    Compact,
    /// **Rebuild this conversation's prompt from the tools seated now**, forking it
    /// onto the new prefix the way a compaction forks onto a summary. A turn, so
    /// it is queued exactly like one.
    ///
    /// The tool schemas live in the stable prefix and a session's is fixed when it
    /// is created, so a conversation opened without a shell can never call one
    /// however its daemon is later seated. This is the only thing that changes it.
    Reseat {
        /// Summarise as well as re-seating. The default is to carry every item
        /// across. See `ClientFrame::ReseatSession`.
        summarise: bool,
    },
    Interrupt {
        reason: String,
    },
    /// **A parent speaking to one of its own subagents while that child is in flight** —
    /// a live correction, delivered into the turn that is already running.
    ///
    /// Not a [`CommandKind::Prompt`], and the variant exists for that reason alone: a
    /// prompt is *the operator, typing into a running turn* — it is recorded as the
    /// operator (`Speaker::Operator`), consecutive ones coalesce into a single held
    /// message, and a take-back drops them. A parent's correction is none of that. It is
    /// an agent's utterance arriving in a session where that agent is not seated, and the
    /// child's steering path already has a door for exactly that (`SteeringMessage::normal`,
    /// recorded `Speaker::Agent`). Reusing `Prompt` would write the correction into the
    /// child's transcript as though the operator had typed it.
    ///
    /// `from` is the parent's session id, carried so the child — and anyone reading its
    /// trail afterwards — can say *which* parent spoke. It is not the submitter: the daemon
    /// relays this from another session's hub, and [`Hub::submit`] records `from` as the
    /// identity rather than [`DAEMON_SUBMITTER`].
    Message {
        from: String,
        text: String,
    },
    /// A head asked to move the running command to the background. **Not queued
    /// like a prompt** — it is acted on by the exec backend's wait loop, which is
    /// already blocked where the worker cannot reach — but the frame still rides the
    /// queue so the between-turns case is announced rather than dropped.
    Promote,
    /// Move this session's project to a named point (`allow-all`, `writes-allowed`,
    /// an opencode name…), persisted in the mode store. Serialized on the queue like
    /// everything else; unlike a prompt it does not start a turn. See `D13`.
    Mode {
        name: String,
        /// Carried from the frame: the operator confirmed an unconfined `allow-all`.
        /// Not defaulted here — a queued command is built in this process and every
        /// construction site should have to say which it means.
        consented: bool,
    },
    /// **An operator's own call, admitted before it runs** — R24 part two, decision 4.
    ///
    /// Carries what it must for the daemon to write the admission as a person's act:
    /// the head's `call_id` (the key [`CommandKind::OperatorResult`] comes back under), the
    /// name that was already checked against [`crate::protocol::HEAD_RUN_TOOLS`] on the
    /// connection's thread, and the identity of the head that asked.
    ///
    /// It rides the command queue like every other verb so the admission is written by the
    /// same single worker that writes every other adjudication — a second writer of the
    /// corpus is a second place for it to disagree with itself.
    OperatorCall {
        call_id: String,
        name: String,
        arguments: String,
        /// The head that asked. Becomes the `who` in `human:<who>` and in the row's
        /// `CallOrigin`, so the two records name the actor the same way.
        who: String,
        /// **Who runs it** — R31. `true` is the daemon, through the tool this session
        /// already seats; `false` is the head, which then sends `OperatorResult`. Carried
        /// through the queue unchanged so the admission and the execution cannot disagree
        /// about which of them it was.
        execute: bool,
    },
    /// **What that call produced.** Appends the `ToolResult` row with its `origin` set.
    OperatorResult {
        call_id: String,
        outcome: letibot_transcript::ToolOutcome,
        payload: String,
    },
    /// **The operator's own shell line — a `!` command, run by the daemon.**
    ///
    /// Not the door ([`CommandKind::OperatorCall"]): that frames a call a TOOL owns, checked
    /// against [`crate::protocol::HEAD_RUN_TOOLS`] on the connection's thread and admitted with a
    /// corpus row. A shell line owns itself — the operator typed it — so there is no name to check
    /// and no admission to record. What this carries is the typed line (bang included, so the
    /// `User` row is the operator's words verbatim) and the identity of the head that asked, which
    /// becomes the `who` in the row's `CallOrigin` exactly as the door's does.
    ///
    /// It rides the queue for the same reason every other verb does: the single worker is the one
    /// writer of the session's transcript, and a second writer is a second place for it to
    /// disagree with itself.
    OperatorShell {
        /// The line as submitted, `!` first. Validated at the frame (`ClientFrame::OperatorShell`),
        /// so anything reaching this queue already passed the daemon's re-check.
        line: String,
        /// The head that asked. Becomes the `who` in `CallOrigin::Operator`, so the row and the
        /// door's rows name the actor the same way.
        who: String,
    },
    /// A slash command for the daemon: `flowy …`, `models …`.
    Slash {
        line: String,
    },
    /// **Read a window of one background job's output, for a head's jobs pane.**
    ///
    /// A command rather than a request-answered frame, because job output lives in the exec host
    /// — the worker's, not the server's — so the ask has to reach the worker. Publishing the
    /// answer as [`crate::event::SessionEvent::JobOutput`] is then the same rule a slash reply
    /// follows: a verb reads, and what it found lands on the log.
    ///
    /// `offset` is absolute into the job's output, so it stays meaningful after the ring moves
    /// under it, and the answer names the next one.
    ReadJobOutput {
        job: String,
        offset: u64,
    },
    /// A head took its queued prompts back — the operator pulled the queued line
    /// into the composer to edit it. Consumed by the running turn's steering
    /// poll (which drops the head's held operator text with it); between turns
    /// it is a quiet no-op, because a prompt that survived to here is about to
    /// run as its own turn and is no longer the operator's to take back.
    WithdrawPrompts,
    /// **The operator's half of the todo board**, replaced wholesale.
    ///
    /// A head owns these rows — they are its own store's contents — so it sends the whole list on
    /// every change rather than a delta: a delta protocol for a list of tens of items would be a
    /// second source of truth about them, and `TodoBoard::set_operator` replaces one half atomically.
    ///
    /// Nothing here is a decision and nothing is gated: the operator's own list is not a tool call.
    SetOperatorTodos {
        items: Vec<crate::event::TodoEntry>,
    },
    /// A head settled an open request. **Which kind** it settled is [`Reply`], and
    /// it is an enum rather than two variants here because every consumer that only
    /// cares "an answer arrived for `req_id`" already destructures this variant with
    /// `..` — a second variant would have silently skipped those arms.
    Answer {
        req_id: String,
        reply: Reply,
    },
}

/// What a head sent back, and the two things it can be.
///
/// §11.6 says a permission and a question are one mechanism differing in `kind`.
/// This is that difference, made a type: one `req_id`, two payloads, and no way to
/// pass one where the other is read.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// An adjudication grant or denial, by option id — `AllowOnce`, `RejectOnce`
    /// and the rest live behind that id.
    Permission {
        option_id: String,
        /// The operator's own glob for an *always allow*, when they typed one.
        /// `None` means *use the pattern derived from the call*, which is what
        /// every head did before this existed — never *match nothing*.
        pattern: Option<String>,
        /// **What the operator wants the model told**, for `deny_and_tell`.
        ///
        /// That option's label has always been *"Deny, and tell the model why"*
        /// and nothing carried the why: the head had no field for it, the wire
        /// had no field for it, and no code anywhere branched on the id. The
        /// operator: *"deny and tell doesnt work - there is no input for the
        /// 'tell' part"*. It was a label for a feature that was never built.
        ///
        /// `None` on every other option, and on a `deny_and_tell` answered from
        /// the ladder with nothing typed — which then reads as a plain denial
        /// and says so, rather than claiming a reason was given.
        note: Option<String>,
    },
    /// A person's answer to a question: a choice, a note on it, or typed text
    /// (`PROTOCOL_VERSION` 5). Deliberately **not** expressed with
    /// [`crate::event::OptionKind`] — see `crates/sessionlog/src/question.rs`.
    Question(crate::question::QuestionAnswer),
}

impl Reply {
    /// For the `CommandIssued` announcement and the counters.
    pub fn as_str(&self) -> &'static str {
        match self {
            Reply::Permission { .. } => "permission",
            Reply::Question(_) => "question",
        }
    }
}

/// **Where an answer goes when somebody is blocked waiting for it.**
///
/// The command queue is the wrong door for an answer and the reason is a deadlock,
/// not a preference. §13.2's one worker drains [`Hub::take_own_work`] and runs the
/// turn; a gated tool call happens **inside** that turn, so the worker is inside
/// `dispatch` when the adjudicator blocks. An answer pushed onto the same queue is
/// drained by the thread that is waiting for it, which is never.
///
/// So an answer is delivered **on the socket reader's thread**, synchronously, the
/// way [`crate::protocol::ClientFrame::ListSessions`] is answered off the queue for
/// the same class of reason: it is not an act on the session's timeline, it is the
/// second half of an act already in flight.
///
/// Installed by whoever holds the pending decisions — `letibot-harnessd`'s
/// `Answers`. A hub with no sink keeps the old behaviour and queues the command,
/// which is what every test in this crate drives and what a daemon with no
/// adjudication still does.
pub trait AnswerSink: Send + Sync {
    /// Deliver an answer to whoever is waiting for `req_id`.
    ///
    /// Returns whether anybody was. `false` means the answer reached the sink and
    /// found nothing waiting — a decision that already timed out, or one this
    /// process never asked. The caller does **not** turn that into an error: the
    /// head is told the command was accepted, and the late answer is dropped rather
    /// than applied to a call that has already been refused. A late allow applied to
    /// a call the gate reported as `not_run` is the worst available outcome.
    fn answer(&self, req_id: &str, identity: &str, reply: &Reply) -> bool;

    /// **An interrupt arrived**: stop waiting for anything still open.
    ///
    /// A waiter woken this way has *not* been answered — it reports cancellation,
    /// which the gate fails closed on. Without this an operator who pressed Esc
    /// twice would watch the session sit out the rest of a decision's deadline,
    /// because the thread holding the tool call is not the thread that drains
    /// interrupts.
    fn cancel(&self, why: &str);

    /// For the startup disclosure and `EXPLAIN`: who can actually answer here.
    fn describe(&self) -> String;
}

impl CommandKind {
    fn verb(&self) -> &'static str {
        match self {
            CommandKind::Prompt { .. } => "prompt",
            CommandKind::Compact => "compact",
            CommandKind::Reseat { .. } => "reseat",
            CommandKind::Interrupt { .. } => "interrupt",
            CommandKind::Message { .. } => "message",
            CommandKind::Answer { .. } => "answer",
            CommandKind::Mode { .. } => "mode",
            CommandKind::Slash { .. } => "slash",
            CommandKind::ReadJobOutput { .. } => "read-job-output",
            CommandKind::OperatorCall { .. } => "operator-call",
            CommandKind::OperatorResult { .. } => "operator-result",
            CommandKind::OperatorShell { .. } => "operator-shell",
            CommandKind::WithdrawPrompts => "take-back",
            CommandKind::SetOperatorTodos { .. } => "operator todos",
            CommandKind::Promote => "promote",
        }
    }
}

struct Head {
    id: String,
    kind: String,
    identity: String,
    caps: Caps,
    queue: VecDeque<Envelope>,
    /// **Frames addressed to this head alone, and never to the log.** The pane's byte
    /// stream, and nothing else today — see [`Delivery::Frames`]. Drained before `queue`.
    ///
    /// Bounded, and **the bound is a drop rather than a wait**: a head that cannot keep up
    /// with a screen's repaints is a head that has stopped reading its socket, and blocking
    /// the writer here would stop the pty's reader thread, which would stop the program.
    /// A dropped pane frame is a repaint that is one frame stale, which a terminal survives;
    /// a blocked one is a program that stops drawing.
    side: VecDeque<ServerFrame>,
    /// `Some(reason)` once this head has been demoted. Its queue is cleared and
    /// stays cleared until it takes the resync.
    needs_resync: Option<String>,
    mark: ReadMark,
}

struct Inner {
    log: SessionLog,
    view: SessionView,
    heads: Vec<Head>,
    /// **Admitted operator calls their head has not reported yet** (R24), keyed by the
    /// head's own `call_id` → `(owning head, name, who)`. See [`Hub::detach`].
    operator_calls: std::collections::HashMap<String, (String, String, String)>,
    next_head: u64,
    commands: VecDeque<QueuedCommand>,
    /// **A wake aimed at the thread that owns this session's turn rather than at the
    /// daemon's worker.** See [`Hub::wake_its_own_reader`] for who sends one and why the
    /// bell cannot be used for it. Taken once, by [`Hub::take_own_work`].
    own_wake: bool,
    closed: bool,
    /// Rung when this hub takes a command, so **one** worker can wait on many
    /// sessions without a timer. See [`crate::registry`].
    ///
    /// `None` for a hub nobody registered, which is the single-session case and
    /// every test in this file: [`Hub::take_own_work`] blocks on this hub's own
    /// condvar and needs no bell at all.
    bell: Option<Arc<crate::registry::Bell>>,
    /// Where a settled decision goes instead of the command queue. See
    /// [`AnswerSink`] for why the queue cannot carry it.
    answers: Option<Arc<dyn AnswerSink>>,
    /// Open password requests: `req_id` → the channel to the `askpass` connection
    /// waiting on it. The secret goes through the channel and nowhere else — not
    /// the log, not the view, not a `CommandIssued`.
    secrets: HashMap<String, std::sync::mpsc::SyncSender<Option<String>>>,
    next_secret: u64,
    /// Open screen requests: `req_id` → the tool call waiting for a head to draw
    /// itself. First answer wins; later ones find nothing and are dropped.
    screens: HashMap<String, std::sync::mpsc::SyncSender<(usize, usize, Vec<String>)>>,
    next_screen: u64,
}

/// What a session looks like from outside it: enough for a picker, and cheap
/// enough to compute for every session on every list.
///
/// Cut under the hub's own lock in one pass, rather than assembled from four
/// accessors, because four accessors are four different instants and a list in
/// which one row is 30 ms older than the next is a list that can show a session as
/// both running and finished.
///
/// `Default` is all zeroes and an empty id: what a session that has **no hub** looks
/// like, which is the shape a stored-but-not-live session takes in a listing. It is
/// derived rather than hand-written so that a field added here cannot quietly acquire
/// a plausible default nobody chose.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionStatus {
    pub session_id: String,
    /// The head seq: how much has happened here.
    pub seq: u64,
    /// Transcript rows the view is holding.
    pub items: usize,
    /// Heads attached right now. Zero is normal and is **not** a reason to stop
    /// anything: idle is quiet, not unwatched.
    pub heads: usize,
    /// A turn is generating in this session at this instant.
    pub running: bool,
    /// The model the last turn named, or empty before the first turn.
    pub model: String,
    /// `Envelope::ts` of the last event. Zero before anything has happened.
    pub last_ms: u64,
}

/// **What the thread that owns a session's queue found to do.**
///
/// Two kinds of thing, and the name of the second is the point: a `Wake` is not a command
/// and carries no text, because what it means is *look at what you own and run the turn it
/// makes* — the same job [`crate::registry::Bell::ring_wake`] does for a session the daemon
/// holds, said to the thread that holds this one instead. See
/// [`Hub::wake_its_own_reader`].
#[derive(Debug)]
pub enum OwnWork {
    /// A command somebody submitted: a head's prompt, a parent's kill, a relayed message.
    Command(QueuedCommand),
    /// Something this session owns has settled and nobody else will tell it.
    Wake,
    /// The hub is closed and the queue is empty: this reader is done.
    Closed,
}

/// One session's log, view, heads and command queue.
pub struct Hub {
    inner: Mutex<Inner>,
    /// One condition for everything. A publish wakes every waiting head and the
    /// command worker; each re-checks its own predicate. Simpler than a condvar per
    /// head, and the wake storm is bounded by the head count, which is small by
    /// construction (§13.2's "tmux for an agent", not a broadcast service).
    cv: Condvar,
    /// A head asked to move the running command to the background, and this is the
    /// identity of whoever asked. `None` when no request is pending. Shared with the
    /// exec backend so the `bash` tool's wait loop can honour it without the worker
    /// — which is blocked inside that wait — having to deliver it.
    promote: Arc<Mutex<Option<String>>>,
}

impl Hub {
    pub fn new(session_id: impl Into<String>) -> Arc<Hub> {
        Hub::with_bounds(session_id, LogBounds::default(), ViewBounds::default())
    }

    pub fn with_bounds(
        session_id: impl Into<String>,
        log: LogBounds,
        view: ViewBounds,
    ) -> Arc<Hub> {
        let id = session_id.into();
        Arc::new(Hub {
            inner: Mutex::new(Inner {
                log: SessionLog::new(id.clone(), log),
                view: SessionView::new(id, view),
                heads: Vec::new(),
                operator_calls: std::collections::HashMap::new(),
                next_head: 0,
                commands: VecDeque::new(),
                own_wake: false,
                closed: false,
                bell: None,
                answers: None,
                secrets: HashMap::new(),
                next_secret: 0,
                screens: HashMap::new(),
                next_screen: 0,
            }),
            cv: Condvar::new(),
            promote: Arc::new(Mutex::new(None)),
        })
    }

    /// A head asked to move the running command to the background. Record who, so
    /// the outcome can attribute it. Consumed by the backend's `bash` wait loop.
    pub fn request_promote(&self, identity: impl Into<String>) {
        *self.promote.lock().unwrap_or_else(|e| e.into_inner()) = Some(identity.into());
    }

    /// Take the pending promote request, clearing it. `Some(identity)` when a head
    /// asked.
    pub fn take_promote_request(&self) -> Option<String> {
        self.promote
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// A head asked to move the running command to the background. Resolve that
    /// head's identity and record it, so the outcome can attribute who asked.
    /// `None` when the head is not attached.
    pub fn request_promote_from(&self, head_id: &str) -> Option<String> {
        let identity = self
            .lock()
            .heads
            .iter()
            .find(|h| h.id == head_id)
            .map(|h| h.identity.clone())?;
        self.request_promote(identity.clone());
        Some(identity)
    }

    /// **Who a head is, by id** — the identity the gate records in `human:<who>`.
    ///
    /// `None` for a head this hub is not holding, which is a real state: a
    /// `ClientFrame::OperatorCall` can arrive on a connection whose seat has already been
    /// detached by a switch.
    pub fn identity_of(&self, head_id: &str) -> Option<String> {
        self.lock()
            .heads
            .iter()
            .find(|h| h.id == head_id)
            .map(|h| h.identity.clone())
    }

    /// The shared channel, for the daemon to hand to the exec backend.
    pub fn promote_channel(&self) -> Arc<Mutex<Option<String>>> {
        self.promote.clone()
    }

    /// Register this hub with a cross-session wake, so a worker serving several
    /// sessions can block on one condition instead of polling N.
    ///
    /// Called by [`crate::registry::Registry`] at creation, before any head can
    /// reach the hub — so there is no window in which a command is queued and the
    /// bell does not know.
    pub fn set_bell(&self, bell: Arc<crate::registry::Bell>) {
        self.lock().bell = Some(bell);
    }

    /// **Install the answer sink**, so a settled decision reaches whoever is blocked
    /// on it rather than the queue that whoever-is-blocked is supposed to drain.
    ///
    /// Called by the daemon when it opens a session with an adjudicator that asks a
    /// head. Before it is called — and for every hub in a test — an answer is queued
    /// as it always was.
    pub fn set_answer_sink(&self, sink: Arc<dyn AnswerSink>) {
        self.lock().answers = Some(sink);
    }

    /// `sudo` wants a password: raise it to every head, hand back the receiver
    /// the `askpass` connection waits on. `deadline` is when the helper gives up,
    /// so a head can show it.
    pub fn request_secret(
        &self,
        prompt: &str,
        command: &str,
        deadline_ms: u64,
    ) -> (String, std::sync::mpsc::Receiver<Option<String>>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let req_id = {
            let mut g = self.lock();
            g.next_secret += 1;
            let id = format!("secret-{}-{}", g.log.session_id(), g.next_secret);
            g.secrets.insert(id.clone(), tx);
            id
        };
        self.publish(SessionEvent::SecretRequested {
            req_id: req_id.clone(),
            prompt: prompt.to_string(),
            command: command.to_string(),
            deadline: deadline_ms,
        });
        (req_id, rx)
    }

    /// A head answered a password request. `true` when a helper was still
    /// waiting on it; the log records that it was answered and by whom, never
    /// what with.
    pub fn give_secret(&self, req_id: &str, secret: Option<String>, by: &str) -> bool {
        let tx = self.lock().secrets.remove(req_id);
        let Some(tx) = tx else {
            return false;
        };
        let given = secret.is_some();
        let delivered = tx.try_send(secret).is_ok();
        self.publish(SessionEvent::SecretSettled {
            req_id: req_id.to_string(),
            given: given && delivered,
            by: by.to_string(),
        });
        delivered
    }

    /// **Ask every attached head to draw itself.** Returns the receiver the caller
    /// waits on; the first head to answer wins.
    ///
    /// The daemon cannot answer this itself at any fidelity worth having: it holds
    /// the log and the view, never a rendered cell, and what a person sees depends
    /// on their width, scroll, theme and folds.
    pub fn request_screen(
        &self,
    ) -> (
        String,
        std::sync::mpsc::Receiver<(usize, usize, Vec<String>)>,
    ) {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let req_id = {
            let mut g = self.lock();
            g.next_screen += 1;
            let id = format!("screen-{}-{}", g.log.session_id(), g.next_screen);
            g.screens.insert(id.clone(), tx);
            id
        };
        self.publish(SessionEvent::ScreenRequested {
            req_id: req_id.clone(),
        });
        (req_id, rx)
    }

    /// A head answered. `false` when nothing was waiting — the request timed out,
    /// or another head was faster, and neither is an error.
    pub fn give_screen(&self, req_id: &str, cols: usize, rows_n: usize, rows: Vec<String>) -> bool {
        let tx = self.lock().screens.remove(req_id);
        tx.is_some_and(|tx| tx.try_send((cols, rows_n, rows)).is_ok())
    }

    /// Nobody drew in time; forget it so a late answer finds nothing.
    pub fn abandon_screen(&self, req_id: &str) {
        self.lock().screens.remove(req_id);
    }

    /// The helper gave up (deadline, or its connection closed): forget the request.
    pub fn abandon_secret(&self, req_id: &str, by: &str) {
        if self.lock().secrets.remove(req_id).is_some() {
            self.publish(SessionEvent::SecretSettled {
                req_id: req_id.to_string(),
                given: false,
                by: by.to_string(),
            });
        }
    }

    /// Who can answer an open decision in this session, in the sink's own words.
    /// `None` when nothing is installed, which is a different fact from "nobody is
    /// attached".
    pub fn answer_sink_describes(&self) -> Option<String> {
        self.lock().answers.as_ref().map(|s| s.describe())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock means a head thread panicked while holding it. The log is
        // still consistent — every mutation under this lock is infallible — so
        // recovering is strictly better than taking the whole session down with it.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn session_id(&self) -> String {
        self.lock().log.session_id().to_string()
    }

    /// One row's body by its **session ordinal**, for a head reading a window of it.
    ///
    /// The view's, under the view's lock — see `SessionView::row_body_at` for why it lends the
    /// whole thing and the *caller* windows it.
    pub fn row_body_at(&self, row: usize) -> Option<String> {
        self.lock().view.row_body_at(row)
    }

    pub fn head_seq(&self) -> u64 {
        self.lock().log.head_seq()
    }

    pub fn dropped(&self) -> u64 {
        self.lock().log.dropped()
    }

    /// Append an event and fan it out. **Never blocks on a head.**
    pub fn publish(&self, event: SessionEvent) -> Envelope {
        let env = {
            let mut g = self.lock();
            g.append_and_fan(event)
        };
        self.cv.notify_all();
        env
    }

    /// **A frame that is not the record, fanned out to every head of this session.**
    ///
    /// The pane's byte stream is the only caller today. It is not [`Hub::publish`] and cannot
    /// be: an event is appended to the log, replayed on attach and scrubbed on the way, and a
    /// screen's repaints are none of those things — see
    /// [`crate::protocol::ServerFrame::TermOutput`]. So this appends nothing, moves no seq,
    /// writes no row, and wakes every head the way an event does.
    ///
    /// **Never blocks, and drops the oldest pane frame when a head is behind.** See
    /// [`Head::side`]: a blocked writer here is the pty's reader thread blocked, which is the
    /// program blocked in `write`, which is a pane that has stopped. A dropped repaint is a
    /// frame that is one stale, and a terminal survives that by construction — the next
    /// repaint carries the whole screen anyway.
    pub fn push_frame(&self, frame: ServerFrame) {
        {
            let mut g = self.lock();
            for h in &mut g.heads {
                if h.side.len() >= MAX_SIDE {
                    h.side.pop_front();
                }
                h.side.push_back(frame.clone());
            }
        }
        self.cv.notify_all();
    }

    /// Attach content to an already-appended transcript row, **and fan it out**.
    ///
    /// §4.5's `TranscriptAppended` has no content field, so this is the route the
    /// body takes. It publishes rather than only folding, because folding into the
    /// view alone reaches exactly the heads that have not attached yet — every head
    /// that *was* watching is left with a placeholder no later frame will fill.
    /// See [`crate::event::SessionEvent::TranscriptContent`].
    pub fn record_item(&self, item_id: &str, item: TranscriptItem) {
        self.publish(SessionEvent::TranscriptContent {
            item_id: item_id.to_string(),
            item: Box::new(item),
        });
    }

    /// Every event still in the scrollback, in seq order.
    ///
    /// The session **as a record**, which is what T13.1 asks the log to be and what
    /// `letibot-tui --replay` consumes. Cloned rather than lent, because the log
    /// lives under the same lock the fan-out takes and a borrow held across a
    /// render would stall a turn.
    pub fn retained(&self) -> Vec<Envelope> {
        self.lock().log.retained().cloned().collect()
    }

    /// A snapshot of the current state, for a head that is not attached (a test, a
    /// `harnessctl status`).
    pub fn snapshot(&self) -> Snapshot {
        let g = self.lock();
        g.view.snapshot(g.log.head_seq(), g.log.dropped())
    }

    /// ATTACH. Snapshot-or-gap, decided and delivered **under one lock**.
    ///
    /// The `HeadAttached` event is appended *before* the snapshot is cut, so the
    /// attaching head sees its own arrival and every other head sees it too. The
    /// subscriber is installed in the same critical section, so the first event it
    /// receives is exactly `snapshot.seq + 1`. There is no window.
    pub fn attach(
        &self,
        kind: impl Into<String>,
        identity: impl Into<String>,
        caps: Caps,
        since_seq: u64,
    ) -> Attached {
        let kind = kind.into();
        let identity = identity.into();
        let out = {
            let mut g = self.lock();
            g.next_head += 1;
            let head_id = format!("h{}", g.next_head);

            // 1. Announce, into the log and to the heads that are already there.
            g.append_and_fan(SessionEvent::HeadAttached {
                head_id: head_id.clone(),
                kind: kind.clone(),
                identity: identity.clone(),
            });

            // 2. Decide snapshot or gap, against the log as it now stands.
            let at = g.log.head_seq();
            let dropped = g.log.dropped();
            let (snapshot, resumed_from, backlog, scrubbed) = if since_seq == 0 {
                (
                    Some(g.view.snapshot(at, dropped)),
                    None,
                    Vec::new(),
                    ScrubReport::default(),
                )
            } else {
                match g.log.since(since_seq) {
                    Some(gap) => {
                        // Everything replayed is scrubbed. Live and stored are
                        // different artifacts of the same stream, and a resume is
                        // a replay.
                        let (kept, report) = scrub_replay(g.log.retained(), &gap);
                        (None, Some(since_seq), kept, report)
                    }
                    // RESYNC. A normal outcome.
                    None => (
                        Some(g.view.snapshot(at, dropped)),
                        None,
                        Vec::new(),
                        ScrubReport::default(),
                    ),
                }
            };

            // 3. Register, with an empty queue: everything up to `at` has just been
            //    handed over, and anything after it will be pushed here.
            g.heads.push(Head {
                id: head_id.clone(),
                kind,
                identity,
                caps,
                queue: VecDeque::new(),
                side: VecDeque::new(),
                needs_resync: None,
                mark: ReadMark {
                    seq: since_seq.min(at),
                    ..ReadMark::default()
                },
            });

            Attached {
                head_id,
                dropped,
                snapshot,
                resumed_from,
                scrubbed,
                backlog,
            }
        };
        self.cv.notify_all();
        out
    }

    /// Wait for the next delivery for `head_id`, up to `max` events at a time.
    ///
    /// Blocks until there is something. Returns [`Delivery::Closed`] once the hub is
    /// closed or the head is gone.
    pub fn next_batch(&self, head_id: &str, max: usize) -> Delivery {
        let mut g = self.lock();
        loop {
            let at = g.log.head_seq();
            let dropped = g.log.dropped();
            let Some(idx) = g.heads.iter().position(|h| h.id == head_id) else {
                return Delivery::Closed;
            };
            if let Some(reason) = g.heads[idx].needs_resync.take() {
                let snapshot = g.view.snapshot(at, dropped);
                g.heads[idx].queue.clear();
                g.heads[idx].side.clear();
                g.heads[idx].mark.seq = at;
                return Delivery::Resync {
                    reason,
                    dropped,
                    snapshot: Box::new(snapshot),
                    scrubbed: ScrubReport::default(),
                };
            }
            // **The side channel first**, so a screen's repaint is not stuck behind a batch
            // of a thousand deltas — see [`Delivery::Frames`].
            if !g.heads[idx].side.is_empty() {
                let frames: Vec<ServerFrame> = g.heads[idx].side.drain(..).collect();
                return Delivery::Frames(frames);
            }
            if !g.heads[idx].queue.is_empty() {
                let n = max.min(g.heads[idx].queue.len());
                let events: Vec<Envelope> = g.heads[idx].queue.drain(..n).collect();
                return Delivery::Events(Batch::new(events));
            }
            // **A CLOSED HUB STILL OWES THE HEAD WHAT WAS QUEUED FOR IT, and this
            // check used to sit at the TOP of this loop — where it won every race
            // against the drain below and dropped the queue outright.**
            //
            // MEASURED, and it flaked once in three runs on a loaded CI runner
            // (36867928391, `a_stop_closes_the_registry_even_with_a_command_queued_
            // and_nobody_draining`): the head saw `["Bye(daemon shutting down)"]` and
            // nothing else. The `daemon_stopping` warning HAD been published — and
            // the shutting-down path publishes it to every hub *before* it closes the
            // registry, precisely so every other head is told why — but the pump's
            // first `next_batch` after the close returned `Closed` before looking at
            // the queue, so the warning was not late. It never existed.
            //
            // That made a documented ordering untrue: `Registry::close`'s own comment
            // says the announcement goes first "while the hubs are still open and
            // every other head can still be reached", and this loop is the only thing
            // that could deliver it.
            //
            // A longer deadline cannot fix a frame that is never produced, which is
            // why `until`'s `Timeout => continue` (added the same day, for a real
            // premature-give-up bug) did not and could not help here. The two are
            // different failures: that one was tolerance, this one is ORDERING.
            if g.closed {
                return Delivery::Closed;
            }
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// The head asked for a fresh snapshot. Demote it; the next `next_batch`
    /// delivers one.
    pub fn request_resync(&self, head_id: &str, reason: impl Into<String>) {
        {
            let mut g = self.lock();
            if let Some(h) = g.heads.iter_mut().find(|h| h.id == head_id) {
                h.queue.clear();
                // **The pane's frames go too.** A repaint from before the resync would be
                // drawn as though it were the screen now, which is the one thing a resync
                // exists to stop being true.
                h.side.clear();
                h.needs_resync = Some(reason.into());
            }
        }
        self.cv.notify_all();
    }

    /// Record a head's read mark. Called when the head's ack arrives — which is
    /// after it wrote the batch out, not when it received it.
    pub fn ack(&self, head_id: &str, ack: Ack) {
        let mut g = self.lock();
        if let Some(h) = g.heads.iter_mut().find(|h| h.id == head_id) {
            h.mark.apply(ack);
        }
    }

    /// What a head has read, rendered and filtered. `None` if it is not attached.
    pub fn mark(&self, head_id: &str) -> Option<ReadMark> {
        self.lock()
            .heads
            .iter()
            .find(|h| h.id == head_id)
            .map(|h| h.mark)
    }

    /// Detach. Announced, and **nothing else happens**: no turn is cancelled, no
    /// work is reaped. Idle is quiet, not unwatched.
    pub fn detach(&self, head_id: &str) {
        let ev = {
            let mut g = self.lock();
            let Some(i) = g.heads.iter().position(|h| h.id == head_id) else {
                return;
            };
            let h = g.heads.remove(i);
            SessionEvent::HeadDetached {
                head_id: h.id,
                kind: h.kind,
                identity: h.identity,
            }
        };
        self.publish(ev);
        // **And what that head left half-done** — R24 part two, decision 4's failure case.
        //
        // An operator's call is admitted on the first frame and reported on the second, and
        // the whole reason there are two is that the admission must precede the run. So the
        // window between them is real: a head that dies in it leaves an admission recorded
        // for a call whose result never arrived, and a corpus row saying `admit` about a
        // thing that may never have happened is exactly the kind of claim this tree refuses.
        //
        // The hub is where a head is lost, which is why the pending set is here and not in
        // the harness: this is the one place that knows the head is gone.
        let orphans: Vec<(String, String, String)> = {
            let mut g = self.lock();
            let mine: Vec<String> = g
                .operator_calls
                .iter()
                .filter(|(_, (owner, _, _))| owner == head_id)
                .map(|(call_id, _)| call_id.clone())
                .collect();
            mine.into_iter()
                .filter_map(|c| g.operator_calls.remove(&c).map(|(_, n, w)| (c, n, w)))
                .collect()
        };
        for (call_id, name, who) in orphans {
            self.publish(SessionEvent::Warning {
                code: "operator_call_abandoned".into(),
                detail: format!(
                    "`{who}` was admitted to run `{name}` themselves (call {call_id}) and the \
                     head that asked went away before reporting what it did. **The admission \
                     is on the record and the result is not** — so the corpus holds an \
                     `admit` for a call whose outcome nobody knows, and this sentence is the \
                     only thing that says so. Nothing by that name is pending any more."
                ),

                compaction: None,
            });
        }
    }

    /// **Remember an admitted operator call until the head reports what it did** (R24).
    ///
    /// `(owner head, name, who)` keyed by the head's own `call_id`. See [`Self::detach`] for
    /// why the window this covers is real rather than defensive.
    pub fn note_operator_call(&self, call_id: &str, head_id: &str, name: &str, who: &str) {
        self.lock().operator_calls.insert(
            call_id.to_string(),
            (head_id.to_string(), name.to_string(), who.to_string()),
        );
    }

    /// **The operator's call came back.** `None` when the daemon is not holding that
    /// `call_id` — a head reporting a result for a call it never had admitted, which is
    /// refused by name rather than appended, because appending it would put a row in the
    /// conversation that no admission stands behind.
    pub fn take_operator_call(&self, call_id: &str) -> Option<(String, String)> {
        self.lock()
            .operator_calls
            .remove(call_id)
            .map(|(_, name, who)| (name, who))
    }

    pub fn attached_heads(&self) -> usize {
        self.lock().heads.len()
    }

    /// **Heads that said they will answer a decision**, by identity.
    ///
    /// Not `attached_heads`. A flowy connector attaches with `can_decide: false` and
    /// is a head in every other sense; posting a decision to a session whose only
    /// head is one of those and then waiting for its deadline is §11.5's *"a real
    /// answer given for a fake reason"*.
    ///
    /// An adjudicator asks this **before** posting, so *nobody is attached* is
    /// answered in microseconds rather than after a five-minute wait — the
    /// difference between a session that says why it cannot proceed and one that
    /// looks hung.
    pub fn deciding_heads(&self) -> Vec<String> {
        self.lock()
            .heads
            .iter()
            .filter(|h| h.caps.can_decide)
            .map(|h| h.identity.clone())
            .collect()
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Validate and enqueue a mutating command.
    ///
    /// The three policies, argued:
    ///
    /// - **A prompt on a stale `expected_seq` is accepted**, with a note. §13.2 is
    ///   explicit that a second head's prompt is queued rather than rejected —
    ///   *"that is what a human expects from a shared session"* — and a human who
    ///   typed while the screen moved still meant it.
    /// - **An interrupt is accepted regardless.** §13.2: idempotent, any head, and
    ///   announced with the issuer.
    /// - **An answer to a decision that is no longer open is rejected.** This is the
    ///   live half of the scrub: §13.2b's *"a queued reply is discarded rather than
    ///   held, because a reply to a question asked ten minutes ago is not an answer,
    ///   it is input arriving from nowhere."*
    pub fn submit(
        &self,
        head_id: &str,
        client_request_id: impl Into<String>,
        expected_seq: u64,
        kind: CommandKind,
    ) -> ServerFrame {
        let client_request_id = client_request_id.into();
        // Declared, not initialised: every early return below leaves the function
        // without ringing, and `None` here would be a value nothing ever reads.
        let ring: Option<(Arc<crate::registry::Bell>, String)>;
        // Declared beside `ring` and for the same reason: every early return below
        // leaves without delivering, and a `None` initialiser would be a value
        // nothing ever reads.
        let delivery: Option<(Arc<dyn AnswerSink>, String, Reply, String)>;
        let cancellation: Option<(Arc<dyn AnswerSink>, String)>;
        let frame = {
            let mut g = self.lock();
            let actual = g.log.head_seq();
            // **THE DAEMON IS NOT A HEAD — IT IS THE THING THAT SEATS THEM.** Requiring the
            // caller to be one of this session's heads is the right rule, and it is what stops
            // a stranger's command being attributed to somebody. But the process that holds
            // this hub has business here that no head has: stopping a turn on behalf of a
            // caller that is not seated in it. `DAEMON_SUBMITTER` is that caller's name, and
            // before this arm existed the hub refused it — MEASURED 2026-10-05, every
            // `job_kill` of a subagent answered `Rejected { reason: "not attached" }`, and the
            // tool then formatted that refusal into *"interrupted the turn"*, so the defect was
            // invisible from the one place a person looks.
            //
            // **Narrow on purpose: the daemon may INTERRUPT, and may RELAY a parent's message
            // to a child, and nothing else.** An `Answer` needs `can_decide`, a `Prompt` speaks
            // as a person, a `SetOperatorTodos` writes the operator's own board — each would be
            // the daemon impersonating somebody, and neither of the two verbs it does have is
            // that. The door is only as wide as the need: a subagent's turn is stopped by the
            // first of them and steered, without being restarted or spoken over, by the second.
            let (identity, can_decide) = match g.heads.iter().find(|h| h.id == head_id) {
                Some(h) => (h.identity.clone(), h.caps.can_decide),
                None if head_id == DAEMON_SUBMITTER
                    && matches!(
                        kind,
                        CommandKind::Interrupt { .. } | CommandKind::Message { .. }
                    ) =>
                {
                    // **A relayed message keeps its own author.** The daemon is the *caller* —
                    // the parent is seated in another session's hub, not this one — but it is
                    // not the speaker, and the child is better off for being told so: the whole
                    // point of being able to message a subagent is that it can tell which parent
                    // said it, and what to do about it. So the identity written into the child's
                    // log is `from`, never `\0daemon`. The interrupt has no such author to keep
                    // — its reason is in the text — and stays named after the caller.
                    let who = match &kind {
                        CommandKind::Message { from, .. } => from.clone(),
                        _ => DAEMON_SUBMITTER.to_string(),
                    };
                    (who, false)
                }
                None => {
                    return ServerFrame::Rejected {
                        client_request_id,
                        reason: "not attached".into(),
                        expected_seq,
                        actual_seq: actual,
                    };
                }
            };

            if let CommandKind::Answer { req_id, reply } = &kind {
                if !can_decide {
                    return ServerFrame::Rejected {
                        client_request_id,
                        reason: REJECT_READ_ONLY.into(),
                        expected_seq,
                        actual_seq: actual,
                    };
                }
                let Some(open) = g.view.open_decisions().iter().find(|d| &d.req_id == req_id)
                else {
                    return ServerFrame::Rejected {
                        client_request_id,
                        reason: REJECT_UNKNOWN_DECISION.into(),
                        expected_seq,
                        actual_seq: actual,
                    };
                };
                // A malformed answer is refused **here**, before it reaches anything
                // that would act on it, and the question stays open. Accepting it and
                // letting the tool sort it out would leave a settled decision behind
                // an unanswered question, which is the one state the view must never
                // hold.
                // **A vocabulary may not settle the other vocabulary's request.**
                // The two types exist so that a free-form sentence cannot land where
                // a policy engine reads a grant; without this check they could still
                // be crossed at the door, and a `free` answer would settle a
                // permission by validating against an empty ladder — accepted, and
                // then meaning nothing to the gate waiting on it.
                //
                // The test is `kind == "question"` rather than a match on both names,
                // and the asymmetry is the fail-closed direction: only a request that
                // says it is a question accepts a person's sentence. A `kind` this
                // build does not recognise is treated as a permission, so the
                // vocabulary with the weaker consequence is the one that has to
                // announce itself.
                let is_question = open.kind == "question";
                let mismatch = match (reply, is_question) {
                    (Reply::Question(_), false) => Some(
                        "that request is a permission and this is a question's answer; \
                         a permission is settled by option id",
                    ),
                    (Reply::Permission { .. }, true) => Some(
                        "that request is a question and this is a permission's answer; \
                         a question is settled by a choice, a note or typed text",
                    ),
                    _ => None,
                };
                if let Some(why) = mismatch {
                    return ServerFrame::Rejected {
                        client_request_id,
                        reason: format!("{}: {why}", crate::question::REJECT_MALFORMED_ANSWER),
                        expected_seq,
                        actual_seq: actual,
                    };
                }
                // **Against `choices`, not `options`.** A question's options are its
                // plain-text choices (`PROTOCOL_VERSION` 7); `options` is the
                // adjudication ladder and is empty for one. Validating a question
                // against `options.len()` was validating every answer against zero,
                // which rejected `option: 0` — the single most common answer there is
                // — as an option the question never offered.
                if let Reply::Question(a) = reply
                    && let Err(defect) = a.validate(open.choices.len())
                {
                    return ServerFrame::Rejected {
                        client_request_id,
                        reason: format!("{}: {defect}", crate::question::REJECT_MALFORMED_ANSWER),
                        expected_seq,
                        actual_seq: actual,
                    };
                }
            }

            let stale = expected_seq != 0 && expected_seq < actual;
            let note = match (&kind, stale) {
                (CommandKind::Prompt { .. }, true) => {
                    format!("{REJECT_STALE_SEQ}: queued anyway as a follow-up user item")
                }
                (CommandKind::Prompt { .. }, false) => crate::protocol::NOTE_PROMPT_QUEUED.into(),
                (CommandKind::Compact, true) => {
                    format!("{REJECT_STALE_SEQ}: queued anyway")
                }
                (CommandKind::Compact, false) => crate::protocol::NOTE_COMPACT_QUEUED.into(),
                // Queued like a compaction, and stale-tolerant for the same reason:
                // a head that asked while the screen moved still meant it.
                (CommandKind::Reseat { .. }, true) => {
                    format!("{REJECT_STALE_SEQ}: queued anyway")
                }
                (CommandKind::Reseat { summarise }, false) => if *summarise {
                    "re-seat queued — summarising, so the conversation is replaced by it"
                } else {
                    "re-seat queued — the conversation is carried across as it is"
                }
                .into(),
                (CommandKind::Interrupt { .. }, _) => "interrupt requested".into(),
                (CommandKind::Message { from, .. }, _) => {
                    format!("a message to this session from {from}")
                }
                (CommandKind::WithdrawPrompts, _) => "prompt take-back requested".into(),
                (CommandKind::SetOperatorTodos { items }, _) => {
                    format!("the operator's {} todo(s) sent", items.len())
                }
                (CommandKind::Promote, _) => "background requested".into(),
                (CommandKind::Answer { reply, .. }, _) => {
                    format!("{} answered", reply.as_str())
                }
                // "requested" said nothing about when, and the answer used to be
                // "after the turn" — which is why it read as having done nothing.
                // A running turn takes it at its next round boundary now
                // (`Harness::apply_queued_mode`), so this is true in both states.
                (CommandKind::Mode { name, consented }, _) => format!(
                    "mode `{name}` requested — it takes effect from the next call, \
                     whether or not a turn is running{}",
                    if *consented {
                        " — with the operator's confirmation that this box is the boundary"
                    } else {
                        ""
                    }
                ),
                (CommandKind::Slash { line }, _) => format!("/{line}"),
                // A read, not a mutation: a head that asked while the screen moved
                // still meant it, and nothing this command alters depends on the seq.
                (CommandKind::ReadJobOutput { job, .. }, _) => {
                    format!("reading output of job {job}")
                }
                // Stale-tolerant like a prompt: a head that asked while the screen
                // moved still meant it, and refusing here would make the door fail on
                // a busy session — the one state it exists for.
                (CommandKind::OperatorCall { name, .. }, true) => {
                    format!("{REJECT_STALE_SEQ}: admitted anyway — the operator's own {name} call")
                }
                (CommandKind::OperatorCall { name, .. }, false) => {
                    format!("admitted the operator's own {name} call")
                }
                (CommandKind::OperatorResult { call_id, .. }, _) => {
                    format!("result for the operator's call {call_id}")
                }
                // Stale-tolerant like the door beside it, and for its own reason: the
                // operator who typed a `!` line while the screen moved still meant
                // it, and the line's whole point is that the daemon — not this head —
                // runs it. Mid-turn it waits for the round boundary (the same pickup
                // the door's `execute: true` uses), which is a queue, not a refusal.
                (CommandKind::OperatorShell { line, .. }, true) => format!(
                    "{REJECT_STALE_SEQ}: queued anyway — the operator's own shell line `{line}`"
                ),
                (CommandKind::OperatorShell { line, .. }, false) => {
                    format!("the operator's own shell line `{line}` queued for this session to run")
                }
            };

            let verb = kind.verb();
            // **An answer does not go on the command queue when anybody is waiting
            // for one.** See [`AnswerSink`]: the worker that drains this queue is the
            // thread blocked inside the turn that asked, so queueing the answer hands
            // it to its own waiter. Taken here, delivered after the lock is released,
            // for the same reason the bell is: the sink takes its own mutex.
            //
            // A hub with no sink queues it exactly as before, which is what a daemon
            // with no adjudication does and what every test in this file drives.
            let deliver = match (&kind, g.answers.clone()) {
                (CommandKind::Answer { req_id, reply }, Some(sink)) => {
                    Some((sink, req_id.clone(), reply.clone()))
                }
                _ => None,
            };
            // Taken here, rung *after* the lock is released: `Bell::ring` takes its
            // own mutex, and taking a second lock inside this one is how a lock
            // order gets invented by accident.
            ring = g.bell.clone().map(|b| (b, g.log.session_id().to_string()));
            // **An interrupt cancels a decision that is being waited on.** It still
            // queues — the turn is interrupted the way it always was — and the sink
            // is told as well, because the thread blocked inside a tool call is not
            // draining that queue and would otherwise sit out its whole deadline
            // after the operator asked it to stop.
            let cancel = match (&kind, g.answers.clone()) {
                (CommandKind::Interrupt { reason }, Some(sink)) => Some((sink, reason.clone())),
                _ => None,
            };
            if deliver.is_none() {
                g.commands.push_back(QueuedCommand {
                    head_id: head_id.to_string(),
                    identity: identity.clone(),
                    client_request_id: client_request_id.clone(),
                    at_seq: actual,
                    kind,
                });
            }
            delivery = deliver.map(|(sink, req_id, reply)| (sink, req_id, reply, identity.clone()));
            cancellation = cancel;
            // The announcement §13.2 requires: "announced as an event so both heads
            // see it and who did it".
            let env = g.append_and_fan(SessionEvent::CommandIssued {
                head_id: head_id.to_string(),
                identity,
                command: verb.to_string(),
                client_request_id: client_request_id.clone(),
                note: note.clone(),
            });
            ServerFrame::Accepted {
                client_request_id,
                seq: env.seq,
                note,
            }
        };
        self.cv.notify_all();
        // Outside the lock. The waiter wakes on the sink's own condition, takes its
        // own mutex, and must not be doing that while this one is held.
        //
        // The return value is deliberately dropped. `false` means nothing was waiting
        // — a decision that already timed out, or one another process asked — and the
        // head has already been told `Accepted`, which was true: the answer was
        // accepted and it settled nothing. Turning it into a rejection here would make
        // a race look like a malformed frame.
        if let Some((sink, req_id, reply, identity)) = delivery {
            sink.answer(&req_id, &identity, &reply);
        }
        if let Some((sink, reason)) = cancellation {
            sink.cancel(&reason);
        }
        if let Some((bell, id)) = ring {
            bell.ring(&id);
        }
        frame
    }

    /// This session, as a row in a picker. One lock, one instant.
    pub fn status(&self) -> SessionStatus {
        let g = self.lock();
        SessionStatus {
            session_id: g.log.session_id().to_string(),
            seq: g.log.head_seq(),
            items: g.view.item_count(),
            heads: g.heads.len(),
            running: g.view.turn().is_some_and(|t| t.state.is_running()),
            model: g.view.turn().map(|t| t.model.clone()).unwrap_or_default(),
            last_ms: g.log.last_ts(),
        }
    }

    /// Take the next thing for the session's single reader. Blocks. `Closed` once
    /// the hub is closed.
    ///
    /// **The reader is whoever runs this session's turns**, and for a session the daemon
    /// holds that is the daemon's worker; for a subagent it is the thread its parent spawned
    /// it on (`harness::serve_child`), which is the same thread that runs its turns. Both
    /// take from this one queue, and the two things they can be handed are a command a head
    /// submitted and a wake the daemon handed up — see [`Hub::wake_its_own_reader`] and
    /// [`Hub::give_back_to_its_own_reader`].
    pub fn take_own_work(&self) -> OwnWork {
        let mut g = self.lock();
        loop {
            // **A command first**, because a command has a head behind it (or a parent's
            // kill) and a wake has nobody waiting. The `Bell` keeps the same order between
            // work and wakes, and for the same reason.
            if let Some(c) = g.commands.pop_front() {
                return OwnWork::Command(c);
            }
            if g.own_wake {
                g.own_wake = false;
                return OwnWork::Wake;
            }
            if g.closed {
                return OwnWork::Closed;
            }
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// **A wake for the thread that owns this session's turn, rather than for the daemon's
    /// worker.**
    ///
    /// [`crate::registry::Bell::ring_wake`] is how the daemon learns a session has
    /// something to act on between turns, and it is the right door for every session the
    /// daemon holds a harness for: the worker drains that session's completions and runs the
    /// turn they make. A subagent is not one of those — its harness lives on its parent's
    /// spawn thread, so a ring naming it reaches `Sessions::wake`, finds no harness in
    /// `open`, and is discarded (R58 measured exactly that). So a wake whose session the
    /// daemon does not hold is handed HERE instead, and this door rings nothing: it sets a
    /// flag and wakes this hub's own condvar, which is where the thread that IS running the
    /// session is blocked.
    ///
    /// **Why not the bell with the subagent's id, and let the daemon relay it there?**
    /// Because the daemon would have to queue something to hand over, and every queued thing
    /// here rings the bell in turn — a wake that produces a wake. This door is the shortest
    /// honest path from *something this session owns has settled* to the thread that can act
    /// on it, and it leaves the bell meaning one thing: *the daemon has work to do*.
    ///
    /// `false` for a closed hub: a session that is gone has no reader, and the settlement
    /// that prompted this was handed up a level before it closed (see
    /// `jobwatch::JobWatchers::stop`).
    pub fn wake_its_own_reader(&self) -> bool {
        self.hand_to_its_own_reader_with(None)
    }

    /// **A command the daemon took off this queue but must not run itself.**
    ///
    /// The daemon's worker and a subagent's own thread take from ONE queue, so either can
    /// win a command a head submitted for a subagent — and the worker's answer to an
    /// interrupt is *"nothing was generating"*, which for a subagent that is mid-turn in its
    /// own thread is a lie. So a command that belongs to the session's own reader is put back
    /// here and that reader is woken: the same door as [`Hub::wake_its_own_reader`], with the
    /// work attached.
    pub fn give_back_to_its_own_reader(&self, cmd: QueuedCommand) -> bool {
        if !matches!(cmd.kind, CommandKind::Interrupt { .. }) {
            return false;
        }
        self.hand_to_its_own_reader_with(Some(cmd))
    }

    /// **Wake this hub's own condvar, with or without work attached.** The one door both
    /// [`Hub::wake_its_own_reader`] and [`Hub::give_back_to_its_own_reader`] go through, so
    /// the two cannot come to ring or lock differently.
    ///
    /// **A command is its own wake, and the flag is only for a wake with nothing behind
    /// it.** Setting the flag alongside a command leaves a second, phantom wake behind it:
    /// [`Hub::take_own_work`] hands back the command first and the flag survives that take,
    /// so the reader's next pass finds a settlement that does not exist and spends a turn on
    /// it. The flag is genuinely sticky across a command — a wake and a prompt can both be
    /// waiting, and the wake must still be there after the prompt is answered — which is why
    /// this is fixed where the flag is SET rather than where it is read.
    fn hand_to_its_own_reader_with(&self, cmd: Option<QueuedCommand>) -> bool {
        {
            let mut g = self.lock();
            if g.closed {
                return false;
            }
            match cmd {
                Some(cmd) => g.commands.push_back(cmd),
                None => g.own_wake = true,
            }
        }
        // Outside the lock, for the reason `submit` rings outside it: a waiter takes its own
        // mutex on the way out and must not be doing that while this one is held.
        self.cv.notify_all();
        true
    }

    /// Non-blocking form, for a worker that also has other things to do.
    pub fn try_command(&self) -> Option<QueuedCommand> {
        self.lock().commands.pop_front()
    }

    /// The next command a **running turn** can act on, leaving the rest queued.
    ///
    /// Mid-turn steering may consume a prompt (a follow-up user item), an
    /// interrupt, or a parent's correction to a subagent in flight. Everything else
    /// belongs to the between-turn worker, and popping
    /// it here would lose it: the worker is inside the very turn that is polling,
    /// and would never see a command this path swallowed. Scanned rather than
    /// popped-and-dropped for exactly that reason — a [`CommandKind::Compact`]
    /// queued behind a running turn must still be there when the turn ends.
    pub fn try_steering_command(&self) -> Option<QueuedCommand> {
        let mut g = self.lock();
        let i = (0..g.commands.len()).find(|&i| {
            matches!(
                g.commands[i].kind,
                CommandKind::Prompt { .. }
                    | CommandKind::Interrupt { .. }
                    | CommandKind::Message { .. }
            )
        })?;
        g.commands.remove(i)
    }

    /// **Is the operator waiting to say something?** — without taking it.
    ///
    /// [`Self::try_steering_command`] removes what it finds, which is right for
    /// the steering path and wrong for a caller deciding whether to hand the turn
    /// back: a message consumed here would never reach the worker as a prompt of
    /// its own. So this only looks.
    ///
    /// Scoped to `Prompt`, not `Interrupt`: an interrupt already stops the turn
    /// through its own path and needs nobody's help.
    pub fn has_queued_prompt(&self) -> bool {
        self.lock()
            .commands
            .iter()
            .any(|c| matches!(c.kind, CommandKind::Prompt { .. }))
    }

    /// **The next mode change a running turn can act on.**
    ///
    /// A mode command used to sit here until the worker came back, which is
    /// after the turn — so `/mode` during a turn moved nothing while the turn
    /// kept asking under the old point. That is precisely when an operator
    /// reaches for it: they are being asked repeatedly and want it to stop.
    /// `Harness::set_mode`'s own doc says the gate "reads its mode at decision
    /// time" and that nobody typing `/mode` means "next time" — true between
    /// turns and false during one, which is the gap this closes.
    ///
    /// Scanned and popped like [`Self::try_steering_command`], and for the same
    /// reason it is safe to: the worker polls this at a ROUND boundary, on the
    /// thread that owns the harness, so the change is applied by its owner and
    /// not raced into from a head's connection.
    pub fn try_mode_command(&self) -> Option<QueuedCommand> {
        let mut g = self.lock();
        let i = (0..g.commands.len())
            .find(|&i| matches!(g.commands[i].kind, CommandKind::Mode { .. }))?;
        g.commands.remove(i)
    }

    /// **The next operator's own call a running turn can act on** — R31's deposit.
    ///
    /// The door's whole value is that it is issued **while a turn is running**: the operator
    /// watches the model go down a wrong path and drops the doc in. That was not true until
    /// this existed, and the mechanism that made it false is worth stating because it is not
    /// obvious from any one file:
    ///
    /// * `ClientFrame::OperatorCall` → `hub.submit(CommandKind::OperatorCall)` → the queue;
    /// * the queue is drained by the **single worker** (`harnessd`'s `Daemon::run`);
    /// * `Sessions::run_prompt` runs the whole turn **inside** that drain, so the worker is
    ///   inside the turn and cannot reach anything else;
    /// * `try_steering_command` takes `Prompt` and `Interrupt`, and `try_mode_command` takes
    ///   `Mode` — so a door call sat behind the turn, and a turn can run for minutes.
    ///
    /// So this is the third of the same picker, and it is the same shape for the same reason
    /// `try_mode_command`'s own comment gives: **a command whose whole point is to act during
    /// a turn has to be taken by the thread that owns the harness, at a round boundary.**
    ///
    /// # Scoped to `execute: true`, and the scoping is the design
    ///
    /// A call with `execute: false` is one a HEAD runs itself and reports back about; the
    /// admission is what it waits for and nothing about that needs to beat the turn — the
    /// head is already doing the work in its own process. A call with `execute: true` is the
    /// DAEMON's to run, and the row it appends is the deposit the next reader sees, so it is
    /// the one that has to land mid-turn. Leaving the other kind here would hand a head's
    /// call to the worker and its result to nobody.
    ///
    /// # The operator's shell line rides the same pickup
    ///
    /// [`CommandKind::OperatorShell`] is taken here too, for the same reason in its own
    /// words: the operator typed a `!` line while a turn was running, the daemon — not the
    /// head — is the one that runs it, and the rows it appends are the deposit the next
    /// round reads. A `!` line that waited for the turn to end would make the one command
    /// surface that exists for *now* the slowest verb on the screen.
    pub fn try_head_run_command(&self) -> Option<QueuedCommand> {
        let mut g = self.lock();
        let i = (0..g.commands.len()).find(|&i| {
            matches!(
                g.commands[i].kind,
                CommandKind::OperatorCall { execute: true, .. } | CommandKind::OperatorShell { .. }
            )
        })?;
        g.commands.remove(i)
    }

    /// The next take-back a **running turn** can act on, dropping the issuing
    /// head's still-queued prompts with it.
    ///
    /// A take-back is only meaningful while the prompts it names are unconsumed,
    /// which is the steering poll's exact window: during a long tool call the
    /// operator's prompts sit here, and the withdraw that follows them up must
    /// remove them from this queue, not from the engine's held steering — the
    /// engine has not seen them yet. Scanned rather than popped-and-dropped like
    /// [`Self::try_steering_command`], and scoped to the head that asked, so one
    /// head's recall never takes another head's queued prompt.
    ///
    /// **And it takes the head's whole queue, in one go.** The operator ruled it, 2026-09-25: *"yes
    /// whole messages queue is dequeued in one go"* — and the `↑` they were describing is exactly
    /// this: the head recalls **every** line it is holding above the composer into the composer in
    /// one press (the head's own `submit` merges the run into a single echo behind a running
    /// turn, so what the operator sees as *the queue* is one entry with N lines). So the daemon drops every
    /// queued prompt of that head, not one of them, and not the oldest alone.
    ///
    /// **A queued `!` line goes with it** ([`CommandKind::OperatorShell`]), because the
    /// recall that asks for the take-back pulls the bang line's echo out of the
    /// composer's queue too — and a shell command that runs after it was visibly
    /// taken back is not a message landing late, it is work happening. Prompt and
    /// bang are dropped together or the echo would be a lie about what the daemon
    /// still holds.
    ///
    /// **"Every prompt of that head that was here when it asked" — which is the same sentence.**
    /// The rule is positional (`0..at`, the prefix in front of the withdraw frame) and that is not
    /// a narrowing: the head submits each line **as it is typed**, so everything in its queue at
    /// recall time is already in front of the withdraw on the socket, and everything behind it
    /// arrived after — the copy the operator re-sent from the composer, which was never in the
    /// queue they took back and must not be dropped with it. Both halves matter, and the second is
    /// measured: dropping a head's whole run *regardless of when it arrived* is what this function
    /// did before `56151c1`, and on the operator's own session it ate the corrected resend
    /// (`lol, it is a bag` → `↑` → `lol, it is a bug`, the resend never landing) while the head sat
    /// drawing `queued` over a sentence the conversation had never seen.
    ///
    /// **The frame already says this**, and needs no field to say it: `WithdrawPrompts` carries no
    /// payload naming prompts, and arrival order is the payload. Read the body for where the other
    /// half of the take-back lives, which is `Pending::absorb`'s `retain` — and read *that* one for
    /// the head it does not name.
    pub fn try_withdraw_command(&self) -> bool {
        let mut g = self.lock();
        let Some(at) = (0..g.commands.len())
            .find(|&i| matches!(g.commands[i].kind, CommandKind::WithdrawPrompts))
        else {
            return false;
        };
        let cmd = g
            .commands
            .remove(at)
            .expect("the index was just found in this deque");
        // **THE WHOLE OF THE HEAD'S QUEUE AS IT STOOD — the prefix, `0..at`.** Read the docstring
        // for the ruling this satisfies (**two** queued lines are both taken, in one go) and for
        // the measurement that fixes the other edge (`56151c1`: a resend that arrives after the
        // withdraw is the operator's corrected copy and survives, because it was never in the
        // queue they took back).
        //
        // The two facts that make the positional rule the whole-queue rule, both checked in the
        // head rather than assumed here: `submit` sends each line the moment Enter is pressed
        // (`Action::Prompt`, one socket, in order), and nothing re-sends a held line later — the
        // only thing that can arrive behind this withdraw is something typed *after* the recall.
        let mut kept = VecDeque::with_capacity(g.commands.len());
        for (i, c) in g.commands.drain(..).enumerate() {
            let named = i < at
                && c.head_id == cmd.head_id
                && matches!(
                    c.kind,
                    CommandKind::Prompt { .. } | CommandKind::OperatorShell { .. }
                );
            if !named {
                kept.push_back(c);
            }
        }
        g.commands = kept;
        true
    }

    /// Shut the hub down. Every waiter wakes with [`Delivery::Closed`], and
    /// [`Hub::take_own_work`] returns [`OwnWork::Closed`].
    pub fn close(&self) {
        let ring = {
            let mut g = self.lock();
            g.closed = true;
            g.bell.clone().map(|b| (b, g.log.session_id().to_string()))
        };
        self.cv.notify_all();
        // A closed hub is news for the cross-session worker too: it is blocked on
        // the bell, not on this condvar, and would otherwise sleep through a
        // shutdown that only concerns one of its sessions.
        if let Some((bell, id)) = ring {
            bell.ring(&id);
        }
    }

    /// Settled decisions, for a head that wants to check before it answers.
    pub fn settled(&self) -> StoredProjection {
        let g = self.lock();
        StoredProjection::of(g.log.retained())
    }
}

impl Inner {
    /// Append, fold into the view, and push to every head. The whole of fan-out.
    fn append_and_fan(&mut self, event: SessionEvent) -> Envelope {
        let env = self.log.append(event);
        self.view.apply(&env);
        for h in &mut self.heads {
            if h.needs_resync.is_some() {
                // Already demoted; its queue is going to be thrown away.
                continue;
            }
            if h.queue.len() >= h.caps.queue.max(1) {
                // Demote, do not block and do not drop silently. The head is told,
                // by being handed a snapshot instead of a hole.
                h.queue.clear();
                h.needs_resync = Some(format!(
                    "queue of {} overflowed at seq {}",
                    h.caps.queue, env.seq
                ));
                continue;
            }
            h.queue.push_back(env.clone());
        }
        env
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    fn attach(hub: &Hub, queue: usize) -> Attached {
        hub.attach(
            "tui",
            "dead@lab2x1",
            Caps {
                queue,
                ..Caps::default()
            },
            0,
        )
    }

    #[test]
    fn a_promote_request_is_recorded_with_who_and_taken_once() {
        let hub = Hub::new("s");
        let a = attach(&hub, 4);
        // A head asks, and the channel resolves its identity.
        assert_eq!(
            hub.request_promote_from(&a.head_id).as_deref(),
            Some("dead@lab2x1")
        );
        assert_eq!(hub.take_promote_request().as_deref(), Some("dead@lab2x1"));
        // Taken, not read: a second take sees nothing, so the bash wait loop cannot
        // promote the same command twice.
        assert_eq!(hub.take_promote_request(), None);
        // An unattached head resolves to no request.
        assert_eq!(hub.request_promote_from("nobody"), None);
    }

    /// **A wake handed to the session's own reader is taken THERE, once, and rings no bell.**
    ///
    /// The operator's design, in their words: *"think about it like it is an erlang supervision
    /// tree. we talk to parents and they own lifecycle."* A subagent's settlement is its
    /// parent's, and the parent is a session the daemon does not hold (`Sessions::wake`), so the
    /// wake is handed to the thread that runs it — and the two facts that make that a route
    /// rather than a hope are asserted here:
    ///
    ///   * a waiting reader is woken and finds it, and a SECOND take finds nothing, because a
    ///     keeper flag would turn one settlement into a turn every time the loop came round;
    ///   * **the bell is not rung**, so the daemon's worker is not told about work it cannot do
    ///     — which is the whole reason this door is not `Bell::ring_wake`.
    ///
    /// A command still comes first when both are waiting: a head that pressed enter
    /// outranks a settlement nobody is holding.
    #[test]
    fn a_wake_handed_to_the_own_reader_is_taken_there_and_rings_nothing() {
        use crate::registry::{Registry, SessionWiring, Work, WorkOrIdle};

        // **The bell is read without blocking**: `next_work` waits for ever when nothing is
        // pending, so "nothing was rung" has to be asked as "the worker has nothing to do
        // as of now".
        let quiet = |r: &Registry| {
            matches!(
                r.next_work_until(Some(std::time::Instant::now())),
                WorkOrIdle::Idle
            )
        };

        let r = Registry::new();
        let hub = r
            .create("s-child", "", SessionWiring::default())
            .expect("the child registers");
        // Drain the create's `Open`, so the next thing off the bell is the settlement's —
        // and there must not be one.
        assert!(matches!(r.next_work(), Some(Work::Open(id)) if id == "s-child"));

        assert!(hub.wake_its_own_reader(), "a live hub takes a wake");
        assert!(
            matches!(hub.take_own_work(), OwnWork::Wake),
            "the thread that owns the session is woken by it"
        );
        assert!(
            quiet(&r),
            "and the DAEMON's worker is told nothing: this wake is not work it can do"
        );
        // The take is a take: a keeper flag would spend a turn on every pass of a loop
        // that has nothing left to read.
        hub.wake_its_own_reader();
        assert!(matches!(hub.take_own_work(), OwnWork::Wake));

        // **A stop put back for its own reader**, which is the other thing this door
        // carries: the worker can win an interrupt the parent aimed at its child, and
        // its answer to one is `interrupt_idle` — a false sentence for a child that is
        // mid-turn in its own thread.
        let cmd = QueuedCommand {
            head_id: DAEMON_SUBMITTER.to_string(),
            identity: DAEMON_SUBMITTER.to_string(),
            client_request_id: "job_kill-1".into(),
            at_seq: hub.head_seq(),
            kind: CommandKind::Interrupt {
                reason: "job_kill".into(),
            },
        };
        assert!(hub.give_back_to_its_own_reader(cmd));
        match hub.take_own_work() {
            OwnWork::Command(c) => assert!(
                matches!(c.kind, CommandKind::Interrupt { .. }),
                "the interrupt is back in front of the session's own reader: {c:?}"
            ),
            other => panic!("expected the interrupt back, got {other:?}"),
        }
        assert!(quiet(&r), "and still nothing for the daemon's worker");

        // Only a STOP goes back: a prompt is work the daemon CAN serve, by opening the
        // session, and putting one back would take a head's own words away from it.
        let prompt = QueuedCommand {
            head_id: "h1".into(),
            identity: "dead@lab2x1".into(),
            client_request_id: "c1".into(),
            at_seq: hub.head_seq(),
            kind: CommandKind::Prompt {
                text: "hello".into(),
            },
        };
        assert!(
            !hub.give_back_to_its_own_reader(prompt),
            "a prompt is not a stop and is not this door's"
        );

        // A hub that is closed has no reader to hand anything to, which is a `false`
        // rather than a promise: the settlement that prompted a wake was handed up a
        // level when this session stopped (`JobWatchers::stop`).
        hub.close();
        assert!(
            !hub.wake_its_own_reader(),
            "a closed hub has nobody to wake"
        );
        assert!(matches!(hub.take_own_work(), OwnWork::Closed));
    }

    #[test]
    fn a_head_that_was_already_attached_is_sent_the_body_it_was_promised() {
        // The fault this variant exists for. Attaching first is the whole point:
        // a head that attaches *later* was always fine, because the snapshot
        // carried the content, and that is what hid this for so long.
        let hub = Hub::new("s");
        let a = attach(&hub, 64);
        hub.publish(appended("s.0", "user"));
        hub.record_item(
            "s.0",
            letibot_transcript::TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![letibot_transcript::UserPart::Text {
                    text: "the operator's own prompt".into(),
                }],
            },
        );
        let Delivery::Events(b) = hub.next_batch(&a.head_id, 64) else {
            panic!("expected events");
        };
        let bodies: Vec<_> = b
            .events()
            .iter()
            .filter(|e| matches!(e.event, SessionEvent::TranscriptContent { .. }))
            .collect();
        assert_eq!(
            bodies.len(),
            1,
            "the body never reached a head that was watching: {:?}",
            b.events()
                .iter()
                .map(|e| e.event.kind())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_slow_head_is_demoted_and_told_and_never_stalls_the_publisher() {
        let hub = Hub::new("s");
        let a = attach(&hub, 4);
        for i in 0..1000 {
            hub.publish(warn(&format!("w{i}")));
        }
        assert_eq!(hub.head_seq(), 1001, "publishing never blocked");
        match hub.next_batch(&a.head_id, 64) {
            Delivery::Resync {
                reason, snapshot, ..
            } => {
                assert!(reason.contains("overflow"), "{reason}");
                assert_eq!(snapshot.seq, 1001, "and it is told where it now is");
            }
            other => panic!("expected a resync, got {other:?}"),
        }
    }

    #[test]
    fn a_late_head_gets_a_snapshot_and_then_the_very_next_event() {
        let hub = Hub::new("s");
        hub.publish(turn_started("t1"));
        hub.publish(delta("t1", "hel"));
        hub.publish(delta("t1", "lo"));
        let a = attach(&hub, 64);
        assert_eq!(
            a.snapshot.as_ref().unwrap().turn.as_ref().unwrap().text,
            "hello"
        );
        let at = a.snapshot.as_ref().unwrap().seq;
        hub.publish(delta("t1", "!"));
        let Delivery::Events(b) = hub.next_batch(&a.head_id, 64) else {
            panic!()
        };
        assert_eq!(b.events()[0].seq, at + 1, "no gap, no duplicate");
    }

    #[test]
    fn detaching_the_last_head_does_not_stop_anything() {
        let hub = Hub::new("s");
        let a = attach(&hub, 64);
        hub.publish(turn_started("t1"));
        hub.detach(&a.head_id);
        assert_eq!(hub.attached_heads(), 0);
        for i in 0..10 {
            hub.publish(delta("t1", &format!("{i}")));
        }
        // Idle is quiet, not unwatched: the work carried on and a new head sees it.
        let b = attach(&hub, 64);
        assert_eq!(b.snapshot.unwrap().turn.unwrap().text, "0123456789");
    }

    #[test]
    fn a_resume_is_scrubbed_and_the_count_travels_with_it() {
        let hub = Hub::new("s");
        let a = attach(&hub, 64);
        let from = hub.head_seq();
        hub.publish(requested("r1", "rm -rf /"));
        hub.publish(progress("t1"));
        hub.publish(answered("r1", "deny"));
        hub.detach(&a.head_id);

        let b = hub.attach("tui", "dead@lab2x1", Caps::default(), from);
        assert!(
            !b.backlog
                .iter()
                .any(|e| matches!(e.event, SessionEvent::DecisionRequested { .. })),
            "a resumed head must not be re-asked a settled question"
        );
        assert_eq!(b.scrubbed.settled_decisions, 1);
        assert_eq!(b.scrubbed.prompt_progress, 1);
    }

    #[test]
    fn answering_a_settled_decision_is_refused_rather_than_held() {
        let hub = Hub::new("s");
        let a = attach(&hub, 64);
        hub.publish(requested("r1", "rm -rf /"));
        hub.publish(answered("r1", "deny"));
        let f = hub.submit(
            &a.head_id,
            "c1",
            0,
            CommandKind::Answer {
                req_id: "r1".into(),
                reply: Reply::Permission {
                    option_id: "allow".into(),
                    pattern: None,
                    note: None,
                },
            },
        );
        assert!(matches!(
            f,
            ServerFrame::Rejected { ref reason, .. } if reason == REJECT_UNKNOWN_DECISION
        ));
    }

    #[test]
    fn a_second_heads_prompt_is_queued_not_rejected_and_says_who() {
        let hub = Hub::new("s");
        let a = hub.attach("tui", "alice", Caps::default(), 0);
        let b = hub.attach("remote", "bob", Caps::default(), 0);
        hub.publish(turn_started("t1"));
        let f = hub.submit(
            &b.head_id,
            "c9",
            1, // deliberately stale
            CommandKind::Prompt {
                text: "also check the logs".into(),
            },
        );
        let ServerFrame::Accepted { note, .. } = f else {
            panic!("a prompt from a second head must not be rejected")
        };
        assert!(note.contains("queued"), "{note}");

        // Both heads see it, and who did it.
        let Delivery::Events(batch) = hub.next_batch(&a.head_id, 64) else {
            panic!()
        };
        let issued = batch
            .events()
            .iter()
            .find_map(|e| match &e.event {
                SessionEvent::CommandIssued {
                    identity, command, ..
                } => Some((identity.clone(), command.clone())),
                _ => None,
            })
            .expect("the queuing is announced");
        assert_eq!(issued, ("bob".to_string(), "prompt".to_string()));

        let cmd = hub.try_command().unwrap();
        assert_eq!(cmd.identity, "bob");
    }

    #[test]
    fn a_read_only_head_cannot_answer() {
        let hub = Hub::new("s");
        let a = hub.attach(
            "flowy",
            "room:general",
            Caps {
                can_decide: false,
                ..Caps::default()
            },
            0,
        );
        hub.publish(requested("r1", "rm -rf /"));
        let f = hub.submit(
            &a.head_id,
            "c1",
            0,
            CommandKind::Answer {
                req_id: "r1".into(),
                reply: Reply::Permission {
                    option_id: "allow".into(),
                    pattern: None,
                    note: None,
                },
            },
        );
        assert!(
            matches!(f, ServerFrame::Rejected { ref reason, .. } if reason == REJECT_READ_ONLY)
        );
    }

    #[test]
    fn a_question_answer_settles_the_same_request_a_permission_would() {
        let hub = Hub::new("s");
        let a = attach(&hub, 64);
        hub.publish(asked(
            "r1",
            "which approach?",
            &["rebuild first", "patch in place"],
            "both work and they differ in cost",
        ));
        let f = hub.submit(
            &a.head_id,
            "c1",
            0,
            CommandKind::Answer {
                req_id: "r1".into(),
                reply: Reply::Question(
                    crate::question::QuestionAnswer::choosing(1).with_note("only on CUDA"),
                ),
            },
        );
        match f {
            ServerFrame::Accepted { note, .. } => assert_eq!(note, "question answered"),
            other => panic!("expected acceptance, got {other:?}"),
        }
    }

    /// **The deadlock this seam exists to avoid, as a test.**
    ///
    /// With a sink installed the answer must NOT be on the command queue, because the
    /// only thread that drains that queue is the one blocked inside the turn that
    /// asked. A test that only checked the sink saw the answer would still pass if the
    /// command were queued as well, and the queued copy is what a later `dispatch`
    /// would report as an answer to nothing.
    #[test]
    fn an_answer_reaches_the_sink_and_never_the_queue() {
        struct Recorder(Mutex<Vec<(String, String, String)>>);
        impl AnswerSink for Recorder {
            fn answer(&self, req_id: &str, identity: &str, reply: &Reply) -> bool {
                self.0.lock().unwrap().push((
                    req_id.to_string(),
                    identity.to_string(),
                    reply.as_str().to_string(),
                ));
                true
            }
            fn cancel(&self, _why: &str) {}
            fn describe(&self) -> String {
                "recorder".into()
            }
        }
        let hub = Hub::new("s");
        let sink = Arc::new(Recorder(Mutex::new(Vec::new())));
        hub.set_answer_sink(sink.clone());
        let a = hub.attach("tui", "alice", Caps::default(), 0);
        hub.publish(requested("r1", "write src/main.rs"));

        let f = hub.submit(
            &a.head_id,
            "c1",
            0,
            CommandKind::Answer {
                req_id: "r1".into(),
                reply: Reply::Permission {
                    option_id: "allow".into(),
                    pattern: None,
                    note: None,
                },
            },
        );
        assert!(matches!(f, ServerFrame::Accepted { .. }), "{f:?}");
        assert_eq!(
            sink.0.lock().unwrap().as_slice(),
            [(
                "r1".to_string(),
                "alice".to_string(),
                "permission".to_string()
            )],
            "the answer must reach whoever is blocked on it, with who said it"
        );
        assert!(
            hub.try_command().is_none(),
            "an answer must not also be queued: the worker that would drain it is the \
             thread waiting for it"
        );
        // And the announcement still happens, because a second head has to see that
        // somebody answered.
        let Delivery::Events(batch) = hub.next_batch(&a.head_id, 64) else {
            panic!()
        };
        assert!(
            batch.events().iter().any(|e| matches!(
                &e.event,
                SessionEvent::CommandIssued { command, .. } if command == "answer"
            )),
            "the answer is still announced on the log"
        );
    }

    /// Without a sink nothing changes: the command queues, which is what a daemon
    /// with no adjudication does and what every other test here drives.
    #[test]
    fn with_no_sink_an_answer_queues_exactly_as_it_did() {
        let hub = Hub::new("s");
        let a = attach(&hub, 64);
        hub.publish(requested("r1", "rm -rf /"));
        hub.submit(
            &a.head_id,
            "c1",
            0,
            CommandKind::Answer {
                req_id: "r1".into(),
                reply: Reply::Permission {
                    option_id: "allow".into(),
                    pattern: None,
                    note: None,
                },
            },
        );
        let cmd = hub.try_command().expect("queued as before");
        assert!(matches!(cmd.kind, CommandKind::Answer { .. }));
    }

    /// A question's answer may not settle a permission. Both types exist so a
    /// free-form sentence cannot land where a policy engine reads a grant, and
    /// before this check it could — `free` validated against an empty ladder and was
    /// accepted.
    #[test]
    fn the_two_answer_vocabularies_do_not_cross() {
        let hub = Hub::new("s");
        let a = attach(&hub, 64);
        hub.publish(requested("r1", "write src/main.rs"));
        hub.publish(asked("r2", "which?", &["a", "b"], "stuck"));

        let f = hub.submit(
            &a.head_id,
            "c1",
            0,
            CommandKind::Answer {
                req_id: "r1".into(),
                reply: Reply::Question(crate::question::QuestionAnswer::free("go ahead")),
            },
        );
        assert!(
            matches!(f, ServerFrame::Rejected { ref reason, .. } if reason.contains("permission")),
            "{f:?}"
        );

        let f = hub.submit(
            &a.head_id,
            "c2",
            0,
            CommandKind::Answer {
                req_id: "r2".into(),
                reply: Reply::Permission {
                    option_id: "allow".into(),
                    pattern: None,
                    note: None,
                },
            },
        );
        assert!(
            matches!(f, ServerFrame::Rejected { ref reason, .. } if reason.contains("question")),
            "{f:?}"
        );
    }

    #[test]
    fn a_malformed_question_answer_leaves_the_question_open() {
        // D10: an answer that does not conform is refused HERE, before anything acts
        // on it. Accepting it would leave a settled decision behind an unanswered
        // question, which is the one state the view must never hold.
        let hub = Hub::new("s");
        let a = attach(&hub, 64);
        hub.publish(asked(
            "r1",
            "which approach?",
            &["rebuild first", "patch in place"],
            "both work",
        ));
        for bad in [
            crate::question::QuestionAnswer::default(),
            crate::question::QuestionAnswer::choosing(9),
            crate::question::QuestionAnswer {
                note: Some("hmm".into()),
                ..Default::default()
            },
        ] {
            let f = hub.submit(
                &a.head_id,
                "c1",
                0,
                CommandKind::Answer {
                    req_id: "r1".into(),
                    reply: Reply::Question(bad.clone()),
                },
            );
            match f {
                ServerFrame::Rejected { reason, .. } => {
                    assert!(
                        reason.starts_with(crate::question::REJECT_MALFORMED_ANSWER),
                        "{reason}"
                    );
                    assert!(reason.contains("still open"), "{reason}");
                }
                other => panic!("{bad:?} was accepted: {other:?}"),
            }
        }
        // And it really is still open, so the person can be asked again.
        assert!(
            hub.snapshot()
                .open_decisions
                .iter()
                .any(|d| d.req_id == "r1"),
            "the question must survive a malformed answer"
        );
    }
}

#[cfg(test)]
mod mode_steering_tests {
    use super::*;
    use crate::testing::*;

    /// **Looking is not taking.**
    ///
    /// `has_queued_prompt` exists so a round that backgrounded a job can decide to
    /// hand the floor back. If it consumed what it found, the operator's message
    /// would vanish from the worker's queue and be answered by nobody — which is
    /// the defect `try_steering_command` was careful about in the other direction.
    /// So: it sees the prompt, and the prompt is still there afterwards.
    #[test]
    fn peeking_at_a_queued_prompt_leaves_it_for_the_worker() {
        let hub = Hub::new("s-peek");
        let a = hub.attach("tui", "dead", Caps::default(), 0);
        assert!(!hub.has_queued_prompt(), "nothing queued yet");

        hub.submit(
            &a.head_id,
            "c1",
            0,
            CommandKind::Prompt {
                text: "stop and look at this".into(),
            },
        );
        assert!(hub.has_queued_prompt());
        // Twice, because the whole point is that the first look changed nothing.
        assert!(hub.has_queued_prompt());
        assert!(
            matches!(
                hub.take_own_work(),
                OwnWork::Command(QueuedCommand {
                    kind: CommandKind::Prompt { .. },
                    ..
                })
            ),
            "the prompt must still be there for the worker"
        );
        assert!(!hub.has_queued_prompt(), "and gone once it is taken");
    }

    /// An interrupt is not a prompt: it stops the turn through its own path, and a
    /// yield that fired on it would end turns the operator only meant to cut short.
    #[test]
    fn an_interrupt_is_not_a_queued_prompt() {
        let hub = Hub::new("s-peek2");
        let a = hub.attach("tui", "dead", Caps::default(), 0);
        hub.submit(
            &a.head_id,
            "c1",
            0,
            CommandKind::Interrupt {
                reason: "esc".into(),
            },
        );
        assert!(!hub.has_queued_prompt());
    }

    /// **THE DAEMON MAY INTERRUPT, MAY RELAY A PARENT'S MESSAGE, AND NOTHING ELSE.**
    ///
    /// `job_kill` of a subagent was refused here, every time, and the refusal was invisible:
    /// `harnessd`'s task runner submitted the interrupt under [`DAEMON_SUBMITTER`] — *"the
    /// vocabulary of a caller that is not a head at all"*, its comment said — while this
    /// function's head lookup did not know the name, so every kill answered
    /// `Rejected { reason: "not attached" }`. MEASURED 2026-10-05: the operator's three subagents
    /// were told they had been stopped, went on running, and their row counts grew (102 to 108)
    /// while the kill was in flight. The door is open for the daemon now — and ONLY for the
    /// interrupt and the relayed message, because an `Answer` needs `can_decide`, a `Prompt`
    /// speaks as a person, and the daemon is neither.
    #[test]
    fn the_daemon_may_interrupt_and_relay_and_nothing_else() {
        let hub = Hub::new("s-daemon-door");

        // **A parent's correction to a subagent in flight** — relayed by the daemon, because
        // the parent is seated in another session's hub entirely. Admitted like the interrupt,
        // and it must NOT be recorded as the daemon: the child's trail has to name which parent
        // spoke, or the correction is advice from nowhere.
        let relay = hub.submit(
            DAEMON_SUBMITTER,
            "task_message-1",
            0,
            CommandKind::Message {
                from: "s-parent".into(),
                text: "stop and report what you have".into(),
            },
        );
        assert!(
            matches!(relay, ServerFrame::Accepted { .. }),
            "the daemon's relay of a parent's message was refused: {relay:?}"
        );
        // Taken mid-turn, which is the only thing that makes it *live* correction: a message
        // that waits for the worker arrives after the turn it was meant to steer.
        let queued = hub
            .try_steering_command()
            .expect("a relayed message is not mid-turn steering, so it waited for the worker");
        assert_eq!(
            queued.identity, "s-parent",
            "the postman was recorded as the speaker: {queued:?}"
        );
        assert_eq!(queued.head_id, DAEMON_SUBMITTER);
        assert!(
            matches!(queued.kind, CommandKind::Message { .. }),
            "the wrong kind came back: {queued:?}"
        );

        // The thing the daemon actually needs: stop a turn in a session where none of our heads
        // is seated, which is every subagent from the parent's point of view.
        let ok = hub.submit(
            DAEMON_SUBMITTER,
            "job_kill-1",
            0,
            CommandKind::Interrupt {
                reason: "the parent asked for this turn to stop".into(),
            },
        );
        assert!(
            matches!(ok, ServerFrame::Accepted { .. }),
            "the daemon's own interrupt was refused: {ok:?}"
        );

        // And nothing else. Each of these would be the daemon impersonating somebody.
        for (kind, what) in [
            (
                CommandKind::Prompt {
                    text: "speak as the operator".into(),
                },
                "send a prompt",
            ),
            (CommandKind::Compact, "force a compaction"),
        ] {
            let r = hub.submit(DAEMON_SUBMITTER, "c2", 0, kind);
            match r {
                ServerFrame::Rejected { reason, .. } => {
                    assert_eq!(reason, "not attached", "the daemon was allowed to {what}")
                }
                other => panic!("the daemon was allowed to {what}: {other:?}"),
            }
        }

        // **And the rule the door is an exception TO still holds.** `not attached` is what a
        // name nobody minted gets, for an interrupt like anything else — an exception that
        // swallowed the rule would be worse than the bug it fixes.
        let r = hub.submit(
            "nobody",
            "c3",
            0,
            CommandKind::Interrupt { reason: "?".into() },
        );
        match r {
            ServerFrame::Rejected { reason, .. } => assert_eq!(reason, "not attached"),
            other => panic!("a name nobody minted was admitted: {other:?}"),
        }
    }

    /// **A mode change reaches a running turn.**
    ///
    /// It used to sit in the queue until the worker came back — which is after
    /// the turn — so `/mode allow-all` during a turn moved nothing while the turn
    /// went on asking under the old point. Measured on the operator's own session,
    /// 2026-09-20: four asks at `automode-edits` after they had set `allow-all`,
    /// and the two confirmation cards arriving only when the model stopped.
    #[test]
    fn a_mode_command_is_taken_mid_turn_and_a_compaction_still_waits() {
        let hub = Hub::new("s1");
        let a = hub.attach("tui", "dead", Caps::default(), 0);

        hub.submit(&a.head_id, "c1", 0, CommandKind::Compact);
        hub.submit(
            &a.head_id,
            "c2",
            0,
            CommandKind::Mode {
                name: "allow-all".into(),
                consented: true,
            },
        );

        // The mode is taken now, from inside the turn.
        let got = hub.try_mode_command().expect("a mode command");
        match got.kind {
            CommandKind::Mode { name, consented } => {
                assert_eq!(name, "allow-all");
                assert!(consented, "the operator's confirmation travelled with it");
            }
            other => panic!("took the wrong command: {other:?}"),
        }
        // Once. A second poll finds nothing rather than re-applying it.
        assert!(hub.try_mode_command().is_none());

        // And the compaction queued in front of it is untouched: it belongs to the
        // worker, at the end of the turn, exactly as before.
        assert!(
            hub.try_steering_command().is_none(),
            "a compaction is not steering"
        );
        assert!(
            matches!(
                hub.take_own_work(),
                OwnWork::Command(QueuedCommand {
                    kind: CommandKind::Compact,
                    ..
                })
            ),
            "the compaction was eaten"
        );
    }

    /// **A door call issued during a turn is taken by the turn** — R31's deposit.
    ///
    /// The requirement: *"it must be issuable WHILE A TURN IS RUNNING — the operator watches
    /// the model go down a wrong path and drops the doc in. A door that only opens at idle is
    /// worth a fraction of this."* Before this, it was idle-only, and the mechanism is in
    /// `Hub::try_head_run_command`'s own docs: the door's frame queues a `CommandKind` that
    /// only the single worker drains, and the worker is inside the turn.
    ///
    /// Three assertions, and the second is the one that keeps it honest:
    ///
    /// 1. an `execute: true` call is taken here, mid-turn;
    /// 2. **an `execute: false` call is NOT** — that head runs its own tool and reports back,
    ///    so admitting it here would hand its result to a worker that never saw the call;
    /// 3. it is taken once, and a prompt queued in front of it is left alone.
    #[test]
    fn a_door_call_issued_during_a_turn_is_taken_by_the_turn() {
        let hub = Hub::new("s");
        let head = hub.attach("tui", "dead", Caps::default(), 0);

        // A head with its own tool runtime: the daemon admits and the HEAD runs it.
        hub.submit(
            &head.head_id,
            "c1",
            0,
            CommandKind::OperatorCall {
                call_id: "h1-1".into(),
                name: "web_fetch".into(),
                arguments: r#"{"url":"http://example.invalid"}"#.into(),
                who: "dead".into(),
                execute: false,
            },
        );
        assert!(
            hub.try_head_run_command().is_none(),
            "a call the head runs itself is not the daemon's to run mid-turn"
        );

        // And one the daemon runs, which is the deposit.
        hub.submit(
            &head.head_id,
            "c2",
            0,
            CommandKind::OperatorCall {
                call_id: "h1-2".into(),
                name: "read".into(),
                arguments: r#"{"path":"crates/tui/src/app.rs"}"#.into(),
                who: "dead".into(),
                execute: true,
            },
        );
        let got = hub.try_head_run_command().expect("the daemon's own call");
        match got.kind {
            CommandKind::OperatorCall {
                call_id,
                name,
                who,
                execute,
                ..
            } => {
                assert_eq!(call_id, "h1-2");
                assert_eq!(name, "read");
                assert_eq!(
                    who, "dead",
                    "the identity the corpus records as `human:<who>`"
                );
                assert!(execute, "and the daemon is the one that runs it");
            }
            other => panic!("took the wrong command: {other:?}"),
        }
        // Once, and the head's own call is STILL THERE for the worker — not dropped, which
        // would leave its result arriving for a call nothing admitted.
        assert!(hub.try_head_run_command().is_none());
        let left = hub
            .try_command()
            .expect("the head's own call is still queued");
        assert!(
            matches!(left.kind, CommandKind::OperatorCall { execute: false, .. }),
            "{:?}",
            left.kind
        );
    }

    /// **A closed hub still hands over what was queued for the head — ordered, not
    /// raced.**
    ///
    /// The integration test this protects
    /// (`a_stop_closes_the_registry_even_with_a_command_queued_and_nobody_draining`)
    /// only caught the defect once in three runs on a loaded runner, because it
    /// depended on which thread won. This does not depend on anything: publish, close,
    /// then ask. The first answer must be the event; only then `Closed`.
    ///
    /// It would have failed every time before the fix, because `next_batch` checked
    /// `closed` before it looked at the queue.
    #[test]
    fn a_closed_hub_delivers_what_was_queued_before_it_says_closed() {
        let hub = Hub::new("ordered-shutdown");
        let head = hub.attach("h", "tui", Caps::default(), 0).head_id;

        // **No drain first, and that is not an omission.** A snapshot attach leaves
        // this head's queue empty, and `next_batch` on an empty queue of an OPEN hub
        // BLOCKS on its condvar — MEASURED: the first version of this test did
        // `let _ = hub.next_batch(..)` here to "clear any backlog", and the test hung
        // until it was killed. Publish, then close, then ask: the first answer is the
        // event because there is something to answer with.
        hub.publish(SessionEvent::Warning {
            code: "daemon_stopping".into(),
            detail: "`dead` asked this daemon to stop".into(),
            compaction: None,
        });
        hub.close();

        let first = hub.next_batch(&head, 256);
        match &first {
            Delivery::Events(b) => assert!(
                b.events().iter().any(|e| matches!(
                    &e.event,
                    SessionEvent::Warning { code, .. } if code == "daemon_stopping"
                )),
                "the event that WAS queued must be delivered, not dropped: {b:?}"
            ),
            other => panic!(
                "a closed hub with a queued event returned {other:?}; the head would \
                 never be told why the daemon went"
            ),
        }

        // And only now, with the queue empty, does it say closed.
        assert_eq!(
            hub.next_batch(&head, 256),
            Delivery::Closed,
            "the hub must still report Closed once it has delivered what it owed"
        );
    }
}
