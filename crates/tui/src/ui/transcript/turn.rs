//! A turn's footer: how it ended, its tokens and its timings.

use crate::ui::render::{RenderConfig, row_strings};
use letibot_sessionlog::view::TurnState;
use rano::agent::blocks::TurnEnd;
use rano::render::{Line, Span};
use rano::style::Role;

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
        // Ours, not rano's: a park is not an interrupt, and an interrupt keeps the turn's calls
        // and edits whatever happened to the partial answer.
        TurnState::Parked {
            model,
            until_ms,
            retry_at_ms,
        } => return notice(cfg, park_line(model, *until_ms, *retry_at_ms)),
        TurnState::Interrupted { reason, .. } if is_park_reason(reason) => {
            return notice(
                cfg,
                format!("── parked on a usage limit: {reason} - {WAYS_OUT}"),
            );
        }
        TurnState::Interrupted {
            reason,
            partial_kept,
        } => {
            let kept = if *partial_kept {
                "the partial answer is kept"
            } else {
                "no partial answer"
            };
            let line = if reason.is_empty() {
                format!("── interrupted ({kept})")
            } else {
                format!("── interrupted: {reason} ({kept})")
            };
            return notice(cfg, line);
        }
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

/// The two ways out of a park, in the status row's words.
const WAYS_OUT: &str = "esc ends the turn, /models switches";

fn notice(cfg: &RenderConfig, text: String) -> Vec<String> {
    row_strings(
        &[Line::new(vec![Span::role(text, Role::Pending)])],
        cfg.palette(),
    )
}

fn park_line(model: &str, until_ms: u64, retry_at_ms: Option<u64>) -> String {
    let now = letibot_sessionlog::event::now_ms();
    let again = match retry_at_ms {
        Some(at) => format!(", asking again at {}", local_clock(now, at)),
        None => String::new(),
    };
    format!(
        "── parked on {model}'s limit until {} (local time){again} - {WAYS_OUT}",
        local_clock(now, until_ms)
    )
}

/// The park reasons a daemon without `TurnInterrupted::park` sends, spelled by its `limit.rs`.
fn is_park_reason(reason: &str) -> bool {
    reason.starts_with("paused until ")
        || reason.starts_with("limited until ")
        || reason.starts_with("this session is paused until ")
        || (reason.starts_with("waiting ") && reason.contains(" for the reset at "))
}

/// `00:08` in the local zone, with the weekday when it is not today.
fn local_clock(now_ms: u64, ms: u64) -> String {
    let (Some(now), Some(at)) = (local_tm(now_ms), local_tm(ms)) else {
        return String::new();
    };
    let hm = format!("{:02}:{:02}", at.tm_hour, at.tm_min);
    if (now.tm_year, now.tm_yday) == (at.tm_year, at.tm_yday) {
        return hm;
    }
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    format!("{} {hm}", DAYS[at.tm_wday.rem_euclid(7) as usize])
}

fn local_tm(ms: u64) -> Option<libc::tm> {
    let secs = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`, both owned here; the `_r` form
    // does not hand back a shared static.
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    ok.then_some(tm)
}
