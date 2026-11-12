//! The store split (`docs/store-split.md`): two files, one schema each, one writer each.
//!
//! The table assignment is DATA and lives here alone: [`place_of`] answers which file a table's
//! rows are in, so a table's NAME decides its file and nothing has to remember the list. Each file
//! is created from the tree's own DDL filtered by that assignment, so neither file can drift from
//! what `Store::migrate` builds on a single-file store.
//!
//! A box reaches that layout one project at a time (§ Carving one project), and the state in
//! between is this module's too: `sessions.db` is then the shared file, holding the shared tables
//! and the project tables of every key not carved out of it yet, `projects/<key>.db` exists for the
//! keys a carve recorded, and [`carved_keys`] is the record. [`shared_path`] and [`file_of`] answer
//! where a table's rows are in either shape, so no caller has to know which one it is reading.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak, mpsc};

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    CORPUS_INDEXES_SQL, READER_FLOOR_SQL, RECALL_SQL, Result, SCHEMA_SQL, SCHEMA_VERSION,
    SESSION_INDEXES_SQL, Store, StoreError, citations, leases, marks, modes, owners, pragmas, procs,
    prompts, queues, seat, subagents, told, writers,
};

/// The shared file, beside the directory project files are in (§ The shape).
pub const SHARED_FILE: &str = "shared.db";

/// One file per folder's daemon, under the data directory: `projects/<key>.db`.
pub const PROJECTS_DIR: &str = "projects";

/// The single file a daemon wrote before the split. While projects are carved out of it one at a
/// time it IS the shared file (§ Carving one project), so this build reads it in both shapes.
pub const LEGACY_FILE: &str = "sessions.db";

/// The transition's one record, in `sessions.db` and nowhere else: which project keys have their
/// own file already. Created by a carve and by no schema at all, so a fresh set never holds it and
/// `--finish` drops it with the rest of the old layout.
pub const CARVED_TABLE: &str = "carved_project";

/// The table's DDL. Not one of [`batches`]: it belongs to the transition, not to either file of
/// the finished set, which is why the assignment has no line for it.
pub const CARVED_SQL: &str = "CREATE TABLE IF NOT EXISTS carved_project (key TEXT PRIMARY KEY, \
     at_ms INTEGER NOT NULL)";

/// Which file a table lives in, as § Table assignment lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// `shared.db`: what is read across folders or arbitrated between daemons.
    Shared,
    /// `projects/<key>.db`: everything one folder's sessions own.
    Project,
    /// One row per file, in both files: the two tables that say what schema a file is.
    Both,
}

/// The two files of a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreFile {
    Shared,
    Project,
}

/// Project file - everything one folder's sessions own (§ Table assignment).
pub const PROJECT_TABLES: &[&str] = &[
    // the history and the token blobs
    "transcript",
    "transcript_item",
    // written once per prefix, duplicated per project so a fork reads no other file
    "stable_prefix",
    // per-session state
    "todo",
    "citation",
    "session_marks",
    "session_mode",
    "session_monitor",
    // a session's background work
    "job",
    "job_context",
    "process",
    // the prompt journal
    "queued_prompt",
    // parent and child are always in the same daemon, so a spawn stays one transaction
    "subagent_run",
    // written by the session that asked and read back out of this file (§ The corpus)
    "adjudication",
    // the project's own past conversation, indexed for recall: the folder's history, so the
    // folder's file, and no key to filter because every row in it is this project's
    "recall_unit",
];

/// Shared file - what is read across folders or arbitrated between daemons. The doc's list plus the
/// four tables added after it was written (`claude_review`, `queue_mute`, `seat_todo`, `seat_job`),
/// each by the doc's own rule: box-wide, or read by whichever daemon serves the queue.
pub const SHARED_TABLES: &[&str] = &[
    // the index: the session list, the picker and `--continue NAME` read this and nothing else
    "session",
    // which daemon holds a session, which arbitrates across daemons by definition
    "session_writer",
    // keyed by repository, and a repository's queue is served by whichever daemon holds its lease
    "review_queue",
    "merge_queue",
    "queue_lease",
    "queue_transition",
    "queue_told",
    "target_health",
    "claude_review",
    "queue_mute",
    // ports and hosts are box-wide, and so is the heartbeat
    "host_lease",
    "daemon_life",
    // keyed by folder, read before a daemon has a session
    "project_mode",
    "mode_import",
    // a seat is not a session, and its board and jobs are read by every head
    "seat_todo",
    "seat_job",
    // an owner and what it holds are box-wide: a reaper with no daemon opens one file, and a
    // resource's host is a column rather than a file
    "owner",
    "holding",
    "reap_event",
];

