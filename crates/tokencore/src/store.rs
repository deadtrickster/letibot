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

/// Why `session.role` is nullable, and why the comment is here and not in the DDL.
///
/// A role belongs to the SESSION and not to the process. The runtime was already
/// shaped for that: `Sessions` holds one `Harness` per session and a `ToolRuntime`
/// is per-`Harness`, so two sessions in one daemon can already carry different tool
/// sets — they all resolved from the same `--role` flag only because there was
/// nowhere to record anything else. `stable_prefix` has keyed on `tools_json` since
/// v1, so two roles in one daemon get two content-addressed prefix rows with no
/// coordination needed.
///
/// **NULL is not `coder`.** It means *unrecorded*, which is what every session
/// written before v2 is, and it has to keep meaning "the daemon's own role" or a
/// resume would silently re-seat an old conversation.
///
/// The prose lives in Rust because SQLite's `ALTER TABLE ... DROP COLUMN` rewrites
/// the stored `CREATE TABLE` text and fails with `incomplete input` when a
/// multi-line comment sits inside the parens. That is not hypothetical: it broke
/// the migration test's fixture, which builds a v1 store by dropping this column.
pub const ROLE_COLUMN: () = ();

pub const SCHEMA_VERSION: i64 = 3;

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
    created_at     INTEGER NOT NULL,
    role           TEXT              -- v2; see ROLE_COLUMN below. NULL = unrecorded
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

-- The session's todo list, as the model last wrote it. Mutable metadata about a
-- conversation, in the same class as the title: it rides no chain and carries no
-- append-only trigger, because a todo list the model revised three times is one
-- list with a history nobody asked to keep. One row per session, replaced whole.
CREATE TABLE IF NOT EXISTS todo (
    session_id  TEXT PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
    todos_json  TEXT NOT NULL,   -- JSON array of {content, status}
    updated_ms  INTEGER NOT NULL
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
    /// The store is fine and the operation is not allowed. Separate from
    /// [`StoreError::Corrupt`] because they send a reader to opposite places: one is
    /// "your database is damaged", the other is "the database is doing its job".
    /// Printing a refused delete as corruption is how an operator ends up running
    /// `PRAGMA integrity_check` on a healthy file.
    Refused(String),
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
            StoreError::Refused(m) => write!(f, "{m}"),
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
    /// The role to seat, or `None` for the daemon's own.
    pub role: Option<String>,
}

/// A session as the store holds it: enough to list it, pick it and resume it.
///
/// Separate from [`SessionRecord`], which is the *write* shape. This one carries
/// three things that are computed rather than stored — the transcript id, the row
/// count and the last activity — because every caller that lists sessions needs all
/// three and would otherwise write the same three subqueries slightly differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSession {
    pub id: String,
    /// `None` when nobody has named it. Distinct from `Some("")`, which
    /// [`Store::set_title`] does not write.
    pub title: Option<String>,
    pub model_id: String,
    pub dialect_sha: String,
    pub workspace_root: String,
    pub owner: String,
    /// The role this session was opened with, or `None` if it predates v2 or was
    /// opened without one. `None` means the daemon's `--role`, not `coder`.
    pub role: Option<String>,
    pub created_ms: i64,
    /// The newest transcript for this session, or `None` if it has none at all.
    pub transcript_id: Option<String>,
    /// Rows in the session's current (newest) transcript. Not every transcript's
    /// rows summed: with forks, that would count the history a compaction just
    /// stopped carrying.
    pub items: u32,
    /// When the last row was written, falling back to `created_ms` for a session
    /// nothing ran in. Never `Option`: "never used" is a time, not an absence, and
    /// an `Option` here would make every caller invent the same fallback.
    pub last_activity_ms: i64,
}

/// One line of a session's todo list, as the model wrote it.
///
/// Stored as mutable session metadata — the `set_title` class, not the
/// append-only class: a todo list the model revised three times is one list with
/// a history nobody asked to keep. The whole list is replaced on every write,
/// because a delta the model got wrong is a delta nobody can audit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

