//! **Recovering shapes for decisions the operator already made.**
//!
//! The shape cache went in after these rows were written, so a store full of the
//! operator's own approvals carries `shape IS NULL` on every one of them. Left
//! alone, the first session after the cache landed re-asks every question they had
//! already answered — which is the exact complaint the cache exists to answer.
//!
//! **What this does NOT do is trust the old row.** A stored `tier` and a stored
//! `baseline` are what the classifier said on the day, and the classifier has
//! changed since — `cd` became an inspect, the secret list grew, the rule
//! protecting the rule files arrived. Reinstating a year-old verdict would import
//! yesterday's mistakes as today's standing grants. So every candidate is
//! **re-derived from the command text with today's code**, and the row's own
//! columns are used for exactly two things: who decided (a human) and where they
//! decided it (the session's workspace).
//!
//! A row is filled only when all of these hold *now*:
//!
//! * the tool is `bash`. It is the only one with a command, and a shape is a
//!   parse of a command — `edit` and `web_search` have no shape to recover and are
//!   not failures;
//! * today's layer A puts it at `may_approve`. A command that now reaches
//!   `always_ask` — a secret directory, the config that guards the config — is
//!   left alone however it was answered then;
//! * today's layer A finds no `Destroy` intent. A shape holes its operands, so a
//!   deletion is never settled by one;
//! * it parses to a shape at all.
//!
//! Anything else is skipped **by name**, and the dry run prints why.

use letibot_tokencore::store::Store;
use letibot_tools::adjudicate::{ActionClass, Tier};
use letibot_tools::intent::{Baseline, Intent, ShellTrust, Surroundings};
use letibot_tools::schema::Access;

/// One row, after today's classifier has had its say.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub request_id: String,
    pub workspace: String,
    pub command: String,
    pub shape: String,
    pub class: String,
}

/// A row that will not be filled, and the reason in words.
#[derive(Debug, Clone)]
pub struct Skipped {
    pub request_id: String,
    pub why: String,
}

#[derive(Debug, Default)]
pub struct Report {
    pub considered: usize,
    pub candidates: Vec<Candidate>,
    pub skipped: Vec<Skipped>,
    /// How many rows filled, once applied. `None` for a dry run.
    pub filled: Option<usize>,
}

/// Work out what could be filled, without writing anything.
pub fn plan(path: &std::path::Path) -> Result<Report, String> {
    let store = Store::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    let rows = store
        .shapeless_human_admits()
        .map_err(|e| format!("reading the corpus: {e}"))?;

    let mut report = Report {
        considered: rows.len(),
        ..Default::default()
    };

    for row in rows {
        if row.tool != "bash" {
            report.skipped.push(Skipped {
                request_id: row.request_id,
                why: format!("`{}` has no command, so it has no shape", row.tool),
            });
            continue;
        }
        let command = match serde_json::from_str::<serde_json::Value>(&row.arguments_json)
            .ok()
            .and_then(|v| {
                v.get("command")
                    .and_then(|c| c.as_str())
                    .map(str::to_string)
            }) {
            Some(c) => c,
            None => {
                report.skipped.push(Skipped {
                    request_id: row.request_id,
                    why: "no `command` argument to re-read".into(),
                });
                continue;
            }
        };

        // Today's reading of the same text, in the project it was approved in.
        let baseline = Baseline::of_command(&command, &surroundings(&row.workspace_root));

        if !matches!(baseline.tier, Tier::MayApprove) {
            report.skipped.push(Skipped {
                request_id: row.request_id,
                why: format!(
                    "today this is `{}`, not `may_approve` — it asks now whatever \
                     was answered then",
                    baseline.tier.as_str()
                ),
            });
            continue;
        }
        if baseline.intents.contains(&Intent::Destroy) {
            report.skipped.push(Skipped {
                request_id: row.request_id,
                why: "destructive, and a shape holes its operands".into(),
            });
            continue;
        }
        let Some(shape) = letibot_tools::adjudicate::shape_of(&baseline) else {
            report.skipped.push(Skipped {
                request_id: row.request_id,
                why: "the command does not parse to a shape".into(),
            });
            continue;
        };

        // The class exactly as `request_from` derives it for a `bash` call: no
        // `path` argument, so `path_is_inside` is true and nothing was stat'd.
        // Constant for every command, which is why it is recoverable at all — it
        // never depended on the surroundings of the day.
        let class = ActionClass::host(Access::Exec, true, false);

        report.candidates.push(Candidate {
            request_id: row.request_id,
            workspace: row.workspace_root,
            command,
            shape,
            class: class.to_string(),
        });
    }

    Ok(report)
}

