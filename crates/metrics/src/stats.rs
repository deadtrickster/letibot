//! **`letibot stats`** — the read side: §3's questions as queries, each answering a table.
//!
//! Opens `metrics.db` **read-only**, loads the views ([`crate::store::VIEWS`], `TEMP`), and never
//! writes, never starts a daemon. Phase 1's verbs (§5): the day's summary, turns, worker waits
//! (Q3), deliveries and wakes (Q1/Q2), tool reliability (Q8), latency and throughput per model
//! (Q7), the operator's marks with their turns' numbers (Q15), one session's timeline, and raw
//! SQL with the views loaded.
//!
//! Percentiles are computed here rather than in SQL: `PERCENT_RANK` over a window is the doc's
//! spelling, and it costs a sort per partition the reader does anyway — reading the column and
//! sorting it once is the same work with one less clause to get wrong.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, params};

/// One answer: column names and rows of text, printed by [`Table::render`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    pub title: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    fn new(title: &str, headers: &[&str]) -> Table {
        Table {
            title: title.to_string(),
            headers: headers.iter().map(|h| h.to_string()).collect(),
            rows: Vec::new(),
        }
    }

    /// Aligned columns under a title line; numbers right-aligned, text left.
    pub fn render(&self) -> String {
        let mut w: Vec<usize> = self.headers.iter().map(|h| h.chars().count()).collect();
        for r in &self.rows {
            for (i, c) in r.iter().enumerate() {
                if i < w.len() {
                    w[i] = w[i].max(c.chars().count());
                }
            }
        }
        let numeric = |s: &str| !s.is_empty() && s.parse::<f64>().is_ok();
        let line = |cells: &[String]| {
            cells
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let pad = w.get(i).copied().unwrap_or(0);
                    if numeric(c) {
                        format!("{c:>pad$}")
                    } else {
                        format!("{c:<pad$}")
                    }
                })
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_string()
        };
        let mut out = String::new();
        if !self.title.is_empty() {
            out.push_str(&format!("{}\n", self.title));
        }
        out.push_str(&line(&self.headers));
        out.push('\n');
        for r in &self.rows {
            out.push_str(&line(r));
            out.push('\n');
        }
        if self.rows.is_empty() {
            out.push_str("(no rows)\n");
        }
        out
    }
}

/// Open the file read-only and load the views. A missing file is said, not created.
pub fn open(path: &Path) -> Result<Connection, String> {
    if !path.exists() {
        return Err(format!(
            "no metrics file at {} — no daemon with metrics on has written one yet",
            path.display()
        ));
    }
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    let _ = c.busy_timeout(std::time::Duration::from_secs(2));
    c.execute_batch(crate::store::VIEWS)
        .map_err(|e| format!("the views did not load: {e}"))?;
    Ok(c)
}

/// `2d`, `12h`, `30m`, `90s`, `all` → milliseconds back from now (`all` is everything).
pub fn parse_since(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s == "all" {
        return Ok(u64::MAX);
    }
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num
        .parse()
        .map_err(|_| format!("`{s}` is not a span — try 2d, 12h, 30m or all"))?;
    let per = match unit {
        "d" => 86_400_000,
        "h" | "" => 3_600_000,
        "m" => 60_000,
        "s" => 1_000,
        _ => return Err(format!("`{s}`: the unit is d, h, m or s")),
    };
    Ok(n.saturating_mul(per))
}

/// The `ts_ms` floor for a span back from `now`.
pub fn floor(now_ms: u64, back_ms: u64) -> i64 {
    now_ms.saturating_sub(back_ms) as i64
}

fn cell(v: ValueRef<'_>) -> String {
    match v {
        ValueRef::Null => String::new(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{f:.0}")
            } else {
                format!("{f:.2}")
            }
        }
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        ValueRef::Blob(b) => format!("<{} bytes>", b.len()),
    }
}

