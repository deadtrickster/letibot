//! `/fork [N]`: a new session whose conversation is this one up to the operator's message N
//! and the exchange it started. The store copies the rows; this session is not changed.

use letibot_transcript::{Speaker, TranscriptItem, UserPart};

use super::{Harness, HarnessError};

/// What `/fork` did, or the messages it can fork at when no number was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkOutcome {
    /// The messages, 1-based, with their first line.
    Listed(Vec<String>),
    Forked {
        session_id: String,
        transcript_id: String,
        /// The message forked at, 1-based.
        at: usize,
        rows: usize,
        /// Rows of this conversation the fork does not carry.
        left: usize,
    },
}

/// A row that starts an exchange: something a person or a peer said. Agent-appended rows
/// (job settlements, steering) are part of the exchange they land in.
fn is_message(item: &TranscriptItem) -> bool {
    matches!(item, TranscriptItem::User { speaker, .. } if !matches!(speaker, Speaker::Agent))
}

fn first_line(item: &TranscriptItem) -> String {
    let TranscriptItem::User { parts, .. } = item else {
        return String::new();
    };
    let text = parts
        .iter()
        .find_map(|p| match p {
            UserPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("[attachment]");
    let line = text.lines().next().unwrap_or("").trim();
    match line.char_indices().nth(72) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    }
}

/// Rows kept by a fork at message `at` (1-based): everything before message `at + 1`.
pub(crate) fn fork_point(items: &[TranscriptItem], at: usize) -> Option<usize> {
    let starts: Vec<usize> = (0..items.len())
        .filter(|&i| is_message(&items[i]))
        .collect();
    if at == 0 || at > starts.len() {
        return None;
    }
    Some(starts.get(at).copied().unwrap_or(items.len()))
}

impl Harness {
    /// Fork this session at message `at`, or list the messages when `at` is `None`.
    pub fn fork_at(&mut self, at: Option<usize>) -> Result<ForkOutcome, HarnessError> {
        let messages: Vec<String> = self
            .session
            .items
            .iter()
            .filter(|i| is_message(i))
            .map(first_line)
            .collect();
        let Some(at) = at else {
            return Ok(ForkOutcome::Listed(messages));
        };
        let Some(keep) = fork_point(&self.session.items, at) else {
            return Err(HarnessError::Setup(format!(
                "this conversation has {} message(s), so there is no message {at} to fork at. \
                 `/fork` lists them.",
                messages.len()
            )));
        };
        let (session_id, transcript_id) = self.fork_into_new_session(keep)?;
        Ok(ForkOutcome::Forked {
            session_id,
            transcript_id,
            at,
            rows: keep,
            left: self.session.items.len() - keep,
        })
    }

    /// A new store session holding this conversation's first `keep` rows: its id and transcript.
    /// The limit hook's `new_session` forks through here as `/fork` does.
    pub(crate) fn fork_into_new_session(
        &mut self,
        keep: usize,
    ) -> Result<(String, String), HarnessError> {
        // Rows still in memory would be missing from the copy.
        self.persist()?;
        let Some(store) = self.store.as_ref() else {
            return Err(HarnessError::Setup(
                "a fork is a new session in the store, and this session has none".into(),
            ));
        };
        let mut session_id = format!("s-{}", crate::config::now_ns());
        while store
            .session(&session_id)
            .map_err(|e| HarnessError::Store(e.to_string()))?
            .is_some()
        {
            session_id = format!("s-{}", crate::config::now_ns());
        }
        let transcript_id = store
            .fork_session(&self.cfg.session_id, &session_id, keep as u32)
            .map_err(|e| HarnessError::Store(e.to_string()))?;
        // A fork of a child is a child, so its own point is a session row: a root's point is the
        // project's row and already there, and writing one for a fork of a root would move every
        // later session in the workspace. The ceiling comes from the parent the row now names.
        if self.parent_point.is_some()
            && let Err(e) =
                crate::modes::record(&self.mode_store, None, &session_id, self.cfg.mode, false)
        {
            self.warn(
                "fork_point_unpersisted",
                format!(
                    "the fork {session_id} holds this session's point `{}` now, but it could \
                     not be recorded, so reopening it will re-derive the point from its \
                     parent: {e}",
                    self.cfg.mode.name
                ),
            );
        }
        Ok((session_id, transcript_id))
    }
}

/// The head's lines for a `/fork`.
pub fn fork_reply(outcome: &ForkOutcome) -> Vec<String> {
    match outcome {
        ForkOutcome::Listed(messages) if messages.is_empty() => {
            vec!["no message to fork at yet".into()]
        }
        ForkOutcome::Listed(messages) => {
            let mut lines = vec!["`/fork N` makes a new session that ends after message N:".into()];
            lines.extend(
                messages
                    .iter()
                    .enumerate()
                    .map(|(i, m)| format!("  {}. {m}", i + 1)),
            );
            lines
        }
        ForkOutcome::Forked {
            session_id,
            at,
            rows,
            left,
            ..
        } => vec![format!(
            "forked at message {at} into {session_id}: {rows} row(s) carried, {left} left \
             here. This session is unchanged. `/resume {session_id}` opens the fork."
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str, speaker: Speaker) -> TranscriptItem {
        TranscriptItem::User {
            parts: vec![UserPart::Text { text: text.into() }],
            speaker,
        }
    }

    fn answer() -> TranscriptItem {
        TranscriptItem::Assistant {
            text: "ok".into(),
            tool_calls: vec![],
            truncated: false,
        }
    }

    #[test]
    fn the_fork_point_ends_before_the_next_message() {
        let items = vec![
            user("one", Speaker::Operator),
            answer(),
            user("job settled", Speaker::Agent),
            answer(),
            user("two", Speaker::Operator),
            answer(),
            user("three", Speaker::Operator),
        ];
        assert_eq!(
            fork_point(&items, 1),
            Some(4),
            "agent rows stay in the exchange"
        );
        assert_eq!(fork_point(&items, 2), Some(6));
        assert_eq!(
            fork_point(&items, 3),
            Some(7),
            "the last message keeps every row"
        );
        assert_eq!(fork_point(&items, 0), None);
        assert_eq!(fork_point(&items, 4), None);
    }
}
