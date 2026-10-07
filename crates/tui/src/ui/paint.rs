//! **Small painting helpers** shared by the widgets: dim, warn, a git colour, a fold word.

use crate::app::*;
use crate::render::{RenderConfig, sgr};
use std::borrow::Cow;

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
pub(crate) fn git_paint(cfg: &RenderConfig, role: crate::gitfield::GitRole, s: &str) -> String {
    use crate::gitfield::GitRole as R;
    match role {
        R::BranchClean => colour(cfg, sgr::GREEN, s),
        R::BranchDirty => colour(cfg, sgr::YELLOW, s),
        R::Behind | R::Ahead => colour(cfg, sgr::CYAN, s),
        R::Stash => colour(cfg, sgr::MAGENTA, s),
        R::Action => {
            if cfg.color {
                format!("{}{}{s}{}", sgr::BOLD, sgr::MAGENTA, sgr::RESET)
            } else {
                s.to_string()
            }
        }
        R::Conflict => {
            if cfg.color {
                format!("{}{}{s}{}", sgr::BOLD, sgr::RED, sgr::RESET)
            } else {
                s.to_string()
            }
        }
        R::Staged => colour(cfg, sgr::GREEN, s),
        R::Unstaged => colour(cfg, sgr::YELLOW, s),
        R::Untracked => colour(cfg, sgr::DIM, s),
    }
}

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
pub(crate) fn first_sentence(basis: &str) -> Cow<'_, str> {
    let line = basis.lines().next().unwrap_or("").trim();
    let end = line.find(". ").map(|i| i + 1).unwrap_or(line.len());
    let s = &line[..end];
    const CAP: usize = 140;
    if s.chars().count() <= CAP {
        return Cow::Borrowed(s);
    }
    let cut: String = s.chars().take(CAP).collect();
    Cow::Owned(format!("{}…", cut.trim_end()))
}

/// A result envelope's marker line: `<<<TOOL_ERROR 5ebfdef6>>>`, `<<<END_OK …>>>`.
///
/// Matched by SHAPE rather than against a list of kinds, so a kind added to
/// `letibot_tools::result::Envelope` does not start leaking here on the day it
/// lands. A body line that happens to look like one cannot exist: the envelope
/// rewrites every `<<<` in a payload to `< < <` precisely so its own markers are
/// unforgeable.
pub(crate) fn is_envelope(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("<<<") && l.ends_with(">>>") && l.len() > 6
}

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
/// **One note line, in the register its code was classified into** — R19, R29 part two.
///
/// The colour comes in rather than being decided here, because the decision is
/// `letibot_sessionlog::warning`'s and this is only where it is painted. The three are named
/// at the call site so the whole mapping is readable in one place.
pub(crate) fn note_line(cfg: &RenderConfig, sgr_code: &str, s: &str) -> String {
    colour(cfg, sgr_code, s)
}

pub(crate) fn fold_word(f: Fold) -> &'static str {
    match f {
        Fold::Folded => "folded",
        Fold::Open => "open",
    }
}
