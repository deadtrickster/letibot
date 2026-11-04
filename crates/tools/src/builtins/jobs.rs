//! `job_list`, `job_output`, `job_wait`, `job_kill`: background job control where every
//! predicate is a handle, so no wait or kill can match its own waiter, and where three
//! outcomes stay distinct - it happened, the deadline passed, it was never there.

use std::time::Duration;

use letibot_transcript::text::clip_chars;
use serde_json::Value;

use crate::exec::{JobId, JobView, ProcessHost, ScopeId, ScopeKind, Waited};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// Bytes `job_output` returns by default. It is the recovery path for a capped
/// `bash`, so this is generous - and it is excluded from the spill budget by name
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

/// Clause 1 for a job id that is not there: the list, and the nearest name in it.
/// The table is the one the name was matched against, so the answer cannot drift from it.
pub(crate) fn unknown_job(jobs: &[JobView], asked: &str) -> Invocation {
    let mut body = if jobs.is_empty() {
        // A denominator of zero. `no such job` over an empty table and `no such
        // job` over a table of twelve are different facts.
        "this session has started 0 jobs, so there is nothing to look in. Start one \
         with `bash`."
            .to_string()
    } else {
        let mut s = format!("this session has {}:\n", counted(jobs.len(), "job", "jobs"));
        for j in jobs {
            s.push_str(&format!(
                "  {} — {} — {}\n",
                j.label(),
                j.state.word(),
                clip(&j.command, 80)
            ));
        }
        s
    };
    // Both handles, because both are accepted. A model that wrote a name it nearly
    // remembered should be told the name, and a model that wrote an id should be told the
    // id; the nearest is the nearest of either.
    let names: Vec<String> = jobs
        .iter()
        .flat_map(|j| [j.id.0.clone(), j.slug.clone().unwrap_or_default()])
        .filter(|n| !n.is_empty())
        .collect();
    if let Some(n) = nearest(asked, &names) {
        body.push_str(&format!("\nthe nearest to `{asked}` is `{n}`.\n"));
    }
    Invocation::failed(format!("no job called `{asked}`"), body)
}

/// Which job the caller meant, over the daemon's table rather than over the host.
/// The id wins, and a name that fits more than one job is not a name.
enum Picked {
    /// Exactly one job, by its index in the table: the id the caller wrote, or the one job
    /// carrying that name.
    One(usize),
    /// Nothing in the table has that id and nothing carries that name.
    Unknown,
    /// The name fits more than one job, so it does not identify one.
    Ambiguous(Vec<JobId>),
}

fn pick_job(jobs: &[JobView], asked: &str) -> Picked {
    // An empty argument is not a name and not an id: the caller asked for nothing.
    if asked.trim().is_empty() {
        return Picked::Unknown;
    }
    // The id first, and it always wins. It is the handle the daemon mints, and a job
    // whose *name* happens to be another job's id must not steal the call.
    if let Some(i) = jobs.iter().position(|j| j.id.0 == asked) {
        return Picked::One(i);
    }
    let named: Vec<JobId> = jobs
        .iter()
        .filter(|j| j.slug.as_deref() == Some(asked))
        .map(|j| j.id.clone())
        .collect();
    match named.len() {
        0 => Picked::Unknown,
        1 => Picked::One(
            jobs.iter()
                .position(|j| j.id == named[0])
                .expect("the id came from this table"),
        ),
        _ => Picked::Ambiguous(named),
    }
}

/// A job, from what the caller wrote: the id the daemon mints, or the name the agent
/// stated when it backgrounded it. Exact, never fuzzy; a near miss is only named in the refusal.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
fn resolve_job(host: &dyn ProcessHost, asked: &str) -> Result<JobView, Invocation> {
    resolve_job_in(&host.jobs(), asked)
}

/// [`resolve_job`] over a table already in hand - the one rule, for a caller that is not a job
/// tool: `todo_write`'s `wait_on` names a job the same way `job_output` does, and a second
/// spelling of *which job did the caller mean* would be the one that disagreed.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
pub(crate) fn resolve_job_in(jobs: &[JobView], asked: &str) -> Result<JobView, Invocation> {
    match pick_job(jobs, asked) {
        Picked::One(i) => Ok(jobs[i].clone()),
        Picked::Unknown => Err(unknown_job(jobs, asked)),
        Picked::Ambiguous(ids) => Err(ambiguous_job(jobs, asked, &ids)),
    }
}

/// The refusal for a name that fits more than one job: guessing would act on a job the
/// caller did not name, so both ids are listed and the name is kept as it was asked for.
fn ambiguous_job(jobs: &[JobView], asked: &str, ids: &[JobId]) -> Invocation {
    let lines: String = ids
        .iter()
        .filter_map(|id| jobs.iter().find(|j| &j.id == id))
        .map(|j| {
            format!(
                "  {} — {} — {}\n",
                j.label(),
                j.state.word(),
                clip(&j.command, 80)
            )
        })
        .collect();
    Invocation::failed(
        format!(
            "`{asked}` is the name of {} jobs, so it does not name one",
            ids.len()
        ),
        format!(
            "{lines}\nNothing was done. Use the id — the name was meant to save looking it \
             up, and a name that fits two jobs cannot do that.\n"
        ),
    )
}

