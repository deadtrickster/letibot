//! **Small painting helpers** shared by the widgets: dim, warn, a git colour, a fold word.

use crate::app::*;
use crate::ui::render::{RenderConfig, sgr};

pub(crate) fn colour(cfg: &RenderConfig, code: &str, s: &str) -> String {
    if cfg.color {
        format!("{code}{s}{}", sgr::RESET)
    } else {
        s.to_string()
    }
}

/// **One git segment, painted for its role** — leticl's `+git-styles+`, one colour per
/// segment, chosen to say what the segment SAYS: green branch when the tree is clean and
/// yellow when it is not (the one fact a person reads at a glance), staged green, unstaged
/// yellow, conflicts red and bold because nothing else on that row is a demand, the action
/// magenta and bold, untracked dim because it is usually noise.
///
/// `colour`'s one-code shape is kept — bold is spelled as a second SGR rather than a composed
/// `1;35`, the same way `sgr::BOLD_ITALIC` composes exactly the pairs that recur — because
/// two escapes reset once and read the same as the composed ones in every terminal this row
/// has been drawn on.
/// The first sentence of a refusal's reasoning, capped.
///
/// Layer A's `basis` is written for the model: it names every construct it could
/// not resolve, one indented paragraph each, and ends with the instruction to
/// re-issue. The operator needs the first clause of that — *what happened* — and
/// nothing else, because the rest is already in front of them as the tool result.
///
/// Cut at the first sentence end, then hard-capped: a "sentence" written without a
/// full stop is still not a paragraph a status line should carry.
/// **Borrows when the first sentence is already in the input**, which it usually is.
///
/// A `String` return meant a copy of a slice of the caller's own string — the whole sentence, for
/// a function whose job is to point at part of one. It is called per tool row drawn, and the
/// `Owned` branch is only the over-long case (past `CAP`), where the truncation genuinely has to
/// build something new.
pub(crate) use rano::agent::text::first_sentence;

/// A result envelope's marker line: `<<<TOOL_ERROR 5ebfdef6>>>`, `<<<END_OK …>>>` — rano's
/// rule now (the tool row filters by it), here for the tests that pin its shape.
#[cfg(test)]
pub(crate) use rano::agent::text::is_envelope;

pub(crate) fn dim(cfg: &RenderConfig, s: &str) -> String {
    colour(cfg, sgr::DIM, s)
}

pub(crate) fn warn_line(cfg: &RenderConfig, s: &str) -> String {
    colour(cfg, sgr::RED, s)
}

/// **The line a routine warning is drawn as** — the other register, and not a quieter
/// version of the one above.
///
/// `head-parity-2026-09-21.md` **R19**, the operator's ruling of 2026-09-22: *"routine is
/// painted as failure"* — `compacted`, `auto_compact`, `daemon_stopping` and a fourth
/// arrived on a head that had just attached, all four in the red a denial gets, and four
/// notes read as a wall. **A housekeeping notice and a refused call must not look
/// alike**, and the argument is not taste: an operator met by a red block on every
/// restart learns to skip it, and the block is where a real denial lives.
///
/// The difference is the whole of it: no `!`, no red — the bullet the head already uses
/// for a line that is dim and factual — and the code is kept, because it is the word a
/// reader greps the log for. Which codes are routine is [`letibot_sessionlog::warning`]'s
/// table and not this head's opinion: the codes are the log's vocabulary and both heads
/// render them.
pub(crate) fn fold_word(f: Fold) -> &'static str {
    match f {
        Fold::Folded => "folded",
        Fold::Open => "open",
    }
}
