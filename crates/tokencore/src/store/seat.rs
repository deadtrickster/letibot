//! A Claude Code seat's todo board (`docs/seat-todo.md`). `todo`'s class - mutable metadata about a
//! conversation, replaced whole, riding no chain - with a `seat` where that table has a
//! `session_id`: the seat is not a session, so this table has no foreign key and no cascade.

use rusqlite::{OptionalExtension, params};

use super::{Result, Store, TodoItem, now_ms};

pub const SEAT_TODO_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS seat_todo (
    seat        TEXT PRIMARY KEY,
    todos_json  TEXT NOT NULL,   -- JSON array of {content, status, by, needs}
    updated_ms  INTEGER NOT NULL
);
"#;

/// A seat's own background jobs (`seat_job_start`): `job`'s class with a `seat` where that table
/// has a `session_id`, and for the same reason - a seat is not a session, so there is no row for
/// the foreign key to point at. One row per handle, UPSERTED: a job has one state at a time, and
/// the settlement is the same job as the start. `start` is `/proc/<pid>/stat`'s `starttime`, the
/// half of a pid that survives reuse.
pub const SEAT_JOB_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS seat_job (
    seat       TEXT NOT NULL,
    handle     TEXT NOT NULL,
    command    TEXT NOT NULL,
    cwd        TEXT NOT NULL,
    name       TEXT NOT NULL DEFAULT '',
    pid        INTEGER NOT NULL DEFAULT 0,
    start      INTEGER NOT NULL DEFAULT 0,
    log        TEXT NOT NULL DEFAULT '',
    state      TEXT NOT NULL,
    started_ms INTEGER NOT NULL,
    ended_ms   INTEGER NOT NULL DEFAULT 0,
    elapsed_ms INTEGER NOT NULL DEFAULT 0,
    produced   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (seat, handle)
);
"#;

/// One job a seat started, as the store keeps it. `state` is the listing's own word - `running`
/// until the process ends, then what it ended with (`exited 0`, `killed`, `lost`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SeatJob {
    pub handle: String,
    /// The argv, as one line, for a reader: the row the seat asked for.
    pub command: String,
    pub cwd: String,
    /// The short name the seat gave it, or nothing.
    pub name: String,
    pub pid: u32,
    /// `/proc/<pid>/stat`'s `starttime`: a pid with another start time is another process.
    pub start: u64,
    /// Where its output went.
    pub log: String,
    pub state: String,
    pub started_ms: i64,
    /// When it ended, and how long it ran - zero while it has not.
    pub ended_ms: i64,
    pub elapsed_ms: u64,
    /// Bytes it wrote, read at settlement.
    pub produced: u64,
}

impl Store {
    /// The seat's board as it was last written - empty for a seat that never wrote one, which is
    /// the same state as a board it left empty.
    pub fn seat_todos(&self, seat: &str) -> Result<Vec<TodoItem>> {
        let json: Option<String> = self
            .conn()
            .query_row(
                "SELECT todos_json FROM seat_todo WHERE seat = ?1",
                params![seat],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json
            .map(|j| serde_json::from_str(&j))
            .transpose()?
            .unwrap_or_default())
    }

    /// Replace the seat's board whole, for `put_todos`' reason: a board is what it is now, not a
    /// sequence of edits.
    pub fn put_seat_todos(&self, seat: &str, todos: &[TodoItem]) -> Result<()> {
        self.exec(
            "INSERT INTO seat_todo (seat, todos_json, updated_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(seat) DO UPDATE SET todos_json = ?2, updated_ms = ?3",
            params![seat, serde_json::to_string(todos)?, now_ms()],
        )?;
        Ok(())
    }

