//! **The etalon, read against layer A as it is today.**
//!
//! Plan: `docs/guard-corpus-plan.md` §2 and §7. The etalon is every tool call
//! this operator sat through — Claude Code, opencode and letibot, on every box
//! that offered its transcripts — with the operator's words beside it and what
//! became of it: it ran, it errored, or it was refused. Extracted by
//! `scripts/etalon-extract.py` into JSONL; commands and utterances only, never
//! tool output.
//!
//! This module answers one question about a layer-A change before it lands:
//! **what would it have done to the calls that actually happened?** Each row's
//! command is read by `Baseline::of_command` — today's classifier, today's lists
//! — and its tier is crossed with the row's outcome. Two cells matter:
//!
//! * **ran × always_ask** — calls the operator let run (or did not stop) that
//!   layer A would now put in front of them every time. In automode that is a
//!   prompt the operator would not have had; the rule that fired is named so a
//!   port can be judged rule by rule.
//! * **refused × always_ask** — calls a human (or Claude Code's classifier)
//!   refused that layer A would catch before any model is asked. The point of
//!   a port.
//!
//! A rule that grows the first cell without growing the second does not land
//! (plan §2). The rest of the table — `may_approve`, `auto`, `not_run` — says
//! how much of the corpus reaches the model at all, and how much layer A cannot
//! read (a bare name under an undeclared shell, an unresolvable construct).
//!
//! What this is NOT: a measurement of the gatekeeper model. That is
//! `calibrate.rs`, over the 402 labelled rows. This is layer A alone, on forty
//! thousand real commands.

use std::collections::BTreeMap;
use std::path::Path;

use letibot_tools::adjudicate::Tier;
use letibot_tools::intent::{Baseline, BaselineVerdict, Surroundings};

/// One row of the JSONL, the fields this report reads.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Row {
    pub source: String,
    #[serde(default)]
    pub session: String,
    /// Where the call ran. The scope half of layer A — inside or outside the
    /// project — is judged against THIS, not against the daemon's own
    /// workspace: the rows come from many projects, and judging an emacs
    /// session's `cp` against letibot's tree calls every one of them
    /// "outside the project".
    #[serde(default)]
    pub cwd: String,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub outcome: String,
    #[serde(default)]
    pub label: Option<serde_json::Value>,
}

/// What layer A said about one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Read {
    Auto,
    MayApprove,
    AlwaysAsk,
    Blocked,
    NotRun,
}

