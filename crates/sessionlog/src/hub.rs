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
    /// **When the command was accepted**, wall-clock ms — so the worker can say how long it
    /// sat in the queue (`worker.cmd`, `docs/metrics.md` §4 #6). The c12d044 stall, 29–89 s
    /// behind a store read, was found with `perf` because this field did not exist.
    pub enqueued_ms: u64,
    pub kind: CommandKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandKind {
    Prompt {
        text: String,
        /// **The chips the operator put in the composer** (`PROTOCOL_VERSION` 50).
        ///
        /// Carried through the queue as the frame sent them and converted by the harness at
        /// the turn's own seam — the daemon reads the files (`crate::attachments`), so a
        /// prompt is still one item in the queue however much it carries, and a take-back
        /// (`WithdrawPrompts`) drops the chips with the words that named them.
        attachments: Vec<crate::protocol::Attachment>,
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
    /// **A head ASKS for the point that needs a person** (`PROTOCOL_VERSION` 54, B8).
    ///
    /// Not [`CommandKind::Mode`] with `consented: false` and a hope: that command means *set
    /// it*, and setting a point without consent is refused by the harness for the reason the
    /// point exists. This one means *put the question in front of somebody*, and what the
    /// harness does with it is decide whether anybody has to be asked at all — a point that
    /// needs no confirmation is applied by this command too, and settled as applied.
    ModeConfirm {
        name: String,
    },
    /// **A person's answer to that ask**, from the head the card was sent to.
    ///
    /// Sent by a head, never by the daemon, and the harness accepts it only while the request
    /// is open and only from a head that was asked — see `Harness::answer_mode_confirm`. A
    /// `yes` opens one point for one session; a `no` applies nothing.
    ModeConsent {
        req_id: String,
        yes: bool,
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
    /// **What the operator's own shell line produced — back from the run's own thread.**
    ///
    /// [`CommandKind::OperatorShell`] no longer runs on the worker: it is handed to a thread
    /// of its own (`harnessd`'s `bangrun`), because a run that holds the worker holds every
    /// other session's work with it — the operator's report, in their words: *"while a run of
    /// theirs is in flight, nothing else runs. another `!` line only queues."* That thread
    /// waits on the command and hands THIS back when it ends, and the worker — which stayed
    /// free the whole time — appends the two rows and starts the turn.
    ///
    /// It is a variant beside [`CommandKind::OperatorResult`] rather than that variant reused,
    /// and the reason is the door's own rule: `OperatorResult` appends only what an ADMITTED
    /// call produced (`take_operator_call` refuses a call id with no admission behind it), and
    /// a `!` line has no admission by design — *"no gate, no adjudication row, no
    /// `OperatorCallAllowed`"*. Faking one to reuse the arm would put the operator's own rows
    /// under machinery built to refuse them; carrying the same fields under an honest name
    /// does not.
    ///
    /// **It is never taken mid-turn.** The round-boundary picker (`try_head_run_command`)
    /// deliberately does not match it: the run is already over, so there is nothing left to
    /// wait out, and the rows can land at the next between-turns pass — where the turn they
    /// are for starts.
    OperatorShellResult {
        /// The line as submitted, verbatim — the `User` row's text.
        line: String,
        /// Who asked, as [`CommandKind::OperatorShell`] carried it. Becomes the `who` in
        /// `CallOrigin::Operator` and the identity the daemon's own log names.
        who: String,
        /// The call id the worker minted (`bang-<n>`), so the row is the one the run was
        /// started for and no other.
        call_id: String,
        outcome: letibot_transcript::ToolOutcome,
        /// What the model will read — already rendered and capped by the runtime's own
        /// spill policy, exactly as the synchronous path's `result.render()` was.
        payload: String,
        /// **The spill half, when there was one.** The note the worker writes names what
        /// was produced and where the rest went; these are its two numbers, carried rather
        /// than recomputed — a second implementation of the cap is a second answer.
        spill: Option<ShellSpill>,
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
    /// **The operator's half of the todo board**, replaced wholesale — plus the state moves they
    /// asked for on rows that are not theirs.
    ///
    /// A head owns these rows — they are its own store's contents — so it sends the whole list on
    /// every change rather than a delta: a delta protocol for a list of tens of items would be a
    /// second source of truth about them, and `TodoBoard::set_operator` replaces one half atomically.
    ///
    /// **`moved` is the reach and not a second list.** `/todo postpone|resume` used to name only the
    /// operator's own rows, so a row the MODEL wrote — accurate, and waiting on a person's hand —
    /// had no door at all: `todo_write` refuses `postponed`, and the operator's verb could not name
    /// the row. These entries carry the row's words and the state the operator gave it, and the
    /// daemon resolves them against the model's half by content — the board's one key. Membership
    /// is untouched: a head can move a model's row's state and can never add, drop or re-author one.
    ///
    /// Nothing here is a decision and nothing is gated: the operator's own list is not a tool call.
    SetOperatorTodos {
        items: Vec<crate::event::TodoEntry>,
        moved: Vec<crate::event::TodoState>,
    },
    /// A head settled an open request. **Which kind** it settled is [`Reply`], and
    /// it is an enum rather than two variants here because every consumer that only
    /// cares "an answer arrived for `req_id`" already destructures this variant with
    /// `..` — a second variant would have silently skipped those arms.
    Answer {
        req_id: String,
        reply: Reply,
    },
    /// **The operator answered the question a todo row waits on** (`ClientFrame::AnswerTodo`,
    /// `PROTOCOL_VERSION` 47). Not an [`CommandKind::Answer`]: that settles an open decision a
    /// tool call is blocked on, and nothing is blocked on this one — the row is on the board,
    /// the board is the harness's, and the answer is validated there against the row's own
    /// options. A running turn takes it at its next step boundary (it is one of
    /// [`Hub::try_steering_command`]'s kinds, as an operator's prompt is); between turns the
    /// worker applies it and runs the turn it makes.
    AnswerTodo {
        content: String,
        question: String,
        answer: crate::question::QuestionAnswer,
    },
}

/// **The spill half of a finished operator run**, as the size note needs it.
///
/// Two numbers, carried rather than recomputed: [`crate::protocol`] does not know what a
/// spill store is and must not learn, and the worker that writes the note needs exactly
/// these — how much the command produced in full, and the hash that locates the rest.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellSpill {
    /// What the command produced in full, before the inline cap.
    pub full_bytes: usize,
    /// The locator, as `read_spill hash=…` spells it.
    pub hash: String,
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
    /// The command's one-word name, as the daemon's log and a refusal's sentence spell it.
    pub fn verb(&self) -> &'static str {
        match self {
            CommandKind::Prompt { .. } => "prompt",
            CommandKind::Compact => "compact",
            CommandKind::Reseat { .. } => "reseat",
            CommandKind::Interrupt { .. } => "interrupt",
            CommandKind::Message { .. } => "message",
            CommandKind::Answer { .. } => "answer",
            CommandKind::AnswerTodo { .. } => "answer-todo",
            CommandKind::Mode { .. } => "mode",
            CommandKind::ModeConfirm { .. } => "mode-confirm",
            CommandKind::ModeConsent { .. } => "mode-consent",
            CommandKind::Slash { .. } => "slash",
            CommandKind::ReadJobOutput { .. } => "read-job-output",
            CommandKind::OperatorCall { .. } => "operator-call",
            CommandKind::OperatorResult { .. } => "operator-result",
            CommandKind::OperatorShell { .. } => "operator-shell",
            CommandKind::OperatorShellResult { .. } => "operator-shell-result",
            CommandKind::WithdrawPrompts => "take-back",
            CommandKind::SetOperatorTodos { .. } => "operator todos",
            CommandKind::Promote => "promote",
        }
    }
}

/// **What one session's hub holds**, for the daemon's memory census (`harnessd::memwatch`).
///
/// Every number here is something a bound in this crate claims to keep — the log by
/// [`crate::log::LogBounds`], each head's queue by the gap bound, the view by
/// [`crate::view::ViewBounds`] — so the census is also how a broken bound is SEEN rather than
/// inferred from a daemon that died at 108 GB with nothing in its log to say why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HubCensus {
    pub session_id: String,
    pub log_events: usize,
    pub log_bytes: usize,
    pub log_bound_bytes: usize,
    pub view: crate::view::ViewCensus,
    pub heads: usize,
    /// The longest queue any head has, and the bound it is held to.
    pub head_queue_max: usize,
    pub head_queue_bound: usize,
    pub head_side_max: usize,
    pub commands: usize,
    pub operator_calls: usize,
    pub secrets: usize,
    pub screens: usize,
    /// **What this session's HARNESS holds** — the half of a session's cost that is not on the
    /// hub at all. See [`Footprint`]; all zeroes for a hub nobody registered one on.
    pub footprint: Footprint,
}

