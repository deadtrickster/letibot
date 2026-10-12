//! **A stale prompt's note, drawn as ONE line** — what changed, and the daemon's whole sentence
//! one click away.
//!
//! The operator, pasting what pg-noop's head drew after its restart — the `prefix_stale` note
//! as three wrapped rows and a `… +1 lines · /notes` seam: *"these gray walls are hilarious …
//! make them the usual one liners"*.
//!
//! ```text
//! prompt stale · seated 09-20 17:51 · new: task_resume · click for details
//! prompt stale · seated 09-20 17:51 · instructions changed · new: task_resume, pkill · gone: job_wait · changed: todo_write · click for details
//! ```
//!
//! **The line is a pointer, not the decision.** The reseat card (`app::reseat`) is where the
//! operator chooses — `alt-r` reseats, `alt-p` shows the card — and it stays on the screen while
//! the prompt is stale. The note is the record that it was said, so it says what changed and
//! where to decide, and a click opens the sentence whole.
//!
//! The sentence is `harness::stale_prefix_sentence` in harnessd; the parser keys on its opening
//! (`session ID was seated WHEN with a prompt this daemon would not compose now: WHAT changed
//! since.`) and on the three tool lists' own lead-ins. Anything else is `None` and the note is
//! drawn as it always was.

use rano::style::Role;

use crate::ui::transcript::line_cards::{Card, WarningKind};

/// What the line ends with: the one hint every notice line carries (R89). The reseat card's own
/// keys are on its strip — `alt-r` reseats, `alt-p` shows the card — and the popup holds the
/// daemon's whole sentence.
use crate::ui::transcript::line_cards::CLICK as POINTER;

const SEATED: &str = " was seated ";
const COMPOSE: &str = " with a prompt this daemon would not compose now: ";
const NEW: &str =
    "Seated here but not in this conversation's prompt, so the model cannot call them: ";
const GONE: &str = "In the prompt but not seated here, so a call to them refuses: ";
const GREW: &str =
    "Seated on both sides with a different schema, so the model is working from the older one: ";

/// **One stale prompt, as the sentence says it.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stale {
    pub(crate) session: String,
    pub(crate) seated: String,
    /// The system instructions changed.
    pub(crate) instructions: bool,
    /// The tool schemas changed.
    pub(crate) schemas: bool,
    /// Neither moved — the seat itself is what differs.
    pub(crate) seat: bool,
    pub(crate) new: Vec<String>,
    pub(crate) gone: Vec<String>,
    pub(crate) grew: Vec<String>,
}

impl Stale {
    pub(crate) fn line(&self) -> String {
        let mut parts = vec![
            "prompt stale".to_string(),
            format!("seated {}", self.seated),
        ];
        if self.instructions {
            parts.push("instructions changed".into());
        }
        if self.seat {
            parts.push("seat changed".into());
        }
        for (word, list) in [
            ("new", &self.new),
            ("gone", &self.gone),
            ("changed", &self.grew),
        ] {
            if !list.is_empty() {
                parts.push(format!("{word}: {}", list.join(", ")));
            }
        }
        // The schemas moved but no tool is named: say that much.
        if self.schemas && self.new.is_empty() && self.gone.is_empty() && self.grew.is_empty() {
            parts.push("tool schemas changed".into());
        }
        parts.push(POINTER.into());
        parts.join(" · ")
    }

    fn card(&self) -> Card {
        Card::new(
            self.line(),
            0,
            "prompt stale",
            Role::Attention,
            format!("prompt stale · {} · seated {}", self.session, self.seated),
        )
    }
}

/// The list a lead-in names, up to the sentence's end.
fn list_after(text: &str, lead: &str) -> Vec<String> {
    let Some(at) = text.find(lead) else {
        return Vec::new();
    };
    let rest = &text[at + lead.len()..];
    let end = rest.find(". ").unwrap_or(rest.len());
    rest[..end]
        .trim_end_matches('.')
        .split(", ")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// **A `prefix_stale` warning's detail, read** — `None` for a shape this head does not know.
pub(crate) fn stale_of(detail: &str) -> Option<Stale> {
    let r = detail.strip_prefix("session ")?;
    let (session, r) = r.split_once(SEATED)?;
    let (seated, r) = r.split_once(COMPOSE)?;
    let (what, _) = r.split_once(" changed since.")?;
    if session.is_empty() || session.contains(char::is_whitespace) || seated.contains('\n') {
        return None;
    }
    Some(Stale {
        session: session.to_string(),
        seated: seated.to_string(),
        instructions: what.contains("the system instructions"),
        schemas: what.contains("the tool schemas"),
        seat: what.starts_with("the seat "),
        new: list_after(detail, NEW),
        gone: list_after(detail, GONE),
        grew: list_after(detail, GREW),
    })
}

/// **The stale prompt in the card registry** (`line_cards`): the `prefix_stale` note.
pub(crate) const WARNING: WarningKind = WarningKind {
    codes: &["prefix_stale"],
    read: warning_card,
};

fn warning_card(detail: &str) -> Option<Card> {
    stale_card(detail)
}

/// **The stale prompt's card**, from the warning's detail or from the same sentence written as a
/// session row (R89 item 11: the session row was drawn whole).
pub(crate) fn stale_card(text: &str) -> Option<Card> {
    stale_of(text).map(|s| s.card())
}
