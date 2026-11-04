//! A turn's footer: how it ended, its tokens and its timings.

use crate::ui::render::{RenderConfig, row_strings};
use letibot_sessionlog::view::TurnState;
use rano::agent::blocks::TurnEnd;

/// How a turn ended, said in words rather than in the wire's vocabulary.
/// Only an ending that is news is drawn: `length` means the answer was cut off,
/// so it must never read like a clean `eos`.
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
