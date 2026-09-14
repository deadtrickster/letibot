//! `pkill` — find processes by a pattern, and kill them by pid. Never by the
//! pattern.
//!
//! What it replaces: `pkill -f 'harnessd-rice.sock'` killed a running command
//! mid-flight; `pkill -9 -f 'flowy inbox --as NAME'` killed the shell running
//! it, twice in one night (exit 144), four times across three seats; and on
//! 2026-09-14 the author of this file did it again with `pkill -f "fake.py
//! 18811"` inside a command whose own line launched `fake.py 18811`. Every one
//! of those was a shell matching a pattern against `ps` output that included
//! the shell. `TODO.md` T21 and `exec/monitor.rs` record the count as eight.
//!
//! # Two steps, and the second takes a handle
//!
//! `action: list` (the default) runs [`crate::exec::procs::find`] — this
//! process, its ancestors and the process host's protected pids are removed
//! before the pattern is applied — and prints pid, age and command line, with
//! `PROTECTED` or `job <id>` beside what cannot or should not be killed here.
//!
//! `action: kill` takes `pids`: numbers that were in that listing. Each must
//! still match the pattern now (a pid that was reused since is refused, not
//! killed), must not be protected, and is signalled by (pid, start time).
//! `signal` is `term` (the default), `int`, `hup` or `kill`; after `term` the
//! tool waits two seconds and says which are gone and which are not, so a
//! `kill` is a decision made on a reading rather than a reflex.
//!
//! A job started by this session is named as such and refused: `job_kill`
//! reaps it with its cgroup, and killing its pid from here would leave the
//! job's record saying "running".

use std::time::Duration;

use serde_json::Value;

use crate::exec::procs::{self, KillRefused, Signal};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

pub struct Pkill;

/// How long `term` is given before the tool reports what is still there.
const GRACE: Duration = Duration::from_secs(2);