/// A command on ONE line, cut to `n` characters. The cap is the caller's, and each has its own.
fn clip(s: &str, n: usize) -> String {
    clip_chars(&s.replace('\n', " "), n)
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

// -------------------------------------------------------------- job_list

/// The two lines a `job_list` opens with: the denominator, and where the rest went when
/// nothing is running. Pure so the wording is assertable without a host or a cgroup tree.
fn job_list_summary(
    want: Option<&str>,
    everything: bool,
    total: usize,
    in_scope: usize,
    running: usize,
) -> String {
    let settled = in_scope - running;
    let jobs = counted(total, "job", "jobs");
    // Nothing running is the answer, so it goes first; the settled count inside it says
    // where the rest went, rather than being said again on the line below.
    if running == 0 && settled > 0 {
        return match want {
            Some(w) => format!(
                "Nothing is running in scope `{w}`; {} there {} settled — \
                 `job_list` with `all: true` shows them.\n",
                counted(settled, "job", "jobs"),
                have(settled)
            ),
            None => format!(
                "Nothing is running; {} {} settled — `job_list` with \
                 `all: true` shows every one.\n",
                counted(settled, "job", "jobs"),
                have(settled)
            ),
        };
    }
    match (want, everything) {
        (Some(w), true) => {
            format!("{in_scope} of {jobs} this session started are in scope `{w}`.\n")
        }
        (Some(w), false) => format!(
            "{running} of {jobs} this session started are still running in scope `{w}`; \
             {} there are not listed — `job_list` with `all: true` shows \
             them.\n",
            counted(settled, "settled job", "settled jobs")
        ),
        (None, true) => format!("{jobs} this session has started.\n"),
        (None, false) => format!(
            "{running} of {jobs} this session has started are still running; {} settled \
             and not listed — `job_list` with `all: true` shows every one.\n",
            counted(settled, "job", "jobs")
        ),
    }
}

pub struct JobList;

impl Tool for JobList {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "job_list",
            "List the commands this session has started, what state each is in, how \
             much output it has produced and which scope owns it — plus every monitor \
             that is watching and every one that has settled with the reason it did, \
             every job that was moved to a different scope, and what every scope that \
             has already ended killed on its way out. **Only the ACTIVE jobs are listed \
             by default** — the ones whose completion has not arrived — because a listing \
             that leads with forty finished `grep`s buries the one job anybody acts on; \
             how many were left out is always stated. `all` includes the settled ones, and \
             `scope` shows one scope's. Use this instead of `ps` or `pgrep`: it reads cgroup \
             membership, so it has no pattern that could match the process asking.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "description": "Show only jobs owned by this scope: `turn`, `session`, or an explicit scope's name."},
                    "all": {"type": "boolean", "description": "Include the jobs that have already settled. Off by default: without it the listing is the ACTIVE jobs only, with the number hidden stated."}
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
        // Active by default: the one job anybody acts on must not be buried under the
        // session's history, and the count of what was hidden is printed, not silent.
        let everything = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
        let all = host.jobs();
        let in_scope: Vec<_> = all
            .iter()
            .filter(|j| want.is_none_or(|w| j.owner.kind.as_str() == w || j.owner.name == w))
            .collect();
        let running: Vec<_> = in_scope
            .iter()
            .copied()
            .filter(|j| j.state.is_running())
            .collect();
        let jobs: Vec<&_> = if everything {
            in_scope.clone()
        } else {
            running.clone()
        };

        let mut body = String::new();
        // The denominator, first and always: "0 jobs" under a filter is not the same fact
        // as "0 jobs", so the wording is a pure function and `job_list_summary` is asserted.
        body.push_str(&job_list_summary(
            want,
            everything,
            all.len(),
            in_scope.len(),
            running.len(),
        ));

        for j in &jobs {
            body.push_str(&format!(
                "\n{}  [{}]\n  command: {}\n  scope:   {} — {}\n  pid:     {}\n  \
                 elapsed: {}\n  output:  {} bytes{}\n{}{}",
                // The name beside the id, as everywhere ([`JobView::label`]): the listing is
                // where a model finds the handle, and the name it stated is a handle now.
                j.label(),
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
                },
                // Where the whole of it is, on every row that has somewhere: the window's
                // byte count alone would be the one place that did not name the file.
                j.output_file
                    .as_ref()
                    .map(|f| format!("  file:    {} (from byte {})\n", f.path, f.from))
                    .unwrap_or_default(),
                j.memory_clause()
                    .map(|m| format!("  memory:  {m}\n"))
                    .unwrap_or_default()
            ));
        }

        // The scopes, so a listing shows what is open and what will reap it.
        let scopes = host.scopes();
        if !scopes.is_empty() {
            body.push_str(&format!(
                "\n{} open:\n",
                counted(scopes.len(), "scope", "scopes")
            ));
            for s in &scopes {
                body.push_str(&format!("  {} — {}\n", s, s.kind.reaped_when()));
            }
        }

        // The monitors, beside the jobs: "what is running and what is watching" is one
        // question, and a watcher nobody lists is a watcher nobody can reap.
        if let Some(monitors) = host.monitors() {
            let live = monitors.list();
            let done = monitors.firings();
            body.push_str(&format!(
                "\n{} watching, {} fired",
                counted(live.len(), "monitor", "monitors"),
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
            for f in &done {
                // Why it fired, not that it did. A firing whose record said only
                // "done" would have thrown away the answer it was declared to get.
                body.push_str(&format!(
                    "  {} — was watching {} — {}\n",
                    f.name,
                    f.watch,
                    f.fired.word()
                ));
            }
        }

        // The promotions: a job whose lifetime changed under the model is a fact somebody
        // has to be able to read back.
        let promotions = host.promotions();
        if !promotions.is_empty() {
            body.push_str(&format!(
                "\n{} moved to a different scope this session:\n",
                counted(promotions.len(), "job", "jobs")
            ));
            for p in &promotions {
                body.push_str(&format!("  {}\n", p.summary()));
            }
        }

        // THE REAP LOG. This is the part that makes the zero falsifiable: a scope
        // that ended and killed nothing and a scope that ended and killed three
        // are different rows, and neither of them is silence.
        body.push_str(&reaps_section(&host.reap_log()));

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
                    "job": {"type": "string", "description": "The job to read: its id (`j65`) or the name the agent stated when it backgrounded the work (`release-build`)."},
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
        // The id or the name the agent stated. See [`resolve_job`]: the id wins, and a
        // name that fits two jobs is refused rather than guessed at.
        let view = match resolve_job(host, id) {
            Ok(v) => v,
            Err(inv) => return inv,
        };
        let jid = view.id.clone();
        // The name every sentence about this job uses, once, so no branch can drift. The
        // sentences naming an ARGUMENT keep the bare id: only half of a label resolves.
        let label = view.label();
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
        let denominator = slice.denominator(&jid, &label);

        // A job that has written nothing is not an empty answer about its output: whether
        // it is still running decides what that means. Three cases, not two.
        if slice.produced == 0 {
            // A redirected job captured nothing, which is not the same answer as writing
            // nothing: its output went to a file, so sending the model to poll would be wrong.
            if let Some(path) = crate::builtins::output_redirect_path(&view.command) {
                return Invocation::abstained(
                    format!("`{label}`'s output goes to `{path}`, not to its window"),
                    format!(
                        "`{label}` was started with its stdout redirected to `{path}`, so the \
                         job's capture is empty by construction — this is not a job that \
                         wrote nothing and not a window that has not filled yet. Read the \
                         file with `read`, or `tail -n` it with `bash`.\n\n{} That never \
                         depended on the capture, so do not poll for it with a `sleep` or a \
                         loop.\n\ncommand: {}\n",
                        super::DELIVERED,
                        clip(&view.command, 200),
                    ),
                );
            }
            let why = if view.state.is_running() {
                format!(
                    "`{label}` is still running ({} elapsed) and has written nothing \
                     yet. That is not evidence that it will not — but there is nothing \
                     to do here. {}",
                    secs(view.elapsed),
                    super::DELIVERED
                )
            } else if view.state.never_ran() {
                format!(
                    "`{label}` never ran — {} — so there is no output and nothing to read: \
                     not an empty result, an absent run. The command was not started at \
                     all, so nothing here is a measurement of what it would have done.",
                    view.state.word()
                )
            } else {
                format!("`{label}` {} and wrote nothing at all.", view.state.word())
            };
            return Invocation::abstained(
                format!("`{label}` has produced no output"),
                format!("{why}\n\ncommand: {}\n", clip(&view.command, 200)),
            );
        }

        let mut inv = Invocation::ok(format!(
            "{}\n[{} — {}]\n{}{}",
            slice.text(),
            view.state.word(),
            denominator,
            if view.state.is_running() {
                format!(
                    "`{label}` is still running; this window is what it had written \
                     when it was read.\n"
                )
            } else {
                String::new()
            },
            // The window is bounded and the file is not, so the reader is told where the rest
            // is. Named as a place the bytes are, not as a tool: a confined session cannot
            // read outside its view.
            match &view.output_file {
                Some(f) => format!(
                    "[every byte of it is also in `{}`, from byte {} of this job's output — the \
                     file keeps growing with the window, so the 8 MiB is the window's bound and \
                     not the output's]\n",
                    f.path, f.from
                ),
                None => String::new(),
            }
        ));
        if let Some(f) = &view.output_file
            && let Some(why) = &f.problem
        {
            inv = inv.with_note(format!(
                "`{}` STOPPED GROWING at {} bytes — {why}. Everything after that point is only \
                 in this window, which keeps the last {} bytes.",
                f.path, f.bytes, slice.retained
            ));
        }
        if slice.dropped > 0 && from < slice.dropped {
            inv = inv.with_note(match &view.output_file {
                // The bytes are not gone, they are in the file - and saying *gone* over a job
                // that was promoted is the lie this whole note exists to stop.
                Some(f) => format!(
                    "offset {from} is behind what is still retained, so this window starts at \
                     {}. Those bytes are not lost: they are in `{}`, from byte {} of this job's \
                     output.",
                    slice.from, f.path, f.from
                ),
                None => format!(
                    "offset {from} is behind what is still retained, so this window \
                     starts at {} instead. Those bytes are gone: capture keeps the last \
                     {} bytes and this job outran that.",
                    slice.from, slice.retained
                ),
            });
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
            "Wait for a job to finish or a scope to empty. For a job this session \
             started in the background it returns AT ONCE — its completion is delivered \
             to you, and asking only makes it arrive the moment it ends; it blocks only \
             for a job or scope this session is not told about. Give `job` or `scope`, \
             and optionally `timeout_ms`; a deadline always applies. The three endings are reported as three different outcomes: it finished, \
             the deadline passed and it is still running, or nothing was ever there \
             to wait for. Use this instead of writing a `while ... sleep` loop — a \
             loop's condition can match the shell evaluating it, and a job id \
             cannot.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "job": {"type": "string", "description": "Wait for this job to leave the running state: its id (`j65`) or the name the agent stated (`release-build`)."},
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
    // What an ended job's memory did, as one more line of its result: every
    // result reports its peak, which a wait is the result of.
    fn memory_line(v: Option<&JobView>) -> String {
        v.and_then(JobView::memory_clause)
            .map(|m| format!("\n  memory:  {m}"))
            .unwrap_or_default()
    }
    let before = match resolve_job(host, id) {
        Ok(v) => v,
        Err(inv) => return inv,
    };
    let jid = before.id.clone();
    // The name every sentence about this job uses, once, so no branch can drift. The lines
    // naming an ARGUMENT keep the bare id: only half of a label resolves.
    let label = before.label();
    // Presence, before absence. A job that already finished is not a wait that
    // succeeded; it is a wait there was nothing to do, and saying so keeps the two
    // apart.
    if !before.state.is_running() {
        return Invocation::ok(format!(
            "`{label}` had already {} before this wait began — {} elapsed, {} bytes of \
             output. Nothing was waited for. Read it with `job_output`.{}",
            before.state.word(),
            secs(before.elapsed),
            before.produced,
            memory_line(Some(&before))
        ));
    }

    // The harness already promises to tell the model when a watched job settles, so waiting
    // on one blocks the floor for an answer already in flight; the verb stays for jobs
    // nothing delivers. The watch is keyed by id, so a name resolves before it is asked.
    if ctx.completion_delivered(&jid.0) {
        // And asking is wanting: a result nothing in the plan waits on is held for the
        // next break, and a wait is the model saying this one is not that. So the daemon is told
        // to hand it over the moment it ends - which is what the sentence below promises.
        ctx.want_completion(&jid.0, crate::runtime::WantedNow::Awaited);
        return Invocation::ok(format!(
            "nothing to wait for: `{label}`'s completion reaches you on its own when it \
             ends — the daemon is already watching it and, because you asked, will hand you \
             the result the moment it ends, unprompted. Waiting here would hold the floor for \
             a deadline while the answer you want is already in flight.\n  command: \
             {}\n  output:  {} bytes so far — read it with `job_output` job=\"{id}\"\n\n\
             Carry on with something else. `job_kill` stops it if you do not want it.",
            clip(&before.command, 160),
            before.produced
        ));
    }

    ctx.progress(format!(
        "waiting on `{label}`: {} bytes so far, deadline {}",
        before.produced,
        secs(timeout)
    ));
    // The operator can cut a wait short, and this is the wait they most want to cut: Ctrl-B
    // reaches `bash` and not a `job_wait` holding the floor. The job is already background,
    // so the deadline is spent in slices and the promote channel is read between them.
    let waited = match wait_greedily(ctx, host, &jid, timeout) {
        Ok(Some(w)) => w,
        // The operator asked for the floor. Not a failure and not a timeout: the
        // job is untouched and still theirs to read, and saying "timeout" here
        // would tell the model a deadline expired when none did.
        Ok(None) => {
            let now = host.job(&jid);
            return Invocation::ok(format!(
                "the operator interrupted this wait — `{label}` is still running and was \
                 NOT touched.\n  command: {}\n  output:  {} bytes so far — read it with \
                 `job_output` job=\"{id}\"\n\nThey have something to say; read it before \
                 waiting again.",
                clip(&before.command, 160),
                now.as_ref().map(|v| v.produced).unwrap_or(0)
            ));
        }
        Err(e) => return Invocation::failed(e.to_string(), String::new()),
    };
    let after = host.job(&jid);
    match waited {
        Waited::Happened { state, took } => Invocation::ok(format!(
            "`{label}` {} after {}.\n  command: {}\n  output:  {} bytes — read it with \
             `job_output` job=\"{id}\"{}",
            state.map(|s| s.word()).unwrap_or_default(),
            secs(took),
            clip(&before.command, 160),
            after.as_ref().map(|v| v.produced).unwrap_or(0),
            memory_line(after.as_ref())
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
                "`{label}` is STILL RUNNING after {}. This is not a failure and it is \
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
            needs_in_view: Vec::new(),
            media: None,
        },
        Waited::NeverStarted { .. } => Invocation::failed(
            format!("`{label}` was running a moment ago and the wait could not observe it"),
            "this is a harness defect rather than an answer about the job.".to_string(),
        ),
    }
}

/// Spend `timeout` on `jid` in slices, watching the promote channel between them.
/// `Ok(None)` means the operator asked for the floor before the job settled.
fn wait_greedily(
    ctx: &mut InvokeCtx<'_>,
    host: &dyn ProcessHost,
    jid: &JobId,
    timeout: Duration,
) -> Result<Option<Waited>, crate::exec::ExecError> {
    const SLICE: Duration = Duration::from_millis(250);
    let started = std::time::Instant::now();
    loop {
        // Checked before the first slice as well as between them: a request that arrived
        // while the model was choosing this tool is early, not stale.
        if ctx.backend.promote_requested().is_some() || ctx.operator_waiting() {
            return Ok(None);
        }
        let left = timeout.saturating_sub(started.elapsed());
        if left.is_zero() {
            // The deadline the caller asked for, reported against the whole wait
            // rather than the last slice of it.
            let j = host.job(jid);
            return Ok(Some(Waited::Deadline {
                took: started.elapsed(),
                produced: j.as_ref().map(|v| v.produced).unwrap_or(0),
                since_last_output: j.as_ref().and_then(|v| v.since_last_output),
            }));
        }
        match host.wait_job(jid, left.min(SLICE))? {
            // A slice that expired is not the wait expiring; go round again.
            Waited::Deadline { .. } => continue,
            settled => return Ok(Some(settled)),
        }
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
    ctx.progress(format!(
        "waiting on scope `{sid}`, deadline {}",
        secs(timeout)
    ));
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
                "scope `{sid}` STILL HOLDS {} after {}. Nothing was \
                 killed. `job_list` shows what is in it; `job_kill` with \
                 scope=\"{name}\" ends it.",
                counted(
                    host.jobs()
                        .iter()
                        .filter(|j| j.scope.path.starts_with(&sid.path) && j.state.is_running())
                        .count(),
                    "process",
                    "processes"
                ),
                secs(took)
            ),
            notes: vec![],
            edit: None,
            needs_in_view: Vec::new(),
            media: None,
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

/// `job_kill` stops a job or a subagent, and the two are stopped differently: a job is
/// reaped (a cgroup, a pid, descendants), a subagent is interrupted, since it owns no
/// process and the turn it runs is the only thing there is to stop.
pub struct JobKill {
    /// `None` when nothing here can start a subagent, which is the honest configuration
    /// for a harness driven without a session registry.
    tasks: Option<std::sync::Arc<dyn crate::builtins::task::TaskRunner>>,
}

impl JobKill {
    /// The no-subagent form: a session that cannot spawn one has nothing for the runner
    /// branch to reach, and `kill_subagent` says so by name rather than silently falling
    /// through to a job lookup that will also fail.
    pub fn new() -> JobKill {
        JobKill { tasks: None }
    }

    /// The daemon's form: the same runner `task` uses, so a handle this stops is a handle
    /// that tool handed out.
    pub fn with_tasks(tasks: std::sync::Arc<dyn crate::builtins::task::TaskRunner>) -> JobKill {
        JobKill { tasks: Some(tasks) }
    }

    /// Stop a subagent, or say why not.
    /// The process host is not consulted and must not be: `task` needs no host, so gating
    /// this on one would leave a session unable to stop its own child.
    fn kill_subagent(&self, handle: &str) -> Result<String, String> {
        match &self.tasks {
            Some(r) => r.kill(handle),
            None => Err(format!(
                "no subagent `{handle}` can be stopped here: this session has no runner \
                 that starts them. Nothing was stopped."
            )),
        }
    }

    /// The subagent half of `job_kill`: `Some` when the call was a child's, `None` to go on
    /// to the host's jobs. A name a child and a job share is refused naming both rather than
    /// guessed, the rule a name two jobs share already follows.
    fn route_subagent(&self, id: &str, jobs: &[JobView]) -> Option<Invocation> {
        use crate::builtins::task::{PickedChild, ambiguous_child, pick_child};
        let children = self
            .tasks
            .as_ref()
            .map(|r| r.children())
            .unwrap_or_default();
        match pick_child(&children, id) {
            PickedChild::Ambiguous(many) => Some(ambiguous_child(id, &many)),
            PickedChild::One(handle) if handle != id.trim() => {
                if let Picked::One(i) = pick_job(jobs, id) {
                    let child = children
                        .iter()
                        .find(|c| c.handle == handle)
                        .map(|c| c.label())
                        .unwrap_or(handle);
                    return Some(Invocation::failed(
                        format!("`{id}` names a job and a subagent, so it does not name one"),
                        format!(
                            "  job: {}\n  subagent: {child}\n\nNothing was stopped. Use the \
                             handle of the one you mean.\n",
                            jobs[i].label()
                        ),
                    ));
                }
                self.kill_subagent(&handle)
                    .ok()
                    .map(|said| Invocation::ok(format!("{said}\n")))
            }
            // An exact child's handle: the runner's answer is the answer, refusal included.
            // Falling through to the host would answer about a table nobody asked about.
            PickedChild::One(handle) => Some(match self.kill_subagent(&handle) {
                Ok(said) => Invocation::ok(format!("{said}\n")),
                Err(why) => {
                    Invocation::failed(format!("`{id}` was not stopped"), format!("{why}\n"))
                }
            }),
            _ => self
                .kill_subagent(id)
                .ok()
                .map(|said| Invocation::ok(format!("{said}\n"))),
        }
    }
}

impl Default for JobKill {
    fn default() -> Self {
        JobKill::new()
    }
}

impl Tool for JobKill {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "job_kill",
            "Stop a job, a subagent, or everything in a scope. Give `job` or `scope`. \
             A job is killed by its cgroup, so every descendant the command started goes \
             with it and there is no pattern that could match something else. **A subagent \
             handle is stopped by interrupting the turn it is running** — a subagent owns \
             no process, so there is no pid or cgroup to report, and a `task_result` on it \
             afterwards says it was stopped rather than answered. The result lists what was \
             actually killed, and says whether anything survived. Use this instead of \
             `pkill` or `kill`.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "job": {"type": "string", "description": "The job to stop: its id (`j65`) or the name the agent stated (`release-build`) — or a subagent, by its handle or its slug."},
                    "scope": {"type": "string", "description": "Kill everything in this scope: `turn`, `session`, or an explicit scope's name."}
                }
            }),
            // It ends processes. Clause 4 wants the widest thing it can do, and
            // "stop the operator's long build" is an exec-class effect.
            Access::Exec,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let job = args.get("job").and_then(|v| v.as_str());
        let scope = args.get("scope").and_then(|v| v.as_str());
        // A subagent first, and before the host is asked for: a session with no process host
        // must still be able to stop its own child, so this cannot sit behind `processes()`.
        // A child may be named by its slug, refused when a job answers to that name too.
        if let Some(id) = job {
            let jobs = ctx
                .backend
                .processes()
                .map(|h| h.jobs())
                .unwrap_or_default();
            if let Some(done) = self.route_subagent(id, &jobs) {
                return done;
            }
        }
        let Some(host) = ctx.backend.processes() else {
            // A host that cannot start processes has no jobs; the runner was asked above, so
            // the sentence names the missing host alone.
            return no_process_host(ctx);
        };
        // What the harness knows and `/proc` may not at the instant of the kill: a process
        // caught between fork and exec reads `[sh] (no command line)`.
        let mut asked_for: Option<String> = None;
        // Which job this was, in the spelling every other sentence uses ([`JobView::label`]);
        // the reaping record below names the cgroup, which answers a different question.
        let mut killed: Option<String> = None;
        let reaping = match (job, scope) {
            (Some(id), _) => {
                // The id or the name the agent stated - see [`resolve_job`].
                let view = match resolve_job(host, id) {
                    Ok(v) => v,
                    Err(inv) => return inv,
                };
                asked_for = Some(view.command.clone());
                killed = Some(view.label());
                host.kill_job(&view.id)
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
        if let Some(job) = &killed {
            body.push_str(&format!("the job was: {job}\n"));
        }
        if let Some(cmd) = &asked_for {
            body.push_str(&format!("the command was: {}\n", clip(cmd, 200)));
        }
        body.push_str(&format!(
            "observed before the kill: {}\n",
            counted(r.observed.len(), "process", "processes")
        ));
        for p in &r.observed {
            body.push_str(&format!(
                "  pid {} {} — {}\n",
                p.pid,
                p.comm,
                clip(&p.label(), 140)
            ));
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
                    "{} survived the kill of {}",
                    counted(r.survivors.len(), "process", "processes"),
                    r.scope
                ),
                body,
            );
        }
        Invocation::ok(body)
    }
}

/// The verb a count takes, for the sentence that puts one beside it: `1 job has settled`,
/// `3 jobs have settled`. The noun and the verb agree with the same number, so they are
/// picked the same way.
fn have(n: usize) -> &'static str {
    if n == 1 { "has" } else { "have" }
}

