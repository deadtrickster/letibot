//! What one session's harness holds, and the ceiling it is held to. `memwatch` says what a
//! daemon's HUB holds (the event log, the view, each head's queue) and nothing about the
//! structures on the harness: the in-memory transcript, the ledger's token region, the
//! authorisation trail. Those are the biggest per-session terms, so the harness registers a
//! [`FootprintSource`] on its hub at open and the hub's census carries a [`Footprint`]. The
//! numbers are restated rather than walked: the census runs on the watchdog's thread, the
//! transcript belongs to the thread that runs the session, and a lock taken from the census
//! is the cycle `ActivitySource` documents.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use letibot_sessionlog::hub::{Footprint, FootprintSource};

use crate::harness::{TRAIL_SCAN, TrailMirror, UTTERANCE_CHARS};

/// Bytes of TEXT one token of the window may carry. A real vocabulary averages ~3.5; a byte
/// vocabulary (`Vocab::bytes`, what a provider session with no GGUF opens on) is 1. Eight is
/// above both, and the slack is for the framing a row's JSON adds around its text.
const TEXT_BYTES_PER_TOKEN: usize = 8;

/// Bytes of token REGION one token of the window may cost: `TokenId` is a `u32` (4) and the
/// region's capacity doubles (×2), so a region is at most twice its tokens.
const REGION_BYTES_PER_TOKEN: usize = 8;

/// The most a UTF-8 character costs, for the trail's clip.
const BYTES_PER_CHAR: usize = 4;

/// The trail's ceiling: the utterances it keeps, each clipped to what it will ever say.
/// `TrailMirror::say` keeps at most [`TRAIL_SCAN`] and clips each to [`UTTERANCE_CHARS`]
/// characters, so this is a bound the code holds rather than a hope.
pub const fn trail_ceiling() -> usize {
    TRAIL_SCAN * UTTERANCE_CHARS * BYTES_PER_CHAR
}

/// The floor under a computed ceiling.
pub const MIN_CEILING: usize = 64 << 20;

/// A session with no wall has no compaction, so nothing bounds its transcript.
/// This is a tripwire and it says so: a session past it grows until it is closed.
pub const NO_WALL_CEILING: usize = 64 << 20;

/// The ceiling for a session with this context window, in tokens. See the module docs for the
/// arithmetic and what each constant is worth.
pub fn ceiling_for(window: Option<u64>) -> usize {
    match window {
        Some(w) => {
            let per_token = TEXT_BYTES_PER_TOKEN + REGION_BYTES_PER_TOKEN;
            ((w as usize).saturating_mul(per_token) + trail_ceiling()).max(MIN_CEILING)
        }
        None => NO_WALL_CEILING,
    }
}

/// The two numbers the census cannot walk for itself, restated by the thread that
/// owns them. Atomics rather than a lock: the reader is the memory watchdog, and
/// a lock here would be one a turn holds, taken from the census.
#[derive(Debug)]
pub struct Gauges {
    items: AtomicUsize,
    items_text_bytes: AtomicUsize,
    ledger_region_bytes: AtomicUsize,
    ledger_tokens: AtomicUsize,
    ceiling_bytes: AtomicUsize,
}

impl Gauges {
    /// A gauge set for a session whose ceiling is `ceiling_bytes`. The values start at zero and
    /// are wrong until the harness's first [`restate`](Self::restate), which is at open.
    pub fn new(ceiling_bytes: usize) -> Arc<Gauges> {
        Arc::new(Gauges {
            items: AtomicUsize::new(0),
            items_text_bytes: AtomicUsize::new(0),
            ledger_region_bytes: AtomicUsize::new(0),
            ledger_tokens: AtomicUsize::new(0),
            ceiling_bytes: AtomicUsize::new(ceiling_bytes),
        })
    }