impl Tool for Pkill {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "pkill",
            "Find processes of this user by a `pattern` in their command line and \
             kill them BY PID. `action: \"list\"` (the default) shows every match — \
             pid, age, command line — with PROTECTED beside this daemon, its \
             ancestors and the model server, and `job <id>` beside a job of this \
             session (use `job_kill` for those). `action: \"kill\"` takes `pids` \
             from that listing and `signal` (`term` default, `int`, `hup`, `kill`), \
             checks each pid still matches the pattern and is not protected, sends \
             the signal, waits two seconds and reports which are gone. The pattern \
             can never match this tool's own process: it is not a shell, and the \
             process evaluating your call is removed before matching.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "A substring of the command line (case-sensitive). Required."},
                    "action": {"type": "string", "description": "`list` (default) or `kill`."},
                    "pids": {"type": "array", "items": {"type": "integer"}, "description": "For `kill`: the pids to signal, from the listing."},
                    "signal": {"type": "string", "description": "For `kill`: `term` (default), `int`, `hup`, `kill`."}
                },
                "required": ["pattern"]
            }),
            Access::Exec,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(pattern) = args.get("pattern").and_then(|v| v.as_str()).map(str::trim) else {
            return Invocation::failed(
                "pkill needs a pattern",
                "give `pattern`. Nothing was done.",
            );
        };
        if pattern.is_empty() {
            return Invocation::failed(
                "pkill needs a pattern",
                "an empty pattern matches everything; refused.",
            );
        }
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
        let found = procs::find(pattern, &[]);
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");
        match action {
            "list" => Invocation::ok(listing(pattern, &found, &protected, &jobs)),
            "kill" => {
                let Some(pids) = args.get("pids").and_then(|v| v.as_array()) else {
                    return Invocation::failed(
                        "kill needs `pids`",
                        format!(
                            "name the pids from the listing. Nothing was signalled.\n\n{}",
                            listing(pattern, &found, &protected, &jobs)
                        ),
                    );
                };
                let signal = match args.get("signal").and_then(|v| v.as_str()) {
                    None => Signal::Term,
                    Some(s) => match Signal::parse(s) {
                        Some(sig) => sig,
                        None => {
                            return Invocation::failed(
                                format!("`{s}` is not a signal this tool sends"),
                                "there are four: `term`, `int`, `hup`, `kill`. Nothing was signalled.",
                            );
                        }
                    },
                };
                let mut report = format!("signal {} for pattern `{pattern}`:\n", signal.as_str());
                let mut sent: Vec<procs::ProcInfo> = Vec::new();
                for v in pids {
                    let Some(pid) = v.as_u64().map(|p| p as u32) else {
                        report.push_str(&format!("  {v}: not a pid\n"));
                        continue;
                    };
                    let Some(p) = found.iter().find(|p| p.pid == pid) else {
                        report.push_str(&format!(
                            "  {pid}: REFUSED — not among the processes matching `{pattern}` now \
                             (gone, reused, or never in the listing)\n"
                        ));
                        continue;
                    };
                    if let Some((_, why)) = protected.iter().find(|(pp, _)| *pp == pid) {
                        report.push_str(&format!("  {pid}: REFUSED — PROTECTED: {why}\n"));
                        continue;
                    }
                    if let Some((_, job)) = jobs.iter().find(|(jp, _)| *jp == pid) {
                        report.push_str(&format!(
                            "  {pid}: REFUSED — it is job `{job}` of this session; `job_kill` \
                             reaps it with its cgroup\n"
                        ));
                        continue;
                    }
                    match procs::kill_exact(p.pid, p.start, signal) {
                        Ok(()) => {
                            report.push_str(&format!(
                                "  {pid}: {} sent ({})\n",
                                signal.as_str(),
                                p.cmdline
                            ));
                            sent.push(p.clone());
                        }
                        Err(KillRefused::Gone) => {
                            report.push_str(&format!("  {pid}: already gone\n"))
                        }
                        Err(KillRefused::Self_) => report
                            .push_str(&format!("  {pid}: REFUSED — this process or an ancestor\n")),
                        Err(KillRefused::Os(e)) => {
                            report.push_str(&format!("  {pid}: not sent — {e}\n"))
                        }
                    }
                }
                if !sent.is_empty() && signal != Signal::Kill {
                    std::thread::sleep(GRACE);
                    let still: Vec<&procs::ProcInfo> = sent
                        .iter()
                        .filter(|p| procs::alive(p.pid, p.start))
                        .collect();
                    if still.is_empty() {
                        report.push_str(&format!(
                            "all {} gone within {:.0}s.\n",
                            sent.len(),
                            GRACE.as_secs_f32()
                        ));
                    } else {
                        report.push_str(&format!(
                            "still running after {:.0}s: {}. That is a reading, not a failure; \
                             `signal: \"kill\"` on those pids if it is not shutting down.\n",
                            GRACE.as_secs_f32(),
                            still
                                .iter()
                                .map(|p| p.pid.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                } else if !sent.is_empty() {
                    report.push_str(&format!("{} signalled with KILL.\n", sent.len()));
                }
                Invocation::ok(report)
            }
            other => Invocation::failed(
                format!("`{other}` is not a pkill action"),
                "there are two: `list` (the default) and `kill`. Nothing was done.",
            ),
        }
    }
}

fn listing(
    pattern: &str,
    found: &[procs::ProcInfo],
    protected: &[(u32, String)],
    jobs: &[(u32, String)],
) -> String {
    if found.is_empty() {
        return format!(
            "no process of this user matches `{pattern}` — not counting this daemon, its \
             ancestors and pid 1, which are never listed. Another user's processes are not \
             listed either."
        );
    }
    let mut s = format!(
        "{} process(es) match `{pattern}` (pid, age, command line):\n",
        found.len()
    );
    for p in found {
        s.push_str(&p.line());
        if let Some((_, why)) = protected.iter().find(|(pp, _)| *pp == p.pid) {
            s.push_str(&format!("   PROTECTED — {why}"));
        } else if let Some((_, job)) = jobs.iter().find(|(jp, _)| *jp == p.pid) {
            s.push_str(&format!("   job `{job}` of this session — use job_kill"));
        }
        s.push('\n');
    }
    s.push_str("To kill: `pkill` again with `action: \"kill\"` and `pids: [...]` from above.");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::ToolCall;
    use std::process::{Command, Stdio};

    fn run(rt: &mut ToolRuntime, args: &str) -> String {
        rt.invoke(
            "t",
            &ToolCall {
                id: "c".into(),
                name: "pkill".into(),
                arguments: args.into(),
            },
            &mut crate::NullToolSink,
        )
        .payload
    }

    #[test]
    fn the_listing_marks_protected_pids_and_this_sessions_jobs() {
        let p = |pid: u32, cmd: &str| procs::ProcInfo {
            pid,
            ppid: 1,
            start: 0,
            comm: "x".into(),
            cmdline: cmd.into(),
            age: Duration::from_secs(90),
            state: 'S',
            cpu_ticks: 0,
            rss_kb: 0,
        };
        let found = vec![
            p(10, "llama-server --port 8080"),
            p(11, "sleep 30"),
            p(12, "cargo test"),
        ];
        let protected = vec![(
            10,
            "the model server this session talks to, on 127.0.0.1:8080".to_string(),
        )];
        let jobs = vec![(12, "job-7".to_string())];
        let l = listing("s", &found, &protected, &jobs);
        assert!(l.contains("3 process(es) match `s`"), "{l}");
        assert!(
            l.contains("PROTECTED — the model server this session talks to"),
            "{l}"
        );
        assert!(
            l.contains("job `job-7` of this session — use job_kill"),
            "{l}"
        );
        assert!(l.contains("     11     1m30s  sleep 30\n"), "{l}");
    }

    #[test]
    fn lists_then_kills_by_pid_and_refuses_what_the_listing_did_not_show() {
        let mut reg = Registry::new();
        reg.register(Box::new(Pkill)).unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let backend = crate::backend::HostBackend::new(d.path()).unwrap();
        struct Admit;
        impl crate::runtime::Gate for Admit {
            fn admit(&mut self, _: &crate::runtime::GateCall<'_>) -> crate::runtime::GateDecision {
                crate::runtime::GateDecision::Admit
            }
        }
        let mut rt = ToolRuntime::new(reg, Box::new(backend)).with_gate(Box::new(Admit));
        let marker = format!("letibot-pkill-tool-{}", std::process::id());
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .arg("letibot-sh")
            .arg(&marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let list = run(&mut rt, &format!(r#"{{"pattern": "{marker}"}}"#));
        assert!(
            list.contains(&format!("1 process(es) match `{marker}`")),
            "{list}"
        );
        assert!(list.contains(&child.id().to_string()), "{list}");
        // A pid not in the listing is refused, whoever it is.
        let refused = run(
            &mut rt,
            &format!(
                r#"{{"pattern": "{marker}", "action": "kill", "pids": [1, {}]}}"#,
                std::process::id()
            ),
        );
        assert!(refused.contains("1: REFUSED — not among"), "{refused}");
        assert!(
            refused.contains(&format!("{}: REFUSED — not among", std::process::id())),
            "{refused}"
        );
        // The listed one dies.
        let killed = run(
            &mut rt,
            &format!(
                r#"{{"pattern": "{marker}", "action": "kill", "pids": [{}]}}"#,
                child.id()
            ),
        );
        assert!(killed.contains("TERM sent"), "{killed}");
        assert!(killed.contains("all 1 gone"), "{killed}");
        let _ = child.wait();
        let again = run(&mut rt, &format!(r#"{{"pattern": "{marker}"}}"#));
        assert!(again.contains("no process of this user matches"), "{again}");
        let bad = run(
            &mut rt,
            r#"{"pattern": "x", "action": "kill", "pids": [2], "signal": "9"}"#,
        );
        assert!(bad.contains("there are four"), "{bad}");
    }
}
