//! `job_list`, `job_output`, `job_wait`, `job_kill` — background job control, and
//! the four things that make it different from the three harnesses that ship one.
//!
//! # 1. Every predicate is a handle
//!
//! `job_kill` takes a job id or a scope. `job_wait` takes a job id or a scope.
//! Neither takes a pattern, and that is not an omission — it is the mechanism.
//! `TODO.md` T24: *"`pkill -f X` becomes 'kill this cgroup' — no pattern, so
//! nothing to self-match; `until pgrep -f X` becomes 'is this cgroup non-empty' —
//! no predicate that can match its own waiter."* T21.1 and T21.2 are not guarded
//! here, they are **inexpressible** here.
//!
//! # 2. There is a wait verb at all
//!
//! Because otherwise the model writes the loop. The fleet counted a hand-written
//! polling loop in **seventy-six spellings** before `wait` became one verb, and
//! every one of those spellings is a chance to write the self-matching one. A
//! capability the model asks for cannot be composed wrong.
//!
//! # 3. Three exit conditions, never collapsed
//!
//! It happened / the deadline passed quietly and it is still running / it was
//! never there. F5 forbids the second being reported as the first, and the seat
//! brief's *only count absence after presence* is what makes the third necessary:
//! an empty scope at t=0 is a boot window, not a finish.
//!
//! # 4. The reap log is a tool result
//!
//! `job_list` shows what every ended scope killed. A reaper whose zero is
//! unfalsifiable is the empty-haystack bug one layer up, and the only cure for it
//! is that the record is somewhere somebody reads.

use std::time::Duration;

use serde_json::Value;

use crate::exec::{JobId, ProcessHost, ScopeId, ScopeKind, Waited};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// Bytes `job_output` returns by default. It is the recovery path for a capped
/// `bash`, so this is generous — and it is excluded from the spill budget by name
/// ([`crate::exec::exec_budget`]) for the same reason.
const DEFAULT_OUTPUT_BYTES: usize = 60_000;
/// The longest a wait may be asked for. Past this the honest thing is to let the
/// turn end and look again.
const MAX_WAIT_MS: u64 = 900_000;
const DEFAULT_WAIT_MS: u64 = 60_000;

fn no_process_host(ctx: &InvokeCtx<'_>) -> Invocation {
    Invocation::failed(
        "this session's backend cannot start processes, so it has no jobs",
        format!(
            "the backend is `{}`. This is not a claim that there are no jobs — there \
             is no job table to look in. A session that runs commands is opened with \
             `HostBackend::executable`.",
            ctx.backend.describe()
        ),
    )
}

/// Clause 1 for a job id that is not there: the list, and the nearest.
pub(crate) fn unknown_job(host: &dyn ProcessHost, asked: &str) -> Invocation {
    let jobs = host.jobs();
    let mut body = if jobs.is_empty() {
        // A denominator of zero. `no such job` over an empty table and `no such
        // job` over a table of twelve are different facts.
        "this session has started 0 jobs, so there is nothing to look in. Start one \
         with `bash`."
            .to_string()
    } else {
        let mut s = format!("this session has {} job(s):\n", jobs.len());
        for j in &jobs {
            s.push_str(&format!(
                "  {} — {} — {}\n",
                j.id,
                j.state.word(),
                clip(&j.command, 80)
            ));
        }
        s
    };
    let names: Vec<String> = jobs.iter().map(|j| j.id.0.clone()).collect();
    if let Some(n) = nearest(asked, &names) {
        body.push_str(&format!("\nthe nearest to `{asked}` is `{n}`.\n"));
    }
    Invocation::failed(format!("no job called `{asked}`"), body)
}

fn clip(s: &str, n: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= n {
        return s;
    }
    format!("{}…", s.chars().take(n).collect::<String>())
}

fn nearest(want: &str, have: &[String]) -> Option<String> {
    have.iter()
        .map(|h| (super::edit_distance(want, h), h))
        .filter(|(d, h)| *d <= h.len().max(want.len()) / 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, h)| h.clone())
}

