//! Fan-out, attach, resync, and the serialized command queue. The daemon alone reads the model
//! stream; heads read the log. The pane's byte stream never enters the log. One queue per
//! session, so its turn thread is the only writer to its transcript.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use letibot_transcript::TranscriptItem;

use crate::cursor::{Batch, ReadMark};
use crate::event::{Envelope, SessionEvent, rides_announcement};
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
    /// Frames that are not the record: the pane's byte stream, and anything else that belongs to
    /// this connection now rather than to the conversation.
    Frames(Vec<ServerFrame>),
    /// This head fell behind, or asked. Not an error: reset to the snapshot and continue
    /// from `snapshot.seq + 1`.
    Resync {
        reason: String,
        dropped: u64,
        snapshot: Box<Snapshot>,
        scrubbed: ScrubReport,
    },
    /// The daemon is shutting down, and everything queued for this head has already been
    /// delivered. That second half is why a head can stop on this rather than on a timer.
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

/// The daemon's own name in a submit: no head can have it, because a head's id comes from
/// the `Hello` this hub mints and is never NUL-prefixed. [`Hub::submit`] admits it for an
/// [`CommandKind::Interrupt`] and a relayed [`CommandKind::Message`], and for nothing else.
pub const DAEMON_SUBMITTER: &str = "\0daemon";

/// How many unread pane frames one head may hold before the oldest is dropped. The bytes come
/// from a process painting a screen, so a head that has stopped reading must not grow without a
/// bound.
const MAX_SIDE: usize = 256;

/// A mutating command, after validation, waiting for the session's single command worker:
/// commands are serialized on a per-session queue.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedCommand {
    pub head_id: String,
    pub identity: String,
    pub client_request_id: String,
    /// The log's head seq when the command was accepted.
    pub at_seq: u64,
    /// When the command was accepted, wall-clock ms, so the worker can say how long it sat in
    /// the queue - a stall behind a store read is unreadable without this number.
    pub enqueued_ms: u64,
    pub kind: CommandKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandKind {
    Prompt {
        text: String,
        /// The chips the operator put in the composer, carried as the frame sent them. Converted
        /// at the turn's own seam, so a provider never sees a head's shape.
        attachments: Vec<crate::protocol::Attachment>,
        /// What the operator pointed at in an editor, rendered by the daemon into one user part
        /// after their words. It rides the command and the journal, so a prompt queued across a
        /// daemon restart still carries what the operator was looking at.
        context: Option<crate::protocol::EditorContext>,
    },
    /// Compact the session: one summary turn, then the history is replaced by that summary
    /// through a transcript fork. Queued like a prompt because it is a turn, and accepted on
    /// a stale `expected_seq` because a head that asked while the screen moved still meant it.
    Compact,
    /// Rebuild this conversation's prompt from the tools seated now, forking it onto the new
    /// prefix as a compaction forks onto a new base.
    Reseat {
        /// Summarise as well as re-seating. The default is to carry every item
        /// across. See `ClientFrame::ReseatSession`.
        summarise: bool,
    },
    Interrupt {
        reason: String,
    },
    /// A parent speaking to one of its own subagents while that child is in flight: a live
    /// correction delivered into the turn already running.
    Message {
        from: String,
        text: String,
    },
    /// A head asked to move the running command to the background. The exec backend's wait
    /// loop acts on it, already blocked where the worker cannot reach; the frame rides the
    /// queue only so the between-turns case is announced rather than dropped.
    Promote,
    /// Move this session's project to a named point (`allow-all`, `writes-allowed`, an
    /// opencode name...), persisted in the mode store. Queued like everything else, but
    /// unlike a prompt it starts no turn.
    Mode {
        name: String,
        /// Carried from the frame: the operator confirmed an unconfined `allow-all`. Not
        /// defaulted, so every construction site has to say which one it means.
        consented: bool,
    },
    /// A head asks for the point that needs a person. Not [`CommandKind::Mode`] with
    /// `consented: false`: that command means *set it*, and this one only asks.
    ModeConfirm {
        name: String,
    },
    /// A person's answer to that ask, from the head the card was sent to: accepted only while
    /// the request is open and only from that head.
    ModeConsent {
        req_id: String,
        yes: bool,
    },
    /// An operator's own call, admitted before it runs. It carries what the daemon needs to write
    /// the admission as a person's act.
    OperatorCall {
        call_id: String,
        name: String,
        arguments: String,
        /// The head that asked. Becomes the `who` in `human:<who>` and in the row's
        /// `CallOrigin`, so the two records name the actor the same way.
        who: String,
        /// Who runs it: `true` is the daemon, through the tool this session already seats;
        /// `false` is the head, which then sends `OperatorResult`. Carried unchanged so the
        /// admission and the execution cannot disagree about which it was.
        execute: bool,
    },
    /// What that call produced: appends the `ToolResult` row with its `origin` set.
    OperatorResult {
        call_id: String,
        outcome: letibot_transcript::ToolOutcome,
        payload: String,
    },
    /// The operator's own shell line: a `!` command, run by the daemon. Not the door: that frame
    /// names a tool the head has, and this one is the daemon's own.
    OperatorShell {
        /// The line as submitted, `!` first, already re-checked at the frame.
        line: String,
        /// The head that asked. Becomes the `who` in `CallOrigin::Operator`, so the row and the
        /// door's rows name the actor the same way.
        who: String,
    },
    /// The operator handed this session one skill's body: `/skills NAME`, and the pane's Enter. It
    /// carries the name and what the body is.
    LoadSkill {
        /// The skill, as `ServerFrame::Skills` listed it.
        name: String,
        /// What followed the name on the operator's line: their own prompt, in the same turn.
        then: String,
    },
    /// What the operator's own shell line produced, back from the run's own thread, so the rows
    /// are written where the turn that ran it can see them.
    OperatorShellResult {
        /// The line as submitted, verbatim - the `User` row's text.
        line: String,
        /// Who asked, as [`CommandKind::OperatorShell`] carried it. Becomes the `who` in
        /// `CallOrigin::Operator` and the identity the daemon's own log names.
        who: String,
        /// The call id the worker minted (`bang-<n>`), so the row is the one the run was
        /// started for and no other.
        call_id: String,
        outcome: letibot_transcript::ToolOutcome,
        /// What the model will read, already rendered and capped by the runtime's own spill
        /// policy.
        payload: String,
        /// The spill half, when there was one: the two numbers the worker's size note names,
        /// carried rather than recomputed - a second implementation of the cap is a second
        /// answer.
        spill: Option<ShellSpill>,
    },
    /// A slash command for the daemon: `flowy ...`, `models ...`.
    Slash {
        line: String,
    },
    /// Read a window of one background job's output, for a head's jobs pane. A command rather
    /// than a request-answered frame, because it reaches the worker that owns the exec host.
    ReadJobOutput {
        job: String,
        offset: u64,
    },
    /// A head took its queued prompts back - the operator pulled the line into the composer to
    /// edit it. The running turn's steering poll drops the held text with it; between turns it
    /// is a no-op, because a prompt that got this far is about to run as its own turn.
    WithdrawPrompts,
    /// The operator's half of the todo board, replaced wholesale, plus the state moves they asked
    /// for on rows that are not theirs.
    SetOperatorTodos {
        items: Vec<crate::event::TodoEntry>,
        moved: Vec<crate::event::TodoState>,
    },
    /// A head settled an open request; which kind is [`Reply`]. One variant rather than two
    /// because every consumer that only cares that an answer arrived destructures with `..`,
    /// so a second variant would silently skip those arms.
    Answer {
        req_id: String,
        reply: Reply,
    },
    /// The operator answered the question a todo row waits on. Not an answer to a card: nothing
    /// is blocked on this one, the row simply becomes due.
    AnswerTodo {
        content: String,
        question: String,
        answer: crate::question::QuestionAnswer,
    },
}

