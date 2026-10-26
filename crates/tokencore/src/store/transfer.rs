//! **One session, out of one store and into another** — the transfer half of
//! `docs/remote-hosts.md` RH4, where a session's whole daemon moves to a remote host and
//! back.
//!
//! # What moves, and what does not
//!
//! A row belongs to a session when the session id is its key. Those tables are listed in
//! [`TABLES`], and the list is the whole of the rule: `session`, its transcripts and their
//! items, the todo list, the job history and the job contexts, the monitors it declared,
//! the mode it was put at, the decisions the gate took in it and the citations its replies
//! landed. **`subagent_run` is deliberately NOT here** even though it names the session:
//! its rows are keyed by the CHILD's id, and a child is a session of its own — carrying
//! the parent's rows across would tell the destination daemon to resume a child whose
//! transcript did not travel, and it would abandon it. A child moves by being migrated by
//! id, like anything else. The queue tables, `project_mode` and `daemon_life` are keyed by
//! a repository, a root or a process, none of which is a session.
//!
//! # Why the rows are moved as ROWS
//!
//! A hand-written struct per table would be a second copy of the schema, and this store's
//! schema has forty-column tables that grow by a column every few weeks — a copy of it
//! that has to be kept in step is a copy that will not be. A row travels as
//! `(column names, values)` read off the statement itself, so a column added tomorrow
//! travels tomorrow. The price is that a destination whose schema does NOT know a column
//! is refused by name rather than silently dropping it, which is the right way round: a
//! migrated session missing a column is a session that behaves differently from the one
//! that was left behind.
//!
//! # Two verifications, and why both
//!
//! [`Store::import_session`] runs [`TokenLedger::verify_chain`] twice: once over the rows
//! **in the file**, before anything is written, and once over the rows **in the
//! destination store**, inside the transaction that writes them. The first refuses a
//! truncated or edited token blob without touching the destination — [`SessionTransfer::digest`]
//! covers the parts the chain cannot see, such as an item's JSON. The second is the same
//! check a resume makes (`letibot_turn::resume`), and it is the one that answers the
//! question the migration is actually asking: *will the daemon on the other side be able
//! to open this conversation?*
//!
//! # The marker
//!
//! A session has one live copy. The copy left behind carries `session.lives_on` — the
//! name of the endpoint that holds it now, `local` or a host from `hosts.toml` — and
//! `session.moved_ms`, so `--sessions`, `--continue` and a head can say where it is
//! instead of offering a stale local copy. The live copy's marker is NULL. The column is
//! set by [`Store::set_lives_on`] and is the ONE thing a migration changes about a row it
//! did not create.

use rusqlite::OptionalExtension;
use rusqlite::types::{Value as SqlValue, ValueRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Result, Store, StoreError, blob_to_tokens};
use crate::ledger::{LedgerRow, TokenLedger};
use crate::vocab::TokenId;

/// What this module writes into [`SessionTransfer::format`], and what it refuses to read.
///
/// A number rather than an assumption, for the reason every version in this store is one:
/// a file written by a later build may carry a table this one would silently drop, and a
/// migration that quietly lost a transcript is worse than one that refused.
pub const TRANSFER_FORMAT: u32 = 1;

/// **The tables a session's own rows are in**, and the query that finds them.
///
/// `(table, where, order)` — the third entry is what makes a transcript's items insert in
/// `seq` order, which the store's own insert trigger requires ([`super::SCHEMA_SQL`]'s
/// `transcript_item_append_only_insert`): rows out of order are refused, so the order here
/// is not a nicety.
///
/// The list is ordered as the inserts must be, because foreign keys are on: a prefix
/// before the transcript that names it, a transcript before its items, the session before
/// everything keyed by it.
const TABLES: &[(&str, &str, &str)] = &[
    (
        "stable_prefix",
        "id IN (SELECT stable_prefix_id FROM transcript WHERE session_id = ?1)",
        "ORDER BY id",
    ),
    ("session", "id = ?1", ""),
    (
        "transcript",
        "session_id = ?1",
        "ORDER BY created_at, rowid",
    ),
    (
        "transcript_item",
        "transcript_id IN (SELECT id FROM transcript WHERE session_id = ?1)",
        "ORDER BY transcript_id, seq",
    ),
    ("todo", "session_id = ?1", ""),
    ("job", "session_id = ?1", "ORDER BY handle"),
    ("job_context", "session_id = ?1", "ORDER BY handle"),
    ("session_monitor", "session_id = ?1", "ORDER BY name"),
    ("session_mode", "session_id = ?1", ""),
    (
        "adjudication",
        "session_id = ?1",
        "ORDER BY decided_ms, request_id",
    ),
    ("citation", "session_id = ?1", "ORDER BY text_hash"),
];

/// One table's rows, as the store holds them.
///
/// `columns` comes from the statement, not from a constant: the file a store writes
/// describes itself, and a reader that meets a column it does not know says so.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TableExport {
    pub table: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl TableExport {
    /// One column of one row, by name. A column this table does not have is `None` — for
    /// the two marker columns a store newer than the file may not carry.
    pub fn cell<'a>(&self, row: &'a [Value], name: &str) -> Option<&'a Value> {
        let at = self.columns.iter().position(|c| c == name)?;
        row.get(at)
    }
}

/// **One session, its transcripts and everything keyed by it** — the file that travels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionTransfer {
    pub format: u32,
    /// When this file was written, so a reader can say how old a copy is.
    pub exported_ms: i64,
    /// The session id, repeated outside the rows: every refusal names it, and a reader
    /// that has only the file should not have to parse a table to find it.
    pub session_id: String,
    pub tables: Vec<TableExport>,
    /// SHA-256 over this struct with the field absent, hex. The chain covers the tokens;
    /// this covers everything the chain cannot see — an item's JSON, a todo list, a mode.
    /// It is an INTEGRITY check and not a signature: it says the file is the file that was
    /// written, and nothing about who wrote it. The transport is ssh to the operator's own
    /// machine, and the file never leaves the two stores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

