//! **A turn's footer**: how it ended, its tokens and its timings.

use crate::ui::render::{RenderConfig, row_strings};
use letibot_sessionlog::view::TurnState;
use rano::agent::blocks::TurnEnd;

/// How a turn ended, said in words rather than in the wire's vocabulary.
///
/// The footer carries **only the ending that is news**. Its stats line —
/// prompt, cache, rate, wall time, output — moved to the session header, where
/// the rest of the turn's numbers live; an ordinary ending (`eos`, `word`) now
/// leaves the body with no footer line at all, because `── 1.2k out` hovering
/// above the composer was a settled fact occupying the row a live fact used to
/// have to earn. What stays is the case §5.7's rule is about: `length` is not a
/// normal ending, it means the answer was cut off mid-sentence, and truncation
/// is never folded into success — a display that lets it read like `eos` folds
/// it at the last possible moment.
pub(crate) fn turn_footer(cfg: &RenderConfig, state: &TurnState) -> Vec<String> {
    use letibot_sessionlog::event::FinishReason as F;
    let end = match state {
        TurnState::Running => TurnEnd::Quiet,
        TurnState::Finished { finish_reason, .. } => match finish_reason {
            F::Length => TurnEnd::CutShort,
            F::Aborted => TurnEnd::Aborted,
            F::Eos | F::Word => TurnEnd::Quiet,
            F::Other(s) => TurnEnd::Other(s.clone()),
        },
        TurnState::Interrupted {
            reason,
            partial_kept,
        } => TurnEnd::Interrupted {
            reason: reason.clone(),
            kept: *partial_kept,
        },
        TurnState::Failed {
            error,
            partial_kept,
        } => TurnEnd::Failed {
            error: error.clone(),
            kept: *partial_kept,
        },
    };
    row_strings(&end.lines(cfg.width), cfg.palette())
}