    /// Restate the transcript and the region from what the harness holds now. A full walk each
    /// time and never an increment: an increment that misses a site under-reports for ever, and
    /// a restatement that misses one is only as stale as the last row.
    pub fn restate(&self, items: usize, text_bytes: usize, region_bytes: usize, tokens: usize) {
        self.items.store(items, Ordering::Relaxed);
        self.items_text_bytes.store(text_bytes, Ordering::Relaxed);
        self.ledger_region_bytes
            .store(region_bytes, Ordering::Relaxed);
        self.ledger_tokens.store(tokens, Ordering::Relaxed);
    }

    /// The ceiling in force, which is not fixed at open: `retune_window` and a
    /// `/models` switch both rewrite `Config::context_window` mid-life, and a
    /// ceiling left at the window the session opened on bounds the wrong number.
    pub fn ceiling(&self) -> usize {
        self.ceiling_bytes.load(Ordering::Relaxed)
    }

    /// Restate it; see [`ceiling`](Self::ceiling) for why it moves.
    pub fn set_ceiling(&self, ceiling_bytes: usize) {
        self.ceiling_bytes.store(ceiling_bytes, Ordering::Relaxed);
    }

    /// The footprint these gauges describe, with the trail left at zero; see
    /// [`of`] for the whole answer.
    pub fn read(&self) -> Footprint {
        Footprint {
            items: self.items.load(Ordering::Relaxed),
            items_text_bytes: self.items_text_bytes.load(Ordering::Relaxed),
            ledger_region_bytes: self.ledger_region_bytes.load(Ordering::Relaxed),
            ledger_tokens: self.ledger_tokens.load(Ordering::Relaxed),
            trail_bytes: 0,
            trail_utterances: 0,
            ceiling_bytes: self.ceiling(),
        }
    }
}

/// The whole footprint: the restated numbers plus the trail, which is read live. The trail
/// is not in the gauges because it does not need to be:
pub fn of(gauges: &Gauges, trail: &TrailMirror) -> Footprint {
    let (trail_bytes, trail_utterances) = trail.held();
    Footprint {
        trail_bytes,
        trail_utterances,
        ..gauges.read()
    }
}

/// The source a hub's census asks: the gauges plus the trail.
pub fn source(gauges: Arc<Gauges>, trail: Arc<TrailMirror>) -> FootprintSource {
    Arc::new(move || of(&gauges, &trail))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The arithmetic is the claim, so it is asserted rather than described: a window's text and
    /// its region are each bounded by the window, and the floor is what a small window gets.
    #[test]
    fn the_ceiling_is_the_windows_arithmetic_with_a_floor_under_it() {
        // 32 768 tokens: 512 KiB of text-and-region arithmetic + 144 KiB of trail, far under the
        // 64 MiB floor, so the floor wins.
        assert_eq!(ceiling_for(Some(32_768)), MIN_CEILING);
        // 200 000 tokens: 3 344 000, still under the 64 MiB floor, so the floor wins. A long
        // session's rows carry more than the per-token arithmetic sees, which is why the floor
        // is what it is.
        assert_eq!(ceiling_for(Some(200_000)), MIN_CEILING);
        // The crossover: (64 MiB − 144 KiB) / 16 ≈ 4 172 588 tokens, so a 4 100 000-token
        // window is still the floor and a 4 300 000-token one is not.
        assert_eq!(ceiling_for(Some(4_100_000)), MIN_CEILING);
        assert!(ceiling_for(Some(4_300_000)) > MIN_CEILING);
        // A session with no wall gets the tripwire, and the floor no longer exceeds it.
        assert_eq!(ceiling_for(None), NO_WALL_CEILING);
        assert_eq!(NO_WALL_CEILING, MIN_CEILING);
    }

    /// The trail's ceiling is the product of the two caps `trail` already read, so a session
    /// cannot hold more than this however much the operator says.
    #[test]
    fn the_trails_ceiling_is_its_two_caps_multiplied() {
        assert_eq!(trail_ceiling(), 60 * 600 * 4);
        assert!(trail_ceiling() < MIN_CEILING);
    }
}