    /// Every seat's board, by handle, for a head's pane: a seat is not a session, so this is the
    /// only read that finds one. A row per seat, in the handles' own order, each with its edges.
    pub fn seat_boards(&self) -> Result<Vec<(String, Vec<TodoItem>)>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT seat, todos_json FROM seat_todo ORDER BY seat")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (seat, json) = row?;
            out.push((seat, serde_json::from_str(&json)?));
        }
        Ok(out)
    }

    /// Every job this seat has recorded, oldest first. A `running` row of a bridge that died with
    /// the job still up is kept as it was written: the store says what happened, and the process
    /// table says whether it still runs.
    pub fn seat_jobs(&self, seat: &str) -> Result<Vec<SeatJob>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT handle, command, cwd, name, pid, start, log, state, started_ms, ended_ms,
                    elapsed_ms, produced
               FROM seat_job WHERE seat = ?1 ORDER BY started_ms, handle",
        )?;
        let rows = st.query_map(params![seat], |r| {
            Ok(SeatJob {
                handle: r.get(0)?,
                command: r.get(1)?,
                cwd: r.get(2)?,
                name: r.get(3)?,
                pid: r.get::<_, i64>(4)? as u32,
                start: r.get::<_, i64>(5)? as u64,
                log: r.get(6)?,
                state: r.get(7)?,
                started_ms: r.get(8)?,
                ended_ms: r.get(9)?,
                elapsed_ms: r.get::<_, i64>(10)? as u64,
                produced: r.get::<_, i64>(11)? as u64,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Write one job's row, replacing that handle's last one: [`Store::put_job`]'s contract, one
    /// table over.
    pub fn put_seat_job(&self, seat: &str, job: &SeatJob) -> Result<()> {
        self.exec(
            "INSERT INTO seat_job
               (seat, handle, command, cwd, name, pid, start, log, state, started_ms, ended_ms,
                elapsed_ms, produced)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(seat, handle) DO UPDATE SET
               command = ?3, cwd = ?4, name = ?5, pid = ?6, start = ?7, log = ?8, state = ?9,
               started_ms = ?10, ended_ms = ?11, elapsed_ms = ?12, produced = ?13",
            params![
                seat,
                job.handle,
                job.command,
                job.cwd,
                job.name,
                job.pid as i64,
                job.start as i64,
                job.log,
                job.state,
                job.started_ms,
                job.ended_ms,
                job.elapsed_ms as i64,
                job.produced as i64
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{TodoBy, TodoNeed, TodoStatus};

    fn row(content: &str) -> TodoItem {
        TodoItem {
            content: content.into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Model,
            when: None,
            needs: vec![TodoNeed::Child {
                id: "s-package".into(),
            }],
        }
    }

    /// A seat's rows go in and come back with their edges, replaced whole: the wait on a package is
    /// half of what a board IS, and a restart that lost it would silent the row.
    #[test]
    fn a_seats_rows_are_written_replaced_and_read_back() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.seat_todos("cc@proj").unwrap().is_empty());
        let row = row("collect the package");
        s.put_seat_todos("cc@proj", std::slice::from_ref(&row))
            .unwrap();
        assert_eq!(s.seat_todos("cc@proj").unwrap(), vec![row.clone()]);
        assert!(
            s.seat_todos("cc@other").unwrap().is_empty(),
            "a seat's board is its own"
        );
        // Two seats, two boards: the primary key is the handle and not one shared row.
        s.put_seat_todos("cc@other", std::slice::from_ref(&row))
            .unwrap();
        assert_eq!(s.seat_todos("cc@proj").unwrap().len(), 1);
        s.put_seat_todos("cc@proj", &[]).unwrap();
        assert!(s.seat_todos("cc@proj").unwrap().is_empty());
        assert_eq!(
            s.seat_todos("cc@other").unwrap().len(),
            1,
            "one seat's write never reaches another's board"
        );
    }

    /// Every seat's board comes back by handle, so a head that asks once draws all of them: the
    /// board the seat wrote, with its edges, and nothing invented for a seat that wrote none.
    #[test]
    fn the_seats_boards_are_listed_by_handle() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.seat_boards().unwrap().is_empty(), "no seat, no board");
        s.put_seat_todos("cc@proj", std::slice::from_ref(&row("collect")))
            .unwrap();
        s.put_seat_todos("cc@other", std::slice::from_ref(&row("wait for CI")))
            .unwrap();
        let boards = s.seat_boards().unwrap();
        let names: Vec<&str> = boards.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(names, vec!["cc@other", "cc@proj"], "one row per seat");
        assert_eq!(
            boards[1].1,
            vec![row("collect")],
            "the rows keep the wait on the package"
        );
    }

    /// A store from before v41 gains the table on its next open, and a seat's rows written into it
    /// are read back by another connection: the restart a bridge takes between two MCP runs.
    #[test]
    fn a_v40_store_gains_the_seat_table_and_keeps_a_seats_rows() {
        let path = std::env::temp_dir().join(format!(
            "letibot-seat-todo-{}-{}.db",
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
            s.put_seat_todos("cc@proj", &[row("keep me")]).unwrap();
        }
        {
            // Back to a store that predates the table, the way one on disk from an older bridge is.
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute("DROP TABLE seat_todo", []).unwrap();
            c.execute("UPDATE schema_version SET version = 40", [])
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert!(
            s.seat_todos("cc@proj").unwrap().is_empty(),
            "the table came back, empty"
        );
        s.put_seat_todos("cc@proj", &[row("keep me")]).unwrap();
        let next_bridge = Store::open_private(&path).unwrap();
        assert_eq!(
            next_bridge.seat_todos("cc@proj").unwrap(),
            vec![row("keep me")],
            "a connection of its own - the next bridge - reads what was written"
        );
    }

    fn job(handle: &str, state: &str) -> SeatJob {
        SeatJob {
            handle: handle.into(),
            command: "scripts/correctness_gate.sh --ratchet 0".into(),
            cwd: "/w".into(),
            name: "gate".into(),
            pid: 4242,
            start: 991177,
            log: "/run/letibot/gate.log".into(),
            state: state.into(),
            started_ms: 1_700_000_000_000,
            ended_ms: 0,
            elapsed_ms: 0,
            produced: 0,
        }
    }

    /// A seat's own jobs are written and read back, and a settlement is the same row moved: one
    /// state at a time, the pid's start time kept beside it so a reused pid is not this job.
    #[test]
    fn a_seats_jobs_are_written_and_their_settlement_is_the_same_row() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.seat_jobs("cc@proj").unwrap().is_empty());
        s.put_seat_job("cc@proj", &job("j9", "running")).unwrap();
        assert_eq!(s.seat_jobs("cc@proj").unwrap(), vec![job("j9", "running")]);
        assert!(
            s.seat_jobs("cc@other").unwrap().is_empty(),
            "a seat's jobs are its own"
        );
        let mut ended = job("j9", "exited 0");
        ended.ended_ms = 1_700_000_000_500;
        ended.elapsed_ms = 500;
        ended.produced = 42;
        s.put_seat_job("cc@proj", &ended).unwrap();
        assert_eq!(
            s.seat_jobs("cc@proj").unwrap(),
            vec![ended],
            "one row per handle, moved - not a second job"
        );
    }

    /// A v41 store - one from the build before this table - gains it on its next open, and the
    /// next bridge reads what was written into it.
    #[test]
    fn a_v41_store_gains_the_seat_job_table() {
        let path = std::env::temp_dir().join(format!(
            "letibot-seat-job-{}-{}.db",
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
            s.put_seat_job("cc@proj", &job("j9", "running")).unwrap();
        }
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute("DROP TABLE seat_job", []).unwrap();
            c.execute("UPDATE schema_version SET version = 41", [])
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert!(
            s.seat_jobs("cc@proj").unwrap().is_empty(),
            "the table came back, empty"
        );
        s.put_seat_job("cc@proj", &job("j9", "running")).unwrap();
        let next_bridge = Store::open_private(&path).unwrap();
        assert_eq!(
            next_bridge.seat_jobs("cc@proj").unwrap(),
            vec![job("j9", "running")],
            "a connection of its own - the next bridge - reads what was written"
        );
    }
}
