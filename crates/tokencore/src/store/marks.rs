//! What one session's requests have learned about pictures: the bytes a provider refused, and the
//! models that take text only. `todo`'s class - mutable metadata about a conversation, riding no
//! chain - and on disk so a restart does not earn the same 400 again.

use rusqlite::{OptionalExtension, params};

use super::{Result, Store};

pub const MARKS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS session_marks (
    session_id  TEXT PRIMARY KEY REFERENCES session(id) ON DELETE CASCADE,
    marks_json  TEXT NOT NULL,   -- the provider crate's own shape; the store keeps it opaque
    updated_ms  INTEGER NOT NULL
);
"#;

impl Store {
    /// The marks this session's row carries, as the text the provider crate wrote, or `None` for a
    /// session that has learned nothing yet.
    pub fn session_marks(&self, session_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT marks_json FROM session_marks WHERE session_id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Replace this session's marks whole. One row per session rather than a row per mark, because
    /// the set is small, bounded and always written as what it is now.
    pub fn set_session_marks(&self, session_id: &str, marks_json: &str) -> Result<()> {
        self.exec(
            "INSERT INTO session_marks (session_id, marks_json, updated_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET marks_json = ?2, updated_ms = ?3",
            params![session_id, marks_json, super::now_ms()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SessionRecord;

    fn a_store_with_one_session() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.put_session(&SessionRecord {
            id: "sess-marks".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: "/w".into(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();
        s
    }

    /// The marks go with the session and come back as they were written, replaced whole on the
    /// second write: a set half-old and half-new would withhold a picture the retry has since
    /// learned to send.
    #[test]
    fn a_sessions_marks_are_written_replaced_and_read_back() {
        let s = a_store_with_one_session();
        assert_eq!(s.session_marks("sess-marks").unwrap(), None);
        s.set_session_marks("sess-marks", r#"{"pictures":[["k","no"]],"models":[]}"#)
            .unwrap();
        assert_eq!(
            s.session_marks("sess-marks").unwrap().as_deref(),
            Some(r#"{"pictures":[["k","no"]],"models":[]}"#)
        );
        s.set_session_marks("sess-marks", "{}").unwrap();
        assert_eq!(
            s.session_marks("sess-marks").unwrap().as_deref(),
            Some("{}")
        );
        assert_eq!(s.session_marks("another-session").unwrap(), None);
    }
}
