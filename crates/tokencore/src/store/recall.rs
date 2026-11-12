//! The recall index: one row per UNIT of a project's past conversation, and the tier's vector for
//! it when the box has an embedder. A unit is a person's turn and the reply it got, so the index is
//! searched by what was asked rather than by which row happened to contain a word.
//!
//! It lives in the project file, which is what makes "this project's past" the file's own question:
//! no key is filtered here, and a set's other folders are not in the way.

use rusqlite::params;

use super::{Result, Store};

pub const RECALL_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS recall_unit (
    unit        TEXT PRIMARY KEY,   -- `<session>#<seq>` of the unit's first human row
    session     TEXT NOT NULL,
    title       TEXT NOT NULL DEFAULT '',  -- the session's title, so a hint can name its source
    at_ms       INTEGER NOT NULL,          -- when the unit's human row was written
    text        TEXT NOT NULL,             -- the unit's words, bounded, what the score reads
    model       TEXT NOT NULL DEFAULT '',  -- the embedder that made `vector`; '' is none yet
    vector      BLOB,                      -- little-endian f32, `dim` of them, or NULL
    built_ms    INTEGER NOT NULL DEFAULT 0 -- when the vector was made
);
CREATE INDEX IF NOT EXISTS recall_unit_by_session ON recall_unit (session, at_ms);
"#;

/// One unit of the conversation the prompt no longer carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallUnit {
    pub unit: String,
    pub session: String,
    pub title: String,
    pub at_ms: i64,
    pub text: String,
}