fn secs(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{}.{}s", s, d.subsec_millis() / 100)
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

// ---------------------------------------------------------------- job_list

pub struct JobList;

impl Tool for JobList {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "job_list",
            "List the commands this session has started, what state each is in, how \
             much output it has produced and which scope owns it — plus every monitor \
             that is watching and every one that has settled with the reason it did, \
             every job that was moved to a different scope, and what every scope that \
             has already ended killed on its way out. Takes no arguments; optionally \
             `scope` to show only one scope's jobs. Use this instead of `ps` or \
             `pgrep`: it reads cgroup membership, so it has no pattern that could \
             match the process asking.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "description": "Show only jobs owned by this scope: `turn`, `session`, or an explicit scope's name."}
                }
            }),
            // Reads the harness's own tables and changes nothing. Clause 4 says
            // declare the widest thing it CAN do, and this one cannot do anything.
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(host) = ctx.backend.processes() else {
            return no_process_host(ctx);
        };
        let want = args.get("scope").and_then(|v| v.as_str());
        let all = host.jobs();
        let jobs: Vec<_> = all
            .iter()
            .filter(|j| {
                want.is_none_or(|w| j.owner.kind.as_str() == w || j.owner.name == w)
            })
            .collect();

        let mut body = String::new();
        // The denominator, first and always. §2.2: a count does not travel without
        // the size of what it was drawn from, and "0 jobs" under a filter is not
        // the same fact as "0 jobs".
        body.push_str(&match want {
            Some(w) => format!(
                "{} of {} job(s) this session started are in scope `{w}`.\n",
                jobs.len(),
                all.len()
            ),
            None => format!("{} job(s) this session has started.\n", all.len()),
        });

        if jobs.is_empty() && !all.is_empty() {
            body.push_str(&format!(
                "\nno job is in `{}`. The scopes with jobs are: {}.\n",
                want.unwrap_or(""),
                {
                    let mut ks: Vec<String> =
                        all.iter().map(|j| j.owner.to_string()).collect();
                    ks.sort();
                    ks.dedup();
                    ks.join(", ")
                }
            ));
        }

        for j in &jobs {
            body.push_str(&format!(
                "\n{}  [{}]\n  command: {}\n  scope:   {} — {}\n  pid:     {}\n  \
                 elapsed: {}\n  output:  {} bytes{}\n",
                j.id,
                j.state.word(),
                clip(&j.command, 160),
                j.owner,
                j.owner.kind.reaped_when(),
                j.pid,
                secs(j.elapsed),
                j.produced,
                match j.since_last_output {
                    // Work done, and when it last moved. Not "alive".
                    Some(d) => format!(", last wrote {} ago", secs(d)),
                    None => ", nothing written yet".to_string(),
                }
            ));
        }

        // The scopes, so a listing shows what is open and what will reap it.
        let scopes = host.scopes();
        if !scopes.is_empty() {
            body.push_str(&format!("\n{} scope(s) open:\n", scopes.len()));
            for s in &scopes {
                body.push_str(&format!("  {} — {}\n", s, s.kind.reaped_when()));
            }
        }

        // THE MONITORS. T24 requirement 2: *"the head can show what is watching
        // and for whom … an invisible watcher is an unreapable one."* They are
        // here rather than behind a `monitor_list` of their own because "what is
        // running and what is watching" is one question, and a second listing
        // nobody opens is how a watcher becomes invisible without anybody hiding
        // it.
        if let Some(monitors) = host.monitors() {
            let live = monitors.list();
            let done = monitors.history();
            body.push_str(&format!(
                "\n{} monitor(s) watching, {} settled",
                live.len(),
                done.len()
            ));
            if live.is_empty() && done.is_empty() {
                body.push_str(
                    " — this session has declared none. That is a fact about this \
                     session, not evidence that nothing is worth watching.\n",
                );
            } else {
                body.push('\n');
            }
            for m in &live {
                body.push_str(&format!(
                    "  {} — watching {}\n    owner: {} — {}\n    declared by: {}; {} \
                     left of a {:.0}s ttl\n",
                    m.name,
                    m.watch.describe(),
                    m.owner,
                    m.owner.kind.reaped_when(),
                    m.declared_by,
                    m.remaining().map(secs).unwrap_or_else(|| "none".into()),
                    m.ttl().as_secs_f32(),
                ));
            }
            for m in &done {
                // **Why it fired, not that it did.** A settled monitor whose
                // record said only "done" would have thrown away the answer it
                // was declared to get.
                body.push_str(&format!(
                    "  {} — was watching {} — {}\n",
                    m.name,
                    m.watch.describe(),
                    m.settled().map(|f| f.word()).unwrap_or_default()
                ));
            }
        }

        // THE PROMOTIONS. A lifetime that changed under the model is a fact
        // somebody has to be able to read back: a job whose owner is now the
        // session, when the model started it in the turn, is a different thing
        // from one that started there.
        let promotions = host.promotions();
        if !promotions.is_empty() {
            body.push_str(&format!(
                "\n{} job(s) moved to a different scope this session:\n",
                promotions.len()
            ));
            for p in &promotions {
                body.push_str(&format!("  {}\n", p.summary()));
            }
        }

        // THE REAP LOG. This is the part that makes the zero falsifiable: a scope
        // that ended and killed nothing and a scope that ended and killed three
        // are different rows, and neither of them is silence.
        let reaps = host.reap_log();
        body.push_str(&format!(
            "\n{} scope(s) have ended this session",
            reaps.len()
        ));
        if reaps.is_empty() {
            body.push_str(
                " — so this session has reaped nothing yet. That is a fact about the \
                 reaper, not evidence that nothing leaked.\n",
            );
        } else {
            body.push_str(", and each recorded what it killed:\n");
            for r in &reaps {
                body.push_str(&format!("  {}\n", r.summary()));
                if !r.survivors.is_empty() {
                    body.push_str(&format!(
                        "    SURVIVORS: {} — this reap did not finish the job\n",
                        r.survivors
                            .iter()
                            .map(|p| p.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }

        if all.is_empty() {
            return Invocation::ok(body).with_note(
                "0 jobs started is not 0 processes on the box — it is a statement \
                 about this session only. `job_list` sees cgroup membership under \
                 this session's scopes and nothing else.",
            );
        }
        Invocation::ok(body)
    }
}

// -------------------------------------------------------------- job_output

pub struct JobOutput;

impl Tool for JobOutput {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "job_output",
            "Read what a job has written. Give `job`; optionally `offset` (an \
             absolute byte position, so it stays meaningful while the job keeps \
             writing) and `limit`. The result always states which bytes it is, out \
             of how many the job has produced, and names the next offset when there \
             is more. This is where the rest of a capped `bash` result is.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "job": {"type": "string", "description": "The job id, as `bash` or `job_list` reported it."},
                    "offset": {"type": "integer", "description": "Absolute byte offset to start at. Defaults to the beginning of what is still retained."},
                    "limit": {"type": "integer", "description": "How many bytes to return."}
                },
                "required": ["job"]
            }),
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(host) = ctx.backend.processes() else {
            return no_process_host(ctx);
        };
        let Some(id) = args.get("job").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "job_output needs a job",
                "call `job_output` again with `job` set to a job id. `job_list` shows \
                 them.",
            );
        };
        let jid = JobId(id.to_string());
        let Some(view) = host.job(&jid) else {
            return unknown_job(host, id);
        };
        let from = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_OUTPUT_BYTES);

        let slice = match host.output(&jid, from, limit) {
            Ok(s) => s,
            Err(e) => return Invocation::failed(e.to_string(), String::new()),
        };
        let denominator = slice.denominator(&jid);

        // A job that has written nothing is not an empty answer about its output —
        // it is a job that has written nothing, and whether it is still running
        // decides what that means. §2.2's zero-denominator rule, in its exec form.
        if slice.produced == 0 {
            let why = if view.state.is_running() {
                format!(
                    "`{id}` is still running ({} elapsed) and has written nothing \
                     yet. That is not evidence that it will not — call `job_wait` \
                     with job=\"{id}\" and a deadline rather than polling.",
                    secs(view.elapsed)
                )
            } else {
                format!(
                    "`{id}` {} and wrote nothing at all.",
                    view.state.word()
                )
            };
            return Invocation::abstained(
                format!("`{id}` has produced no output"),
                format!("{why}\n\ncommand: {}\n", clip(&view.command, 200)),
            );
        }

        let mut inv = Invocation::ok(format!(
            "{}\n[{} — {}]\n{}",
            slice.text(),
            view.state.word(),
            denominator,
            if view.state.is_running() {
                format!(
                    "`{id}` is still running; this window is what it had written \
                     when it was read."
                )
            } else {
                String::new()
            }
        ));
        if slice.dropped > 0 && from < slice.dropped {
            inv = inv.with_note(format!(
                "offset {from} is behind what is still retained, so this window \
                 starts at {} instead. Those bytes are gone: capture keeps the last \
                 {} bytes and this job outran that.",
                slice.from, slice.retained
            ));
        }
        inv
    }
}

