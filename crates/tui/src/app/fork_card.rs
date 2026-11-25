//! The fork card: the message to fork at, chosen from the conversation rather than counted.
//!
//! A row is one turn of the operator's - the time it landed and its first line - and Enter forks
//! there. The card names the row by the id a head holds (`<transcript>.<row>`) and not by a number:
//! the daemon numbers the messages of its whole stored history, a head holds what it was sent, and
//! a number derived from one of them would fork at the other's message.

use super::*;
use letibot_transcript::{Speaker, TranscriptItem, UserPart};

/// One row of the card: what a person reads, and what the fork is taken at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForkRow {
    /// The id of the row in the transcript, which the daemon resolves.
    pub(crate) id: String,
    /// `14:32 · the first line`, as the card draws it.
    pub(crate) label: String,
}

impl App {
    /// The conversation's operator turns, oldest first: what the card offers.
    pub(crate) fn operator_turns(&self) -> Vec<ForkRow> {
        self.items
            .iter()
            .filter_map(|row| {
                let TranscriptItem::User { parts, speaker } = row.item.as_ref()? else {
                    return None;
                };
                if !matches!(speaker, Speaker::Operator) {
                    return None;
                }
                // The first words of the turn, or the fact that it is an attachment: a row with no
                // text is a real turn and a fork can be taken at it.
                let text = parts
                    .iter()
                    .find_map(|p| match p {
                        UserPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .unwrap_or("[attachment]");
                let line = text.lines().next().unwrap_or("").trim();
                let short: String = match line.char_indices().nth(60) {
                    Some((i, _)) => format!("{}…", &line[..i]),
                    None => line.to_string(),
                };
                let clock = crate::ui::transcript::blocks::clock_time(row.ts);
                Some(ForkRow {
                    id: row.item_id.clone(),
                    label: if clock.is_empty() {
                        short
                    } else {
                        format!("{clock} · {short}")
                    },
                })
            })
            .collect()
    }

    /// `/fork` alone: the card over the conversation's turns. `None` when there is nothing to choose
    /// from, and the operator is told so rather than shown an empty card.
    pub(crate) fn open_fork_card(&mut self) -> Option<Action> {
        self.fork_rows = self.operator_turns();
        if self.fork_rows.is_empty() {
            self.say(
                "no message to fork at yet - a fork is taken at one of your own turns, and this \
                 conversation has none",
            );
            return None;
        }
        self.pick = Some(Pick::Fork);
        self.close_surface(Surface::Picker);
        self.close_surface(Surface::Config);
        self.seed_pick();
        None
    }

    /// Take the row the card's cursor is on: the fork the daemon is asked for, by the id it drew.
    pub(crate) fn take_fork_row(&mut self) -> Option<Action> {
        let at = self.mode_sel.min(self.fork_rows.len().saturating_sub(1));
        let Some(row) = self.fork_rows.get(at).cloned() else {
            self.pick = None;
            return None;
        };
        self.pick = None;
        let label = row.label.clone();
        self.fork_rows.clear();
        self.say(&format!("forking at {label}…"));
        Some(Action::Slash {
            line: format!("fork {}", row.id),
        })
    }
}
