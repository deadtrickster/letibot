//! `/fork [N|ROW]`: a new session whose conversation is this one up to the operator's message N
//! and the exchange it started. The store copies the rows; this session is not changed.

use letibot_transcript::{Speaker, SystemOrigin, TranscriptItem, UserPart};

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

/// One message of the conversation: the transcript holding it, the row it starts at, its first line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Site {
    transcript: String,
    row: usize,
    text: String,
}

/// This session's stored history, oldest transcript first: the messages a fork can be taken at,
/// the transcripts they live in, and how many rows each of those holds.
struct History {
    sites: Vec<Site>,
    transcripts: Vec<String>,
    rows: Vec<usize>,
}

impl History {
    fn total(&self) -> usize {
        self.rows.iter().sum()
    }

    /// How many messages the rows before `keep` of `transcript` hold: the message a fork there is
    /// the fork at.
    fn before(&self, transcript: &str, keep: usize) -> usize {
        let place = |name: &str| self.transcripts.iter().position(|t| t == name);
        let mine = place(transcript);
        self.sites
            .iter()
            .filter(|s| match (place(&s.transcript), mine) {
                (Some(other), Some(mine)) => other < mine || (other == mine && s.row < keep),
                _ => false,
            })
            .count()
    }

    fn rows_of(&self, transcript: &str) -> Option<usize> {
        self.transcripts
            .iter()
            .position(|t| t == transcript)
            .map(|i| self.rows[i])
    }
}

/// How many of a transcript's leading rows are copies of its parent's last ones. `fork_onto` writes
/// a fork as `[note] + tail`, so a summarising fork opens with its own note and the rows after it
/// repeat the parent's last ones; a re-seat carries the whole conversation and has no note.
fn carried(parent: &[TranscriptItem], child: &[TranscriptItem]) -> usize {
    let note = usize::from(matches!(
        child.first(),
        Some(TranscriptItem::System {
            origin: SystemOrigin::Update,
            ..
        })
    ));
    let head = &child[note..];
    for k in (1..=parent.len().min(head.len())).rev() {
        let tail = &parent[parent.len() - k..];
        if tail.iter().zip(head).all(|(p, c)| same_row(p, c)) {
            return note + k;
        }
    }
    note
}

/// The same row of a conversation. A prune restubs old tool bodies and leaves every message alone,
/// so two rows that are both not messages count as one when they are the same kind of row.
fn same_row(parent: &TranscriptItem, child: &TranscriptItem) -> bool {
    parent == child
        || (!is_message(parent)
            && !is_message(child)
            && std::mem::discriminant(parent) == std::mem::discriminant(child))
}

impl Harness {
    /// Fork this session at message `at`, or list the messages when `at` is `None`.
    pub fn fork_at(&mut self, at: Option<usize>) -> Result<ForkOutcome, HarnessError> {
        let history = self.history()?;
        let Some(at) = at else {
            return Ok(ForkOutcome::Listed(
                history.sites.iter().map(|s| s.text.clone()).collect(),
            ));
        };
        let Some(site) = history.sites.get(at - 1).cloned() else {
            return Err(HarnessError::Setup(format!(
                "this conversation has {} message(s), so there is no message {at} to fork at. \
                 `/fork` lists them.",
                history.sites.len()
            )));
        };
        let keep = history
            .sites
            .iter()
            .find(|s| s.transcript == site.transcript && s.row > site.row)
            .map(|s| s.row)
            .unwrap_or_else(|| history.rows_of(&site.transcript).unwrap_or(0));
        self.fork_here(&history, &site.transcript, keep)
    }

