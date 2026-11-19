//! The `turn_end` job acting: the asleep reminder replaces or confirms the regex intent nudge, and
//! note capture proposes. Only fixed templates with verified quotes reach the session.

use letibot_tokencore::store::{TodoItem, TodoStatus};
use letibot_transcript::{OfferKind, Speaker, TranscriptItem};

use super::runner::Outcome;
use super::verdict::{Quoted, Reminder, Shown, Verdict};
use super::words::{Author, Said};

/// Each recent message under its author's header, with its span: the operator's own words and an
/// agent's line are never one heading.
pub fn push_words(shown: &mut Shown, words: &[Said]) {
    for w in words {
        let header = match &w.author {
            Author::Operator => "operator's last message (the human, in their own words):".into(),
            other => format!("message from {}:", other.label()),
        };
        shown.push_said(&header, w.author.clone(), Some(w.row), &w.text);
    }
}

/// The rows a turn_end check is shown, as data: the recent messages with their authors, the turn's
/// final answer, the open todos, and whether anything blocked it.
pub fn shown(words: &[Said], reply: &str, todos: &[TodoItem], blocked: &[String]) -> Shown {
    let mut s = Shown::default();
    push_words(&mut s, words);
    s.push_said(
        "assistant's final answer this turn:",
        Author::Assistant,
        None,
        reply,
    );
    let mut data = std::mem::take(&mut s.data);
    data.push_str("open todos:\n");
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
    s.data = data;
    s
}

/// The persistent shadow's question at a turn end. The turn's rows were already fed; the question
/// names where the turn began and adds the todos and what blocked it.
pub fn question(
    turn_from: Option<usize>,
    todos: &[TodoItem],
    blocked: &[String],
) -> super::conversation::Question {
    let mut d = match turn_from {
        Some(r) => {
            format!("the judged turn began at row {r} and ended with the newest assistant row\n\n")
        }
        None => "the judged turn ended with the newest assistant row\n\n".to_string(),
    };
    d.push_str("open todos:\n");
    let open: Vec<&TodoItem> = todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Pending | TodoStatus::InProgress))
        .collect();
    if open.is_empty() {
        d.push_str("(none)\n");
    }
    for t in open {
        d.push_str(&format!("- {}\n", t.content.trim()));
    }
    d.push_str("\nblocked by: ");
    if blocked.is_empty() {
        d.push_str("nothing recorded\n");
    } else {
        d.push_str(&blocked.join("; "));
        d.push('\n');
    }
    super::conversation::Question {
        job: super::Job::TurnEnd,
        facts: d,
        turn_from,
        rules: Vec::new(),
    }
}

/// The row the turn now ending began at: the last message that drove a turn.
pub fn turn_start(items: &[TranscriptItem]) -> Option<usize> {
    items.iter().rposition(|i| {
        matches!(
            i,
            TranscriptItem::User {
                speaker: Speaker::Operator | Speaker::Peer { .. },
                ..
            }
        )
    })
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

/// The line a shadow row's body names its session on, which the head reads to open it.
pub const SESSION_LINE: &str = "shadow session: ";

/// A proposed note, as a display-only row in the watched session: the operator sees it, the model
/// does not, and nothing writes the note. `at` is the quoted row's clock time, when known.
pub fn note_row(quote: &Quoted, at: Option<&str>, shadow_session: &str) -> TranscriptItem {
    let row = quote.row.map(|r| format!(" (row {r})")).unwrap_or_default();
    let at = at.map(|t| format!(" at {t}")).unwrap_or_default();
    TranscriptItem::Offer {
        kind: OfferKind::Shadow,
        title: format!("shadow suggests saving a note: {}", first_line(&quote.text)),
        body: format!(
            "shadow suggests saving a note:\n\n{}\n\nquoted from {}{at}{row}\n\n{SESSION_LINE}{shadow_session}",
            quote.text.trim(),
            quote.author,
        ),
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
    moved: bool,
    shadow_session: &str,
) -> (Act, Option<String>) {
    // A fallback releases the held nudge only about the turn it was held for: once the session
    // moved on, a late regex nudge is about a turn that is no longer current.
    let fallback = |why: &str| match held {
        Some(_) if moved => (
            Act::Nothing,
            Some(format!(
                "turn_end · {why}, and the session moved on; the held regex nudge is dropped"
            )),
        ),
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
        // The session moved on: whatever came next answers the turn better than a late nudge. Said
        // in the shadow's own record rather than dropped in silence, because "asked, answered, and
        // nothing came of it" is the fact a reader of a missed reminder has to be able to find.
        Outcome::Stale { judged, now } => (
            Act::Nothing,
            Some(format!(
                "turn_end · stale: judged #{judged} and the session is now at #{now}; nothing goes out"
            )),
        ),
        Outcome::Superseded => (
            Act::Nothing,
            Some("turn_end · superseded by a newer check; nothing goes out".into()),
        ),
    }
}

/// Whether a shadow reminder went out after the last message that drove a turn: the operator's,
/// or an agent's line, which is a new instruction though not the operator's words.
pub fn reminded_since_operator(items: &[TranscriptItem]) -> bool {
    for item in items.iter().rev() {
        if let TranscriptItem::User { speaker, .. } = item {
            if matches!(speaker, Speaker::Operator | Speaker::Peer { .. }) {
                return false;
            }
            if letibot_sessionlog::view::body_of(item).contains(MARK) {
                return true;
            }
        }
    }
    false
}

/// The operator's last message in their own words, never an agent's line or a harness template.
pub fn last_operator(items: &[TranscriptItem]) -> Option<String> {
    items
        .iter()
        .rev()
        .find(|i| super::words::human(i))
        .map(letibot_sessionlog::view::body_of)
}
