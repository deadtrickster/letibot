//! A process in the world, and how to say it is still the same one: where it runs, what makes a
//! reused pid not it, which group a teardown signals, when it must be gone, and how it was last
//! looked at, including a process a restarted daemon adopted.

use rusqlite::{OptionalExtension, params};

use super::{Result, Store, now_ms};

/// The table both a fresh store and the v34 migration create.
pub const PROCS_SQL: &str = r#"
-- **Every process letibot started, as the supervisor sees it** (schema 34). Written by
-- `harnessd`'s supervisor and by nothing else; the `job` table beside it stays the pane's
-- history of a handle, and this is the record a liveness check, a deadline, a teardown and a
-- restart all read. See `crates/tokencore/src/store/procs.rs` for why they are two.
CREATE TABLE IF NOT EXISTS process (
    session_id    TEXT NOT NULL,
    handle        TEXT NOT NULL,
    -- NULL = this box; otherwise a host from hosts.toml
    host          TEXT,
    pid           INTEGER NOT NULL,
    -- the group a teardown signals; NULL when the pid does not lead one
    pgid          INTEGER,
    -- what makes a reused pid NOT this process: /proc start time, `ps -o lstart`, or the
    -- remote job directory's pid and command line. Never empty.
    identity      TEXT NOT NULL,
    command       TEXT NOT NULL,
    cwd           TEXT NOT NULL,
    -- the scope a teardown names as its owner, for a reader of the record
    scope         TEXT NOT NULL,
    started_ms    INTEGER NOT NULL,
    -- a wall clock, so a deadline outlives the process that armed it
    deadline_ms   INTEGER,
    -- starting | running | done | stopped | dead | lost
    state         TEXT NOT NULL,
    -- the exit status, for `done`; NULL for every other state and for a signal
    exit          INTEGER,
    -- where the whole of its output lives, when it lives in a file
    log_path      TEXT,
    -- how much of that file has been read back so far
    log_read      INTEGER NOT NULL DEFAULT 0,
    -- the last liveness check: when, how long it took, and what it answered
    check_ms      INTEGER,
    check_took_ms INTEGER,
    check_how     TEXT,
    PRIMARY KEY (session_id, handle)
);
-- The supervisor's one query is "everything still open", and its teardown's is
-- "everything this session owns"; both are this index.
CREATE INDEX IF NOT EXISTS process_open_idx ON process (state);
CREATE INDEX IF NOT EXISTS process_session_idx ON process (session_id, state);
"#;

/// Where a supervised process stands - six states, and see the module header for why the last
/// three are not interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcState {
    /// Launched, and not yet told apart from a launch that failed.
    Starting,
    /// The last check found the same process.
    Running,
    /// It exited, and the record's [`exit`](ProcessRecord::exit) is its status.
    Done,
    /// Something ended it on purpose.
    Stopped,
    /// Gone, with no exit recorded.
    Dead,
    /// The host could not be reached to say - the process may still run.
    Lost,
}

impl ProcState {
    /// The word the row holds and every reader spells.
    pub fn word(self) -> &'static str {
        match self {
            ProcState::Starting => "starting",
            ProcState::Running => "running",
            ProcState::Done => "done",
            ProcState::Stopped => "stopped",
            ProcState::Dead => "dead",
            ProcState::Lost => "lost",
        }
    }

    /// The row's word back into the value. `None` for a word this build does not know, which is a
    /// row written by a newer build rather than corruption.
    pub fn of(word: &str) -> Option<ProcState> {
        Some(match word {
            "starting" => ProcState::Starting,
            "running" => ProcState::Running,
            "done" => ProcState::Done,
            "stopped" => ProcState::Stopped,
            "dead" => ProcState::Dead,
            "lost" => ProcState::Lost,
            _ => return None,
        })
    }

    /// Is this a process the supervisor still has to look after? `lost` is - the job may
    /// still be running - and that is the whole point of the state.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            ProcState::Starting | ProcState::Running | ProcState::Lost
        )
    }

    /// Did it end, whatever the ending was? The complement of [`ProcState::is_open`] once a
    /// check has said so.
    pub fn is_ended(self) -> bool {
        !self.is_open()
    }
}