/// **Any query, as a table** — `letibot stats sql`, and every verb below that is one query.
pub fn query(
    c: &Connection,
    title: &str,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Table, String> {
    let mut st = c.prepare(sql).map_err(|e| e.to_string())?;
    let headers: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let n = headers.len();
    let mut rows = st.query(args).map_err(|e| e.to_string())?;
    let mut t = Table {
        title: title.to_string(),
        headers,
        rows: Vec::new(),
    };
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        t.rows.push(
            (0..n)
                .map(|i| r.get_ref(i).map(cell).unwrap_or_default())
                .collect(),
        );
    }
    Ok(t)
}

fn pct(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

/// **The day at a glance**: turns, rounds, tool calls and errors, cost, the worker's p95 wait,
/// deliveries mid-turn, wakes, marks, and the writer's own drops.
pub fn summary(c: &Connection, since: i64) -> Result<Table, String> {
    let one = |sql: &str| -> Result<String, String> {
        c.query_row(sql, params![since], |r| r.get_ref(0).map(cell))
            .map_err(|e| e.to_string())
    };
    let mut waits: Vec<i64> = {
        let mut st = c
            .prepare("SELECT n1 FROM event WHERE kind = 'worker.cmd' AND ts_ms >= ?1")
            .map_err(|e| e.to_string())?;
        st.query_map(params![since], |r| r.get::<_, Option<i64>>(0))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok().flatten())
            .collect()
    };
    waits.sort_unstable();
    let mut t = Table::new("", &["measure", "value"]);
    let mut put = |k: &str, v: String| t.rows.push(vec![k.to_string(), v]);
    put(
        "turns",
        one("SELECT COUNT(*) FROM event WHERE kind = 'turn.end' AND ts_ms >= ?1")?,
    );
    put(
        "rounds",
        one("SELECT COUNT(*) FROM event WHERE kind = 'round' AND ts_ms >= ?1")?,
    );
    put(
        "tool calls",
        one("SELECT COUNT(*) FROM event WHERE kind = 'tool.call' AND ts_ms >= ?1")?,
    );
    put(
        "tool errors",
        one(
            "SELECT COUNT(*) FROM event WHERE kind = 'tool.call' AND ts_ms >= ?1 \
             AND outcome <> 'ok'",
        )?,
    );
    put(
        "cost (USD)",
        one(
            "SELECT printf('%.4f', COALESCE(SUM(attrs->>'cost_micros_usd'), 0) / 1e6) \
             FROM event WHERE kind = 'turn.end' AND ts_ms >= ?1",
        )?,
    );
    put("worker commands", waits.len().to_string());
    put("worker wait p95 (ms)", pct(&waits, 0.95).to_string());
    put(
        "worker wait max (ms)",
        waits.last().copied().unwrap_or(0).to_string(),
    );
    put(
        "deliveries",
        one("SELECT COUNT(*) FROM event WHERE kind = 'delivery' AND ts_ms >= ?1")?,
    );
    put(
        "deliveries mid-turn",
        one(
            "SELECT COUNT(*) FROM event WHERE kind = 'delivery' AND ts_ms >= ?1 \
             AND attrs->>'mid_turn' = 1",
        )?,
    );
    put(
        "wakes that ran a turn",
        one("SELECT COUNT(*) FROM event WHERE kind = 'wake' AND ts_ms >= ?1 AND outcome = 'ran'")?,
    );
    put(
        "nags",
        one("SELECT COUNT(*) FROM event WHERE kind = 'nag' AND ts_ms >= ?1")?,
    );
    put(
        "compactions",
        one("SELECT COUNT(*) FROM event WHERE kind = 'compaction' AND ts_ms >= ?1")?,
    );
    put(
        "marks good / bad / note",
        one(
            "SELECT SUM(name = 'good') || ' / ' || SUM(name = 'bad') || ' / ' || SUM(name = 'note') \
             FROM event WHERE kind = 'operator.feedback' AND ts_ms >= ?1",
        )?,
    );
    put(
        "events dropped",
        one(
            "SELECT COALESCE(SUM(n1), 0) FROM event WHERE kind = 'metrics.dropped' \
             AND ts_ms >= ?1",
        )?,
    );
    Ok(t)
}

