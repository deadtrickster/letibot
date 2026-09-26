//! The append-only event log: monotonic `seq`, bounded scrollback, and a `dropped`
//! count that is **present and zero** rather than absent.
//!
//! §13.2 makes the log authoritative state. §13.2b makes the boundedness a
//! disclosure: *"an absent field and a zero field must not look the same when the
//! field is the disclosure"*. So `dropped` is a `u64` on every `Hello`, and the
//! serialiser is forbidden from eliding it — there is a test.

use std::collections::VecDeque;

use crate::event::{Envelope, SessionEvent, now_ms};

/// How much scrollback to keep, and when a gap is too big to replay.
///
/// §13.2's numbers: *"if the gap exceeds a bound (default 5,000 events or 4 MiB)
/// the daemon answers `RESYNC`"*. Kept as configuration rather than as constants
/// because §13.3's matching rule — the head's buffer size — is configurable too,
/// and a bound you cannot move is a bound somebody reimplements next to yours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogBounds {
    /// Events retained. Older ones are dropped and counted.
    pub scrollback_events: usize,
    /// Bytes retained (approximate, by serialised event size).
    pub scrollback_bytes: usize,
    /// A resume gap larger than this is answered with RESYNC.
    pub max_gap_events: usize,
    /// A resume gap heavier than this is answered with RESYNC.
    pub max_gap_bytes: usize,
    /// Items retained in the materialized view handed to a late head.
    pub snapshot_items: usize,
}

impl Default for LogBounds {
    fn default() -> Self {
        LogBounds {
            scrollback_events: 20_000,
            scrollback_bytes: 16 * 1024 * 1024,
            max_gap_events: 5_000,
            max_gap_bytes: 4 * 1024 * 1024,
            snapshot_items: 2_000,
        }
    }
}

/// One retained event plus the byte cost that made it count against the bound.
#[derive(Debug, Clone)]
struct Retained {
    env: Envelope,
    bytes: usize,
}

/// The log. Not `Sync` on its own; [`crate::hub::Hub`] is what puts it behind a
/// lock, and it is deliberately a plain data structure so the locking is visible
/// in one place.
#[derive(Debug)]
pub struct SessionLog {
    session_id: String,
    bounds: LogBounds,
    ring: VecDeque<Retained>,
    bytes: usize,
    /// The seq of the last appended event. `0` means nothing has been appended;
    /// seq numbering starts at 1, so `0` is also a legal `since_seq` meaning
    /// "I have seen nothing".
    head_seq: u64,
    /// How many events have fallen off the back. Present and zero, always.
    dropped: u64,
    /// **A scratch buffer for measuring an event's serialised size.**
    ///
    /// `append` needs the byte length of the event as JSON — it is the unit the retention bounds
    /// are kept in — and it used to get it from `serde_json::to_string(&env)`, which builds the
    /// whole JSON as a `String` and throws it away. That is once per event, and one event is one
    /// token on the wire path, so it was a full re-serialisation of every event purely to count
    /// its characters.
    ///
    /// A `Vec<u8>` on the log rather than a local, so its capacity survives from event to event:
    /// `to_writer` into it and take `.len()`. The bytes were never read before and still are not —
    /// what is wanted is the length, and the encoding work is unavoidable because the length *is*
    /// the encoded length. What is avoidable is allocating the string.
    size_scratch: Vec<u8>,
}

