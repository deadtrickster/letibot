//! **Small helpers the rows share**: the first sentence of a reason, a fold's word. The
//! painting they once did here is `crate::ui::rows` and rano's.

use crate::app::*;

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

pub(crate) fn fold_word(f: Fold) -> &'static str {
    match f {
        Fold::Folded => "folded",
        Fold::Open => "open",
    }
}