/// **What one session's harness holds in memory** — R66.
///
/// A hub can account for itself (`HubCensus`): its log, its view, its heads' queues. But the
/// biggest per-session structures in a daemon are on the other side of the wire — the harness's
/// in-memory transcript, the ledger's token region, the authorisation trail — and a census that
/// cannot see them answers the question R66 asked with a `rss` line and no name on it.
///
/// # What is counted, and what is deliberately not
///
/// | field | counts | does **not** count |
/// |---|---|---|
/// | `items` | rows in the in-memory transcript | the rows the store holds, or the hub's view's |
/// | `items_text_bytes` | `TranscriptItem::bytes` summed over those rows — one definition, the same one the view's byte bound uses | ids, enums, struct overhead, the `Vec`'s spare capacity |
/// | `ledger_region_bytes` | the token region's MAPPED bytes (`TokenRegion::bytes`) | the pages not yet written; the ledger's rows and their hashes |
/// | `ledger_tokens` | tokens appended, the prefix included | — |
/// | `trail_bytes` | bytes of text the authorisation trail holds | the `Said` structs around them |
/// | `trail_utterances` | utterances held | the items it has scanned, which is a count and not a body |
///
/// So `total()` is a **floor** on what the session costs and a fair reading of which of the
/// three grew. It is not a heap measurement: `memwatch`'s `Counting` allocator is, and the
/// difference between the two is the honest answer to *how much of the daemon is this session*.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Footprint {
    /// Rows the in-memory transcript holds.
    pub items: usize,
    /// Bytes of TEXT those rows carry — [`letibot_transcript::TranscriptItem::bytes`].
    pub items_text_bytes: usize,
    /// Bytes the ledger's token region is mapped at, capacity and not occupancy.
    pub ledger_region_bytes: usize,
    /// Tokens appended to the ledger, the stable prefix included.
    pub ledger_tokens: usize,
    /// Bytes of text the authorisation trail holds.
    pub trail_bytes: usize,
    /// Utterances the authorisation trail holds.
    pub trail_utterances: usize,
    /// **The ceiling this session's harness states for [`total`](Self::total)** — the window's
    /// arithmetic, or a flat tripwire for a session with no wall. See
    /// `letibot_harnessd::footprint` for the numbers and what justifies each.
    ///
    /// Zero means *no ceiling was stated*, which is not the same fact as *inside it*: a hub
    /// nobody registered a footprint on has nothing to be over.
    pub ceiling_bytes: usize,
}

impl Footprint {
    /// The three byte terms, summed. See the type's table for what each one counts.
    pub fn total(&self) -> usize {
        self.items_text_bytes + self.ledger_region_bytes + self.trail_bytes
    }

    /// **Has this session grown past the ceiling its harness states for itself?**
    ///
    /// The ceiling is the harness's own claim, so this is a bound CHECKED rather than a tripwire
    /// chosen here. A hub nobody registered a footprint on states no ceiling, and `0` therefore
    /// answers `false` — *nothing was claimed* is not *a bound held*.
    pub fn over_ceiling(&self) -> bool {
        self.ceiling_bytes > 0 && self.total() > self.ceiling_bytes
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
    /// **The last [`SessionEvent::HeldResults`] this hub published** (R91.12) — republished to a
    /// head when it attaches or takes a resync, because the event is ephemeral (scrubbed from
    /// every replay, in no snapshot) and the daemon says it only when the counts CHANGE. Without
    /// this a head that attached mid-turn showed nothing held while results were, and one that
    /// reconnected after missing the zero kept a stale count for good.
    held_now: Option<SessionEvent>,
    /// **The last [`SessionEvent::ActivityChanged`] this hub published** — republished to a head
    /// when it attaches (after a snapshot, a gap or on a resync), for [`Inner::held_now`]'s reason
    /// exactly: [`crate::activity::Activity`] is ephemeral — scrubbed from every replay — and the
    /// daemon says it only when the value CHANGES. Without this a head that attached mid-turn or
    /// reconnected across a change kept the activity from before it.
    activity_now: Option<SessionEvent>,
    /// **The last [`SessionEvent::Filling`] this hub published** — republished to a head when it
    /// attaches, for the reason the two above are, and it is the longest wait of the three.
    ///
    /// A restore announces itself BEFORE the worker reaches it (`server`'s `ResumeSession` arm
    /// paints `0 of N rows` from the store's own count), and the head that most needs that
    /// sentence is the one that arrives a moment later and is handed the empty snapshot: a fill
    /// is in no snapshot, so without this the head drew a session it had just been told holds
    /// 166 rows as one that *"has said nothing yet"*. MEASURED 2026-10-26, the operator's
    /// `leticode --session` against a daemon busy with another session's turn.
    ///
    /// **Only an INCOMPLETE fill is held** — see `Inner::append_and_fan` — so a restore that
    /// finished is never re-announced to a head that attaches afterwards. A restore that FAILED
    /// leaves its `0 of N` here, which a head draws for the fifteen seconds its own no-news window
    /// allows, beside the `resume_failed` warning that says what went wrong.
    filling_now: Option<SessionEvent>,
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
    /// **The metrics tap** (TODO R81): the session's "now" for a mark, and the rows the hub
    /// already carries. See [`crate::metrics_tap`].
    tap: crate::metrics_tap::Tap,
}

/// **How the daemon above a hub computes what the session is waiting on** — see
/// [`Hub::refresh_activity`].
///
/// A closure rather than a trait object with methods, because there is one question and it is
/// asked with the hub it is about: the answer is derived from the hub's own view (the turn, its
/// calls, the cards still open) *and* from facts no event on this hub carries (the watchers on
/// its jobs and children, the monitors it armed, the settlements it is owed). The hub cannot see
/// the second half — `letibot-sessionlog` has no idea what a job is — so the half that owns them
/// registers how to ask, and the hub owns the one thing that must not be duplicated: when the
/// value changed.
///
/// The hub is handed back in so the closure cannot answer about a different session than the one
/// it was registered on, and so it holds no `Arc<Hub>` of its own (which would be a cycle).
///
/// # What a source may not do, and what it cost to learn
///
/// **It may not wait on a lock that a thread outside the publishing path can hold.** [`Hub::publish`]
/// calls it ([`Hub::refresh_activity`]), and `publish` is made from inside state machines that hold
/// their own locks — the merge queue's `RepoQueue::pass` → `apply_gate` → `judged` → `raise` →
/// `publish_for_repository` is the one that measured it. So a source that blocks turns *a publish*
/// into *a lock the queue's thread holds while it holds the queue*, which is a cycle the moment any
/// holder of that lock is waiting for the queue.
///
/// Measured 2026-10-29 (`harnessd 0.5.1`, pid 53775): the queue's worker sat in
/// `refresh_activity` → `activity::facts_of` → `JobWatchers::pending` → `Mutex::lock` for 8½ hours,
/// 795 of 795 samples, because a session closing itself published while holding the queue that
/// read was about to take. The lease's heartbeat stopped at the second `regate`, a target stayed
/// `gating` with no gate process, and an approved branch sat `queued` with nothing to land it.
///
/// The shape that satisfies it, and what `letibot_harnessd::activity::source` now uses: the facts a
/// source reads are either atomics or **leaf** locks — held only across a clone or a store of an
/// `Arc`, never across a call that can block — so no source read can be part of a cycle. A source
/// that must read something else should publish a snapshot from the thread that owns it rather than
/// take that thread's lock here.
pub type ActivitySource = Arc<dyn Fn(&Hub) -> crate::activity::Activity + Send + Sync>;

/// **How the daemon above a hub says what this session's HARNESS holds** — see [`Footprint`].
///
/// A closure, like [`ActivitySource`], and for the same reason: the structures are on the
/// harness, which `letibot-sessionlog` has no idea exists, so the half that owns them registers
/// how to ask and the hub owns where the answer goes.
///
/// # Why it takes no `&Hub`, unlike the activity's source
///
/// The activity's source is handed its hub so it cannot answer about a different session. This
/// one cannot get that wrong: it reads one harness's own fields, and a harness belongs to one
/// session by construction. It is called from [`Hub::census`] — **after the hub's lock is
/// dropped**, because the trail it reads is a lock the harness takes on every turn, and holding
/// the hub's while taking that one is the cycle [`ActivitySource`] documents.
///
/// # What a source may not do
///
/// **It may not take a lock a turn holds, and it may not do work that grows with the
/// transcript.** It is read once a minute by the memory watchdog and, in a test, on demand. So
/// the harness does not walk its items here: the numbers are restated by the thread that owns
/// them at every point they change (`Harness::reconcile`), and this reads them out of atomics
/// plus one short-lived lock on the trail.
pub type FootprintSource = Arc<dyn Fn() -> Footprint + Send + Sync>;

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
    /// **The deadline the reader itself handed [`Hub::take_own_work_until`] arrived before any
    /// work did.** Not work and not a wake: a clock the reader asked to be told about, which is
    /// how a subagent's serving thread (`harness::serve_child`) delivers its idle plan-check on
    /// the thread that owns the harness — the only thread that may drive it. Work still wins:
    /// a command or wake that arrives first is returned first, exactly as the bell keeps work
    /// ahead of wakes for a session the daemon holds.
    TimedOut,
}