/// A count, said in words: `1 job`, `3 jobs`. A placeholder is for a writer who does not
/// know the number, and every caller here knows it.
fn counted(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The reap log's section, the count and its verb included. Pure, for `job_list_summary`'s
/// reason: a kill records a reaping, so the sentence a model reads after one is assertable
/// without a host or a cgroup tree.
fn reaps_section(reaps: &[crate::exec::Reaping]) -> String {
    let mut out = format!(
        "\n{} {} ended this session",
        counted(reaps.len(), "scope", "scopes"),
        have(reaps.len())
    );
    if reaps.is_empty() {
        out.push_str(
            " — so this session has reaped nothing yet. That is a fact about the \
             reaper, not evidence that nothing leaked.\n",
        );
        return out;
    }
    out.push_str(", and each recorded what it killed:\n");
    for r in reaps {
        out.push_str(&format!("  {}\n", r.summary()));
        if !r.survivors.is_empty() {
            out.push_str(&format!(
                "    SURVIVORS: {} — this reap did not finish the job\n",
                r.survivors
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    out
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
        let mut s = format!(
            "this session has {} open:\n",
            counted(open.len(), "scope", "scopes")
        );
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
            (
                "job_list",
                JobList.schema().access,
                JobList.schema().description,
            ),
            (
                "job_output",
                JobOutput.schema().access,
                JobOutput.schema().description,
            ),
            (
                "job_wait",
                JobWait.schema().access,
                JobWait.schema().description,
            ),
            (
                "job_kill",
                JobKill::new().schema().access,
                JobKill::new().schema().description,
            ),
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
        // Blocked rather than guarded: if this ever fails, somebody
        // added the argument that brings the whole failure mode back.
        for s in [
            JobList.schema(),
            JobOutput.schema(),
            JobWait.schema(),
            JobKill::new().schema(),
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

    /// A job, as the table holds it - every field a resolver does not read filled with
    /// something obvious, so a test says which facts it is about.
    fn view(id: &str, slug: Option<&str>) -> JobView {
        let scope = || ScopeId {
            kind: ScopeKind::Session,
            name: "s".into(),
            path: std::path::PathBuf::from("/sys/fs/cgroup/x"),
        };
        JobView {
            id: JobId(id.into()),
            command: "cargo test".into(),
            slug: slug.map(|s| s.to_string()),
            scope: scope(),
            owner: scope(),
            cwd: "/".into(),
            pid: 1,
            background: Some(letibot_transcript::Backgrounding::Asked),
            state: crate::exec::JobState::Running,
            elapsed: Duration::from_secs(1),
            ran_for: None,
            produced: 0,
            since_last_output: None,
            memory_cap: crate::exec::MemoryCap::NotAsked,
            memory: None,
            output_file: None,
        }
    }

    /// A job is addressable by its id or by the name the agent stated, and the id wins.
    /// Nothing is a prefix or a guess: `release` is not a way to say `release-build`.
    #[test]
    fn a_job_resolves_by_its_id_or_by_the_name_the_agent_stated() {
        let table = vec![
            view("j1", Some("release-build")),
            view("j2", None),
            // A name that collides with another job's id. The id is the handle the daemon
            // mints, so it wins: a job whose name happens to be `j1` must not steal a call
            // for `j1`.
            view("j3", Some("j1")),
        ];
        let one = |asked: &str| match pick_job(&table, asked) {
            Picked::One(i) => table[i].id.0.clone(),
            Picked::Unknown => panic!("`{asked}` resolved to nothing"),
            Picked::Ambiguous(_) => panic!("`{asked}` was ambiguous"),
        };
        assert_eq!(one("j1"), "j1", "the id is the handle");
        assert_eq!(one("release-build"), "j1", "and the name is the agent's");
        assert_eq!(one("j2"), "j2", "a job nobody named is still addressable");
        assert_eq!(
            one("j3"),
            "j3",
            "by its own id, not by the id its name collides with"
        );

        for asked in ["release", "build", "release-build-2", "j"] {
            assert!(
                matches!(pick_job(&table, asked), Picked::Unknown),
                "`{asked}` was resolved by something other than the whole string"
            );
        }
        // An empty call names nothing. In particular it must not match the wire's spelling of
        // *nobody named this*, which is the empty string.
        assert!(matches!(pick_job(&table, ""), Picked::Unknown));
        assert!(matches!(pick_job(&table, "   "), Picked::Unknown));
        assert!(matches!(
            pick_job(&[view("j9", Some(""))], ""),
            Picked::Unknown
        ));
    }

    /// A name that fits two jobs is refused, not guessed at: acting on whichever sorted
    /// first would act on a job the caller did not name.
    #[test]
    fn a_name_that_fits_two_jobs_is_refused_rather_than_guessed() {
        let table = vec![view("j1", Some("tests")), view("j2", Some("tests"))];
        match pick_job(&table, "tests") {
            Picked::Ambiguous(ids) => assert_eq!(
                ids,
                vec![JobId("j1".into()), JobId("j2".into())],
                "both are named, so the caller can say which it meant"
            ),
            _ => panic!("a name two jobs answer to resolved to one of them"),
        }
        // The way out the refusal names is still unambiguous.
        assert!(matches!(pick_job(&table, "j1"), Picked::One(0)));
        assert!(matches!(pick_job(&table, "j2"), Picked::One(1)));
    }

    /// The miss for an id nothing holds: the subject names the id it refused, and the table it
    /// looked in is counted in words, so a table of one is not reported as `1 jobs`.
    #[test]
    fn an_unknown_job_names_the_id_and_counts_the_table_in_words() {
        let one = unknown_job(&[view("j1", None)], "j999");
        match &one.outcome {
            letibot_transcript::ToolOutcome::Failed { reason } => {
                assert_eq!(reason, "no job called `j999`")
            }
            other => panic!("a miss is a refusal: {other:?}"),
        }
        assert!(
            one.payload.starts_with("this session has 1 job:"),
            "{}",
            one.payload
        );
        let many = unknown_job(&[view("j1", None), view("j2", None)], "j999");
        assert!(
            many.payload.starts_with("this session has 2 jobs:"),
            "{}",
            many.payload
        );
    }

    /// A refusal names the job the way it was ASKED FOR, while the rows beneath it, which
    /// answer which job is which, are drawn by [`JobView::label`]. Asserted together
    /// because the point is the difference between them.
    #[test]
    fn a_refusal_names_the_name_it_refused_and_the_rows_beneath_it_are_labelled() {
        let table = vec![
            view("j1", Some("tests")),
            view("j2", Some("tests")),
            // A third, so the listing is not only the pair that collided.
            view("j3", Some("release-build")),
        ];
        let ids = match pick_job(&table, "tests") {
            Picked::Ambiguous(ids) => ids,
            _ => panic!("`tests` fits two jobs and must not resolve"),
        };
        let refused = ambiguous_job(&table, "tests", &ids);
        let reason = match &refused.outcome {
            letibot_transcript::ToolOutcome::Failed { reason } => reason.clone(),
            other => panic!("an ambiguous name is a refusal: {other:?}"),
        };
        assert!(
            reason.contains("`tests` is the name of 2 jobs"),
            "the refusal must say which name it refused: {reason}"
        );
        assert!(
            !reason.contains("tests · j1"),
            "a refusal must not answer with a label the caller never wrote: {reason}"
        );
        // The rows beneath it ARE labels: there the question is which job is which, and a
        // reader who has to pick one needs the name it was given.
        assert!(
            refused.payload.contains("tests · j1") && refused.payload.contains("tests · j2"),
            "the listing must label the jobs it is offering: {}",
            refused.payload
        );
        // And the way out is unchanged: the id, which identifies one job by construction.
        assert!(
            refused.payload.contains("Use the id"),
            "{}",
            refused.payload
        );
    }

    /// The default is the active jobs, and the count of what it hid is stated: a filter
    /// that silently drops forty settled jobs is the same defect as burying them.
    #[test]
    fn the_default_listing_is_the_active_jobs_and_says_what_it_hid() {
        let head = job_list_summary(None, false, 44, 44, 1);
        assert!(
            head.contains("1 of 44 jobs this session has started are still running"),
            "{head}"
        );
        assert!(head.contains("43 jobs settled and not listed"), "{head}");
        assert!(
            head.contains("`job_list` with `all: true`"),
            "the way back to the finished ones must be named: {head}"
        );
        assert!(
            !head.contains("Nothing is running"),
            "something IS running, so that clause is false here: {head}"
        );

        // With `all`, nothing is hidden and no note is owed - the count is the old one.
        let every = job_list_summary(None, true, 44, 44, 1);
        assert!(
            every.contains("44 jobs this session has started"),
            "{every}"
        );
        assert!(
            !every.contains("not listed") && !every.contains("all: true"),
            "`all` must not claim a hidden count or offer a way back: {every}"
        );

        // Nothing running is an answer, not an empty listing - and it leads, because the live
        // run of this default printed the same fact twice, one line apart (see the function).
        let idle = job_list_summary(None, false, 44, 44, 0);
        assert!(
            idle.starts_with("Nothing is running; 44 jobs have settled"),
            "{idle}"
        );
        assert!(
            idle.contains("`job_list` with `all: true` shows every one"),
            "{idle}"
        );
        assert!(
            !idle.contains("0 of 44"),
            "the accounting line would restate the answer: {idle}"
        );

        // And the scope arm keeps the same promise about what it hid *there*.
        let scoped = job_list_summary(Some("turn"), false, 44, 7, 2);
        assert!(
            scoped.contains("2 of 44 jobs this session started are still running in scope `turn`"),
            "{scoped}"
        );
        assert!(
            scoped.contains("5 settled jobs there are not listed"),
            "{scoped}"
        );
        let scoped_all = job_list_summary(Some("turn"), true, 44, 7, 2);
        assert!(
            scoped_all.contains("7 of 44 jobs this session started are in scope `turn`"),
            "{scoped_all}"
        );
        assert!(!scoped_all.contains("not listed"), "{scoped_all}");
    }

    /// The verb agrees with the count beside it, because a number in words is only half the
    /// sentence: `1 job has settled` and `43 jobs have settled` are the same fact, said two ways.
    #[test]
    fn a_verb_agrees_with_the_count_beside_it() {
        let one = job_list_summary(None, false, 1, 1, 0);
        assert!(
            one.starts_with("Nothing is running; 1 job has settled"),
            "{one}"
        );
        let many = job_list_summary(None, false, 3, 3, 0);
        assert!(
            many.starts_with("Nothing is running; 3 jobs have settled"),
            "{many}"
        );
        let scoped = job_list_summary(Some("turn"), false, 1, 1, 0);
        assert!(scoped.contains("1 job there has settled"), "{scoped}");
        // The one that is still running leaves a count of one settled behind it, and the
        // sentence has to be grammatical for that too.
        let rest = job_list_summary(None, false, 2, 2, 1);
        assert!(rest.contains("1 job settled and not listed"), "{rest}");
        assert!(!rest.contains("1 have"), "{rest}");
    }

    /// The line the reap log opens with: a kill ends the job's cgroup and records one reaping, so
    /// the listing says one scope has ended rather than that nothing was reaped.
    #[test]
    fn one_reaping_is_one_scope_that_has_ended() {
        let one = crate::exec::Reaping {
            scope: ScopeId {
                kind: ScopeKind::Session,
                name: "s".into(),
                path: std::path::PathBuf::from("/sys/fs/cgroup/x"),
            },
            at: std::time::SystemTime::now(),
            mechanism: crate::exec::HOST_KILL,
            observed: Vec::new(),
            survivors: Vec::new(),
            waited: Duration::ZERO,
            removed: true,
            note: None,
        };
        let said = reaps_section(std::slice::from_ref(&one));
        assert!(
            said.starts_with("\n1 scope has ended this session"),
            "{said}"
        );
        assert!(said.contains("recorded what it killed"), "{said}");

        let none = reaps_section(&[]);
        assert!(
            none.starts_with("\n0 scopes have ended this session"),
            "{none}"
        );
        assert!(none.contains("reaped nothing yet"), "{none}");
    }

    /// A count the tool already knows is said in words. `job(s)` is a form for a writer who
    /// does not know the number, and the number is right there - so both shapes are asserted
    /// through the opening line a `job_list` actually prints, and neither placeholder survives.
    #[test]
    fn a_count_is_said_in_words_not_a_placeholder() {
        // The singular and the plural, through the function the listing is rendered by.
        let one = job_list_summary(None, true, 1, 1, 1);
        assert!(
            one.contains("1 job this session has started"),
            "one job is `1 job`: {one}"
        );
        let many = job_list_summary(None, true, 3, 3, 3);
        assert!(
            many.contains("3 jobs this session has started"),
            "three jobs are `3 jobs`: {many}"
        );

        // The nouns no listing of this session's jobs has room for: a scope and a monitor.
        assert_eq!(counted(1, "job", "jobs"), "1 job");
        assert_eq!(counted(3, "job", "jobs"), "3 jobs");
        assert_eq!(counted(1, "scope", "scopes"), "1 scope");
        assert_eq!(counted(2, "monitor", "monitors"), "2 monitors");
        assert_eq!(counted(0, "scope", "scopes"), "0 scopes");
        assert_eq!(counted(4, "process", "processes"), "4 processes");

        for text in [one, many] {
            assert!(!text.contains("(s)"), "a count in a placeholder: {text}");
            assert!(!text.contains("(es)"), "a count in a placeholder: {text}");
        }
    }

    /// `job_kill` reaches a subagent through the runner, which is what makes its handle
    /// meaningful, and a session with no runner refuses by name instead of silently
    /// trying the host.
    #[test]
    fn job_kill_stops_a_subagent_through_its_runner() {
        use crate::builtins::task::{TaskRunner, TaskSpec, TaskStatus};

        /// The runner as a recorder: what it was asked to stop is the assertion.
        struct Recorded(std::sync::Mutex<Vec<String>>);

        impl TaskRunner for Recorded {
            fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
                Err("this runner starts nothing".into())
            }
            fn collect(&self, _handle: &str, _timeout: std::time::Duration) -> TaskStatus {
                TaskStatus::Unknown
            }
            fn kill(&self, handle: &str) -> Result<String, String> {
                self.0.lock().expect("recorded").push(handle.to_string());
                Ok(format!("stopped `{handle}`"))
            }
        }

        // No runner: refused by name, and the refusal is about the runner rather than
        // about a job id - a model that read the other sentence would go looking for a
        // typo in a handle that is correct.
        let said = super::JobKill::new()
            .kill_subagent("s-1-sub-9")
            .expect_err("a session with no runner cannot stop a subagent");
        assert!(said.contains("no runner"), "{said}");
        assert!(
            said.contains("s-1-sub-9"),
            "the refusal names the handle it was given: {said}"
        );

        let runner = std::sync::Arc::new(Recorded(Default::default()));
        let said = super::JobKill::with_tasks(runner.clone())
            .kill_subagent("s-1-sub-9")
            .expect("the runner stopped it");
        assert!(said.contains("stopped"), "{said}");
        assert_eq!(
            runner.0.lock().expect("recorded").as_slice(),
            ["s-1-sub-9".to_string()],
            "the handle the model gave is the handle the runner was asked about"
        );

        // A refused stop is a refusal, not a sentence: whatever the runner says comes back
        // unchanged, so a daemon that refused cannot be reported as a state change.
        struct Refuser;

        impl TaskRunner for Refuser {
            fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
                Err("this runner starts nothing".into())
            }
            fn collect(&self, _handle: &str, _timeout: std::time::Duration) -> TaskStatus {
                TaskStatus::Unknown
            }
            fn kill(&self, _handle: &str) -> Result<String, String> {
                Err("not attached".into())
            }
        }

        let refused = super::JobKill::with_tasks(std::sync::Arc::new(Refuser))
            .kill_subagent("s-1-sub-9")
            .expect_err("a refused stop must not read as a stop");
        assert!(
            refused.contains("not attached"),
            "the runner's own words must reach the caller, unchanged: {refused}"
        );
    }

    /// `job_kill` takes a subagent's slug as it takes a job's name, and refuses a name two
    /// children share - or a child and a job share - without stopping anything. The runner must
    /// be asked about the HANDLE the name stands for.
    #[test]
    fn job_kill_stops_a_subagent_named_by_its_slug_and_refuses_a_shared_name() {
        use crate::builtins::task::{ChildName, TaskRunner, TaskSpec, TaskStatus};
        use letibot_transcript::ToolOutcome;

        struct Named(std::sync::Mutex<Vec<String>>);

        impl TaskRunner for Named {
            fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
                Err("this runner starts nothing".into())
            }
            fn collect(&self, _handle: &str, _timeout: std::time::Duration) -> TaskStatus {
                TaskStatus::Unknown
            }
            fn children(&self) -> Vec<ChildName> {
                [
                    ("s-1-sub-1", "fix-login"),
                    ("s-1-sub-2", "bench"),
                    ("s-1-sub-3", "bench"),
                    // A child this runner knows and will not stop - see `kill`.
                    ("s-1-sub-4", "settled"),
                ]
                .into_iter()
                .map(|(h, s)| ChildName {
                    handle: h.into(),
                    slug: Some(s.into()),
                    own: true,
                    lost: None,
                })
                .collect()
            }
            fn kill(&self, handle: &str) -> Result<String, String> {
                if handle == "s-1-sub-4" {
                    return Err(format!(
                        "nothing is running `{handle}`: its turn has ended, so there is nothing \
                         to stop"
                    ));
                }
                if !self.children().iter().any(|c| c.handle == handle) {
                    return Err(format!("no subagent `{handle}` in this session"));
                }
                self.0.lock().expect("recorded").push(handle.to_string());
                Ok(format!("stopped `{handle}`"))
            }
        }

        let runner = std::sync::Arc::new(Named(Default::default()));
        let kill = super::JobKill::with_tasks(runner.clone());

        let stopped = kill
            .route_subagent("fix-login", &[])
            .expect("a child's name is the child's");
        assert!(
            stopped.payload.contains("stopped `s-1-sub-1`"),
            "{}",
            stopped.payload
        );
        let refused = kill
            .route_subagent("bench", &[])
            .expect("a shared name is refused here");
        assert!(
            matches!(refused.outcome, ToolOutcome::Failed { .. }),
            "{:?}",
            refused.outcome
        );
        assert!(
            refused.payload.contains("bench · s-1-sub-2")
                && refused.payload.contains("bench · s-1-sub-3"),
            "{}",
            refused.payload
        );
        // A name no child carries goes on to the host's jobs.
        assert!(kill.route_subagent("release-build", &[]).is_none());
        // A handle this runner knows, refused, is the refusal - not the host's job table.
        // `job_kill` on a child whose record was never written answered
        // *"this session has started 0 jobs"*, about a child `task_result` was listing.
        let refused_handle = kill
            .route_subagent("s-1-sub-4", &[])
            .expect("a child's own handle is answered here, whatever the answer is");
        assert!(
            matches!(refused_handle.outcome, ToolOutcome::Failed { .. }),
            "{:?}",
            refused_handle.outcome
        );
        assert!(
            refused_handle
                .payload
                .contains("nothing is running `s-1-sub-4`"),
            "{}",
            refused_handle.payload
        );
        // A name a child and a job of this session share names neither.
        let both = kill
            .route_subagent("fix-login", &[view("j7", Some("fix-login"))])
            .expect("a name a job shares is refused here");
        assert!(
            both.payload.contains("fix-login · j7")
                && both.payload.contains("fix-login · s-1-sub-1"),
            "{}",
            both.payload
        );
        assert_eq!(
            runner.0.lock().expect("recorded").as_slice(),
            ["s-1-sub-1".to_string()],
            "the name reached the runner as its handle, and the shared name reached nothing"
        );
    }
}
