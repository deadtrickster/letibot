//! The store behind the `transcript` and `digest` tools.
//!
//! `letibot-tools` cannot name `letibot-tokencore` — it is the lower crate — so
//! the tools declare a [`TranscriptSource`] trait and this is the daemon's
//! implementation of it, the same seam `harness` uses for `DaemonFacts`.
//!
//! # The fork chain is the whole job
//!
//! A session does not have *a* transcript. It has a chain of them, one per
//! compaction: each fork writes a new `transcript` row whose `parent_transcript_id`
//! points at the one it replaced and whose `forked_at_seq` says how many rows were
//! carried over. The conversation running now is the newest link, and everything a
//! compaction folded into a summary is in the links behind it.
//!
//! Which means a search that only looks at the current transcript answers "no"
//! to exactly the questions worth asking. Walking the chain is not an extra
//! feature here; it is the reason the tool exists.
//!
//! # Reading, not rendering
//!
//! A row's `item_json` is the record and its `tokens` are one rendering of it.
//! This reads the record: [`visible_text`] turns a `TranscriptItem` into what a
//! person would see, so a `match` predicate is matched against the conversation
//! rather than against serde tags and field names. It never touches the token
//! blobs, the hash chain or the stable prefix — nothing here can move a row, and
//! the append-only triggers would refuse if it tried.
//!
//! # Its own connection
//!
//! `rusqlite::Connection` is `Send` but not `Sync`, and a tool is `Send + Sync`
//! by trait. So this opens the store file a **second** time behind a `Mutex`
//! rather than sharing the harness's connection. Two readers on one SQLite file
//! is the ordinary case, and the separation earns something besides
//! compilability: a search that takes a second cannot hold the lock the turn
//! needs to append its next row.

use letibot_tokencore::store::Store;
use letibot_tools::builtins::transcript::{
    ChainLink, SessionRow, TranscriptHits, TranscriptQuery, TranscriptRow, TranscriptSource,
};
use letibot_transcript::{ToolOutcome, TranscriptItem, UserPart};

pub struct StoreTranscripts {
    /// This reader's own connection to the store file. See the module docs.
    store: std::sync::Mutex<Store>,
    /// The session a call from this harness is about when it names none.
    session_id: String,
}

impl StoreTranscripts {
    /// Open the store at `path` for reading. `Err` names the path, because the
    /// one thing a caller can do about it is check the daemon's `--store`.
    pub fn open(path: &std::path::Path, session_id: String) -> Result<Self, String> {
        let store = Store::open(path).map_err(|e| {
            format!(
                "the transcript store at {} could not be opened: {e}",
                path.display()
            )
        })?;
        Ok(StoreTranscripts {
            store: std::sync::Mutex::new(store),
            session_id,
        })
    }

