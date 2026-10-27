//! **Metrics and operator feedback** — the event row, and the one way to send one.
//!
//! `docs/metrics.md` is the design and TODO R81 schedules it. The short of it: every decision
//! R61–R79 took was made from a hand query, a log grep or one anecdote, and each should have been
//! a `SELECT`. So the daemon writes one append-only row per observation — a turn, a model round, a
//! tool call, a delivery, a wake, a nag, a compaction, an operator's mark — into a SIBLING file,
//! `~/.local/share/letibot/metrics.db`, never into the store (the operator's decision 1,
//! 2026-10-11: eleven daemons already share the store's one writer lock).
//!
//! # The one rule: [`emit`] never blocks and never touches SQL
//!
//! The c12d044 lesson — the worker sat 29–89 s behind a store read nobody had timed — is that
//! anything on the worker's path is paid by every prompt. So an instrument builds a small
//! [`Event`] and hands it to a **bounded** channel with `try_send`. A full channel drops the event
//! and counts it ([`dropped`]); the writer thread reports the count as a `metrics.dropped` row. No
//! lock shared with the store, no allocation beyond the event, no file. The writer — one thread
//! per daemon, behind the `store` feature — drains the channel and commits a batch a second.
//!
//! **Off is free.** Nothing is installed until the daemon calls [`store::start`] (the `[metrics]
//! enabled` switch, on by default — decision 6), so a head, a test and a daemon with metrics off
//! all reach an [`emit`] that returns at the first branch.
//!
//! # Typed at the site, generic in the file
//!
//! A row is `kind`, `name`, `outcome`, three integer slots and a small JSON `attrs` (§2.2), so a
//! new kind is a new constant and a new view — never an `ALTER`, and a build that knows fewer
//! kinds writes beside one that knows more. [`kind`] names every kind this tree writes; what each
//! slot means per kind is the table in `docs/metrics.md` §2.3, and the views in [`store::VIEWS`]
//! are where that table becomes column names.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

pub use serde_json::Value;

#[cfg(feature = "store")]
pub mod stats;
#[cfg(feature = "store")]
pub mod store;