impl Read {
    fn of(b: &Baseline) -> Read {
        if matches!(b.verdict, BaselineVerdict::NotRun { .. }) {
            return Read::NotRun;
        }
        match b.tier {
            Tier::Auto => Read::Auto,
            Tier::MayApprove => Read::MayApprove,
            Tier::AlwaysAsk { .. } => Read::AlwaysAsk,
            Tier::Blocked { .. } => Read::Blocked,
        }
    }
    fn word(self) -> &'static str {
        match self {
            Read::Auto => "auto",
            Read::MayApprove => "may_approve",
            Read::AlwaysAsk => "always_ask",
            Read::Blocked => "blocked",
            Read::NotRun => "not_run",
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub rows: usize,
    pub commands: usize,
    /// (source, outcome, read) → count.
    pub cells: BTreeMap<(String, String, Read), usize>,
    /// Which `always_ask` rule fired on a row that RAN, and how often — the
    /// false-prompt table, by rule.
    pub prompts_by_rule: BTreeMap<&'static str, usize>,
    /// Which rule fired on a row that was REFUSED — the catches, by rule.
    pub catches_by_rule: BTreeMap<&'static str, usize>,
    /// A few examples per rule of a ran-row it would prompt on, so a reader can
    /// see the command and not only the count.
    pub prompt_examples: BTreeMap<&'static str, Vec<String>>,
    /// Why layer A could not read a command, by first sentence, with counts.
    pub not_run_why: BTreeMap<String, usize>,
    /// The constructs that made a command unreadable, by how many commands
    /// each appears in — and, separately, how many commands each construct is
    /// the ONLY reason for. The second number is what resolving that one
    /// construct would recover; the first is only how common it is.
    pub not_run_constructs: BTreeMap<String, (usize, usize)>,
    /// Where here-document bodies go, by count of commands: the program that
    /// reads the body on stdin, or the file it is redirected into (by its
    /// extension or directory class). Plan §4b: a body headed somewhere
    /// executable is a program, and this is how many there are to read.
    pub heredoc_sinks: BTreeMap<String, usize>,
    /// Program heads layer A does not know (`Intent::Unknown`), by count of
    /// commands they appear in. The list the next tranche of heads is chosen
    /// from — by what this operator actually runs, not by a paper's table.
    pub unknown_heads: BTreeMap<String, usize>,
}

/// Read the JSONL and cross every `bash` row with today's layer A.
pub fn report(path: &Path, env: &Surroundings, limit: usize) -> Result<Report, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut r = Report::default();
    // `LETIBOT_ETALON_DUMP=path` writes one line per command — its row number,
    // read and rule — so two builds can be diffed row by row, which the summary
    // cannot do: a rule that gains 500 and loses 500 shows as unchanged.
    let mut dump = std::env::var_os("LETIBOT_ETALON_DUMP").map(|p| std::io::BufWriter::new(std::fs::File::create(p).expect("dump path")));
    for (row_no, line) in text.lines().take(limit).enumerate() {
        let row: Row = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        r.rows += 1;
        if row.tool != "bash" {
            continue;
        }
        let Some(cmd) = row.arguments.get("command").and_then(|v| v.as_str()) else {
            continue;
        };
        r.commands += 1;
        let own;
        let env_for_row = if row.cwd.is_empty() {
            env
        } else {
            own = Surroundings {
                workspace: Some(row.cwd.clone().into()),
                ..env.clone()
            };
            &own
        };
        let b = Baseline::of_command(cmd, env_for_row);
        let read = Read::of(&b);
        if b.intents.contains(&letibot_tools::intent::Intent::Unknown) {
            if let Some(n) = &b.command {
                for st in &n.stages {
                    if let letibot_code::shell::Word::Literal(p) = &st.program {
                        let base = p.rsplit('/').next().unwrap_or(p).to_string();
                        // Only the stages the table does not name: re-read THIS
                        // stage's own text, so `git status` is judged as `git
                        // status` and not as a bare `git`, which has no sub-verb
                        // and would read as unknown for the wrong reason.
                        let text = n.source.get(st.span.start..st.span.end).unwrap_or(&base);
                        let probe = Baseline::of_command(text, env_for_row);
                        if probe.intents.contains(&letibot_tools::intent::Intent::Unknown) {
                            *r.unknown_heads.entry(base).or_default() += 1;
                        }
                    }
                }
            }
        }
        if let Some(n) = &b.command {
            for st in &n.stages {
                let Some(body) = st.redirects.iter().find_map(|rd| match &rd.target {
                    letibot_code::shell::RedirectTarget::HereDoc { body, .. } => Some(body),
                    _ => None,
                }) else {
                    continue;
                };
                let prog = st.program_name().unwrap_or("<unresolved>");
                let file = st.redirects.iter().find_map(|rd| match &rd.target {
                    letibot_code::shell::RedirectTarget::File(w) if rd.op.writes() => w.text().map(str::to_string),
                    _ => None,
                });
                let shebang = body.trim_start().starts_with("#!");
                let key = match file {
                    Some(f) => {
                        let ext = f.rsplit('/').next().unwrap_or(&f).rsplit_once('.').map(|(_, e)| e.to_string());
                        format!("{prog} > {}{}", ext.map(|e| format!(".{e}")).unwrap_or_else(|| "(no ext)".into()), if shebang { " #!" } else { "" })
                    }
                    None => format!("{prog}{}", if shebang { " #!" } else { "" }),
                };
                *r.heredoc_sinks.entry(key).or_default() += 1;
            }
        }
        if let Some(d) = dump.as_mut() {
            use std::io::Write;
            let rule = match (&b.tier, &b.verdict) {
                (_, BaselineVerdict::NotRun { .. }) => "not_run".to_string(),
                (Tier::AlwaysAsk { rule, .. }, _) => rule.to_string(),
                (Tier::Blocked { rule, .. }, _) => rule.as_str().to_string(),
                (Tier::Auto, _) => "auto".into(),
                (Tier::MayApprove, _) => "may_approve".into(),
            };
            let _ = writeln!(d, "{row_no}\t{}\t{}\t{rule}", row.source, row.outcome);
        }
        *r.cells
            .entry((row.source.clone(), row.outcome.clone(), read))
            .or_default() += 1;
        match (&b.tier, &b.verdict) {
            (_, BaselineVerdict::NotRun { why }) => {
                let key = why.split(['.', '\n']).next().unwrap_or(why).trim().to_string();
                *r.not_run_why.entry(key).or_default() += 1;
                if let Some(n) = &b.command {
                    let mut kinds: Vec<&str> = n.unresolved.iter().map(|u| u.construct.as_str()).collect();
                    kinds.sort();
                    kinds.dedup();
                    let alone = kinds.len() == 1;
                    for k in kinds {
                        let e = r.not_run_constructs.entry(k.to_string()).or_default();
                        e.0 += 1;
                        if alone {
                            e.1 += 1;
                        }
                    }
                }
            }
            (Tier::AlwaysAsk { rule, .. }, _) => {
                if row.outcome == "refused" {
                    *r.catches_by_rule.entry(rule).or_default() += 1;
                } else if row.outcome == "ran" {
                    *r.prompts_by_rule.entry(rule).or_default() += 1;
                    let ex = r.prompt_examples.entry(rule).or_default();
                    if ex.len() < 3 {
                        ex.push(one_line(cmd));
                    }
                }
            }
            (Tier::Blocked { rule, .. }, _) => {
                let name: &'static str = rule.as_str();
                if row.outcome == "refused" {
                    *r.catches_by_rule.entry(name).or_default() += 1;
                } else if row.outcome == "ran" {
                    *r.prompts_by_rule.entry(name).or_default() += 1;
                    let ex = r.prompt_examples.entry(name).or_default();
                    if ex.len() < 3 {
                        ex.push(one_line(cmd));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(r)
}

fn one_line(s: &str) -> String {
    let flat = s.replace('\n', " ");
    if flat.chars().count() > 110 {
        format!("{}…", flat.chars().take(110).collect::<String>())
    } else {
        flat
    }
}

impl Report {
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "etalon: {} rows, {} bash commands read by layer A as it is now\n\n",
            self.rows, self.commands
        ));
        // The table: rows are (source, outcome); columns are the reads.
        let reads = [Read::Auto, Read::MayApprove, Read::AlwaysAsk, Read::Blocked, Read::NotRun];
        let mut keys: Vec<(String, String)> = self
            .cells
            .keys()
            .map(|(s, o, _)| (s.clone(), o.clone()))
            .collect();
        keys.sort();
        keys.dedup();
        out.push_str(&format!("{:<24} {:<8}", "source", "outcome"));
        for rd in reads {
            out.push_str(&format!(" {:>13}", rd.word()));
        }
        out.push('\n');
        for (s, o) in &keys {
            out.push_str(&format!("{s:<24} {o:<8}"));
            for rd in reads {
                let n = self.cells.get(&(s.clone(), o.clone(), rd)).copied().unwrap_or(0);
                out.push_str(&format!(" {n:>13}"));
            }
            out.push('\n');
        }
        // The two cells that decide whether a rule lands.
        let sum = |outcome: &str, rd: Read| -> usize {
            self.cells
                .iter()
                .filter(|((_, o, r), _)| o == outcome && *r == rd)
                .map(|(_, n)| n)
                .sum()
        };
        let ran = sum("ran", Read::Auto) + sum("ran", Read::MayApprove) + sum("ran", Read::AlwaysAsk)
            + sum("ran", Read::Blocked) + sum("ran", Read::NotRun);
        let refused: usize = reads.iter().map(|r| sum("refused", *r)).sum();
        out.push_str(&format!(
            "\nran × always_ask (would prompt now): {} of {} ({:.1}%)\n",
            sum("ran", Read::AlwaysAsk) + sum("ran", Read::Blocked),
            ran,
            pct(sum("ran", Read::AlwaysAsk) + sum("ran", Read::Blocked), ran)
        ));
        out.push_str(&format!(
            "refused × always_ask (caught before the model): {} of {} ({:.1}%)\n",
            sum("refused", Read::AlwaysAsk) + sum("refused", Read::Blocked),
            refused,
            pct(sum("refused", Read::AlwaysAsk) + sum("refused", Read::Blocked), refused)
        ));
        out.push_str(&format!(
            "not_run (layer A could not read it): {} of {} ({:.1}%)\n",
            sum("ran", Read::NotRun) + sum("refused", Read::NotRun) + sum("error", Read::NotRun),
            self.commands,
            pct(sum("ran", Read::NotRun) + sum("refused", Read::NotRun) + sum("error", Read::NotRun), self.commands)
        ));
        if !self.prompts_by_rule.is_empty() {
            out.push_str("\nprompts on rows that ran, by rule:\n");
            let mut v: Vec<_> = self.prompts_by_rule.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            for (rule, n) in v {
                out.push_str(&format!("  {n:>6}  {rule}\n"));
                for ex in self.prompt_examples.get(rule).into_iter().flatten() {
                    out.push_str(&format!("          e.g. {ex}\n"));
                }
            }
        }
        if !self.catches_by_rule.is_empty() {
            out.push_str("\ncatches on rows that were refused, by rule:\n");
            let mut v: Vec<_> = self.catches_by_rule.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            for (rule, n) in v {
                out.push_str(&format!("  {n:>6}  {rule}\n"));
            }
        }
        if !self.unknown_heads.is_empty() {
            let total: usize = self.unknown_heads.values().sum();
            out.push_str(&format!("\nprogram heads layer A does not know ({total} occurrences), top 24:\n"));
            let mut v: Vec<_> = self.unknown_heads.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            for (head, n) in v.into_iter().take(24) {
                out.push_str(&format!("  {n:>6}  {head}\n"));
            }
        }
        if !self.not_run_why.is_empty() {
            out.push_str("\nnot_run, by reason:\n");
            let mut v: Vec<_> = self.not_run_why.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            for (why, n) in v.into_iter().take(8) {
                out.push_str(&format!("  {n:>6}  {why}\n"));
            }
        }
        if !self.heredoc_sinks.is_empty() {
            let total: usize = self.heredoc_sinks.values().sum();
            out.push_str(&format!("\nhere-documents ({total} stages), by where the body goes (top 24):\n"));
            let mut v: Vec<_> = self.heredoc_sinks.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            for (k, n) in v.into_iter().take(24) {
                out.push_str(&format!("  {n:>6}  {k}\n"));
            }
        }
        if !self.not_run_constructs.is_empty() {
            out.push_str("\nnot_run, by construct (commands it appears in / commands it is the only reason for):\n");
            let mut v: Vec<_> = self.not_run_constructs.iter().collect();
            v.sort_by(|a, b| b.1.0.cmp(&a.1.0));
            for (k, (any, alone)) in v {
                out.push_str(&format!("  {any:>6} / {alone:>6}  {k}\n"));
            }
        }
        out
    }
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { 100.0 * n as f64 / d as f64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three rows, one per cell that matters: a plain read that ran, a secret
    /// read that was refused, and a command layer A cannot read. The table has
    /// to put each in its own cell and name the rule on the catch.
    #[test]
    fn the_table_crosses_outcome_with_todays_read_and_names_the_rule() {
        let dir = std::env::temp_dir().join(format!("letibot-etalon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("etalon.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"source":"t","cwd":"/w","tool":"bash","arguments":{"command":"grep -n foo /w/a.rs"},"outcome":"ran"}"#, "\n",
                r#"{"source":"t","cwd":"/w","tool":"bash","arguments":{"command":"cat ~/.ssh/id_ed25519"},"outcome":"refused"}"#, "\n",
                r#"{"source":"t","cwd":"/w","tool":"bash","arguments":{"command":"$CMD --flag"},"outcome":"ran"}"#, "\n",
                r#"{"source":"t","cwd":"/w","tool":"edit","arguments":{"path":"/w/a.rs"},"outcome":"ran"}"#, "\n",
            ),
        )
        .unwrap();
        let env = Surroundings {
            home: Some("/home/dead".into()),
            workspace: Some("/w".into()),
            shell: letibot_tools::intent::ShellTrust::Pinned { how: "test".into() },
            seen_hosts: Default::default(),
        };
        let r = report(&path, &env, 100).unwrap();
        assert_eq!(r.rows, 4);
        assert_eq!(r.commands, 3, "an edit row is not a command");
        let cell = |o: &str, rd: Read| r.cells.get(&("t".into(), o.into(), rd)).copied().unwrap_or(0);
        assert_eq!(cell("ran", Read::MayApprove) + cell("ran", Read::Auto), 1, "{:?}", r.cells);
        assert_eq!(cell("refused", Read::AlwaysAsk) + cell("refused", Read::Blocked), 1, "{:?}", r.cells);
        assert_eq!(cell("ran", Read::NotRun), 1, "{:?}", r.cells);
        assert_eq!(r.catches_by_rule.values().sum::<usize>(), 1, "{:?}", r.catches_by_rule);
        assert!(r.prompts_by_rule.is_empty(), "{:?}", r.prompts_by_rule);
        let text = r.render();
        assert!(text.contains("refused × always_ask (caught before the model): 1 of 1"), "{text}");
        assert!(text.contains("would prompt now): 0 of 2"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