    /// Fork at a row the head named by id (`<transcript>.<row>`), which is how a message from
    /// before a compaction is chosen: a head holds such a row but cannot number it.
    pub fn fork_at_row(&mut self, item_id: &str) -> Result<ForkOutcome, HarnessError> {
        let (transcript, row) = item_id
            .rsplit_once('.')
            .and_then(|(t, r)| Some((t, r.parse::<usize>().ok()?)))
            .ok_or_else(|| {
                HarnessError::Setup(format!(
                    "`{item_id}` is not a row of a conversation: a row is named \
                     `<transcript>.<number>`"
                ))
            })?;
        let history = self.history()?;
        let Some(rows) = history.rows_of(transcript) else {
            return Err(HarnessError::Setup(format!(
                "{item_id} is not a row of this conversation - its transcript is not in this \
                 session's history, so there is nothing here to fork at"
            )));
        };
        if row >= rows {
            return Err(HarnessError::Setup(format!(
                "the rows {item_id} belongs to are gone from the store: transcript {transcript} \
                 holds {rows} row(s) and the one to fork at is #{row}. A fork carries the stored \
                 rows, so there is no history here to carry"
            )));
        }
        let keep = history
            .sites
            .iter()
            .find(|s| s.transcript == transcript && s.row > row)
            .map(|s| s.row)
            .unwrap_or(rows);
        self.fork_here(&history, transcript, keep)
    }

    /// Fork at the first `keep` rows of `transcript`, which is this conversation's history up to
    /// the end of one message's exchange.
    fn fork_here(
        &mut self,
        history: &History,
        transcript: &str,
        keep: usize,
    ) -> Result<ForkOutcome, HarnessError> {
        // An empty transcript with a later one behind it is a hole: forking past it would carry the
        // summary of a conversation whose beginning is gone, which is the one thing a fork must not
        // do quietly.
        let mine = history
            .transcripts
            .iter()
            .position(|t| t == transcript)
            .unwrap_or(0);
        if let Some(hole) = history.rows[..mine].iter().position(|&n| n == 0) {
            return Err(HarnessError::Setup(format!(
                "the rows of transcript {} are gone from the store, so a fork here cannot carry \
                 the uncompacted history: what is left above this message is the summary that \
                 replaced it",
                history.transcripts[hole]
            )));
        }
        let at = history.before(transcript, keep) + 1;
        let (session_id, transcript_id) = self.fork_into_new_session_from(transcript, keep)?;
        Ok(ForkOutcome::Forked {
            session_id,
            transcript_id,
            at,
            rows: keep,
            left: history.total().saturating_sub(keep),
        })
    }