/// Fill the rows the plan found. Idempotent: the UPDATE only ever fills a hole.
pub fn apply(path: &std::path::Path, report: &mut Report) -> Result<(), String> {
    let store = Store::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    let mut filled = 0;
    for c in &report.candidates {
        if store
            .backfill_shape(&c.request_id, &c.shape, &c.class)
            .map_err(|e| format!("filling {}: {e}", c.request_id))?
        {
            filled += 1;
        }
    }
    report.filled = Some(filled);
    Ok(())
}

/// The surroundings a command was judged in: its own project, this box's home, and
/// a shell that is pinned — which is what the daemon declares for a real session.
/// Without the pin a bare command name is unresolved and every row would skip.
fn surroundings(workspace: &str) -> Surroundings {
    Surroundings {
        // A replay of recorded rows, not a live session: there is no scratch
        // directory to place a path in, and inventing one would classify a path
        // by a directory that did not exist when the row was written.
        scratch: None,
        home: std::env::var("HOME").ok().map(Into::into),
        workspace: Some(workspace.into()),
        shell: ShellTrust::Pinned {
            how: "the session this decision was taken in".into(),
        },
        seen_hosts: Default::default(),
    }
}

impl Report {
    /// What the operator reads before deciding whether to apply it.
    ///
    /// Grouped by shape, because the count that answers "will this stop the asking"
    /// is the number of DISTINCT shapes, not the number of rows.
    pub fn render(&self) -> String {
        use std::collections::BTreeMap;
        let mut by_shape: BTreeMap<(&str, &str), Vec<&Candidate>> = BTreeMap::new();
        for c in &self.candidates {
            by_shape
                .entry((c.workspace.as_str(), c.shape.as_str()))
                .or_default()
                .push(c);
        }

        let mut out = String::new();
        out.push_str(&format!(
            "{} human-approved rows carry no shape.\n\
             {} can be recovered, in {} distinct shape(s).\n\n",
            self.considered,
            self.candidates.len(),
            by_shape.len()
        ));

        let mut ws = "";
        for ((workspace, shape), rows) in &by_shape {
            if *workspace != ws {
                out.push_str(&format!("{workspace}\n"));
                ws = workspace;
            }
            out.push_str(&format!("  {:3}x  {shape}\n", rows.len()));
            // One real command per shape, so the operator can see what the holes
            // stood for rather than taking the shape on trust.
            out.push_str(&format!("         e.g. {}\n", one_line(&rows[0].command)));
        }

        if !self.skipped.is_empty() {
            let mut why: BTreeMap<&str, usize> = BTreeMap::new();
            for s in &self.skipped {
                *why.entry(s.why.as_str()).or_default() += 1;
            }
            out.push_str(&format!("\n{} left alone:\n", self.skipped.len()));
            for (w, n) in why {
                out.push_str(&format!("  {n:3}x  {w}\n"));
            }
        }

        match self.filled {
            Some(n) => out.push_str(&format!("\nfilled {n} row(s).\n")),
            None => out.push_str(
                "\n(a dry run — `--backfill-shapes-write` fills the rows above. \
                 They become standing approvals for those shapes, in those projects, \
                 at the next session.)\n",
            ),
        }
        out
    }
}

fn one_line(s: &str) -> String {
    let flat = s.replace('\n', " ");
    if flat.chars().count() > 96 {
        format!("{}…", flat.chars().take(96).collect::<String>())
    } else {
        flat
    }
}