/// One supervised process, as the store holds it. See the module header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRecord {
    /// The session that owns it: what a teardown of that session kills, and what a restart's
    /// re-attachment re-arms.
    pub session: String,
    /// The handle the model knows it by - `j65`.
    pub handle: String,
    /// `None` for this box, a host from `hosts.toml` otherwise. The unit of a liveness check
    /// is this field, never the job: one check per host per tick.
    pub host: Option<String>,
    pub pid: u32,
    /// The process group a teardown signals, when the pid leads one - asked rather than assumed,
    /// because `kill -TERM -<pid>` on a pid that is not a leader signals *the caller's* group.
    pub pgid: Option<u32>,
    /// What makes a reused pid not this process. See the module header.
    pub identity: String,
    pub command: String,
    pub cwd: String,
    /// The scope whose end reaps it, as a name - `session.s12`, `turn.t3`, or an explicit one.
    pub scope: String,
    pub started_ms: i64,
    /// When it must be gone, as a wall clock. `None` = nobody asked.
    pub deadline_ms: Option<i64>,
    pub state: ProcState,
    /// The exit status. Only `done` has one: a signal and a vanishing are not exit codes, and a
    /// reader that averaged them would be reporting a failure a command never gave.
    pub exit: Option<i32>,
    /// Where the whole of its output is going, when it goes to a file - the remote job directory's
    /// `<id>.out`, or a promoted run's spill.
    pub log_path: Option<String>,
    pub log_read: u64,
    /// The last check: when, how long it took, and what it answered. `check_how` is a short
    /// word - `proc`, `ps`, `ssh`, `jobdir`, `expired` - never a payload.
    pub check_ms: Option<i64>,
    pub check_took_ms: Option<u64>,
    pub check_how: Option<String>,
}

impl ProcessRecord {
    /// A record for a process that has just been launched, with the check fields unset - the
    /// honest state of a process nobody has looked at yet.
    pub fn starting(
        session: impl Into<String>,
        handle: impl Into<String>,
        host: Option<String>,
        pid: u32,
    ) -> ProcessRecord {
        ProcessRecord {
            session: session.into(),
            handle: handle.into(),
            host,
            pid,
            pgid: None,
            identity: String::new(),
            command: String::new(),
            cwd: String::new(),
            scope: String::new(),
            started_ms: now_ms(),
            deadline_ms: None,
            state: ProcState::Starting,
            exit: None,
            log_path: None,
            log_read: 0,
            check_ms: None,
            check_took_ms: None,
            check_how: None,
        }
    }

    /// The line a reader is shown for this record, e.g. `j65 on lab2x1 · running`. One
    /// spelling, because the activity, the pane and a notice all name the same record.
    pub fn label(&self) -> String {
        match &self.host {
            Some(h) => format!("{} on {}", self.handle, h),
            None => self.handle.clone(),
        }
    }
}

/// What one check of one process answered - the three fields a check writes, in one value so
/// a caller cannot set the outcome and forget the clock that dates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub state: ProcState,
    /// The exit status, when this check is the one that saw it end.
    pub exit: Option<i32>,
    /// When the check ran.
    pub at_ms: i64,
    /// How long it took.
    pub took_ms: u64,
    /// A short word for how it was answered - see [`ProcessRecord::check_how`].
    pub how: String,
}

impl Check {
    pub fn new(state: ProcState, at_ms: i64, took_ms: u64, how: impl Into<String>) -> Check {
        Check {
            state,
            exit: None,
            at_ms,
            took_ms,
            how: how.into(),
        }
    }

    pub fn with_exit(mut self, exit: i32) -> Check {
        self.exit = Some(exit);
        self
    }
}

const COLUMNS: &str = "session_id, handle, host, pid, pgid, identity, command, cwd, scope, \
                       started_ms, deadline_ms, state, exit, log_path, log_read, check_ms, \
                       check_took_ms, check_how";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ProcessRecord> {
    let state: String = r.get(11)?;
    Ok(ProcessRecord {
        session: r.get(0)?,
        handle: r.get(1)?,
        host: r.get(2)?,
        pid: r.get::<_, i64>(3)? as u32,
        pgid: r.get::<_, Option<i64>>(4)?.map(|v| v as u32),
        identity: r.get(5)?,
        command: r.get(6)?,
        cwd: r.get(7)?,
        scope: r.get(8)?,
        started_ms: r.get(9)?,
        deadline_ms: r.get(10)?,
        // A word this build does not know reads as `lost`, which is the conservative answer and
        // the one that keeps the supervisor looking: `done` would settle a job that may be
        // running, and a row the daemon cannot read is not evidence that anything ended.
        state: ProcState::of(&state).unwrap_or(ProcState::Lost),
        exit: r.get::<_, Option<i64>>(12)?.map(|v| v as i32),
        log_path: r.get(13)?,
        log_read: r.get::<_, i64>(14)? as u64,
        check_ms: r.get(15)?,
        check_took_ms: r.get::<_, Option<i64>>(16)?.map(|v| v as u64),
        check_how: r.get(17)?,
    })
}

