//! **What one session's HARNESS holds, and the ceiling it is held to** — the other half of R66.
//!
//! `memwatch` could say what a daemon's HUB holds (`HubCensus`: the event log, the view, each
//! head's queue) and nothing about the structures on the harness — the in-memory transcript, the
//! ledger's token region, the authorisation trail — which are the biggest per-session terms and
//! the ones a session's own work grows. R66's daemon reached 113 GB and the census could only
//! print an `rss` line with no session's name on it.
//!
//! So the harness registers a [`FootprintSource`] on its hub at open, and the hub's census
//! carries a [`Footprint`]. See [`Footprint`]'s table for exactly what each term counts and what
//! it deliberately leaves out.
//!
//! # Why the numbers are restated rather than walked
//!
//! The census runs on the memory watchdog's thread; the transcript belongs to the thread that
//! runs the session, `&mut`, and there is no lock to take. A source that walked `Session::items`
//! would need one — and taking a lock a turn holds from the census is the cycle
//! [`letibot_sessionlog::hub::ActivitySource`] documents at length (the queue's 8½-hour
//! deadlock). So the thread that OWNS the transcript restates the sums into atomics at every
//! point it changes ([`Harness::reconcile`](crate::harness::Harness)), and the source reads them.
//! Each restatement is a full walk of the rows, not an increment, so a missed restatement is a
//! stale number and never a wrong one; the trail is read live under its own short lock, because
//! it is already an `Arc<Mutex<…>>` and its cap is a hundred and forty kilobytes.
//!
//! # The ceiling, and what justifies each number
//!
//! A session's transcript is bounded by its CONTEXT WINDOW: `plan_cut` forks the conversation
//! when the resident count reaches the window, so what a session holds is at most a window's
//! worth of rows plus whatever the turn that crossed the wall was holding. Two conversions:
//!
//! * **text** — a token of a real vocabulary is about 3.5 bytes of text; 8 is the round number
//!   above it, and it also covers a byte vocabulary's 1 byte per token with room for a row's
//!   JSON framing.
//! * **the token region** — 4 bytes per token (`TokenId` is a `u32`) and its capacity DOUBLES,
//!   so a region may hold twice the tokens appended: 8 bytes per token of the window.
//!
//! plus the trail's own cap ([`trail_ceiling`]), which is a count and a clip and not a window's.
//! A session with no window (`context_window: None` — a one-shot, a test, a child that inherited
//! none) has no compaction to bound it, and gets [`NO_WALL_CEILING`]: a tripwire, stated as one.
//!
//! For this box's ordinary session — a 32 768-token window — that is 512 KiB + 144 KiB, and the
//! [`MIN_CEILING`] floor wins. For a 200 000-token window it is 3.3 MiB. Multiplied by the 492
//! sessions one repository's daemon held before R68.3, the ceiling sums to ~2 GB: which is the
//! point of the arithmetic, and it says R66's 113 GB was never *sessions at their ceiling*.

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
///
/// This is a bound the code now HOLDS rather than a hope: `TrailMirror::say` keeps at most
/// [`TRAIL_SCAN`] utterances and clips each to [`UTTERANCE_CHARS`] characters — both of which
/// were already the only values `trail()` ever read, so nothing observable changed. Before that
/// it kept every user-side text for the life of the session, unbounded, and the census could
/// only have reported a number with no ceiling over it.
pub const fn trail_ceiling() -> usize {
    TRAIL_SCAN * UTTERANCE_CHARS * BYTES_PER_CHAR
}

/// The floor under a computed ceiling. A session whose window is small still holds its stable
/// prefix: the region alone is 216 KiB for a 54 000-token prompt (a byte vocabulary's message
/// zero, `Vocab::bytes`), and the rows a turn appends sit on top of it — so the floor is a
/// megabyte, and every ordinary session sat well under it — until a session that ran eighteen
/// hours at a 1 000 000-token window measured 22 MiB against the ~15 MiB the per-token arithmetic
/// gave it (a long session's rows carry tool-call metadata and reasoning the 16-bytes-per-token
/// estimate does not see). 64 MiB is the floor the no-wall tripwire already uses, so a session
/// with a wall now has at least the headroom of one without (operator: "15mb is on a small side?
/// bump it to 64").
pub const MIN_CEILING: usize = 64 << 20;

