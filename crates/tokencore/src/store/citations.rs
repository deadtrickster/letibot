//! **The lines a reply cited, as they were when it was written** (schema 30).
//!
//! A model citing code writes a fence labelled `path:start-end` with nothing in it, and the daemon
//! fills it from the file (`letibot_harnessd::citations`). What it read is kept here rather than in
//! the reply: rewriting a message the model already generated would make the next request's
//! prefix differ from the cached one from that message on. The rows are keyed by the reply's own
//! text, not by its row id, because a fork (`/reseat`, a compaction) carries the reply under a new
//! id and the citation must come with it — what was cited is a fact about the words, not the row.

use rusqlite::{OptionalExtension, params};

use super::{Result, Store};

pub const CITATIONS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS citation (
    session_id  TEXT NOT NULL,
    text_hash   TEXT NOT NULL,   -- of the reply's text, so a carried reply finds its snapshot
    json        TEXT NOT NULL,   -- the citations, as the daemon's event carries them
    at_ms       INTEGER NOT NULL,
    PRIMARY KEY (session_id, text_hash)
);
"#;

impl Store {
    /// The citations stored for a reply, as the JSON they were written with.
    pub fn citations(&self, session_id: &str, text_hash: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT json FROM citation WHERE session_id = ?1 AND text_hash = ?2",
                params![session_id, text_hash],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// **Keep a reply's citations** — the first snapshot wins: a reply is cited once, when it
    /// lands, and a later write for the same words is the same reply carried, not a new reading.
    pub fn put_citations(
        &self,
        session_id: &str,
        text_hash: &str,
        json: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.exec(
            "INSERT OR IGNORE INTO citation (session_id, text_hash, json, at_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, text_hash, json, now_ms as i64],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replys_citations_round_trip_and_the_first_snapshot_wins() {
        let dir = std::env::temp_dir().join(format!("letibot-cite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s = Store::open(&dir.join("s.db")).unwrap();
        assert_eq!(s.citations("s1", "h").unwrap(), None);
        s.put_citations("s1", "h", "[1]", 1).unwrap();
        s.put_citations("s1", "h", "[2]", 2).unwrap();
        assert_eq!(s.citations("s1", "h").unwrap().as_deref(), Some("[1]"));
        assert_eq!(s.citations("s2", "h").unwrap(), None, "another session's");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