/// Named by § Table assignment and created by no fresh store: the v24 arm's archive of the single
/// pre-v24 pair. Routed to `shared.db` so a migrated file's rows land where the doc says.
pub const LEGACY_SHARED_TABLES: &[&str] = &["merge_queue_v23", "merge_review_v23"];

/// One per file, in both files (§ Table assignment, the last line of the shared list).
pub const BOTH_TABLES: &[&str] = &["schema_version", "schema_reader_floor"];

/// Which file a table's rows are in, or `None` for a table this module has never been told about -
/// which is the test's failure by name, not a default.
pub fn place_of(table: &str) -> Option<Place> {
    if PROJECT_TABLES.contains(&table) {
        Some(Place::Project)
    } else if SHARED_TABLES.contains(&table) || LEGACY_SHARED_TABLES.contains(&table) {
        Some(Place::Shared)
    } else if BOTH_TABLES.contains(&table) {
        Some(Place::Both)
    } else {
        None
    }
}

/// Does `file` hold `table`? The one question a routed statement and a test both ask.
pub fn holds(file: StoreFile, table: &str) -> bool {
    match place_of(table) {
        Some(Place::Both) => true,
        Some(Place::Shared) => file == StoreFile::Shared,
        Some(Place::Project) => file == StoreFile::Project,
        None => false,
    }
}

/// Every table `file` is assigned, the two it shares with the other file included.
pub fn tables_in(file: StoreFile) -> impl Iterator<Item = &'static str> {
    let own: &'static [&'static str] = match file {
        StoreFile::Shared => SHARED_TABLES,
        StoreFile::Project => PROJECT_TABLES,
    };
    BOTH_TABLES.iter().chain(own.iter()).copied()
}

/// The batches `Store::migrate` builds a fresh file from, in its own order, plus the reader-floor
/// table its `ensure_reader_floor` runs. One list, so a batch added there is routed or refused here.
pub(super) fn batches() -> [&'static str; 18] {
    [
        SCHEMA_SQL,
        SESSION_INDEXES_SQL,
        CORPUS_INDEXES_SQL,
        queues::QUEUES_SQL,
        subagents::SUBAGENTS_SQL,
        modes::MODES_SQL,
        told::TOLD_SQL,
        marks::MARKS_SQL,
        seat::SEAT_TODO_SQL,
        seat::SEAT_JOB_SQL,
        citations::CITATIONS_SQL,
        writers::WRITERS_SQL,
        procs::PROCS_SQL,
        leases::LEASES_SQL,
        prompts::PROMPTS_SQL,
        RECALL_SQL,
        owners::OWNERS_SQL,
        READER_FLOOR_SQL,
    ]
}

/// One batch split at its statements. Every statement in the tree's schema starts at the left
/// margin, so a line beginning with `CREATE` begins one and a trigger's body stays inside its own.
pub(super) fn statements(batch: &str) -> Vec<&str> {
    let mut starts = Vec::new();
    let mut at = 0;
    for line in batch.split_inclusive('\n') {
        if line.trim_start().starts_with("CREATE ") {
            starts.push(at);
        }
        at += line.len();
    }
    let mut out = Vec::with_capacity(starts.len());
    for (i, start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(batch.len());
        out.push(&batch[*start..end]);
    }
    out
}

/// The table one statement is about: the name after `CREATE TABLE`, and for an index or a trigger
/// the name its `ON` clause names.
pub(super) fn table_of(statement: &str) -> Option<String> {
    let words: Vec<&str> = statement
        .split_whitespace()
        .map(clean_word)
        .filter(|word| !word.is_empty())
        .collect();
    let mut at = 1;
    if words.get(at)?.eq_ignore_ascii_case("UNIQUE") {
        at += 1;
    }
    let kind = *words.get(at)?;
    at += 1;
    if words.get(at)?.eq_ignore_ascii_case("IF") {
        at += 3; // IF NOT EXISTS
    }
    match kind.to_ascii_lowercase().as_str() {
        "table" => Some(words.get(at)?.to_string()),
        "index" | "trigger" => {
            let on = words
                .iter()
                .skip(at)
                .position(|w| w.eq_ignore_ascii_case("ON"))?;
            Some(words.get(at + on + 1)?.to_string())
        }
        _ => None,
    }
}

