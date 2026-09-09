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

use std::collections::VecDeque;
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
    Prompt { text: String },
    Interrupt { reason: String },
    Answer { req_id: String, option_id: String },
}

impl CommandKind {
    fn verb(&self) -> &'static str {
        match self {
            CommandKind::Prompt { .. } => "prompt",
            CommandKind::Interrupt { .. } => "interrupt",
            CommandKind::Answer { .. } => "answer",
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
}

/// What a session looks like from outside it: enough for a picker, and cheap
/// enough to compute for every session on every list.
///
/// Cut under the hub's own lock in one pass, rather than assembled from four
/// accessors, because four accessors are four different instants and a list in
/// which one row is 30 ms older than the next is a list that can show a session as
/// both running and finished.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
            }),
            cv: Condvar::new(),
        })
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

            if let CommandKind::Answer { req_id, .. } = &kind {
                if !can_decide {
                    return ServerFrame::Rejected {
                        client_request_id,
                        reason: REJECT_READ_ONLY.into(),
                        expected_seq,
                        actual_seq: actual,
                    };
                }
                if !g.view.open_decisions().iter().any(|d| &d.req_id == req_id) {
                    return ServerFrame::Rejected {
                        client_request_id,
                        reason: REJECT_UNKNOWN_DECISION.into(),
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
                (CommandKind::Interrupt { .. }, _) => "interrupt requested".into(),
                (CommandKind::Answer { .. }, _) => "decision answered".into(),
            };

            let verb = kind.verb();
            // Taken here, rung *after* the lock is released: `Bell::ring` takes its
            // own mutex, and taking a second lock inside this one is how a lock
            // order gets invented by accident.
            ring = g
                .bell
                .clone()
                .map(|b| (b, g.log.session_id().to_string()));
            g.commands.push_back(QueuedCommand {
                head_id: head_id.to_string(),
                identity: identity.clone(),
                client_request_id: client_request_id.clone(),
                at_seq: actual,
                kind,
            });
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
            b.events().iter().map(|e| e.event.kind()).collect::<Vec<_>>()
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
                option_id: "allow".into(),
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
                option_id: "allow".into(),
            },
        );
        assert!(
            matches!(f, ServerFrame::Rejected { ref reason, .. } if reason == REJECT_READ_ONLY)
        );
    }
}
