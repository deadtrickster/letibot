//! **Waiting, measured**: how long an unfilled announcement may wait and how big a fill must be
//! to be drawn. The fill, fold and announced-rows progress itself is one line of the status row
//! now (`ui::progress_row`); the cat survives on the attach screen (`rano::agent::screens`).

#[cfg(doc)]
use crate::app::*;

/// How long a bulk announcement may stay unfilled before the head says it is not coming, in
/// milliseconds. Only a carry sets it - its bodies are published in the same loop as its
/// announcements - so the window is one delivery batch, and seconds suffice.
pub(crate) const BODY_PATIENCE: u64 = 5_000;

/// The smallest fill the head draws a bar for, in the operation's own units. A screen decision: the
/// daemon names and counts every operation and the head decides what is worth a bar. 256 sits above
/// an ordinary turn and below the smallest carry, and gates the bar only, never the sentence.
pub(crate) const MIN_FILLING: u64 = 256;

/// How long an attach may go unanswered before the waiting screen says how to get out —
/// rano's (`rano::agent::screens`), named here for the tests that drive the clock past it.
#[cfg(test)]
pub(crate) use rano::agent::screens::ATTACH_IMPATIENT;

/// The cat's frames — rano's (`rano::agent::loading`), one copy for the attach screen and the
/// fill and compaction rows — named here for the tests that pin their width and their walk.
#[cfg(test)]
pub(crate) use rano::agent::loading::{CAT_FRAMES, CAT_SLOT};