/// **Q3 — the worker's queue wait per command verb, per build** (c12d044).
pub fn waits(c: &Connection, since: i64) -> Result<Table, String> {
    let mut st = c
        .prepare(
            "SELECT build, name, n1, dur_ms FROM event WHERE kind = 'worker.cmd' AND ts_ms >= ?1",
        )
        .map_err(|e| e.to_string())?;
    let mut by: BTreeMap<(String, String), (Vec<i64>, Vec<i64>)> = BTreeMap::new();
    let rows = st
        .query_map(params![since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            ))
        })
        .map_err(|e| e.to_string())?;
    for r in rows.flatten() {
        let e = by.entry((r.0, r.1)).or_default();
        e.0.push(r.2);
        e.1.push(r.3);
    }
    let mut t = Table::new(
        "worker queue wait per command (enqueue → dequeue), and the command's own time",
        &[
            "build",
            "verb",
            "n",
            "p50_ms",
            "p95_ms",
            "max_ms",
            "over_5s",
            "run_p95_ms",
        ],
    );
    let mut rows: Vec<_> = by.into_iter().collect();
    rows.sort_by_key(|(_, (w, _))| std::cmp::Reverse(w.iter().copied().max().unwrap_or(0)));
    for ((build, verb), (mut w, mut d)) in rows {
        w.sort_unstable();
        d.sort_unstable();
        t.rows.push(vec![
            build,
            verb,
            w.len().to_string(),
            pct(&w, 0.5).to_string(),
            pct(&w, 0.95).to_string(),
            w.last().copied().unwrap_or(0).to_string(),
            w.iter().filter(|x| **x > 5000).count().to_string(),
            pct(&d, 0.95).to_string(),
        ]);
    }
    Ok(t)
}

/// **Q1 and Q2 — what arrives, how, and mid-turn or not; and the wakes by cause** (R79).
pub fn deliveries(c: &Connection, since: i64) -> Result<Vec<Table>, String> {
    Ok(vec![
        query(
            c,
            "deliveries: what, how, by which rule and door",
            "SELECT what, how, COALESCE(rule, '') AS rule, COALESCE(door, '') AS door, \
                    COUNT(*) AS n, SUM(mid_turn = 1) AS mid_turn, \
                    CAST(AVG(held_ms) AS INT) AS avg_held_ms, MAX(held_ms) AS max_held_ms \
             FROM delivery WHERE ts_ms >= ?1 GROUP BY 1, 2, 3, 4 ORDER BY n DESC",
            &[&since],
        )?,
        query(
            c,
            "wakes by cause",
            "SELECT cause, outcome, COUNT(*) AS n, SUM(items) AS items \
             FROM wake WHERE ts_ms >= ?1 GROUP BY 1, 2 ORDER BY n DESC",
            &[&since],
        )?,
        query(
            c,
            "turns by door",
            "SELECT door, COALESCE(cause, '') AS cause, COUNT(*) AS n \
             FROM turn_start WHERE ts_ms >= ?1 GROUP BY 1, 2 ORDER BY n DESC",
            &[&since],
        )?,
    ])
}

/// **Q8 — tool reliability**: error rate, duration, memory peak against cap.
pub fn tools(c: &Connection, since: i64) -> Result<Table, String> {
    query(
        c,
        "tool calls",
        "SELECT tool, COUNT(*) AS n, ROUND(AVG(outcome <> 'ok'), 3) AS err_rate, \
                CAST(AVG(dur_ms) AS INT) AS avg_ms, MAX(dur_ms) AS max_ms, \
                CAST(AVG(full_bytes) AS INT) AS avg_bytes, MAX(peak_kib) AS max_peak_kib, \
                SUM(peak_kib > 0.9 * cap_kib) AS near_cap \
         FROM tool_call WHERE ts_ms >= ?1 GROUP BY 1 ORDER BY n DESC",
        &[&since],
    )
}