fn clean_word(word: &str) -> &str {
    word.trim_matches(|c: char| matches!(c, '(' | ')' | ';' | ','))
}

/// The file holding the shared tables: `shared.db` once `--finish` renamed it, and `sessions.db`
/// while some project's rows are still in it. Asked as a question about the directory, so a
/// caller never has to know which state the box is in.
pub fn shared_path(dir: &Path) -> PathBuf {
    let finished = dir.join(SHARED_FILE);
    let legacy = dir.join(LEGACY_FILE);
    if !finished.is_file() && legacy.is_file() {
        legacy
    } else {
        finished
    }
}

/// Where `table`'s rows for `key` are, in the shape the directory is in: a project table of a key
/// no carve has taken yet is still in `sessions.db`, which is the answer
/// `Config::store_at().path(table)` needs - a path to a project file that does not exist is a
/// create waiting to happen.
pub fn file_of(dir: &Path, key: &str, table: &str) -> PathBuf {
    let shared = shared_path(dir);
    let project = dir.join(PROJECTS_DIR).join(format!("{key}.db"));
    match place_of(table) {
        Some(Place::Project)
            if shared.file_name().is_some_and(|name| name == LEGACY_FILE) && !project.is_file() =>
        {
            shared
        }
        Some(Place::Project) => project,
        _ => shared,
    }
}

/// The keys a store file records as carved, or `None` for a file nothing has been carved out of
/// (it has the transition's table or it does not) - which is how an untouched `sessions.db` is
/// told from one mid-transition.
pub fn carved_keys(store: &Store) -> Result<Option<BTreeSet<String>>> {
    let conn = store.connection();
    let has: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [CARVED_TABLE],
        |r| r.get(0),
    )?;
    if has == 0 {
        return Ok(None);
    }
    let mut st = conn.prepare(&format!("SELECT key FROM {CARVED_TABLE} ORDER BY key"))?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    Ok(Some(rows.collect::<std::result::Result<_, _>>()?))
}

/// The DDL one file is built from: the statements whose table it holds, in `migrate`'s own order.
pub fn schema_sql(file: StoreFile) -> String {
    let mut text = String::new();
    for batch in batches() {
        for statement in statements(batch) {
            let Some(table) = table_of(statement) else {
                continue;
            };
            if holds(file, &table) {
                text.push_str(statement.trim_end());
                text.push('\n');
            }
        }
    }
    match file {
        StoreFile::Shared => text,
        StoreFile::Project => without_cross_file_keys(text),
    }
}

/// A project file holds no `session` row - the index is `shared.db`'s (§ Table assignment) - so a
/// key to it could never be checked here, and with `foreign_keys` on it would refuse the first
/// append for a table this file does not have. The keys between project tables are kept.
fn without_cross_file_keys(sql: String) -> String {
    sql.replace(" REFERENCES session(id) ON DELETE CASCADE", "")
        .replace(" REFERENCES session(id)", "")
}