impl SessionLog {
    pub fn new(session_id: impl Into<String>, bounds: LogBounds) -> Self {
        SessionLog {
            session_id: session_id.into(),
            bounds,
            ring: VecDeque::new(),
            bytes: 0,
            head_seq: 0,
            dropped: 0,
            size_scratch: Vec::new(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn bounds(&self) -> LogBounds {
        self.bounds
    }

    /// The seq of the newest event, or 0 if there is none.
    pub fn head_seq(&self) -> u64 {
        self.head_seq
    }

    /// The seq of the oldest *retained* event, or `head_seq + 1` when empty.
    pub fn first_seq(&self) -> u64 {
        self.ring
            .front()
            .map(|r| r.env.seq)
            .unwrap_or(self.head_seq + 1)
    }

    /// Events that fell off the back of the scrollback. Never elided.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// `ts` of the newest retained event, or `0` when nothing has been appended.
    ///
    /// The log's own clock, not a wall clock in whoever asks — a session list read
    /// from a recorded log must sort the same way it did live.
    pub fn last_ts(&self) -> u64 {
        self.ring.back().map(|r| r.env.ts).unwrap_or(0)
    }

    /// Append, stamping `(session_id, seq, ts)`.
    pub fn append(&mut self, event: SessionEvent) -> Envelope {
        self.head_seq += 1;
        let env = Envelope {
            session_id: self.session_id.clone(),
            seq: self.head_seq,
            ts: now_ms(),
            event,
        };
        // Serialised size is what the *gap* bound is measured in, so measuring the
        // retention bound the same way keeps one unit in the type.
        //
        // **Into the scratch buffer, not into a new `String`.** `to_string` built the whole JSON
        // once per event and dropped it; `to_writer` into a reused buffer is the same encode and
        // the same length with no allocation after the first few events have sized it. `clear()`
        // keeps the capacity, which is what makes the steady state free.
        self.size_scratch.clear();
        let bytes = serde_json::to_writer(&mut self.size_scratch, &env)
            .map(|()| self.size_scratch.len())
            .unwrap_or(0);
        self.ring.push_back(Retained {
            env: env.clone(),
            bytes,
        });
        self.bytes += bytes;
        self.trim();
        env
    }

    fn trim(&mut self) {
        while self.ring.len() > self.bounds.scrollback_events
            || (self.bytes > self.bounds.scrollback_bytes && self.ring.len() > 1)
        {
            if let Some(r) = self.ring.pop_front() {
                self.bytes -= r.bytes;
                self.dropped += 1;
            }
        }
    }

    /// Events with `seq > since`, and whether the whole gap could be served.
    ///
    /// Returns `None` when the gap cannot be honoured — either it runs off the back
    /// of the scrollback, or it exceeds §13.2's bound. **Resync is a normal
    /// outcome, never an error**, so the caller's job on `None` is to take a fresh
    /// snapshot, not to report a failure.
    pub fn since(&self, since: u64) -> Option<Vec<Envelope>> {
        if since > self.head_seq {
            // A head ahead of the log. Only reachable across a daemon restart that
            // lost events; a fresh snapshot is the only honest answer.
            return None;
        }
        if since + 1 < self.first_seq() {
            return None;
        }
        let mut out = Vec::new();
        let mut bytes = 0usize;
        for r in self.ring.iter().filter(|r| r.env.seq > since) {
            bytes += r.bytes;
            if out.len() >= self.bounds.max_gap_events || bytes > self.bounds.max_gap_bytes {
                return None;
            }
            out.push(r.env.clone());
        }
        Some(out)
    }

    /// Every retained event, oldest first. The fold that builds a snapshot reads
    /// this; nothing else should.
    pub fn retained(&self) -> impl Iterator<Item = &Envelope> {
        self.ring.iter().map(|r| &r.env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::warn;

    #[test]
    fn seq_is_monotonic_and_starts_at_one() {
        let mut log = SessionLog::new("s", LogBounds::default());
        assert_eq!(log.head_seq(), 0);
        assert_eq!(log.append(warn("a")).seq, 1);
        assert_eq!(log.append(warn("b")).seq, 2);
        assert_eq!(log.head_seq(), 2);
    }

    #[test]
    fn scrollback_is_bounded_and_the_drop_is_disclosed() {
        let bounds = LogBounds {
            scrollback_events: 4,
            ..LogBounds::default()
        };
        let mut log = SessionLog::new("s", bounds);
        for i in 0..10 {
            log.append(warn(&format!("w{i}")));
        }
        assert_eq!(log.retained().count(), 4);
        assert_eq!(log.dropped(), 6, "the count is the disclosure");
        assert_eq!(log.first_seq(), 7);
        // The seq of what survived is unchanged by the drop: a head that acked 7
        // still means event 7.
        assert_eq!(log.retained().next().unwrap().seq, 7);
    }

    #[test]
    fn a_gap_that_ran_off_the_back_is_resync_not_an_error() {
        let bounds = LogBounds {
            scrollback_events: 4,
            ..LogBounds::default()
        };
        let mut log = SessionLog::new("s", bounds);
        for i in 0..10 {
            log.append(warn(&format!("w{i}")));
        }
        assert!(log.since(2).is_none(), "off the back of the scrollback");
        assert_eq!(log.since(8).unwrap().len(), 2);
        assert_eq!(log.since(10).unwrap().len(), 0, "caught up is not a gap");
    }

    #[test]
    fn a_gap_over_the_bound_is_resync() {
        let bounds = LogBounds {
            max_gap_events: 3,
            ..LogBounds::default()
        };
        let mut log = SessionLog::new("s", bounds);
        for i in 0..10 {
            log.append(warn(&format!("w{i}")));
        }
        assert!(log.since(0).is_none());
        assert_eq!(log.since(7).unwrap().len(), 3);
    }

    #[test]
    fn a_head_ahead_of_the_log_resyncs_rather_than_being_told_it_is_caught_up() {
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(warn("a"));
        assert!(log.since(9).is_none());
    }
}