impl Store {
    /// The newest units of this file's project, newest first. `limit` is the whole bound: a round
    /// scores what it is given, so this is where the index's cost is decided.
    pub fn recall_units(&self, limit: usize) -> Result<Vec<RecallUnit>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT unit, session, title, at_ms, text FROM recall_unit
              ORDER BY at_ms DESC, unit ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(RecallUnit {
                unit: r.get(0)?,
                session: r.get(1)?,
                title: r.get(2)?,
                at_ms: r.get(3)?,
                text: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Put units in the index, leaving a vector that is already there alone: the text of a unit
    /// never changes once its rows are written, so a re-read is the same row and not an edit.
    pub fn put_recall_units(&self, units: &[RecallUnit]) -> Result<()> {
        for u in units {
            self.exec(
                "INSERT INTO recall_unit (unit, session, title, at_ms, text)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(unit) DO UPDATE SET
                     title = excluded.title, at_ms = excluded.at_ms, text = excluded.text",
                params![u.unit, u.session, u.title, u.at_ms, u.text],
            )?;
        }
        Ok(())
    }

    /// The units this model has no vector for, newest first: what an index build still owes.
    pub fn recall_pending(&self, model: &str, limit: usize) -> Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT unit, text FROM recall_unit
              WHERE vector IS NULL OR model <> ?1
              ORDER BY at_ms DESC, unit ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![model, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// One session's transcripts, newest first: the reads the index makes to find what a prompt no
    /// longer carries. A fork keeps its parent's rows, so every transcript but the live one is
    /// out of the window and worth indexing.
    pub fn recall_transcripts(&self, session_id: &str, most: usize) -> Result<Vec<(String, i64)>> {
        let conn = self.at("transcript");
        let mut stmt = conn.prepare(
            "SELECT id, created_at FROM transcript WHERE session_id = ?1
              ORDER BY created_at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, most as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// One transcript's rows in `seq` order, as the unit builder reads them.
    pub fn recall_rows(
        &self,
        transcript_id: &str,
        most: usize,
    ) -> Result<Vec<(u32, String, String, i64)>> {
        let conn = self.at("transcript_item");
        let mut stmt = conn.prepare(
            "SELECT seq, kind, item_json, created_at FROM transcript_item
              WHERE transcript_id = ?1 ORDER BY seq ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![transcript_id, most as i64], |r| {
            Ok((
                r.get::<_, i64>(0)? as u32,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every unit's vector this model made, by unit id. A row for another model is left out: two
    /// models' vectors are two widths, and a score across them is not a score.
    pub fn recall_vectors(
        &self,
        model: &str,
    ) -> Result<std::collections::BTreeMap<String, Vec<f32>>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT unit, vector FROM recall_unit WHERE model = ?1 AND vector IS NOT NULL",
        )?;
        let rows = stmt.query_map([model], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = std::collections::BTreeMap::new();
        for row in rows {
            let (unit, bytes) = row?;
            out.insert(unit, from_bytes(&bytes));
        }
        Ok(out)
    }

    /// Record the vectors one build made, and say which model made them.
    pub fn put_recall_vectors(
        &self,
        model: &str,
        rows: &[(String, Vec<f32>)],
        now_ms: u64,
    ) -> Result<()> {
        for (unit, vector) in rows {
            self.exec(
                "UPDATE recall_unit SET model = ?2, vector = ?3, built_ms = ?4 WHERE unit = ?1",
                params![unit, model, to_bytes(vector), now_ms as i64],
            )?;
        }
        Ok(())
    }
}

/// A vector as bytes, little-endian and fixed-width: the store's BLOB is not a JSON array, and a
/// 384-wide vector spelled as text is four times the row for no reading anybody does.
fn to_bytes(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// The bytes back, with a trailing partial float dropped rather than panicking: a row truncated by
/// a crash is a shorter vector, and a shorter vector scores zero against everything.
fn from_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> (std::path::PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!("letibot-recall-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s = Store::open(&dir.join("s.db")).unwrap();
        (dir, s)
    }

    fn unit(id: &str, at: i64) -> RecallUnit {
        RecallUnit {
            unit: id.into(),
            session: "s1".into(),
            title: "the store split".into(),
            at_ms: at,
            text: format!("{id}: the etcd reference-format archives"),
        }
    }

    #[test]
    fn a_unit_round_trips_and_a_reread_keeps_its_vector() {
        let (dir, s) = store("rt");
        s.put_recall_units(&[unit("s1#4", 10)]).unwrap();
        s.put_recall_vectors("tiny", &[("s1#4".into(), vec![0.5, -0.25])], 11)
            .unwrap();
        // The same unit read again from the transcript: the text wins, the vector stays.
        s.put_recall_units(&[unit("s1#4", 10)]).unwrap();
        assert_eq!(s.recall_units(10).unwrap(), vec![unit("s1#4", 10)]);
        let vectors = s.recall_vectors("tiny").unwrap();
        assert_eq!(vectors.get("s1#4"), Some(&vec![0.5, -0.25]));
        assert!(s.recall_pending("tiny", 10).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn another_models_vector_is_neither_read_nor_counted_as_done() {
        let (dir, s) = store("model");
        s.put_recall_units(&[unit("s1#4", 10)]).unwrap();
        s.put_recall_vectors("tiny", &[("s1#4".into(), vec![1.0])], 11)
            .unwrap();
        assert!(s.recall_vectors("other").unwrap().is_empty());
        // A second model's build owes this unit again, and writes its own row over it.
        assert_eq!(s.recall_pending("other", 10).unwrap().len(), 1);
        s.put_recall_vectors("other", &[("s1#4".into(), vec![1.0, 0.0])], 12)
            .unwrap();
        assert_eq!(s.recall_vectors("other").unwrap()["s1#4"], vec![1.0, 0.0]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The newest units are what a build spends its budget on, and the cap is the whole bound.
    #[test]
    fn units_come_back_newest_first_and_bounded() {
        let (dir, s) = store("order");
        let units: Vec<RecallUnit> = (0..5).map(|i| unit(&format!("s1#{i}"), i)).collect();
        s.put_recall_units(&units).unwrap();
        let back = s.recall_units(2).unwrap();
        assert_eq!(
            back.iter().map(|u| u.unit.as_str()).collect::<Vec<_>>(),
            vec!["s1#4", "s1#3"]
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