/// The spill half of a finished operator run, as the size note needs it: how much the command
/// produced in full, and the hash that names the spill.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellSpill {
    /// What the command produced in full, before the inline cap.
    pub full_bytes: usize,
    /// The locator, as `read_spill hash=...` spells it.
    pub hash: String,
}

/// What a head sent back, and the two things it can be. A permission and a question are one
/// mechanism differing in `kind`, so this is that difference made a type: one `req_id`, two
/// payloads, and no way to pass one where the other is read.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// An adjudication grant or denial, by option id - `AllowOnce`, `RejectOnce`
    /// and the rest live behind that id.
    Permission {
        option_id: String,
        /// The operator's own glob for an *always allow*, when they typed one. `None` means
        /// use the pattern derived from the call, never *match nothing*.
        pattern: Option<String>,
        /// What the operator wants the model told, for `deny_and_tell`. `None` on every other option,
        /// and required on that one.
        note: Option<String>,
    },
    /// A person's answer to a question: a choice, a note on it, or typed text. Deliberately
    /// not expressed with [`crate::event::OptionKind`] - see `question.rs`.
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

/// Where an answer goes when somebody is blocked waiting for it. The command queue is the wrong
/// door: the thread that would drain it is the one blocked.
pub trait AnswerSink: Send + Sync {
    /// Deliver an answer to whoever is waiting for `req_id`, and say whether anybody was. `false`
    /// means the answer reached nobody and the caller must persist it instead.
    fn answer(&self, req_id: &str, identity: &str, reply: &Reply) -> bool;

    /// An interrupt arrived: stop waiting for anything still open. A waiter woken this way has not
    /// been answered, and reports that rather than a made-up reply.
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
            CommandKind::LoadSkill { .. } => "load-skill",
            CommandKind::OperatorShellResult { .. } => "operator-shell-result",
            CommandKind::WithdrawPrompts => "take-back",
            CommandKind::SetOperatorTodos { .. } => "operator todos",
            CommandKind::Promote => "promote",
        }
    }
}

/// What one session's hub holds, for the daemon's memory census: every number is something a
/// bound here claims to keep.
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
    /// What this session's HARNESS holds - the half of a session's cost that is not on the
    /// hub at all. See [`Footprint`]; all zeroes for a hub nobody registered one on.
    pub footprint: Footprint,
}

/// What one session's harness holds in memory: the biggest per-session structures are on the
/// far side of the wire, so the census asks the harness for its own numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Footprint {
    /// Rows the in-memory transcript holds.
    pub items: usize,
    /// Bytes of TEXT those rows carry - [`letibot_transcript::TranscriptItem::bytes`].
    pub items_text_bytes: usize,
    /// Bytes the ledger's token region is mapped at, capacity and not occupancy.
    pub ledger_region_bytes: usize,
    /// Tokens appended to the ledger, the stable prefix included.
    pub ledger_tokens: usize,
    /// Bytes of text the authorisation trail holds.
    pub trail_bytes: usize,
    /// Utterances the authorisation trail holds.
    pub trail_utterances: usize,
    /// The ceiling this session's harness states for [`total`](Self::total): the window's
    /// arithmetic, or a flat tripwire for a session with no window.
    pub ceiling_bytes: usize,
}

impl Footprint {
    /// The three byte terms, summed. See the type's table for what each one counts.
    pub fn total(&self) -> usize {
        self.items_text_bytes + self.ledger_region_bytes + self.trail_bytes
    }

    /// Has this session grown past the ceiling its harness states for itself? A bound checked
    /// rather than a tripwire chosen here.
    pub fn over_ceiling(&self) -> bool {
        self.ceiling_bytes > 0 && self.total() > self.ceiling_bytes
    }
}

/// The command queue. Operator prompts in it are copied to the store (when a journal is set) after
/// every change, so a daemon killed with prompts queued still has them.
#[derive(Default)]
struct CommandQueue {
    q: VecDeque<QueuedCommand>,
    journal: Option<Journal>,
}

/// The queue's side of the journal. The store write runs on the journal's own thread: a store
/// another process holds locked must not hold the hub's lock, which every head and the session's
/// turn need.
struct Journal {
    tx: std::sync::mpsc::Sender<(u64, Vec<JournaledPrompt>)>,
    last: Vec<JournaledPrompt>,
    sent: u64,
    saved: Arc<(Mutex<u64>, Condvar)>,
}

/// Save the newest snapshot, retrying a failed one until a newer snapshot replaces it. Ends when
/// the hub is gone, after the last snapshot is written.
fn run_journal(
    mut sink: Box<dyn PromptJournal>,
    rx: std::sync::mpsc::Receiver<(u64, Vec<JournaledPrompt>)>,
    saved: Arc<(Mutex<u64>, Condvar)>,
    session_id: String,
) {
    use std::sync::mpsc::RecvTimeoutError;
    let Ok(mut want) = rx.recv() else { return };
    loop {
        while let Ok(newer) = rx.try_recv() {
            want = newer;
        }
        match sink.save(&want.1) {
            Ok(()) => {
                let (n, cv) = &*saved;
                *n.lock().unwrap_or_else(|e| e.into_inner()) = want.0;
                cv.notify_all();
                match rx.recv() {
                    Ok(next) => want = next,
                    Err(_) => return,
                }
            }
            Err(e) => {
                eprintln!("  {session_id} · queued prompts not saved: {e}");
                match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                    Ok(next) => want = next,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        let _ = sink.save(&want.1);
                        return;
                    }
                }
            }
        }
    }
}

/// An accepted operator prompt that has not started.
#[derive(Debug, Clone, PartialEq)]
pub struct JournaledPrompt {
    pub enqueued_ms: u64,
    pub head_id: String,
    pub identity: String,
    pub client_request_id: String,
    pub text: String,
    pub attachments: Vec<crate::protocol::Attachment>,
    /// The editor context the prompt carried, kept beside its attachments for the same reason:
    /// a daemon killed mid-queue puts the prompt back with what the operator was looking at.
    pub context: Option<crate::protocol::EditorContext>,
}

/// Where queued prompts are kept across a daemon kill. The daemon implements it over its store, so
/// this crate links no database.
pub trait PromptJournal: Send {
    /// Make the saved queue exactly `prompts`, oldest first.
    fn save(&mut self, prompts: &[JournaledPrompt]) -> Result<(), String>;
}

impl CommandQueue {
    fn push_back(&mut self, c: QueuedCommand) {
        self.q.push_back(c);
        self.sync();
    }
    fn pop_front(&mut self) -> Option<QueuedCommand> {
        let c = self.q.pop_front();
        self.sync();
        c
    }
    fn remove(&mut self, i: usize) -> Option<QueuedCommand> {
        let c = self.q.remove(i);
        self.sync();
        c
    }
    fn len(&self) -> usize {
        self.q.len()
    }
    fn is_empty(&self) -> bool {
        self.q.is_empty()
    }
    fn iter(&self) -> impl Iterator<Item = &QueuedCommand> {
        self.q.iter()
    }
    fn replace(&mut self, q: VecDeque<QueuedCommand>) {
        self.q = q;
        self.sync();
    }
    fn prompts(&self) -> Vec<JournaledPrompt> {
        self.q
            .iter()
            .filter_map(|c| match &c.kind {
                CommandKind::Prompt {
                    text,
                    attachments,
                    context,
                } => Some(JournaledPrompt {
                    enqueued_ms: c.enqueued_ms,
                    head_id: c.head_id.clone(),
                    identity: c.identity.clone(),
                    client_request_id: c.client_request_id.clone(),
                    text: text.clone(),
                    attachments: attachments.clone(),
                    context: context.clone(),
                }),
                _ => None,
            })
            .collect()
    }
    /// Hand the prompts to the journal's thread if they changed. Never waits on the store.
    fn sync(&mut self) {
        let now = self.prompts();
        let Some(j) = &mut self.journal else { return };
        if now == j.last {
            return;
        }
        j.sent += 1;
        let _ = j.tx.send((j.sent, now.clone()));
        j.last = now;
    }
}

