//! Small helpers the rows share: the first sentence of a reason, a fold's word. Painting is
//! `crate::ui::rows` and rano's.

use crate::app::*;

/// The first sentence of a refusal's reasoning, capped: layer A's `basis` is written for the model,
/// and the operator needs the first clause, the rest already being in front of them as the tool
/// result. Cut at the first sentence end, then hard-capped, since a long clause is not a paragraph.
pub(crate) use rano::agent::text::first_sentence;

/// A result envelope's marker line: `<<<TOOL_ERROR 5ebfdef6>>>`, `<<<END_OK ...>>>` - rano's rule
/// now (the tool row filters by it), here for the tests that pin its shape.
#[cfg(test)]
pub(crate) use rano::agent::text::is_envelope;

pub(crate) fn fold_word(f: Fold) -> &'static str {
    match f {
        Fold::Folded => "folded",
        Fold::Open => "open",
    }
}
