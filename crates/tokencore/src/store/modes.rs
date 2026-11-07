//! Where each project sits, and where each session does: the permission modes. A write is one
//! row. `project_mode` is where a new session opens; `session_mode` is where this one was put,
//! and comes back with it on a restart.

use rusqlite::{OptionalExtension, params};

use super::{Result, Store, now_ms};

pub const MODES_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS project_mode (
    root         TEXT PRIMARY KEY,   -- the project root, canonical
    mode         TEXT NOT NULL,      -- the point's name
    consented_at TEXT,               -- the operator's answer for `allow-all`; NULL = none
    set_at       INTEGER NOT NULL    -- unix ms
);
CREATE TABLE IF NOT EXISTS mode_import (
    source       TEXT PRIMARY KEY,   -- the legacy file imported, so it is imported once
    at           INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS queue_mute (
    workspace    TEXT NOT NULL,      -- the session folder, canonical
    repository   TEXT NOT NULL,      -- a repository's directory name under it, or its key
    set_at       INTEGER NOT NULL,
    PRIMARY KEY (workspace, repository)
);
CREATE TABLE IF NOT EXISTS session_mode (
    session_id   TEXT PRIMARY KEY,
    mode         TEXT NOT NULL,
    consented_at TEXT,
    set_at       INTEGER NOT NULL
);
"#;

/// One project's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMode {
    pub root: String,
    pub mode: String,
    pub consented_at: Option<String>,
}

impl Store {
    /// Every project's row, by root.
    pub fn project_modes(&self) -> Result<Vec<ProjectMode>> {
        let conn = self.conn();
        let mut st =
            conn.prepare("SELECT root, mode, consented_at FROM project_mode ORDER BY root")?;
        let rows = st
            .query_map([], |r| {
                Ok(ProjectMode {
                    root: r.get(0)?,
                    mode: r.get(1)?,
                    consented_at: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Put a project at a point, with the operator's answer or none.
    pub fn set_project_mode(&self, root: &str, mode: &str, consented: Option<&str>) -> Result<()> {
        self.exec(
            "INSERT INTO project_mode (root, mode, consented_at, set_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(root) DO UPDATE SET mode = ?2, consented_at = ?3, set_at = ?4",
            params![root, mode, consented, now_ms()],
        )?;
        Ok(())
    }

    /// Drop a project's row. Whether there was one.
    pub fn remove_project_mode(&self, root: &str) -> Result<bool> {
        Ok(self.exec("DELETE FROM project_mode WHERE root = ?1", params![root])? > 0)
    }

    /// Whether the legacy file at `source` was imported into this store already.
    pub fn mode_imported(&self, source: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM mode_import WHERE source = ?1",
                params![source],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Record that the legacy file at `source` was imported.
    pub fn mark_mode_imported(&self, source: &str) -> Result<()> {
        self.exec(
            "INSERT OR IGNORE INTO mode_import (source, at) VALUES (?1, ?2)",
            params![source, now_ms()],
        )?;
        Ok(())
    }

    /// The point this session was put at, and the answer that opened it, if it was put anywhere.
    pub fn session_mode(&self, session_id: &str) -> Result<Option<(String, Option<String>)>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT mode, consented_at FROM session_mode WHERE session_id = ?1",
                params![session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// The repositories a workspace's sessions are not told the gate's news of.
    pub fn queue_mutes(&self, workspace: &str) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT repository FROM queue_mute WHERE workspace = ?1 ORDER BY repository",
        )?;
        let rows = st
            .query_map(params![workspace], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    /// Mute (`true`) or listen to (`false`) one repository of a workspace.
    pub fn set_queue_mute(&self, workspace: &str, repository: &str, muted: bool) -> Result<()> {
        if muted {
            self.exec(
                "INSERT OR IGNORE INTO queue_mute (workspace, repository, set_at) VALUES (?1, ?2, ?3)",
                params![workspace, repository, now_ms()],
            )?;
        } else {
            self.exec(
                "DELETE FROM queue_mute WHERE workspace = ?1 AND repository = ?2",
                params![workspace, repository],
            )?;
        }
        Ok(())
    }

    /// Forget every mute of a workspace.
    pub fn clear_queue_mutes(&self, workspace: &str) -> Result<()> {
        self.exec(
            "DELETE FROM queue_mute WHERE workspace = ?1",
            params![workspace],
        )?;
        Ok(())
    }

    /// Record the point this session was put at.
    pub fn set_session_mode(
        &self,
        session_id: &str,
        mode: &str,
        consented: Option<&str>,
    ) -> Result<()> {
        self.exec(
            "INSERT INTO session_mode (session_id, mode, consented_at, set_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id) DO UPDATE SET mode = ?2, consented_at = ?3, set_at = ?4",
            params![session_id, mode, consented, now_ms()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().expect("store")
    }

    #[test]
    fn a_project_row_is_one_row_and_writes_do_not_touch_others() {
        let s = store();
        s.set_project_mode("/p/peerdb", "allow-all", Some("2026-10-12T07:00:00Z"))
            .unwrap();
        s.set_project_mode("/p/letim", "allow-all", None).unwrap();
        s.set_project_mode("/p/letim", "writes-allowed", None)
            .unwrap();
        let rows = s.project_modes().unwrap();
        assert_eq!(rows.len(), 2, "{rows:?}");
        let peerdb = rows.iter().find(|r| r.root == "/p/peerdb").unwrap();
        assert_eq!(peerdb.consented_at.as_deref(), Some("2026-10-12T07:00:00Z"));
        assert!(s.remove_project_mode("/p/letim").unwrap());
        assert!(!s.remove_project_mode("/p/letim").unwrap());
    }

    #[test]
    fn queue_mutes_are_per_workspace_and_set_clear_idempotently() {
        let s = store();
        s.set_queue_mute("/w/a", "x", true).unwrap();
        s.set_queue_mute("/w/a", "x", true).unwrap();
        s.set_queue_mute("/w/a", "y", true).unwrap();
        s.set_queue_mute("/w/b", "x", true).unwrap();
        assert_eq!(s.queue_mutes("/w/a").unwrap(), ["x", "y"]);
        s.set_queue_mute("/w/a", "x", false).unwrap();
        assert_eq!(s.queue_mutes("/w/a").unwrap(), ["y"]);
        s.clear_queue_mutes("/w/a").unwrap();
        assert!(s.queue_mutes("/w/a").unwrap().is_empty());
        assert_eq!(s.queue_mutes("/w/b").unwrap(), ["x"]);
    }

    #[test]
    fn a_session_keeps_its_own_point() {
        let s = store();
        assert_eq!(s.session_mode("s-1").unwrap(), None);
        s.set_session_mode("s-1", "allow-all", Some("t")).unwrap();
        s.set_session_mode("s-1", "allow-all", Some("t2")).unwrap();
        assert_eq!(
            s.session_mode("s-1").unwrap(),
            Some(("allow-all".into(), Some("t2".into())))
        );
    }
}