/// What an import changed, for the sentence the operator reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub session_id: String,
    /// Transcripts carried, and the rows in them.
    pub transcripts: usize,
    pub items: usize,
    /// Todo lists, jobs, monitors, decisions and citations carried.
    pub todos: usize,
    pub jobs: usize,
    pub monitors: usize,
    pub decisions: usize,
    pub citations: usize,
    /// **This store already held the session** and the file CAME IT HOME — the rows before the
    /// file's were already here, and what this import did was carry the rest. A round trip is
    /// two of those, because a move back to a laptop lands in the store the session was last
    /// in: the copy left behind is the prefix of the file, and nothing appended to it while it
    /// was marked.
    pub caught_up: bool,
}

/// Where an import puts the session — the two things a transfer cannot know about the
/// other side of the move.
#[derive(Debug, Clone)]
pub struct ImportInto {
    /// The checkout the session works in THERE. A path from the source machine is a path
    /// that does not exist here, and a session whose workspace did not move opens onto a
    /// folder nobody has.
    pub workspace: String,
}

impl SessionTransfer {
    /// The table of this name, if the file carries it.
    pub fn table(&self, name: &str) -> Option<&TableExport> {
        self.tables.iter().find(|t| t.table == name)
    }

    /// Every row of one table, or an empty slice.
    pub fn rows(&self, name: &str) -> &[Vec<Value>] {
        self.table(name).map(|t| t.rows.as_slice()).unwrap_or(&[])
    }

    /// **The session's own row**, or a refusal — nothing else in the file means anything
    /// without it.
    pub fn session_row(&self) -> Result<(&TableExport, &Vec<Value>)> {
        let t = self.table("session").ok_or_else(|| {
            StoreError::Refused(format!(
                "{}: the file carries no `session` table, so there is no session in it. \
                 Nothing was imported.",
                self.session_id
            ))
        })?;
        let row = t.rows.first().ok_or_else(|| {
            StoreError::Refused(format!(
                "{}: the file's `session` table is empty. Nothing was imported.",
                self.session_id
            ))
        })?;
        Ok((t, row))
    }

    /// **The digest this file SHOULD carry**, recomputed from its contents.
    ///
    /// Serialised with the digest absent, from the deserialised struct — so the bytes
    /// hashed are this build's canonical spelling of the file and not the bytes that
    /// arrived. A file whose field order or whitespace differs still verifies, which is
    /// what makes the check about the CONTENTS; a file whose contents differ does not.
    fn recomputed_digest(&self) -> Result<String> {
        let mut bare = self.clone();
        bare.digest = None;
        let bytes = serde_json::to_vec(&bare)?;
        Ok(hex(&sha256(&bytes)))
    }

    /// Whether the digest is the one the contents hash to.
    pub fn digest_agrees(&self) -> Result<bool> {
        let Some(said) = &self.digest else {
            return Ok(false);
        };
        Ok(*said == self.recomputed_digest()?)
    }
}

impl Store {
    /// **Write one session out** — every row of every table in [`TABLES`], and the digest
    /// over them.
    pub fn export_session(&self, session_id: &str) -> Result<SessionTransfer> {
        if self.session(session_id)?.is_none() {
            return Err(StoreError::NotFound(format!("session {session_id}")));
        }
        let mut tables = Vec::with_capacity(TABLES.len());
        for (table, filter, order) in TABLES {
            let sql = format!("SELECT * FROM {table} WHERE {filter} {order}");
            let mut st = self.conn.prepare(&sql)?;
            let columns: Vec<String> = st.column_names().iter().map(|c| c.to_string()).collect();
            let mut rows = st.query(rusqlite::params![session_id])?;
            let mut out: Vec<Vec<Value>> = Vec::new();
            while let Some(row) = rows.next()? {
                let mut cells = Vec::with_capacity(columns.len());
                for i in 0..columns.len() {
                    cells.push(cell_out(row.get_ref(i)?)?);
                }
                out.push(cells);
            }
            tables.push(TableExport {
                table: (*table).to_string(),
                columns,
                rows: out,
            });
        }
        let mut transfer = SessionTransfer {
            format: TRANSFER_FORMAT,
            exported_ms: super::now_ms(),
            session_id: session_id.to_string(),
            tables,
            digest: None,
        };
        let digest = transfer.recomputed_digest()?;
        transfer.digest = Some(digest);
        Ok(transfer)
    }