impl Store {
    /// Write one process's whole record, replacing that handle's last one. An upsert rather than
    /// an append, for `put_job`'s reason: a process has one state at a time.
    pub fn put_process(&self, p: &ProcessRecord) -> Result<()> {
        // `?`, and not a swallowed `let _ =`: a record that did not land is a process nobody will
        // check, so the failure travels to the caller, which says it on the session.
        self.exec(
            "INSERT INTO process (session_id, handle, host, pid, pgid, identity, command, cwd,
                                  scope, started_ms, deadline_ms, state, exit, log_path, log_read,
                                  check_ms, check_took_ms, check_how)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(session_id, handle) DO UPDATE SET
               host = excluded.host, pid = excluded.pid, pgid = excluded.pgid,
               identity = excluded.identity, command = excluded.command, cwd = excluded.cwd,
               scope = excluded.scope, started_ms = excluded.started_ms,
               deadline_ms = excluded.deadline_ms, state = excluded.state, exit = excluded.exit,
               log_path = excluded.log_path, log_read = excluded.log_read,
               check_ms = excluded.check_ms, check_took_ms = excluded.check_took_ms,
               check_how = excluded.check_how",
            params![
                p.session,
                p.handle,
                p.host,
                p.pid as i64,
                p.pgid.map(|v| v as i64),
                p.identity,
                p.command,
                p.cwd,
                p.scope,
                p.started_ms,
                p.deadline_ms,
                p.state.word(),
                p.exit.map(|v| v as i64),
                p.log_path,
                p.log_read as i64,
                p.check_ms,
                p.check_took_ms.map(|v| v as i64),
                p.check_how,
            ],
        )?;
        Ok(())
    }

    /// Every process still open, across every session and host: the supervisor's first read after
    /// a restart and its work list on each tick.
    pub fn open_processes(&self) -> Result<Vec<ProcessRecord>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM process WHERE state IN ('starting', 'running', 'lost')
              ORDER BY host IS NOT NULL, host, session_id, handle"
        ))?;
        let rows = st.query_map([], row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Every process this session owns, open or ended, oldest first - what a teardown reads
    /// and what a test asserts against.
    pub fn processes_of(&self, session_id: &str) -> Result<Vec<ProcessRecord>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM process WHERE session_id = ?1 ORDER BY started_ms, handle"
        ))?;
        let rows = st.query_map(params![session_id], row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// One record, by its two keys. `None` for a handle this store never supervised.
    pub fn process(&self, session_id: &str, handle: &str) -> Result<Option<ProcessRecord>> {
        Ok(self
            .conn()
            .prepare(&format!(
                "SELECT {COLUMNS} FROM process WHERE session_id = ?1 AND handle = ?2"
            ))?
            .query_row(params![session_id, handle], row)
            .optional()?)
    }

    /// Write what one check answered: the state, the exit when this check saw one, and the clock
    /// that dates it. One statement, so a record cannot carry a fresh state and a stale clock.
    pub fn check_process(&self, session_id: &str, handle: &str, c: &Check) -> Result<()> {
        self.exec(
            "UPDATE process SET state = ?3, exit = COALESCE(?4, exit), check_ms = ?5,
                                check_took_ms = ?6, check_how = ?7
               WHERE session_id = ?1 AND handle = ?2",
            params![
                session_id,
                handle,
                c.state.word(),
                c.exit.map(|v| v as i64),
                c.at_ms,
                c.took_ms as i64,
                c.how
            ],
        )?;
        Ok(())
    }

    /// How much of its log has been read back, so a reader that comes back after a restart
    /// resumes rather than re-reading a file from the top.
    pub fn set_process_log_read(&self, session_id: &str, handle: &str, bytes: u64) -> Result<()> {
        self.exec(
            "UPDATE process SET log_read = ?3 WHERE session_id = ?1 AND handle = ?2",
            params![session_id, handle, bytes as i64],
        )?;
        Ok(())
    }
}
