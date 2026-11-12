//! The migration from one `sessions.db` to the split store (`docs/store-split.md`): `shared.db`
//! and `projects/<key>.db` out, `sessions.db.pre-split` left behind.
//!
//! Two doors: [`migrate`] is the whole file in one offline pass, and [`carve`] takes one project key
//! out at a time so every other project's daemon keeps running (§ Carving one project). [`finish`]
//! is the last step of the second road, which renames the file every daemon has written since R92
//! once nothing is left in it.
//!
//! The project key comes from a session's ROOT, never from its own `workspace_root`: a child's is
//! the worktree it works in, and hashing that is what gives 570 folders where the roots give 29.
//! Every check of step 4 is a refusal - a pass that does not add up renames nothing, so the old
//! file keeps its name and a daemon still finds a whole store.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use sha2::{Digest, Sha256};

use crate::ledger::{TokenLedger, hex};
use crate::store::split::{self, StoreFile};
use crate::store::{Result, SCHEMA_VERSION, Store, StoreError, pragmas};

/// What the old file is called once a pass has verified: kept until the operator deletes it
/// (§ Migration, step 5), because it is the only copy of everything until they are sure.
pub const KEPT_SUFFIX: &str = ".pre-split";

/// The key a `workspace_root` hashes to: the first twelve hex characters of its SHA-256 over the
/// path with nothing appended - the key the folder's daemon socket and log already wear
/// (`crates/bridge/src/daemon.rs::key`). Over the RECORDED string, so a folder that no longer
/// exists still has the key its sessions were written under.
pub fn project_key(workspace_root: &str) -> String {
    let mut h = Sha256::new();
    h.update(workspace_root.as_bytes());
    let digest = h.finalize();
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// One project file this pass wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFile {
    pub key: String,
    pub path: PathBuf,
    /// How many sessions the file's rows belong to.
    pub sessions: usize,
}

/// What a pass did, for the command that prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub projects: Vec<ProjectFile>,
    pub sessions: usize,
    /// The old file, under its new name.
    pub kept: PathBuf,
    /// The v24 archive tables carried into `shared.db` because they held rows.
    pub carried: Vec<String>,
    /// Transcripts that did not replay in the old store either, copied as they were stored: a
    /// resume repairs the shapes it knows, and the split is not where history is rewritten.
    pub unreplayed: Vec<String>,
}

/// What one carve did, for the command that prints it (§ Carving one project).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carved {
    pub key: String,
    pub path: PathBuf,
    pub sessions: usize,
    /// Transcripts that did not replay in the old store either, copied as they were stored.
    pub unreplayed: Vec<String>,
    /// The keys still holding project rows in `sessions.db`, so the command can say what is left.
    pub uncarved: Vec<String>,
}

/// What `--finish` did: the renamed file, and the keys it took with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub shared: PathBuf,
    pub carved: Vec<String>,
}

/// One daemon's claim on a session's transcript, as the `session_writer` rows put it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub session: String,
    pub owner: String,
    pub host: String,
    pub pid: u32,
}

