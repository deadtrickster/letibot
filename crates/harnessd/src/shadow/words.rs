//! Who said what, for every job's data. An agent's line through the bridge drives a turn like the
//! operator's prompt, but it is never the operator's words: no quote of it is an operator
//! correction, and no label is read from it.

use letibot_transcript::{Speaker, TranscriptItem};

/// The author of a row a check is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Author {
    /// The human at the keyboard, in their own words.
    Operator,
    /// Another agent, by its handle.
    Peer(String),
    /// The watched session's model.
    Assistant,
    /// A tool's output, a file or a fetched page: shown as evidence, never quotable as a note.
    Tool,
    /// Composed by the daemon: a template, a steering line, a notice. Evidence, never authority.
    Harness,
}

impl Author {
    /// How a check and a row name it.
    pub fn label(&self) -> String {
        match self {
            Author::Operator => "the operator".into(),
            Author::Peer(from) => format!("{from} (an agent, not the operator)"),
            Author::Assistant => "the assistant".into(),
            Author::Tool => "a tool's output".into(),
            Author::Harness => "the harness".into(),
        }
    }
}

/// One row's words, with its author and its index in the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    pub author: Author,
    pub row: usize,
    pub text: String,
}

/// A harness template on an operator row (`[answer] on ...`, `[restart] ...`): the daemon composed
/// it, so it is not the operator's own words even when their act caused it.
pub fn templated(text: &str) -> bool {
    let t = text.trim_start();
    let Some(rest) = t.strip_prefix('[') else {
        return false;
    };
    rest.find(']').is_some_and(|end| {
        end > 0
            && end <= 24
            && rest[..end]
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == ' ' || c == '-' || c == '_')
    })
}

/// A row stored as the operator's before the peer speaker existed, recognised by the handle the
/// bridge seats type first (`cc@rano: ...`). Old rows stay as they are; no reader takes them as
/// the operator's words.
pub fn signed_by_an_agent(text: &str) -> bool {
    let t = text.trim_start();
    t.strip_prefix("cc@").is_some_and(|rest| {
        rest.split_once(':').is_some_and(|(handle, _)| {
            !handle.is_empty() && handle.len() <= 40 && !handle.contains(char::is_whitespace)
        })
    })
}

/// The signing handle of an old operator row an agent wrote, `cc@rano`.
pub(crate) fn legacy_handle(item: &TranscriptItem) -> Option<String> {
    let text = letibot_sessionlog::view::body_of(item);
    signed_by_an_agent(&text).then(|| {
        let t = text.trim_start();
        let end = t.find(": ").or_else(|| t.find(':')).unwrap_or(t.len());
        t[..end].to_string()
    })
}

/// The human typed this row: an operator row whose text is not a harness template or a line an
/// agent signed.
pub fn human(item: &TranscriptItem) -> bool {
    if !matches!(
        item,
        TranscriptItem::User {
            speaker: Speaker::Operator,
            ..
        }
    ) {
        return false;
    }
    let text = letibot_sessionlog::view::body_of(item);
    !templated(&text) && !signed_by_an_agent(&text)
}

/// A row that moves the shadow's position: an answer, the operator's words or a peer's line. A peer
/// line is a new turn, so a verdict about the turn before it is stale.
pub fn moves_position(item: &TranscriptItem) -> bool {
    matches!(
        item,
        TranscriptItem::Assistant { .. }
            | TranscriptItem::User {
                speaker: Speaker::Operator | Speaker::Peer { .. },
                ..
            }
    )
}

/// The words that drove the session lately, oldest first: the operator's last row in their own
/// words, and every peer line after it (at most `PEERS`, the newest kept).
pub fn recent(items: &[TranscriptItem]) -> Vec<Said> {
    const PEERS: usize = 3;
    let mut out = Vec::new();
    for (row, item) in items.iter().enumerate().rev() {
        let TranscriptItem::User { speaker, .. } = item else {
            continue;
        };
        match speaker {
            Speaker::Peer { from } if out.len() < PEERS => out.push(Said {
                author: Author::Peer(from.clone()),
                row,
                text: letibot_sessionlog::view::body_of(item),
            }),
            Speaker::Operator if out.len() < PEERS && legacy_handle(item).is_some() => {
                out.push(Said {
                    author: Author::Peer(legacy_handle(item).unwrap_or_default()),
                    row,
                    text: letibot_sessionlog::view::body_of(item),
                })
            }
            Speaker::Operator if human(item) => {
                out.push(Said {
                    author: Author::Operator,
                    row,
                    text: letibot_sessionlog::view::body_of(item),
                });
                break;
            }
            _ => {}
        }
    }
    out.reverse();
    out
}
