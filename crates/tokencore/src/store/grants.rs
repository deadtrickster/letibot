//! What the operator granted a session from a card: a path into its view, a class of calls, a
//! memory ceiling. A row lives as long as its session does, so a daemon restart re-applies it.
//! A `queue` row is keyed on the review queue's gatekeeper and covers every reviewer under it.

use rusqlite::params;

use super::{Result, Store, now_ms};

pub const GRANTS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS session_grant (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT NOT NULL,   -- the session, or the gatekeeper for a queue grant
    scope       TEXT NOT NULL,   -- 'session' | 'queue'
    kind        TEXT NOT NULL,   -- 'view' | 'allow' | 'memory'
    key         TEXT NOT NULL,   -- what makes two grants one: a path, a coverage, 'memory'
    body        TEXT NOT NULL,   -- the grant itself, JSON, opaque to the store
    why         TEXT NOT NULL DEFAULT '',
    granted_ms  INTEGER NOT NULL,
    UNIQUE (session_id, scope, kind, key)
);
CREATE INDEX IF NOT EXISTS session_grant_by_session ON session_grant (session_id, scope);
"#;

/// One stored grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRow {
    pub id: i64,
    pub session_id: String,
    pub scope: String,
    pub kind: String,
    pub key: String,
    pub body: String,
    pub why: String,
    pub granted_ms: i64,
}

/// A grant to write. A second write of the same `(session, scope, kind, key)` replaces the body,
/// which is how a read grant becomes a read-write one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGrant<'a> {
    pub session_id: &'a str,
    pub scope: &'a str,
    pub kind: &'a str,
    pub key: &'a str,
    pub body: &'a str,
    pub why: &'a str,
}

impl Store {
    pub fn put_grant(&self, g: &NewGrant<'_>) -> Result<()> {
        self.exec(
            "INSERT INTO session_grant (session_id, scope, kind, key, body, why, granted_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(session_id, scope, kind, key) DO UPDATE SET body = ?5, why = ?6",
            params![
                g.session_id,
                g.scope,
                g.kind,
                g.key,
                g.body,
                g.why,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Every grant held under `(session_id, scope)`, oldest first.
    pub fn grants(&self, session_id: &str, scope: &str) -> Result<Vec<GrantRow>> {
        let conn = self.at("session_grant");
        let mut st = conn.prepare(
            "SELECT id, session_id, scope, kind, key, body, why, granted_ms FROM session_grant
             WHERE session_id = ?1 AND scope = ?2 ORDER BY id",
        )?;
        let rows = st
            .query_map(params![session_id, scope], |r| {
                Ok(GrantRow {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    scope: r.get(2)?,
                    kind: r.get(3)?,
                    key: r.get(4)?,
                    body: r.get(5)?,
                    why: r.get(6)?,
                    granted_ms: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Drop one grant by id. Whether there was one.
    pub fn revoke_grant(&self, id: i64) -> Result<bool> {
        Ok(self.exec("DELETE FROM session_grant WHERE id = ?1", params![id])? > 0)
    }

    /// Drop every grant of `kind` held under `(session_id, scope)`. How many went.
    pub fn clear_grants(&self, session_id: &str, scope: &str, kind: &str) -> Result<usize> {
        Ok(self.exec(
            "DELETE FROM session_grant WHERE session_id = ?1 AND scope = ?2 AND kind = ?3",
            params![session_id, scope, kind],
        )?)
    }

    /// Drop everything a session holds: its life is over.
    pub fn forget_session_grants(&self, session_id: &str) -> Result<usize> {
        Ok(self.exec(
            "DELETE FROM session_grant WHERE session_id = ?1 AND scope = 'session'",
            params![session_id],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant<'a>(session: &'a str, scope: &'a str, key: &'a str, body: &'a str) -> NewGrant<'a> {
        NewGrant {
            session_id: session,
            scope,
            kind: "view",
            key,
            body,
            why: "answered on a card",
        }
    }

    #[test]
    fn a_grant_is_kept_per_session_and_scope_and_a_rewrite_replaces_it() {
        let s = Store::open_in_memory().unwrap();
        s.put_grant(&grant("s-1", "session", "/build", r#"{"writable":false}"#))
            .unwrap();
        s.put_grant(&grant("s-1", "session", "/build", r#"{"writable":true}"#))
            .unwrap();
        s.put_grant(&grant("s-gk", "queue", "/data", r#"{"writable":false}"#))
            .unwrap();
        let mine = s.grants("s-1", "session").unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].body, r#"{"writable":true}"#);
        assert!(s.grants("s-1", "queue").unwrap().is_empty());
        assert_eq!(s.grants("s-gk", "queue").unwrap().len(), 1);

        assert!(s.revoke_grant(mine[0].id).unwrap());
        assert!(!s.revoke_grant(mine[0].id).unwrap());
        assert!(s.grants("s-1", "session").unwrap().is_empty());
    }

    #[test]
    fn forgetting_a_session_leaves_its_queue_rows() {
        let s = Store::open_in_memory().unwrap();
        s.put_grant(&grant("s-gk", "session", "/a", "{}")).unwrap();
        s.put_grant(&grant("s-gk", "queue", "/b", "{}")).unwrap();
        assert_eq!(s.forget_session_grants("s-gk").unwrap(), 1);
        assert_eq!(s.grants("s-gk", "queue").unwrap().len(), 1);
        assert_eq!(s.clear_grants("s-gk", "queue", "view").unwrap(), 1);
    }
}