/// The claims a store holds, whoever took them: the lease this pass must not run under.
pub fn writer_claims(store: &Store) -> Result<Vec<Claim>> {
    let conn = store.connection();
    let mut st = conn.prepare(
        "SELECT session_id, owner, host, pid FROM session_writer ORDER BY session_id, owner",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Claim {
            session: r.get(0)?,
            owner: r.get(1)?,
            host: r.get(2)?,
            pid: r.get::<_, i64>(3)?.max(0) as u32,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// The claims a daemon could still be writing under: this host's pids that are running, and ANY
/// claim naming another host, which is one this box cannot ask about pids for and so must not
/// split under. Stale rows are the ordinary case and do not stop a pass.
pub fn live_claims(claims: &[Claim], host: &str, alive: impl Fn(u32) -> bool) -> Vec<Claim> {
    claims
        .iter()
        .filter(|claim| claim.host != host || alive(claim.pid))
        .cloned()
        .collect()
}

/// Run the five steps on `store`, into `data_dir`.
pub fn migrate(store: &Path, data_dir: &Path) -> Result<Report> {
    refuse_a_second_run(data_dir)?;
    if !store.is_file() {
        return Err(StoreError::Refused(format!(
            "{} is not a store file - point this at the sessions.db to split",
            store.display()
        )));
    }
    // Opened by a `Store` for what that door already does: an older file is carried forward to
    // the schema the copy is written against, and a newer one is refused by name.
    let old = Store::open(store)?;
    if split::carved_keys(&old)?.is_some() {
        return Err(StoreError::Refused(format!(
            "{} is being carved one project at a time - a key is already out of it. Carve the \
             rest with `--project KEY` and finish with `--finish`; the whole pass is for a store \
             nothing has been taken out of. Nothing was renamed",
            store.display()
        )));
    }
    let plan = plan(&old)?;
    let from = Connection::open(store)?;
    match split_into(&from, data_dir, &plan) {
        Ok((projects, carried, unreplayed)) => {
            drop(from);
            drop(old);
            let kept = keep_old(store)?;
            Ok(Report {
                sessions: plan.key_of.len(),
                projects,
                kept,
                carried,
                unreplayed,
            })
        }
        Err(refused) => {
            drop(from);
            drop(old);
            // Step 4 answered with a refusal, so the old file is still the only truth and the
            // directory held no set before this pass: every file made here is this pass's to take
            // back, or a second attempt would refuse the debris as a split store.
            remove_written(data_dir);
            Err(refused)
        }
    }
}

/// The live claims a carve of `key` may not run under: the sessions of THIS project only. Every
/// other project's daemon keeps running, which is the whole reason to carve one key at a time.
pub fn blocking_claims(
    key_of: &BTreeMap<String, String>,
    key: &str,
    claims: &[Claim],
    host: &str,
    alive: impl Fn(u32) -> bool,
) -> Vec<Claim> {
    let mine: Vec<Claim> = claims
        .iter()
        .filter(|claim| key_of.get(&claim.session).is_some_and(|k| k == key))
        .cloned()
        .collect();
    live_claims(&mine, host, alive)
}

/// Carve `key` out of `store` (`docs/store-split.md` § Carving one project): one project file, the
/// same per-table and chain checks a whole pass makes, the key recorded in `sessions.db`, and only
/// then that key's rows deleted from it. The refusal under a live writer claim is this function's
/// and not a command's, so a caller cannot copy out a session a daemon is writing.
pub fn carve(
    store: &Path,
    data_dir: &Path,
    key: &str,
    host: &str,
    alive: impl Fn(u32) -> bool + Copy,
) -> Result<Carved> {
    if data_dir.join(split::SHARED_FILE).is_file() {
        return Err(StoreError::Refused(format!(
            "{} is already split: {} is there, so there is nothing left to carve out of {}. \
             Nothing was written",
            data_dir.display(),
            split::SHARED_FILE,
            split::LEGACY_FILE
        )));
    }
    if !store.is_file() {
        return Err(StoreError::Refused(format!(
            "{} is not a store file - point this at the {} to carve",
            store.display(),
            split::LEGACY_FILE
        )));
    }
    same_dir(store, data_dir)?;
    // Opened by a `Store` for what that door already does, as the whole pass does: an older file is
    // carried to the schema the copy is written against, a newer one is refused by name.
    let old = Store::open(store)?;
    let plan = plan(&old)?;
    let Some(sessions) = plan.by_key.get(key) else {
        return Err(StoreError::Refused(format!(
            "{key} is not a project key in this store: no session's tree has a root folder that \
             hashes to it. The keys here are {}; nothing was written",
            or_keys(&plan)
        )));
    };
    let carved = split::carved_keys(&old)?;
    if carved.as_ref().is_some_and(|keys| keys.contains(key)) {
        return Err(StoreError::Refused(format!(
            "project {key} is recorded as carved already - a key is carved once, and its rows in \
             {} are gone. Nothing was written",
            split::LEGACY_FILE
        )));
    }
    let path = data_dir.join(split::PROJECTS_DIR).join(format!("{key}.db"));
    if path.exists() {
        return Err(StoreError::Refused(format!(
            "{} is already there and {key} is not recorded as carved: an earlier carve stopped \
             after writing it. Check the file, then delete it and run this again - nothing was \
             written",
            path.display()
        )));
    }
    let claims = writer_claims(&old)?;
    let blocked = blocking_claims(&plan.key_of, key, &claims, host, alive);
    if let Some(first) = blocked.first() {
        let more = match blocked.len() - 1 {
            0 => String::new(),
            n => format!(", and {n} more session(s) of this project"),
        };
        return Err(StoreError::Refused(format!(
            "a daemon is writing this project: session {} is claimed by {} (pid {} on {}){more}. \
             Stop that project's daemon - `letibot --stop` in its folder - and carve it again. \
             Other projects' daemons are not a reason to stop, and nothing was written",
            first.session, first.owner, first.pid, first.host
        )));
    }
    let from = Connection::open(store)?;
    let file = write_project(&from, data_dir, key, sessions)?;
    match carve_into(&from, &old, &file, key) {
        Ok(unreplayed) => {
            let carved = split::carved_keys(&old)?.unwrap_or_default();
            Ok(Carved {
                key: key.to_string(),
                path: file.path,
                sessions: sessions.len(),
                unreplayed,
                uncarved: uncarved(&plan, &carved),
            })
        }
        // The file is this carve's own debris, and a second attempt would refuse it as a carved
        // project: a check that failed leaves no file behind, as it leaves `sessions.db` untouched.
        Err(refused) => {
            remove_written_file(&file.path);
            Err(refused)
        }
    }
}

/// The half of a carve that can fail after its file is written - the chain replay, the record and
/// the delete - in the order the doc states: no row leaves `sessions.db` before the copy it is
/// leaving for has been verified and recorded as carved.
fn carve_into(
    from: &Connection,
    old: &Store,
    file: &ProjectFile,
    key: &str,
) -> Result<Vec<String>> {
    let unreplayed = verify_chains(&file.path, Some(old))?;
    refuse_cross_key_forks(from)?;
    let tx = from.unchecked_transaction()?;
    // The delete names rows across the project tables, so the keys between them are checked at
    // COMMIT rather than per statement.
    tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
    // The index learns this key on the row itself, so a question asked from another folder - a
    // `--handover` for a session of a project that is not the caller's - resolves to the file the
    // rows are in. A whole pass writes the same column for the same reason.
    if !columns(&tx, "session")?
        .iter()
        .any(|column| column == "project")
    {
        tx.execute_batch("ALTER TABLE session ADD COLUMN project TEXT")?;
    }
    tx.execute(
        "UPDATE session SET project = ?1 WHERE id IN (SELECT id FROM temp.mine)",
        params![key],
    )?;
    tx.execute_batch(split::CARVED_SQL)?;
    tx.execute(
        &format!(
            "INSERT INTO {} (key, at_ms) VALUES (?1, ?2)",
            split::CARVED_TABLE
        ),
        params![key, now_ms()],
    )?;
    delete_key_rows(&tx)?;
    tx.commit().map_err(|e| {
        StoreError::Refused(format!(
            "project {key}'s rows could not be removed from {}: {e}. Nothing was deleted, and the \
             file this pass wrote was taken back",
            split::LEGACY_FILE
        ))
    })?;
    Ok(unreplayed)
}

/// One key's rows of every project table, out of the shared file. The `transcript_item` triggers
/// refuse a DELETE, so they are dropped and put back from their own `sqlite_master` text; and
/// `stable_prefix` keeps every row a project not carved yet still names (§ Table assignment
/// duplicates it per file, so one prefix can be two projects').
fn delete_key_rows(conn: &Connection) -> Result<()> {
    let triggers = triggers_on(conn, split::PROJECT_TABLES)?;
    for (name, _) in &triggers {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {name}"))?;
    }
    // Before the transcripts go, while the remaining projects' rows still say which prefixes they
    // name: one another project's transcript holds is not this key's to delete.
    conn.execute_batch(
        "DELETE FROM stable_prefix
          WHERE id IN (SELECT stable_prefix_id FROM transcript
                        WHERE id IN (SELECT id FROM temp.mine_transcript))
            AND NOT EXISTS (SELECT 1 FROM transcript t
                             WHERE t.stable_prefix_id = stable_prefix.id
                               AND t.id NOT IN (SELECT id FROM temp.mine_transcript))",
    )?;
    for table in split::PROJECT_TABLES {
        // The prefix is done above, and a transcript's rows go before the transcript itself.
        if matches!(*table, "stable_prefix" | "transcript") {
            continue;
        }
        let Some(filter) = rows_of(table) else {
            return Err(StoreError::Refused(format!(
                "{table} is a project table by the table assignment and this pass has no way to \
                 find its rows - nothing was deleted"
            )));
        };
        conn.execute_batch(&format!("DELETE FROM {table}{}", filter.where_clause()))?;
    }
    // The transcript rows go after their items, and the prefixes above are already decided.
    let transcript = rows_of("transcript").ok_or_else(|| {
        StoreError::Refused("transcript: no row filter - nothing was deleted".to_string())
    })?;
    conn.execute_batch(&format!(
        "DELETE FROM transcript{}",
        transcript.where_clause()
    ))?;
    for (_, sql) in &triggers {
        conn.execute_batch(sql)?;
    }
    Ok(())
}

/// Every trigger on one of these tables, with the text that recreates it: the delete above has to
/// take them out of the way and put them back exactly as the file had them.
fn triggers_on(conn: &Connection, tables: &[&str]) -> Result<Vec<(String, String)>> {
    let list = tables
        .iter()
        .map(|table| format!("'{table}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut st = conn.prepare(&format!(
        "SELECT name, sql FROM sqlite_master WHERE type = 'trigger' AND sql IS NOT NULL \
         AND tbl_name IN ({list}) ORDER BY name"
    ))?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// A transcript of a project not carved yet that is a fork of one of this key's: the delete would
/// break a reference no file of the split layout can hold, so this is refused before anything
/// leaves the shared file. A whole pass refuses the same shape at its own step 4.
fn refuse_cross_key_forks(conn: &Connection) -> Result<()> {
    let stray: Option<String> = conn
        .query_row(
            "SELECT id FROM transcript
              WHERE id NOT IN (SELECT id FROM temp.mine_transcript)
                AND parent_transcript_id IN (SELECT id FROM temp.mine_transcript)
              LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match stray {
        None => Ok(()),
        Some(id) => Err(StoreError::Refused(format!(
            "transcript {id} belongs to another project and forks this one's parent transcript, a \
             reference two files cannot carry - neither project can be carved while it is there, \
             and a whole pass refuses the same shape. Nothing was deleted from {}",
            split::LEGACY_FILE
        ))),
    }
}

/// A carve's own debris: the file, and the journal beside it, because a half-written project file
/// is worse than none and the next attempt would refuse it as a carved project. The directory goes
/// too when this carve was the one that made it - an empty `projects/` is a line in a listing that
/// says a carve happened.
fn remove_written_file(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(appended(path, suffix));
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

/// `--finish` (`docs/store-split.md` § Carving one project, the last step): once every key is
/// carved, `sessions.db` holds no project row and is renamed to the shared file P3 opens. The
/// project tables go with the rename: the finished layout has them in the project files alone, and
/// an empty `adjudication` table in `shared.db` is the thing the whole pass refuses by name.
pub fn finish(
    store: &Path,
    data_dir: &Path,
    host: &str,
    alive: impl Fn(u32) -> bool,
) -> Result<Finished> {
    let shared = data_dir.join(split::SHARED_FILE);
    if shared.is_file() {
        return Err(StoreError::Refused(format!(
            "{} is already there: this store has been finished, and a second rename would be a \
             second shared file. Nothing was renamed",
            shared.display()
        )));
    }
    if !store.is_file() {
        return Err(StoreError::Refused(format!(
            "{} is not a store file - point this at the {} to finish",
            store.display(),
            split::LEGACY_FILE
        )));
    }
    same_dir(store, data_dir)?;
    let old = Store::open(store)?;
    // The rename moves the file every daemon holds open, so this one step is the box's and runs
    // with nothing writing. Every carve before it stopped one daemon.
    let claims = writer_claims(&old)?;
    let live = live_claims(&claims, host, alive);
    if let Some(first) = live.first() {
        let more = match live.len() - 1 {
            0 => String::new(),
            n => format!(", and {n} other claim(s)"),
        };
        return Err(StoreError::Refused(format!(
            "session {} is still claimed by {} (pid {} on {}){more}: stop every daemon before the \
             last step, which renames the file they all hold. Nothing was renamed",
            first.session, first.owner, first.pid, first.host
        )));
    }
    let carved = split::carved_keys(&old)?;
    let plan = plan(&old)?;
    let files = files_in(&data_dir.join(split::PROJECTS_DIR));
    if carved.is_none() && files.is_empty() {
        return Err(StoreError::Refused(format!(
            "nothing has been carved out of {}: there is no project file under {} and no {} row. \
             Carve a key first with `--project KEY`, and finish once every key is out",
            store.display(),
            data_dir.join(split::PROJECTS_DIR).display(),
            split::CARVED_TABLE
        )));
    }
    // A `carved_project` that is not there with project files on disk is a finish that stopped
    // between its two halves: the drops are done and the rename is all that is left.
    if let Some(carved) = &carved {
        let left = uncarved(&plan, carved);
        if !left.is_empty() {
            let mut named = left.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
            if left.len() > 8 {
                named.push_str(&format!(", and {} more", left.len() - 8));
            }
            return Err(StoreError::Refused(format!(
                "{} project(s) are not carved yet: {named}. Carve each one with `--project KEY` \
                 first - the shared file may hold no project's rows. Nothing was renamed",
                left.len()
            )));
        }
    }
    drop(old);
    let conn = Connection::open(store)?;
    // The tables are empty and about to disappear, so this is the one connection in the crate
    // that writes a store with `foreign_keys` off: dropping a parent first would otherwise be a
    // constraint failure rather than a shorter file.
    conn.pragma_update(None, "foreign_keys", false)?;
    if carved.is_some() {
        for table in split::PROJECT_TABLES {
            let rows = count(&conn, table)?;
            if rows > 0 {
                return Err(StoreError::Refused(format!(
                    "{table} still holds {rows} row(s) in {} and every key the index names is \
                     carved: a project row belongs in a project file, so this store is not clean \
                     enough to rename. Nothing was renamed",
                    store.display()
                )));
            }
        }
    }
    let tx = conn.unchecked_transaction()?;
    for table in split::PROJECT_TABLES {
        tx.execute_batch(&format!("DROP TABLE IF EXISTS {table}"))?;
    }
    tx.execute_batch(&format!("DROP TABLE IF EXISTS {}", split::CARVED_TABLE))?;
    tx.commit()?;
    // The journal travels with the file or the rename loses the last commits: a full checkpoint
    // leaves nothing in it, and the two side files are then the file's to recreate.
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    drop(conn);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(appended(store, suffix));
    }
    std::fs::rename(store, &shared).map_err(|e| {
        StoreError::Refused(format!(
            "every key is carved and the project tables are dropped, but {} could not be renamed \
             to {}: {e}. Run this again once that is fixed",
            store.display(),
            shared.display()
        ))
    })?;
    Ok(Finished {
        shared,
        carved: carved
            .map(|keys| keys.into_iter().collect())
            .unwrap_or_default(),
    })
}

/// A carve's data directory is the store's own: the record of a carve is a table in `sessions.db`,
/// so a project file somewhere else is one no daemon of that folder would ever open.
fn same_dir(store: &Path, data_dir: &Path) -> Result<()> {
    let parent = store.parent().unwrap_or(Path::new("."));
    match (
        std::fs::canonicalize(parent),
        std::fs::canonicalize(data_dir),
    ) {
        (Ok(here), Ok(that)) if here == that => Ok(()),
        _ => Err(StoreError::Refused(format!(
            "{} is not in {}: the carve keeps its record in {} and its project files in \
             {}/{}, which must be one directory. Nothing was written",
            store.display(),
            data_dir.display(),
            split::LEGACY_FILE,
            data_dir.display(),
            split::PROJECTS_DIR
        ))),
    }
}

/// The keys that still have project rows in the shared file: every key the index names that no
/// carve recorded. A key whose sessions are all gone is not one - it has no rows to copy.
fn uncarved(plan: &Plan, carved: &BTreeSet<String>) -> Vec<String> {
    plan.by_key
        .keys()
        .filter(|key| !carved.contains(*key))
        .cloned()
        .collect()
}

/// The keys a store does hold, for a refusal that names them: a key is a hash, and a caller who
/// typed one from memory deserves the list.
fn or_keys(plan: &Plan) -> String {
    let keys: Vec<&str> = plan.by_key.keys().map(String::as_str).collect();
    match keys.is_empty() {
        true => "none - the index holds no session".to_string(),
        false => keys.join(", "),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Sessions to keys (§ Migration, steps 1-2).
struct Plan {
    /// Every session, and the key its tree's root hashes to.
    key_of: BTreeMap<String, String>,
    /// The same the other way round: each key's sessions, in id order.
    by_key: BTreeMap<String, Vec<String>>,
}

fn split_into(
    from: &Connection,
    data_dir: &Path,
    plan: &Plan,
) -> Result<(Vec<ProjectFile>, Vec<String>, Vec<String>)> {
    let mut projects = Vec::with_capacity(plan.by_key.len());
    for (key, sessions) in &plan.by_key {
        projects.push(write_project(from, data_dir, key, sessions)?);
    }
    let (_, carried) = write_shared(from, data_dir, plan)?;
    let unreplayed = verify_across(from, &projects)?;
    Ok((projects, carried, unreplayed))
}

/// One project file: created, every project table's rows for this key copied, and both sides of
/// every count compared, inside one transaction (§ Migration, step 2).
fn write_project(
    from: &Connection,
    data_dir: &Path,
    key: &str,
    sessions: &[String],
) -> Result<ProjectFile> {
    let projects = data_dir.join(split::PROJECTS_DIR);
    std::fs::create_dir_all(&projects)
        .map_err(|e| StoreError::Refused(format!("could not make {}: {e}", projects.display())))?;
    let path = projects.join(format!("{key}.db"));
    stage(from, sessions)?;
    let into = create(&path, StoreFile::Project)?;
    let tx = into.unchecked_transaction()?;
    // A fork names a transcript written before it and a job names a session, so the keys between
    // project tables are checked at COMMIT rather than per row.
    tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
    let mut tables: Vec<(&str, Option<Rows>)> = Vec::with_capacity(split::PROJECT_TABLES.len());
    for table in split::PROJECT_TABLES {
        let Some(filter) = rows_of(table) else {
            return Err(StoreError::Refused(format!(
                "{table} is a project table by the table assignment and this pass has no way to \
                 find its rows - nothing was renamed"
            )));
        };
        copy_table(from, &tx, table, Some(filter))?;
        tables.push((table, Some(filter)));
    }
    tx.commit()?;
    verify_tables(from, &into, &tables)?;
    drop(into);
    Ok(ProjectFile {
        key: key.to_string(),
        path,
        sessions: sessions.len(),
    })
}

/// The old file's rows for one key, as the temp tables every filter reads: its sessions and the
/// transcripts those sessions wrote.
fn stage(from: &Connection, sessions: &[String]) -> Result<()> {
    from.execute_batch(
        "DROP TABLE IF EXISTS temp.mine;
         DROP TABLE IF EXISTS temp.mine_transcript;
         CREATE TEMP TABLE mine (id TEXT PRIMARY KEY);
         CREATE TEMP TABLE mine_transcript (id TEXT PRIMARY KEY);",
    )?;
    {
        let mut insert = from.prepare("INSERT INTO temp.mine (id) VALUES (?1)")?;
        for session in sessions {
            insert.execute(params![session])?;
        }
    }
    from.execute_batch(
        "INSERT INTO temp.mine_transcript (id)
         SELECT id FROM transcript WHERE session_id IN (SELECT id FROM temp.mine)",
    )?;
    Ok(())
}

/// `shared.db` (§ Migration, step 3): every shared table whole, the `session` index gaining the
/// column that says which project file a session's rows are in.
fn write_shared(from: &Connection, data_dir: &Path, plan: &Plan) -> Result<(PathBuf, Vec<String>)> {
    let path = data_dir.join(split::SHARED_FILE);
    let into = create(&path, StoreFile::Shared)?;
    into.execute_batch("CREATE TEMP TABLE mine_key (id TEXT PRIMARY KEY, project TEXT NOT NULL)")?;
    {
        let mut insert = into.prepare("INSERT INTO temp.mine_key (id, project) VALUES (?1, ?2)")?;
        for (session, key) in &plan.key_of {
            insert.execute(params![session, key])?;
        }
    }
    let tx = into.unchecked_transaction()?;
    tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
    let mut copied: Vec<(&str, Option<Rows>)> = Vec::new();
    for table in split::SHARED_TABLES {
        if columns(&tx, table)?.is_empty() {
            continue; // a table this store never had, because it predates it
        }
        copy_table(from, &tx, table, None)?;
        copied.push((table, None));
    }
    let mut carried = Vec::new();
    for table in split::LEGACY_SHARED_TABLES {
        if carry_archive(from, &tx, table)? {
            carried.push((*table).to_string());
        }
    }
    tx.execute_batch("ALTER TABLE session ADD COLUMN project TEXT")?;
    tx.execute(
        "UPDATE session SET project = (SELECT project FROM temp.mine_key WHERE id = session.id)",
        [],
    )?;
    let unplaced: Option<String> = tx
        .query_row(
            "SELECT id FROM session WHERE project IS NULL LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(session) = unplaced {
        return Err(StoreError::Refused(format!(
            "session {session} resolved to no project key, so the index could not say which file \
             holds it - nothing was renamed"
        )));
    }
    tx.commit()?;
    // § The corpus: a decision lives in a project file and nowhere else, so the split store must not
    // hold the table at all - the assignment routing it to both files is what this catches.
    if !columns(&into, "adjudication")?.is_empty() {
        return Err(StoreError::Refused(
            "shared.db holds an adjudication table, and a decision belongs in exactly one project \
             file - nothing was renamed"
                .to_string(),
        ));
    }
    verify_tables(from, &into, &copied)?;
    drop(into);
    Ok((path, carried))
}

/// The v24 arm's archive: routed to `shared.db` by the assignment and created by no DDL here, so
/// the old file's own `sqlite_master` row is the only record of its shape. Empty is what
/// "dropped by the split if empty" means.
fn carry_archive(from: &Connection, into: &Connection, table: &str) -> Result<bool> {
    if columns(from, table)?.is_empty() {
        return Ok(false);
    }
    let rows: i64 = from.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
    if rows == 0 {
        return Ok(false);
    }
    let sql: String = from.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |r| r.get(0),
    )?;
    into.execute_batch(&sql)?;
    copy_table(from, into, table, None)?;
    Ok(true)
}

/// A new file with `file`'s schema and this build's stamp. A file that is already there is
/// refused by name: the pass runs once, and copying over a file a daemon has been writing is the
/// one mistake it exists to prevent.
fn create(path: &Path, file: StoreFile) -> Result<Connection> {
    if path.exists() {
        return Err(StoreError::Refused(format!(
            "{} is already there - this pass runs once, on a store that has not been split",
            path.display()
        )));
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            StoreError::Refused(format!("could not make {}: {e}", parent.display()))
        })?;
    }
    let conn = Connection::open(path)?;
    pragmas(&conn)?;
    conn.execute_batch(&split::schema_sql(file))?;
    conn.execute("DELETE FROM schema_version", [])?;
    conn.execute(
        "INSERT INTO schema_version (version) VALUES (?1)",
        params![SCHEMA_VERSION],
    )?;
    Ok(conn)
}

/// How one project table's rows are found for one key (§ Table assignment, project half).
#[derive(Debug, Clone, Copy)]
enum Rows {
    /// The rows whose column names one of the key's sessions.
    Session(&'static str),
    /// The rows whose column names one of the key's transcripts - the history's own two.
    Transcript(&'static str),
    /// Content-addressed rows the key's transcripts name, copied into every file that names them
    /// so a fork or a resume reads no other project's file.
    Referenced,
}

impl Rows {
    /// The `WHERE` that finds these rows, over the connection's own file plus the temp tables
    /// [`stage`] fills. Empty for a table that is copied whole.
    fn where_clause(self) -> String {
        match self {
            Rows::Session(column) => format!(" WHERE {column} IN (SELECT id FROM temp.mine)"),
            Rows::Transcript(column) => {
                format!(" WHERE {column} IN (SELECT id FROM temp.mine_transcript)")
            }
            Rows::Referenced => " WHERE id IN (SELECT stable_prefix_id FROM transcript \
                                  WHERE id IN (SELECT id FROM temp.mine_transcript))"
                .to_string(),
        }
    }
}

/// Project tables that are caches over the transcripts, absent from a store older than them.
const DERIVED: &[&str] = &["recall_unit"];

/// Which rows of a project table are one key's, or `None` for a table the assignment gained and
/// this pass does not know - a refusal by name, never a copy of nothing.
fn rows_of(table: &str) -> Option<Rows> {
    Some(match table {
        "transcript" => Rows::Session("session_id"),
        "transcript_item" => Rows::Transcript("transcript_id"),
        "stable_prefix" => Rows::Referenced,
        // per-session state, a session's background work, the prompt journal, its decisions
        "todo" | "citation" | "session_marks" | "session_mode" | "session_monitor" | "job"
        | "job_context" | "process" | "queued_prompt" | "adjudication" => {
            Rows::Session("session_id")
        }
        // parent and child are always in one daemon, and a child that never got a session row of
        // its own still has a parent that did
        "subagent_run" => Rows::Session("parent_id"),
        "recall_unit" => Rows::Session("session"),
        _ => return None,
    })
}

/// One table's rows, in primary-key order so a transcript's rows arrive in `seq` order - which is
/// what the append-only trigger under them insists on.
fn copy_table(
    from: &Connection,
    into: &Connection,
    table: &str,
    filter: Option<Rows>,
) -> Result<()> {
    let wanted = columns(into, table)?;
    let held = columns(from, table)?;
    // An index the daemon rebuilds from the transcripts: an old store without it has nothing to carry.
    if held.is_empty() && DERIVED.contains(&table) {
        return Ok(());
    }
    if held.is_empty() {
        return Err(StoreError::Refused(format!(
            "the old store has no {table} table and this build's schema does - the two are not \
             the same store, and nothing was renamed"
        )));
    }
    if let Some(missing) = wanted.iter().find(|column| !held.contains(column)) {
        return Err(StoreError::Refused(format!(
            "the old store's {table} has no {missing} column and this build's schema does - \
             nothing was renamed"
        )));
    }
    let list = wanted.join(", ");
    let holders = (1..=wanted.len())
        .map(|at| format!("?{at}"))
        .collect::<Vec<_>>()
        .join(", ");
    let where_clause = filter.map(Rows::where_clause).unwrap_or_default();
    let order = primary_key(into, table)?.join(", ");
    let mut read = from.prepare(&format!(
        "SELECT {list} FROM {table}{where_clause} ORDER BY {order}"
    ))?;
    let mut write = into.prepare(&format!("INSERT INTO {table} ({list}) VALUES ({holders})"))?;
    let mut rows = read.query([])?;
    while let Some(row) = rows.next()? {
        let values: Vec<rusqlite::types::Value> = (0..wanted.len())
            .map(|at| row.get(at))
            .collect::<std::result::Result<_, _>>()?;
        write.execute(params_from_iter(values))?;
    }
    Ok(())
}

/// Step 4 for one file: each table's row count and the hash of its primary keys are the old
/// store's for those rows, per § Migration.
fn verify_tables(
    from: &Connection,
    into: &Connection,
    tables: &[(&str, Option<Rows>)],
) -> Result<()> {
    for (table, filter) in tables {
        let want = keys(from, table, *filter)?;
        let got = keys(into, table, None)?;
        if want != got {
            return Err(StoreError::Refused(format!(
                "{table}: the new file holds {} row(s) whose primary keys hash to {}, and this \
                 project's rows in the old store are {} hashing to {}. Nothing was renamed",
                got.0, got.1, want.0, want.1
            )));
        }
    }
    Ok(())
}

/// Step 4 across files: every old row in exactly one file, and every transcript replaying its
/// hash chain the way a resume reads it - or failing exactly as it does in the old store.
fn verify_across(from: &Connection, projects: &[ProjectFile]) -> Result<Vec<String>> {
    verify_once(from, projects)?;
    verify_adjudication(from, projects)?;
    let source = from
        .path()
        .filter(|p| !p.is_empty())
        .map(|p| Store::open_reader(Path::new(p)))
        .transpose()?;
    let mut unreplayed = Vec::new();
    for file in projects {
        unreplayed.extend(verify_chains(&file.path, source.as_ref())?);
    }
    Ok(unreplayed)
}

/// "A row of every project table is in exactly one file": one file per key was checked as it was
/// written, and this is the sum, which a row copied into two files would inflate. `stable_prefix`
/// is duplicated per project by the assignment, so it has no sum rule.
fn verify_once(from: &Connection, projects: &[ProjectFile]) -> Result<()> {
    let tables: Vec<&str> = split::PROJECT_TABLES
        .iter()
        .copied()
        .filter(|table| *table != "stable_prefix")
        .collect();
    let mut sums = vec![0usize; tables.len()];
    for file in projects {
        let into = Connection::open(&file.path)?;
        for (at, table) in tables.iter().enumerate() {
            sums[at] += count(&into, table)?;
        }
    }
    for (at, table) in tables.iter().enumerate() {
        let want = count(from, table)?;
        if sums[at] != want {
            return Err(StoreError::Refused(format!(
                "{table}: the old store holds {want} row(s) and the project files hold {} - a row \
                 is missing or in two files, and nothing was renamed",
                sums[at]
            )));
        }
    }
    Ok(())
}

/// "Every `adjudication` row is in exactly one project file": the old count is the sum over the
/// new files, and a decision in two of them is named. `shared.db` holding the table at all is the
/// other half of § The corpus, checked where that file is written.
fn verify_adjudication(from: &Connection, projects: &[ProjectFile]) -> Result<()> {
    let want: BTreeSet<String> = key_texts(from, "adjudication", None)?.into_iter().collect();
    let mut seen: HashMap<String, &Path> = HashMap::new();
    for file in projects {
        let into = Connection::open(&file.path)?;
        for row in key_texts(&into, "adjudication", None)? {
            if let Some(there) = seen.insert(row.clone(), &file.path) {
                return Err(StoreError::Refused(format!(
                    "adjudication row {row} is in both {} and {} - a decision belongs to exactly \
                     one file, and nothing was renamed",
                    there.display(),
                    file.path.display()
                )));
            }
        }
    }
    if seen.len() != want.len() {
        let first = want.iter().find(|row| !seen.contains_key(*row));
        return Err(StoreError::Refused(format!(
            "{} of the old store's {} adjudication row(s) are in no project file{} - nothing was \
             renamed",
            want.len() - seen.len(),
            want.len(),
            first
                .map(|row| format!(", {row} first"))
                .unwrap_or_default()
        )));
    }
    Ok(())
}

/// The last clause of step 4, and the one check that reads the copy the way a resume will: every
/// transcript's rows rebuild its chain from the stable prefix it names.
fn verify_chains(path: &Path, source: Option<&Store>) -> Result<Vec<String>> {
    let replay = |store: &Store, transcript: &str| -> Result<Option<String>> {
        let loaded = store.load_transcript(transcript)?;
        Ok(TokenLedger::restore(
            transcript,
            &loaded.prefix_tokens,
            loaded.h_init,
            &loaded.ledger_input(),
        )
        .err()
        .map(|e| e.to_string()))
    };
    let store = Store::open(path)?;
    let transcripts = {
        let conn = store.connection();
        strings(&conn, "SELECT id FROM transcript ORDER BY id")?
    };
    let mut unreplayed = Vec::new();
    for transcript in transcripts {
        let Some(e) = replay(&store, &transcript)? else {
            continue;
        };
        // The copy is faithful when the old store fails the same way: carried, and said.
        if let Some(old) = source
            && replay(old, &transcript)?.as_deref() == Some(e.as_str())
        {
            unreplayed.push(format!("{transcript} ({e})"));
            continue;
        }
        return Err(StoreError::Refused(format!(
            "{}: transcript {transcript} does not replay after the copy - {e}. Nothing was \
             renamed",
            path.display()
        )));
    }
    Ok(unreplayed)
}

/// The old file, under the name step 5 leaves it.
fn keep_old(store: &Path) -> Result<PathBuf> {
    let kept = appended(store, KEPT_SUFFIX);
    std::fs::rename(store, &kept).map_err(|e| {
        StoreError::Refused(format!(
            "every check passed and the files are written, but {} could not be renamed to {}: {e}",
            store.display(),
            kept.display()
        ))
    })?;
    // A store closed a moment ago leaves no WAL; one that does must travel with its file or the
    // kept copy is missing its last commits.
    for suffix in ["-wal", "-shm"] {
        let wal = appended(store, suffix);
        if wal.exists() {
            let _ = std::fs::rename(&wal, appended(&kept, suffix));
        }
    }
    Ok(kept)
}

/// A pass that has already run leaves a set here, and copying a `sessions.db` over files a daemon
/// has been writing since is the one thing this pass must never do.
fn refuse_a_second_run(data_dir: &Path) -> Result<()> {
    let shared = data_dir.join(split::SHARED_FILE);
    let made = std::iter::once(shared)
        .filter(|path| path.exists())
        .chain(files_in(&data_dir.join(split::PROJECTS_DIR)));
    match made.into_iter().next() {
        None => Ok(()),
        Some(first) => Err(StoreError::Refused(format!(
            "{} is already there - this pass runs ONCE, on a store that has not been split. \
             Nothing was written and nothing was renamed",
            first.display()
        ))),
    }
}

/// Every store file this pass could have made under `dir`: the directory held none of them when
/// the pass started, so each one here is its own to remove.
fn remove_written(data_dir: &Path) {
    let shared = data_dir.join(split::SHARED_FILE);
    for path in [
        shared.clone(),
        appended(&shared, "-wal"),
        appended(&shared, "-shm"),
    ] {
        let _ = std::fs::remove_file(path);
    }
    for file in files_in(&data_dir.join(split::PROJECTS_DIR)) {
        let _ = std::fs::remove_file(&file);
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(appended(&file, suffix));
        }
    }
}

/// The files under `dir` whose name says it is a store: the only two places this pass writes are
/// `projects/` and the shared file.
fn files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "db"))
        .collect()
}

/// `path` with `suffix` put on the end of its name, as step 5 spells the kept file.
fn appended(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Sessions to keys (§ Migration, step 1): each session to its tree's root, and the root's
/// recorded `workspace_root` to a project key.
fn plan(store: &Store) -> Result<Plan> {
    let mut parents: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut workspace: BTreeMap<String, String> = BTreeMap::new();
    {
        let conn = store.connection();
        let mut st =
            conn.prepare("SELECT id, parent_session_id, workspace_root FROM session ORDER BY id")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (id, parent, recorded) = row?;
            parents.insert(id.clone(), parent);
            workspace.insert(id, recorded);
        }
    }
    let mut key_of = BTreeMap::new();
    for id in parents.keys() {
        let root = root_of(id, &parents)?;
        let recorded = workspace.get(root).ok_or_else(|| {
            StoreError::Refused(format!(
                "session {root} is the root of a tree and the store has no workspace_root for it, \
                 so its tree has no folder to hash a project key from - nothing was renamed"
            ))
        })?;
        if recorded.is_empty() {
            return Err(StoreError::Refused(format!(
                "session {root} records no workspace_root, so its tree has no folder to hash a \
                 project key from - nothing was renamed"
            )));
        }
        key_of.insert(id.clone(), project_key(recorded));
    }
    let mut by_key: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (session, key) in &key_of {
        by_key.entry(key.clone()).or_default().push(session.clone());
    }
    Ok(Plan { key_of, by_key })
}

/// The root of a session's tree. A parent this store does not hold stops the walk: the child is
/// then its own root, which is the answer a resume gives it too.
fn root_of<'a>(id: &'a str, parents: &'a BTreeMap<String, Option<String>>) -> Result<&'a str> {
    let mut at = id;
    let mut seen = BTreeSet::new();
    while let Some(Some(parent)) = parents.get(at) {
        if !seen.insert(at) {
            return Err(StoreError::Refused(format!(
                "the parent edges of the sessions form a cycle at {at}, so that tree has no root \
                 and no project key - nothing was renamed"
            )));
        }
        at = parent.as_str();
    }
    Ok(at)
}

/// A table's rows as `(how many, a hash of their primary keys in sorted order)`: a count a
/// truncated copy cannot fake, and an identity a reordered one keeps.
fn keys(conn: &Connection, table: &str, filter: Option<Rows>) -> Result<(usize, String)> {
    let mut texts = key_texts(conn, table, filter)?;
    texts.sort_unstable();
    let mut h = Sha256::new();
    for one in &texts {
        h.update(one.as_bytes());
        h.update([0]);
    }
    Ok((texts.len(), hex(&h.finalize().into())))
}

/// One string per row from the table's primary-key columns, each column terminated so two columns
/// cannot run together.
fn key_texts(conn: &Connection, table: &str, filter: Option<Rows>) -> Result<Vec<String>> {
    let key = primary_key(conn, table)?;
    let where_clause = filter.map(Rows::where_clause).unwrap_or_default();
    let mut st = conn.prepare(&format!(
        "SELECT {} FROM {table}{where_clause}",
        key.join(", ")
    ))?;
    let mut rows = st.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut one = String::new();
        for at in 0..key.len() {
            one.push_str(&as_text(row.get_ref(at)?));
            one.push('\u{1}');
        }
        out.push(one);
    }
    Ok(out)
}

fn as_text(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "\u{2205}".to_string(),
        ValueRef::Integer(number) => number.to_string(),
        ValueRef::Real(number) => number.to_string(),
        ValueRef::Text(text) => String::from_utf8_lossy(text).into_owned(),
        ValueRef::Blob(bytes) => bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
    }
}

/// The columns of a table's primary key, in key order. Empty for a table that is not there.
fn primary_key(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut keyed: Vec<(i64, String)> = {
        let mut st = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(5)?, r.get::<_, String>(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    keyed.retain(|(at, _)| *at > 0);
    keyed.sort();
    if keyed.is_empty() {
        return Err(StoreError::Refused(format!(
            "{table} has no primary key, so a row of it cannot be counted - nothing was renamed"
        )));
    }
    Ok(keyed.into_iter().map(|(_, column)| column).collect())
}

/// A table's columns in the file's own order. Empty for a table the file does not hold.
fn columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut st = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = st.query_map([], |r| r.get::<_, String>(1))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn count(conn: &Connection, table: &str) -> Result<usize> {
    let rows: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
    Ok(rows.max(0) as usize)
}

fn strings(conn: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut st = conn.prepare(sql)?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
