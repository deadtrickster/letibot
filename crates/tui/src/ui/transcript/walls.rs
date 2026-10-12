//! **How many screen rows the session's notices take, measured on real rows** (R89).
//!
//! Not a test of behaviour — a measurement, run by hand: the rows are exported read-only from a
//! `sessions.db` (agent-spoken user rows and System rows, texts deduplicated) to JSONL, one
//! `{"kind":"agent"|"system","origin":…,"ts":…,"text":…}` an line, and every row is drawn
//! through the transcript's own renderers ([`session_row_lines`], [`system_row_lines`]) at 100
//! columns. A row is a WALL when it draws more screen rows than it has notices — one line a
//! notice is the rule (`line_cards`), with the job card's two lines a job the one exception the
//! operator asked for.
//!
//! ```text
//! LETIBOT_WALL_ROWS=rows.jsonl cargo test -p letibot-tui walls -- --ignored --nocapture
//! ```

#[cfg(test)]
mod tests {
    use crate::ui::render::RenderConfig;
    use crate::ui::transcript::item::system_row_lines;
    use crate::ui::transcript::user::session_row_lines;
    use std::collections::{BTreeMap, HashMap};

    /// The key the reviewer's scripts group by: a `[tag]`, or the first three words.
    fn key(text: &str) -> String {
        if text.starts_with('[')
            && let Some(end) = text.find(']')
            && text[1..end].chars().all(|c| c.is_ascii_lowercase() || c == ' ')
        {
            return text[..=end].to_string();
        }
        text.split_whitespace().take(3).collect::<Vec<_>>().join(" ")
    }

    /// The most lines a row may draw and not be a wall: a line a notice (a `\n\n` group) and a
    /// line a settlement bullet, and a second line for each job's.
    fn allowed(text: &str) -> usize {
        let groups = text.split("\n\n").count();
        let bullets = text.matches("\n  - ").count();
        let jobs = text.matches("\n  - `j").count();
        groups + bullets + jobs
    }

    #[test]
    #[ignore = "a measurement over an exported sessions.db, run by hand"]
    fn walls() {
        let Some(path) = std::env::var_os("LETIBOT_WALL_ROWS") else {
            eprintln!("LETIBOT_WALL_ROWS is not set: nothing to measure");
            return;
        };
        let cfg = RenderConfig {
            width: 100,
            color: false,
            ..RenderConfig::default()
        };
        let tails = HashMap::new();
        // key → (rows, walls, screen rows, max screen rows)
        let mut by: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
        let mut totals = (0usize, 0usize, 0usize);
        for line in std::fs::read_to_string(path).expect("rows").lines() {
            let v: serde_json::Value = serde_json::from_str(line).expect("a row");
            let text = v["text"].as_str().unwrap_or_default();
            let ts = v["ts"].as_u64().unwrap_or(0);
            let (k, drawn) = if v["kind"] == "system" {
                let origin = match v["origin"].as_str() {
                    Some("bootstrap") => letibot_transcript::SystemOrigin::Bootstrap,
                    _ => letibot_transcript::SystemOrigin::Update,
                };
                (
                    format!("system: {}", key(text)),
                    system_row_lines(text, origin, ts, &cfg).len(),
                )
            } else {
                (key(text), session_row_lines(text, ts, &cfg, &[], &tails).len())
            };
            let wall = drawn > allowed(text);
            let e = by.entry(k).or_default();
            e.0 += 1;
            e.1 += usize::from(wall);
            e.2 += drawn;
            e.3 = e.3.max(drawn);
            totals.0 += 1;
            totals.1 += usize::from(wall);
            totals.2 += drawn;
        }
        let mut rows: Vec<_> = by.into_iter().collect();
        rows.sort_by(|a, b| b.1.2.cmp(&a.1.2));
        println!(
            "{:<40} {:>6} {:>6} {:>9} {:>6}",
            "notice", "rows", "walls", "screen", "max"
        );
        for (k, (n, w, s, m)) in rows {
            if s > 0 {
                println!("{k:<40.40} {n:>6} {w:>6} {s:>9} {m:>6}");
            }
        }
        println!(
            "TOTAL rows={} walls={} screen_rows={}",
            totals.0, totals.1, totals.2
        );
    }
}