impl std::ops::Index<usize> for CommandQueue {
    type Output = QueuedCommand;
    fn index(&self, i: usize) -> &QueuedCommand {
        &self.q[i]
    }
}

struct Head {
    id: String,
    kind: String,
    identity: String,
    caps: Caps,
    queue: VecDeque<Envelope>,
    /// Frames addressed to this head alone and never to the log: the pane's byte stream. Drained
    /// before the record, because a terminal that falls behind is a terminal that is wrong.
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
    /// Admitted operator calls their head has not reported yet, keyed by the head's own
    /// `call_id` -> `(owning head, name, who)`. See [`Hub::detach`].
    operator_calls: std::collections::HashMap<String, (String, String, String)>,
    next_head: u64,
    commands: CommandQueue,
    /// A wake aimed at the thread that owns this session's turn rather than at the daemon's
    /// worker - see [`Hub::wake_its_own_reader`] for why the bell cannot be used for it.
    /// Taken once, by [`Hub::take_own_work`].
    own_wake: bool,
    /// The last `HeldResults` this hub published, republished to a head that attaches or takes a
    /// resync: the event is a state, not a moment.
    held_now: Option<SessionEvent>,
    /// The last `ActivityChanged` this hub published, republished to a head that attaches or takes
    /// a resync, for the same reason.
    activity_now: Option<SessionEvent>,
    /// The last `Filling` this hub published, republished when a head attaches: the fill is the
    /// longest wait of the three, and a head that arrives late must still show it.
    filling_now: Option<SessionEvent>,
    /// The retry series a model call is in, republished to a head that attaches or resyncs.
    /// Without it a head arriving mid-series draws nothing and reads a failing endpoint as fine.
    /// `None` once the series has its ending.
    retry_now: Option<SessionEvent>,
    closed: bool,
    /// Rung when this hub takes a command, so a reader can wait on its own session without a
    /// timer. See [`crate::registry`].
    bell: Option<Arc<crate::registry::Bell>>,
    /// Where a settled decision goes instead of the command queue. See
    /// [`AnswerSink`] for why the queue cannot carry it.
    answers: Option<Arc<dyn AnswerSink>>,
    /// Open password requests: `req_id` -> the channel to the `askpass` connection
    /// waiting on it. The secret goes through the channel and nowhere else - not the log,
    /// not the view, not a `CommandIssued`.
    secrets: HashMap<String, std::sync::mpsc::SyncSender<Option<String>>>,
    next_secret: u64,
    /// Open screen requests: `req_id` -> the tool call waiting for a head to draw itself.
    /// First answer wins; later ones find nothing and are dropped.
    screens: HashMap<String, std::sync::mpsc::SyncSender<(usize, usize, Vec<String>)>>,
    next_screen: u64,
    /// The metrics tap: the session's "now" for a mark, and the rows the hub already carries.
    /// See [`crate::metrics_tap`].
    tap: crate::metrics_tap::Tap,
}

/// How the daemon above a hub computes what the session is waiting on. A closure rather than a
/// method, because the answer needs the harness and this crate has no harness.
pub type ActivitySource = Arc<dyn Fn(&Hub) -> crate::activity::Activity + Send + Sync>;

/// How the daemon above a hub says what this session's harness holds. A closure like
/// [`ActivitySource`], and for its reason.
pub type FootprintSource = Arc<dyn Fn() -> Footprint + Send + Sync>;

/// What a session looks like from outside it: enough for a picker, and cheap enough to compute
/// for every session on every list.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionStatus {
    pub session_id: String,
    /// The head seq: how much has happened here.
    pub seq: u64,
    /// Transcript rows the view is holding.
    pub items: usize,
    /// Heads attached right now. Zero is normal and is not a reason to stop anything: idle is
    /// quiet, not unwatched.
    pub heads: usize,
    /// A turn is generating in this session at this instant.
    pub running: bool,
    /// The model the last turn named, or empty before the first turn.
    pub model: String,
    /// `Envelope::ts` of the last event. Zero before anything has happened.
    pub last_ms: u64,
    /// Room lines this session's pen holds: what it has been told and its model has not been put
    /// in front of yet. Read off the last `HeldResults` the hub published, so it is zero for a
    /// session whose daemon has never held anything.
    #[serde(default)]
    pub held_rooms: u64,
}

/// What the thread that owns a session's queue found to do. A `Wake` is not a command and
/// carries no text: it means look again, something inside the daemon changed.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // one queued command rides the queue by value; the unit variants cost nothing
pub enum OwnWork {
    /// A command somebody submitted: a head's prompt, a parent's kill, a relayed message.
    Command(QueuedCommand),
    /// Something this session owns has settled and nobody else will tell it.
    Wake,
    /// The hub is closed and the queue is empty: this reader is done.
    Closed,
    /// The deadline the reader itself handed the wait arrived before any work did: a clock the
    /// reader asked for, and not a failure.
    TimedOut,
}

/// Who runs this session's turns: the one reader its command queue has. Two readers would be
/// two writers on one transcript, which the append-only trigger refuses after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Owner {
    /// The daemon's worker: every session a head created or resumed, and the default.
    #[default]
    Worker,
    /// A thread of its own: a subagent, whose harness lives on the thread its parent spawned it
    /// on. Set ([`Hub::owned_by_its_own_thread`]) before the hub is adopted into a registry, so
    /// the worker can never see the session and think it is its own.
    OwnThread,
}

/// One session's log, view, heads and command queue.
pub struct Hub {
    inner: Mutex<Inner>,
    /// Who drains [`Inner::commands`]. See [`Owner`]. Outside the lock because it is written
    /// once, before the hub is reachable, and read on every pop.
    owner: std::sync::atomic::AtomicU8,
    /// One condition for everything. A publish wakes every waiting head and the command
    /// worker, and each re-checks its own predicate: simpler than a condvar per head, and the
    /// wake storm is bounded by the head count, which is small by construction.
    cv: Condvar,
    /// A head asked to move the running command to the background, and this is the identity of
    /// whoever asked. `None` when no request is pending. Shared with the exec backend so the
    /// `bash` tool's wait loop honours it while the worker is blocked inside that wait.
    promote: Arc<Mutex<Option<String>>>,
    /// What this session is waiting on, as it was last published - see [`Hub::set_activity`].
    /// Outside [`Inner`] on purpose: the compare-and-publish must not run under the lock every
    /// head's fan-out takes.
    activity: Mutex<crate::activity::Activity>,
    /// How the daemon above this hub computes the value - see [`Hub::refresh_activity`].
    /// Registered once, and given the hub it is about so the two cannot disagree about which
    /// session they mean.
    activity_source: Mutex<Option<ActivitySource>>,
    /// How the daemon above this hub says what the HARNESS holds - see [`FootprintSource`].
    /// Registered once, by the session's own harness at open.
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
                commands: CommandQueue::default(),
                own_wake: false,
                held_now: None,
                activity_now: None,
                filling_now: None,
                retry_now: None,
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