/// **Who runs this session's turns — the one reader its command queue has** (R68.1).
///
/// A session has exactly one owner, and before this the hub did not know which. Two readers
/// took from one queue: the thread that runs a subagent (`Hub::take_own_work_until`, from
/// `harness::serve_child`) and the daemon's worker (`Hub::try_command`, through
/// `Registry::next_work_until`), and the worker popped ANYTHING. `Interrupt` and `Message` were
/// handed back to the child's thread when the worker won them; `Prompt` was not — so a head
/// prompting the subagent it was attached to could make the worker build a SECOND `Harness` for
/// a live child: two writers on one transcript, the child's `watch_runs`, `set_prompt` and
/// `notes` replaced under it, a second deadline thread. CONFIRMED by the 2026-10-11 review
/// (`TODO.md` section 4, *the daemon*, finding 1).
///
/// So the owner is a fact of the hub and the doors read it, rather than each arm remembering
/// to give a command back: [`Hub::try_command`] (the worker's pop) answers `None` for a hub it
/// does not own, and `Sessions::harness_as` refuses to build a harness for a session that is
/// live under another owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Owner {
    /// **The daemon's worker** — every session a head created or resumed, and the default for
    /// every hub, which is what every hub was before this tag existed.
    #[default]
    Worker,
    /// **A thread of its own** — a subagent, whose harness lives on the thread its parent
    /// spawned it on and borrows `Parts` built there. Set by that thread
    /// ([`Hub::owned_by_its_own_thread`]) before the hub is adopted into a registry, so there
    /// is no instant in which the worker can see the session and think it is its own.
    OwnThread,
}

