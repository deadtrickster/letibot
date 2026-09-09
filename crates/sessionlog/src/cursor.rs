//! The read mark, shaped so the reusable bug cannot be written.
//!
//! §13.2b, on `inbox.go:421-428`:
//!
//! > **The mark advances over everything READ, not everything delivered** — called
//! > the single most reusable bug in that repo, and it is one we would otherwise
//! > hit exactly: *a filtering consumer that advances only over what it kept
//! > rereads its own output forever.*
//!
//! Two words in that sentence do a lot of work, so to be exact about which of the
//! three quantities is the right one:
//!
//! | quantity | what it is | correct mark? |
//! |---|---|---|
//! | delivered | pushed into the head's queue | **no** — not yet consumed; a crash loses them |
//! | read | pulled out of the queue and examined | **yes** |
//! | kept | survived the head's filter | **no** — this is the bug |
//!
//! So the mark is "read": everything the head has looked at, whether or not it
//! displayed it. [`Batch`] is the only thing that produces one, and it exposes
//! [`Batch::last_seq`] — the last seq *in the batch* — and no accessor at all for
//! "the last seq I kept". A head cannot write the bug because there is nothing to
//! write it with.
//!
//! And the second half of the same rule: the ack is sent **after** the head has
//! written the batch out, not on receipt (`inbox.go:489-493`). A crash then costs a
//! duplicate, never a silence. [`Batch::ack_after_render`] is named for it.

use crate::event::Envelope;
use crate::protocol::Ack;

/// A run of events handed to a head in one go.
///
/// Consuming the batch is what makes it "read". The head then filters, renders what
/// it kept, and only then acks.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    events: Vec<Envelope>,
}

impl Batch {
    pub fn new(events: Vec<Envelope>) -> Self {
        Batch { events }
    }

    pub fn events(&self) -> &[Envelope] {
        &self.events
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// The last seq in the batch — everything the head has now read.
    ///
    /// There is no `last_kept_seq`, and adding one would be adding the bug.
    pub fn last_seq(&self) -> Option<u64> {
        self.events.last().map(|e| e.seq)
    }

    /// Build the ack. Call this **after** the rendered events are on the screen or
    /// on the wire, never before.
    ///
    /// `rendered + filtered` must equal the batch length: a head that loses track of
    /// an event has a broken filter, and this is where it becomes visible instead of
    /// looking like a quiet session.
    pub fn ack_after_render(&self, rendered: u64, filtered: u64) -> Option<Ack> {
        let seq = self.last_seq()?;
        debug_assert_eq!(
            rendered + filtered,
            self.events.len() as u64,
            "BUG: {} events read, {rendered} rendered + {filtered} filtered. An event \
             that is neither is an event nobody can account for.",
            self.events.len()
        );
        Some(Ack {
            seq,
            rendered,
            filtered,
        })
    }
}

/// The daemon's record of where one head has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadMark {
    /// The highest seq this head has acked. Monotonic.
    pub seq: u64,
    /// Cumulative, across the life of the attachment.
    pub rendered: u64,
    /// Cumulative. A head that has been busy and rendered nothing is a *different*
    /// state from a head that has seen nothing, and this is the field that tells
    /// them apart.
    pub filtered: u64,
    /// Acks that tried to go backwards. Counted rather than rejected: a duplicate
    /// ack after a reconnect is expected and harmless, but a stream of them means a
    /// head is confused and somebody should be able to see that.
    pub stale_acks: u64,
}

impl ReadMark {
    /// Apply an ack. Never moves backwards.
    pub fn apply(&mut self, ack: Ack) {
        if ack.seq <= self.seq {
            self.stale_acks += 1;
            return;
        }
        self.seq = ack.seq;
        self.rendered += ack.rendered;
        self.filtered += ack.filtered;
    }

    /// Everything this head has accounted for.
    pub fn accounted(&self) -> u64 {
        self.rendered + self.filtered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{LogBounds, SessionLog};
    use crate::testing::{progress, warn};

    fn batch_of(n: u64) -> Batch {
        let mut log = SessionLog::new("s", LogBounds::default());
        let mut v = Vec::new();
        for i in 0..n {
            // Alternate two kinds so a filter has something to drop.
            v.push(if i % 2 == 0 {
                log.append(warn(&format!("w{i}")))
            } else {
                log.append(progress("t1"))
            });
        }
        Batch::new(v)
    }

    #[test]
    fn a_filtering_consumer_does_not_reread_its_own_output_forever() {
        // A head that keeps nothing. If the mark advanced over what it kept, the
        // mark would never move and the same events would arrive again on the next
        // resume, forever.
        let b = batch_of(10);
        let ack = b.ack_after_render(0, 10).unwrap();
        assert_eq!(ack.seq, 10, "the mark advances over everything READ");
        assert_eq!(ack.rendered, 0);
        assert_eq!(ack.filtered, 10);

        let mut mark = ReadMark::default();
        mark.apply(ack);
        assert_eq!(mark.seq, 10);
        // The resume gap after this ack is empty, which is the whole point.
        let mut log = SessionLog::new("s", LogBounds::default());
        for i in 0..10 {
            log.append(warn(&format!("w{i}")));
        }
        assert!(log.since(mark.seq).unwrap().is_empty());
    }

    #[test]
    fn busy_and_none_of_it_was_for_me_is_distinguishable_from_quiet() {
        let mut busy = ReadMark::default();
        busy.apply(Batch::new(vec![]).ack_after_render(0, 0).unwrap_or(Ack {
            seq: 10,
            rendered: 0,
            filtered: 10,
        }));
        let quiet = ReadMark::default();
        assert_ne!(busy.filtered, quiet.filtered);
        assert_eq!(busy.accounted(), 10);
        assert_eq!(quiet.accounted(), 0);
    }

    #[test]
    fn there_is_no_way_to_ack_the_last_kept_seq() {
        // Enforced by absence: this test is the reminder. `Batch` exposes
        // `last_seq` only, and `ack_after_render` derives `seq` from it rather than
        // taking one.
        // The needle is assembled at runtime so that this assertion does not match
        // itself — a source-scanning test that finds its own text always passes.
        let needle = format!("fn last{}kept_seq", "_");
        assert!(!include_str!("cursor.rs").contains(&needle));
    }

    #[test]
    fn a_replayed_batch_after_a_crash_costs_a_duplicate_not_a_silence() {
        // Ack after render: if the head dies between rendering event 7 and acking,
        // the mark is still 4 and events 5..7 arrive again. Duplicates, not a gap.
        let mut mark = ReadMark::default();
        mark.apply(Ack {
            seq: 4,
            rendered: 4,
            filtered: 0,
        });
        let mut log = SessionLog::new("s", LogBounds::default());
        for i in 0..7 {
            log.append(warn(&format!("w{i}")));
        }
        let gap = log.since(mark.seq).unwrap();
        assert_eq!(gap.len(), 3, "5, 6, 7 — including the one it had rendered");
    }

    #[test]
    fn the_mark_never_goes_backwards_and_says_when_it_was_asked_to() {
        let mut mark = ReadMark::default();
        mark.apply(Ack {
            seq: 9,
            rendered: 9,
            filtered: 0,
        });
        mark.apply(Ack {
            seq: 4,
            rendered: 4,
            filtered: 0,
        });
        assert_eq!(mark.seq, 9);
        assert_eq!(mark.rendered, 9, "a stale ack is not counted twice");
        assert_eq!(mark.stale_acks, 1);
    }
}