    /// Keep this session's queued operator prompts in `sink`, after putting back `stored` - the
    /// ones a dead daemon left. Returns how many were put back. Idempotent: a second call finds
    /// the journal set and does nothing.
    pub fn journal_prompts(
        &self,
        sink: Box<dyn PromptJournal>,
        stored: Vec<JournaledPrompt>,
    ) -> usize {
        let mut g = self.lock();
        if g.commands.journal.is_some() {
            return 0;
        }
        let at_seq = g.log.head_seq();
        let n = stored.len();
        for p in &stored {
            g.commands.q.push_back(QueuedCommand {
                head_id: p.head_id.clone(),
                identity: p.identity.clone(),
                client_request_id: p.client_request_id.clone(),
                at_seq,
                enqueued_ms: p.enqueued_ms,
                kind: CommandKind::Prompt {
                    text: p.text.clone(),
                    attachments: p.attachments.clone(),
                    context: p.context.clone(),
                },
            });
        }
        let session_id = g.log.session_id().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let saved = Arc::new((Mutex::new(0u64), Condvar::new()));
        let for_thread = saved.clone();
        let spawned = std::thread::Builder::new()
            .name("prompt-journal".into())
            .spawn(move || run_journal(sink, rx, for_thread, session_id));
        if spawned.is_err() {
            return n;
        }
        g.commands.journal = Some(Journal {
            tx,
            last: stored,
            sent: 0,
            saved,
        });
        drop(g);
        if n > 0 {
            self.cv.notify_all();
        }
        n
    }

    /// Wait until the journal has written every change handed to it, up to `within`. True when it
    /// has, or when there is no journal.
    pub fn journal_settled(&self, within: std::time::Duration) -> bool {
        let (want, saved) = {
            let g = self.lock();
            match &g.commands.journal {
                Some(j) => (j.sent, j.saved.clone()),
                None => return true,
            }
        };
        let (n, cv) = &*saved;
        let g = n.lock().unwrap_or_else(|e| e.into_inner());
        let (g, _) = cv
            .wait_timeout_while(g, within, |done| *done < want)
            .unwrap_or_else(|e| e.into_inner());
        *g >= want
    }

    /// Mark this session as served by the thread that is opening it, not by the worker. See
    /// [`Owner`]. Called before the hub is adopted.
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

    /// Resolve a head's identity and record the promote request, so the outcome can attribute
    /// who asked. `None` when the head is not attached.
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

    /// Who a head is, by id: the identity the gate records in `human:<who>`. `None` for a head
    /// this hub is not holding, which is what a stopped head looks like.
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

    /// Register this hub with a cross-session wake, so a worker serving several sessions blocks
    /// on one condition instead of polling.
    pub fn set_bell(&self, bell: Arc<crate::registry::Bell>) {
        self.lock().bell = Some(bell);
    }

    /// Install the answer sink, so a settled decision reaches whoever is blocked on it rather
    /// than the queue that whoever-is-blocked would have to drain.
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

    /// Ask every attached head to draw itself; the first head to answer wins. The daemon cannot
    /// answer this itself: it holds no screen.
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