// ---------------------------------------------------------------- job_wait

pub struct JobWait;

impl Tool for JobWait {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "job_wait",
            "Block until a job finishes or a scope empties, then return. Give `job` \
             or `scope`, and optionally `timeout_ms`; a deadline always applies. The \
             three endings are reported as three different outcomes: it finished, \
             the deadline passed and it is still running, or nothing was ever there \
             to wait for. Use this instead of writing a `while ... sleep` loop — a \
             loop's condition can match the shell evaluating it, and a job id \
             cannot.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "job": {"type": "string", "description": "Wait for this job to leave the running state."},
                    "scope": {"type": "string", "description": "Wait for this scope's cgroup to empty: `turn`, `session`, or an explicit scope's name."},
                    "timeout_ms": {"type": "integer", "description": "How long to wait before returning with the thing still running."}
                }
            }),
            // Blocks the turn but starts nothing and stops nothing.
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(host) = ctx.backend.processes() else {
            return no_process_host(ctx);
        };
        let job = args.get("job").and_then(|v| v.as_str());
        let scope = args.get("scope").and_then(|v| v.as_str());
        if job.is_some() && scope.is_some() {
            return Invocation::failed(
                "job_wait takes `job` or `scope`, not both",
                "they are different waits — one job leaving `running`, or a whole \
                 cgroup emptying — and doing both in one call would give one outcome \
                 for two questions. Nothing was waited on.",
            );
        }
        let timeout = Duration::from_millis(
            args.get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_WAIT_MS)
                .clamp(1, MAX_WAIT_MS),
        );

        match (job, scope) {
            (Some(id), None) => wait_on_job(ctx, host, id, timeout),
            (None, Some(name)) => wait_on_scope(ctx, host, name, timeout),
            _ => Invocation::failed(
                "job_wait needs something to wait for",
                "give `job` (a job id from `bash` or `job_list`) or `scope` (`turn`, \
                 `session`, or an explicit scope's name). There is deliberately no \
                 pattern argument: a pattern can match the process waiting on it, \
                 and a handle cannot.",
            ),
        }
    }
}

