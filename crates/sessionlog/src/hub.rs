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
    /// This head fell behind, or asked. **Not an error.** Reset to the snapshot and
    /// continue from `snapshot.seq + 1`.
    Resync {
        reason: String,
        dropped: u64,
        snapshot: Box<Snapshot>,
        scrubbed: ScrubReport,
    },
    /// The daemon is shutting down.
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
    Reseat,
    Interrupt {
        reason: String,
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
    /// A slash command for the daemon: `flowy …`, `models …`.
    Slash {
        line: String,
    },
    /// A head took its queued prompts back — the operator pulled the queued line
    /// into the composer to edit it. Consumed by the running turn's steering
    /// poll (which drops the head's held operator text with it); between turns
    /// it is a quiet no-op, because a prompt that survived to here is about to
    /// run as its own turn and is no longer the operator's to take back.
    WithdrawPrompts,
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
/// not a preference. §13.2's one worker drains [`Hub::take_command`] and runs the
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
            CommandKind::Reseat => "reseat",
            CommandKind::Interrupt { .. } => "interrupt",
            CommandKind::Answer { .. } => "answer",
            CommandKind::Mode { .. } => "mode",
            CommandKind::Slash { .. } => "slash",
            CommandKind::WithdrawPrompts => "take-back",
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
    /// `Some(reason)` once this head has been demoted. Its queue is cleared and
    /// stays cleared until it takes the resync.
    needs_resync: Option<String>,
    mark: ReadMark,
}

struct Inner {
    log: SessionLog,
    view: SessionView,
    heads: Vec<Head>,
    next_head: u64,
    commands: VecDeque<QueuedCommand>,
    closed: bool,
    /// Rung when this hub takes a command, so **one** worker can wait on many
    /// sessions without a timer. See [`crate::registry`].
    ///
    /// `None` for a hub nobody registered, which is the single-session case and
    /// every test in this file: [`Hub::take_command`] blocks on this hub's own
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
                next_head: 0,
                commands: VecDeque::new(),
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
            if g.closed {
                return Delivery::Closed;
            }
            let at = g.log.head_seq();
            let dropped = g.log.dropped();
            let Some(idx) = g.heads.iter().position(|h| h.id == head_id) else {
                return Delivery::Closed;
            };
            if let Some(reason) = g.heads[idx].needs_resync.take() {
                let snapshot = g.view.snapshot(at, dropped);
                g.heads[idx].queue.clear();
                g.heads[idx].mark.seq = at;
                return Delivery::Resync {
                    reason,
                    dropped,
                    snapshot: Box::new(snapshot),
                    scrubbed: ScrubReport::default(),
                };
            }
            if !g.heads[idx].queue.is_empty() {
                let n = max.min(g.heads[idx].queue.len());
                let events: Vec<Envelope> = g.heads[idx].queue.drain(..n).collect();
                return Delivery::Events(Batch::new(events));
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
            let Some(h) = g.heads.iter().find(|h| h.id == head_id) else {
                return ServerFrame::Rejected {
                    client_request_id,
                    reason: "not attached".into(),
                    expected_seq,
                    actual_seq: actual,
                };
            };
            let identity = h.identity.clone();
            let can_decide = h.caps.can_decide;

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
                (CommandKind::Reseat, true) => format!("{REJECT_STALE_SEQ}: queued anyway"),
                (CommandKind::Reseat, false) => "re-seat queued".into(),
                (CommandKind::Interrupt { .. }, _) => "interrupt requested".into(),
                (CommandKind::WithdrawPrompts, _) => "prompt take-back requested".into(),
                (CommandKind::Promote, _) => "background requested".into(),
                (CommandKind::Answer { reply, .. }, _) => {
                    format!("{} answered", reply.as_str())
                }
                (CommandKind::Mode { name, consented }, _) => format!(
                    "mode `{name}` requested{}",
                    if *consented {
                        " — with the operator's confirmation that this box is the boundary"
                    } else {
                        ""
                    }
                ),
                (CommandKind::Slash { line }, _) => format!("/{line}"),
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

    /// Take the next command for the session's single worker. Blocks. `None` once
    /// the hub is closed.
    pub fn take_command(&self) -> Option<QueuedCommand> {
        let mut g = self.lock();
        loop {
            if let Some(c) = g.commands.pop_front() {
                return Some(c);
            }
            if g.closed {
                return None;
            }
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Non-blocking form, for a worker that also has other things to do.
    pub fn try_command(&self) -> Option<QueuedCommand> {
        self.lock().commands.pop_front()
    }

    /// The next command a **running turn** can act on, leaving the rest queued.
    ///
    /// Mid-turn steering may consume a prompt (a follow-up user item) or an
    /// interrupt. Everything else belongs to the between-turn worker, and popping
    /// it here would lose it: the worker is inside the very turn that is polling,
    /// and would never see a command this path swallowed. Scanned rather than
    /// popped-and-dropped for exactly that reason — a [`CommandKind::Compact`]
    /// queued behind a running turn must still be there when the turn ends.
    pub fn try_steering_command(&self) -> Option<QueuedCommand> {
        let mut g = self.lock();
        let i = (0..g.commands.len()).find(|&i| {
            matches!(
                g.commands[i].kind,
                CommandKind::Prompt { .. } | CommandKind::Interrupt { .. }
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
    pub fn try_withdraw_command(&self) -> bool {
        let mut g = self.lock();
        let Some(cmd) = (0..g.commands.len())
            .find(|&i| matches!(g.commands[i].kind, CommandKind::WithdrawPrompts))
            .and_then(|i| g.commands.remove(i))
        else {
            return false;
        };
        g.commands.retain(|c| {
            !(c.head_id == cmd.head_id && matches!(c.kind, CommandKind::Prompt { .. }))
        });
        true
    }

    /// Shut the hub down. Every waiter wakes with [`Delivery::Closed`].
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
