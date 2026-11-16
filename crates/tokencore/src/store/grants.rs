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
    granted_by  TEXT NOT NULL DEFAULT '',  -- v51; who answered: `human:<name>`, `shadow:<judge>`
    UNIQUE (session_id, scope, kind, key)
);
CREATE INDEX IF NOT EXISTS session_grant_by_session ON session_grant (session_id, scope);
"#;

/// A session's shadow breaker per job: kept beside the grants it made, so a restart that keeps
/// the grants keeps the trip too. Only an explicit re-arm clears it.
pub const BREAKER_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS shadow_breaker (
    session_id    TEXT NOT NULL,     -- the session whose shadow answers
    job           TEXT NOT NULL,     -- the shadow job: 'permission'
    disagreements INTEGER NOT NULL,
    tripped_ms    INTEGER,           -- NULL while armed
    why           TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (session_id, job)
);
"#;

/// A job's breaker as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakerRow {
    pub disagreements: u32,
    pub tripped_ms: Option<i64>,
    pub why: String,
}

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
    pub by: String,
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
    pub by: &'a str,
}

impl Store {
    pub fn put_grant(&self, g: &NewGrant<'_>) -> Result<()> {
        self.exec(
            "INSERT INTO session_grant
                 (session_id, scope, kind, key, body, why, granted_ms, granted_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(session_id, scope, kind, key)
                 DO UPDATE SET body = ?5, why = ?6, granted_ms = ?7, granted_by = ?8",
            params![
                g.session_id,
                g.scope,
                g.kind,
                g.key,
                g.body,
                g.why,
                now_ms(),
                g.by
            ],
        )?;
        Ok(())
    }

    /// The grant table's state as one value: it moves on every insert, update and delete, from
    /// any connection, so a reader can tell a stale copy without re-reading every row.
    pub fn grant_generation(&self) -> Result<String> {
        let conn = self.at("session_grant");
        Ok(conn.query_row(
            "SELECT COALESCE(MAX(id), 0), COUNT(*), COALESCE(MAX(granted_ms), 0) FROM session_grant",
            [],
            |r| {
                Ok(format!(
                    "{}:{}:{}",
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?
                ))
            },
        )?)
    }

    /// Every grant held under `(session_id, scope)`, oldest first.
    pub fn grants(&self, session_id: &str, scope: &str) -> Result<Vec<GrantRow>> {
        self.grant_rows(
            "WHERE session_id = ?1 AND scope = ?2",
            params![session_id, scope],
        )
    }

    /// Every grant whose answerer starts with `prefix`, in any session, oldest first.
    pub fn grants_by(&self, prefix: &str) -> Result<Vec<GrantRow>> {
        self.grant_rows(
            "WHERE substr(granted_by, 1, length(?1)) = ?1",
            params![prefix],
        )
    }

    fn grant_rows(&self, filter: &str, args: impl rusqlite::Params) -> Result<Vec<GrantRow>> {
        let conn = self.at("session_grant");
        let mut st = conn.prepare(&format!(
            "SELECT id, session_id, scope, kind, key, body, why, granted_ms, granted_by
             FROM session_grant {filter} ORDER BY id"
        ))?;
        let rows = st
            .query_map(args, |r| {
                Ok(GrantRow {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    scope: r.get(2)?,
                    kind: r.get(3)?,
                    key: r.get(4)?,
                    body: r.get(5)?,
                    why: r.get(6)?,
                    granted_ms: r.get(7)?,
                    by: r.get(8)?,
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

    /// Drop the grants under `(session_id, scope)` granted before `before_ms`. How many went.
    pub fn expire_grants(&self, session_id: &str, scope: &str, before_ms: i64) -> Result<usize> {
        Ok(self.exec(
            "DELETE FROM session_grant WHERE session_id = ?1 AND scope = ?2 AND granted_ms < ?3",
            params![session_id, scope, before_ms],
        )?)
    }

    /// `session_id`'s breaker for `job`, or `None` when it never counted a disagreement.
    pub fn breaker(&self, session_id: &str, job: &str) -> Result<Option<BreakerRow>> {
        use rusqlite::OptionalExtension;
        let conn = self.at("shadow_breaker");
        Ok(conn
            .query_row(
                "SELECT disagreements, tripped_ms, why FROM shadow_breaker
                 WHERE session_id = ?1 AND job = ?2",
                params![session_id, job],
                |r| {
                    Ok(BreakerRow {
                        disagreements: r.get::<_, i64>(0)?.max(0) as u32,
                        tripped_ms: r.get(1)?,
                        why: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn put_breaker(&self, session_id: &str, job: &str, b: &BreakerRow) -> Result<()> {
        self.exec(
            "INSERT INTO shadow_breaker (session_id, job, disagreements, tripped_ms, why)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id, job)
                 DO UPDATE SET disagreements = ?3, tripped_ms = ?4, why = ?5",
            params![session_id, job, b.disagreements as i64, b.tripped_ms, b.why],
        )?;
        Ok(())
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
            by: "human:op",
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

    #[test]
    fn a_breaker_is_kept_until_rewritten() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.breaker("s-1", "permission").unwrap(), None);
        let tripped = BreakerRow {
            disagreements: 2,
            tripped_ms: Some(7),
            why: "revoked".into(),
        };
        s.put_breaker("s-1", "permission", &tripped).unwrap();
        assert_eq!(
            s.breaker("s-1", "permission").unwrap(),
            Some(tripped.clone())
        );
        assert_eq!(s.breaker("s-2", "permission").unwrap(), None);
        let armed = BreakerRow {
            disagreements: 0,
            tripped_ms: None,
            why: String::new(),
        };
        s.put_breaker("s-1", "permission", &armed).unwrap();
        assert_eq!(s.breaker("s-1", "permission").unwrap(), Some(armed));
    }

    #[test]
    fn grants_are_found_by_who_answered_them() {
        let s = Store::open_in_memory().unwrap();
        s.put_grant(&grant("s-1", "session", "/a", "{}")).unwrap();
        s.put_grant(&NewGrant {
            by: "shadow:deepseek",
            ..grant("s-2", "session", "/b", "{}")
        })
        .unwrap();
        let shadow = s.grants_by("shadow:").unwrap();
        assert_eq!(shadow.len(), 1);
        assert_eq!(
            (shadow[0].session_id.as_str(), shadow[0].by.as_str()),
            ("s-2", "shadow:deepseek")
        );
        assert_eq!(s.grants("s-1", "session").unwrap()[0].by, "human:op");
    }
}
