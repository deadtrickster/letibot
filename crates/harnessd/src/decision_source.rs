//! The adjudication corpus behind the `decisions` tool.
//!
//! The sibling of [`crate::transcript_source`], and the same seam for the same
//! reason: `letibot-tools` cannot name `letibot-tokencore`, so the tool declares
//! a [`DecisionSource`] and this implements it.
//!
//! # Filtering in SQL, and why the predicates are not columns
//!
//! The tool's arguments are the names a person uses — `by=oracle`, `verdict=deny`,
//! `asked=true` — and the mapping onto the thirty-column `adjudication` table
//! lives here, in one file, once. That is the whole bargain the operator asked
//! for: *"the model shouldn't derive the storage, its format, or what path it"*
//! is at.
//!
//! Two mappings are worth naming because a caller would get them wrong:
//!
//! * **`by` matches a prefix.** The stored value is `human:dead`, `oracle:glm`,
//!   `boundary:flow`, `gate:timeout` — a decider and its identity joined by a
//!   colon. The question is almost always about the decider, so `by=boundary`
//!   finds `boundary:flow` and `boundary:always_ask`. `operator` is accepted as a
//!   synonym for `human`, because that is the word the rest of the system uses.
//! * **`asked` is not `verdict_by = human`.** The corpus keeps them apart on
//!   purpose: `asked` means a person was actually put in front of the call, and a
//!   standing rule the operator wrote earlier decides plenty of calls nobody was
//!   asked about. Collapsing them is how a corpus stops being able to say which
//!   labels cost a human a keystroke.
//!
//! # The counts the tool reports are the store's own
//!
//! [`Store::corpus_counts`] already computes them, with the reasoning for why
//! there are four rather than three written at its definition. Recomputing them
//! here would be a second definition of "measured" that drifts from the first.

use letibot_tokencore::rusqlite;
use letibot_tokencore::store::{Store, ASKED_SQL, DISAGREEMENT_SQL, MEASURED_SQL};
use letibot_tools::builtins::decisions::{
    DecisionCounts, DecisionHits, DecisionQuery, DecisionRow, DecisionSource,
};

pub struct StoreDecisions {
    /// This reader's own connection. See `transcript_source`'s module docs: a
    /// `Connection` is `Send` but not `Sync`, and a tool is both.
    store: std::sync::Mutex<Store>,
    session_id: String,
}