/// A todo's state. Serde as the lower-case words, so a stored list reads the
/// same in `sqlite3` as it does here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// What a stable prefix was rendered from and by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StablePrefixMeta {
    pub dialect_sha: String,
    pub vocab_source: String,
    pub n_tokens: u32,
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
        // WAL lets readers and the writer run at once, but two *writers* still
        // serialise, and rusqlite's default is to fail immediately rather than wait.
        // Two connections exist by design — the worker writes rows, a second one
        // answers a head's list and writes a title — so a rename landing during a
        // turn's `append_item` must wait a moment rather than come back as
        // SQLITE_BUSY on a database that is working exactly as intended.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
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
            // An older store is MIGRATED, not left alone and not rebuilt. Before v2
            // this arm was `Some(_) => return Ok(())`, so `SCHEMA_SQL` only ever ran
            // on an empty file and there was no way to add a column to a store that
            // already held conversations. The rows are append-only and cannot be
            // recreated, so a migration is the only shape this can take.
            Some(v) if v < SCHEMA_VERSION => {
                self.migrate_from(v)?;
                self.conn.execute("DELETE FROM schema_version", [])?;
                self.conn.execute(
                    "INSERT INTO schema_version (version) VALUES (?1)",
                    params![SCHEMA_VERSION],
                )?;
                return Ok(());
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

    /// Apply the steps from `from` up to [`SCHEMA_VERSION`].
    ///
    /// One arm per version, each idempotent on its own, so a store two versions
    /// behind is carried forward by running both rather than by a special case.
    ///
    /// **A step may add and it may not destroy.** `transcript_item` carries a
    /// `BEFORE DELETE` trigger that raises, so a migration that wanted to rewrite
    /// rows could not, and that is the guarantee rather than a convention here.
    fn migrate_from(&self, from: i64) -> Result<()> {
        if from < 2 {
            // v2: a session records its own role. `ALTER TABLE ... ADD COLUMN` with
            // no default writes NULL into every existing row, which is exactly the
            // "unrecorded, use the daemon default" case the column documents.
            self.conn
                .execute_batch("ALTER TABLE session ADD COLUMN role TEXT")?;
        }
        if from < 3 {
            // v3: the session todo list. A migrated store never runs `SCHEMA_SQL`
            // — `migrate` returns early — so the table is created here, and
            // `IF NOT EXISTS` keeps a store that somehow already has one honest.
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS todo (
                     session_id  TEXT PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
                     todos_json  TEXT NOT NULL,
                     updated_ms  INTEGER NOT NULL
                 );",
            )?;
        }
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

    /// The whole stable-prefix row, for a caller rebuilding the prefix a session
    /// was actually created with.
    ///
    /// A compaction fork keeps the session's own prefix rather than the one the
    /// running daemon would render now — the same rule a resume follows — and
    /// building that fork needs the record's `system` and `tools_json`, which the
    /// token-and-hash getter above does not carry.
    pub fn stable_prefix_record(&self, id: &str) -> Result<StablePrefixRecord> {
        let (dialect_sha, system, tools_json, tokens, h_init, vocab_source): (
            String,
            String,
            String,
            Vec<u8>,
            Vec<u8>,
            String,
        ) = self
            .conn
            .query_row(
                "SELECT dialect_sha, system, tools_json, tokens, h_init, vocab_source
                   FROM stable_prefix WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("stable_prefix {id}")))?;
        Ok(StablePrefixRecord {
            dialect_sha,
            system,
            tools_json: serde_json::from_str(&tools_json)?,
            tokens: blob_to_tokens(&tokens)?,
            h_init: blob_to_hash(&h_init)?,
            vocab_source,
        })
    }

    pub fn put_session(&self, rec: &SessionRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO session
               (id, title, model_id, dialect_sha, workspace_root, owner, approvers_json,
                created_at, role)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                rec.id,
                rec.title,
                rec.model_id,
                rec.dialect_sha,
                rec.workspace_root,
                rec.owner,
                serde_json::to_string(&rec.approvers)?,
                now_ms(),
                rec.role,
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

    /// Every session in the store, newest activity first: what a picker or
    /// `letibot --sessions` is drawn from.
    ///
    /// Ordered by the **last row written**, not by `session.created_at`. "Which one
    /// was I just in" is the question this list is asked, and a session created on
    /// Monday and used ten minutes ago is the answer to it; ordering by creation
    /// puts it at the bottom.
    ///
    /// `LEFT JOIN`, so a session whose transcript row exists but holds nothing still
    /// appears — with `items: 0` and `last_activity_ms` falling back to its creation
    /// time. Those are exactly the sessions somebody wants to delete, and a list that
    /// hides them is a list that cannot be acted on.
    pub fn list_sessions(&self) -> Result<Vec<StoredSession>> {
        // The "current transcript" subquery orders by `created_at DESC, rowid
        // DESC`, and the rowid is not decoration: a compaction fork is written in
        // the same millisecond as the transcript it forked from, and a
        // created-at-only order breaks that tie arbitrarily — a resume could come
        // back on the parent, putting the full history behind the next prompt.
        // Insertion order is what "newest" means here.
        //
        // The item count is the **current transcript's** rows, not every
        // transcript's: since forks exist, a cross-transcript count double-counts
        // the history a compaction just stopped carrying, and a picker row that
        // grew because the conversation got shorter is a lie.
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.title, s.model_id, s.dialect_sha, s.workspace_root, s.owner,
                    s.created_at,
                    (SELECT t.id FROM transcript t
                      WHERE t.session_id = s.id
                      ORDER BY t.created_at DESC, t.rowid DESC LIMIT 1),
                    (SELECT COUNT(*) FROM transcript_item i
                      WHERE i.transcript_id = (SELECT t.id FROM transcript t
                          WHERE t.session_id = s.id
                          ORDER BY t.created_at DESC, t.rowid DESC LIMIT 1)),
                    (SELECT MAX(i.created_at) FROM transcript_item i
                       JOIN transcript t ON t.id = i.transcript_id
                      WHERE t.session_id = s.id),
                    s.role
               FROM session s",
        )?;
        let mut out: Vec<StoredSession> = stmt
            .query_map([], |r| {
                let created_ms: i64 = r.get(6)?;
                let last: Option<i64> = r.get(9)?;
                Ok(StoredSession {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    model_id: r.get(2)?,
                    dialect_sha: r.get(3)?,
                    workspace_root: r.get(4)?,
                    owner: r.get(5)?,
                    created_ms,
                    transcript_id: r.get(7)?,
                    items: r.get::<_, i64>(8)? as u32,
                    last_activity_ms: last.unwrap_or(created_ms),
                    role: r.get(10)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        out.sort_by_key(|s| std::cmp::Reverse(s.last_activity_ms));
        Ok(out)
    }

    /// The name on disk right now, or `None` for a session nobody has named.
    ///
    /// Read rather than remembered: a title can be set by a head through the
    /// registry's own connection while a harness holds an older idea of it, and a
    /// derivation that trusted its cached copy would overwrite a name the operator
    /// had just typed.
    pub fn title(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT title FROM session WHERE id = ?1", params![id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten()
            .filter(|t: &String| !t.is_empty()))
    }

    /// One session, or `None`.
    pub fn session(&self, id: &str) -> Result<Option<StoredSession>> {
        Ok(self.list_sessions()?.into_iter().find(|s| s.id == id))
    }

    /// Name a session, or clear its name with an empty string.
    ///
    /// `session` carries no append-only trigger and is not part of the chain: a
    /// title is metadata *about* a conversation and changing it changes no prompt
    /// byte. That is the whole reason renaming is allowed here while
    /// [`Store::append_item`]'s rows cannot be touched at all.
    pub fn set_title(&self, id: &str, title: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE session SET title = ?2 WHERE id = ?1",
            params![id, (!title.is_empty()).then(|| title.to_string())],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }

    /// The session's todo list, in the order the model last wrote it.
    ///
    /// Empty when the session has none — "never wrote one" and "cleared it" are
    /// the same state from the outside, and pretending they differ is a pane
    /// showing a distinction nothing stands behind.
    pub fn todos(&self, session_id: &str) -> Result<Vec<TodoItem>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT todos_json FROM todo WHERE session_id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json
            .map(|j| serde_json::from_str(&j))
            .transpose()?
            .unwrap_or_default())
    }

    /// Replace the session's todo list wholesale. The whole list every time —
    /// there is no append, no reorder-by-id, no delta: the model writes what the
    /// list now is, and that is what the store holds.
    pub fn put_todos(&self, session_id: &str, todos: &[TodoItem]) -> Result<()> {
        self.conn.execute(
            "INSERT INTO todo (session_id, todos_json, updated_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET todos_json = ?2, updated_ms = ?3",
            params![session_id, serde_json::to_string(todos)?, now_ms()],
        )?;
        Ok(())
    }

    /// Remove a session that holds no transcript rows.
    ///
    /// **A session with rows cannot be deleted, and that is not an omission.**
    /// `transcript_item` carries a `BEFORE DELETE` trigger that raises — §4.1's
    /// "immutable, with exactly one mutation: append", said in SQL — so there is no
    /// statement this method could run that would remove a conversation. Offering a
    /// `--delete` that quietly left the rows behind and dropped only the `session`
    /// row would be worse than refusing: the tokens would still be on disk, orphaned
    /// and unreachable, and the operator would believe they were gone.
    ///
    /// What *is* deletable is a session nothing ever ran in — the row a daemon
    /// writes at startup and a `/new` that was never used. Those accumulate, they
    /// are the ones a picker is cluttered by, and removing them destroys nothing.
    pub fn delete_empty_session(&self, id: &str) -> Result<()> {
        let Some(s) = self.session(id)? else {
            return Err(StoreError::NotFound(format!("session {id}")));
        };
        if s.items > 0 {
            return Err(StoreError::Refused(format!(
                "session {id} holds {} transcript row(s) and cannot be deleted: \
                 transcript_item carries a BEFORE DELETE trigger, because an \
                 append-only log whose history can be removed is not one. Rename it \
                 instead.",
                s.items
            )));
        }
        // Order matters: `transcript.session_id` is a foreign key and
        // `foreign_keys` is on.
        self.conn.execute(
            "DELETE FROM transcript WHERE session_id = ?1",
            params![id],
        )?;
        self.conn
            .execute("DELETE FROM session WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// What a stable prefix was rendered from and by. `None` if it is not here.
    pub fn stable_prefix_meta(&self, id: &str) -> Result<Option<StablePrefixMeta>> {
        Ok(self
            .conn
            .query_row(
                "SELECT dialect_sha, vocab_source, n_tokens FROM stable_prefix WHERE id = ?1",
                params![id],
                |r| {
                    Ok(StablePrefixMeta {
                        dialect_sha: r.get(0)?,
                        vocab_source: r.get(1)?,
                        n_tokens: r.get::<_, i64>(2)? as u32,
                    })
                },
            )
            .optional()?)
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
            role: None,
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
            TranscriptItem::Assistant { text: "hi".into(), tool_calls: vec![], truncated: false },
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

    /// A v1 store — one written before a session could record its role — is carried
    /// forward, and every row in it reads back as "unrecorded".
    ///
    /// This is the test the operator's ten live sessions depend on. `migrate()` used
    /// to return `Ok(())` for any version it already knew, so a column added to
    /// `SCHEMA_SQL` reached a fresh file and nothing else; the failure would have been
    /// a `no such column: role` on the first list, after the daemon had already
    /// started.
    #[test]
    fn a_v1_store_is_migrated_and_its_rows_read_as_unrecorded() {
        // A file, not `open_in_memory`, because the point is a store that already
        // exists on disk at an older version. No dev-dependency for one test.
        let path = std::env::temp_dir().join(format!(
            "letibot-migrate-v1-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        struct Clean(std::path::PathBuf);
        impl Drop for Clean {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _clean = Clean(path.clone());

        // Make a REAL store, then reverse the v2 step on it. Hand-writing the v1
        // DDL was the first attempt and it was wrong: the fixture lacked
        // `transcript`, which `list_sessions` joins, so the test failed on a table
        // the migration never touches. Reversing a real store keeps every other
        // table exactly as v1 had it and cannot drift from `SCHEMA_SQL`.
        {
            let s = Store::open(&path).unwrap();
            s.put_session(&SessionRecord {
                id: "s-old".into(),
                title: Some("a title".into()),
                model_id: "m".into(),
                dialect_sha: "sha".into(),
                workspace_root: "/w".into(),
                owner: "dead".into(),
                role: None,
                approvers: vec![],
            })
            .unwrap();
        }
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(
                "ALTER TABLE session DROP COLUMN role;
                 DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (1);",
            )
            .unwrap();
            // Prove the fixture really is v1: the column is gone.
            assert!(
                c.query_row("SELECT role FROM session", [], |r| r.get::<_, Option<String>>(0))
                    .is_err(),
                "the fixture still has a role column, so it is not a v1 store"
            );
        }

        // Opening it runs the migration.
        let s = Store::open(&path).unwrap();
        let got = s.session("s-old").unwrap().expect("the v1 row survived");
        assert_eq!(got.title.as_deref(), Some("a title"));
        assert_eq!(
            got.role, None,
            "a session written before v2 must read as unrecorded, not as some default"
        );

        // The version moved, and opening again is a no-op rather than a re-migration.
        let v: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        drop(s);
        let s = Store::open(&path).unwrap();
        assert!(s.session("s-old").unwrap().is_some(), "reopen is idempotent");

        // And a role written after the migration comes back.
        s.put_session(&SessionRecord {
            id: "s-new".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: Some("planner".into()),
            approvers: vec![],
        })
        .unwrap();
        assert_eq!(
            s.session("s-new").unwrap().unwrap().role.as_deref(),
            Some("planner")
        );
    }

    #[test]
    fn a_v2_store_is_migrated_and_gains_a_todo_table() {
        // Same fixture rule as the v1 test: a real store, one step reversed, and
        // the migration has to put back exactly what `SCHEMA_SQL` would have.
        let path = std::env::temp_dir().join(format!(
            "letibot-migrate-v2-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        struct Clean(std::path::PathBuf);
        impl Drop for Clean {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _clean = Clean(path.clone());

        {
            let s = Store::open(&path).unwrap();
            s.put_session(&SessionRecord {
                id: "s-todo".into(),
                title: None,
                model_id: "m".into(),
                dialect_sha: "sha".into(),
                workspace_root: "/w".into(),
                owner: "dead".into(),
                role: Some("coder".into()),
                approvers: vec![],
            })
            .unwrap();
        }
        {
            // Reverse the v3 step: the table goes away, the version says 2.
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute("DROP TABLE todo", []).unwrap();
            c.execute("UPDATE schema_version SET version = 2", []).unwrap();
        }
        let s = Store::open(&path).unwrap();
        let v: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        // The migrated table works, and the session's rows survived the trip.
        assert!(s.todos("s-todo").unwrap().is_empty());
        s.put_todos(
            "s-todo",
            &[TodoItem {
                content: "ship the pane".into(),
                status: TodoStatus::InProgress,
            }],
        )
        .unwrap();
        assert_eq!(s.todos("s-todo").unwrap().len(), 1);
    }

    #[test]
    fn todos_are_replaced_whole_and_read_back_in_order() {
        let s = store();
        let _seeded = seeded(&s);

        // No list yet: empty, not an error, not a distinction from "cleared".
        assert!(s.todos("sess-1").unwrap().is_empty());

        let first = vec![
            TodoItem {
                content: "read the harness".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                content: "seat the tool".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                content: "render the pane".into(),
                status: TodoStatus::Pending,
            },
        ];
        s.put_todos("sess-1", &first).unwrap();
        assert_eq!(s.todos("sess-1").unwrap(), first, "order survives the store");

        // The second write is the list, not a patch on it: the revision the model
        // made is the only one the store holds.
        let second = vec![TodoItem {
            content: "render the pane".into(),
            status: TodoStatus::InProgress,
        }];
        s.put_todos("sess-1", &second).unwrap();
        assert_eq!(s.todos("sess-1").unwrap(), second);

        // An empty write clears; the row remains and answers empty.
        s.put_todos("sess-1", &[]).unwrap();
        assert!(s.todos("sess-1").unwrap().is_empty());
    }
}