fn wait_on_job(
    ctx: &mut InvokeCtx<'_>,
    host: &dyn ProcessHost,
    id: &str,
    timeout: Duration,
) -> Invocation {
    let jid = JobId(id.to_string());
    let Some(before) = host.job(&jid) else {
        return unknown_job(host, id);
    };
    // Presence, before absence. A job that already finished is not a wait that
    // succeeded; it is a wait there was nothing to do, and saying so keeps the two
    // apart.
    if !before.state.is_running() {
        return Invocation::ok(format!(
            "`{id}` had already {} before this wait began — {} elapsed, {} bytes of \
             output. Nothing was waited for. Read it with `job_output`.",
            before.state.word(),
            secs(before.elapsed),
            before.produced
        ));
    }

    ctx.progress(format!(
        "waiting on `{id}`: {} bytes so far, deadline {}",
        before.produced,
        secs(timeout)
    ));
    let waited = match host.wait_job(&jid, timeout) {
        Ok(w) => w,
        Err(e) => return Invocation::failed(e.to_string(), String::new()),
    };
    let after = host.job(&jid);
    match waited {
        Waited::Happened { state, took } => Invocation::ok(format!(
            "`{id}` {} after {}.\n  command: {}\n  output:  {} bytes — read it with \
             `job_output` job=\"{id}\"",
            state.map(|s| s.word()).unwrap_or_default(),
            secs(took),
            clip(&before.command, 160),
            after.as_ref().map(|v| v.produced).unwrap_or(0)
        )),
        // A deadline is its own outcome and carries PROGRESS, not a liveness bit.
        // `ToolOutcome::Timeout` exists precisely so this cannot be mistaken for a
        // completion the caller can build on.
        Waited::Deadline {
            took,
            produced,
            since_last_output,
        } => Invocation {
            outcome: letibot_transcript::ToolOutcome::Timeout,
            payload: format!(
                "`{id}` is STILL RUNNING after {}. This is not a failure and it is \
                 not a completion.\n  command: {}\n  produced: {produced} bytes so \
                 far\n  last wrote: {}\n\nIt was not killed. Wait again with a longer \
                 `timeout_ms`, read what it has with `job_output`, or stop it with \
                 `job_kill`.",
                secs(took),
                clip(&before.command, 160),
                match since_last_output {
                    Some(d) => format!("{} ago", secs(d)),
                    None => "never — it has written nothing".into(),
                }
            ),
            notes: vec![],
            edit: None,
        },
        Waited::NeverStarted { .. } => Invocation::failed(
            format!("`{id}` was running a moment ago and the wait could not observe it"),
            "this is a harness defect rather than an answer about the job.".to_string(),
        ),
    }
}