/// One file's writer: the thread that owns its one write connection, and the queue a caller hands
/// work to. `run` sends and waits, so a caller holds no lock across a write; `exec`/`write_tx` keep
/// their own connection until P3 re-points the daemon at this queue.
pub struct Writer {
    path: PathBuf,
    jobs: Option<mpsc::Sender<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

impl Writer {
    /// Start the thread and its connection, so a refused open is the caller's error rather than a
    /// file that quietly never got its schema.
    fn spawn(path: PathBuf) -> Result<Self> {
        let (jobs, queued) = mpsc::channel::<Job>();
        let (ready, opened) = mpsc::channel::<Result<()>>();
        let mine = path.clone();
        let thread = std::thread::Builder::new()
            .name(format!("store-writer-{}", file_name(&path)))
            .spawn(move || {
                let conn = Connection::open(&mine)
                    .map_err(StoreError::from)
                    .and_then(|conn| pragmas(&conn).map(|()| conn));
                let mut conn = match conn {
                    Ok(conn) => {
                        let _ = ready.send(Ok(()));
                        conn
                    }
                    Err(e) => {
                        let _ = ready.send(Err(e));
                        return;
                    }
                };
                while let Ok(job) = queued.recv() {
                    job(&mut conn);
                }
            })
            .map_err(|e| {
                StoreError::Refused(format!(
                    "could not start the writer for {}: {e}",
                    path.display()
                ))
            })?;
        match opened.recv() {
            Ok(Ok(())) => Ok(Writer {
                path,
                jobs: Some(jobs),
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(StoreError::Refused(format!(
                "the writer for {} stopped before it opened the file",
                path.display()
            ))),
        }
    }

    /// Hand `job` to this file's writer and answer what it returned. The caller blocks on the
    /// answer and holds nothing else, which is the whole point of the queue.
    pub fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (answer, arrival) = mpsc::channel();
        let queued = move |conn: &mut Connection| {
            let _ = answer.send(job(conn));
        };
        self.jobs
            .as_ref()
            .ok_or_else(|| self.stopped())?
            .send(Box::new(queued))
            .map_err(|_| self.stopped())?;
        arrival.recv().map_err(|_| self.stopped())?
    }

    /// The file this writer writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn stopped(&self) -> StoreError {
        StoreError::Refused(format!(
            "the writer for {} has stopped",
            self.path.display()
        ))
    }
}

impl Drop for Writer {
    /// Closing the queue ends the thread's `recv`, and the join waits for a job already in flight so
    /// its connection is never closed under it.
    fn drop(&mut self) {
        let _ = self.jobs.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A project key's two files, opened once per process: `shared.db` and `projects/<key>.db`.
pub struct StoreSet {
    dir: PathBuf,
    key: String,
    shared: Store,
    project: Store,
    shared_writer: Writer,
    project_writer: Writer,
}

impl StoreSet {
    /// Open - and on a fresh data directory create - this key's two files. A second call in one
    /// process answers the same set: one connection and one writer per file, not a second of either.
    pub fn open(dir: &Path, key: &str) -> Result<Arc<StoreSet>> {
        check_key(key)?;
        let projects = dir.join(PROJECTS_DIR);
        std::fs::create_dir_all(&projects).map_err(|e| {
            StoreError::Refused(format!("could not make {}: {e}", projects.display()))
        })?;
        // Held across the creation so two threads cannot each build a set; nothing inside opens one.
        let mut open = sets().lock().unwrap_or_else(|e| e.into_inner());
        let id = (canonical(dir), key.to_string());
        if let Some(set) = open.get(&id).and_then(Weak::upgrade) {
            return Ok(set);
        }
        let set = Arc::new(Self::create(dir, key)?);
        open.insert(id, Arc::downgrade(&set));
        Ok(set)
    }

    fn create(dir: &Path, key: &str) -> Result<Self> {
        let shared_path = dir.join(SHARED_FILE);
        let project_path = dir.join(PROJECTS_DIR).join(format!("{key}.db"));
        let shared_writer = Writer::spawn(shared_path.clone())?;
        let writer = shared_writer.path().to_path_buf();
        shared_writer.run(move |conn| prepare(conn, &writer, StoreFile::Shared))?;
        let project_writer = Writer::spawn(project_path.clone())?;
        let writer = project_writer.path().to_path_buf();
        project_writer.run(move |conn| prepare(conn, &writer, StoreFile::Project))?;
        Ok(StoreSet {
            dir: dir.to_path_buf(),
            key: key.to_string(),
            shared: Store::open(&shared_path)?,
            project: Store::open(&project_path)?,
            shared_writer,
            project_writer,
        })
    }

    /// The data directory this set is under.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The project key these files are named by.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The file every listing reads, and the file this folder's sessions own.
    pub fn store(&self, file: StoreFile) -> &Store {
        match file {
            StoreFile::Shared => &self.shared,
            StoreFile::Project => &self.project,
        }
    }

    pub fn shared(&self) -> &Store {
        &self.shared
    }

    pub fn project(&self) -> &Store {
        &self.project
    }

    /// This file's one writer: the queue every write to it is meant to go through.
    pub fn writer(&self, file: StoreFile) -> &Writer {
        match file {
            StoreFile::Shared => &self.shared_writer,
            StoreFile::Project => &self.project_writer,
        }
    }

    /// Where a file of this set is.
    pub fn path(&self, file: StoreFile) -> PathBuf {
        match file {
            StoreFile::Shared => self.dir.join(SHARED_FILE),
            StoreFile::Project => self.dir.join(PROJECTS_DIR).join(format!("{}.db", self.key)),
        }
    }
}

/// A handle onto one key's two files: both connections in one [`Store`], so a call routes by
/// [`place_of`] and no caller names a file. `create` is the daemon's door, which makes the files
/// when they are not there; a reader's door leaves them alone, and a data directory whose store
/// has not been split is [`refuse_unsplit`]'s refusal by name.
pub fn open_handle(dir: &Path, key: &str, create: bool) -> Result<Store> {
    check_key(key)?;
    let projects = dir.join(PROJECTS_DIR);
    let shared_path = dir.join(SHARED_FILE);
    let project_path = projects.join(format!("{key}.db"));
    // Mid-transition (§ Carving one project): a `sessions.db` and no `shared.db` yet. Read before
    // `refuse_unsplit`, which is the answer for a file nothing has been carved out of - the same
    // files, and only the transition's record tells them apart.
    if !shared_path.is_file() && dir.join(LEGACY_FILE).is_file() {
        return open_transition(dir, key);
    }
    if create {
        // A daemon is the first to touch a fresh directory, so its door refuses an un-split one
        // too: creating a set beside `sessions.db` hides every session that file holds.
        refuse_unsplit(dir)?;
        std::fs::create_dir_all(&projects).map_err(|e| {
            StoreError::Refused(format!("could not make {}: {e}", projects.display()))
        })?;
        // Created from the tree's own DDL filtered by the assignment, and stamped with this
        // build's version, so `Store::open` below has nothing to migrate in either file.
        for (path, file) in [
            (&shared_path, StoreFile::Shared),
            (&project_path, StoreFile::Project),
        ] {
            let conn = Connection::open(path)?;
            pragmas(&conn)?;
            prepare(&conn, path, file)?;
        }
    } else {
        refuse_unsplit(dir)?;
        if !shared_path.is_file() {
            return Err(StoreError::Refused(format!(
                "{} holds no {SHARED_FILE}: there is no split store here to read",
                dir.display()
            )));
        }
    }
    let shared = Store::open(&shared_path)?;
    // A set whose project file is not there is still a set: its listing is the index's, and a
    // question about a transcript is the question a renamed-away file cannot answer.
    let project = match project_path.is_file() {
        true => Some(Store::open(&project_path)?.shared.clone()),
        false => None,
    };
    Ok(Store::of_set(shared.shared.clone(), project))
}

/// A data directory mid-transition: `sessions.db` is the shared file, and a key has a project file
/// of its own only once a carve recorded it. An uncarved key opens that file as its ONLY one - its
/// project rows are still in it - but as a set's handle: the listing must not fall back to the
/// single-file query, which reads the history this file no longer holds whole (a carved project's
/// transcripts are in its own file), and would answer a cut-down session list.
fn open_transition(dir: &Path, key: &str) -> Result<Store> {
    let legacy = dir.join(LEGACY_FILE);
    let shared = Store::open(&legacy)?;
    let Some(carved) = carved_keys(&shared)? else {
        return Err(unsplit(dir));
    };
    let project_path = dir.join(PROJECTS_DIR).join(format!("{key}.db"));
    if !carved.contains(key) {
        return Ok(Store::of_set(shared.shared.clone(), None));
    }
    if !project_path.is_file() {
        return Err(StoreError::Refused(format!(
            "{} records project {key} as carved and {} is not there, so that key's rows are in \
             neither file. Put the file back, or delete its {CARVED_TABLE} row and carve the key \
             again - this daemon writes nothing until one of those is true.",
            legacy.display(),
            project_path.display()
        )));
    }
    let project = Store::open(&project_path)?;
    Ok(Store::of_set(
        shared.shared.clone(),
        Some(project.shared.clone()),
    ))
}

/// A set's file that is not there yet, in a data directory that still holds `sessions.db`, is
/// refused rather than created: any door that made one would hide every session the old file
/// holds. A file already there opens, which is what the migration's own checks need.
pub(crate) fn refuse_creating_beside_legacy(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let parent = path.parent();
    let dir = match path.file_name().and_then(|n| n.to_str()) {
        Some(SHARED_FILE) => parent,
        _ if parent
            .and_then(Path::file_name)
            .is_some_and(|n| n == PROJECTS_DIR) =>
        {
            parent.and_then(Path::parent)
        }
        _ => None,
    };
    let Some(dir) = dir else { return Ok(()) };
    let old = dir.join(LEGACY_FILE);
    // Once `shared.db` is there the directory is split, and a `sessions.db` beside it is one an
    // old process made after the split: refusing on it would stop every new folder's daemon.
    if old.is_file() && !dir.join(SHARED_FILE).is_file() {
        return Err(StoreError::Refused(format!(
            "{} is not split yet: it holds {LEGACY_FILE}, so {} is not created beside it. Carve \
             one project at a time with `letibot-migrate-split --project KEY {} {}`, or split the \
             whole file with every daemon stopped.",
            dir.display(),
            path.display(),
            old.display(),
            dir.display()
        )));
    }
    Ok(())
}

/// The single file a daemon used to write, with nothing carved out of it yet: splitting it is the
/// operator's, because a daemon that started by copying would be copying a store nobody had
/// stopped writing to.
pub fn refuse_unsplit(dir: &Path) -> Result<()> {
    if dir.join(LEGACY_FILE).is_file() && !dir.join(SHARED_FILE).is_file() {
        return Err(unsplit(dir));
    }
    Ok(())
}

fn unsplit(dir: &Path) -> StoreError {
    StoreError::Refused(format!(
        "{} is not split yet: it holds {LEGACY_FILE} and no {SHARED_FILE}. Carve one project at a \
         time with `letibot-migrate-split --project KEY {} {}`, or split the whole file with \
         every daemon stopped - this daemon never migrates on its own.",
        dir.display(),
        dir.join(LEGACY_FILE).display(),
        dir.display()
    ))
}

/// Every set this process has open, by data directory and project key. A `Weak`, like [`Store`]'s
/// own registry: the set lives exactly as long as something holds it.
type OpenSets = Mutex<HashMap<(PathBuf, String), Weak<StoreSet>>>;

fn sets() -> &'static OpenSets {
    static OPEN: std::sync::OnceLock<OpenSets> = std::sync::OnceLock::new();
    OPEN.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A directory with its symlinks resolved, so two spellings of one data directory meet. A directory
/// that does not exist yet keeps its own spelling, and `StoreSet::open` has just made it.
fn canonical(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// A key names a file under `projects/`, so it must be one: the hex of the folder's path hash.
fn check_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(StoreError::Refused(format!(
            "'{key}' is not a project key: the hex characters of the folder's path hash"
        )));
    }
    Ok(())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "a store".into())
}

/// Create `path`'s schema for `file`, or leave a file that already has one: the version stamped
/// here is what `Store::open` reads to see there is nothing to migrate.
fn prepare(conn: &Connection, path: &Path, file: StoreFile) -> Result<()> {
    let Some(found) = stored_version(conn)? else {
        conn.execute_batch(&schema_sql(file))?;
        conn.execute("DELETE FROM schema_version", [])?;
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            params![SCHEMA_VERSION],
        )?;
        return Ok(());
    };
    // A newer file under a floor this build meets is `Store::open`'s to take, as for one file.
    if found > SCHEMA_VERSION
        && !matches!(super::floor::read_floor(conn, "main")?, Some(f) if f <= SCHEMA_VERSION)
    {
        return Err(too_new(path, found));
    }
    // Older: `Store::open`'s own migration carries it forward, the rule every store file has.
    Ok(())
}

/// What `schema_version` says, or `None` for a file that has no schema yet. Only a missing table is
/// a fresh file: reading every failure as one would stamp a version over a file it could not read.
fn stored_version(conn: &Connection) -> Result<Option<i64>> {
    match conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .optional()
    {
        Ok(found) => Ok(found),
        Err(e) if e.to_string().contains("no such table") => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// A file written by a build newer than this one, refused BY NAME: a daemon that will not start owes
/// the person reading the line the file and both numbers.
fn too_new(path: &Path, found: i64) -> StoreError {
    StoreError::Refused(format!(
        "{}: store schema is version {found}; this build knows {SCHEMA_VERSION} — restart from a \
         build at least that new",
        path.display()
    ))
}