    /// **Read one session in**, or change nothing at all.
    ///
    /// Refuses by name, before writing: a file this build does not understand, a file whose
    /// contents are not the ones it was written with, a token chain that does not rebuild,
    /// and a session id the destination already holds. Every one of those is a migration
    /// that would otherwise leave two half-copies of one conversation and no way to tell
    /// which is which.
    pub fn import_session(
        &self,
        transfer: &SessionTransfer,
        into: &ImportInto,
    ) -> Result<ImportReport> {
        let id = transfer.session_id.as_str();
        if transfer.format != TRANSFER_FORMAT {
            return Err(StoreError::Refused(format!(
                "{id}: this file is format {} and this build writes and reads format \
                 {TRANSFER_FORMAT}. Nothing was imported — a newer file may carry rows this \
                 build would silently drop, and a migrated session missing a transcript is \
                 worse than one that did not move.",
                transfer.format
            )));
        }
        if !transfer.digest_agrees()? {
            return Err(StoreError::Refused(format!(
                "{id}: this file is not the one that was written — its digest does not \
                 match its contents. Nothing was imported. Either the file was edited or \
                 truncated on the way here, and a conversation half-carried is worse than \
                 one that did not move."
            )));
        }
        let (session, row) = transfer.session_row()?;
        let carried = match session.cell(row, "id") {
            Some(Value::String(s)) => s.clone(),
            _ => {
                return Err(StoreError::Refused(format!(
                    "{id}: the file's session row carries no id. Nothing was imported."
                )));
            }
        };
        if carried != id {
            return Err(StoreError::Refused(format!(
                "{id}: the file says it carries a session and its own row is `{carried}`. \
                 Nothing was imported."
            )));
        }
        if let Some(existing) = self.session(id)? {
            // **Two copies are only allowed to be two copies when one of them is marked.**
            // The copy here is the one a PREVIOUS move left behind: its rows are a prefix of the
            // file's (nothing could append to it — the marker refuses to open it), so the file
            // catches it up. A copy here that is NOT marked is the state a migration exists to
            // prevent — two live transcripts of one conversation, each going on being appended
            // to — and no reading of the file can tell which of them a person means.
            if existing.lives_on.is_none() {
                return Err(StoreError::Refused(format!(
                    "{id} is already in this store, and it is not marked as living anywhere \
                     else. Nothing was imported: two copies of one conversation that each go on \
                     being appended to is the state a migration exists to prevent. Delete the \
                     copy here first, or migrate this one instead."
                )));
            }
            verify_chains(transfer)?;
            let mut report = self.write_tx(|tx| catch_up(tx, transfer, into))?;
            report.caught_up = true;
            return Ok(report);
        }
        // **The chain, before anything is written.** The same rebuild a resume does: the
        // prefix's tokens, the rows in `seq` order, every link re-hashed.
        verify_chains(transfer)?;

        let report = self.write_tx(|tx| {
            for table in &transfer.tables {
                if table.rows.is_empty() {
                    continue;
                }
                let known = table_columns(tx, &table.table)?;
                for column in &table.columns {
                    if !known.iter().any(|k| k == column) {
                        return Err(StoreError::Refused(format!(
                            "{id}: this store's `{}` table has no `{column}` column, and \
                             the file carries one. Nothing was imported: the destination \
                             build is not the build that wrote the file, and a session \
                             missing a column behaves differently from the one that was \
                             left behind.",
                            table.table
                        )));
                    }
                }
                let insert = format!(
                    "{} INTO {} ({}) VALUES ({})",
                    // **A prefix is content-addressed, so the identical row may already be
                    // here** — a second session on the same prompt inherits one, and a
                    // migration must not fail because the destination already has it.
                    // Everything else is the session's own row or nothing.
                    if table.table == "stable_prefix" {
                        "INSERT OR IGNORE"
                    } else {
                        "INSERT"
                    },
                    table.table,
                    table.columns.join(", "),
                    (1..=table.columns.len())
                        .map(|i| format!("?{i}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let mut st = tx.prepare(&insert)?;
                for row in &table.rows {
                    let mut cells: Vec<SqlValue> = Vec::with_capacity(row.len());
                    for value in row {
                        cells.push(cell_in(value)?);
                    }
                    st.execute(rusqlite::params_from_iter(cells.iter()))?;
                }
            }
            settle(tx, transfer, into)
        })?;
        Ok(report)
    }

    /// **Say where a session lives now** — `None` for the store you are reading (it is
    /// live here), or the endpoint's name and when it moved.
    pub fn lives_on(&self, session_id: &str) -> Result<Option<(String, Option<i64>)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT lives_on, moved_ms FROM session WHERE id = ?1",
                rusqlite::params![session_id],
                |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<i64>>(1)?)),
            )
            .optional()?
            .and_then(|(on, at)| on.map(|on| (on, at))))
    }

    /// **Mark a session as living somewhere else**, or clear the mark when it comes home.
    ///
    /// `None` clears it — the copy in front of you is the live one. The mark is the only
    /// thing a migration writes into the source store, and it writes nothing else: the
    /// rows stay, because a migration that deleted them would have no way back when the
    /// far side turned out to be a host that could not build.
    pub fn set_lives_on(&self, session_id: &str, on: Option<&str>) -> Result<()> {
        let n = self.exec(
            "UPDATE session SET lives_on = ?2, moved_ms = ?3 WHERE id = ?1",
            rusqlite::params![session_id, on, on.map(|_| super::now_ms()),],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("session {session_id}")));
        }
        Ok(())
    }

    /// Every session marked as living somewhere else, by id — what a destination must know
    /// before it takes a copy back, and what `--sessions` reads to say where a row went.
    pub fn migrated_sessions(&self) -> Result<Vec<(String, String, Option<i64>)>> {
        let mut st = self.conn.prepare(
            "SELECT id, lives_on, moved_ms FROM session WHERE lives_on IS NOT NULL ORDER BY id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

/// **The workspace is the destination's, and the marker is the destination's** — then the chain
/// is rebuilt again over what this store now holds, and the report is counted.
///
/// The two destination facts cannot come from the file: it was written before anybody knew where
/// the copy would land. A session that arrived carrying the source's workspace would open onto a
/// folder that is not here, and one carrying the source's marker would say it lives somewhere
/// other than where it now does.
///
/// **The second chain check is inside the transaction**, so a store that cannot rebuild the
/// conversation rolls back to the state it was in rather than keeping rows nothing can open. It
/// is the same check a resume makes, and it is the one that answers the question the migration
/// is actually asking: *will the daemon here be able to open this?*
fn settle(
    tx: &rusqlite::Transaction<'_>,
    transfer: &SessionTransfer,
    into: &ImportInto,
) -> Result<ImportReport> {
    let id = transfer.session_id.as_str();
    tx.execute(
        "UPDATE session SET workspace_root = ?2, lives_on = NULL, moved_ms = NULL WHERE id = ?1",
        rusqlite::params![id, into.workspace],
    )?;
    let mut transcripts = 0usize;
    let mut items = 0usize;
    for (tid, prefix_id) in transcripts_of(tx, id)? {
        let loaded = read_transcript(tx, &tid, &prefix_id)?;
        loaded.verify(&format!(
            "{id}: after the rows were written, this store's copy of transcript {tid} does not \
             rebuild the chain the file describes. The import was rolled back — nothing of this \
             session is in this store."
        ))?;
        transcripts += 1;
        items += loaded.rows.len();
    }
    Ok(ImportReport {
        session_id: id.to_string(),
        transcripts,
        items,
        todos: count(tx, "todo", id)?,
        jobs: count(tx, "job", id)?,
        monitors: count(tx, "session_monitor", id)?,
        decisions: count(tx, "adjudication", id)?,
        citations: count(tx, "citation", id)?,
        caught_up: false,
    })
}

/// **A copy this store already has, brought up to what the file says.**
///
/// The case a round trip lands in: the rows left behind here are the PREFIX of the file's, because
/// the marker refused to open them and nothing appended to them. So the missing rows are appended
/// (the store's own trigger would refuse anything else), the metadata tables are replaced, and the
/// overlap is CHECKED rather than assumed: the last row of the copy here must be the same row, by
/// its chain link, as the file's row at that index. If it is not, the two copies diverged — which
/// means something appended here while the marker said not to — and that is refused by name.
fn catch_up(
    tx: &rusqlite::Transaction<'_>,
    transfer: &SessionTransfer,
    into: &ImportInto,
) -> Result<ImportReport> {
    let id = transfer.session_id.as_str();
    let (session, row) = transfer.session_row()?;
    let known = table_columns(tx, "session")?;
    // Every column the file carries, except the two that name the row itself.
    let mut sets: Vec<String> = Vec::new();
    let mut values: Vec<SqlValue> = Vec::new();
    for (i, column) in session.columns.iter().enumerate() {
        if column == "id" || !known.iter().any(|k| k == column) {
            continue;
        }
        let cell = row
            .get(i)
            .ok_or_else(|| StoreError::Corrupt(format!("session row is short of `{column}`")))?;
        values.push(cell_in(cell)?);
        sets.push(format!("{column} = ?{}", values.len()));
    }
    values.push(SqlValue::Text(id.to_string()));
    tx.execute(
        &format!(
            "UPDATE session SET {} WHERE id = ?{}",
            sets.join(", "),
            values.len()
        ),
        rusqlite::params_from_iter(values.iter()),
    )?;

    // A prefix is content-addressed and may already be here.
    for table in transfer
        .tables
        .iter()
        .filter(|t| t.table == "stable_prefix")
    {
        insert_rows(tx, table, "INSERT OR IGNORE")?;
    }
    // A transcript the copy here does not have (a fork made on the other machine) arrives whole;
    // one it does have keeps its row, which is the same row.
    for table in transfer.tables.iter().filter(|t| t.table == "transcript") {
        insert_rows(tx, table, "INSERT OR IGNORE")?;
    }
    if let Some(items) = transfer.table("transcript_item") {
        // **A copy with rows the file does not have is a divergence, not a superset.** The whole
        // claim this function rests on is that nothing appended here while the copy was marked;
        // a row here that the live copy never saw says that claim is false, and appending the
        // file's rows on top of it would build one conversation out of two.
        for (tid, _) in transcripts_of(tx, id)? {
            let have: i64 = tx.query_row(
                "SELECT COUNT(*) FROM transcript_item WHERE transcript_id = ?1",
                rusqlite::params![tid],
                |r| r.get(0),
            )?;
            let said = items
                .rows
                .iter()
                .filter(|r| items.cell(r, "transcript_id") == Some(&Value::String(tid.clone())))
                .count();
            if have as usize > said {
                return Err(StoreError::Refused(format!(
                    "{id}: this store's copy of transcript {tid} holds {have} row(s) and the \
                     file holds {said} — the copy here was APPENDED TO while it was marked as \
                     living elsewhere. Nothing was imported: two transcripts of one conversation \
                     cannot be told apart from one transcript that was worked on twice."
                )));
            }
        }
        for row in &items.rows {
            let tid = string_cell(items, row, "transcript_id")?;
            let seq = number_cell(items, row, "seq")?;
            let have: i64 = tx.query_row(
                "SELECT COUNT(*) FROM transcript_item WHERE transcript_id = ?1",
                rusqlite::params![tid],
                |r| r.get(0),
            )?;
            if seq < have {
                // Inside the overlap: it must be the SAME row the file describes, by its link.
                let said = hash_cell(items, row, "h_k")?;
                let mine: Vec<u8> = tx.query_row(
                    "SELECT h_k FROM transcript_item WHERE transcript_id = ?1 AND seq = ?2",
                    rusqlite::params![tid, seq],
                    |r| r.get(0),
                )?;
                if mine != said {
                    return Err(StoreError::Refused(format!(
                        "{id}: transcript {tid} has {have} row(s) in this store and the file's \
                         row {seq} is not the same row — the two copies have DIVERGED, which \
                         means something appended here while this copy was marked as living \
                         elsewhere. Nothing was imported: a merge of two conversations cannot \
                         be told apart from a repair of one."
                    )));
                }
                continue;
            }
            insert_rows(
                tx,
                &TableExport {
                    table: "transcript_item".into(),
                    columns: items.columns.clone(),
                    rows: vec![row.clone()],
                },
                "INSERT",
            )?;
        }
    }
    // **The mutable and keyed tables are the file's**, not this copy's: the live copy is the
    // authority for a todo list, a job's state, a monitor, a mode, a ruling and a citation.
    for table in transfer.tables.iter().filter(|t| {
        !matches!(
            t.table.as_str(),
            "session" | "stable_prefix" | "transcript" | "transcript_item"
        )
    }) {
        insert_rows(tx, table, "INSERT OR REPLACE")?;
    }
    settle(tx, transfer, into)
}

fn insert_rows(tx: &rusqlite::Transaction<'_>, table: &TableExport, verb: &str) -> Result<()> {
    if table.rows.is_empty() {
        return Ok(());
    }
    let known = table_columns(tx, &table.table)?;
    for column in &table.columns {
        if !known.iter().any(|k| k == column) {
            return Err(StoreError::Refused(format!(
                "this store's `{}` table has no `{column}` column, and the file carries one. \
                 Nothing was imported: the destination build is not the build that wrote the \
                 file, and a session missing a column behaves differently from the one that was \
                 left behind.",
                table.table
            )));
        }
    }
    let sql = format!(
        "{verb} INTO {} ({}) VALUES ({})",
        table.table,
        table.columns.join(", "),
        (1..=table.columns.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut st = tx.prepare(&sql)?;
    for row in &table.rows {
        let mut cells: Vec<SqlValue> = Vec::with_capacity(row.len());
        for value in row {
            cells.push(cell_in(value)?);
        }
        st.execute(rusqlite::params_from_iter(cells.iter()))?;
    }
    Ok(())
}

/// One cell out of the store, as JSON. **A blob is the only thing that is an object**, so
/// a text column that happens to hold JSON is still a string and the two cannot be
/// confused on the way back.
fn cell_out(value: ValueRef<'_>) -> Result<Value> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::Object(
            [("blob".to_string(), Value::from(hex(b)))]
                .into_iter()
                .collect(),
        ),
    })
}

/// One cell back into the store.
fn cell_in(value: &Value) -> Result<SqlValue> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(*b as i64),
        Value::Number(n) => match n.as_i64() {
            Some(i) => SqlValue::Integer(i),
            None => SqlValue::Real(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => SqlValue::Text(s.clone()),
        Value::Object(o) => match o.get("blob").and_then(|b| b.as_str()) {
            Some(hexed) => SqlValue::Blob(unhex(hexed)?),
            None => {
                return Err(StoreError::Refused(
                    "a cell in the file is an object that is not a blob. Nothing was \
                     imported."
                        .into(),
                ));
            }
        },
        Value::Array(_) => {
            return Err(StoreError::Refused(
                "a cell in the file is an array, which no column of this store holds. \
                 Nothing was imported."
                    .into(),
            ));
        }
    })
}