/// One session's log, view, heads and command queue.
pub struct Hub {
    inner: Mutex<Inner>,
    /// Who drains [`Inner::commands`]. See [`Owner`]. Outside the lock because it is written
    /// once, before the hub is reachable, and read on every pop.
    owner: std::sync::atomic::AtomicU8,
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
    /// **What this session is waiting on, as it was last published** — see
    /// [`Hub::set_activity`]. Outside [`Inner`] on purpose: the compare-and-publish must not run
    /// under the lock every head's fan-out takes, and the value is read by callers that want one
    /// fact and no view.
    activity: Mutex<crate::activity::Activity>,
    /// **How the daemon above this hub computes the value** — see [`Hub::refresh_activity`].
    /// Registered once by whoever owns the session (`letibot_harnessd`'s `activity` module), and
    /// given the hub it is about so the two cannot disagree about which session they mean.
    activity_source: Mutex<Option<ActivitySource>>,
    /// **How the daemon above this hub says what the HARNESS holds** — see [`Footprint`] and
    /// [`FootprintSource`]. Registered once, by the session's own harness at open.
    footprint_source: Mutex<Option<FootprintSource>>,
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
                held_now: None,
                activity_now: None,
                filling_now: None,
                closed: false,
                bell: None,
                answers: None,
                secrets: HashMap::new(),
                next_secret: 0,
                screens: HashMap::new(),
                next_screen: 0,
                tap: Default::default(),
            }),
            cv: Condvar::new(),
            owner: std::sync::atomic::AtomicU8::new(0),
            promote: Arc::new(Mutex::new(None)),
            activity: Mutex::new(crate::activity::Activity::Quiet),
            activity_source: Mutex::new(None),
            footprint_source: Mutex::new(None),
        })
    }

    /// **Mark this session as served by the thread that is opening it, not by the worker.** See
    /// [`Owner`]. Called by a subagent's runner on its own thread before the hub is adopted.
    pub fn owned_by_its_own_thread(&self) {
        self.owner.store(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Who runs this session's turns. See [`Owner`].
    pub fn owner(&self) -> Owner {
        match self.owner.load(std::sync::atomic::Ordering::SeqCst) {
            0 => Owner::Worker,
            _ => Owner::OwnThread,
        }
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

    /// **Where an operator's mark lands** — the turn running (or the last one), its round and
    /// what it is doing; or, for a row they were reading back, the turn and round that row was
    /// written in (`None` for the second half when the row is older than the tap remembers).
    /// Under the hub's own lock and nothing else: this is read on the connection thread, never
    /// through the worker (`docs/metrics.md` §2.5).
    pub fn metrics_now(
        &self,
        item_id: Option<&str>,
    ) -> (crate::metrics_tap::Now, Option<(String, u32)>) {
        let g = self.lock();
        let row = item_id.and_then(|i| g.tap.item(i));
        (g.tap.now(), row)
    }

    pub fn dropped(&self) -> u64 {
        self.lock().log.dropped()
    }

    /// Append an event and fan it out. **Never blocks on a head.**
    pub fn publish(&self, event: SessionEvent) -> Envelope {
        // **What the session is waiting on is re-derived after anything that could have moved
        // it, and it costs nothing when nothing did** — [`Hub::set_activity`] publishes only on a
        // change. The trigger is here rather than at a dozen call sites in the daemon because the
        // sites that matter are the ones that publish: a turn starting or ending, a call proposed
        // or finished, a card raised or answered, a job settled, a child reported. A site that
        // forgot to ask would leave a head reading *done* at a session with work left in it, which
        // is the defect this whole mechanism exists to end.
        let touches = event.touches_activity();
        let env = {
            let mut g = self.lock();
            g.append_and_fan(event)
        };
        self.cv.notify_all();
        if touches {
            self.refresh_activity();
        }
        env
    }

    /// **What this session is waiting on**, as it was last published — the same value a head has.
    pub fn activity(&self) -> crate::activity::Activity {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// **Publish [`SessionEvent::ActivityChanged`] if — and only if — the value changed.**
    ///
    /// The one place the *changed* in *published only when the value changes* is decided, so no
    /// caller has to hold a copy to compare against: a turn's twenty rounds publish one value, and
    /// a job settling under a running turn publishes none. Returns whether anything was published.
    pub fn set_activity(&self, now: crate::activity::Activity) -> bool {
        {
            let mut last = self.activity.lock().unwrap_or_else(|e| e.into_inner());
            if *last == now {
                return false;
            }
            *last = now.clone();
        }
        self.publish(SessionEvent::ActivityChanged { activity: now });
        true
    }

    /// **Register how the daemon computes the value** — see [`ActivitySource`]. Called once, by
    /// whoever owns the session; a second call replaces the first, which is what a re-opened
    /// session needs.
    pub fn set_activity_source(&self, source: ActivitySource) {
        *self
            .activity_source
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(source);
    }

    /// **Re-derive the value and publish it if it moved.** The daemon's own state is what the
    /// source closure reads, so this is how a change that no event announces — a settlement
    /// leaving a watcher's hands, a monitor firing, a prompt sitting in the queue — becomes news.
    ///
    /// Nothing happens for a hub nobody registered a source on (every test, and every hub built
    /// below the daemon), which is the honest reading: such a hub has no jobs to be waiting on.
    ///
    /// **This runs inside [`Hub::publish`], so the source must not block** — see [`ActivitySource`]
    /// for the measurement that made that a rule rather than a wish.
    pub fn refresh_activity(&self) -> bool {
        let source = self
            .activity_source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(source) = source else {
            return false;
        };
        let now = source(self);
        self.set_activity(now)
    }

    /// **A settlement, a monitor's firing, or a prompt is owed to this session's model** — the
    /// third of [`crate::activity::Waiting`]'s three unnamed entries, asked of the queue that owes
    /// it.
    ///
    /// Both halves are the *model's* work and not the head's: a command a head submitted is a turn
    /// the worker has not reached yet, and a wake is a settlement some thread has already queued
    /// for it ([`Hub::wake_its_own_reader`]). It is read outside the lock and is therefore a
    /// moment, not a guarantee — which is right for a value that is republished when it moves.
    pub fn work_pending(&self) -> bool {
        let g = self.lock();
        !g.commands.is_empty() || g.own_wake
    }

    /// **The turn and its calls, as the view sees them right now** — `(a round is generating, a
    /// call of this round is still running)`. The two of [`crate::activity::Waiting`] that come
    /// from the hub's own state rather than from the daemon's watchers.
    pub fn turn_shape(&self) -> (bool, bool) {
        let g = self.lock();
        match g.view.turn() {
            Some(t) => match &t.state {
                crate::view::TurnState::Running => (true, false),
                crate::view::TurnState::Finished { .. } => (
                    false,
                    t.calls
                        .iter()
                        .any(|c| !matches!(c.state, crate::view::CallState::Finished { .. })),
                ),
                // Terminated, whatever its calls were left as — [`Hub::turn_in_flight`]'s rule.
                crate::view::TurnState::Interrupted { .. }
                | crate::view::TurnState::Failed { .. } => (false, false),
            },
            None => (false, false),
        }
    }

    /// **The cards still owed an answer**, oldest first — the one entry of
    /// [`crate::activity::Waiting`] that does not end by itself.
    pub fn open_ask_ids(&self) -> Vec<String> {
        self.lock()
            .view
            .open_decisions()
            .iter()
            .map(|d| d.req_id.clone())
            .collect()
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
        self.push_frame_where(frame, |_| true);
    }

    /// **A frame to the heads a predicate names** — the same door [`Hub::push_frame`] is, with
    /// one head's answer `false`.
    ///
    /// The caller today is a mode confirmation (`PROTOCOL_VERSION` 54): the card goes to every
    /// head that may decide **except the one that asked**, and the settle goes to all of them. It
    /// is a frame rather than an event for `push_frame`'s own reason — a card is about now and is
    /// nobody's history — and selecting by predicate rather than by id list because the daemon
    /// holds the head ids and the predicate IS the rule (*asked*, not *excluded from a list*).
    pub fn push_frame_where(&self, frame: ServerFrame, want: impl Fn(&str) -> bool + Copy) {
        {
            let mut g = self.lock();
            for h in &mut g.heads {
                if !want(&h.id) {
                    continue;
                }
                if h.side.len() >= MAX_SIDE {
                    h.side.pop_front();
                }
                h.side.push_back(frame.clone());
            }
        }
        self.cv.notify_all();
    }

    /// **A frame to ONE head**, for the refusal that belongs to the head that asked and to no
    /// other — a `Rejected` carries no addressee, and sending one to every head would tell the
    /// person's screen about a machine's request.
    pub fn push_frame_to(&self, head_id: &str, frame: ServerFrame) {
        self.push_frame_where(frame, |id| id == head_id);
    }

    /// **The heads that may answer a card**, except the one that asked — the answer to *whom do
    /// we put this in front of*, in the words `Hub::deciding_heads` already uses.
    ///
    /// Empty means nobody attached can answer: a request to open `allow-all` then has no head to
    /// route to, and the asking program is told so rather than left waiting for a person who is
    /// not there. The asker is excluded because a program that could answer its own request would
    /// be consenting for the person the request exists to consult.
    pub fn deciding_heads_except(&self, except: &str) -> Vec<String> {
        self.lock()
            .heads
            .iter()
            .filter(|h| h.caps.can_decide && h.id != except)
            .map(|h| h.id.clone())
            .collect()
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

            // 4. **The held counts, said again** (R91.12), after the snapshot or the gap, so the
            //    new head's first event after either is the count as it stands now.
            if let Some(held) = g.held_now.clone() {
                g.append_and_fan(held);
            }
            // **And what the session is waiting on, for the same reason and in the same place**
            // (52): the event is ephemeral, so a head that attached or reconnected without it would
            // draw a session idle that a turn is running in.
            if let Some(activity) = g.activity_now.clone() {
                g.append_and_fan(activity);
            }
            // **And the operation the session is in the middle of**, when it announced one: a
            // restore says `0 of N rows` before the worker can reach it, and the head that
            // attaches in that window is the one whose empty snapshot needs the sentence most.
            if let Some(filling) = g.filling_now.clone() {
                g.append_and_fan(filling);
            }

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
                // The held counts again, after the snapshot (R91.12): the queue that may have
                // carried the last change was just thrown away.
                if let Some(held) = g.held_now.clone() {
                    g.append_and_fan(held);
                }
                // And the activity, for the reason the attach arm gives (52).
                if let Some(activity) = g.activity_now.clone() {
                    g.append_and_fan(activity);
                }
                // And the operation in flight, for the attach arm's third reason.
                if let Some(filling) = g.filling_now.clone() {
                    g.append_and_fan(filling);
                }
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

    /// **Install what this session's harness holds** — see [`FootprintSource`]. Called once, by
    /// the harness at open, and never removed: a session this hub belongs to lives for the
    /// daemon's life.
    ///
    /// A second registration replaces the first. Nothing depends on that, and it is the honest
    /// reading — the later registration is the harness that is now serving the session.
    pub fn set_footprint(&self, source: FootprintSource) {
        *self
            .footprint_source
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(source);
    }

    /// **What this session's harness holds, now** — all zeroes for a hub nobody registered a
    /// footprint on (every hub built below a harness: a test's, a head's).
    ///
    /// Cheap by construction: the harness restates the transcript and the region into atomics at
    /// every point they change, and this reads them and takes the trail's lock for the length of
    /// a `Vec` walk over at most [`Footprint`]'s trail cap.
    pub fn footprint(&self) -> Footprint {
        let source = self
            .footprint_source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        source.map(|s| s()).unwrap_or_default()
    }

    /// See [`HubCensus`]. Takes the hub's lock for the count and nothing else.
    ///
    /// **The footprint is read after that lock is dropped.** Its source takes the harness's
    /// trail lock, which a turn holds for the length of a write — so reading it under this one
    /// would put a lock a turn takes inside the hub's, which is the cycle [`ActivitySource`]
    /// documents at length.
    pub fn census(&self) -> HubCensus {
        let g = self.lock();
        let (log_events, log_bytes) = g.log.held();
        let bounds = g.log.bounds();
        let out = HubCensus {
            session_id: g.log.session_id().to_string(),
            log_events,
            log_bytes,
            log_bound_bytes: bounds.scrollback_bytes,
            view: g.view.census(),
            heads: g.heads.len(),
            head_queue_max: g.heads.iter().map(|h| h.queue.len()).max().unwrap_or(0),
            head_queue_bound: bounds.max_gap_events,
            head_side_max: g.heads.iter().map(|h| h.side.len()).max().unwrap_or(0),
            commands: g.commands.len(),
            operator_calls: g.operator_calls.len(),
            secrets: g.secrets.len(),
            screens: g.screens.len(),
            footprint: Footprint::default(),
        };
        drop(g);
        HubCensus {
            footprint: self.footprint(),
            ..out
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

            // **A row's question is answered by somebody who may decide**, for the reason an open
            // decision is: a read-only head is watching, and an answer is attributed to a person.
            // Everything else about it — does the row exist, does it still ask this, is the
            // answer one the row offered — is the board's to say, and the board is the harness's.
            //
            // **A mode's consent too** (54), and this check is the same one for the same reason:
            // a watching head is not a person who may open `allow-all` on somebody's session. The
            // other half — *was this head ASKED* — cannot be decided here, because the pending
            // request lives in the harness; `Harness::answer_mode_confirm` refuses one from a
            // head the card never reached.
            if matches!(
                kind,
                CommandKind::AnswerTodo { .. } | CommandKind::ModeConsent { .. }
            ) && !can_decide
            {
                return ServerFrame::Rejected {
                    client_request_id,
                    reason: REJECT_READ_ONLY.into(),
                    expected_seq,
                    actual_seq: actual,
                };
            }
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
                (CommandKind::SetOperatorTodos { items, moved }, _) => {
                    // The moves are counted too: a frame whose whole content is one state move is a
                    // frame the operator is owed an answer about, and "0 todo(s) sent" would read
                    // as *nothing happened*.
                    match (items.len(), moved.len()) {
                        (n, 0) => format!("the operator's {n} todo(s) sent"),
                        (0, m) => format!("the operator's {m} row(s) moved"),
                        (n, m) => format!("the operator's {n} todo(s) sent and {m} row(s) moved"),
                    }
                }
                (CommandKind::Promote, _) => "background requested".into(),
                (CommandKind::Answer { reply, .. }, _) => {
                    format!("{} answered", reply.as_str())
                }
                (CommandKind::AnswerTodo { content, .. }, _) => {
                    format!("the operator answered the question on `{}`", content.trim())
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
                // **Stale-tolerant, because the head that asked while the screen moved
                // still meant it.** Neither of these is a mutation of its own: the ask puts
                // a question in front of somebody, and the answer settles a question whose
                // identity is the `req_id` — a seq that has moved since says nothing about
                // whether that consent is still the one being waited for, and the pending
                // record is what decides that.
                (CommandKind::ModeConfirm { name }, true) => {
                    format!("{REJECT_STALE_SEQ}: asked anyway — the `{name}` confirmation")
                }
                (CommandKind::ModeConfirm { name }, false) => {
                    format!("asked for the `{name}` confirmation")
                }
                (CommandKind::ModeConsent { yes, .. }, _) => if *yes {
                    "the confirmation is answered"
                } else {
                    "the confirmation is refused"
                }
                .into(),
                // **Redacted, because this note is appended to the session log.** The
                // model picker's key card sent `/models P --key K` here, and the key went
                // into the log in plaintext and back to every head in `Accepted`. The
                // worker still gets the line whole — `CommandKind` is not persisted.
                (CommandKind::Slash { line }, _) => format!("/{}", redact_key_args(line)),
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
                // **Never submitted by a head** — no frame produces this kind. It is the
                // daemon's own finished work coming back (`Hub::submit_daemon`), which does
                // not pass through here; if it ever did, the announcement says what it is
                // rather than refusing, because the rows are the record and a refusal would
                // lose them.
                (CommandKind::OperatorShellResult { call_id, .. }, _) => {
                    format!("the operator's shell run `{call_id}` finished")
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
                    enqueued_ms: crate::event::now_ms(),
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
        // **A command waiting for the worker is a turn the model has not taken yet**
        // (`PROTOCOL_VERSION` 52). Said here because nothing else will: the command queue is hub
        // state and no event announces a push, so a session whose worker is pinned inside ANOTHER
        // session's turn — one worker, many sessions — would read as quiet for as long as that
        // turn ran, and a caller waiting on it would be told the work was done. The value is
        // dropped if it did not move, which is the ordinary case.
        self.refresh_activity();
        frame
    }

    /// **A command the daemon enqueues without a head behind it** — the one caller today
    /// is the operator's finished shell run coming back from its own thread.
    ///
    /// [`Hub::submit`] is the door for commands and it checks exactly what a command from
    /// outside needs checked: that the submitter is an attached head, that a stale seq is
    /// answered by name, that a reply frame goes back. None of that applies here. The
    /// submitter is not a head and must not be made to look like one — it is the worker's
    /// own delegate returning finished work to the queue the worker drains — no head is
    /// waiting for a reply (the line was answered `Accepted` when it was submitted, and the
    /// rows are how the result reaches anybody), and there is no seq question to answer:
    /// the run already happened.
    ///
    /// **No `CommandIssued` announcement.** The act was announced when the line was
    /// submitted; this is that same act completing, and its record is the two rows and the
    /// `operator_shell_ran` note the worker appends on it. Announcing the completion as a
    /// second command would be one fact in two places.
    ///
    /// The bell **is** rung, with [`crate::registry::Ring::Command`] — the same ring a head's
    /// submit uses — so an idle worker picks this up at once and `Bell::next_any`'s ordering
    /// holds: a head that pressed enter while the run was in flight is served before it,
    /// because a command outranks a wake and this is a command.
    ///
    /// `false` for a closed hub: the daemon is going away, the queue is being drained by
    /// nobody, and a result handed to it would be lost either way. The caller says so on its
    /// own log rather than pretending the rows landed.
    pub fn submit_daemon(
        &self,
        identity: impl Into<String>,
        client_request_id: impl Into<String>,
        kind: CommandKind,
    ) -> bool {
        let ring = {
            let mut g = self.lock();
            if g.closed {
                return false;
            }
            // Read before the push: the seq is the log's head at the moment the result
            // was queued, which is what `at_seq` means on every other command.
            let at_seq = g.log.head_seq();
            g.commands.push_back(QueuedCommand {
                head_id: DAEMON_SUBMITTER.to_string(),
                identity: identity.into(),
                client_request_id: client_request_id.into(),
                at_seq,
                enqueued_ms: crate::event::now_ms(),
                kind,
            });
            self.cv.notify_all();
            g.bell.clone().map(|b| (b, g.log.session_id().to_string()))
        };
        // Outside the lock, for the reason `submit` rings outside it.
        if let Some((bell, id)) = ring {
            bell.ring(&id);
        }
        true
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

    /// **Whether a TURN is in flight in this session — which is not what
    /// [`SessionStatus::running`] answers.**
    ///
    /// `running` is read off the view's `TurnState`, and the engine publishes `TurnStarted` and
    /// `TurnFinished` **once per ROUND** (`harness.rs`'s `begin_turn_clock` says so in as many
    /// words: *"Called once per prompt, never per round"* — the stamp is the prompt's, the events
    /// are the round's). So `running` is `false` for the whole of a round's tool calls and for the
    /// boundary between two rounds, which is where a turn spends most of its wall clock: a child
    /// executing a ten-minute build reads as *no turn in flight*, and every reader that means *is
    /// a turn in flight* by it is wrong for the whole of that window.
    ///
    /// A turn IS in flight when the round that produced the calls has finished and a call of it has
    /// not — which is the fact the view already holds (`TurnView::calls` with its `CallState`), so
    /// this is a second reading of the same instant's state and not a second record of it. The
    /// terminal states are the exception and are named rather than folded: an interrupted or failed
    /// turn is over, and the calls it abandoned are not a turn in flight.
    ///
    /// **What this still cannot see is the boundary between a round's last call finishing and the
    /// next round's `TurnStarted`** — a real window, seconds long, in which the daemon is appending
    /// results, persisting and publishing. A reader that needs *is this prompt still going* rather
    /// than *is work in flight* is asking the question the round-level events cannot answer at all;
    /// see the `TurnEnded` row in `TODO.md`.
    pub fn turn_in_flight(&self) -> bool {
        let g = self.lock();
        match g.view.turn() {
            Some(t) => match t.state {
                crate::view::TurnState::Running => true,
                // The round's generation ended and its calls are being run: this is the window the
                // round-level `running` flag cannot see.
                crate::view::TurnState::Finished { .. } => t
                    .calls
                    .iter()
                    .any(|c| !matches!(c.state, crate::view::CallState::Finished { .. })),
                // A turn that was interrupted or failed is over, whatever its calls were left as.
                crate::view::TurnState::Interrupted { .. }
                | crate::view::TurnState::Failed { .. } => false,
            },
            // Nothing has run here yet, or the turn in view is gone: no turn is in flight.
            None => false,
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
        self.take_own_work_until(None)
    }

    /// **`take_own_work` with a clock attached** — give up at `deadline` and say so, for the
    /// reader that has something time-based to do between commands.
    ///
    /// That reader is a subagent's serving thread: a child is a session, so it gets the idle
    /// plan-check (*"childs being just a session should get nags"*), and the check must run on
    /// the thread that owns the harness — a worker that tried to run it directly would be a
    /// second writer. `None` is the old indefinite wait, byte for byte, so a child with nothing
    /// armed costs the same sleep it always had.
    ///
    /// The condvar is `wait_timeout`, recomputed per pass, because a timed wait can return
    /// without being notified; the loop re-checks work first so a command arriving at the
    /// deadline's moment is answered before the clock is.
    pub fn take_own_work_until(&self, deadline: Option<std::time::Instant>) -> OwnWork {
        // **No activity refresh here, and that is deliberate** (52). A command leaving this queue
        // is not *the work is over* — it is the opposite: the worker has it and the turn it makes
        // has not started yet. `work_pending` therefore stays true through that gap (`Delivery`),
        // and what ends it is the turn's own `TurnStarted` or the `Warning` a verb that runs no turn
        // answers with — both of which the hub re-derives the value after.
        self.pop_own_work_until(deadline)
    }

    fn pop_own_work_until(&self, deadline: Option<std::time::Instant>) -> OwnWork {
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
            let Some(at) = deadline else {
                g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
                continue;
            };
            let now = std::time::Instant::now();
            if now >= at {
                return OwnWork::TimedOut;
            }
            let (guard, _) = self
                .cv
                .wait_timeout(g, at - now)
                .unwrap_or_else(|e| e.into_inner());
            g = guard;
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
    ///
    /// **Exactly two kinds, and they are the two the daemon may submit at all** — the
    /// `DAEMON_SUBMITTER` arm of [`Hub::submit`] admits an `Interrupt` and a relayed `Message`
    /// and nothing else, so this door is as wide as that one and no wider. The message half
    /// arrived with the between-turns fix: since a queued message is a turn the CHILD's own
    /// thread runs, a worker that won one would be the second writer this tree keeps closing —
    /// and its *"the turn it was meant to steer had ended, so nothing will deliver it"*
    /// sentence would be false, because the child's reader is right there and will run it.
    ///
    /// **And a slash verb, for the ones a child answers itself** — `/nag`, whose clock lives on
    /// the child's own thread and nowhere else (`harnessd/src/nag_pause.rs`), and `/clear`, whose
    /// harness does. Head verbs, not the daemon's, so this is not a widening of what the daemon
    /// may SUBMIT: the worker hands back only those verbs, and a child that takes any other slash
    /// line answers it with `child_door_refused`, exactly as it does when it wins one from the
    /// queue itself.
    pub fn give_back_to_its_own_reader(&self, cmd: QueuedCommand) -> bool {
        if !matches!(
            cmd.kind,
            CommandKind::Interrupt { .. } | CommandKind::Message { .. } | CommandKind::Slash { .. }
        ) {
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
        // Work handed back to this session's own reader is a turn it owes again (52) — the same
        // fact `submit` says for a command.
        self.refresh_activity();
        true
    }

    /// **The worker's pop** — non-blocking, for a worker that also has other things to do.
    ///
    /// `None` for a hub the worker does not own ([`Owner::OwnThread`]), whatever is queued: the
    /// command is the session's own thread's, which [`Hub::submit`] already woke (it notifies
    /// this hub's condvar as well as ringing the bell), and a worker that took it would be the
    /// second writer [`Owner`] exists to make unspellable. The bell's ring for it is then a
    /// ring with nothing behind it, which `Registry::next_work_until` already skips.
    pub fn try_command(&self) -> Option<QueuedCommand> {
        if self.owner() != Owner::Worker {
            return None;
        }
        // See `take_own_work_until`: a command taken is not a delivery finished.
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
                    | CommandKind::AnswerTodo { .. }
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

    /// **Has the operator asked for a stop this turn has not taken yet?** — without taking it.
    ///
    /// [`Self::has_queued_prompt`]'s shape one variant along, and for the same reason: a caller
    /// deciding whether to hand the work on must be able to LOOK. The caller is
    /// `Harness::take_fallback_continuation` — a fallback the daemon owes a continuation turn
    /// (the operator's 2026-10-12 ruling, *"when it finally applied the turn didnt continue"*)
    /// must not be carried out over a stop the operator pressed at the end of the failed turn:
    /// the interrupt arrived after that turn's last round, so nothing in it would act on it, and
    /// a fresh turn started on top of it is either aborted by it or — worse — ignores it.
    pub fn has_queued_interrupt(&self) -> bool {
        self.lock()
            .commands
            .iter()
            .any(|c| matches!(c.kind, CommandKind::Interrupt { .. }))
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

    /// **The next queued command a RUNNING TURN can act on, chosen by the turn.**
    ///
    /// The fourth of the same picker, and the only one whose predicate is not a
    /// `CommandKind` written here — because for this one the *verb* belongs to
    /// `harnessd`. `/models` rides [`CommandKind::Slash`] as the operator's own line,
    /// and its grammar lives in one place (`harnessd`'s `slash::parse`); a picker that
    /// spelled the verb here — even a single `== "models"` — would be a second copy of
    /// that grammar, and the two copies would drift the first time an alias was added.
    /// So the turn passes the question and this takes the answer.
    ///
    /// # What it is for, and the measurement
    ///
    /// MEASURED 2026-10-12, the operator's GLM coding plan, quota exhausted: the
    /// retry ladder took the round again six times over 63 s against an endpoint
    /// answering `http 429: Weekly/Monthly Limit Exhausted`, they typed `/models
    /// deepseek/deepseek-flash` while those backoffs were cycling, and their words on
    /// it are the whole defect — *"the model change didnt take"* and then *"when it
    /// finally applied the turn didnt continue"*. It did not take because the command
    /// sat in this queue until the worker came back, which is after the turn; the turn
    /// was already FAILED by then. That is `try_mode_command`'s defect exactly, one
    /// verb along.
    ///
    /// # The contract on `want`
    ///
    /// It is called on every queued command, in order, and **must be pure**: it is a
    /// question about whether the turn can act on a command, and the command it
    /// answers yes to is taken and applied by the caller. A predicate that resolves a
    /// `/models` line has to do it without storing the key the line carries — see
    /// `slash::mid_turn_model_switch`, which is the one written so far.
    ///
    /// A command the predicate says no to stays where it is, in order, for the
    /// worker — which is the honest outcome for a verb this turn cannot carry out
    /// (a switch that would re-seat the conversation under a turn that is in flight)
    /// and is exactly what happened to every such command before this existed.
    pub fn try_command_for_running_turn(
        &self,
        want: impl Fn(&CommandKind) -> bool,
    ) -> Option<QueuedCommand> {
        let mut g = self.lock();
        let i = (0..g.commands.len()).find(|&i| want(&g.commands[i].kind))?;
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
        if matches!(event, SessionEvent::HeldResults { .. }) {
            self.held_now = Some(event.clone());
        }
        // **The last activity, kept for the head that was not listening** — see
        // [`Inner::activity_now`]. Recorded here, where every publish passes, rather than in
        // [`Hub::set_activity`], so a value replayed from a log read back into a hub is kept too.
        if matches!(event, SessionEvent::ActivityChanged { .. }) {
            self.activity_now = Some(event.clone());
        }
        // **The last fill, kept the same way and for the same kind of reason**: it is ephemeral
        // and the head that needs it most is the one that was not attached yet.
        //
        // **And kept only while it is INCOMPLETE.** A finished fill has nothing left to say — the
        // head clears its own line on `done == total` — so re-saying one to a later attach would
        // be re-telling an operation that is over. The completeness is the whole of the test:
        // `Harness::open` ends the restore with a tick that says `N of N`, which is what takes
        // the line out of here (where it lands in the failed-open case is said on the field,
        // beside the `resume_failed` warning that names the reason).
        if let SessionEvent::Filling { done, total, .. } = &event {
            self.filling_now = (*done < *total).then(|| event.clone());
        }
        self.tap.observe(self.log.session_id(), &event);
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

    /// **The footprint is the harness's to answer and the hub's to carry** — R66. Three shapes
    /// that have to be told apart: a hub nobody registered one on (every hub below a harness — a
    /// test's, a head's) answers zeroes and states no ceiling; a registered one answers what the
    /// source says; and it is asked EVERY time, so a session that grows is seen growing rather
    /// than frozen at the value it had when it was registered.
    #[test]
    fn a_hubs_census_carries_what_its_harness_holds() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let hub = Hub::new("s-footprint");
        assert_eq!(hub.census().footprint, Footprint::default());
        assert!(
            !hub.footprint().over_ceiling(),
            "no ceiling was stated, which is not the same fact as inside one"
        );

        let text = std::sync::Arc::new(AtomicUsize::new(1000));
        let read = text.clone();
        hub.set_footprint(std::sync::Arc::new(move || Footprint {
            items: 3,
            items_text_bytes: read.load(Ordering::Relaxed),
            ledger_region_bytes: 4096,
            ledger_tokens: 1024,
            trail_bytes: 10,
            trail_utterances: 1,
            ceiling_bytes: 4 << 20,
        }));
        let c = hub.census();
        assert_eq!(c.footprint.items, 3);
        assert_eq!(c.footprint.items_text_bytes, 1000);
        assert_eq!(c.footprint.ledger_tokens, 1024);
        assert_eq!(c.footprint.trail_utterances, 1);
        assert_eq!(c.footprint.total(), 1000 + 4096 + 10);
        assert!(!c.footprint.over_ceiling());

        text.store(8 << 20, Ordering::Relaxed);
        let grown = hub.census().footprint;
        assert_eq!(grown.items_text_bytes, 8 << 20);
        assert!(
            grown.over_ceiling(),
            "a harness past its own ceiling must say so"
        );
        // And the rest of the census is unaffected by it: the hub's own numbers are the hub's.
        assert_eq!(grown.ceiling_bytes, 4 << 20);
    }

    /// **A head that attaches, reconnects or resyncs is told the held counts as they stand**
    /// (R91.12). `HeldResults` is ephemeral and said only when the counts change, so before this a
    /// head attached mid-turn read nothing held, and a reconnect whose replay scrubbed the zero
    /// kept a stale count.
    #[test]
    fn a_head_that_attaches_or_reconnects_is_told_the_held_counts() {
        let held = |b: &crate::cursor::Batch| -> Vec<(u64, u64, u64)> {
            b.events()
                .iter()
                .filter_map(|e| match e.event {
                    SessionEvent::HeldResults {
                        jobs,
                        subagents,
                        failed,
                    } => Some((jobs, subagents, failed)),
                    _ => None,
                })
                .collect()
        };
        // A marker after each attach, so a head with nothing else queued is not left blocked
        // in `next_batch` — the missing count reads as an assertion, not a hang.
        let mark = |hub: &Hub| {
            hub.publish(SessionEvent::Warning {
                code: "interrupt_idle".into(),
                detail: String::new(),
                compaction: None,
            });
        };
        let hub = Hub::new("s-held");
        hub.publish(SessionEvent::HeldResults {
            jobs: 2,
            subagents: 1,
            failed: 0,
        });

        // A head that attaches after the count was said.
        let late = attach(&hub, 64);
        mark(&hub);
        let Delivery::Events(b) = hub.next_batch(&late.head_id, 64) else {
            panic!("expected events");
        };
        assert_eq!(held(&b), vec![(2, 1, 0)], "the late head was not told");

        // The count goes to zero while a head is away; it reconnects with a replay, which
        // scrubs the ephemeral zero, and must still be told it.
        let since = hub.snapshot().seq;
        hub.detach(&late.head_id);
        hub.publish(SessionEvent::HeldResults {
            jobs: 0,
            subagents: 0,
            failed: 0,
        });
        let back = hub.attach("tui", "dead@lab2x1", Caps::default(), since);
        assert!(
            back.backlog
                .iter()
                .all(|e| !matches!(e.event, SessionEvent::HeldResults { .. })),
            "the premise: the replay scrubs the held counts"
        );
        mark(&hub);
        let Delivery::Events(b) = hub.next_batch(&back.head_id, 64) else {
            panic!("expected events");
        };
        assert_eq!(
            held(&b),
            vec![(0, 0, 0)],
            "the reconnected head was not told"
        );

        // And a head that takes a resync.
        hub.request_resync(&back.head_id, "test");
        let Delivery::Resync { .. } = hub.next_batch(&back.head_id, 64) else {
            panic!("expected a resync");
        };
        mark(&hub);
        let Delivery::Events(b) = hub.next_batch(&back.head_id, 64) else {
            panic!("expected events");
        };
        assert_eq!(held(&b), vec![(0, 0, 0)], "the resynced head was not told");
    }

    /// **A head that attaches in the middle of a named operation is told which one** — the same
    /// rule `HeldResults` and `ActivityChanged` follow, and the longest wait of the three.
    ///
    /// MEASURED 2026-10-26 (`leticode --session ID`, a daemon already up): a session is
    /// registered the instant a head asks for it and opened when the worker reaches it, so for
    /// however long the turn in front of it runs, a head attached to that session is holding an
    /// empty snapshot. `Filling` is ephemeral and in no snapshot, so without this the one
    /// sentence that says *a restore is coming* never reached the head that needed it.
    #[test]
    fn a_head_that_attaches_mid_operation_is_told_which_one() {
        let fills = |b: &crate::cursor::Batch| -> Vec<(String, u64, u64)> {
            b.events()
                .iter()
                .filter_map(|e| match &e.event {
                    SessionEvent::Filling {
                        what, done, total, ..
                    } => Some((what.clone(), *done, *total)),
                    _ => None,
                })
                .collect()
        };
        let mark = |hub: &Hub| {
            hub.publish(SessionEvent::Warning {
                code: "interrupt_idle".into(),
                detail: String::new(),
                compaction: None,
            });
        };
        let hub = Hub::new("s-filling");
        hub.publish(SessionEvent::Filling {
            what: crate::RESTORING_ROWS.into(),
            unit: "rows".into(),
            done: 0,
            total: 166,
        });

        let late = attach(&hub, 64);
        mark(&hub);
        let Delivery::Events(b) = hub.next_batch(&late.head_id, 64) else {
            panic!("expected events");
        };
        assert_eq!(
            fills(&b),
            vec![(crate::RESTORING_ROWS.to_string(), 0, 166)],
            "the head was handed an empty snapshot and not told why"
        );

        // **And a finished operation is not re-announced**: the restore's own last tick says
        // `166 of 166`, and a head attaching afterwards must not be told that a restore which
        // is over is still running.
        hub.publish(SessionEvent::Filling {
            what: crate::RESTORING_ROWS.into(),
            unit: "rows".into(),
            done: 166,
            total: 166,
        });
        let after = attach(&hub, 64);
        mark(&hub);
        let Delivery::Events(b) = hub.next_batch(&after.head_id, 64) else {
            panic!("expected events");
        };
        assert_eq!(
            fills(&b),
            Vec::new(),
            "a completed fill has nothing left to say"
        );
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
            enqueued_ms: 0,
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

        // **And a parent's message, which is the second half of the same fact.** A message
        // for a child is no longer *"a turn in flight or nothing"*: the child's own reader
        // starts a turn for it, so the worker running this arm's sentence instead would be
        // both the second writer and a lie. Same door, same reader.
        let message = QueuedCommand {
            head_id: DAEMON_SUBMITTER.to_string(),
            identity: "s-parent".into(),
            client_request_id: "task_message-1".into(),
            at_seq: hub.head_seq(),
            enqueued_ms: 0,
            kind: CommandKind::Message {
                from: "s-parent".into(),
                text: "stop and report what you have".into(),
            },
        };
        assert!(hub.give_back_to_its_own_reader(message));
        match hub.take_own_work() {
            OwnWork::Command(c) => assert!(
                matches!(c.kind, CommandKind::Message { .. }),
                "the message is back in front of the session's own reader: {c:?}"
            ),
            other => panic!("expected the message back, got {other:?}"),
        }
        assert!(quiet(&r), "and still nothing for the daemon's worker");

        // Only those two go back: a prompt is work the daemon CAN serve, by opening the
        // session, and putting one back would take a head's own words away from it.
        let prompt = QueuedCommand {
            head_id: "h1".into(),
            identity: "dead@lab2x1".into(),
            client_request_id: "c1".into(),
            at_seq: hub.head_seq(),
            enqueued_ms: 0,
            kind: CommandKind::Prompt {
                text: "hello".into(),
                attachments: Vec::new(),
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
                attachments: Vec::new(),
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
                attachments: Vec::new(),
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
                    attachments: Vec::new(),
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

    /// **The turn's own picker takes what it asks for and nothing else** — the door
    /// `/models` reaches a retrying turn through.
    ///
    /// MEASURED 2026-10-12: the operator typed `/models deepseek/deepseek-flash` while a
    /// 63-second backoff ladder was cycling, and it sat in this queue until the worker came
    /// back — which is after the turn, by which time the turn was FAILED. *"while those
    /// backoffs were cycling - the model change didnt take"*, and then *"when it finally
    /// applied the turn didnt continue"*.
    ///
    /// Three things this pins, and the middle one is the reason the predicate exists at
    /// all:
    ///
    /// 1. a command the predicate says yes to is taken, here, mid-turn;
    /// 2. **a command it says no to stays in the queue, in order, for the worker** — a
    ///    switch that would re-seat the conversation is not this door's to take, and
    ///    dropping it would be worse than leaving it;
    /// 3. it is taken once, and the queue behind it is undisturbed.
    #[test]
    fn the_turns_own_picker_takes_what_it_asks_for_and_leaves_the_rest() {
        let hub = Hub::new("s");
        let head = hub.attach("tui", "dead", Caps::default(), 0);

        // In front of the one the turn wants, and refused by its predicate.
        hub.submit(
            &head.head_id,
            "c1",
            0,
            CommandKind::Slash {
                line: "models dense78".into(),
            },
        );
        hub.submit(
            &head.head_id,
            "c2",
            0,
            CommandKind::Slash {
                line: "models glm/glm-4.6".into(),
            },
        );
        // And one behind it, for the same reason the mode test keeps one: the queue is
        // not drained by taking from the middle of it.
        hub.submit(&head.head_id, "c3", 0, CommandKind::Compact);

        let want = |k: &CommandKind| matches!(k, CommandKind::Slash { line } if line == "models glm/glm-4.6");
        let got = hub
            .try_command_for_running_turn(want)
            .expect("the switch the turn can apply");
        match got.kind {
            CommandKind::Slash { line } => assert_eq!(line, "models glm/glm-4.6"),
            other => panic!("took the wrong command: {other:?}"),
        }
        // Once.
        assert!(hub.try_command_for_running_turn(want).is_none());

        // **The refused one is still there, and still first.** Order is the promise a
        // person reads off the queue, and a picker that reordered it would apply their two
        // switches backwards.
        let first = hub
            .try_command()
            .expect("the refused switch is still queued");
        match first.kind {
            CommandKind::Slash { line } => assert_eq!(line, "models dense78"),
            other => panic!("the queue was reordered: {other:?}"),
        }
        assert!(
            matches!(
                hub.take_own_work(),
                OwnWork::Command(QueuedCommand {
                    kind: CommandKind::Compact,
                    ..
                })
            ),
            "the compaction behind it was eaten"
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

/// **A slash line with its key arguments masked**: the value after `--key` or `--api-key`
/// (or in `--key=VALUE`) becomes `•••`. For anything that persists or echoes a command line.
pub fn redact_key_args(line: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut mask_next = false;
    for word in line.split(' ') {
        if mask_next && !word.is_empty() {
            out.push("•••".into());
            mask_next = false;
            continue;
        }
        match word.split_once('=') {
            Some((flag @ ("--key" | "--api-key"), _)) => out.push(format!("{flag}=•••")),
            _ => {
                if word == "--key" || word == "--api-key" {
                    mask_next = true;
                }
                out.push(word.to_string());
            }
        }
    }
    out.join(" ")
}

#[cfg(test)]
mod redact_tests {
    use super::redact_key_args;

    #[test]
    fn a_key_argument_never_survives_into_the_note() {
        assert_eq!(
            redact_key_args("models deepseek --key sk-abc123"),
            "models deepseek --key •••"
        );
        assert_eq!(
            redact_key_args("models glm --api-key=xyz"),
            "models glm --api-key=•••"
        );
        assert_eq!(
            redact_key_args("models grok  --key  k"),
            "models grok  --key  •••"
        );
        assert_eq!(redact_key_args("models deepseek"), "models deepseek");
        assert!(!redact_key_args("models x --key sk-SECRET y").contains("SECRET"));
    }
}