/// **Every kind this tree writes** — `docs/metrics.md` §2.3's vocabulary, dotted.
pub mod kind {
    /// A turn began. Written with the turn's end (see the harness's `instruments`), stamped
    /// with the moment it began. `name` = the door; n1/n2/n3 = fill tokens, window, rounds.
    pub const TURN_START: &str = "turn.start";
    /// A turn ended. `name` = model; `outcome` = how; n = prompt / cached / output tokens.
    pub const TURN_END: &str = "turn.end";
    /// One model request. `name` = model; `outcome` = finish reason; n = prompt / cached /
    /// output; `attrs.ttfb_ms` the time to the first token.
    pub const ROUND: &str = "round";
    /// One tool call. `name` = tool; n = inline bytes / full bytes / memory peak KiB.
    pub const TOOL_CALL: &str = "tool.call";
    /// A job's or a child's end reaching the model (R79). `name` = what; `outcome` = how.
    pub const DELIVERY: &str = "delivery";
    /// A between-turns wake. `name` = the first cause; `outcome` = `ran` / `empty`.
    pub const WAKE: &str = "wake";
    /// A plan check sent (R77). `attrs.row` names the row; the follow-up says if it was touched.
    pub const NAG: &str = "nag";
    /// Whether the turn after a nag touched the row it named — a second, small row, keyed by
    /// the nag's turn, because the answer is only known a turn later.
    pub const NAG_FOLLOWUP: &str = "nag.followup";
    /// A compaction. n = before / after (shown unit); `attrs` carry the ledger unit.
    pub const COMPACTION: &str = "compaction";
    /// A turn stopped at the context wall. n = resident / window / rounds.
    pub const WALL: &str = "wall";
    /// R71's "my context is full", judged. n = resident / window / score×1000.
    pub const CONTEXT_CLAIM: &str = "context.claim";
    /// A command's wait on the worker (c12d044). n1 = queued ms, n2 = queue depth.
    pub const WORKER_CMD: &str = "worker.cmd";
    /// An idle pass on the worker that took more than 250 ms.
    pub const WORKER_PASS: &str = "worker.pass";
    /// The daemon's memory (R66). n = rss MiB / heap live MiB / heap peak MiB.
    pub const MEM_SAMPLE: &str = "mem.sample";
    /// A hub warning, by code. `outcome` = its class.
    pub const WARNING: &str = "warning";
    /// An operator's command, as the hub saw it issued.
    pub const OPERATOR_ACTION: &str = "operator.action";
    /// A subagent's state change, as the hub saw it.
    pub const SUBAGENT: &str = "subagent";
    /// A background job settled, as the hub saw it.
    pub const JOB: &str = "job";
    /// A decision answered (operator, oracle, timeout).
    pub const DECISION_ANSWER: &str = "decision.answer";
    /// A queue entry moved (pointer; `queue_transition` in the store stays the record).
    pub const QUEUE_MOVE: &str = "queue.move";
    /// **The operator's mark** (§2.5). `name` = `good` / `bad` / `note`.
    pub const FEEDBACK: &str = "operator.feedback";
    /// The writer's own health: events dropped on a full channel since the last report.
    pub const DROPPED: &str = "metrics.dropped";
    /// **A key the writer is holding back** (`docs/metrics.md` §2.9) — one repeating event over its
    /// per-second budget, which on 2026-10-27 meant a defect and not activity.
    ///
    /// `name` = the suppressed kind; `session_id`/`outcome` the key's; n1 = events held back in
    /// that second, n2 = events seen in it (the rate, per second); `attrs.key` is
    /// `session|kind|name|outcome` and `attrs.budget` the budget that was exceeded.
    pub const SUPPRESSED: &str = "metrics.suppressed";
}

/// The wall clock, in milliseconds — the unit of `ts_ms`.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// **One observation** — a row of `event` before it is written.
///
/// `daemon` and `build` are not here: they are the writer's, filled once per row from what
/// [`store::start`] was given. `root_id` may be left `None`: the writer fills it from the tree
/// [`session_parent`] taught it, so a site deep in a subagent need not walk its ancestry.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// **The moment of the observation**, not of the write — a batch lands up to a second later.
    pub ts_ms: u64,
    pub dur_ms: Option<u64>,
    pub session_id: Option<String>,
    pub root_id: Option<String>,
    pub transcript_id: Option<String>,
    pub turn_id: Option<String>,
    pub round: Option<u32>,
    pub kind: &'static str,
    pub name: Option<String>,
    pub outcome: Option<String>,
    /// The kind's three headline numbers (§2.2) — `SUM`-able without parsing JSON.
    pub n: [Option<i64>; 3],
    /// The rest of the kind's fields. Small, and **no content** unless the kind is named in
    /// `[metrics] snippets` (then `snippet` survives the writer; otherwise it is stripped).
    pub attrs: Vec<(&'static str, Value)>,
}

impl Event {
    /// A row of `kind`, stamped now.
    pub fn new(kind: &'static str) -> Event {
        Event {
            ts_ms: now_ms(),
            dur_ms: None,
            session_id: None,
            root_id: None,
            transcript_id: None,
            turn_id: None,
            round: None,
            kind,
            name: None,
            outcome: None,
            n: [None; 3],
            attrs: Vec::new(),
        }
    }

    pub fn at(mut self, ts_ms: u64) -> Event {
        self.ts_ms = ts_ms;
        self
    }

    pub fn dur(mut self, ms: u64) -> Event {
        self.dur_ms = Some(ms);
        self
    }

    /// The session, when there is one. An empty id is no id.
    pub fn session(mut self, id: &str) -> Event {
        self.session_id = (!id.is_empty()).then(|| id.to_string());
        self
    }

    pub fn transcript(mut self, id: &str) -> Event {
        self.transcript_id = (!id.is_empty()).then(|| id.to_string());
        self
    }

