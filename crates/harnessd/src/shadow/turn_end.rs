//! The `turn_end` job acting: the asleep reminder replaces or confirms the regex intent nudge, and
//! note capture proposes. Only fixed templates with verified quotes reach the session.

use letibot_tokencore::store::{TodoItem, TodoStatus};
use letibot_transcript::{OfferKind, Speaker, TranscriptItem};

use super::runner::Outcome;
use super::verdict::{Reminder, Shown, Verdict};

/// The rows a turn_end check is shown, as data: the operator's last words, the turn's final answer,
/// the open todos, and whether anything blocked it.
pub fn shown(operator: Option<&str>, reply: &str, todos: &[TodoItem], blocked: &[String]) -> Shown {
    let mut data = String::new();
    if let Some(op) = operator {
        data.push_str("operator's last message:\n");
        data.push_str(op.trim());
        data.push_str("\n\n");
    }
    data.push_str("assistant's final answer this turn:\n");
    data.push_str(reply.trim());
    data.push_str("\n\nopen todos:\n");
    let open: Vec<&TodoItem> = todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Pending | TodoStatus::InProgress))
        .collect();
    if open.is_empty() {
        data.push_str("(none)\n");
    }
    for t in open {
        data.push_str(&format!("- {}\n", t.content.trim()));
    }
    data.push_str("\nblocked by: ");
    if blocked.is_empty() {
        data.push_str("nothing recorded\n");
    } else {
        data.push_str(&blocked.join("; "));
        data.push('\n');
    }
    Shown {
        data,
        ..Shown::default()
    }
}

/// What the session is told, from a fixed template and a verified quote.
pub fn reminder(promise: &str, shadow_session: &str) -> String {
    format!(
        "[shadow] You said \"{}\" and the turn ended there. Carry on, or say why not. \
         (verdict from {shadow_session})",
        promise.trim()
    )
}

/// The marker a reminder row starts with: one reminder per operator message is counted by it.
pub const MARK: &str = "[shadow] You said";

/// A proposed note, as a display-only row in the watched session: the operator sees it, the model
/// does not, and nothing writes the note.
pub fn note_row(quote: &str, shadow_session: &str) -> TranscriptItem {
    TranscriptItem::Offer {
        kind: OfferKind::Shadow,
        title: format!("shadow proposes a note: {}", first_line(quote)),
        body: format!("{}\n\nfrom {shadow_session}", quote.trim()),
    }
}

fn first_line(s: &str) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    match line.char_indices().nth(80) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.to_string(),
    }
}

/// What a turn_end outcome does to the watched session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// The shadow's reminder, templated.
    Remind(String),
    /// The regex nudge the turn end held back, delivered as it would have been.
    Regex(String),
    Nothing,
}

/// The decision, and the shadow-session row that says what it replaced or confirmed. `held` is
/// the regex nudge the turn end computed and did not deliver; `reminded` is whether a shadow
/// reminder already went out since the operator's last message.
pub fn decide(
    outcome: &Outcome,
    held: Option<&str>,
    reminded: bool,
    shadow_session: &str,
) -> (Act, Option<String>) {
    let fallback = |why: &str| match held {
        Some(t) => (
            Act::Regex(t.to_string()),
            Some(format!("turn_end · {why}; the regex nudge goes out")),
        ),
        None => (Act::Nothing, None),
    };
    match outcome {
        Outcome::Verdict(Verdict::TurnEnd { reminder: r, .. }) => match r {
            Reminder::Owed { .. } if reminded => (
                Act::Nothing,
                Some("turn_end · owed, withheld: one reminder per operator message".into()),
            ),
            Reminder::Owed { promise, .. } => (
                Act::Remind(reminder(promise, shadow_session)),
                Some(if held.is_some() {
                    "turn_end · owed, confirms the regex nudge; the reminder goes out".into()
                } else {
                    "turn_end · owed where the regex was silent; the reminder goes out".into()
                }),
            ),
            Reminder::None => (
                Act::Nothing,
                held.map(|_| "turn_end · none, replaces the regex nudge; nothing goes out".into()),
            ),
            Reminder::Unsure => fallback("unsure"),
        },
        Outcome::Void(_) => fallback("void"),
        Outcome::TimedOut => fallback("timed out"),
        Outcome::Failed(_) => fallback("failed"),
        Outcome::Paused => fallback("paused"),
        Outcome::Verdict(_) => fallback("a verdict of the wrong job"),
        // The session moved on: whatever came next answers the turn better than a late nudge.
        Outcome::Stale { .. } | Outcome::Superseded => (Act::Nothing, None),
    }
}

/// Whether a shadow reminder went out after the operator's last message.
pub fn reminded_since_operator(items: &[TranscriptItem]) -> bool {
    for item in items.iter().rev() {
        if let TranscriptItem::User { speaker, .. } = item {
            if *speaker == Speaker::Operator {
                return false;
            }
            if letibot_sessionlog::view::body_of(item).contains(MARK) {
                return true;
            }
        }
    }
    false
}

/// The operator's last message, for the data a check is shown.
pub fn last_operator(items: &[TranscriptItem]) -> Option<String> {
    items.iter().rev().find_map(|i| match i {
        TranscriptItem::User {
            speaker: Speaker::Operator,
            ..
        } => Some(letibot_sessionlog::view::body_of(i)),
        _ => None,
    })
}
