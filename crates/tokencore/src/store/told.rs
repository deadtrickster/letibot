//! What the queues have already said, kept across a restart.
//!
//! The sets lived in the daemon's memory, so every restart re-gated the same red tip and resumed
//! dormant sessions to re-tell them. Here "this was said" is a row keyed by what was said about,
//! one table for every teller, carrying the count and the clock a backoff reads.

use rusqlite::{OptionalExtension, params};

use super::{Result, Store};

pub const TOLD_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS queue_told (
    key     TEXT PRIMARY KEY,   -- what was said about, as the teller spells it
    count   INTEGER NOT NULL,   -- how many times it was said (or tried)
    at_ms   INTEGER NOT NULL    -- the last time, unix ms
);
"#;

/// One key's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Told {
    pub count: u32,
    pub at_ms: u64,
}

impl Store {
    /// **Say `key` once**: `true` when this call is the first (the row was written), `false`
    /// when it was already said — by this daemon or one before it.
    pub fn tell_once(&self, key: &str, now_ms: u64) -> Result<bool> {
        let n = self.exec(
            "INSERT OR IGNORE INTO queue_told (key, count, at_ms) VALUES (?1, 1, ?2)",
            params![key, now_ms as i64],
        )?;
        Ok(n == 1)
    }

    /// **Count another telling of `key`** and answer the count after it (1 for the first).
    pub fn tell_again(&self, key: &str, now_ms: u64) -> Result<u32> {
        self.exec(
            "INSERT INTO queue_told (key, count, at_ms) VALUES (?1, 1, ?2)
             ON CONFLICT(key) DO UPDATE SET count = count + 1, at_ms = excluded.at_ms",
            params![key, now_ms as i64],
        )?;
        Ok(self.told(key)?.map(|t| t.count).unwrap_or(1))
    }

    /// What was said about `key`, if anything.
    pub fn told(&self, key: &str) -> Result<Option<Told>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT count, at_ms FROM queue_told WHERE key = ?1",
                params![key],
                |r| {
                    Ok(Told {
                        count: r.get::<_, i64>(0)?.max(0) as u32,
                        at_ms: r.get::<_, i64>(1)?.max(0) as u64,
                    })
                },
            )
            .optional()?)
    }

    /// **Every key written under `prefix`**, oldest first, with what was said about it: the reading a
    /// queue of owed things needs, of what was written down and has not been forgotten yet.
    pub fn told_with_prefix(&self, prefix: &str) -> Result<Vec<(String, Told)>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT key, count, at_ms FROM queue_told WHERE substr(key, 1, length(?1)) = ?1
               ORDER BY at_ms, key",
        )?;
        let rows = st.query_map(params![prefix], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Told {
                    count: r.get::<_, i64>(1)?.max(0) as u32,
                    at_ms: r.get::<_, i64>(2)?.max(0) as u64,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// **Forget every key that starts with `prefix`** — the state they were about is over (a red
    /// target is green again), so the next time it comes back it is news. Answers how many rows
    /// were forgotten: more than none is "this was said, and is not true any more".
    pub fn untell_prefix(&self, prefix: &str) -> Result<usize> {
        Ok(self.exec(
            "DELETE FROM queue_told WHERE substr(key, 1, length(?1)) = ?1",
            params![prefix],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Store, std::path::PathBuf) {
        let p = std::env::temp_dir().join(format!(
            "letibot-told-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        (Store::open(&p).unwrap(), p)
    }

    #[test]
    fn a_key_is_told_once_counted_and_forgotten_by_prefix_and_survives_a_reopen() {
        let (s, p) = store();
        assert!(s.tell_once("rung:/r:health:main:abc:red", 5).unwrap());
        assert!(!s.tell_once("rung:/r:health:main:abc:red", 6).unwrap());
        drop(s);
        let s = Store::open(&p).unwrap();
        assert!(
            !s.tell_once("rung:/r:health:main:abc:red", 7).unwrap(),
            "a reopened store (a restarted daemon) remembers"
        );
        assert_eq!(s.tell_again("killed:/r:main:abc", 10).unwrap(), 1);
        assert_eq!(s.tell_again("killed:/r:main:abc", 20).unwrap(), 2);
        assert_eq!(
            s.told("killed:/r:main:abc").unwrap(),
            Some(Told {
                count: 2,
                at_ms: 20
            })
        );
        assert_eq!(s.untell_prefix("rung:/r:health:main:").unwrap(), 1);
        assert_eq!(s.untell_prefix("rung:/r:health:main:").unwrap(), 0);
        assert!(s.tell_once("rung:/r:health:main:abc:red", 8).unwrap());

        // **What was written down under a prefix, and only that**: a queue of owed notices reads
        // this, so another teller's key has to stay out and the clock has to come back with it.
        assert!(s.tell_once("refusal:abc:s-1", 30).unwrap());
        assert!(s.tell_once("refusal:def:s-2", 40).unwrap());
        assert_eq!(
            s.told_with_prefix("refusal:").unwrap(),
            vec![
                (
                    "refusal:abc:s-1".to_string(),
                    Told {
                        count: 1,
                        at_ms: 30
                    }
                ),
                (
                    "refusal:def:s-2".to_string(),
                    Told {
                        count: 1,
                        at_ms: 40
                    }
                ),
            ],
            "oldest first, with the count and the clock each was written at"
        );
        assert!(
            s.told_with_prefix("nothing:").unwrap().is_empty(),
            "a prefix nobody wrote under is no row, not every row"
        );
        let _ = std::fs::remove_file(&p);
    }
}
