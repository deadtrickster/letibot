//! The adjudication corpus behind the `decisions` tool. The sibling of
//! [`crate::transcript_source`], and the same seam for the same reason

use letibot_tokencore::rusqlite;
use letibot_tokencore::store::{ASKED_SQL, DISAGREEMENT_SQL, MEASURED_SQL, Store};
use letibot_tools::builtins::decisions::{
    DecisionCounts, DecisionHits, DecisionQuery, DecisionRow, DecisionSource,
};

pub struct StoreDecisions {
    /// Where the corpus is: a scoped read resolves through its index to one project file, and
    /// `session=any` sweeps every file under its data directory (§ The corpus).
    at: crate::config::StoreAt,
    /// This reader's handle onto the process's one connection. See
    /// `transcript_source`'s module docs: a `Connection` is `Send` but not `Sync`, and a tool is
    /// both.
    store: std::sync::Mutex<Store>,
    session_id: String,
}

impl StoreDecisions {
    pub fn open(at: impl Into<crate::config::StoreAt>, session_id: String) -> Result<Self, String> {
        let at: crate::config::StoreAt = at.into();
        let store = at.open_existing().map_err(|e| {
            format!(
                "the decision corpus at {} could not be opened: {e}",
                at.display()
            )
        })?;
        Ok(StoreDecisions {
            at,
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

impl StoreDecisions {
    /// One file's answer to a scoped question, with the reader kept alive for the call when the
    /// file is another project's: the index names the file and this opens it, no sweep.
    fn scoped<T>(
        &self,
        session: &str,
        f: impl FnOnce(&rusqlite::Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let store = self.store();
        match crate::corpus::session_file(&self.at, &store, session)? {
            crate::corpus::Scoped::Own => f(&store.connection_of("adjudication"))
                .map_err(|e| format!("reading {}: {e}", self.at.path("adjudication").display())),
            crate::corpus::Scoped::Elsewhere(path) => {
                let reader = Store::open_reader(&path)
                    .map_err(|e| format!("reading {}: {e}", path.display()))?;
                f(&reader.connection_of("adjudication"))
                    .map_err(|e| format!("reading {}: {e}", path.display()))
            }
        }
    }

    /// `session=any`: every project file, read-only, in key order (§ The corpus). A file that will
    /// not open is named and the sweep goes on with the rest. An id, being unique, stops it.
    fn sweep(&self, q: &DecisionQuery, page: usize) -> Result<DecisionHits, String> {
        let mut merged = Found::default();
        let mut unreadable = Vec::new();
        let by_id = q.request.is_some();
        for path in crate::corpus::project_files(&self.at) {
            let Ok(reader) = Store::open_reader(&path) else {
                unreadable.push(format!("{} (it would not open)", path.display()));
                continue;
            };
            match find_in(&reader.connection_of("adjudication"), None, q, page) {
                Ok(found) => {
                    let hit = by_id && !found.rows.is_empty();
                    merged.absorb(found);
                    if hit {
                        break;
                    }
                }
                Err(e) => unreadable.push(format!("{} ({e})", path.display())),
            }
        }
        // One page of one corpus: the newest rows of every file, sorted together, so `offset` is
        // an offset into the corpus and not into whichever file was read first.
        merged.rows.sort_by(newest_first);
        let mut hits = page_of(merged, q, "any");
        hits.unreadable = unreadable;
        Ok(hits)
    }
}

impl DecisionSource for StoreDecisions {
    fn find(&self, q: &DecisionQuery) -> Result<DecisionHits, String> {
        let scope = self.scope(q.session.as_deref())?;
        let page = q.offset + q.limit;
        match &scope {
            Some(s) => {
                let found = self.scoped(s, |conn| find_in(conn, Some(s), q, page))?;
                Ok(page_of(found, q, s))
            }
            // A single decision by id ignores every other predicate, including the session: an id
            // is already unique, and a `request=` that came back empty because the caller was in a
            // different session is the miss this tool exists to stop.
            None => self.sweep(q, if q.request.is_some() { 1 } else { page }),
        }
    }

    fn counts(&self, session: Option<&str>) -> Result<DecisionCounts, String> {
        // Whole-corpus counts are the sum of every file's, from the store's own definition; see
        // the module docs for why they are not recomputed here.
        let Some(s) = self.scope(session)? else {
            let (c, unreadable) = crate::corpus::sweep_counts(&self.at);
            return Ok(DecisionCounts {
                total: c.total,
                decided_by_operator: c.decided_by_operator,
                measured: c.measured,
                disagreements: c.disagreements,
                unreadable,
            });
        };
        self.scoped(&s, |conn| scoped_counts(conn, &s))
    }
}

/// One file's answer to a [`find_in`], before the page is taken: the rows this page could draw
/// from, and the numbers the answer reports. The sweep merges several of these, so none of them
/// pages to a global offset.
#[derive(Default)]
struct Found {
    rows: Vec<DecisionRow>,
    scanned: usize,
    matched: usize,
    verdicts: Vec<String>,
    tools: Vec<String>,
    deciders: Vec<String>,
}

impl Found {
    fn absorb(&mut self, other: Found) {
        self.rows.extend(other.rows);
        self.scanned += other.scanned;
        self.matched += other.matched;
        self.verdicts.extend(other.verdicts);
        self.tools.extend(other.tools);
        self.deciders.extend(other.deciders);
        for list in [&mut self.verdicts, &mut self.tools, &mut self.deciders] {
            list.sort();
            list.dedup();
            list.truncate(24);
        }
    }
}

/// Newest first, id breaking a tie: one order for a file and for the merged corpus.
fn newest_first(a: &DecisionRow, b: &DecisionRow) -> std::cmp::Ordering {
    b.decided_ms
        .cmp(&a.decided_ms)
        .then_with(|| b.request_id.cmp(&a.request_id))
}

/// One file's answer, paged: `last` caps the match set before the page is taken, so `last=64` with
/// a filter means the newest 64 MATCHING decisions - the same reading `transcript` gives it.
fn page_of(found: Found, q: &DecisionQuery, session: &str) -> DecisionHits {
    let cap = q
        .last
        .map(|n| (n as usize).min(found.matched))
        .unwrap_or(found.matched);
    let take = q.limit.min(cap.saturating_sub(q.offset));
    DecisionHits {
        rows: found.rows.into_iter().skip(q.offset).take(take).collect(),
        session_id: session.into(),
        scanned: found.scanned,
        matched: cap,
        verdicts_present: found.verdicts,
        tools_present: found.tools,
        deciders_present: found.deciders,
        unreadable: Vec::new(),
    }
}

/// One file's rows, counts and values for a query: the scope and the filters, then the newest
/// `page` rows. Nothing here skips an offset, which is what lets a sweep merge several files.
fn find_in(
    conn: &rusqlite::Connection,
    scope: Option<&str>,
    q: &DecisionQuery,
    page: usize,
) -> Result<Found, String> {
    let mut wheres: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = scope {
        wheres.push("session_id = ?1".to_string());
        binds.push(Box::new(s.to_string()));
    }
    // The scope's clause and binds, kept apart from the filters'. The miss report lists
    // what is present in the SCOPE, because a query that matched nothing has no values of its
    // own to list - so these are reused after the filter binds have been pushed.
    let scope_sql = wheres
        .first()
        .map(|w| format!(" WHERE {w}"))
        .unwrap_or_default();
    let scope_binds: Vec<Box<dyn rusqlite::ToSql>> = scope
        .map(|s| vec![Box::new(s.to_string()) as Box<dyn rusqlite::ToSql>])
        .unwrap_or_default();
    // "0 of 4000 searched" and "0 of 0" are different answers, and only one of
    // them is about the predicates.
    let scanned = scalar(
        conn,
        &format!("SELECT COUNT(*) FROM adjudication{scope_sql}"),
        &scope_binds,
    )?;

    if let Some(want) = &q.request {
        wheres.push(format!("request_id = ?{}", binds.len() + 1));
        binds.push(Box::new(want.clone()));
    }
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
    )?;

    let mut rows = Vec::new();
    if matched > 0 && page > 0 {
        let sql = format!(
            "SELECT {COLS} FROM adjudication{where_sql} ORDER BY decided_ms DESC, \
             request_id DESC LIMIT ?{}",
            binds.len() + 1
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("preparing the decision query: {e}"))?;
        let mut params: Vec<&dyn rusqlite::ToSql> = binds
            .iter()
            .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
            .collect();
        let take = page.min(matched) as i64;
        params.push(&take);
        rows = stmt
            .query_map(params.as_slice(), row_from)
            .map_err(|e| format!("reading decisions: {e}"))?
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| format!("reading decisions: {e}"))?;
    }

    Ok(Found {
        rows,
        scanned,
        matched,
        // Computed over the SCOPE, not over the filtered set - a miss report
        // listing "the verdicts present" from a query that matched nothing
        // would list nothing.
        verdicts: distinct(conn, "verdict", &scope_sql, &scope_binds),
        tools: distinct(conn, "tool", &scope_sql, &scope_binds),
        deciders: distinct(conn, "verdict_by", &scope_sql, &scope_binds),
    })
}

/// One file's counts for one session: `Store::corpus_counts`'s own definition with the session
/// bound, and its predicates imported rather than re-typed - a copied `WHERE` clause is exactly
/// how the two drifted before.
fn scoped_counts(conn: &rusqlite::Connection, session: &str) -> Result<DecisionCounts, String> {
    let one = |extra: &str| -> Result<u64, String> {
        scalar(
            conn,
            &format!("SELECT COUNT(*) FROM adjudication WHERE session_id = ?1{extra}"),
            &[Box::new(session.to_string()) as Box<dyn rusqlite::ToSql>],
        )
        .map(|n| n as u64)
    };
    Ok(DecisionCounts {
        total: one("")?,
        decided_by_operator: one(&format!(" AND {ASKED_SQL}"))?,
        measured: one(&format!(" AND {MEASURED_SQL}"))?,
        disagreements: one(&format!(" AND {DISAGREEMENT_SQL}"))?,
        unreadable: Vec::new(),
    })
}

/// One `COUNT` in one file. A count that failed is not a count of nothing: the sweep NAMES a file
/// it could not read (§ The corpus), and `.unwrap_or(0)` answers zero for a file that is not a
/// database.
fn scalar(
    conn: &rusqlite::Connection,
    sql: &str,
    binds: &[Box<dyn rusqlite::ToSql>],
) -> Result<usize, String> {
    let params: Vec<&dyn rusqlite::ToSql> = binds
        .iter()
        .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
        .collect();
    // A count that failed is not a count of nothing: the sweep NAMES a file it could not read
    // (§ The corpus), and `.unwrap_or(0)` would answer zero for a file that is not a database.
    conn.query_row(sql, params.as_slice(), |r| r.get::<_, i64>(0))
        .map(|n| n.max(0) as usize)
        .map_err(|e| format!("counting the corpus: {e}"))
}

/// The distinct values of one column in scope, for the miss report. Bounded: a
/// miss that answered with four hundred tool names would be its own problem.
fn distinct(
    conn: &rusqlite::Connection,
    column: &str,
    scope_sql: &str,
    binds: &[Box<dyn rusqlite::ToSql>],
) -> Vec<String> {
    // `column` is never caller-supplied - the three call sites pass literals - so
    // interpolating it is safe, and the scope is still bound.
    let joiner = if scope_sql.is_empty() { "WHERE" } else { "AND" };
    let sql = format!(
        "SELECT DISTINCT {column} FROM adjudication{scope_sql} \
         {joiner} {column} IS NOT NULL AND {column} != '' LIMIT 24"
    );
    let params: Vec<&dyn rusqlite::ToSql> = binds
        .iter()
        .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
        .collect();
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
