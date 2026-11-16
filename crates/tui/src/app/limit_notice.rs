//! The limit notice: a move, park or return a usage limit caused, held above the composer until
//! the operator dismisses it. It reads the note `/notes` already holds, so `ctrl-n` and
//! `/notes dismiss N` take it down and the conversation gets no row of its own.

use super::*;
use rano::agent::status as facts;

/// The warning codes that raise the limit notice: a session that moved models, parked, or came back.
pub(crate) const LIMIT_NOTICES: &[&str] = &[
    "model_fallback",
    "limit_park",
    "limit_resumed",
    "limit_returned",
];

impl App {
    /// The newest limit note the reader has not dismissed, with how many older ones are also up.
    pub(crate) fn limit_notice(&self) -> Option<(String, usize)> {
        let mut up = self.notes.iter().filter_map(|(_, n)| match n {
            Note::Warned(w) if LIMIT_NOTICES.contains(&w.code.as_str()) && !self.is_retired(n) => {
                Some(w)
            }
            _ => None,
        });
        let newest = up.next_back()?;
        // The parked turn's own line says it, in local time.
        if newest.code == "limit_park" && self.turn_parked() {
            return None;
        }
        Some((limit_words(&newest.code, &newest.detail), up.count()))
    }

    /// The turn in view is parked on a limit, and its own line says so.
    pub(crate) fn turn_parked(&self) -> bool {
        matches!(
            self.turn.as_ref().and_then(|t| t.state.as_ref()),
            Some(letibot_sessionlog::view::TurnState::Parked { .. })
        )
    }

    /// The limit notice as resident lines, in the warning register.
    pub(crate) fn limit_notice_line(&self, w: usize) -> Vec<String> {
        let Some((said, older)) = self.limit_notice() else {
            return Vec::new();
        };
        let more = if older > 0 {
            format!(" · {older} more in /notes")
        } else {
            String::new()
        };
        let said = format!("{said}{more} · ctrl-n dismisses");
        crate::ui::render::row_strings(&facts::warning(&said, w), self.cfg.palette())
    }
}

/// One line for a limit warning: `glm/glm-5.3 hit its limit (resets 07:15:00 UTC) - now on
/// deepseek/deepseek-flash`. A sentence of a shape not read here keeps its first clause.
pub(crate) fn limit_words(code: &str, detail: &str) -> String {
    let flat = crate::ui::transcript::line_cards::flat(detail);
    let read = match code {
        "model_fallback" => moved(&flat),
        "limit_park" => parked(&flat),
        "limit_returned" => returned(&flat),
        _ => None,
    };
    read.unwrap_or_else(|| {
        let first = crate::ui::transcript::line_cards::sentences(&flat)
            .into_iter()
            .next()
            .unwrap_or_default()
            .to_string();
        crate::ui::transcript::notice_card::short(&first, 160)
    })
}

/// `[fallback] M reset AT is past wait_if_within W - on NEXT until then.`, or the outage's
/// `the model at M is out - ... session moves to NEXT, the first name ...`.
fn moved(s: &str) -> Option<String> {
    if let Some(rest) = s.strip_prefix("[fallback] ") {
        let (model, rest) = rest.split_once(" reset ")?;
        let (at, rest) = rest.split_once(" is past wait_if_within ")?;
        let (_, rest) = rest.split_once(" - on ")?;
        let (next, _) = rest.split_once(" until then")?;
        return Some(format!(
            "{model} hit its limit (resets {at}) - now on {next}"
        ));
    }
    let rest = s.strip_prefix("the model at ")?;
    let (model, _) = rest.split_once(" is out")?;
    let (_, rest) = rest.split_once("session moves to ")?;
    let (next, _) = rest.split_once(", the first name")?;
    Some(format!("{model} is out - now on {next}"))
}

/// `M said `...`, so this session is WORDS. Nothing is lost ...`
fn parked(s: &str) -> Option<String> {
    let (model, rest) = s.split_once(" said `")?;
    let (_, rest) = rest.rsplit_once("`, so this session is ")?;
    let words = rest.split(". ").next()?.trim_end_matches('.');
    Some(format!("{model} hit its limit - {words}"))
}

/// `the limit on M lifted: ...`, the switch back that took.
fn returned(s: &str) -> Option<String> {
    let rest = s.strip_prefix("the limit on ")?;
    let (model, _) = rest.split_once(" lifted: ")?;
    Some(format!("{model}'s limit lifted - back on {model}"))
}