    /// A head answered. `false` when nothing was waiting - the request timed out, or another
    /// head was faster, and neither is an error.
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
        // A poisoned lock means a head thread panicked while holding it. The log is still
        // consistent - every mutation under this lock is infallible - so recovering is strictly
        // better than taking the whole session down with it.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn session_id(&self) -> String {
        self.lock().log.session_id().to_string()
    }

    /// One row's body by its session ordinal, for a head reading a window of it: the view's, under
    /// the view's lock.
    pub fn row_body_at(&self, row: usize) -> Option<String> {
        self.lock().view.row_body_at(row)
    }

    /// One whole row by the same ordinal, for a head scrolling above its window.
    pub fn row_at(&self, row: usize) -> Option<crate::view::SnapshotItem> {
        self.lock().view.row_at(row)
    }

    pub fn head_seq(&self) -> u64 {
        self.lock().log.head_seq()
    }

    /// Where an operator's mark lands: the turn running (or the last one), its round and what it
    /// is doing; or, for a row they were not in, nothing.
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

    /// The one door for a warning: `(code, detail)` and nothing else, so every site says the same
    /// shape and a head's card, the store's line and the daemon's log cannot be told two ways.
    /// `warn_with` is the same door for the few that carry a compaction report.
    pub fn warn(&self, code: impl Into<String>, detail: impl Into<String>) -> Envelope {
        self.warn_with(code, detail, None)
    }

    /// [`Hub::warn`] for a warning that carries what a compaction did: one construction of the
    /// variant in this crate, so the three fields cannot be filled in two orders by two callers.
    ///
    /// A developer's diagnostic leaves a line here as well: a head draws it once per session and
    /// `/notes` lists it, and the numbers themselves belong where they were measured.
    pub fn warn_with(
        &self,
        code: impl Into<String>,
        detail: impl Into<String>,
        compaction: Option<Box<crate::event::CompactionReport>>,
    ) -> Envelope {
        let (code, detail) = (code.into(), detail.into());
        if let Some(line) = crate::warning::developer_log(&code, &detail) {
            eprintln!("{line}");
        }
        self.publish(SessionEvent::warning_with(code, detail, compaction))
    }

    /// Append an event and fan it out. Never blocks on a head.
    pub fn publish(&self, event: SessionEvent) -> Envelope {
        // What the session is waiting on is re-derived after anything that could have moved it,
        // and it costs nothing when nothing did: the comparison is against the last published value.
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

    /// What this session is waiting on, as it was last published - the same value a head has.
    pub fn activity(&self) -> crate::activity::Activity {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Publish `ActivityChanged` if, and only if, the value changed: the one place that comparison
    /// happens, so no caller can flood a head with the same activity.
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

    /// Register how the daemon computes the value - see [`ActivitySource`]. Called once by
    /// whoever owns the session; a second call replaces the first, which a re-opened session
    /// needs.
    pub fn set_activity_source(&self, source: ActivitySource) {
        *self
            .activity_source
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(source);
    }

    /// Re-derive the value and publish it if it moved: the daemon's own state is what the source
    /// reads, so this is how a change nothing announced still reaches a head.
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

    /// A settlement, a monitor's firing, or a prompt is owed to this session's model. Both
    /// halves are the model's work, and a head shows them rather than acting on them.
    pub fn work_pending(&self) -> bool {
        let g = self.lock();
        !g.commands.is_empty() || g.own_wake
    }

    /// The turn and its calls as the view sees them right now: (a round is generating, a call of
    /// this round is still running). The two entries of [`crate::activity::Waiting`] that come
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
                // Terminated, whatever its calls were left as - [`Hub::turn_in_flight`]'s rule.
                crate::view::TurnState::Interrupted { .. }
                | crate::view::TurnState::Failed { .. } => (false, false),
            },
            None => (false, false),
        }
    }

    /// The cards still owed an answer, oldest first - the one entry of
    /// [`crate::activity::Waiting`] that does not end by itself.
    pub fn open_ask_ids(&self) -> Vec<String> {
        self.lock()
            .view
            .open_decisions()
            .iter()
            .map(|d| d.req_id.clone())
            .collect()
    }

    /// A frame that is not the record, fanned out to every head of this session. It cannot be
    /// [`Hub::publish`]: an event is a row and this is a moment.
    pub fn push_frame(&self, frame: ServerFrame) {
        self.push_frame_where(frame, |_| true);
    }

    /// A frame to the heads a predicate names, the same door [`Hub::push_frame`] is. A card goes
    /// to every head that may decide it.
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

    /// A frame to ONE head, for the refusal that belongs to the head that asked: a `Rejected`
    /// carries no addressee, and sending one to every head would tell a person's screen about a
    /// machine's request.
    pub fn push_frame_to(&self, head_id: &str, frame: ServerFrame) {
        self.push_frame_where(frame, |id| id == head_id);
    }

    /// The heads that may answer a card, except the one that asked. Empty means nobody attached
    /// can answer, and the asking program is told that rather than waiting.
    pub fn deciding_heads_except(&self, except: &str) -> Vec<String> {
        self.lock()
            .heads
            .iter()
            .filter(|h| h.caps.can_decide && h.id != except)
            .map(|h| h.id.clone())
            .collect()
    }

    /// Attach content to an already-appended row the announcement could not carry. A row that
    /// rode its announcement is not stored again; a head that read no body is given
    /// `TranscriptContent` at send time (see [`for_head`]).
    pub fn record_item(&self, item_id: &str, item: TranscriptItem) {
        if self.lock().view.row_has_body(item_id) {
            return;
        }
        self.publish(SessionEvent::TranscriptContent {
            item_id: item_id.to_string(),
            item: Box::new(item),
        });
    }

    /// One row, announced and readable: the announcement carries the body when it fits, and
    /// [`Hub::record_item`] supplies it for a row that does not. Every door that publishes a row it
    /// already holds goes through here, so the bound cannot be applied twice and differently.
    pub fn append_row(&self, item_id: &str, kind: &str, ledger_head: &str, item: &TranscriptItem) {
        self.publish(SessionEvent::TranscriptAppended {
            item_id: item_id.to_string(),
            kind: kind.to_string(),
            ledger_head: ledger_head.to_string(),
            item: rides_announcement(item).then(|| Box::new(item.clone())),
            body_bytes: item.bytes() as u64,
        });
        self.record_item(item_id, item.clone());
    }

    /// Every event still in the scrollback, in seq order: the session as a record, which is what
    /// a replay consumes.
    pub fn retained(&self) -> Vec<Envelope> {
        self.lock().log.retained().cloned().collect()
    }

    /// A snapshot of the current state, for a head that is not attached (a test, a
    /// `harnessctl status`).
    pub fn snapshot(&self) -> Snapshot {
        let g = self.lock();
        g.view.snapshot(g.log.head_seq(), g.log.dropped())
    }

    /// ATTACH: snapshot-or-gap, decided and delivered under one lock. The `HeadAttached` event is
    /// appended before the snapshot, so the head's own arrival is in the record.
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
                        // Everything replayed is scrubbed: live and stored are different
                        // artifacts of the same stream, and a resume is a replay.
                        let (kept, report) = scrub_replay(g.log.retained(), &gap);
                        let kept = kept.into_iter().flat_map(|e| for_head(&caps, e)).collect();
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

            // 4. The held counts, said again after the snapshot or the gap, so the new head's
            //    first event after either is the count as it stands now.
            if let Some(held) = g.held_now.clone() {
                g.append_and_fan(held);
            }
            // And what the session is waiting on, for the same reason: the event is ephemeral,
            // so a head that attached without it would draw a session idle that a turn is
            // running in.
            if let Some(activity) = g.activity_now.clone() {
                g.append_and_fan(activity);
            }
            // And the operation the session is in the middle of, when it announced one: the head
            // that attaches in that window is the one whose empty snapshot needs the sentence
            // most.
            if let Some(filling) = g.filling_now.clone() {
                g.append_and_fan(filling);
            }
            // And the retry series, when one is running: a head that attaches in that window would
            // otherwise draw a transcript with nothing wrong in it while the model is not answering.
            if let Some(retrying) = g.retry_now.clone() {
                g.append_and_fan(retrying);
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

    /// Wait for the next delivery for `head_id`, up to `max` events at a time. Blocks until there
    /// is something; a closed hub answers the backlog first and then ends.
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
                // The held counts again, after the snapshot: the queue that may have carried the
                // last change was just thrown away.
                if let Some(held) = g.held_now.clone() {
                    g.append_and_fan(held);
                }
                // And the activity, for the reason the attach arm gives.
                if let Some(activity) = g.activity_now.clone() {
                    g.append_and_fan(activity);
                }
                // And the operation in flight, for the attach arm's third reason.
                if let Some(filling) = g.filling_now.clone() {
                    g.append_and_fan(filling);
                }
                // And the running retry series, for the attach arm's fourth.
                if let Some(retrying) = g.retry_now.clone() {
                    g.append_and_fan(retrying);
                }
                return Delivery::Resync {
                    reason,
                    dropped,
                    snapshot: Box::new(snapshot),
                    scrubbed: ScrubReport::default(),
                };
            }
            // The side channel first, so a screen's repaint is not stuck behind a batch of a
            // thousand deltas - see [`Delivery::Frames`].
            if !g.heads[idx].side.is_empty() {
                let frames: Vec<ServerFrame> = g.heads[idx].side.drain(..).collect();
                return Delivery::Frames(frames);
            }
            if !g.heads[idx].queue.is_empty() {
                let n = max.min(g.heads[idx].queue.len());
                let caps = g.heads[idx].caps.clone();
                let events: Vec<Envelope> = g.heads[idx]
                    .queue
                    .drain(..n)
                    .flat_map(|e| for_head(&caps, e))
                    .collect();
                return Delivery::Events(Batch::new(events));
            }
            // A CLOSED HUB STILL OWES THE HEAD WHAT WAS QUEUED FOR IT, so this check sits AFTER
            // the drains: at the top of the loop it won every race and dropped the queue, losing
            // the `daemon_stopping` sentence the shutdown path had just published.
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
                // The pane's frames go too: a repaint from before the resync would be drawn as
                // the screen now, which is the one thing a resync exists to stop being true.
                h.side.clear();
                h.needs_resync = Some(reason.into());
            }
        }
        self.cv.notify_all();
    }

    /// Record a head's read mark. Called when the head's ack arrives - which is after it wrote
    /// the batch out, not when it received it.
    pub fn ack(&self, head_id: &str, ack: Ack) {
        let mut g = self.lock();
        if let Some(h) = g.heads.iter_mut().find(|h| h.id == head_id) {
            h.mark.apply(ack);
        }
    }

    /// Install what this session's harness holds. Called once by the harness at open, and never
    /// removed, so a census always has an answer.
    pub fn set_footprint(&self, source: FootprintSource) {
        *self
            .footprint_source
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(source);
    }

    /// What this session's harness holds, now: all zeroes for a hub nobody registered a footprint
    /// on, which is a session with no harness.
    pub fn footprint(&self) -> Footprint {
        let source = self
            .footprint_source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        source.map(|s| s()).unwrap_or_default()
    }

    /// See [`HubCensus`]. Takes the hub's lock for the count and nothing else, and reads the
    /// footprint after that lock is dropped.
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

    /// Detach. Announced, and nothing else happens: no turn is cancelled, no work is reaped.
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
        // And what that head left half-done. An operator's call is admitted on one frame and
        // reported on another, and the admission must not outlive the connection that made it.
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
            self.warn(
                "operator_call_abandoned",
                format!(
                    "`{who}` was admitted to run `{name}` themselves (call {call_id}) and the \
                     head that asked went away before reporting what it did. **The admission \
                     is on the record and the result is not** — so the corpus holds an \
                     `admit` for a call whose outcome nobody knows, and this sentence is the \
                     only thing that says so. Nothing by that name is pending any more."
                ),
            );
        }
    }

    /// Remember an admitted operator call until the head reports what it did, as `(owner head,
    /// name, who)` keyed by the head's own `call_id`. See [`Self::detach`] for why the window
    /// this covers is real.
    pub fn note_operator_call(&self, call_id: &str, head_id: &str, name: &str, who: &str) {
        self.lock().operator_calls.insert(
            call_id.to_string(),
            (head_id.to_string(), name.to_string(), who.to_string()),
        );
    }

    /// The operator's call came back. `None` when the daemon is not holding that `call_id`: a
    /// head reporting a call it never had admitted is refused by name rather than appended,
    /// because a row no admission stands behind is a row the conversation cannot justify.
    pub fn take_operator_call(&self, call_id: &str) -> Option<(String, String)> {
        self.lock()
            .operator_calls
            .remove(call_id)
            .map(|(_, name, who)| (name, who))
    }

    pub fn attached_heads(&self) -> usize {
        self.lock().heads.len()
    }

    /// Heads that said they will answer a decision, by identity. Not `attached_heads`: a connector
    /// attaches with `can_decide: false` and is not a person who may decide.
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

    /// Validate and enqueue a mutating command. A prompt on a stale `expected_seq` is accepted
    /// with a note, because a second head's prompt must not be lost to a race.
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
        // Declared beside `ring` and for the same reason: every early return below leaves
        // without delivering, and a `None` initialiser would be a value nothing ever reads.
        let delivery: Option<(Arc<dyn AnswerSink>, String, Reply, String)>;
        let cancellation: Option<(Arc<dyn AnswerSink>, String)>;
        let frame = {
            let mut g = self.lock();
            let actual = g.log.head_seq();
            // The daemon is not a head, it is the thing that seats them. Requiring the caller to be
            // one of this session's heads stops a stray connection acting on a session it does not hold.
            let (identity, can_decide) = match g.heads.iter().find(|h| h.id == head_id) {
                Some(h) => (h.identity.clone(), h.caps.can_decide),
                None if head_id == DAEMON_SUBMITTER
                    && matches!(
                        kind,
                        CommandKind::Interrupt { .. } | CommandKind::Message { .. }
                    ) =>
                {
                    // A relayed message keeps its own author: the daemon is the caller, not the
                    // speaker, and being able to message a subagent is what the edge exists for.
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

            // A row's question and a mode's consent are answered only by somebody who may decide:
            // a watching head is not a person who may decide.
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
                // A malformed answer is refused HERE, before it reaches anything that would act on
                // it, and the question stays open rather than being settled wrongly.
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
                // Against `choices`, not `options`: a question's options are its plain-text choices,
                // and `options` is the adjudication ladder.
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
                // The skill is named because the operator is about to read a long body:
                // "queued" alone would not say WHICH skill was handed over.
                (CommandKind::LoadSkill { name, .. }, true) => {
                    format!("{REJECT_STALE_SEQ}: `{name}` queued anyway")
                }
                (CommandKind::LoadSkill { name, .. }, false) => {
                    format!("`{name}` queued — its body is handed to the model in the next turn")
                }
                (CommandKind::Answer { reply, .. }, _) => {
                    format!("{} answered", reply.as_str())
                }
                (CommandKind::AnswerTodo { content, .. }, _) => {
                    format!("the operator answered the question on `{}`", content.trim())
                }
                // "requested" alone says nothing about when: a running turn takes it at its next
                // round boundary (`Harness::apply_queued_mode`), so the sentence is true in both
                // states.
                (CommandKind::Mode { name, consented }, _) => format!(
                    "mode `{name}` requested — it takes effect from the next call, \
                     whether or not a turn is running{}",
                    if *consented {
                        " — with the operator's confirmation that this box is the boundary"
                    } else {
                        ""
                    }
                ),
                // Stale-tolerant: neither is a mutation of its own. The ask puts a question in
                // front of somebody and the answer settles one whose identity is the `req_id`,
                // so a moved seq says nothing about which consent is still being waited for.
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
                // Redacted, because this note is appended to the session log and read back by
                // every head: a `--key K` went into the log in plaintext once. The worker still
                // gets the line whole, since `CommandKind` is not persisted.
                (CommandKind::Slash { line }, _) => format!("/{}", redact_key_args(line)),
                // A read, not a mutation: a head that asked while the screen moved
                // still meant it, and nothing this command alters depends on the seq.
                (CommandKind::ReadJobOutput { job, .. }, _) => {
                    format!("reading output of job {job}")
                }
                // Stale-tolerant like a prompt: refusing here would make the door fail on a
                // busy session, which is the one state it exists for.
                (CommandKind::OperatorCall { name, .. }, true) => {
                    format!("{REJECT_STALE_SEQ}: admitted anyway — the operator's own {name} call")
                }
                (CommandKind::OperatorCall { name, .. }, false) => {
                    format!("admitted the operator's own {name} call")
                }
                (CommandKind::OperatorResult { call_id, .. }, _) => {
                    format!("result for the operator's call {call_id}")
                }
                // Stale-tolerant like the door beside it: the line's whole point is that the
                // daemon, not this head, runs it, and mid-turn it waits for the round boundary -
                // a queue, not a refusal.
                (CommandKind::OperatorShell { line, .. }, true) => format!(
                    "{REJECT_STALE_SEQ}: queued anyway — the operator's own shell line `{line}`"
                ),
                (CommandKind::OperatorShell { line, .. }, false) => {
                    format!("the operator's own shell line `{line}` queued for this session to run")
                }
                // Never submitted by a head: no frame produces this kind. It is the daemon's own
                // finished work coming back through its own door.
                (CommandKind::OperatorShellResult { call_id, .. }, _) => {
                    format!("the operator's shell run `{call_id}` finished")
                }
            };

            let verb = kind.verb();
            // An answer does not go on the command queue when anybody is waiting for one - see
            // [`AnswerSink`]. Taken here and delivered after the lock is released, because the
            // sink takes its own mutex. A hub with no sink queues it instead.
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
            // An interrupt cancels a decision being waited on. It still queues, and the sink is
            // told as well, because the thread blocked inside a tool call is not draining that
            // queue and would otherwise sit out its whole deadline.
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
            // Announced as an event, so both heads see it and who did it.
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
        // Outside the lock: the waiter wakes on the sink's own condition and takes its own mutex,
        // and must not be doing that while this lock is held.
        if let Some((sink, req_id, reply, identity)) = delivery {
            sink.answer(&req_id, &identity, &reply);
        }
        if let Some((sink, reason)) = cancellation {
            sink.cancel(&reason);
        }
        if let Some((bell, id)) = ring {
            bell.ring(&id);
        }
        // A command waiting for the worker is a turn the model has not taken yet. Said here
        // because nothing else will: the queue is the only witness.
        self.refresh_activity();
        frame
    }

    /// A command the daemon enqueues without a head behind it: the operator's finished shell run
    /// coming back from its own thread, and a settlement the daemon itself decided.
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
            held_rooms: match &g.held_now {
                Some(SessionEvent::HeldResults { room, .. }) => *room,
                _ => 0,
            },
        }
    }

    /// Whether a TURN is in flight here, which is not what `SessionStatus::running` answers: that
    /// is the round-level flag, false for the whole of a round's tool calls.
    pub fn turn_in_flight(&self) -> bool {
        let g = self.lock();
        match g.view.turn() {
            Some(t) => match t.state {
                crate::view::TurnState::Running => true,
                // A round that did not end the prompt leaves it in flight, calls or not: the
                // daemon's `ends_turn` is the only word on it. An ending still waits on its calls.
                crate::view::TurnState::Finished { ends_turn, .. } => {
                    !ends_turn
                        || t.calls
                            .iter()
                            .any(|c| !matches!(c.state, crate::view::CallState::Finished { .. }))
                }
                // A turn that was interrupted or failed is over, whatever its calls were left as.
                crate::view::TurnState::Interrupted { .. }
                | crate::view::TurnState::Failed { .. } => false,
            },
            // Nothing has run here yet, or the turn in view is gone: no turn is in flight.
            None => false,
        }
    }

    /// Publish the prompt's ending when the last round said the prompt goes on and nothing came
    /// after it (a loop that returned between rounds). Zero usage, so no head is billed twice.
    /// Returns whether a frame was published.
    pub fn end_the_prompt(&self) -> bool {
        let event = {
            let g = self.lock();
            match g.view.turn() {
                Some(t) => match &t.state {
                    crate::view::TurnState::Finished {
                        finish_reason,
                        usage,
                        timings,
                        ends_turn: false,
                    } => Some(SessionEvent::TurnFinished {
                        turn_id: t.turn_id.clone(),
                        finish_reason: finish_reason.clone(),
                        // The last round's numbers, so a head's context meter does not read zero;
                        // no cost, because that round's ending already billed it.
                        usage: crate::event::Usage {
                            cost_micros_usd: None,
                            ..*usage
                        },
                        timings: *timings,
                        ends_turn: true,
                        prefix_check: None,
                        dialect_template_sha: String::new(),
                    }),
                    _ => None,
                },
                None => None,
            }
        };
        match event {
            Some(e) => {
                self.publish(e);
                true
            }
            None => false,
        }
    }

    /// Take the next thing for the session's single reader. Blocks; `Closed` once the hub is. The
    /// reader is whoever runs this session's turns.
    pub fn take_own_work(&self) -> OwnWork {
        self.take_own_work_until(None)
    }

    /// `take_own_work` with a clock attached: give up at `deadline` and say so. The reader that
    /// needs it is a subagent's server thread, which also has to answer its parent.
    pub fn take_own_work_until(&self, deadline: Option<std::time::Instant>) -> OwnWork {
        // No activity refresh for a command, deliberately: a command leaving this queue is not the work
        // being over - the worker has it and the turn it makes has not started. `work_pending`
        // therefore stays true through that gap, and the turn's own `TurnStarted` ends it.
        self.pop_own_work_until(deadline)
    }

    fn pop_own_work_until(&self, deadline: Option<std::time::Instant>) -> OwnWork {
        let mut g = self.lock();
        loop {
            // A command first, because a command has a head behind it and a wake has nobody
            // waiting. The `Bell` keeps the same order between work and wakes.
            if let Some(c) = g.commands.pop_front() {
                return OwnWork::Command(c);
            }
            // A wake carries no turn of its own (the daemon's posted jobs ring it too), so its end
            // is said here: left published it read as `delivery` with nothing owed.
            if g.own_wake {
                g.own_wake = false;
                drop(g);
                self.refresh_activity();
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

    /// A wake for the thread that owns this session's turn, rather than for the daemon's worker:
    /// the two readers are different threads and must be woken apart.
    pub fn wake_its_own_reader(&self) -> bool {
        self.hand_to_its_own_reader_with(None)
    }

    /// A command the daemon took off this queue but must not run itself. The worker and a
    /// subagent's own thread take from one queue, so the taker must say which it is.
    pub fn give_back_to_its_own_reader(&self, cmd: QueuedCommand) -> bool {
        if !matches!(
            cmd.kind,
            CommandKind::Interrupt { .. } | CommandKind::Message { .. } | CommandKind::Slash { .. }
        ) {
            return false;
        }
        self.hand_to_its_own_reader_with(Some(cmd))
    }

    /// Wake this hub's own condvar, with or without work attached: the one door the two
    /// functions above go through, so they cannot disagree about the lock.
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
        // Work handed back to this session's own reader is a turn it owes again, the same fact
        // `submit` says for a command.
        self.refresh_activity();
        true
    }

    /// The worker's pop: non-blocking, for a worker that also has other things to do. `None` for
    /// a hub the worker does not own.
    pub fn try_command(&self) -> Option<QueuedCommand> {
        if self.owner() != Owner::Worker {
            return None;
        }
        // See `take_own_work_until`: a command taken is not a delivery finished.
        self.lock().commands.pop_front()
    }

    /// The next command a running turn can act on, leaving the rest queued. Mid-turn steering may
    /// consume a prompt, an interrupt or a take-back, and nothing else.
    pub fn try_steering_command(&self) -> Option<QueuedCommand> {
        self.try_steering_command_of(|k| {
            matches!(
                k,
                CommandKind::Prompt { .. }
                    | CommandKind::Interrupt { .. }
                    | CommandKind::Message { .. }
                    | CommandKind::AnswerTodo { .. }
            )
        })
    }

    /// The first queued command `take` accepts, the rest left in order. A cheap turn takes
    /// only a stop and a row's answer, so a prompt waits for a full turn of its own.
    pub fn try_steering_command_of(
        &self,
        take: impl Fn(&CommandKind) -> bool,
    ) -> Option<QueuedCommand> {
        let mut g = self.lock();
        let i = (0..g.commands.len()).find(|&i| take(&g.commands[i].kind))?;
        g.commands.remove(i)
    }

    /// Is the operator waiting to say something, without taking it:
    /// [`Self::try_steering_command`] removes what it finds, and a reader that only asks must not.
    pub fn has_queued_prompt(&self) -> bool {
        self.lock()
            .commands
            .iter()
            .any(|c| matches!(c.kind, CommandKind::Prompt { .. }))
    }

    /// Has the operator asked for a stop this turn has not taken yet, without taking it:
    /// [`Self::has_queued_prompt`]'s shape, for the interrupt.
    pub fn has_queued_interrupt(&self) -> bool {
        self.lock()
            .commands
            .iter()
            .any(|c| matches!(c.kind, CommandKind::Interrupt { .. }))
    }

    /// Take a queued stop, leaving every other command where it is - a wait a person ended must
    /// not eat the prompt they typed beside it. `None` when nobody asked for a stop.
    pub fn take_queued_interrupt(&self) -> Option<String> {
        let mut g = self.lock();
        let i = (0..g.commands.len())
            .find(|&i| matches!(g.commands[i].kind, CommandKind::Interrupt { .. }))?;
        match g.commands.remove(i)?.kind {
            CommandKind::Interrupt { reason } => Some(reason),
            _ => None,
        }
    }

    /// The next mode change a running turn can act on: `/mode` is what an operator reaches for
    /// while being asked, so it has to be reachable mid-turn.
    pub fn try_mode_command(&self) -> Option<QueuedCommand> {
        let mut g = self.lock();
        let i = (0..g.commands.len())
            .find(|&i| matches!(g.commands[i].kind, CommandKind::Mode { .. }))?;
        g.commands.remove(i)
    }

    /// The next operator's own call a running turn can act on. Its whole value is that it is
    /// issued while a turn is running.
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

    /// The next queued command a running turn can act on, chosen by the turn: the only picker
    /// whose predicate is not a `CommandKind`.
    pub fn try_command_for_running_turn(
        &self,
        want: impl Fn(&CommandKind) -> bool,
    ) -> Option<QueuedCommand> {
        let mut g = self.lock();
        let i = (0..g.commands.len()).find(|&i| want(&g.commands[i].kind))?;
        g.commands.remove(i)
    }

    /// The next take-back a running turn can act on, dropping the issuing head's still-queued
    /// prompts with it: a take-back recalls what that head queued and nothing else.
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
        // The whole of the head's queue as it stood: the prefix `0..at`. Those two facts are what
        // make the positional rule the whole-queue rule rather than an approximation.
        let mut kept = VecDeque::with_capacity(g.commands.len());
        for (i, c) in std::mem::take(&mut g.commands.q).into_iter().enumerate() {
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
        g.commands.replace(kept);
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
        // The last activity, kept for the head that was not listening - see
        // [`Inner::activity_now`]. Recorded here, where every publish passes, rather than in
        // [`Hub::set_activity`], so a value replayed from a log read back into a hub is kept.
        if matches!(event, SessionEvent::ActivityChanged { .. }) {
            self.activity_now = Some(event.clone());
        }
        // The last fill, kept the same way: it is ephemeral, and the head that needs it most is
        // the one that was not attached when it was published.
        if let SessionEvent::Filling { done, total, .. } = &event {
            self.filling_now = (*done < *total).then(|| event.clone());
        }
        // And the retry line, which is `Filling`'s rule with the ending as the zero: a line that has
        // settled or gone is not in flight, so a head attaching now is not sent one it cannot end.
        if let SessionEvent::ModelRetry { ended, gone, .. } = &event {
            self.retry_now = (ended.is_none() && !*gone).then(|| event.clone());
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

/// What a head is sent for one logged event. A head that does not declare `row_bodies` reads no
/// body off an announcement, so it gets the announcement bare and the body as `TranscriptContent`
/// at the same seq; the log keeps the body once.
fn for_head(caps: &Caps, env: Envelope) -> Vec<Envelope> {
    if caps.row_bodies {
        return vec![env];
    }
    let Envelope {
        session_id,
        seq,
        ts,
        event,
    } = env;
    let make = |event| Envelope {
        session_id: session_id.clone(),
        seq,
        ts,
        event,
    };
    match event {
        SessionEvent::TranscriptAppended {
            item_id,
            kind,
            ledger_head,
            item: Some(item),
            body_bytes,
        } => vec![
            make(SessionEvent::TranscriptAppended {
                item_id: item_id.clone(),
                kind,
                ledger_head,
                item: None,
                body_bytes,
            }),
            make(SessionEvent::TranscriptContent { item_id, item }),
        ],
        event => vec![make(event)],
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests;

#[cfg(test)]
mod mode_steering_tests {
    use super::*;

    /// A queued prompt is saved when accepted, unsaved when taken, and a new hub is handed it.
    #[test]
    fn a_queued_prompt_is_journaled_until_taken_and_put_back_by_a_new_hub() {
        use std::sync::{Arc, Mutex};
        struct Kept(Arc<Mutex<Vec<JournaledPrompt>>>);
        impl PromptJournal for Kept {
            fn save(&mut self, p: &[JournaledPrompt]) -> Result<(), String> {
                *self.0.lock().unwrap() = p.to_vec();
                Ok(())
            }
        }
        let kept = Arc::new(Mutex::new(Vec::new()));
        let hub = Hub::new("s-journal");
        assert_eq!(
            hub.journal_prompts(Box::new(Kept(kept.clone())), Vec::new()),
            0
        );
        let a = hub.attach("tui", "dead", Caps::default(), 0);
        hub.submit(
            &a.head_id,
            "c1",
            0,
            CommandKind::Prompt {
                text: "do the thing".into(),
                attachments: Vec::new(),
                context: None,
            },
        );
        assert!(hub.journal_settled(std::time::Duration::from_secs(5)));
        let saved = kept.lock().unwrap().clone();
        assert_eq!(saved.len(), 1, "an accepted prompt is saved");
        // The daemon is killed here. A new hub is handed what was saved.
        let again = Hub::new("s-journal");
        let n = again.journal_prompts(Box::new(Kept(Arc::new(Mutex::new(Vec::new())))), saved);
        assert_eq!(n, 1);
        match again.take_own_work() {
            OwnWork::Command(c) => assert!(
                matches!(&c.kind, CommandKind::Prompt { text, .. } if text == "do the thing"),
                "{c:?}"
            ),
            other => panic!("{other:?}"),
        }
        // Taking it empties the saved queue on the first hub too.
        assert!(matches!(hub.take_own_work(), OwnWork::Command(_)));
        assert!(hub.journal_settled(std::time::Duration::from_secs(5)));
        assert!(kept.lock().unwrap().is_empty(), "taken, so no longer saved");
    }

    /// A journal whose store is locked by another process must not hold the session up: the
    /// prompt is accepted and taken at once, and saved when the store lets go.
    #[test]
    fn a_blocked_journal_holds_up_neither_the_submit_nor_the_take() {
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::{Duration, Instant};
        struct Locked {
            gate: Arc<Mutex<mpsc::Receiver<()>>>,
            kept: Arc<Mutex<Vec<JournaledPrompt>>>,
        }
        impl PromptJournal for Locked {
            fn save(&mut self, p: &[JournaledPrompt]) -> Result<(), String> {
                let _ = self
                    .gate
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(30));
                *self.kept.lock().unwrap() = p.to_vec();
                Ok(())
            }
        }
        let (release, gate) = mpsc::channel();
        let kept = Arc::new(Mutex::new(Vec::new()));
        let hub = Arc::new(Hub::new("s-journal-locked"));
        hub.journal_prompts(
            Box::new(Locked {
                gate: Arc::new(Mutex::new(gate)),
                kept: kept.clone(),
            }),
            Vec::new(),
        );
        let a = hub.attach("tui", "dead", Caps::default(), 0);
        let (done, finished) = mpsc::channel();
        let h = hub.clone();
        let head = a.head_id.clone();
        std::thread::spawn(move || {
            h.submit(
                &head,
                "c1",
                0,
                CommandKind::Prompt {
                    text: "tell me about it".into(),
                    attachments: Vec::new(),
                    context: None,
                },
            );
            let taken = matches!(
                h.take_own_work_until(Some(Instant::now() + Duration::from_secs(1))),
                OwnWork::Command(_)
            );
            let _ = done.send(taken);
        });
        let taken = finished.recv_timeout(Duration::from_secs(5));
        let _ = release.send(());
        let _ = release.send(());
        assert_eq!(
            taken,
            Ok(true),
            "the submit or the take waited on the journal's store write"
        );
        let _ = kept;
    }

    /// Looking is not taking: `has_queued_prompt` exists so a round that backgrounded a job can
    /// decide to hand the floor back, and if it consumed what it found the operator's message
    /// would vanish from the worker's queue and be answered by nobody.
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
                context: None,
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

    /// The daemon may interrupt, may relay a parent's message, and nothing else. The door exists
    /// because killing a subagent should not also lose what it was told.
    #[test]
    fn the_daemon_may_interrupt_and_relay_and_nothing_else() {
        let hub = Hub::new("s-daemon-door");

        // A parent's correction to a subagent in flight, relayed by the daemon because the
        // parent is seated in another session's hub. It must not be recorded as the daemon: the
        // child's trail has to name which parent spoke, or the correction is advice from nowhere.
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
                    context: None,
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

        // And the rule the door is an exception TO still holds: `not attached` is what a name
        // nobody minted gets, for an interrupt like anything else - an exception that swallowed
        // the rule would be worse than the bug it fixes.
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

    /// A mode change reaches a running turn: left in the queue it moved nothing, and the turn
    /// went on asking under the old point until the model stopped.
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

    /// A door call issued during a turn is taken by the turn: a door that only opened at idle
    /// would be worth a fraction of what it is for.
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
        // Once, and the head's own call is STILL THERE for the worker - not dropped, which
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

    /// The turn's own picker takes what it asks for and nothing else: the door `/models` reaches
    /// a retrying turn through.
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

        // The refused one is still there, and still first. Order is the promise a person reads
        // off the queue, and a picker that reordered it would apply their two switches backwards.
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

    /// A closed hub still hands over what was queued for the head, ordered rather than raced.
    #[test]
    fn a_closed_hub_delivers_what_was_queued_before_it_says_closed() {
        let hub = Hub::new("ordered-shutdown");
        let head = hub.attach("h", "tui", Caps::default(), 0).head_id;

        // No drain first, and that is not an omission: a snapshot attach leaves this head's
        // queue empty and `next_batch` on an empty queue of an OPEN hub blocks on its condvar.
        // Publish, then close, then ask, so the first answer is the event.
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

/// A slash line with its key arguments masked: the value after `--key` or `--api-key`, or in
/// `--key=VALUE`, becomes `•••`. For anything that persists or echoes a command line.
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