/// **A session with no wall has no compaction, so nothing bounds its transcript.** This is a
/// tripwire and it says so: a session past it is one that will grow until it is closed.
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

/// **The two numbers the census cannot walk for itself, restated by the thread that owns them.**
///
/// Atomics rather than a lock, because the reader is the memory watchdog and the writer is the
/// session's own thread: a lock here would be a lock a turn holds, taken from the census.
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
    /// are wrong until the harness's first [`restate`](Self::restate) — which is at open.
    pub fn new(ceiling_bytes: usize) -> Arc<Gauges> {
        Arc::new(Gauges {
            items: AtomicUsize::new(0),
            items_text_bytes: AtomicUsize::new(0),
            ledger_region_bytes: AtomicUsize::new(0),
            ledger_tokens: AtomicUsize::new(0),
            ceiling_bytes: AtomicUsize::new(ceiling_bytes),
        })
    }

    /// **Restate the transcript and the region from what the harness holds now.**
    ///
    /// A full walk each time and never an increment: an increment that misses a site under-
    /// reports for ever, and a restatement that misses one is only as stale as the last row.
    pub fn restate(&self, items: usize, text_bytes: usize, region_bytes: usize, tokens: usize) {
        self.items.store(items, Ordering::Relaxed);
        self.items_text_bytes.store(text_bytes, Ordering::Relaxed);
        self.ledger_region_bytes
            .store(region_bytes, Ordering::Relaxed);
        self.ledger_tokens.store(tokens, Ordering::Relaxed);
    }

    /// **The ceiling in force**, which is not fixed at open: `retune_window` and a `/models`
    /// switch both rewrite `Config::context_window` mid-life, and a ceiling that stayed at the
    /// window the session OPENED on would be a bound over the wrong number. Restated with
    /// everything else in [`Harness::restate_footprint`](crate::harness::Harness).
    pub fn ceiling(&self) -> usize {
        self.ceiling_bytes.load(Ordering::Relaxed)
    }

    /// Restate it — see [`ceiling`](Self::ceiling) for why it moves.
    pub fn set_ceiling(&self, ceiling_bytes: usize) {
        self.ceiling_bytes.store(ceiling_bytes, Ordering::Relaxed);
    }

    /// **The footprint these gauges describe**, with the trail left at zero — see [`of`] for
    /// the whole answer.
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

/// **The whole footprint**: the restated numbers plus the trail, which is read live.
///
/// The trail is not in the gauges because it does not need to be — it is already shared behind
/// its own short-lived lock, and its cap makes reading it a walk over at most [`TRAIL_SCAN`]
/// strings.
pub fn of(gauges: &Gauges, trail: &TrailMirror) -> Footprint {
    let (trail_bytes, trail_utterances) = trail.held();
    Footprint {
        trail_bytes,
        trail_utterances,
        ..gauges.read()
    }
}

/// **The source a hub's census asks** — the gauges plus the trail.
///
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
        // 200 000 tokens: 3 344 000 — still under the 64 MiB floor, so the floor wins. A long
        // session at this window measured 22 MiB against the 15 MiB the old arithmetic gave it,
        // which is why the floor is 64 MiB now.
        assert_eq!(ceiling_for(Some(200_000)), MIN_CEILING);
        // The crossover: (64 MiB − 144 KiB) / 16 ≈ 4 172 588 tokens, so a 4 100 000-token
        // window is still the floor and a 4 300 000-token one is not.
        assert_eq!(ceiling_for(Some(4_100_000)), MIN_CEILING);
        assert!(ceiling_for(Some(4_300_000)) > MIN_CEILING);
        // A session with no wall gets the tripwire, and the floor no longer exceeds it.
        assert_eq!(ceiling_for(None), NO_WALL_CEILING);
        assert_eq!(NO_WALL_CEILING, MIN_CEILING);
    }

    /// The trail's ceiling is the product of the two caps `trail()` already read, so a session
    /// cannot hold more than this however much the operator says.
    #[test]
    fn the_trails_ceiling_is_its_two_caps_multiplied() {
        assert_eq!(trail_ceiling(), 60 * 600 * 4);
        assert!(trail_ceiling() < MIN_CEILING);
    }
}