    /// A poisoned lock is a reader that panicked mid-query, which leaves the
    /// SQLite connection fine — the panic was ours, not the file's.
    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Which session this query is about. A named one is matched by exact id
    /// first, then by a case-insensitive substring of the id or the title —
    /// the operator's own way of naming one is *"check letibot letibot session"*,
    /// which is a title fragment and not an id.
    fn resolve(&self, want: Option<&str>) -> Result<(String, String), String> {
        let sessions = self
            .store()
            .list_sessions()
            .map_err(|e| format!("the store could not be listed: {e}"))?;
        let Some(want) = want else {
            let title = sessions
                .iter()
                .find(|s| s.id == self.session_id)
                .and_then(|s| s.title.clone())
                .unwrap_or_default();
            return Ok((self.session_id.clone(), title));
        };
        if let Some(s) = sessions.iter().find(|s| s.id == want) {
            return Ok((s.id.clone(), s.title.clone().unwrap_or_default()));
        }
        let needle = want.to_lowercase();
        let mut hits: Vec<_> = sessions
            .iter()
            .filter(|s| {
                s.id.to_lowercase().contains(&needle)
                    || s.title
                        .as_deref()
                        .is_some_and(|t| t.to_lowercase().contains(&needle))
            })
            .collect();
        match hits.len() {
            1 => {
                let s = hits.remove(0);
                Ok((s.id.clone(), s.title.clone().unwrap_or_default()))
            }
            0 => Err(format!(
                "no session matches `{want}`, by id or by title. `what=sessions` lists \
                 the ones there are."
            )),
            // Ambiguity is reported, never resolved by picking: the newest match is
            // a guess, and a digest of the wrong conversation reads exactly like a
            // digest of the right one.
            n => Err(format!(
                "`{want}` matches {n} sessions: {}. Name one by its id.",
                hits.iter()
                    .take(8)
                    .map(|s| s.id.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// The session's transcripts, newest first, each with its generation.
    ///
    /// Newest is `created_at DESC, rowid DESC` — the same tie-break
    /// `Store::list_sessions` uses and for the same reason: a fork is written in
    /// the same millisecond as the transcript it forked from, and insertion order
    /// is what decides which of the two is the conversation running now.
    fn chain(&self, session_id: &str) -> Result<Vec<ChainLink>, String> {
        let store = self.store();
        let conn = store.connection();
        let mut stmt = conn
            .prepare(
                "SELECT t.id, t.forked_at_seq,
                        (SELECT COUNT(*) FROM transcript_item i WHERE i.transcript_id = t.id)
                   FROM transcript t
                  WHERE t.session_id = ?1
                  ORDER BY t.created_at DESC, t.rowid DESC",
            )
            .map_err(|e| format!("preparing the chain query: {e}"))?;
        let rows = stmt
            .query_map([session_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| format!("reading the chain: {e}"))?;
        let mut out = Vec::new();
        for (generation, row) in rows.enumerate() {
            let (transcript_id, forked_at, rows_n) =
                row.map_err(|e| format!("reading the chain: {e}"))?;
            out.push(ChainLink {
                transcript_id,
                generation: generation as u32,
                rows: rows_n as u32,
                forked_at_seq: forked_at.map(|s| s as u32),
            });
        }
        Ok(out)
    }
}

impl TranscriptSource for StoreTranscripts {
    fn sessions(&self) -> Vec<SessionRow> {
        self.store()
            .list_sessions()
            .unwrap_or_default()
            .into_iter()
            .map(|s| SessionRow {
                current: s.id == self.session_id,
                id: s.id,
                title: s.title.unwrap_or_default(),
                rows: s.items,
                last_activity_ms: s.last_activity_ms,
            })
            .collect()
    }

    fn find(&self, q: &TranscriptQuery) -> Result<TranscriptHits, String> {
        let (session_id, session_title) = self.resolve(q.session.as_deref())?;
        let chain = self.chain(&session_id)?;
        let searched: Vec<&ChainLink> = if q.history {
            chain.iter().collect()
        } else {
            chain.iter().take(1).collect()
        };

        let needle = q.matching.as_ref().map(|m| m.to_lowercase());
        let store = self.store();
        let conn = store.connection();
        let mut scanned = 0usize;
        let mut matched: Vec<TranscriptRow> = Vec::new();

        for link in &searched {
            let mut stmt = conn
                .prepare(
                    "SELECT seq, kind, item_json, created_at
                       FROM transcript_item WHERE transcript_id = ?1 ORDER BY seq ASC",
                )
                .map_err(|e| format!("preparing the row query: {e}"))?;
            let rows = stmt
                .query_map([&link.transcript_id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                })
                .map_err(|e| format!("reading rows: {e}"))?;
            for row in rows {
                let (seq, kind, item_json, created_ms) =
                    row.map_err(|e| format!("reading rows: {e}"))?;
                scanned += 1;
                let seq = seq as u32;
                // The seq predicates address the CURRENT transcript's numbering, so
                // they are only applied there — a `from`/`to` range meant for
                // generation 0 would select arbitrary rows in generation 3, whose
                // seq numbers mean something else entirely.
                if link.generation == 0 {
                    if q.from_seq.is_some_and(|f| seq < f) {
                        continue;
                    }
                    if q.to_seq.is_some_and(|t| seq > t) {
                        continue;
                    }
                }
                if !q.kinds.is_empty() && !q.kinds.iter().any(|k| kind_matches(k, &kind)) {
                    continue;
                }
                // Parsing is deferred until the cheap predicates have run: a
                // conversation is tens of thousands of rows and serde is the
                // expensive part of this loop.
                let Ok(item) = serde_json::from_str::<TranscriptItem>(&item_json) else {
                    // A row this build cannot parse is a row written by another
                    // build, not a corrupt one. Skipped rather than failing the
                    // search, because one unreadable row should not hide the
                    // twenty thousand readable ones around it.
                    continue;
                };
                let tool = tool_name(&item);
                if let Some(want) = &q.tool
                    && !tool.eq_ignore_ascii_case(want)
                {
                    continue;
                }
                let text = visible_text(&item);
                if let Some(n) = &needle
                    && !text.to_lowercase().contains(n)
                {
                    continue;
                }
                matched.push(TranscriptRow {
                    transcript_id: link.transcript_id.clone(),
                    seq,
                    kind: kind_word(&kind).to_string(),
                    tool,
                    text,
                    created_ms,
                    generation: link.generation,
                });
            }
        }

        // `last` is the newest N of what matched — applied after the predicates, so
        // `last=20` with a `match` means the twenty newest matching rows and not
        // "whichever of the twenty newest rows happened to match".
        //
        // Newest is generation ascending and seq descending: generation 0 is the
        // conversation running now, and a higher generation is further back.
        matched.sort_by_key(|r| (r.generation, std::cmp::Reverse(r.seq)));
        if let Some(n) = q.last {
            matched.truncate(n as usize);
        }
        let total = matched.len();
        let rows: Vec<TranscriptRow> = matched.into_iter().skip(q.offset).take(q.limit).collect();

        Ok(TranscriptHits {
            rows,
            session_id,
            session_title,
            scanned,
            matched: total,
            chain,
        })
    }
}

/// The `kind` column holds the serde tag. This maps the names a caller may type
/// onto it, so `tool_result` and `toolresult` both work and `mark` reaches
/// `SegmentMark` without the caller knowing the variant's spelling.
fn kind_matches(want: &str, kind: &str) -> bool {
    kind_word(kind) == kind_word(want)
}

fn kind_word(kind: &str) -> &str {
    match kind.trim().to_ascii_lowercase().as_str() {
        "user" => "user",
        "assistant" => "assistant",
        "reasoning" | "thinking" | "think" => "reasoning",
        "tool_result" | "toolresult" | "tool" | "result" => "tool_result",
        "system" => "system",
        "segment_mark" | "segmentmark" | "mark" | "segment" => "mark",
        _ => "",
    }
}

/// The tool a row came from, for a `tool_result`. Empty for every other kind.
fn tool_name(item: &TranscriptItem) -> String {
    match item {
        TranscriptItem::ToolResult { name, .. } => name.clone(),
        _ => String::new(),
    }
}

/// **A row as a person would see it**, which is what a text predicate must match
/// against. The alternative — matching `item_json` — matches serde tags and field
/// names, so `match=kind` hits every row in the store.
///
/// An assistant row carries its tool calls' names and arguments, because "when did
/// I last run that" is asked of the calls and the calls are not rows of their own.
/// A tool result carries its outcome word as well as its payload, so `match=denied`
/// finds the refusals.
fn visible_text(item: &TranscriptItem) -> String {
    match item {
        TranscriptItem::System { text, .. } => text.clone(),
        TranscriptItem::User { parts, .. } => parts
            .iter()
            .map(|p| match p {
                UserPart::Text { text } => text.clone(),
                UserPart::Image { media_type, .. } => format!("[image {media_type}]"),
                UserPart::FileRef { path, .. } => format!("[file {path}]"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        TranscriptItem::Reasoning {
            text, truncated, ..
        } => {
            if *truncated {
                format!("[abandoned thought] {text}")
            } else {
                text.clone()
            }
        }
        TranscriptItem::Assistant {
            text,
            tool_calls,
            truncated,
        } => {
            let mut out = String::new();
            if *truncated {
                out.push_str("[cut short] ");
            }
            out.push_str(text);
            for c in tool_calls {
                out.push_str(&format!("\n{} {}", c.name, c.arguments));
            }
            out
        }
        TranscriptItem::ToolResult {
            name,
            outcome,
            payload,
            ..
        } => {
            let word = match outcome {
                ToolOutcome::Ok => "ok".to_string(),
                ToolOutcome::Abstained { reason } => format!("abstained: {reason}"),
                ToolOutcome::Failed { reason } => format!("failed: {reason}"),
                ToolOutcome::Denied { req_id } => format!("denied (req {req_id})"),
                ToolOutcome::Timeout => "timed out".to_string(),
                ToolOutcome::NotRun { why } => format!("not run: {why}"),
                ToolOutcome::Backgrounded { handle, .. } => format!("backgrounded as {handle}"),
            };
            format!("{name} — {word}\n{payload}")
        }
        // Renders to nothing in a prompt, and is reported as what it is rather
        // than as an empty row somebody will wonder about.
        TranscriptItem::SegmentMark {
            label, kind, edge, ..
        } => format!("[segment {kind} {label} {edge:?}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_transcript::{ReasoningField, SystemOrigin, ToolCall};

    #[test]
    fn an_assistant_row_carries_the_calls_it_made() {
        let item = TranscriptItem::Assistant {
            text: "running it".into(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                arguments: r#"{"command":"cargo test"}"#.into(),
            }],
            truncated: false,
        };
        let t = visible_text(&item);
        assert!(t.contains("running it"));
        // "when did I run cargo test" is asked of the call, which is not a row.
        assert!(t.contains("cargo test"), "{t}");
    }

    /// The two marks a renderer treats specially are visible to a search, because
    /// "why did that thought stop" is a question about exactly those rows.
    #[test]
    fn an_abandoned_thought_and_a_cut_answer_say_so() {
        let r = visible_text(&TranscriptItem::Reasoning {
            text: "counting by hand".into(),
            field: ReasoningField::Inline,
            truncated: true,
        });
        assert!(r.starts_with("[abandoned thought]"), "{r}");
        let a = visible_text(&TranscriptItem::Assistant {
            text: "half a sent".into(),
            tool_calls: Vec::new(),
            truncated: true,
        });
        assert!(a.starts_with("[cut short]"), "{a}");
    }

    /// A tool result's outcome is text, so `match=denied` finds the refusals
    /// without the caller knowing the enum.
    #[test]
    fn a_tool_results_outcome_is_searchable_text() {
        let t = visible_text(&TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "bash".into(),
            outcome: ToolOutcome::Denied {
                req_id: "r9".into(),
            },
            payload: "nothing ran".into(),
            edit: None,
            origin: None,
            media: None,
        });
        assert!(t.contains("denied"), "{t}");
        assert!(t.contains("r9"), "{t}");
        assert!(t.contains("nothing ran"), "{t}");
    }

    #[test]
    fn kind_names_are_forgiving_about_spelling() {
        assert!(kind_matches("tool", "tool_result"));
        assert!(kind_matches("thinking", "reasoning"));
        assert!(kind_matches("mark", "segment_mark"));
        assert!(!kind_matches("user", "assistant"));
        // A word that is not a kind matches nothing rather than everything.
        assert!(!kind_matches("banana", "user"));
    }

    #[test]
    fn a_system_row_is_its_text() {
        let t = visible_text(&TranscriptItem::System {
            text: "you are letibot".into(),
            origin: SystemOrigin::Bootstrap,
        });
        assert_eq!(t, "you are letibot");
    }
}