/// **Q7 — latency and throughput per model**: time to first token, tok/s, cache hit.
pub fn models(c: &Connection, since: i64) -> Result<Table, String> {
    let mut st = c
        .prepare(
            "SELECT model, COALESCE(provider, ''), ttfb_ms, output_tokens, predicted_ms, \
                    dur_ms, prompt_tokens, cached_tokens \
             FROM round_event WHERE ts_ms >= ?1",
        )
        .map_err(|e| e.to_string())?;
    #[derive(Default)]
    struct Acc {
        ttfb: Vec<i64>,
        toks: Vec<f64>,
        prompt: i64,
        cached: i64,
        n: usize,
    }
    let mut by: BTreeMap<(String, String), Acc> = BTreeMap::new();
    let rows = st
        .query_map(params![since], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                r.get::<_, Option<i64>>(7)?.unwrap_or(0),
            ))
        })
        .map_err(|e| e.to_string())?;
    for (model, provider, ttfb, out, predicted_ms, dur, prompt, cached) in rows.flatten() {
        let a = by.entry((model, provider)).or_default();
        a.n += 1;
        if let Some(t) = ttfb {
            a.ttfb.push(t);
        }
        // The generation's own time when the server reported one, else the round's wall
        // minus its first-token wait — a cloud round has no `predicted_ms` of its own.
        let gen_ms = if predicted_ms > 0.0 {
            predicted_ms
        } else {
            (dur - ttfb.unwrap_or(0)).max(0) as f64
        };
        if out > 0 && gen_ms > 0.0 {
            a.toks.push(out as f64 * 1000.0 / gen_ms);
        }
        a.prompt += prompt;
        a.cached += cached;
    }
    let mut t = Table::new(
        "model rounds: time to first token, generation speed, cache hit",
        &[
            "model",
            "provider",
            "rounds",
            "ttfb_p50_ms",
            "ttfb_p95_ms",
            "tok_s_avg",
            "cache_hit",
        ],
    );
    for ((model, provider), mut a) in by {
        a.ttfb.sort_unstable();
        let toks = if a.toks.is_empty() {
            String::new()
        } else {
            format!("{:.1}", a.toks.iter().sum::<f64>() / a.toks.len() as f64)
        };
        let hit = if a.prompt > 0 {
            format!("{:.2}", a.cached as f64 / a.prompt as f64)
        } else {
            String::new()
        };
        t.rows.push(vec![
            model,
            provider,
            a.n.to_string(),
            if a.ttfb.is_empty() {
                String::new()
            } else {
                pct(&a.ttfb, 0.5).to_string()
            },
            if a.ttfb.is_empty() {
                String::new()
            } else {
                pct(&a.ttfb, 0.95).to_string()
            },
            toks,
            hit,
        ]);
    }
    Ok(t)
}

/// **The turns, newest first**, with the marks against each.
pub fn turns(
    c: &Connection,
    since: i64,
    session: Option<&str>,
    limit: u32,
) -> Result<Table, String> {
    query(
        c,
        "turns, newest first",
        "SELECT datetime(e.ts_ms / 1000, 'unixepoch', 'localtime') AS ended, \
                substr(e.session_id, 1, 22) AS session, e.door, e.model, e.outcome, \
                e.rounds, e.tool_calls AS tools, e.tool_errors AS errs, \
                e.prompt_tokens AS prompt, e.output_tokens AS out, \
                CAST(e.dur_ms / 1000 AS INT) AS secs, e.ttfb_ms, \
                (SELECT group_concat(f.name, ',') FROM event f \
                 WHERE f.kind = 'operator.feedback' AND f.turn_id = e.turn_id) AS marks \
         FROM turn_end e WHERE e.ts_ms >= ?1 AND (?2 IS NULL OR e.session_id = ?2) \
         ORDER BY e.ts_ms DESC LIMIT ?3",
        &[&since, &session, &limit],
    )
}

