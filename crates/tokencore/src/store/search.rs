//! Searching conversation ROWS across a project's sessions - the store half of the head's
//! cross-session search.
//!
//! No index is built: `transcript_item` already holds every row of every session, and a row's
//! `item_json` is the thing a reader searches. A scan reads the newest rows of the newest
//! sessions first and stops on its own budgets, so a query over a long-lived project costs a
//! bounded number of rows and answers a page.

use letibot_transcript::TranscriptItem;
use letibot_transcript::search::{Query, row_match};

use super::{Archived, Result, Store, StoredSession};

/// The most hits one answer carries, whatever the caller asked for.
pub const HITS_MAX: usize = 200;

/// Rows one answer may look at, across every session it opens. The bound is what makes a
/// search over a project something a head can ask on every keystroke.
pub const ROWS_SCAN_MAX: usize = 20_000;

/// Bytes of row JSON one answer may read, so a project of enormous rows is still bounded.
pub const BYTES_SCAN_MAX: usize = 8 << 20;

/// Rows read from the store per call while walking a transcript backwards.
const PAGE: u32 = 200;

/// One session a scan reads: its own row, and the transcript a row ordinal numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanTarget {
    pub session: String,
    pub title: String,
    /// The session's workspace, which is the folder a hit's row names.
    pub folder: String,
    /// When the session was last written, for the ordering and a row with no clock of its own.
    pub at_ms: i64,
    pub transcript: String,
    /// Rows in that transcript.
    pub rows: u32,
}

/// One row a query matched, as a list row shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationHit {
    pub session: String,
    pub title: String,
    pub folder: String,
    /// When the ROW was written, which is what a hit's time means: the session's own last
    /// activity would date every hit of a session to the same minute.
    pub at_ms: i64,
    /// `mine`, `agent` or `tool` - `letibot_transcript::search::Role::as_str`'s word.
    pub role: String,
    /// The tool whose words matched, when the role is `tool`.
    pub tool: String,
    /// The row's ordinal in its transcript, so a head that opens the session can page to it.
    pub row: usize,
    pub item_id: String,
    pub excerpt: String,
}

/// What one search found, and what it cost.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conversations {
    pub hits: Vec<ConversationHit>,
    /// The scan stopped on a budget with more to find: another page exists.
    pub more: bool,
    /// Rows looked at, which is the number the cost is read from.
    pub scanned: usize,
    /// Sessions opened.
    pub sessions: usize,
}

impl Store {
    /// The project's sessions a project-wide search reads: archived included, newest activity
    /// first, kept by `keep`. `workspace` is the folder the searcher's own session is in, so a
    /// project's search never answers with another project's conversations.
    pub fn sessions_to_search(
        &self,
        workspace: &str,
        keep: &dyn Fn(&StoredSession) -> bool,
    ) -> Result<Vec<ScanTarget>> {
        let key = crate::migrate_split::project_key(workspace);
        let mut out = Vec::new();
        for s in self.list_sessions_archived(Archived::Any)? {
            if crate::migrate_split::project_key(&s.workspace_root) != key {
                continue;
            }
            if !keep(&s) {
                continue;
            }
            let Some(transcript) = s.transcript_id.clone() else {
                continue;
            };
            out.push(ScanTarget {
                session: s.id.clone(),
                title: s.title.clone().unwrap_or_default(),
                folder: s.workspace_root.clone(),
                at_ms: s.last_activity_ms,
                transcript,
                rows: s.items,
            });
        }
        Ok(out)
    }

    /// One session's scan target, by id - the session-scoped search's whole list. Found whether
    /// the session is archived or not: a head searching the conversation it is looking at is not
    /// asking a question about the archive.
    pub fn scan_target(&self, session_id: &str) -> Result<Option<ScanTarget>> {
        let Some(s) = self.session(session_id)? else {
            return Ok(None);
        };
        let Some(transcript) = s.transcript_id.clone() else {
            return Ok(None);
        };
        Ok(Some(ScanTarget {
            session: s.id,
            title: s.title.unwrap_or_default(),
            folder: s.workspace_root,
            at_ms: s.last_activity_ms,
            transcript,
            rows: s.items,
        }))
    }

    /// Scan `targets` for `query`, newest row first, answering hits `[from, from + count)`.
    /// `more` is set when the scan had to stop with hits still unfound, so a head knows whether
    /// to ask for another page.
    pub fn search_rows(
        &self,
        query: &str,
        targets: &[ScanTarget],
        from: usize,
        count: usize,
    ) -> Result<Conversations> {
        let q = Query::parse(query);
        let count = count.min(HITS_MAX);
        let mut out = Conversations::default();
        if q.is_empty() || q.complaint().is_some() {
            return Ok(out);
        }
        // One hit past the page is what tells a caller another page exists without scanning to
        // the end of the project for it.
        let want = from.saturating_add(count).saturating_add(1);
        let mut found: Vec<ConversationHit> = Vec::new();
        let mut bytes = 0usize;
        for target in targets {
            out.sessions += 1;
            let mut to = target.rows;
            while to > 0 {
                let first = to.saturating_sub(PAGE);
                let page = self.rows_json_from(&target.transcript, first, to - first)?;
                // Newest first, so a page is walked back to front: a reader looking for a word
                // wants last week's turn before the one from January.
                for (seq, item_id, _kind, json, created_at) in page.into_iter().rev() {
                    out.scanned += 1;
                    bytes += json.len();
                    if let Some(hit) = hit_of(&q, target, seq as usize, item_id, created_at, &json)
                    {
                        found.push(hit);
                    }
                    if found.len() >= want
                        || out.scanned >= ROWS_SCAN_MAX
                        || bytes >= BYTES_SCAN_MAX
                    {
                        out.more = found.len() > from + count;
                        out.hits = window(found, from, count);
                        return Ok(out);
                    }
                }
                to = first;
            }
        }
        out.more = found.len() > from + count;
        out.hits = window(found, from, count);
        Ok(out)
    }
}