    /// The conversation as the STORE holds it, oldest transcript first: a compaction's summary
    /// replaced rows that are still there, and a fork at one of them carries those, not the summary.
    /// A transcript's rows carried from its parent are not listed twice.
    fn history(&self) -> Result<History, HarnessError> {
        let chain = match self.store.as_ref() {
            Some(store) => store
                .transcript_chain(&self.cfg.session_id)
                .map_err(|e| HarnessError::Store(e.to_string()))?,
            None => Vec::new(),
        };
        // No transcript row yet - a session the store has not opened - so the rows in memory are
        // the whole of what there is to fork at, which is what a storeless session answers too.
        if chain.is_empty() {
            let sites = self
                .session
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| is_message(item))
                .map(|(row, item)| Site {
                    transcript: self.transcript_id.clone(),
                    row,
                    text: first_line(item),
                })
                .collect();
            return Ok(History {
                sites,
                transcripts: vec![self.transcript_id.clone()],
                rows: vec![self.session.items.len()],
            });
        }
        let mut sites = Vec::new();
        let mut rows = Vec::with_capacity(chain.len());
        let mut parent: Option<Vec<TranscriptItem>> = None;
        for transcript in &chain {
            let items = self.rows_of(transcript)?;
            let start = parent.as_deref().map_or(0, |p| carried(p, &items));
            for (row, item) in items.iter().enumerate().skip(start) {
                if is_message(item) {
                    sites.push(Site {
                        transcript: transcript.clone(),
                        row,
                        text: first_line(item),
                    });
                }
            }
            rows.push(items.len());
            parent = Some(items);
        }
        Ok(History {
            sites,
            transcripts: chain,
            rows,
        })
    }

    /// One transcript's rows, with the rows appended since the last store write - the conversation
    /// is never shorter than what the operator has on screen.
    fn rows_of(&self, transcript: &str) -> Result<Vec<TranscriptItem>, HarnessError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(self.session.items.clone());
        };
        let mut items: Vec<TranscriptItem> = store
            .load_transcript(transcript)
            .map_err(|e| HarnessError::Store(e.to_string()))?
            .items
            .into_iter()
            .map(|(item, _, _)| item)
            .collect();
        if transcript == self.transcript_id && self.session.items.len() > items.len() {
            items.extend_from_slice(&self.session.items[items.len()..]);
        }
        Ok(items)
    }

    /// A new store session holding this conversation's first `keep` rows: its id and transcript.
    /// The limit hook's `new_session` forks through here as `/fork` does.
    pub(crate) fn fork_into_new_session(
        &mut self,
        keep: usize,
    ) -> Result<(String, String), HarnessError> {
        let current = self.transcript_id.clone();
        self.fork_into_new_session_from(&current, keep)
    }

    /// The same, over an EARLIER transcript of this conversation: a fork at a message from before a
    /// compaction carries that transcript's rows, so the summary never enters the new session.
    fn fork_into_new_session_from(
        &mut self,
        from: &str,
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
            .fork_session_at(&self.cfg.session_id, from, keep as u32, &session_id)
            .map_err(|e| HarnessError::Store(e.to_string()))?;
        // A fork is a new conversation of the same folder, so it is NAMED like one: the name it
        // continues, dashed, because two sessions answering to one name are two ways to open the
        // wrong one. An unnamed parent stays unnamed - there is nothing to derive from.
        let base = self.cfg.title.trim();
        if !base.is_empty()
            && let Ok(name) =
                store.free_derived_name(&self.cfg.workspace.display().to_string(), base)
            && let Err(e) = store.set_title(&session_id, &name)
        {
            self.warn(
                "fork_unnamed",
                format!("the fork {session_id} continues this conversation, but its name could not be written: {e}"),
            );
        }
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

    /// The fork point ends before the next message, and an agent-appended row stays in the
    /// exchange it landed in.
    #[test]
    fn the_fork_point_ends_before_the_next_message() {
        let items = [
            user("one", Speaker::Operator),
            answer(),
            user("job settled", Speaker::Agent),
            answer(),
            user("two", Speaker::Operator),
            answer(),
            user("three", Speaker::Operator),
        ];
        let starts: Vec<usize> = (0..items.len())
            .filter(|&i| is_message(&items[i]))
            .collect();
        assert_eq!(
            starts,
            vec![0, 4, 6],
            "three messages, and the agent's row is none"
        );
        let keep = |n: usize| starts.get(n).copied().unwrap_or(items.len());
        assert_eq!(keep(1), 4, "agent rows stay in the exchange");
        assert_eq!(keep(2), 6);
        assert_eq!(keep(3), 7, "the last message keeps every row");
    }

    /// The carry a compaction makes: the note it wrote, then the parent's last rows verbatim.
    #[test]
    fn a_compacted_transcripts_carried_rows_are_not_a_second_copy() {
        let parent = vec![
            user("one", Speaker::Operator),
            answer(),
            user("two", Speaker::Operator),
            answer(),
        ];
        let child = vec![
            TranscriptItem::System {
                text: "[summary]".into(),
                origin: SystemOrigin::Update,
            },
            parent[2].clone(),
            parent[3].clone(),
            user("three", Speaker::Operator),
        ];
        assert_eq!(
            carried(&parent, &child),
            3,
            "the note and the two rows carried after it"
        );
        assert_eq!(
            carried(&parent, &parent.clone()),
            parent.len(),
            "a re-seat carries the whole conversation and has no note"
        );
    }
}