/// **Q15 — every mark with its turn's numbers beside it.**
pub fn feedback(
    c: &Connection,
    since: i64,
    session: Option<&str>,
    only_bad: bool,
) -> Result<Table, String> {
    query(
        c,
        "the operator's marks",
        "SELECT datetime(f.ts_ms / 1000, 'unixepoch', 'localtime') AS at, \
                substr(f.session_id, 1, 22) AS session, f.polarity, \
                COALESCE(f.turn_id, '') AS turn, f.round, f.at_state, \
                COALESCE(f.tags, '') AS tags, COALESCE(f.note, '') AS note, \
                t.model, t.rounds, t.tool_errors AS errs, CAST(t.dur_ms / 1000 AS INT) AS secs \
         FROM feedback f LEFT JOIN turn_end t ON t.turn_id = f.turn_id \
         WHERE f.ts_ms >= ?1 AND (?2 IS NULL OR f.session_id = ?2) \
               AND (?3 = 0 OR f.polarity = 'bad') AND COALESCE(f.outcome, '') <> 'withdrawn' \
         ORDER BY f.ts_ms DESC",
        &[&since, &session, &(only_bad as i64)],
    )
}

/// **One session as a timeline**: a line per turn, deliveries, wakes, nags, compactions and
/// marks in time order.
pub fn session(c: &Connection, session: &str) -> Result<Table, String> {
    query(
        c,
        &format!("session {session}"),
        "SELECT datetime(ts_ms / 1000, 'unixepoch', 'localtime') AS at, kind, \
                COALESCE(name, '') AS name, COALESCE(outcome, '') AS outcome, \
                COALESCE(dur_ms, '') AS dur_ms, COALESCE(n1, '') AS n1, COALESCE(n2, '') AS n2, \
                COALESCE(n3, '') AS n3, \
                COALESCE(attrs->>'note', attrs->>'row', attrs->>'handle', '') AS detail \
         FROM event WHERE session_id = ?1 \
               AND kind IN ('turn.start', 'turn.end', 'delivery', 'wake', 'nag', 'nag.followup', \
                            'compaction', 'wall', 'context.claim', 'operator.feedback') \
         ORDER BY ts_ms, id",
        &[&session],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Identity, Settings, Writer};
    use crate::{Event, kind};

    fn db() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "letibot-stats-{}-{}-{}",
            std::process::id(),
            crate::now_ms(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("metrics.db");
        let mut w = Writer::new(
            crate::store::open(&p).unwrap(),
            Identity {
                daemon: "box/1/0".into(),
                build: "b1".into(),
            },
            Settings::default(),
        );
        let t = crate::now_ms() - 1000;
        let mut batch = vec![
            Event::new(kind::TURN_START)
                .at(t - 5000)
                .session("s-1")
                .turn("t1", None)
                .name("prompt"),
            Event::new(kind::TURN_END)
                .at(t)
                .dur(5000)
                .session("s-1")
                .turn("t1", None)
                .name("glm/glm-5")
                .outcome("eos")
                .n1(1000)
                .n2(800)
                .n3(50)
                .attr("rounds", 2)
                .attr("tool_calls", 1)
                .attr("tool_errors", 1)
                .attr("cost_micros_usd", 1500),
            Event::new(kind::ROUND)
                .at(t)
                .dur(3000)
                .session("s-1")
                .turn("t1", Some(0))
                .name("glm/glm-5")
                .n1(1000)
                .n2(800)
                .n3(50)
                .attr("ttfb_ms", 500),
            Event::new(kind::TOOL_CALL)
                .at(t)
                .dur(20)
                .session("s-1")
                .turn("t1", Some(0))
                .name("bash")
                .outcome("error")
                .n1(10)
                .n2(10)
                .n3(950)
                .attr("cap_kib", 1000),
            Event::new(kind::DELIVERY)
                .at(t)
                .dur(4000)
                .session("s-1")
                .name("job")
                .outcome("immediate")
                .n1(1)
                .attr("door", "steer")
                .attr("mid_turn", true),
            Event::new(kind::WAKE)
                .at(t)
                .session("s-1")
                .name("job")
                .outcome("ran")
                .n1(1),
            Event::new(kind::FEEDBACK)
                .at(t)
                .session("s-1")
                .turn("t1", Some(1))
                .name("bad")
                .attr("note", "#distracted got pulled off")
                .attr("tags", serde_json::json!(["distracted"])),
        ];
        for (verb, q) in [
            ("prompt", 10),
            ("prompt", 20),
            ("prompt", 9000),
            ("slash", 5),
        ] {
            batch.push(
                Event::new(kind::WORKER_CMD)
                    .at(t)
                    .dur(1)
                    .name(verb)
                    .outcome("ok")
                    .n1(q),
            );
        }
        w.write_batch(&batch).unwrap();
        p
    }

    #[test]
    fn every_verb_runs_against_a_temp_db_and_answers_what_was_written() {
        let p = db();
        let c = open(&p).unwrap();
        let since = floor(crate::now_ms(), parse_since("1d").unwrap());

        let s = summary(&c, since).unwrap();
        let get = |k: &str| {
            s.rows
                .iter()
                .find(|r| r[0] == k)
                .map(|r| r[1].clone())
                .unwrap()
        };
        assert_eq!(get("turns"), "1");
        assert_eq!(get("tool errors"), "1");
        assert_eq!(get("cost (USD)"), "0.0015");
        assert_eq!(get("worker wait max (ms)"), "9000");
        assert_eq!(get("deliveries mid-turn"), "1");
        assert_eq!(get("marks good / bad / note"), "0 / 1 / 0");

        let w = waits(&c, since).unwrap();
        assert_eq!(w.rows[0][1], "prompt", "the worst verb first");
        assert_eq!(w.rows[0][6], "1", "one prompt waited over 5 s");

        let d = deliveries(&c, since).unwrap();
        assert_eq!(d[0].rows[0][0], "job");
        assert_eq!(d[0].rows[0][5], "1", "mid-turn");
        assert_eq!(d[1].rows[0][0], "job", "the wake's cause");

        let t = tools(&c, since).unwrap();
        assert_eq!(t.rows[0][0], "bash");
        assert_eq!(t.rows[0][7], "1", "950 of 1000 KiB is near the cap");

        let m = models(&c, since).unwrap();
        assert_eq!(m.rows[0][0], "glm/glm-5");
        assert_eq!(m.rows[0][3], "500");
        assert_eq!(m.rows[0][5], "20.0", "50 tokens in 3000 - 500 ms");

        let tr = turns(&c, since, None, 20).unwrap();
        assert_eq!(tr.rows.len(), 1);
        assert_eq!(
            tr.rows[0].last().unwrap(),
            "bad",
            "the mark rides with its turn"
        );

        let f = feedback(&c, since, Some("s-1"), true).unwrap();
        assert_eq!(f.rows.len(), 1);
        assert!(f.render().contains("#distracted"));

        let tl = session(&c, "s-1").unwrap();
        assert!(tl.rows.iter().any(|r| r[1] == "operator.feedback"));

        let q = query(&c, "", "SELECT COUNT(*) AS n FROM tool_call", &[]).unwrap();
        assert_eq!(q.rows[0][0], "1");
    }

    #[test]
    fn a_reader_never_writes() {
        let p = db();
        let c = open(&p).unwrap();
        assert!(
            query(&c, "", "DELETE FROM event", &[]).is_err(),
            "the connection is read-only"
        );
    }

    #[test]
    fn spans_parse_and_a_missing_file_is_said() {
        assert_eq!(parse_since("2d").unwrap(), 2 * 86_400_000);
        assert_eq!(parse_since("90m").unwrap(), 90 * 60_000);
        assert_eq!(parse_since("all").unwrap(), u64::MAX);
        assert!(parse_since("soon").is_err());
        assert!(open(Path::new("/nonexistent/metrics.db")).is_err());
    }
}