    /// The turn and the 0-based round within it. An empty turn id is no turn.
    pub fn turn(mut self, id: &str, round: Option<u32>) -> Event {
        self.turn_id = (!id.is_empty()).then(|| id.to_string());
        self.round = round;
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Event {
        self.name = Some(name.into());
        self
    }

    pub fn outcome(mut self, outcome: impl Into<String>) -> Event {
        self.outcome = Some(outcome.into());
        self
    }

    pub fn n1(mut self, v: u64) -> Event {
        self.n[0] = Some(v as i64);
        self
    }

    pub fn n2(mut self, v: u64) -> Event {
        self.n[1] = Some(v as i64);
        self
    }

    pub fn n3(mut self, v: u64) -> Event {
        self.n[2] = Some(v as i64);
        self
    }

    pub fn attr(mut self, key: &'static str, v: impl Into<Value>) -> Event {
        self.attrs.push((key, v.into()));
        self
    }

    /// An attribute only when there is one — so a site never writes `null`s it would have to
    /// explain.
    pub fn attr_opt<V: Into<Value>>(self, key: &'static str, v: Option<V>) -> Event {
        match v {
            Some(v) => self.attr(key, v),
            None => self,
        }
    }

    /// The attribute named `key`, if this event has one. For tests and for the writer.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.attrs.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// [`emit`] this event. The spelling every site uses, so a site is one line.
    pub fn emit(self) {
        emit(self);
    }
}

/// What travels on the channel: a row, or something the writer needs to know.
#[derive(Debug)]
pub enum Msg {
    Event(Box<Event>),
    /// `session`'s parent (`None` for a root), so the writer can fill `root_id`.
    Parent {
        session: String,
        parent: Option<String>,
    },
    /// Write everything before this, then answer — for a test, and for a clean shutdown.
    Flush(SyncSender<()>),
}

/// **The bounded channel and its drop counter** — the whole of the emitting side.
///
/// Its own type rather than two statics so the never-blocks property can be tested on a
/// channel nobody drains, without installing anything process-wide.
#[derive(Debug)]
pub struct Emitter {
    tx: SyncSender<Msg>,
    dropped: AtomicU64,
}

/// §2.7's size: 8,192 rows is two minutes of a busy daemon's ~60 rows a turn — a writer that
/// has fallen that far behind is not coming back by waiting, and the count says so.
pub const CHANNEL: usize = 8192;

impl Emitter {
    pub fn new(capacity: usize) -> (Emitter, Receiver<Msg>) {
        let (tx, rx) = sync_channel(capacity);
        (
            Emitter {
                tx,
                dropped: AtomicU64::new(0),
            },
            rx,
        )
    }