fn table_columns(tx: &rusqlite::Transaction<'_>, table: &str) -> Result<Vec<String>> {
    let mut st = tx.prepare(&format!("PRAGMA table_info('{table}')"))?;
    let rows = st.query_map([], |r| r.get::<_, String>(1))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn count(tx: &rusqlite::Transaction<'_>, table: &str, session_id: &str) -> Result<usize> {
    let n: i64 = tx.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
        rusqlite::params![session_id],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// The session's transcripts, parent-first, with the prefix each hangs off.
fn transcripts_of(
    tx: &rusqlite::Transaction<'_>,
    session_id: &str,
) -> Result<Vec<(String, String)>> {
    let mut st = tx.prepare(
        "SELECT id, stable_prefix_id FROM transcript WHERE session_id = ?1
          ORDER BY created_at, rowid",
    )?;
    let rows = st.query_map(rusqlite::params![session_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// One transcript's rows, ready to be re-chained.
struct ReadBack {
    transcript_id: String,
    prefix_tokens: Vec<TokenId>,
    h_init: [u8; 32],
    rows: Vec<(LedgerRow, Vec<TokenId>)>,
}

impl ReadBack {
    /// [`TokenLedger::restore`] and then [`TokenLedger::verify_chain`] — the two passes
    /// `letibot_turn::resume` runs, for the reason its header gives: the first checks the
    /// rows against the blobs it was handed, the second checks the region the session will
    /// actually submit.
    fn verify(&self, why: &str) -> Result<()> {
        if self.rows.is_empty() {
            // A transcript with no rows has no chain to break. Its prefix still has to be
            // there, which `read_transcript` has already insisted on.
            return Ok(());
        }
        let ledger = TokenLedger::restore(
            &self.transcript_id,
            &self.prefix_tokens,
            self.h_init,
            &self.rows,
        )
        .map_err(|e| StoreError::Refused(format!("{why}: {e}")))?;
        ledger
            .verify_chain()
            .map_err(|e| StoreError::Refused(format!("{why}: {e}")))?;
        Ok(())
    }
}

fn read_transcript(
    tx: &rusqlite::Transaction<'_>,
    transcript_id: &str,
    prefix_id: &str,
) -> Result<ReadBack> {
    let (blob, h_init): (Vec<u8>, Vec<u8>) = tx
        .query_row(
            "SELECT tokens, h_init FROM stable_prefix WHERE id = ?1",
            rusqlite::params![prefix_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::Refused(format!(
                "{transcript_id} names the stable prefix {prefix_id}, which this store does \
                 not hold. The import was rolled back — a transcript whose prefix is absent \
                 cannot be resumed."
            ))
        })?;
    let mut st = tx.prepare(
        "SELECT item_id, tok_offset, tok_len, h_k, tokens FROM transcript_item
          WHERE transcript_id = ?1 ORDER BY seq",
    )?;
    let mut rows = st.query(rusqlite::params![transcript_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let item_id: String = row.get(0)?;
        let tok_offset: i64 = row.get(1)?;
        let tok_len: i64 = row.get(2)?;
        let h_k: Vec<u8> = row.get(3)?;
        let tokens: Vec<u8> = row.get(4)?;
        if h_k.len() != 32 {
            return Err(StoreError::Corrupt(format!(
                "{transcript_id}: row `{item_id}` has a {}-byte h_k, and a chain link is 32 \
                 bytes",
                h_k.len()
            )));
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&h_k);
        out.push((
            LedgerRow {
                item_id,
                tok_offset: tok_offset as u32,
                tok_len: tok_len as u32,
                h_k: hash,
            },
            blob_to_tokens(&tokens)?,
        ));
    }
    Ok(ReadBack {
        transcript_id: transcript_id.to_string(),
        prefix_tokens: blob_to_tokens(&blob)?,
        h_init: {
            if h_init.len() != 32 {
                return Err(StoreError::Corrupt(format!(
                    "stable_prefix {prefix_id}: a {}-byte h_init, and it is 32",
                    h_init.len()
                )));
            }
            let mut h = [0u8; 32];
            h.copy_from_slice(&h_init);
            h
        },
        rows: out,
    })
}

/// **The chain of every transcript in the file, rebuilt from the file alone.**
///
/// This is the check that runs before a single row is written, and it is the same one a
/// resume makes — so a file that arrives intact but describes a conversation no daemon
/// could open is refused at the door rather than stored.
fn verify_chains(transfer: &SessionTransfer) -> Result<()> {
    let Some(prefixes) = transfer.table("stable_prefix") else {
        return Ok(());
    };
    let Some(items) = transfer.table("transcript_item") else {
        return Ok(());
    };
    for transcript in transfer.rows("transcript") {
        let t = transfer.table("transcript").expect("asked for by name");
        let tid = string_cell(t, transcript, "id")?;
        let prefix_id = string_cell(t, transcript, "stable_prefix_id")?;
        let Some(prefix) = prefixes
            .rows
            .iter()
            .find(|r| prefixes.cell(r, "id") == Some(&Value::String(prefix_id.clone())))
        else {
            return Err(StoreError::Refused(format!(
                "{tid} names the stable prefix {prefix_id}, which the file does not carry. \
                 Nothing was imported."
            )));
        };
        let prefix_tokens = token_cell(prefixes, prefix, "tokens")?;
        let h_init = hash_cell(prefixes, prefix, "h_init")?;
        let mut rows: Vec<(LedgerRow, Vec<TokenId>)> = Vec::new();
        for row in &items.rows {
            if items.cell(row, "transcript_id") != Some(&Value::String(tid.clone())) {
                continue;
            }
            let tokens = token_cell(items, row, "tokens")?;
            let tok_len = number_cell(items, row, "tok_len")? as usize;
            if tok_len != tokens.len() {
                return Err(StoreError::Refused(format!(
                    "{tid}: row `{}` says it holds {tok_len} tokens and its blob holds {}. \
                     Nothing was imported.",
                    string_cell(items, row, "item_id")?,
                    tokens.len()
                )));
            }
            rows.push((
                LedgerRow {
                    item_id: string_cell(items, row, "item_id")?,
                    tok_offset: number_cell(items, row, "tok_offset")? as u32,
                    tok_len: tok_len as u32,
                    h_k: hash_cell(items, row, "h_k")?,
                },
                tokens,
            ));
        }
        ReadBack {
            transcript_id: tid.clone(),
            prefix_tokens,
            h_init,
            rows,
        }
        .verify(&format!(
            "{tid} does not rebuild the hash chain it was written with, so no daemon could \
             open it. Nothing was imported"
        ))?;
    }
    Ok(())
}

fn string_cell(t: &TableExport, row: &[Value], name: &str) -> Result<String> {
    match t.cell(row, name) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(StoreError::Corrupt(format!(
            "table `{}`: no string `{name}` in this row",
            t.table
        ))),
    }
}

fn number_cell(t: &TableExport, row: &[Value], name: &str) -> Result<i64> {
    match t.cell(row, name) {
        Some(Value::Number(n)) => Ok(n.as_i64().unwrap_or(0)),
        _ => Err(StoreError::Corrupt(format!(
            "table `{}`: no number `{name}` in this row",
            t.table
        ))),
    }
}

fn token_cell(t: &TableExport, row: &[Value], name: &str) -> Result<Vec<TokenId>> {
    match t.cell(row, name) {
        Some(v) => blob_to_tokens(&blob_cell(v)?),
        None => Err(StoreError::Corrupt(format!(
            "table `{}`: no `{name}` in this row",
            t.table
        ))),
    }
}

fn hash_cell(t: &TableExport, row: &[Value], name: &str) -> Result<[u8; 32]> {
    let bytes = match t.cell(row, name) {
        Some(v) => blob_cell(v)?,
        None => {
            return Err(StoreError::Corrupt(format!(
                "table `{}`: no `{name}` in this row",
                t.table
            )));
        }
    };
    if bytes.len() != 32 {
        return Err(StoreError::Corrupt(format!(
            "table `{}`: `{name}` is {} bytes, and a chain hash is 32",
            t.table,
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn blob_cell(value: &Value) -> Result<Vec<u8>> {
    match value {
        Value::Object(o) => match o.get("blob").and_then(|b| b.as_str()) {
            Some(h) => unhex(h),
            None => Err(StoreError::Corrupt("an object that is not a blob".into())),
        },
        _ => Err(StoreError::Corrupt("expected a blob".into())),
    }
}

/// Lowercase hex. Written here rather than pulled in: the only thing this tree needs it
/// for is a transfer file, and a dependency added for sixteen lines is a dependency the
/// whole workspace carries for ever.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn unhex(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return Err(StoreError::Corrupt(format!(
            "a hex blob of odd length ({})",
            text.len()
        )));
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16);
        let lo = (pair[1] as char).to_digit(16);
        match (hi, lo) {
            (Some(hi), Some(lo)) => out.push((hi * 16 + lo) as u8),
            _ => {
                return Err(StoreError::Corrupt(
                    "a hex blob with a character that is not a hex digit".into(),
                ));
            }
        }
    }
    Ok(out)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&out);
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::hash_tokens;
    use crate::store::{JobRecord, NewAdjudication, SessionRecord, StablePrefixRecord, TodoItem};
    use crate::store::{TodoBy, TodoStatus};
    use letibot_transcript::{ReasoningField, SystemOrigin, TranscriptItem};

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

    /// **A session with something in every table a transfer carries.** Three rows of
    /// history, a todo list, a job, a monitor, a mode, one decision and one citation — so
    /// a table the export forgot is a table the round trip below fails on rather than one
    /// nobody looked at.
    fn seeded(s: &Store) -> String {
        let p = prefix();
        let prefix_id = s.put_stable_prefix(&p).unwrap();
        s.put_session(&SessionRecord {
            id: "sess-1".into(),
            title: Some("a conversation".into()),
            model_id: "qwen".into(),
            dialect_sha: p.dialect_sha.clone(),
            workspace_root: "/w".into(),
            owner: "deadtrickster".into(),
            role: Some("coder".into()),
            approvers: vec!["deadtrickster".into()],
            parent_session_id: None,
        })
        .unwrap();
        s.put_transcript("tr-1", "sess-1", &prefix_id).unwrap();
        let mut ledger = TokenLedger::new("tr-1", &[1, 2, 3, 4]).unwrap();
        let items: Vec<(TranscriptItem, Vec<TokenId>)> = vec![
            (
                TranscriptItem::System {
                    text: "boot".into(),
                    origin: SystemOrigin::Bootstrap,
                },
                vec![20, 21],
            ),
            (
                TranscriptItem::User {
                    speaker: Default::default(),
                    parts: vec![letibot_transcript::UserPart::Text {
                        text: "hello".into(),
                    }],
                },
                vec![22],
            ),
            (
                TranscriptItem::Reasoning {
                    text: "think".into(),
                    field: ReasoningField::Inline,
                    truncated: false,
                },
                vec![23, 24, 25],
            ),
        ];
        for (seq, (item, toks)) in items.iter().enumerate() {
            let row = ledger.append(&format!("it-{seq}"), toks).unwrap().clone();
            s.append_item("tr-1", seq as u32, item, &row, toks).unwrap();
        }
        s.put_todos(
            "sess-1",
            &[TodoItem {
                content: "carry the session".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            }],
        )
        .unwrap();
        s.put_job(
            "sess-1",
            &JobRecord {
                handle: "j1".into(),
                command: "cargo test".into(),
                how: "you asked for this".into(),
                state: "exited 0".into(),
                produced: 42,
                elapsed_ms: 900,
                redirect: None,
            },
        )
        .unwrap();
        s.put_monitor("sess-1", "ci", r#"{"every_s":30}"#, "instance-a", 60_000)
            .unwrap();
        s.set_session_mode("sess-1", "writes-allowed", None)
            .unwrap();
        s.record_adjudication(&NewAdjudication {
            request_id: "req-1".into(),
            session_id: "sess-1".into(),
            turn_id: "tr-1#1".into(),
            action: "bash git status".into(),
            baseline: "writes-allowed".into(),
            tier: "ask".into(),
            trail_json: "[]".into(),
            tool: "bash".into(),
            arguments_json: r#"{"command":"git status"}"#.into(),
            mode: "always-ask".into(),
            effect: "read".into(),
            asked: true,
            ..Default::default()
        })
        .unwrap();
        s.put_citations("sess-1", "abc", r#"{"lines":[]}"#, 7)
            .unwrap();
        prefix_id
    }

    fn into() -> ImportInto {
        ImportInto {
            workspace: "/remote/letibot/sess".into(),
        }
    }

    /// **The round trip.** Everything the export carries arrives, and the arrival rebuilds
    /// the same conversation: the same items, the same chain head, the same todo list, job,
    /// monitor and mode. The two tables whose absence would be silent — the items and the
    /// prefix — are the ones the chain check would have refused first.
    #[test]
    fn a_session_round_trips_through_a_file_and_the_chain_verifies() {
        let from = Store::open_in_memory().unwrap();
        seeded(&from);
        let transfer = from.export_session("sess-1").unwrap();
        assert_eq!(transfer.format, TRANSFER_FORMAT);
        assert!(transfer.digest_agrees().unwrap());

        let to = Store::open_in_memory().unwrap();
        let report = to.import_session(&transfer, &into()).unwrap();
        assert_eq!(report.transcripts, 1);
        assert_eq!(report.items, 3);
        assert_eq!(report.todos, 1);
        assert_eq!(report.jobs, 1);
        assert_eq!(report.monitors, 1);
        assert_eq!(report.decisions, 1);
        assert_eq!(report.citations, 1);

        let loaded = to.load_transcript("tr-1").unwrap();
        assert_eq!(loaded.items.len(), 3);
        let ledger = TokenLedger::restore(
            "tr-1",
            &loaded.prefix_tokens,
            loaded.h_init,
            &loaded.ledger_input(),
        )
        .unwrap();
        ledger.verify_chain().unwrap();

        let arrived = to.session("sess-1").unwrap().expect("the row moved");
        assert_eq!(
            arrived.workspace_root, "/remote/letibot/sess",
            "the workspace is the destination's, not the source's"
        );
        assert_eq!(arrived.title.as_deref(), Some("a conversation"));
        assert_eq!(arrived.role.as_deref(), Some("coder"));
        assert_eq!(arrived.lives_on, None, "the arriving copy is the live one");
        assert_eq!(to.todos("sess-1").unwrap().len(), 1);
        assert_eq!(to.jobs("sess-1").unwrap().len(), 1);
        assert_eq!(to.monitors_of("sess-1").unwrap().len(), 1);
        assert_eq!(
            to.session_mode("sess-1").unwrap().map(|(m, _)| m),
            Some("writes-allowed".into())
        );
    }

    /// **A file that was edited is refused, and nothing is written.** Both halves matter:
    /// the digest catches a changed `item_json`, which the chain cannot see, and the chain
    /// catches a changed token blob, which the digest would catch too but whose refusal
    /// names the row.
    #[test]
    fn a_tampered_file_is_refused_and_the_destination_is_unchanged() {
        let from = Store::open_in_memory().unwrap();
        seeded(&from);
        let good = from.export_session("sess-1").unwrap();

        // 1. An item's JSON, edited — the digest is the only one that can see this.
        let mut edited = good.clone();
        let items = edited
            .tables
            .iter_mut()
            .find(|t| t.table == "transcript_item")
            .unwrap();
        let at = items.columns.iter().position(|c| c == "item_json").unwrap();
        items.rows[0][at] = Value::String(r#"{"kind":"system","text":"edited"}"#.into());
        let to = Store::open_in_memory().unwrap();
        let why = to.import_session(&edited, &into()).unwrap_err().to_string();
        assert!(
            why.contains("digest does not match its contents"),
            "a file that is not the one written must be named as such: {why}"
        );
        assert!(
            to.session("sess-1").unwrap().is_none(),
            "nothing may be written when the file is refused"
        );

        // 2. The digest recomputed over the edit — so the tamper is inside the CHAIN now,
        // and the chain check is what refuses it. This is the arm a corrupt-but-consistent
        // file reaches.
        let mut rewired = good.clone();
        let items = rewired
            .tables
            .iter_mut()
            .find(|t| t.table == "transcript_item")
            .unwrap();
        let at = items.columns.iter().position(|c| c == "tokens").unwrap();
        // One token changed, and the lengths still agree — exactly what a bit flip in the
        // blob looks like.
        let value = items.rows[1][at].clone();
        let hexed = blob_cell(&value).unwrap();
        let mut tokens = blob_to_tokens(&hexed).unwrap();
        tokens[0] += 1;
        items.rows[1][at] = Value::Object(
            [(
                "blob".to_string(),
                Value::from(hex(&crate::store::tokens_to_blob(&tokens))),
            )]
            .into_iter()
            .collect(),
        );
        rewired.digest = Some(rewired.recomputed_digest().unwrap());
        let why = to
            .import_session(&rewired, &into())
            .unwrap_err()
            .to_string();
        assert!(
            why.contains("hash chain") || why.contains("chain"),
            "a token blob that no longer matches its link must be refused as a broken \
             chain: {why}"
        );
        assert!(to.session("sess-1").unwrap().is_none());
    }

    /// **A store that already holds the session refuses the second copy.** Two rows for one
    /// conversation, each going on being appended to, is the state a migration exists to
    /// prevent.
    #[test]
    fn a_second_copy_of_one_session_is_refused() {
        let from = Store::open_in_memory().unwrap();
        seeded(&from);
        let transfer = from.export_session("sess-1").unwrap();
        let to = Store::open_in_memory().unwrap();
        to.import_session(&transfer, &into()).unwrap();
        let why = to
            .import_session(&transfer, &into())
            .unwrap_err()
            .to_string();
        assert!(why.contains("already in this store"), "{why}");
    }

    /// **The round trip: a copy that was left behind is brought up to what the live one says.**
    ///
    /// A move back lands in the store the session was last in, so the second import finds a
    /// copy already there. The rows that copy holds are the PREFIX of the file's — the marker
    /// refused to open it, so nothing appended — and what the import does is append the rest.
    /// The overlap is checked by its chain link, and the marker is cleared.
    #[test]
    fn a_marked_copy_is_caught_up_rather_than_refused() {
        let here = Store::open_in_memory().unwrap();
        seeded(&here);
        // The session leaves: the copy here is marked, and the live one is on a host.
        here.set_lives_on("sess-1", Some("lab2x1")).unwrap();
        let gone = here.export_session("sess-1").unwrap();
        let there = Store::open_in_memory().unwrap();
        assert!(there.import_session(&gone, &into()).unwrap().transcripts == 1);

        // Work happens on the host: two more rows, the chain continuing.
        let loaded = there.load_transcript("tr-1").unwrap();
        let mut ledger = TokenLedger::restore(
            "tr-1",
            &loaded.prefix_tokens,
            loaded.h_init,
            &loaded.ledger_input(),
        )
        .unwrap();
        for (seq, text) in [(3u32, "host"), (4, "work")] {
            let toks = vec![100 + seq];
            let row = ledger.append(&format!("it-{seq}"), &toks).unwrap().clone();
            there
                .append_item(
                    "tr-1",
                    seq,
                    &TranscriptItem::User {
                        speaker: Default::default(),
                        parts: vec![letibot_transcript::UserPart::Text {
                            text: text.to_string(),
                        }],
                    },
                    &row,
                    &toks,
                )
                .unwrap();
        }
        let back = there.export_session("sess-1").unwrap();

        let report = here.import_session(&back, &into()).unwrap();
        assert!(
            report.caught_up,
            "the copy here was marked, so this import carries the rest rather than refusing"
        );
        assert_eq!(
            here.session("sess-1").unwrap().unwrap().items,
            5,
            "the two rows written on the host came home"
        );
        assert_eq!(
            here.lives_on("sess-1").unwrap(),
            None,
            "and the copy here is the live one again"
        );
        let loaded = here.load_transcript("tr-1").unwrap();
        let ledger = TokenLedger::restore(
            "tr-1",
            &loaded.prefix_tokens,
            loaded.h_init,
            &loaded.ledger_input(),
        )
        .unwrap();
        ledger.verify_chain().unwrap();
    }

    /// **A copy that appended while it was marked is refused, not merged.** Two conversations
    /// under one id is not something an import can be asked to reconcile.
    #[test]
    fn a_copy_that_diverged_is_refused() {
        let here = Store::open_in_memory().unwrap();
        let prefix_id = seeded(&here);
        here.set_lives_on("sess-1", Some("lab2x1")).unwrap();
        let gone = here.export_session("sess-1").unwrap();
        let there = Store::open_in_memory().unwrap();
        there.import_session(&gone, &into()).unwrap();

        // The stale copy gets a row of its own — the thing the marker exists to prevent, done on
        // purpose here to prove it is caught.
        let loaded = here.load_transcript("tr-1").unwrap();
        let mut ledger = TokenLedger::restore(
            "tr-1",
            &loaded.prefix_tokens,
            loaded.h_init,
            &loaded.ledger_input(),
        )
        .unwrap();
        let toks = vec![7u32, 8];
        let row = ledger.append("it-3", &toks).unwrap().clone();
        here.append_item(
            "tr-1",
            3,
            &TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![letibot_transcript::UserPart::Text {
                    text: "stale".into(),
                }],
            },
            &row,
            &toks,
        )
        .unwrap();
        let _ = prefix_id;

        let why = here
            .import_session(&there.export_session("sess-1").unwrap(), &into())
            .unwrap_err()
            .to_string();
        assert!(
            why.contains("APPENDED TO"),
            "a stale copy that was appended to must be refused by name: {why}"
        );
    }

    /// **The marker is one column and it round trips** — `--sessions` reads it, and a
    /// session that came home is a session whose marker is cleared rather than a row that
    /// was rewritten.
    #[test]
    fn the_marker_says_where_a_copy_lives_and_clears_when_it_comes_home() {
        let s = Store::open_in_memory().unwrap();
        seeded(&s);
        assert_eq!(
            s.lives_on("sess-1").unwrap(),
            None,
            "live here to begin with"
        );
        s.set_lives_on("sess-1", Some("lab2x1")).unwrap();
        let (on, at) = s.lives_on("sess-1").unwrap().expect("marked");
        assert_eq!(on, "lab2x1");
        assert!(at.is_some(), "the day it moved is recorded with where");
        assert_eq!(
            s.session("sess-1").unwrap().unwrap().lives_on.as_deref(),
            Some("lab2x1"),
            "the listing's row carries it, because --sessions prints it"
        );
        assert_eq!(s.migrated_sessions().unwrap().len(), 1);
        s.set_lives_on("sess-1", None).unwrap();
        assert_eq!(s.lives_on("sess-1").unwrap(), None);
        assert!(s.migrated_sessions().unwrap().is_empty());
    }

    /// An empty session — a row a daemon wrote at startup and nobody prompted into — moves
    /// too, and its transcript has no chain to break.
    #[test]
    fn a_session_with_no_rows_moves_and_its_empty_transcript_is_fine() {
        let from = Store::open_in_memory().unwrap();
        let p = prefix();
        let prefix_id = from.put_stable_prefix(&p).unwrap();
        from.put_session(&SessionRecord {
            id: "sess-empty".into(),
            title: None,
            model_id: "qwen".into(),
            dialect_sha: p.dialect_sha.clone(),
            workspace_root: "/w".into(),
            owner: "o".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
        from.put_transcript("tr-empty", "sess-empty", &prefix_id)
            .unwrap();
        let transfer = from.export_session("sess-empty").unwrap();
        let to = Store::open_in_memory().unwrap();
        let report = to.import_session(&transfer, &into()).unwrap();
        assert_eq!(report.transcripts, 1);
        assert_eq!(report.items, 0);
    }
}
