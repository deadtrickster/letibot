//! Warnings that tell a person how to recover, kept past a daemon restart. The hub's view holds the
//! last few in memory; this table is their durable copy, appended and never rewritten, and a new
//! daemon replays a session's tail of it into the view it serves.

use rusqlite::params;

use super::{Result, Store};

pub const DISCLOSURES_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS disclosure (
    session_id TEXT NOT NULL,
    seq        INTEGER NOT NULL,   -- per session, from 0, in the order the hub published them
    ts_ms      INTEGER NOT NULL,   -- the envelope's clock, so a replayed note keeps its identity
    code       TEXT NOT NULL,
    detail     TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
);

CREATE TRIGGER IF NOT EXISTS disclosure_no_update
BEFORE UPDATE ON disclosure BEGIN
    SELECT RAISE(ABORT, 'disclosure is append-only: no UPDATE');
END;
"#;

/// One stored disclosure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disclosure {
    pub ts_ms: u64,
    pub code: String,
    pub detail: String,
}

impl Store {
    /// Append one disclosure to `session_id`'s record.
    pub fn append_disclosure(&self, session_id: &str, d: &Disclosure) -> Result<()> {
        self.write_tx_at("disclosure", |tx| {
            tx.execute(
                "INSERT INTO disclosure (session_id, seq, ts_ms, code, detail) \
                 SELECT ?1, COALESCE(MAX(seq), -1) + 1, ?2, ?3, ?4 \
                 FROM disclosure WHERE session_id = ?1",
                params![session_id, d.ts_ms as i64, d.code, d.detail],
            )?;
            Ok(())
        })
    }

    /// The newest `last` disclosures of `session_id`, oldest first.
    pub fn disclosures(&self, session_id: &str, last: usize) -> Result<Vec<Disclosure>> {
        let conn = self.at("disclosure");
        let mut st = conn.prepare(
            "SELECT ts_ms, code, detail FROM (SELECT seq, ts_ms, code, detail FROM disclosure \
             WHERE session_id = ?1 ORDER BY seq DESC LIMIT ?2) ORDER BY seq",
        )?;
        let rows = st.query_map(params![session_id, last as i64], |r| {
            Ok(Disclosure {
                ts_ms: r.get::<_, i64>(0)?.max(0) as u64,
                code: r.get(1)?,
                detail: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(n: u64) -> Disclosure {
        Disclosure {
            ts_ms: n,
            code: "operator_shell_failed".into(),
            detail: format!("lost {n}"),
        }
    }

    #[test]
    fn the_tail_comes_back_oldest_first_and_a_row_is_never_rewritten() {
        let s = Store::open_in_memory().unwrap();
        for n in 0..5 {
            s.append_disclosure("s1", &said(n)).unwrap();
        }
        s.append_disclosure("s2", &said(9)).unwrap();
        let tail: Vec<u64> = s
            .disclosures("s1", 3)
            .unwrap()
            .iter()
            .map(|d| d.ts_ms)
            .collect();
        assert_eq!(tail, vec![2, 3, 4]);
        let refused = s.conn().execute(
            "UPDATE disclosure SET detail = 'x' WHERE session_id = 's1'",
            [],
        );
        assert!(refused.is_err(), "an UPDATE was taken");
    }
}
