//! `ps` — one question over process state, answered without a pipeline.
//!
//! What it replaces, measured (`PS_USE.md`, 294 calls in 22 days on this box):
//! `ps -eo … | grep X | grep -v grep` (112), the same through `awk` (104),
//! "how old is pid N and what is its command line" (24), `grep -q` for one bit
//! (18), `grep -c` for a count (12), `--sort=-pcpu | head` (8), `--ppid` (1).
//! Eighty percent of them were one question — *is my server running, since
//! when, with what arguments* — and every one was a ritual reproduced from
//! training data, bracket trick included, that a shell then matched against
//! itself. Fifteen fed a kill loop; those are `pkill`'s.
//!
//! Four shapes, one tool: `pattern` (a substring of the command line, never
//! matching this process), `pid` (one process by number), `children_of` (a
//! pid's direct children), `top` (`cpu` or `mem`, the busiest first). Each row
//! is pid, ppid, age, state, CPU%, RSS and the command line, with `PROTECTED`
//! or `job <id>` where `pkill` would say so. Read-only; it never signals.

use serde_json::Value;

use crate::exec::procs::{self, ProcInfo};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

pub struct Ps;

const DEFAULT_LIMIT: usize = 20;

impl Tool for Ps {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "ps",
            "Processes of this user, as a table: pid, ppid, age, state, CPU%, RSS, \
             command line. One of: `pattern` (a substring of the command line — is X \
             running, since when, with what arguments; never matches this tool's own \
             process, so no `grep -v grep`), `pid` (one process), `children_of` (a \
             pid's children), `top` (`cpu` or `mem`: the busiest, most first). `limit` \
             caps the rows (default 20). Read-only: to signal something use `pkill`, \
             to watch it leave use `monitor`.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Substring of the command line or program name (case-sensitive)."},
                    "pid": {"type": "integer", "description": "One process, by pid."},
                    "children_of": {"type": "integer", "description": "The direct children of this pid."},
                    "top": {"type": "string", "enum": ["cpu", "mem"], "description": "Rank every process of this user by CPU or memory."},
                    "limit": {"type": "integer", "description": "At most this many rows (default 20)."}
                }
            }),
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_LIMIT);
        let host = ctx.backend.processes();
        let protected: Vec<(u32, String)> = host
            .map(|h| h.protected().into_iter().map(|p| (p.pid, p.why)).collect())
            .unwrap_or_default();
        let jobs: Vec<(u32, String)> = host
            .map(|h| {
                h.jobs()
                    .into_iter()
                    .map(|j| (j.pid, j.id.0.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let pattern = args.get("pattern").and_then(|v| v.as_str()).map(str::trim);
        let pid = args.get("pid").and_then(|v| v.as_u64()).map(|n| n as u32);
        let children_of = args
            .get("children_of")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let top = args.get("top").and_then(|v| v.as_str());

        let (title, mut rows): (String, Vec<ProcInfo>) = match (pattern, pid, children_of, top) {
            (Some(p), _, _, _) if !p.is_empty() => {
                (format!("processes matching `{p}`"), procs::find(p, &[]))
            }
            (Some(_), _, _, _) => {
                return Invocation::failed(
                    "ps: an empty pattern matches everything",
                    "give a substring of the command line, or use `top`.",
                );
            }
            (None, Some(pid), _, _) => match procs::read(pid) {
                Some(p) if p.state != 'Z' => (format!("pid {pid}"), vec![p]),
                Some(_) => {
                    return Invocation::ok(format!(
                        "pid {pid} is a zombie: it has exited and its parent has not reaped it. \
                         It is not running."
                    ));
                }
                None => {
                    return Invocation::ok(format!(
                        "no process with pid {pid}: it is not running."
                    ));
                }
            },
            (None, None, Some(parent), _) => {
                let mut kids = procs::all(&[]);
                kids.retain(|p| p.ppid == parent);
                (format!("children of pid {parent}"), kids)
            }
            (None, None, None, Some(by)) => {
                let mut all = procs::all(&[]);
                match by {
                    "cpu" => all.sort_by(|a, b| {
                        b.cpu_percent()
                            .partial_cmp(&a.cpu_percent())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }),
                    "mem" => all.sort_by(|a, b| b.rss_kb.cmp(&a.rss_kb)),
                    other => {
                        return Invocation::failed(
                            format!("ps: `top` is `cpu` or `mem`, not `{other}`"),
                            String::new(),
                        );
                    }
                }
                (format!("this user's processes by {by}, busiest first"), all)
            }
            (None, None, None, None) => {
                return Invocation::failed(
                    "ps needs a question",
                    "give `pattern` (is X running), `pid`, `children_of` or `top` (`cpu` | `mem`). \
                     A bare listing is not a question this tool answers.",
                );
            }
        };

        if rows.is_empty() {
            return Invocation::ok(format!(
                "no {title} — not counting this daemon, its ancestors and pid 1, which are \
                 never listed, nor another user's processes."
            ));
        }
        let total = rows.len();
        rows.truncate(limit);
        Invocation::ok(table(&title, &rows, total, &protected, &jobs))
    }
}

fn table(
    title: &str,
    rows: &[ProcInfo],
    total: usize,
    protected: &[(u32, String)],
    jobs: &[(u32, String)],
) -> String {
    let mut s = format!(
        "{total} {title}{}:\n{:>7} {:>7} {:>8} {:>2} {:>6} {:>8}  command\n",
        if rows.len() < total {
            format!(" (first {} shown; raise `limit`)", rows.len())
        } else {
            String::new()
        },
        "pid",
        "ppid",
        "age",
        "st",
        "cpu%",
        "rss"
    );
    for p in rows {
        s.push_str(&format!(
            "{:>7} {:>7} {:>8} {:>2} {:>6.1} {:>8}  {}",
            p.pid,
            p.ppid,
            procs::age_word(p.age),
            p.state,
            p.cpu_percent(),
            rss_word(p.rss_kb),
            if p.cmdline.is_empty() {
                format!("[{}]", p.comm)
            } else {
                p.cmdline.clone()
            }
        ));
        if let Some((_, why)) = protected.iter().find(|(pp, _)| *pp == p.pid) {
            s.push_str(&format!("   PROTECTED — {why}"));
        } else if let Some((_, job)) = jobs.iter().find(|(jp, _)| *jp == p.pid) {
            s.push_str(&format!("   job `{job}` of this session"));
        }
        s.push('\n');
    }
    s
}

fn rss_word(kb: u64) -> String {
    if kb >= 1024 * 1024 {
        format!("{:.1}G", kb as f64 / (1024.0 * 1024.0))
    } else if kb >= 1024 {
        format!("{}M", kb / 1024)
    } else {
        format!("{kb}K")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::ToolCall;

    fn run(args: Value) -> crate::ToolResult {
        let mut reg = Registry::new();
        reg.register(Box::new(Ps)).unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = ToolRuntime::new(reg, Box::new(backend));
        rt.invoke(
            "t",
            &ToolCall {
                id: "c".into(),
                name: "ps".into(),
                arguments: args.to_string(),
            },
            &mut crate::NullToolSink,
        )
    }

    #[test]
    fn a_question_is_required_and_the_answer_is_a_table_that_never_lists_this_process() {
        let none = run(serde_json::json!({}));
        assert!(
            none.payload.contains("not a question this tool answers"),
            "{}",
            none.payload
        );

        // A child we can name, then find by pattern, by pid and as a child.
        let mut child = std::process::Command::new("sleep")
            .arg("7.31")
            .spawn()
            .expect("sleep");
        let by_pattern = run(serde_json::json!({"pattern": "sleep 7.31"}));
        assert!(
            by_pattern.payload.contains("sleep 7.31"),
            "{}",
            by_pattern.payload
        );
        assert!(
            by_pattern.payload.contains("cpu%"),
            "{}",
            by_pattern.payload
        );
        let by_pid = run(serde_json::json!({"pid": child.id()}));
        assert!(
            by_pid.payload.contains(&format!("{:>7}", child.id())),
            "{}",
            by_pid.payload
        );
        let kids = run(serde_json::json!({"children_of": std::process::id()}));
        assert!(kids.payload.contains("sleep 7.31"), "{}", kids.payload);
        // The test binary carries the crate's name; it is never a row.
        let me = run(serde_json::json!({"pattern": "letibot"}));
        let mine = std::process::id().to_string();
        assert!(
            me.payload
                .lines()
                .skip(2)
                .all(|l| l.split_whitespace().next() != Some(mine.as_str())),
            "{}",
            me.payload
        );
        let top = run(serde_json::json!({"top": "mem", "limit": 3}));
        assert!(top.payload.contains("busiest first"), "{}", top.payload);
        assert!(top.payload.lines().count() <= 5, "{}", top.payload);
        child.kill().ok();
        child.wait().ok();
        let gone = run(serde_json::json!({"pid": child.id()}));
        assert!(gone.payload.contains("not running"), "{}", gone.payload);
    }
}
