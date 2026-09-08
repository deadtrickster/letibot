//! The SQLite store for the transcript and its ledger rows.
//!
//! §4.4 names the tables and says what each holds, in prose. It does not give
//! columns, and `docs/workstreams.md` §7 item 11 flags that: *"whoever writes W5
//! writes the schema; anyone else who touches it will guess columns."* This
//! module is the schema, and this comment is the reasoning.
//!
//! # Scope: four tables, not eight
//!
//! §4.4 lists eight persisted tables. Four of them belong to this strand --
//! `stable_prefix`, `session`, `transcript`, `transcript_item` -- and four
//! belong to strands that are not written yet: `event` (W7), `turn_metrics`
//! (W6), `adjudication_request` / `adjudication_decision` (W11), `tool_spill`
//! (W9), `segment` (W13). Those are **deliberately absent**. Inventing their
//! columns here would be the exact failure the workstreams document warns about,
//! one strand guessing another's contract, except worse, because a guess that is
//! already in a migration is harder to correct than one that is not. They arrive
//! as `migrate()` steps 2, 3, ... written by the people who know what goes in
//! them.
//!
//! # The decisions worth arguing about
//!
//! **The tokens are stored.** `transcript_item.tokens` holds the item's token
//! ids as a blob, and it is not redundant with the rendered text. §4.3 clause 6
//! says a restart rebuilds the region *by replaying the ledger's spans*, not by
//! re-rendering, and that is only possible if the ids are durable. Storing only
//! the text and re-tokenizing on restart would reintroduce the renderer
//! non-determinism the hash chain exists to detect, during recovery, when nobody
//! is watching. The cost is 4 bytes per token, about 800 KB for a 200 k-token
//! conversation, which is nothing next to the failure it prevents.
//!
//! **Per item, not one blob per transcript.** An append is one row insert; a
//! restart is one ordered scan. The ledger row and the tokens it describes are
//! written in the *same statement*, so they cannot disagree -- which is the one
//! way `restore` could be fed a lie it is unable to detect.
//!
//! **The stable prefix is content-addressed on `(dialect_sha, system, tools)`,
//! not on `(system, tools)`.** The tokens in that row were produced by a
//! particular renderer. Two dialects rendering the same system text and the same
//! tool schemas produce different tokens, so keying on the text alone would let
//! a GLM session inherit a Qwen session's prefix tokens and be structurally
//! unable to notice. §4.4 says the dialect id *is* the `template_sha`; this puts
//! it where it changes an answer.
//!
//! **`transcript.forked_at_seq` is mine and is not in the plan.** §5.5 says a
//! truncation produces a new transcript with a `parent_transcript_id`, but
//! nothing records *where* the fork happened. Without it a fork is
//! indistinguishable from a fresh transcript that happens to name a parent, and
//! the question "which items did these two share?" -- which is the question the
//! prefix cache is answering -- can only be recovered by comparing token blobs.
//! The `CHECK` makes the two fields live or die together.
//!
//! **Append-only is enforced in SQL, not only in Rust.** Three triggers:
//! `UPDATE` and `DELETE` on `transcript_item` `RAISE(ABORT)`, and an insert
//! trigger refuses any row whose `seq` is not the previous `seq + 1` or whose
//! `tok_offset` is not the previous `tok_offset + tok_len`. The last one is the
//! prefix invariant written as a constraint: a hole, an overlap, a reordering or
//! a re-insert at an old index is rejected by the database, whatever the caller
//! believed it was doing. `crate::ledger`'s type makes a rewrite inexpressible
//! in this process; the triggers make it inexpressible in `sqlite3` on the
//! command line too.
//!
//! **WAL, `synchronous = FULL`.** §4.4 says WAL. `FULL` rather than `NORMAL`
//! because the durable copy of the tokens is the *only* copy -- the region is
//! volatile by design -- so a lost commit is a lost turn, not a re-derivable
//! cache.
//!
//! # Times
//!
//! `created_at` is milliseconds since the Unix epoch, `INTEGER`. Not a string:
//! this column is only ever ordered and differenced.

use std::path::Path;