impl StoreDecisions {
    pub fn open(path: &std::path::Path, session_id: String) -> Result<Self, String> {
        let store = Store::open(path).map_err(|e| {
            format!("the decision corpus at {} could not be opened: {e}", path.display())
        })?;
        Ok(StoreDecisions {
            store: std::sync::Mutex::new(store),
            session_id,
        })
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Which session's decisions, as SQL. `any` is the whole corpus; a name that
    /// is not an exact id is matched against ids and titles, and an ambiguous one
    /// is reported rather than picked.
    fn scope(&self, want: Option<&str>) -> Result<Option<String>, String> {
        let Some(want) = want else {
            return Ok(Some(self.session_id.clone()));
        };
        if want.eq_ignore_ascii_case("any") || want.eq_ignore_ascii_case("all") {
            return Ok(None);
        }
        let sessions = self
            .store()
            .list_sessions()
            .map_err(|e| format!("the store could not be listed: {e}"))?;
        if sessions.iter().any(|s| s.id == want) {
            return Ok(Some(want.to_string()));
        }
        let needle = want.to_lowercase();
        let hits: Vec<_> = sessions
            .iter()
            .filter(|s| {
                s.id.to_lowercase().contains(&needle)
                    || s.title
                        .as_deref()
                        .is_some_and(|t| t.to_lowercase().contains(&needle))
            })
            .collect();
        match hits.len() {
            1 => Ok(Some(hits[0].id.clone())),
            0 => Err(format!(
                "no session matches `{want}`, by id or by title. `session=any` searches \
                 the whole corpus."
            )),
            n => Err(format!(
                "`{want}` matches {n} sessions: {}. Name one by its id, or use \
                 `session=any`.",
                hits.iter()
                    .take(8)
                    .map(|s| s.id.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

/// Every column the tool can show. One list so the `SELECT` and the row builder
/// cannot drift apart.
const COLS: &str = "request_id, session_id, turn_id, decided_ms, action, tier, effect, \
                    tool, mode, verdict, verdict_by, verdict_basis, p_allow, oracle_ms, \
                    oracle_model, asked, operator_kind, operator_note, shown";

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<DecisionRow> {
    Ok(DecisionRow {
        request_id: r.get(0)?,
        session_id: r.get(1)?,
        turn_id: r.get(2)?,
        decided_ms: r.get(3)?,
        action: r.get(4)?,
        tier: r.get(5)?,
        effect: r.get(6)?,
        tool: r.get(7)?,
        mode: r.get(8)?,
        verdict: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
        verdict_by: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
        verdict_basis: r.get::<_, Option<String>>(11)?.unwrap_or_default(),
        p_allow: r.get(12)?,
        oracle_ms: r.get(13)?,
        oracle_model: r.get::<_, Option<String>>(14)?.unwrap_or_default(),
        asked: r.get::<_, i64>(15)? != 0,
        operator_kind: r.get::<_, Option<String>>(16)?.unwrap_or_default(),
        operator_note: r.get::<_, Option<String>>(17)?.unwrap_or_default(),
        // Left as `Option`: `NULL` here means no oracle was consulted, which the
        // tool reports as a fact rather than as an empty brief.
        shown: r.get(18)?,
    })
}

/// `operator` is the word the rest of the system uses; `human` is what the corpus
/// stores. Accepting both costs one line and saves a miss that reads like "the
/// operator decided nothing here".
fn decider_prefix(by: &str) -> String {
    match by.trim().to_ascii_lowercase().as_str() {
        "operator" | "human" | "person" => "human".into(),
        other => other.to_string(),
    }
}

impl DecisionSource for StoreDecisions {
    fn find(&self, q: &DecisionQuery) -> Result<DecisionHits, String> {
        let scope = self.scope(q.session.as_deref())?;
        let store = self.store();
        let conn = store.connection();

        // A single decision by id ignores every other predicate, including the
        // session: an id is already unique, and a `request=` that came back empty
        // because the caller was in a different session is the miss this tool
        // exists to stop.
        if let Some(want) = &q.request {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {COLS} FROM adjudication WHERE request_id = ?1"
                ))
                .map_err(|e| format!("preparing the decision query: {e}"))?;
            let rows: Vec<DecisionRow> = stmt
                .query_map([want], row_from)
                .map_err(|e| format!("reading the decision: {e}"))?
                .collect::<std::result::Result<_, _>>()
                .map_err(|e| format!("reading the decision: {e}"))?;
            let scanned = scalar(conn, "SELECT COUNT(*) FROM adjudication", &[]).unwrap_or(0);
            return Ok(DecisionHits {
                matched: rows.len(),
                rows,
                session_id: scope.unwrap_or_else(|| "any".into()),
                scanned: scanned as usize,
                ..Default::default()
            });
        }

        let mut wheres: Vec<String> = Vec::new();
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(s) = &scope {
            wheres.push("session_id = ?1".to_string());
            binds.push(Box::new(s.clone()));
        }
        // **The scope's clause and binds, kept apart from the filters'.** The miss
        // report lists what is present in the SCOPE, because a query that matched
        // nothing has no values of its own to list — so these are reused after the
        // filter binds have been pushed. Sharing one vector gives a statement with
        // one placeholder and five parameters, which rusqlite refuses.
        let scope_sql = wheres
            .first()
            .map(|w| format!(" WHERE {w}"))
            .unwrap_or_default();
        let scope_binds: Vec<Box<dyn rusqlite::ToSql>> = scope
            .as_ref()
            .map(|s| vec![Box::new(s.clone()) as Box<dyn rusqlite::ToSql>])
            .unwrap_or_default();
        // "0 of 4000 searched" and "0 of 0" are different answers, and only one of
        // them is about the predicates.
        let scanned = scalar(
            conn,
            &format!("SELECT COUNT(*) FROM adjudication{scope_sql}"),
            &scope_binds,
        )
        .unwrap_or(0) as usize;

        if let Some(t) = &q.tool {
            wheres.push(format!("tool = ?{}", binds.len() + 1));
            binds.push(Box::new(t.clone()));
        }
        if let Some(v) = &q.verdict {
            wheres.push(format!("verdict = ?{}", binds.len() + 1));
            binds.push(Box::new(v.trim().to_ascii_lowercase()));
        }
        if let Some(b) = &q.by {
            wheres.push(format!("verdict_by LIKE ?{}", binds.len() + 1));
            binds.push(Box::new(format!("{}%", decider_prefix(b))));
        }
        if let Some(a) = q.asked {
            wheres.push(format!("asked = ?{}", binds.len() + 1));
            binds.push(Box::new(if a { 1i64 } else { 0i64 }));
        }
        if q.disagreements {
            wheres.push("operator_kind IS NOT NULL".into());
        }
        let where_sql = if wheres.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", wheres.join(" AND "))
        };

        let matched = scalar(
            conn,
            &format!("SELECT COUNT(*) FROM adjudication{where_sql}"),
            &binds,
        )
        .unwrap_or(0) as usize;

        // `last` caps the match set before the page is taken, so `last=64` with a
        // filter means the newest 64 MATCHING decisions — the same reading
        // `transcript` gives it.
        let cap = q
            .last
            .map(|n| (n as usize).min(matched))
            .unwrap_or(matched);
        let take = q.limit.min(cap.saturating_sub(q.offset));
        let mut rows = Vec::new();
        if take > 0 {
            let sql = format!(
                "SELECT {COLS} FROM adjudication{where_sql} ORDER BY decided_ms DESC, \
                 request_id DESC LIMIT ?{} OFFSET ?{}",
                binds.len() + 1,
                binds.len() + 2
            );
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("preparing the decision query: {e}"))?;
            let mut params: Vec<&dyn rusqlite::ToSql> =
                binds.iter().map(|b| b.as_ref() as &dyn rusqlite::ToSql).collect();
            let take_i = take as i64;
            let off_i = q.offset as i64;
            params.push(&take_i);
            params.push(&off_i);
            rows = stmt
                .query_map(params.as_slice(), row_from)
                .map_err(|e| format!("reading decisions: {e}"))?
                .collect::<std::result::Result<_, _>>()
                .map_err(|e| format!("reading decisions: {e}"))?;
        }

        Ok(DecisionHits {
            rows,
            session_id: scope.clone().unwrap_or_else(|| "any".into()),
            scanned,
            matched: cap,
            // Computed over the SCOPE, not over the filtered set — a miss report
            // listing "the verdicts present" from a query that matched nothing
            // would list nothing.
            verdicts_present: distinct(conn, "verdict", &scope_sql, &scope_binds),
            tools_present: distinct(conn, "tool", &scope_sql, &scope_binds),
            deciders_present: distinct(conn, "verdict_by", &scope_sql, &scope_binds),
        })
    }

    fn counts(&self, session: Option<&str>) -> Result<DecisionCounts, String> {
        // Whole-corpus counts come from the store's own definition; see the module
        // docs for why they are not recomputed here.
        let scope = self.scope(session)?;
        let store = self.store();
        if scope.is_none() {
            let c = store
                .corpus_counts()
                .map_err(|e| format!("counting the corpus: {e}"))?;
            return Ok(DecisionCounts {
                total: c.total,
                decided_by_operator: c.decided_by_operator,
                measured: c.measured,
                disagreements: c.disagreements,
            });
        }
        let s = scope.unwrap();
        let conn = store.connection();
        let one = |extra: &str| -> u64 {
            scalar(
                conn,
                &format!("SELECT COUNT(*) FROM adjudication WHERE session_id = ?1{extra}"),
                &[Box::new(s.clone()) as Box<dyn rusqlite::ToSql>],
            )
            .unwrap_or(0) as u64
        };
        Ok(DecisionCounts {
            total: one(""),
            // **The predicates are imported, not re-typed.** This function is the
            // scoped twin of `Store::corpus_counts`, and the two drifted exactly as a
            // copied `WHERE` clause does: `measured` read
            // `oracle_ms IS NOT NULL`, which matches **every** row because
            // `oracle_ms` is `0` and never NULL — the scoped banner announced that
            // every decision in the session was measured against a model. And
            // `disagreements` read `operator_kind IS NOT NULL`, which counts a ruling
            // the operator *agreed* with; on this box 2026-09-21 that reported `1863`
            // where the true figure is `1294`.
            //
            // Both are now interpolated from the one definition in the store crate.
            // A count is not measuring what its NAME says, it is measuring what its
            // PREDICATE says — so there is one predicate.
            decided_by_operator: one(&format!(" AND {ASKED_SQL}")),
            measured: one(&format!(" AND {MEASURED_SQL}")),
            disagreements: one(&format!(" AND {DISAGREEMENT_SQL}")),
        })
    }
}

fn scalar(
    conn: &rusqlite::Connection,
    sql: &str,
    binds: &[Box<dyn rusqlite::ToSql>],
) -> Option<i64> {
    let params: Vec<&dyn rusqlite::ToSql> =
        binds.iter().map(|b| b.as_ref() as &dyn rusqlite::ToSql).collect();
    conn.query_row(sql, params.as_slice(), |r| r.get(0)).ok()
}

/// The distinct values of one column in scope, for the miss report. Bounded: a
/// miss that answered with four hundred tool names would be its own problem.
fn distinct(
    conn: &rusqlite::Connection,
    column: &str,
    scope_sql: &str,
    binds: &[Box<dyn rusqlite::ToSql>],
) -> Vec<String> {
    // `column` is never caller-supplied — the three call sites pass literals — so
    // interpolating it is safe, and the scope is still bound. The scope has
    // already opened a `WHERE` when it is non-empty, so this continues it with
    // `AND` rather than opening a second one.
    let joiner = if scope_sql.is_empty() { "WHERE" } else { "AND" };
    let sql = format!(
        "SELECT DISTINCT {column} FROM adjudication{scope_sql} \
         {joiner} {column} IS NOT NULL AND {column} != '' LIMIT 24"
    );
    let params: Vec<&dyn rusqlite::ToSql> =
        binds.iter().map(|b| b.as_ref() as &dyn rusqlite::ToSql).collect();
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params.as_slice(), |r| r.get::<_, String>(0)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rows.filter_map(|r| r.ok()).collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_and_human_are_the_same_decider() {
        assert_eq!(decider_prefix("operator"), "human");
        assert_eq!(decider_prefix("Human"), "human");
        assert_eq!(decider_prefix("person"), "human");
        // Everything else is passed through as typed, lowercased, so `boundary`
        // still reaches `boundary:flow`.
        assert_eq!(decider_prefix("boundary"), "boundary");
        assert_eq!(decider_prefix("Oracle"), "oracle");
    }
}
