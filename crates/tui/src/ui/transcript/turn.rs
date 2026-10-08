//! **A turn's footer**: how it ended, its tokens and its timings.

use crate::ui::render::{RenderConfig, sgr, wrap};
use crate::ui::*;
use letibot_sessionlog::view::TurnState;

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
    match state {
        TurnState::Running => Vec::new(),
        TurnState::Finished { finish_reason, .. } => {
            match finish_reason {
                letibot_sessionlog::event::FinishReason::Length => vec![colour(
                    cfg,
                    sgr::YELLOW,
                    &format!(
                        "── CUT SHORT — it hit the output limit mid-answer; \
                         ask it to continue"
                    ),
                )],
                letibot_sessionlog::event::FinishReason::Aborted => vec![colour(
                    cfg,
                    sgr::YELLOW,
                    &"── stopped early (aborted)".to_string(),
                )],
                // `eos` and `word` are ordinary endings and read as ordinary:
                // no line at all.
                letibot_sessionlog::event::FinishReason::Eos
                | letibot_sessionlog::event::FinishReason::Word => Vec::new(),
                // A reason nobody recognises is shown, never normalised.
                letibot_sessionlog::event::FinishReason::Other(s) => vec![colour(
                    cfg,
                    sgr::YELLOW,
                    &format!("── ended for an unrecognised reason: {s}"),
                )],
            }
        }
        TurnState::Interrupted {
            reason,
            partial_kept,
        } => vec![colour(
            cfg,
            sgr::YELLOW,
            &format!(
                "── interrupted: {reason} ({})",
                if *partial_kept {
                    "what it had written is kept"
                } else {
                    "nothing kept"
                }
            ),
        )],
        // §4.5. A failure is not an ending a turn is allowed to have, so it does
        // not read like one: red, shouted, and wrapped rather than truncated,
        // because the reason is the whole content of the event.
        TurnState::Failed {
            error,
            partial_kept,
        } => {
            let kept = if *partial_kept {
                "what it had written is kept"
            } else {
                "nothing was recorded"
            };
            wrap(&format!("── FAILED — {error} ({kept})"), cfg.width)
                .into_iter()
                .map(|l| warn_line(cfg, &l))
                .collect()
        }
    }
}
