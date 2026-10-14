//! **Waiting, measured**: how long an unfilled announcement may wait and how big a fill must be
//! to be drawn. The fill, fold and announced-rows progress itself is one line of the status row
//! now (`ui::progress_row`); the cat survives on the attach screen (`rano::agent::screens`).

#[cfg(doc)]
use crate::app::*;

/// **How long a bulk announcement may stay unfilled before the head says it is not
/// coming**, in milliseconds.
///
/// **Measured against the right case, which is why it is seconds and not minutes.** The
/// trigger is a snapshot's bulk announcement ([`Bulk`]), so the R2 slow case — a prompt
/// queued behind a running turn, which can honestly sit body-less for minutes — never
/// sets it. What remains is a carry, and a carry's bodies are published by the daemon in
/// the same loop as its announcements (`Harness::republish`, `Harness::import_opencode`),
/// so the window is one delivery batch rather than a generation.
///
/// Measured on this box's store: rows per turn over 957 turns are median 17, p90 130,
/// p99 290, max 591, and the round cadence — how long a *live* row's body has ever really
/// taken — runs to p99 108 s at the worst. The old 120 s value was calibrated against
/// exactly those, and that was the error: with the trigger fixed they are *excluded*, and
/// waiting two minutes to report a body the operator can see is missing now is the same
/// failure as never reporting it. 5 s clears any delivery lag many times over and fires
/// while the operator is still watching. (The store has no notion of a body-less row, so
/// a carry's own body gap cannot be read off it; leticl measured the live case at 31 ms
/// and chose 5 s for the same reason — the number is bounded by the mechanism, not copied
/// from a clock that measures a different thing.)
pub(crate) const BODY_PATIENCE: u64 = 5_000;

/// **The smallest fill the head draws a bar for**, in the operation's own units.
///
/// A **screen** decision — the daemon stays a pure reporter (it names every operation and
/// counts it; the head decides whether the count is worth a bar with a cat on it).
/// Measured at both ends from this box's own store, because a threshold picked by taste is
/// one the next person re-tunes on their first flash:
///
/// * **An ordinary turn's rows**: 957 turns — median 17, p90 130, p99 290, **max 591**.
/// * **A real carry**: 42 forks — smallest **448 rows**, largest 576,374.
///
/// **The two overlap** (591 > 448), so size cannot separate a turn from a carry — which is
/// precisely why the trigger is the shape of the evidence ([`Bulk`]) and not this number.
/// All this decides is whether an operation the daemon *did* name is worth a bar: 256 sits
/// above the ordinary turn's p90 and below the smallest carry ever seen here. **It gates
/// the bar only** — the sentence below it is not gated, because a three-row batch does not
/// deserve a cat but a three-row batch that never lands is exactly what the sentence is
/// for.
pub(crate) const MIN_FILLING: u64 = 256;

/// How long an attach may go unanswered before the waiting screen says how to get out —
/// rano's (`rano::agent::screens`), named here for the tests that drive the clock past it.
#[cfg(test)]
pub(crate) use rano::agent::screens::ATTACH_IMPATIENT;

/// The cat's frames — rano's (`rano::agent::loading`), one copy for the attach screen and the
/// fill and compaction rows — named here for the tests that pin their width and their walk.
#[cfg(test)]
pub(crate) use rano::agent::loading::{CAT_FRAMES, CAT_SLOT, cat_frame};