use letibot_transcript::TranscriptItem;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::ledger::LedgerRow;
use crate::vocab::TokenId;

pub const SCHEMA_VERSION: i64 = 1;

pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version     INTEGER NOT NULL
);

-- Content-addressed and shared between sessions: a new session with the same
-- system text, the same tools and the same dialect inherits a warm prefix for
-- free. (Measured in oracle: sharing one prefix took an identical request from
-- 9,325 tokens processed to 4.)
CREATE TABLE IF NOT EXISTS stable_prefix (
    id           TEXT PRIMARY KEY,   -- hex sha256 over (dialect_sha, system, tools)
    dialect_sha  TEXT NOT NULL,      -- the dialect's template_sha; part of the address
    system       TEXT NOT NULL,
    tools_json   TEXT NOT NULL,      -- JSON array of schema strings, in prefix order
    n_tokens     INTEGER NOT NULL,
    tokens       BLOB NOT NULL,      -- little-endian u32 ids
    h_init       BLOB NOT NULL,      -- 32 bytes: h_{-1} = H(tokens)
    vocab_source TEXT NOT NULL,      -- the GGUF these ids came out of
    created_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS session (
    id             TEXT PRIMARY KEY,
    title          TEXT,
    model_id       TEXT NOT NULL,
    dialect_sha    TEXT NOT NULL,    -- §4.4: the dialect id *is* the template_sha
    workspace_root TEXT NOT NULL,
    owner          TEXT NOT NULL,
    approvers_json TEXT NOT NULL,    -- JSON array of identities
    created_at     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS transcript (
    id                   TEXT PRIMARY KEY,
    session_id           TEXT NOT NULL REFERENCES session(id),
    parent_transcript_id TEXT REFERENCES transcript(id),  -- non-null after a fork
    forked_at_seq        INTEGER,                         -- items carried over
    stable_prefix_id     TEXT NOT NULL REFERENCES stable_prefix(id),
    created_at           INTEGER NOT NULL,
    CHECK ((parent_transcript_id IS NULL) = (forked_at_seq IS NULL))
);

CREATE TABLE IF NOT EXISTS transcript_item (
    transcript_id TEXT NOT NULL REFERENCES transcript(id),
    seq           INTEGER NOT NULL,   -- 0-based, dense; also the ledger row index
    item_id       TEXT NOT NULL,
    kind          TEXT NOT NULL,      -- the serde tag, denormalised for filtering
    item_json     TEXT NOT NULL,      -- the TranscriptItem itself
    tok_offset    INTEGER NOT NULL,   -- ledger row
    tok_len       INTEGER NOT NULL,   -- ledger row
    h_k           BLOB NOT NULL,      -- ledger row, 32 bytes
    tokens        BLOB NOT NULL,      -- little-endian u32 ids, tok_len of them
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (transcript_id, seq)
);

CREATE UNIQUE INDEX IF NOT EXISTS transcript_item_by_item_id
    ON transcript_item (transcript_id, item_id);

-- §4.1: "Immutable, with exactly one mutation: append." Said in SQL.
CREATE TRIGGER IF NOT EXISTS transcript_item_no_update
BEFORE UPDATE ON transcript_item BEGIN
    SELECT RAISE(ABORT, 'transcript_item is append-only: no UPDATE');
END;

CREATE TRIGGER IF NOT EXISTS transcript_item_no_delete
BEFORE DELETE ON transcript_item BEGIN
    SELECT RAISE(ABORT, 'transcript_item is append-only: no DELETE');
END;

-- The prefix invariant as a constraint. A hole, an overlap, a reordering or a
-- re-insert at an old index is refused by the database.
CREATE TRIGGER IF NOT EXISTS transcript_item_append_only_insert
BEFORE INSERT ON transcript_item BEGIN
    SELECT RAISE(ABORT, 'transcript_item seq must be the next one')
    WHERE NEW.seq <> (
        SELECT COALESCE(MAX(seq), -1) + 1
          FROM transcript_item WHERE transcript_id = NEW.transcript_id
    );
    SELECT RAISE(ABORT, 'transcript_item tok_offset must continue the previous row')
    WHERE NEW.seq > 0 AND NEW.tok_offset <> (
        SELECT tok_offset + tok_len FROM transcript_item
         WHERE transcript_id = NEW.transcript_id AND seq = NEW.seq - 1
    );
    SELECT RAISE(ABORT, 'transcript_item tok_len must match the token blob')
    WHERE NEW.tok_len <> LENGTH(NEW.tokens) / 4;
END;
"#;

#[derive(Debug)]
pub enum StoreError {
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    /// A blob whose byte length is not a whole number of token ids, or a hash
    /// that is not 32 bytes. Means the file was written by something else.
    Corrupt(String),
    NotFound(String),
    /// The file was written by a newer build.
    SchemaTooNew { found: i64, known: i64 },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sql(e) => write!(f, "sqlite: {e}"),
            StoreError::Json(e) => write!(f, "json: {e}"),
            StoreError::Corrupt(m) => write!(f, "store is corrupt: {m}"),
            StoreError::NotFound(m) => write!(f, "not found: {m}"),
            StoreError::SchemaTooNew { found, known } => write!(
                f,
                "store schema is version {found}; this build knows {known}"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sql(e)
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Json(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Token ids as a little-endian blob. Explicit, not a pointer cast: this blob
/// outlives the machine that wrote it.
pub fn tokens_to_blob(tokens: &[TokenId]) -> Vec<u8> {
    let mut v = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        v.extend_from_slice(&t.to_le_bytes());
    }
    v
}

pub fn blob_to_tokens(blob: &[u8]) -> Result<Vec<TokenId>> {
    if !blob.len().is_multiple_of(4) {
        return Err(StoreError::Corrupt(format!(
            "token blob of {} bytes is not a whole number of ids",
            blob.len()
        )));
    }
    Ok(blob
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect())
}

fn blob_to_hash(blob: &[u8]) -> Result<[u8; 32]> {
    <[u8; 32]>::try_from(blob)
        .map_err(|_| StoreError::Corrupt(format!("hash of {} bytes, want 32", blob.len())))
}

/// The `serde` tag of an item, denormalised into its own column so a query can
/// filter by kind without parsing every row's JSON.
pub fn item_kind(item: &TranscriptItem) -> &'static str {
    match item {
        TranscriptItem::System { .. } => "system",
        TranscriptItem::User { .. } => "user",
        TranscriptItem::Reasoning { .. } => "reasoning",
        TranscriptItem::Assistant { .. } => "assistant",
        TranscriptItem::ToolResult { .. } => "tool_result",
        TranscriptItem::SegmentMark { .. } => "segment_mark",
    }
}

/// A stable prefix as it goes into the store.
#[derive(Debug, Clone, PartialEq)]
pub struct StablePrefixRecord {
    pub dialect_sha: String,
    pub system: String,
    pub tools_json: Vec<String>,
    pub tokens: Vec<TokenId>,
    pub h_init: [u8; 32],
    pub vocab_source: String,
}

impl StablePrefixRecord {
    /// The content address: `H(dialect_sha || 0 || system || 0 || tool || 0 ...)`.
    ///
    /// The dialect is in the address because the *tokens* are what this row
    /// stores, and two dialects render the same text to different tokens.
    pub fn id(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.dialect_sha.as_bytes());
        h.update([0]);
        h.update(self.system.as_bytes());
        h.update([0]);
        for tool in &self.tools_json {
            h.update(tool.as_bytes());
            h.update([0]);
        }
        crate::ledger::hex(&h.finalize().into())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: String,
    pub title: Option<String>,
    pub model_id: String,
    pub dialect_sha: String,
    pub workspace_root: String,
    pub owner: String,
    pub approvers: Vec<String>,
}

/// Everything needed to rebuild a ledger and a transcript after a restart.
#[derive(Debug, Clone)]
pub struct LoadedTranscript {
    pub transcript_id: String,
    pub session_id: String,
    pub parent_transcript_id: Option<String>,
    pub forked_at_seq: Option<u32>,
    pub stable_prefix_id: String,
    pub prefix_tokens: Vec<TokenId>,
    pub h_init: [u8; 32],
    /// Item, its ledger row, and its tokens -- in `seq` order.
    pub items: Vec<(TranscriptItem, LedgerRow, Vec<TokenId>)>,
}

impl LoadedTranscript {
    /// The `(row, tokens)` pairs `TokenLedger::restore` wants.
    pub fn ledger_input(&self) -> Vec<(LedgerRow, Vec<TokenId>)> {
        self.items
            .iter()
            .map(|(_, row, toks)| (row.clone(), toks.clone()))
            .collect()
    }
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // The region is volatile by design, so this file is the only copy of the
        // tokens. A lost commit is a lost turn.
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let found: Option<i64> = self
            .conn
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .optional()
            .unwrap_or(None);
        match found {
            Some(v) if v > SCHEMA_VERSION => {
                return Err(StoreError::SchemaTooNew {
                    found: v,
                    known: SCHEMA_VERSION,
                });
            }
            Some(_) => return Ok(()),
            None => {}
        }
        self.conn.execute_batch(SCHEMA_SQL)?;
        self.conn
            .execute("DELETE FROM schema_version", [])?;
        self.conn
            .execute("INSERT INTO schema_version (version) VALUES (?1)", params![SCHEMA_VERSION])?;
        Ok(())
    }

    /// Insert a stable prefix, or return the id of the identical one already
    /// there. Idempotent because the id is the content.
    pub fn put_stable_prefix(&self, rec: &StablePrefixRecord) -> Result<String> {
        let id = rec.id();
        self.conn.execute(
            "INSERT OR IGNORE INTO stable_prefix
               (id, dialect_sha, system, tools_json, n_tokens, tokens, h_init, vocab_source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                rec.dialect_sha,
                rec.system,
                serde_json::to_string(&rec.tools_json)?,
                rec.tokens.len() as i64,
                tokens_to_blob(&rec.tokens),
                rec.h_init.as_slice(),
                rec.vocab_source,
                now_ms(),
            ],
        )?;
        Ok(id)
    }

    pub fn get_stable_prefix(&self, id: &str) -> Result<(Vec<TokenId>, [u8; 32])> {
        let (blob, hash): (Vec<u8>, Vec<u8>) = self
            .conn
            .query_row(
                "SELECT tokens, h_init FROM stable_prefix WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("stable_prefix {id}")))?;
        Ok((blob_to_tokens(&blob)?, blob_to_hash(&hash)?))
    }

    pub fn put_session(&self, rec: &SessionRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO session
               (id, title, model_id, dialect_sha, workspace_root, owner, approvers_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                rec.id,
                rec.title,
                rec.model_id,
                rec.dialect_sha,
                rec.workspace_root,
                rec.owner,
                serde_json::to_string(&rec.approvers)?,
                now_ms(),
            ],
        )?;
        Ok(())
    }

    /// A fresh transcript. `parent` is `None`; use [`Store::put_fork`] otherwise.
    pub fn put_transcript(
        &self,
        transcript_id: &str,
        session_id: &str,
        stable_prefix_id: &str,
    ) -> Result<()> {
        self.insert_transcript(transcript_id, session_id, stable_prefix_id, None)
    }

    /// A transcript that came from a fork (§5.5).
    ///
    /// Separate entry point rather than an `Option` argument, so that recording
    /// the divergence is not something a caller can forget by passing `None`.
    pub fn put_fork(
        &self,
        transcript_id: &str,
        session_id: &str,
        stable_prefix_id: &str,
        parent_transcript_id: &str,
        forked_at_seq: u32,
    ) -> Result<()> {
        self.insert_transcript(
            transcript_id,
            session_id,
            stable_prefix_id,
            Some((parent_transcript_id, forked_at_seq)),
        )
    }

    fn insert_transcript(
        &self,
        transcript_id: &str,
        session_id: &str,
        stable_prefix_id: &str,
        fork: Option<(&str, u32)>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO transcript
               (id, session_id, parent_transcript_id, forked_at_seq, stable_prefix_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                transcript_id,
                session_id,
                fork.map(|(p, _)| p),
                fork.map(|(_, s)| s as i64),
                stable_prefix_id,
                now_ms(),
            ],
        )?;
        Ok(())
    }

    /// Append one item and its ledger row, together.
    ///
    /// One statement, so the row and the tokens it describes cannot end up
    /// disagreeing; and the insert trigger refuses it if `seq` or `tok_offset`
    /// does not continue the previous row.
    pub fn append_item(
        &self,
        transcript_id: &str,
        seq: u32,
        item: &TranscriptItem,
        row: &LedgerRow,
        tokens: &[TokenId],
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO transcript_item
               (transcript_id, seq, item_id, kind, item_json,
                tok_offset, tok_len, h_k, tokens, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                transcript_id,
                seq as i64,
                row.item_id,
                item_kind(item),
                serde_json::to_string(item)?,
                row.tok_offset as i64,
                row.tok_len as i64,
                row.h_k.as_slice(),
                tokens_to_blob(tokens),
                now_ms(),
            ],
        )?;
        Ok(())
    }

    /// Everything needed to rebuild the ledger.
    pub fn load_transcript(&self, transcript_id: &str) -> Result<LoadedTranscript> {
        let (session_id, parent, forked_at, prefix_id): (String, Option<String>, Option<i64>, String) =
            self.conn
                .query_row(
                    "SELECT session_id, parent_transcript_id, forked_at_seq, stable_prefix_id
                       FROM transcript WHERE id = ?1",
                    params![transcript_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("transcript {transcript_id}")))?;

        let (prefix_tokens, h_init) = self.get_stable_prefix(&prefix_id)?;

        let mut stmt = self.conn.prepare(
            "SELECT item_id, item_json, tok_offset, tok_len, h_k, tokens
               FROM transcript_item WHERE transcript_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![transcript_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Vec<u8>>(4)?,
                r.get::<_, Vec<u8>>(5)?,
            ))
        })?;

        let mut items = Vec::new();
        for row in rows {
            let (item_id, item_json, tok_offset, tok_len, h_k, blob) = row?;
            let item: TranscriptItem = serde_json::from_str(&item_json)?;
            let ledger_row = LedgerRow {
                item_id,
                tok_offset: tok_offset as u32,
                tok_len: tok_len as u32,
                h_k: blob_to_hash(&h_k)?,
            };
            items.push((item, ledger_row, blob_to_tokens(&blob)?));
        }

        Ok(LoadedTranscript {
            transcript_id: transcript_id.to_string(),
            session_id,
            parent_transcript_id: parent,
            forked_at_seq: forked_at.map(|s| s as u32),
            stable_prefix_id: prefix_id,
            prefix_tokens,
            h_init,
            items,
        })
    }

    pub fn item_count(&self, transcript_id: &str) -> Result<u32> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM transcript_item WHERE transcript_id = ?1",
            params![transcript_id],
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    /// Escape hatch for the tests below and for `EXPLAIN`. Read-only by
    /// convention only, which is why the append-only guarantees live in triggers
    /// rather than in whoever holds this reference.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{TokenLedger, hash_tokens};
    use letibot_transcript::{ReasoningField, SystemOrigin};

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn prefix() -> StablePrefixRecord {
        let tokens = vec![1u32, 2, 3, 4];
        StablePrefixRecord {
            dialect_sha: "aa".repeat(32),
            system: "you are a harness".into(),
            tools_json: vec![r#"{"name":"read"}"#.into()],
            h_init: hash_tokens(&tokens),
            tokens,
            vocab_source: "/models/x.gguf".into(),
        }
    }

    fn seeded(s: &Store) -> (String, String) {
        let p = prefix();
        let prefix_id = s.put_stable_prefix(&p).unwrap();
        s.put_session(&SessionRecord {
            id: "sess-1".into(),
            title: Some("t".into()),
            model_id: "qwen".into(),
            dialect_sha: p.dialect_sha.clone(),
            workspace_root: "/w".into(),
            owner: "deadtrickster".into(),
            approvers: vec!["deadtrickster".into()],
        })
        .unwrap();
        s.put_transcript("tr-1", "sess-1", &prefix_id).unwrap();
        ("tr-1".into(), prefix_id)
    }

    #[test]
    fn a_stable_prefix_is_addressed_by_its_content_and_its_dialect() {
        let s = store();
        let p = prefix();
        let a = s.put_stable_prefix(&p).unwrap();
        let b = s.put_stable_prefix(&p).unwrap();
        assert_eq!(a, b, "the same prefix must not be stored twice");

        // Same text, different renderer, therefore different tokens: it must not
        // collide, or a GLM session would inherit Qwen's ids.
        let mut other = p.clone();
        other.dialect_sha = "bb".repeat(32);
        other.tokens = vec![9, 9, 9];
        other.h_init = hash_tokens(&other.tokens);
        let c = s.put_stable_prefix(&other).unwrap();
        assert_ne!(a, c);
        assert_eq!(s.get_stable_prefix(&a).unwrap().0, vec![1, 2, 3, 4]);
        assert_eq!(s.get_stable_prefix(&c).unwrap().0, vec![9, 9, 9]);
    }

    #[test]
    fn transcript_item_cannot_be_updated_or_deleted() {
        let s = store();
        let (tr, _) = seeded(&s);
        let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();
        let item = TranscriptItem::User {
            parts: vec![letibot_transcript::UserPart::Text { text: "hi".into() }],
        };
        let row = ledger.append("it-0", &[10, 11]).unwrap().clone();
        s.append_item(&tr, 0, &item, &row, &[10, 11]).unwrap();

        let update = s.conn.execute(
            "UPDATE transcript_item SET item_json = '{}' WHERE transcript_id = ?1",
            params![tr],
        );
        assert!(update.is_err(), "UPDATE must be refused by the trigger");
        assert!(format!("{:?}", update.unwrap_err()).contains("append-only"));

        let delete = s
            .conn
            .execute("DELETE FROM transcript_item WHERE transcript_id = ?1", params![tr]);
        assert!(delete.is_err(), "DELETE must be refused by the trigger");
        assert!(format!("{:?}", delete.unwrap_err()).contains("append-only"));

        assert_eq!(s.item_count(&tr).unwrap(), 1);
    }

    #[test]
    fn a_row_that_does_not_continue_the_previous_one_is_refused() {
        let s = store();
        let (tr, _) = seeded(&s);
        let item = TranscriptItem::Reasoning {
            text: "t".into(),
            field: ReasoningField::Inline,
        };
        let good = LedgerRow { item_id: "a".into(), tok_offset: 4, tok_len: 2, h_k: [1; 32] };
        s.append_item(&tr, 0, &item, &good, &[1, 2]).unwrap();

        // A hole in the token stream.
        let gap = LedgerRow { item_id: "b".into(), tok_offset: 9, tok_len: 1, h_k: [2; 32] };
        assert!(s.append_item(&tr, 1, &item, &gap, &[3]).is_err());

        // A seq that skips.
        let skip = LedgerRow { item_id: "c".into(), tok_offset: 6, tok_len: 1, h_k: [3; 32] };
        assert!(s.append_item(&tr, 5, &item, &skip, &[3]).is_err());

        // A re-insert at an index already used: the seq trigger catches it before
        // the primary key does, and either way it is refused.
        let reuse = LedgerRow { item_id: "d".into(), tok_offset: 4, tok_len: 2, h_k: [4; 32] };
        assert!(s.append_item(&tr, 0, &item, &reuse, &[1, 2]).is_err());

        // A blob that does not match the row's length.
        let lying = LedgerRow { item_id: "e".into(), tok_offset: 6, tok_len: 7, h_k: [5; 32] };
        assert!(s.append_item(&tr, 1, &item, &lying, &[3]).is_err());

        // And the good continuation still works.
        let next = LedgerRow { item_id: "f".into(), tok_offset: 6, tok_len: 1, h_k: [6; 32] };
        s.append_item(&tr, 1, &item, &next, &[3]).unwrap();
        assert_eq!(s.item_count(&tr).unwrap(), 2);
    }

    #[test]
    fn a_fork_records_where_it_forked() {
        let s = store();
        let (tr, prefix_id) = seeded(&s);
        s.put_fork("tr-2", "sess-1", &prefix_id, &tr, 3).unwrap();
        let loaded = s.load_transcript("tr-2").unwrap();
        assert_eq!(loaded.parent_transcript_id.as_deref(), Some("tr-1"));
        assert_eq!(loaded.forked_at_seq, Some(3));

        let fresh = s.load_transcript(&tr).unwrap();
        assert_eq!(fresh.parent_transcript_id, None);
        assert_eq!(fresh.forked_at_seq, None);

        // The CHECK keeps the two fields together.
        let half = s.conn.execute(
            "INSERT INTO transcript (id, session_id, parent_transcript_id, forked_at_seq,
                                     stable_prefix_id, created_at)
             VALUES ('tr-3', 'sess-1', 'tr-1', NULL, ?1, 0)",
            params![prefix_id],
        );
        assert!(half.is_err(), "a parent without a fork point must be refused");
    }

    #[test]
    fn store_round_trip_rebuilds_the_same_ledger() {
        let s = store();
        let (tr, _) = seeded(&s);
        let mut ledger = TokenLedger::new(&tr, &[1, 2, 3, 4]).unwrap();

        let items: Vec<TranscriptItem> = vec![
            TranscriptItem::System { text: "boot".into(), origin: SystemOrigin::Bootstrap },
            TranscriptItem::User {
                parts: vec![letibot_transcript::UserPart::Text { text: "hello".into() }],
            },
            TranscriptItem::Reasoning { text: "think".into(), field: ReasoningField::Inline },
            TranscriptItem::SegmentMark {
                segment_id: "s0".into(),
                label: "l".into(),
                kind: "k".into(),
                edge: letibot_transcript::SegmentEdge::Open,
            },
            TranscriptItem::Assistant { text: "hi".into(), tool_calls: vec![] },
        ];
        // The SegmentMark renders to nothing; every other item to something.
        let payloads: Vec<Vec<u32>> =
            vec![vec![20, 21], vec![22], vec![23, 24, 25], vec![], vec![26]];

        for (seq, (item, toks)) in items.iter().zip(&payloads).enumerate() {
            let row = ledger.append(&format!("it-{seq}"), toks).unwrap().clone();
            s.append_item(&tr, seq as u32, item, &row, toks).unwrap();
        }

        let loaded = s.load_transcript(&tr).unwrap();
        assert_eq!(loaded.prefix_tokens, vec![1, 2, 3, 4]);
        assert_eq!(loaded.h_init, ledger.h_init());
        assert_eq!(loaded.items.len(), 5);
        assert_eq!(
            loaded.items.iter().map(|(i, ..)| i.clone()).collect::<Vec<_>>(),
            items
        );

        let restored = TokenLedger::restore(
            "tr-1-restored",
            &loaded.prefix_tokens,
            loaded.h_init,
            &loaded.ledger_input(),
        )
        .unwrap();
        assert_eq!(restored.tokens(), ledger.tokens());
        assert_eq!(restored.head(), ledger.head());
        assert_eq!(restored.rows(), ledger.rows());
        assert_eq!(restored.span(), ledger.span());
        restored.verify_chain().unwrap();
    }

    #[test]
    fn kinds_are_denormalised_correctly() {
        let s = store();
        let (tr, _) = seeded(&s);
        let mut ledger = TokenLedger::new(&tr, &[]).unwrap();
        let item = TranscriptItem::ToolResult {
            call_id: "c".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Abstained { reason: "no cover".into() },
            payload: "{}".into(),
        };
        let row = ledger.append("it", &[1]).unwrap().clone();
        s.append_item(&tr, 0, &item, &row, &[1]).unwrap();
        let kind: String = s
            .conn
            .query_row("SELECT kind FROM transcript_item", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "tool_result");

        // Abstained must survive the round trip as itself, not as Ok.
        let back = s.load_transcript(&tr).unwrap();
        assert_eq!(back.items[0].0, item);
    }
}
