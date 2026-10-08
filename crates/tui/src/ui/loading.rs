//! **Waiting, drawn**: the cat while a session arrives, and the lines for a transcript still
//! filling or being compacted — drawn by `rano::agent::loading`.

use crate::app::*;
use crate::ui::render::{RenderConfig, row, row_strings};
use rano::agent::loading::{Compacting, Filling, centred};

/// One row, centred horizontally in `w` columns.
///
/// Escape-aware, because the thing being centred may already be painted: `width::width`
/// skips ANSI sequences, so a coloured cat and a plain one land in the same column.
/// A plain `chars().count()` is what puts a painted string two columns left of centre,
/// and the whole point of centring is that the eye finds it in the same place from
/// frame to frame as the string changes length — which the walking cat does.
pub(crate) fn centred_row(cfg: &RenderConfig, text: &str, w: usize) -> String {
    row(&centred(text, w), cfg.palette())
}

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

/// **One line for a fill the DAEMON named**: the cat, the bar, and the count.
///
/// The numbers are the daemon's — `what` in its own words, and `done` of `total` in the
/// `unit` it named — so this draws the fact rather than a rendering of it. The head used
/// to draw this line from the rows still lacking a body, which meant inferring the
/// *operation* from the *symptom*: four things produce body-less rows (an ordinary
/// reply, a reseat, a compaction, an import) and only some are a carry, so the line said
/// *"carrying the conversation onto the new prompt"* over every ordinary message. Only
/// the layer doing the operation knows which one it is; that is `SessionEvent::Filling`,
/// and this is the one renderer for all of them.
///
/// A free function for the same reason `filling_line` is: by the time the tail is
/// assembled, `screen` has already borrowed `self` mutably.
pub(crate) fn filling_line(
    what: &str,
    unit: &str,
    done: u64,
    total: u64,
    now_ms: u64,
    cfg: &RenderConfig,
) -> Vec<String> {
    let f = Filling {
        what: what.to_string(),
        unit: unit.to_string(),
        done,
        total,
        now_ms,
    };
    row_strings(&f.lines(cfg.width), cfg.palette())
}

/// **A fold, in the compaction's own units** — the renderer for
/// [`SessionEvent::CompactionProgress`](letibot_sessionlog::SessionEvent::CompactionProgress).
///
/// A free function for `filling_line`'s reason: by the time the tail is assembled,
/// `screen` has already borrowed `self` mutably.
///
/// **Two phases, because a fold has two and they are minutes apart.** While the server
/// is still reading the half's prompt it draws the prefill bar the ordinary turn draws —
/// the same three-valued bar, so a reader who has watched one recognises this one — and
/// the count is *read*. Once it starts writing, the count is *written* and the bar is
/// gone: there is no total to draw a fraction of, which is why that number is a count and
/// not a percentage. A bar that invented a total would be an indicator that is not the
/// fact.
pub(crate) fn compacting_line(c: &CompactionLine, now_ms: u64, cfg: &RenderConfig) -> Vec<String> {
    let line = Compacting {
        half: c.half,
        halves: c.halves,
        prompt_tokens: c.prompt_tokens,
        processed: c.processed,
        written: c.written,
        unit: c.unit.to_string(),
        now_ms,
    };
    row_strings(&line.lines(cfg.width), cfg.palette())
}

/// When the waiting frame starts naming the way out, in milliseconds.
///
/// Under it the cat is a cat and the wait is usually over in a few hundred
/// milliseconds; over it something is wrong, and the operator should be told the escape
/// hatch exists rather than having to discover it. The wait loop in `letibot-tui`'s
/// `main` is what makes the keys live — the first version of that screen did not read
/// them at all, so the hint bar under it named a key that did nothing.
pub(crate) const ATTACH_IMPATIENT: u64 = 2_000;

#[cfg(test)]
pub(crate) use rano::agent::loading::CAT_FRAMES;
/// The cat, its slot and its frame at a time — rano's (`rano::agent::loading`), one copy for
/// the attach screen and the fill and compaction rows.
pub(crate) use rano::agent::loading::{CAT_SLOT, cat_frame};