fn wait_on_scope(
    ctx: &mut InvokeCtx<'_>,
    host: &dyn ProcessHost,
    name: &str,
    timeout: Duration,
) -> Invocation {
    let Some(sid) = resolve_scope(host, name) else {
        return unknown_scope(host, name);
    };
    ctx.progress(format!("waiting on scope `{sid}`, deadline {}", secs(timeout)));
    match host.wait_scope(&sid, timeout) {
        Ok(Waited::Happened { took, .. }) => Invocation::ok(format!(
            "scope `{sid}` is empty after {}. It held at least one process while this \
             wait was running, and now holds none — so this is a finish and not a \
             boot window.",
            secs(took)
        )),
        Ok(Waited::Deadline { took, .. }) => Invocation {
            outcome: letibot_transcript::ToolOutcome::Timeout,
            payload: format!(
                "scope `{sid}` STILL HOLDS {} process(es) after {}. Nothing was \
                 killed. `job_list` shows what is in it; `job_kill` with \
                 scope=\"{name}\" ends it.",
                host.jobs()
                    .iter()
                    .filter(|j| j.scope.path.starts_with(&sid.path) && j.state.is_running())
                    .count(),
                secs(took)
            ),
            notes: vec![],
            edit: None,
        },
        // The seat brief's rule as an outcome: only count absence after presence.
        Ok(Waited::NeverStarted {
            waited_for_presence,
        }) => Invocation::failed(
            format!("scope `{sid}` never held a process, so its emptiness proves nothing"),
            format!(
                "nothing was ever seen in `{sid}` during the {} this waited for \
                 presence. An empty scope before startup is a BOOT WINDOW, not a \
                 finish — reporting it as \"done\" is the failure this outcome exists \
                 to prevent. Start the work first and keep its job id, then wait on \
                 that: `job_wait` with `job` needs no presence check because a job id \
                 only exists once the process does.",
                secs(waited_for_presence)
            ),
        ),
        Err(e) => Invocation::failed(e.to_string(), String::new()),
    }
}

// ---------------------------------------------------------------- job_kill

pub struct JobKill;

impl Tool for JobKill {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "job_kill",
            "Stop a job, or everything in a scope. Give `job` or `scope`. This kills \
             a cgroup, so every descendant the command started goes with it and \
             there is no pattern that could match something else. The result lists \
             what was actually killed, by pid and command line, and says whether \
             anything survived. Use this instead of `pkill` or `kill`.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "job": {"type": "string", "description": "The job id to stop."},
                    "scope": {"type": "string", "description": "Kill everything in this scope: `turn`, `session`, or an explicit scope's name."}
                }
            }),
            // It ends processes. Clause 4 wants the widest thing it can do, and
            // "stop the operator's long build" is an exec-class effect.
            Access::Exec,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(host) = ctx.backend.processes() else {
            return no_process_host(ctx);
        };
        let job = args.get("job").and_then(|v| v.as_str());
        let scope = args.get("scope").and_then(|v| v.as_str());
        // What the harness knows and `/proc` may not be able to say at the instant
        // of the kill: the command the model actually asked for. A record whose
        // only identification is a `/proc` read is a record that reads `[sh] (no
        // command line)` for a process caught between fork and exec.
        let mut asked_for: Option<String> = None;
        let reaping = match (job, scope) {
            (Some(id), _) => {
                let jid = JobId(id.to_string());
                let Some(view) = host.job(&jid) else {
                    return unknown_job(host, id);
                };
                asked_for = Some(view.command.clone());
                host.kill_job(&jid)
            }
            (None, Some(name)) => {
                let Some(sid) = resolve_scope(host, name) else {
                    return unknown_scope(host, name);
                };
                host.end_scope(&sid)
            }
            (None, None) => {
                return Invocation::failed(
                    "job_kill needs something to kill",
                    "give `job` (a job id from `bash` or `job_list`) or `scope` \
                     (`turn`, `session`, or an explicit scope's name). There is \
                     deliberately no pattern argument: `pkill -f X` is the call that \
                     matches the process making it, and a handle cannot. Nothing was \
                     killed.",
                );
            }
        };
        let r = match reaping {
            Ok(r) => r,
            Err(e) => return Invocation::failed(e.to_string(), String::new()),
        };

        // The record, verbatim, in the result. Not a "killed" boolean: the reader
        // has to be able to tell an empty scope from a reaper that did nothing.
        let mut body = format!("{}\n\n", r.summary());
        if let Some(cmd) = &asked_for {
            body.push_str(&format!("the command was: {}\n", clip(cmd, 200)));
        }
        body.push_str(&format!(
            "observed before the kill: {} process(es)\n",
            r.observed.len()
        ));
        for p in &r.observed {
            body.push_str(&format!("  pid {} {} — {}\n", p.pid, p.comm, clip(&p.label(), 140)));
        }
        body.push_str(&format!(
            "mechanism: {}\nwaited {} for the cgroup to empty\nsurvivors after: {}\n\
             cgroup removed: {}\n",
            r.mechanism,
            secs(r.waited),
            if r.survivors.is_empty() {
                "none".to_string()
            } else {
                r.survivors
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            r.removed
        ));
        if let Some(n) = &r.note {
            body.push_str(&format!("note: {n}\n"));
        }

        if r.observed.is_empty() {
            // Instrument, do not congratulate. Nothing was there, and that is not
            // the same as having stopped something.
            return Invocation::ok(body).with_note(
                "nothing was running there, so nothing was killed. That is a \
                 different fact from having stopped something, and this call is not \
                 evidence that anything ever ran.",
            );
        }
        if !r.clean() {
            return Invocation::failed(
                format!(
                    "{} process(es) survived the kill of {}",
                    r.survivors.len(),
                    r.scope
                ),
                body,
            );
        }
        Invocation::ok(body)
    }
}