/// Hits `[from, from + count)`, never past the end.
fn window(found: Vec<ConversationHit>, from: usize, count: usize) -> Vec<ConversationHit> {
    found.into_iter().skip(from).take(count).collect()
}

/// One row as a hit, or `None` when the query does not match it. A row this build cannot decode
/// is skipped rather than failing the scan: one unreadable row must not cost a project's search.
fn hit_of(
    q: &Query,
    target: &ScanTarget,
    row: usize,
    item_id: String,
    created_at: u64,
    json: &str,
) -> Option<ConversationHit> {
    let item: TranscriptItem = serde_json::from_str(json).ok()?;
    let m = row_match(q, &item)?;
    // The row's own wall clock when the store has one, and the session's last activity when it
    // does not: a hit with no time at all would sort as 1970.
    let at_ms = match created_at {
        0 => target.at_ms,
        at => at as i64,
    };
    Some(ConversationHit {
        session: target.session.clone(),
        title: target.title.clone(),
        folder: target.folder.clone(),
        at_ms,
        role: m.role.as_str().to_string(),
        tool: m.tool,
        row,
        item_id,
        excerpt: m.excerpt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{TokenLedger, hash_tokens};
    use crate::store::{SessionRecord, StablePrefixRecord};
    use letibot_transcript::{Speaker, ToolCall, UserPart};

    /// A prefix as a session is really seated with: the ledger chain starts from its tokens, so a
    /// fixture that skipped it could not append a row at all.
    fn prefix(store: &Store) -> (String, Vec<crate::vocab::TokenId>) {
        let tokens = vec![1u32, 2, 3, 4];
        let id = store
            .put_stable_prefix(&StablePrefixRecord {
                dialect_sha: "aa".repeat(32),
                system: "you are a harness".into(),
                tools_json: Vec::new(),
                h_init: hash_tokens(&tokens),
                tokens: tokens.clone(),
                vocab_source: "/models/x.gguf".into(),
                effort: None,
            })
            .expect("the prefix is written");
        (id, tokens)
    }

    /// A session with rows, written the way the daemon writes them: a stable prefix, one
    /// transcript, and a ledger row per item. A fixture that faked `item_json` would not prove
    /// that what a search reads is what a session stored.
    fn seeded(store: &Store, id: &str, parent: Option<&str>, items: Vec<TranscriptItem>) -> String {
        let (prefix_id, tokens) = prefix(store);
        store
            .put_session(&SessionRecord {
                id: id.into(),
                title: Some(format!("{id} title")),
                model_id: "qwen".into(),
                dialect_sha: "aa".repeat(32),
                workspace_root: "/w/one".into(),
                owner: "dead".into(),
                approvers: vec![],
                role: None,
                parent_session_id: parent.map(str::to_string),
            })
            .expect("the session row is written");
        let transcript = format!("{id}#t0");
        store
            .put_transcript(&transcript, id, &prefix_id)
            .expect("the transcript is written");
        let mut ledger = TokenLedger::new(&transcript, &tokens).expect("a ledger");
        for (seq, item) in items.iter().enumerate() {
            let toks = vec![100 + seq as u32];
            let row = ledger
                .append(&format!("{transcript}.{seq}"), &toks)
                .expect("a ledger row")
                .clone();
            store
                .append_item(&transcript, seq as u32, item, &row, &toks)
                .expect("the row is written");
        }
        transcript
    }

    fn user(text: &str) -> TranscriptItem {
        TranscriptItem::User {
            parts: vec![UserPart::Text { text: text.into() }],
            speaker: Speaker::Operator,
        }
    }

    fn assistant(text: &str) -> TranscriptItem {
        TranscriptItem::Assistant {
            text: text.into(),
            tool_calls: Vec::new(),
            truncated: false,
        }
    }

    /// The project's sessions a scan reads: archived included, a subagent removed by the filter
    /// and nothing from another folder - the three rules the head's `all` toggle is made of.
    #[test]
    fn a_project_scan_includes_the_archived_and_the_filter_removes_the_rest() {
        let store = Store::open_in_memory().expect("a store");
        seeded(&store, "s-main", None, vec![user("the cache key is warm")]);
        seeded(
            &store,
            "s-child",
            Some("s-main"),
            vec![assistant("the cache key again")],
        );
        seeded(&store, "s-old", None, vec![user("an old cache key")]);
        store
            .set_archived("s-old", true, Some("test"))
            .expect("archived");
        let every = store
            .sessions_to_search("/w/one", &|_| true)
            .expect("the listing reads");
        let ids: Vec<&str> = every.iter().map(|t| t.session.as_str()).collect();
        assert_eq!(ids.len(), 3, "a session is missing: {ids:?}");
        assert!(ids.contains(&"s-old"), "the archived session is not read");
        let main = store
            .sessions_to_search("/w/one", &|s| s.parent_session_id.is_none())
            .expect("the listing reads");
        let ids: Vec<&str> = main.iter().map(|t| t.session.as_str()).collect();
        assert_eq!(ids.len(), 2, "the subagent survived: {ids:?}");
        let found = store
            .search_rows("cache", &main, 0, 20)
            .expect("the scan runs");
        let sessions: Vec<&str> = found.hits.iter().map(|h| h.session.as_str()).collect();
        assert!(
            sessions.contains(&"s-old"),
            "no hit from the archive: {sessions:?}"
        );
        assert!(
            !sessions.contains(&"s-child"),
            "a subagent's row came back: {sessions:?}"
        );
        assert_eq!(found.hits[0].role, "mine");
        assert!(found.hits[0].excerpt.contains("cache"), "{:?}", found.hits);
    }

    /// Another folder's sessions are not this project's, and the key is the store's own rule.
    #[test]
    fn a_scan_reads_only_the_project_it_was_asked_about() {
        let store = Store::open_in_memory().expect("a store");
        seeded(&store, "s-main", None, vec![user("the cache key")]);
        let other = store
            .sessions_to_search("/w/two", &|_| true)
            .expect("the listing reads");
        assert!(other.is_empty(), "{other:?}");
    }

    /// A page is a window over the hits and says when another one exists, so a head asks for the
    /// next page rather than assuming it holds them all.
    #[test]
    fn a_page_is_bounded_and_says_when_the_next_one_exists() {
        let store = Store::open_in_memory().expect("a store");
        let rows: Vec<TranscriptItem> = (0..6).map(|i| assistant(&format!("needle {i}"))).collect();
        seeded(&store, "s-rows", None, rows);
        let targets = store
            .sessions_to_search("/w/one", &|_| true)
            .expect("the listing reads");
        let first = store
            .search_rows("needle", &targets, 0, 4)
            .expect("the scan");
        assert_eq!(first.hits.len(), 4);
        assert!(first.more, "a fifth hit exists and the page did not say so");
        // Newest first: the last row written is the first hit of the project.
        assert_eq!(first.hits[0].row, 5);
        let second = store
            .search_rows("needle", &targets, 4, 4)
            .expect("the scan");
        assert_eq!(second.hits.len(), 2);
        assert!(!second.more);
        assert_eq!(second.hits[1].row, 0);
    }

    /// A tool's words are found by `tool:` and only by the tool that ran, so a search tells a
    /// shell command from the sentence that proposed it.
    #[test]
    fn a_tool_filter_finds_only_that_tools_words() {
        let store = Store::open_in_memory().expect("a store");
        seeded(
            &store,
            "s-tools",
            None,
            vec![
                TranscriptItem::Assistant {
                    text: "listing the tree".into(),
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "bash".into(),
                        arguments: r#"{"command":"ls -la"}"#.into(),
                    }],
                    truncated: false,
                },
                TranscriptItem::ToolResult {
                    call_id: "c1".into(),
                    name: "bash".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: "a b c".into(),
                    edits: Vec::new(),
                    origin: None,
                    media: None,
                    ran_ms: None,
                    decision: None,
                },
            ],
        );
        let targets = store
            .sessions_to_search("/w/one", &|_| true)
            .expect("reads");
        let bash = store
            .search_rows("tool:bash \"ls -la\"", &targets, 0, 20)
            .expect("runs");
        assert_eq!(bash.hits.len(), 1, "{:?}", bash.hits);
        assert_eq!(bash.hits[0].tool, "bash");
        assert_eq!(bash.hits[0].role, "tool");
        let other = store
            .search_rows("tool:grep ls", &targets, 0, 20)
            .expect("runs");
        assert!(other.hits.is_empty(), "{:?}", other.hits);
    }

    /// One session's target by id is what the head's own find asks for, and it is found whether
    /// or not the session is in the archive.
    #[test]
    fn a_named_session_is_a_target_of_its_own() {
        let store = Store::open_in_memory().expect("a store");
        seeded(&store, "s-one", None, vec![user("the cache key")]);
        store
            .set_archived("s-one", true, Some("test"))
            .expect("archived");
        let target = store
            .scan_target("s-one")
            .expect("the session reads")
            .expect("the archived session is a target");
        assert_eq!(target.session, "s-one");
        assert_eq!(target.rows, 1);
        let found = store
            .search_rows("cache", std::slice::from_ref(&target), 0, 10)
            .expect("the scan runs");
        assert_eq!(found.hits.len(), 1);
        assert_eq!(found.hits[0].item_id, "s-one#t0.0");
    }
}