    /// `try_send`, and a count when it did not go. **Never blocks.**
    pub fn send(&self, m: Msg) -> bool {
        match self.tx.try_send(m) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Ask the writer to write what it holds, and wait at most `timeout` for it.
    pub fn flush(&self, timeout: std::time::Duration) -> bool {
        let (tx, rx) = sync_channel(1);
        // A flush may wait for room: it is never sent from a hot path.
        if self.tx.send(Msg::Flush(tx)).is_err() {
            return false;
        }
        rx.recv_timeout(timeout).is_ok()
    }
}

static GLOBAL: OnceLock<Emitter> = OnceLock::new();

thread_local! {
    /// A test's capture of what THIS thread emits — see [`capture`].
    static CAPTURE: RefCell<Option<Vec<Event>>> = const { RefCell::new(None) };
}

/// **Install the process's emitter.** Once: a second call is refused (`false`), because two
/// writers would be two files' worth of one daemon. [`store::start`] is the caller.
pub fn install(e: Emitter) -> bool {
    GLOBAL.set(e).is_ok()
}

/// Is anything listening — a writer, or a test's capture on this thread? A site whose event is
/// costly to build asks first; most do not need to.
pub fn enabled() -> bool {
    GLOBAL.get().is_some() || CAPTURE.with(|c| c.borrow().is_some())
}

/// **Send one event. Never blocks, never fails, never writes.** See the module header.
pub fn emit(e: Event) {
    let e = CAPTURE.with(|c| match c.borrow_mut().as_mut() {
        Some(v) => {
            v.push(e);
            None
        }
        None => Some(e),
    });
    if let (Some(e), Some(g)) = (e, GLOBAL.get()) {
        g.send(Msg::Event(Box::new(e)));
    }
}

/// **Tell the writer whose child a session is**, so its rows get the tree's root. Called once
/// per session, where the registry records the parent.
pub fn session_parent(session: &str, parent: Option<&str>) {
    if let Some(g) = GLOBAL.get() {
        g.send(Msg::Parent {
            session: session.to_string(),
            parent: parent.map(str::to_string),
        });
    }
}

/// Events dropped on a full channel since the process started.
pub fn dropped() -> u64 {
    GLOBAL.get().map(Emitter::dropped).unwrap_or(0)
}

/// Write what the writer holds and wait for it, at most `timeout`. `false` when nothing is
/// installed or the writer did not answer in time.
pub fn flush(timeout: std::time::Duration) -> bool {
    GLOBAL.get().is_some_and(|g| g.flush(timeout))
}

/// **Run `f` and return what it emitted on this thread** — the instruments' tests. Nested
/// captures are not supported; the inner one would steal the outer's events.
pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<Event>) {
    CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
    let r = f();
    let got = CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default();
    (r, got)
}

/// **The `#words` in an operator's note** — §2.5's tags, free vocabulary, counted as written.
/// Lower-cased, trailing punctuation dropped, a bare `#` ignored.
pub fn tags(note: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in note.split_whitespace() {
        let Some(t) = w.strip_prefix('#') else {
            continue;
        };
        let t = t
            .trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
            .to_lowercase();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// **The emitter never blocks** — the c12d044 rule, asserted on a channel nobody drains:
    /// past its capacity every send returns at once, and each one it could not take is counted.
    #[test]
    fn a_full_channel_drops_and_counts_and_never_blocks() {
        let (e, _rx) = Emitter::new(4);
        let started = Instant::now();
        let sent = (0..1000)
            .filter(|_| e.send(Msg::Event(Box::new(Event::new(kind::ROUND)))))
            .count();
        assert_eq!(sent, 4, "the channel holds exactly its capacity");
        assert_eq!(e.dropped(), 996);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a thousand sends into a full channel took {:?} — something waited",
            started.elapsed()
        );
    }

    #[test]
    fn a_closed_writer_is_a_drop_and_not_a_panic() {
        let (e, rx) = Emitter::new(4);
        drop(rx);
        assert!(!e.send(Msg::Event(Box::new(Event::new(kind::ROUND)))));
        assert_eq!(e.dropped(), 1);
    }

    #[test]
    fn a_capture_takes_this_threads_events_and_nothing_else() {
        let ((), got) = capture(|| {
            Event::new(kind::WAKE).name("job").n1(2).emit();
            std::thread::spawn(|| Event::new(kind::WAKE).name("other-thread").emit())
                .join()
                .unwrap();
        });
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name.as_deref(), Some("job"));
        assert_eq!(got[0].n, [Some(2), None, None]);
        assert!(!CAPTURE.with(|c| c.borrow().is_some()), "the capture ends");
    }

    #[test]
    fn tags_are_the_hash_words_lowercased_once_each() {
        assert_eq!(
            tags("got pulled off by j975 #Distracted, #loop #loop # #slow."),
            vec!["distracted", "loop", "slow"]
        );
        assert!(tags("no tags here").is_empty());
    }

    #[test]
    fn an_empty_id_is_no_id() {
        let e = Event::new(kind::ROUND).session("").turn("", Some(0));
        assert_eq!(e.session_id, None);
        assert_eq!(e.turn_id, None);
    }
}