/// A scope name from the model to a scope this host has open.
pub(crate) fn resolve_scope(host: &dyn ProcessHost, name: &str) -> Option<ScopeId> {
    let open = host.scopes();
    if let Some(s) = open.iter().find(|s| s.name == name) {
        return Some(s.clone());
    }
    let kind = ScopeKind::parse(name)?;
    // The turn and session scopes are addressable by their kind, because that is
    // how the model was told about them in `bash`'s result.
    open.iter().find(|s| s.kind == kind).cloned()
}

pub(crate) fn unknown_scope(host: &dyn ProcessHost, asked: &str) -> Invocation {
    let open = host.scopes();
    let mut body = if open.is_empty() {
        "this session has 0 scopes open, so there is nothing to look in. A scope is \
         created by the first `bash` call that needs it."
            .to_string()
    } else {
        let mut s = format!("this session has {} scope(s) open:\n", open.len());
        for o in &open {
            s.push_str(&format!("  {} — {}\n", o, o.kind.reaped_when()));
        }
        s
    };
    body.push_str(
        "\nname one of those, or one of `turn`, `session`, `explicit` — there are \
         three kinds and no fourth.\n",
    );
    Invocation::failed(format!("no scope called `{asked}`"), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_job_tool_declares_its_access_honestly_and_lints_clean() {
        let tools: Vec<(&str, Access, String)> = vec![
            ("job_list", JobList.schema().access, JobList.schema().description),
            ("job_output", JobOutput.schema().access, JobOutput.schema().description),
            ("job_wait", JobWait.schema().access, JobWait.schema().description),
            ("job_kill", JobKill.schema().access, JobKill.schema().description),
        ];
        for (name, access, desc) in tools {
            assert_eq!(crate::schema::lint_description(&desc), vec![], "{name}");
            // Only the one that ends processes is gated. The three that read are
            // not, which is clause 4 as control flow rather than as a promise.
            let expected = if name == "job_kill" {
                Access::Exec
            } else {
                Access::Read
            };
            assert_eq!(access, expected, "{name}");
        }
    }

    #[test]
    fn no_tool_here_takes_a_pattern() {
        // T21 made inexpressible rather than guarded: if this ever fails, somebody
        // added the argument that brings the whole failure mode back.
        for s in [
            JobList.schema(),
            JobOutput.schema(),
            JobWait.schema(),
            JobKill.schema(),
        ] {
            let names = s.param_names().join(",");
            for banned in ["pattern", "match", "name_regex", "grep", "cmdline"] {
                assert!(
                    !names.contains(banned),
                    "`{}` grew a `{banned}` argument — that is `pkill -f` with a JSON \
                     wrapper, and it can match the process calling it",
                    s.name
                );
            }
        }
    }
}
