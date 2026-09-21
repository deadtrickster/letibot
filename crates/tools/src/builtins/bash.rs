//! `bash` — run a command in the workspace, in a cgroup that owns its lifetime.
//!
//! The single largest gap in `docs/tool-survey.md` §5: *"a shell / exec tool — all
//! five. letibot has none."* What makes ours different is not the shell; it is
//! that the process it starts **has an owner**, and that the two commands a model
//! most often reaches for in a shell — kill-by-pattern and wait-by-pattern — are
//! answered by tools with no pattern in them.
//!
//! # Three refusals, and each hands over what the retry needs
//!
//! | refusal | what it hands over |
//! |---|---|
//! | the gate has nobody to ask | `NotRun` — *nobody decided*, never `Denied`. [`crate::runtime::NoBoundary`] |
//! | the backend cannot start processes | which constructor would, and the fact that it is still not a sandbox |
//! | the command's predicate matches the process running it | the pids, the diagnosis, and the handle-shaped command ([`crate::exec::predicate`]) |
//!
//! # Clause 5, and where the rest of the output is
//!
//! The inline body is capped by line and by byte and **says so with its
//! denominator**. The rest is not in a spill store and is not gone: every `bash`
//! call is a job, and `job_output` reads the job's capture. `docs/tool-survey.md`
//! §3.5's worst case — a clip *"with no locator and no way back"* — is not
//! reachable, because the locator is the job id the result already printed.
//!
//! Shell output keeps the **tail**. A build that fails prints the error last.
//!
//! # Two ways into the background, and the timeout is the kill deadline
//!
//! | | how |
//! |---|---|
//! | the model asks | `background: true` — the job starts in the session scope and runs with no deadline |
//! | a person moves it | from a head (Ctrl+B), through [`ProcessHost::promote`]. See that method for what the head must send |
//!
//! A foreground command that outlives [`DEFAULT_TIMEOUT_MS`] is **killed**, not
//! moved — that is opencode's `timeout` semantics, and a command the model did not
//! ask to run longer than the default must not run forever. The timeout is set
//! unless the model asks otherwise: `timeout_ms` raises it, `background: true`
//! removes it.
//!
//! ## The outcome is a variant, not a wording
//!
//! A backgrounded command comes back as
//! [`letibot_transcript::ToolOutcome::Backgrounded`]; a killed one as
//! [`letibot_transcript::ToolOutcome::Timeout`]. The outcomes it must not be:
//!
//! | | what the model would conclude |
//! |---|---|
//! | `Ok` with a short body | the command produced nothing |
//! | `Failed` | retry — and now two builds are running |
//! | `Backgrounded` for a kill | the command is still running, and it is not |

use std::time::Duration;

use letibot_transcript::Backgrounding;
use serde_json::Value;

use crate::exec::predicate::{Verdict, annotation, refusal};
use crate::exec::{JobState, ProcessHost, Promotion, ScopeKind, SpawnRequest, Waited};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

pub struct Bash;

/// What goes inline before the cap bites. Both are reported when they do.
const MAX_INLINE_BYTES: usize = 30_000;
const MAX_INLINE_LINES: usize = 400;

/// **The default kill deadline, matching opencode's `bashDefaultTimeoutMs`
/// (2 minutes).**
///
/// A foreground command that is still running after this many milliseconds is
/// **killed**, and the result says it timed out and how to ask for more. The model
/// overrides it with `timeout_ms`; `background: true` runs with no deadline, and a
/// person can move a running command to the background from the head, which also
/// removes the deadline.
///
/// opencode's own default is 120000 ms, and leticode is its parity port: a command
/// that the model did not ask to run longer than the default must not run forever.
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// The longest a deadline may be asked for. Matches opencode's 600000 ms ceiling.
const MAX_TIMEOUT_MS: u64 = 600_000;

impl Tool for Bash {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "bash",
            "Run a shell command in the session's workspace and return its output. \
             Give `command`; optionally `cwd` (relative to the workspace), \
             `timeout_ms`, and `background: true` to start it and return \
             immediately. A command you did NOT mark background is moved to the \
             background by the runtime once it has run longer than `timeout_ms`, and \
             the result says so, names its job id and says which call gets its \
             output — it is still running, it did not fail, and starting it again \
             would give you two. Every run is a job with an id: output is capped \
             inline and the rest is read with `job_output`, and a job's completion \
             **arrives on its own** when it ends — you are told, unprompted, so you \
             do not sit and wait on it; a job is stopped with `job_kill`. Every process lands in a cgroup \
             owned by a scope, so a foreground command dies with the turn and a \
             background one with the session unless you name an `scope`. Do not \
             write `pkill`, `pgrep` or a `while ... sleep` wait loop: those match the \
             process running them, and `job_kill` and `job_wait` take a job id \
             instead of a pattern. When this session has a boundary, the command runs \
             in a filesystem view rooted at the workspace: a path outside that view \
             is absent rather than denied, and a result that says a path is not in \
             the view is a fact about the boundary, not about whether anything is \
             there — asking again with a different spelling will not reach it.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The command line, run by /bin/sh in the workspace."},
                    "cwd": {"type": "string", "description": "Directory to run in, relative to the workspace root. Defaults to the root."},
                    "timeout_ms": {"type": "integer", "description": "How long this command may run before it is killed and the result says so. Defaults to 120000 (2 minutes); set it higher for a command you know takes longer. Ignored when background is true."},
                    "background": {"type": "boolean", "description": "Start the command and return its job id at once instead of waiting."},
                    "scope": {"type": "string", "description": "Which scope owns the process: `turn` (dies at the end of this turn), `session` (dies with the session), or `explicit` (survives the session; requires `scope_name`)."},
                    "scope_name": {"type": "string", "description": "Names an `explicit` scope so it can be listed and ended later."}
                },
                "required": ["command"]
            }),
            // Declared as the widest thing it can do, per clause 4. Everything a
            // shell command might do is behind this one word, which is why the
            // gate is consulted on every call including the ones that only `ls`.
            Access::Exec,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "bash needs a command",
                "call `bash` again with `command` set to the shell command to run.",
            );
        };
        if command.trim().is_empty() {
            return Invocation::failed(
                "bash was given an empty command",
                "nothing was run. Give `command` a shell command line.",
            );
        }

        // **What the workspace looked like before this command.** A shell that
        // rewrites a file hands the head nothing, so the change lands with no
        // diff and no record; the sweep after the command is what turns it back
        // into an edit card. See `crate::detect` — one `git status` on a clean
        // tree, and nothing read.
        let root = ctx.backend.root_path().map(std::path::PathBuf::from);
        let before = root.as_deref().map(crate::detect::before);

        // The second gate, and it is a different mechanism from the first: a
        // session whose backend cannot start processes refuses here however the
        // adjudicator answered. Same asymmetry `write` keeps.
        let Some(host) = ctx.backend.processes() else {
            return Invocation::failed(
                "this session's backend cannot start processes",
                format!(
                    "nothing was run. The backend is `{}`. There are two constructors \
                     that can run commands and they are not interchangeable. \
                     `HostBackend::confined` gives the boundary: a cgroup v2 scope \
                     that owns the process's lifetime AND project-scoped mount, PID, \
                     network and user namespaces, so a path outside the workspace is \
                     absent rather than denied. `HostBackend::executable` gives the \
                     lifetime half only — a command there reads this user's whole \
                     filesystem, sees every process, and reaches the network, because \
                     a cgroup bounds a lifetime and not a view. Either one refuses to \
                     exist rather than degrade: no delegated cgroup v2 subtree, or no \
                     usable namespaces, and the session has no exec path at all.",
                    ctx.backend.describe()
                ),
            );
        };

        // T21.1 and T21.2. Before anything is started, because the diagnosis is
        // only useful if it arrives instead of the damage.
        let protected = host.protected();
        let verdict = crate::exec::predicate::examine(command, &protected);
        let mut notes: Vec<String> = Vec::new();
        match &verdict {
            Verdict::Refuse(h) => {
                return Invocation::failed(
                    "the command's process predicate matches the process that would \
                     run it, so it was not run",
                    refusal(command, h),
                );
            }
            Verdict::Annotate(h) => notes.push(annotation(h)),
            Verdict::Clear => {}
        }

        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let cwd = args.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");
        if ctx.backend.stat(cwd).is_none() && cwd != "." {
            let (dir, entries) = super::nearest_listing(ctx.backend, cwd);
            return Invocation::failed(
                format!("no directory at `{cwd}`, so nothing was run"),
                super::render_listing(&dir, &entries, 40),
            )
            .with_note(format!(
                "`{cwd}` does not exist. The listing of `{dir}` is below; run under \
                 one of these, or drop `cwd` to run at the workspace root."
            ));
        }

        // D4: *"if a vm is temporary then it is a session cgroup"*. A foreground
        // command is temporary in the turn; a background one is not, and giving it
        // the turn scope would reap it before the next call could read it.
        let asked_scope = args.get("scope").and_then(|v| v.as_str());
        let default_scope = if background {
            ScopeKind::Session
        } else {
            ScopeKind::Turn
        };
        let scope = match asked_scope.map(ScopeKind::parse) {
            None => default_scope,
            Some(Some(k)) => k,
            Some(None) => {
                return Invocation::failed(
                    format!("`{}` is not a scope", asked_scope.unwrap_or("")),
                    "there are three and no fourth: `turn` (reaped when this turn \
                     ends), `session` (reaped when the session ends), `explicit` \
                     (survives the session, and needs `scope_name` so it can be \
                     listed and ended). Nothing was run.",
                );
            }
        };
        let scope_name = args
            .get("scope_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if scope == ScopeKind::Explicit && scope_name.is_none() {
            return Invocation::failed(
                "an `explicit` scope must be named",
                "an explicit scope outlives this session, so an unnamed one is a \
                 process nobody can refer to in order to end it — which is the leak \
                 with an extra step. Call `bash` again with `scope_name`. Nothing \
                 was run.",
            );
        }

        // The foreground deadline **is** the kill point, matching opencode: a
        // command that outlives `timeout_ms` is killed, not left running.
        let timeout = Duration::from_millis(
            args.get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_TIMEOUT_MS)
                .clamp(1, MAX_TIMEOUT_MS),
        );

        let cwd = match ctx.backend.workdir(cwd) {
            Ok(c) => c,
            Err(e) => {
                return Invocation::failed(format!("cwd `{cwd}`: {e}"), String::new());
            }
        };
        let req = SpawnRequest {
            command: command.to_string(),
            cwd,
            scope,
            scope_name,
            background,
            // So a helper the command runs can say what it is running for —
            // `letibot-askpass` puts it on the password card.
            env: vec![("LETIBOT_COMMAND".to_string(), command.to_string())],
        };
        let id = match host.spawn(&req) {
            Ok(id) => id,
            Err(e) => {
                return Invocation::failed(
                    "the command did not start",
                    format!("{e}\n\nNothing is running and nothing was left behind."),
                );
            }
        };

        if background {
            let view = host.job(&id);
            // **`Backgrounded`, not `Ok`, even though the model asked.** The
            // outcome names a fact about the world — *this command is running and
            // has not answered yet* — and that fact does not depend on who wanted
            // it. Reporting `Ok` here would make a `bash --background` result
            // grounding for an answer nothing has produced, which is exactly what
            // `is_grounded` exists to prevent, and it would leave a head with two
            // shapes to render for one situation.
            let mut inv = Invocation::backgrounded(
                id.0.clone(),
                Duration::ZERO,
                Backgrounding::Asked,
                format!(
                    "carry on — `{id}`'s completion is delivered to you on its own when \
                     it ends, so there is nothing to wait for. `job_output` with \
                     job=\"{id}\" reads what it has written so far; `job_kill` stops it."
                ),
                format!(
                    "started `{id}` in the background.\n  command: {command}\n  \
                     pid: {}\n  scope: {} — {}\n\nIt is running now, and **its \
                     completion will reach you by itself when it ends — do not wait for \
                     it, and do not poll.** Carry on with something else; when the job \
                     finishes you are told, unprompted, with its command, how it ended \
                     and where its output is. `job_kill` stops it. (`job_wait` with \
                     job=\"{id}\" exists for a job you must have the result of before you \
                     can do anything else — waiting on a job you just backgrounded is \
                     giving back the floor you gave up.)",
                    view.as_ref().map(|v| v.pid).unwrap_or(0),
                    scope.as_str(),
                    scope.reaped_when(),
                ),
            );
            for n in notes {
                inv = inv.with_note(n);
            }
            return inv;
        }

        // Foreground. Progress reports WORK DONE — bytes produced — and never
        // "still alive": §8.5, and `liveness-indicators-measure-the-wrong-thing`.
        let foreground = wait_with_progress(ctx, host, &id, timeout);

        let Ok(out) = host.output(&id, 0, usize::MAX) else {
            return Invocation::failed(
                format!("`{id}` started but its output could not be read"),
                "this is a harness defect, not a command failure. The process is in \
                 its scope and will be reaped with it.",
            );
        };
        let full = out.text();
        let (body, capped) = clip_tail(&full, MAX_INLINE_BYTES, MAX_INLINE_LINES);

        // **Absence must be legible.** A path the mount namespace does not contain
        // produces a bare `ENOENT`, and a model reads that as *the file does not
        // exist* — an answer about an empty haystack, and the same defect as a
        // `grep` reporting `0` over a scope it failed to open. The confinement turns
        // it into *"not in this session's filesystem view"*.
        //
        // Scanned over the WHOLE capture rather than the clipped body: a path in the
        // dropped head is exactly the one whose diagnosis got lost.
        //
        // Nothing here stats the host, so the note never says whether the path
        // exists outside — that is not a fact the model needs and not one this
        // boundary should hand out.
        // **And the boundary's own failure is not the command's answer.** F5, in the
        // shape that is easy to miss: a helper that could not set up the namespaces
        // exits with a code indistinguishable from a command's, and rendering it as
        // `[exit 1] the command ran and exited non-zero` tells the model something
        // false about what happened.
        // The boundary's own reading of what this command could not see: the
        // sentences for the model, and the same finding as PATHS for the runtime,
        // which is the layer that can do something about it — see
        // `ToolRuntime::invoke`'s grant request.
        for n in host.absence_notes(&full) {
            notes.push(n);
        }
        let outside = host.outside_paths(&full);
        let launcher_failed = host.launcher_failure(&full);

        // **A person moved it mid-flight** (Ctrl+B). The outcome is `Backgrounded`
        // with the operator named, not a deadline kill — the command is still
        // running, and the handle is the way back.
        if let Foreground::Promoted(p) = foreground {
            let next = format!(
                "carry on — `{id}`'s completion will be delivered to you on its own \
                 when it ends, so there is nothing to wait for. `job_output` with \
                 job=\"{id}\" reads what it has written so far."
            );
            let mut inv = Invocation::backgrounded(
                id.0.clone(),
                p.ran_for,
                p.how.clone(),
                &next,
                format!(
                    "{body}\n\n[`{id}` was moved to the background — you did not ask \
                     for it]\n  command: {command}\n  owned by: {} — {}\n\nIts \
                     completion will reach you on its own when it ends; do not wait on it.\n",
                    p.to,
                    p.to.kind.reaped_when()
                ),
            );
            if !p.complete() {
                inv = inv.with_note(format!(
                    "the promotion is PARTIAL. {} Those processes are still reaped \
                     when this turn ends; the rest outlive it.",
                    p.summary()
                ));
            }
            if capped || !out.complete() {
                inv = inv.with_note(format!(
                    "output was capped inline at {MAX_INLINE_BYTES} bytes / \
                     {MAX_INLINE_LINES} lines, keeping the TAIL. {} Call `job_output` \
                     with job=\"{id}\" for the rest.",
                    out.denominator(&id)
                ));
            }
            for n in notes {
                inv = inv.with_note(n);
            }
            return inv;
        }

        let state = match foreground {
            Foreground::State(s) => s,
            Foreground::Promoted(_) => unreachable!("handled above"),
        };
        let mut inv = match &state {
            // **The deadline, opencode's.** The command outlived `timeout_ms`, so it
            // is **killed**, not promoted. Killing is the point: a command the model
            // did not ask to run longer than the default must not run forever. A
            // model that wants it to outlive the deadline says `background: true`, or
            // a person moves it from the head.
            JobState::Running => {
                let reaped = host.kill_job(&id);
                let mut inv = Invocation::timed_out(format!(
                    "{body}\n\n[the command `{id}` was killed after {:.0}s — it outlived \
                     its deadline]\n  command: {command}\n\nIt did not fail; it was \
                     stopped. To run it longer, call `bash` again with a larger \
                     `timeout_ms`, or `background: true` to run it with no deadline.",
                    timeout.as_secs_f32()
                ));
                if let Err(e) = &reaped {
                    inv = inv.with_note(format!("the kill did not complete: {e}"));
                }
                inv
            }
            JobState::NotScoped => Invocation::failed(
                format!("`{id}` could not join its scope, so the command was NOT run"),
                format!(
                    "{body}\n\nThe wrapper failed to put the process in its cgroup and \
                     refused to exec rather than start a process nothing would reap. \
                     Nothing ran."
                ),
            ),
            // Checked before the exit code is read as an answer, because that is
            // exactly the reading it must not get.
            _ if launcher_failed.is_some() => Invocation::failed(
                format!(
                    "`{id}`'s boundary did not come up, so nothing can be concluded from its exit"
                ),
                format!(
                    "{body}\n\n{}\n\nThis is `not_run` in substance: the confinement is \
                     the thing that failed, and running the command without it is not \
                     an available outcome.",
                    launcher_failed.clone().unwrap_or_default()
                ),
            ),
            JobState::Exited { code: 0 } => Invocation::ok(body),
            // A non-zero exit is the COMMAND's answer, not a harness failure, and it
            // is `ok` with the code stated. Reporting it as `failed` would make a
            // `grep` that found nothing indistinguishable from a broken tool.
            JobState::Exited { code } => Invocation::ok(format!(
                "{body}\n\n[exit {code}] the command ran and exited non-zero. That is \
                 the command's answer, not a harness failure."
            )),
            other => Invocation::failed(
                format!("`{id}` {}", other.word()),
                format!("{body}\n\nThe process did not exit on its own."),
            ),
        };

        // **And what it changed.** Attached as an edit when it is one file, which
        // is the shape a script-that-edits has; named otherwise, because
        // `ToolResult` carries one card and a merge touching forty files is not
        // one card. Either way the operator is told, which is the whole point —
        // a change nobody can see is a change nobody reviewed.
        if let (Some(root), Some(base)) = (root.as_deref(), before.as_ref()) {
            // Names first, and only names: one `git status` and a stat each. The
            // diff below is computed for the ONE file it is drawn for, never for
            // every file that moved — a merge touching forty of them costs forty
            // stats, not forty reads and forty `git show`s.
            let changed = crate::detect::changed_since(root, base);
            match changed.len() {
                0 => {}
                1 => {
                    if let Some(c) = crate::detect::diff_of(root, base, &changed[0]) {
                        let was = c.before.clone().unwrap_or_default();
                        inv = inv.with_note(format!(
                            "this command changed `{}` — the diff beside it was detected \
                             afterwards, not made by `edit`",
                            c.path
                        ));
                        inv.edit = Some(crate::edit::FileEdit {
                            path: c.path.clone(),
                            before: was.clone(),
                            after: c.after.clone(),
                            created: c.created,
                            before_digest: crate::spill::content_hash(was.as_bytes()),
                            after_digest: crate::spill::content_hash(c.after.as_bytes()),
                            replacements: 1,
                            changed: crate::edit::changed_span(&was, &c.after),
                        });
                    }
                }
                n => {
                    let names: Vec<&str> = changed.iter().take(12).map(String::as_str).collect();
                    inv = inv.with_note(format!(
                        "this command changed {n} file(s): {}{}. No diff is shown for a \
                         change this wide; `git diff` is the one to read.",
                        names.join(", "),
                        if n > names.len() { ", …" } else { "" }
                    ));
                }
            }
            if base.skipped > 0 {
                inv = inv.with_note(format!(
                    "{} file(s) were already modified and beyond the snapshot budget, so a \
                     change to one of them is not reported",
                    base.skipped
                ));
            }
        }

        // Clause 5's half of the bargain: the cap is stated with its denominator
        // and the way to the rest is a call, not advice.
        if capped || !out.complete() {
            inv = inv.with_note(format!(
                "output was capped inline at {MAX_INLINE_BYTES} bytes / \
                 {MAX_INLINE_LINES} lines, keeping the TAIL. {} Call `job_output` \
                 with job=\"{id}\" — and `offset` if you want a different window — \
                 for the rest.",
                out.denominator(&id)
            ));
        }
        for n in notes {
            inv = inv.with_note(n);
        }
        // Carried to the runtime, which is the layer that holds the gate and can
        // therefore ASK about widening the view. The tool only reports what the
        // boundary told it.
        inv.needs_in_view = outside;
        inv
    }
}

/// How a foreground wait ended: the command's own state, or a head's Ctrl+B moving
/// it to the background mid-flight.
enum Foreground {
    State(JobState),
    /// A person moved the command to the background from a head. The promotion is
    /// the record; the job is still running and recoverable.
    Promoted(Promotion),
}

/// Wait, emitting progress that is a measurement of work rather than a heartbeat,
/// and honour a head's Ctrl+B by promoting the command mid-flight.
fn wait_with_progress(
    ctx: &mut InvokeCtx<'_>,
    host: &dyn ProcessHost,
    id: &crate::exec::JobId,
    timeout: Duration,
) -> Foreground {
    let started = std::time::Instant::now();
    let step = Duration::from_millis(500);
    let mut last_reported = 0u64;
    loop {
        // A head asked to move this to the background. Honour it here, on the
        // worker's own poll, because the worker is the thing that is blocked and
        // the signal arrived on the hub's promote channel rather than its queue.
        if let Some(identity) = ctx.backend.promote_requested() {
            let promoted = host.promote(
                id,
                ScopeKind::Session,
                None,
                Backgrounding::Operator { identity },
            );
            // Whether the move succeeded or not, it is the answer to the request:
            // the failure path is reported, not retried into a second promotion.
            return match promoted {
                Ok(p) => Foreground::Promoted(p),
                Err(_) => Foreground::State(JobState::Running),
            };
        }
        let left = timeout.saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        match host.wait_job(id, step.min(left)) {
            Ok(Waited::Happened { state: Some(s), .. }) => return Foreground::State(s),
            Ok(_) => {}
            Err(_) => break,
        }
        if let Some(v) = host.job(id) {
            // Only when the number MOVED. A progress event that repeats an
            // unchanged count is the liveness signal this is not supposed to be.
            if v.produced != last_reported {
                last_reported = v.produced;
                ctx.progress(format!(
                    "`{id}`: {} bytes of output after {:.1}s",
                    v.produced,
                    v.elapsed.as_secs_f32()
                ));
            }
        }
    }
    Foreground::State(host.job(id).map(|v| v.state).unwrap_or(JobState::Running))
}

/// Keep the tail, by bytes and by lines, and say whether anything was dropped.
///
/// pi's split, and the reasoning is the same: `read` keeps the head of a file,
/// shell output keeps the tail, because a command that failed says why last. The
/// cut is on a line boundary — a head joined to a tail mid-line reads as one line
/// that never existed.
fn clip_tail(text: &str, max_bytes: usize, max_lines: usize) -> (String, bool) {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept: Vec<&str> = if lines.len() > max_lines {
        lines[lines.len() - max_lines..].to_vec()
    } else {
        lines.clone()
    };
    let mut capped = kept.len() < lines.len();
    while kept.iter().map(|l| l.len() + 1).sum::<usize>() > max_bytes && !kept.is_empty() {
        kept.remove(0);
        capped = true;
    }
    (kept.join("\n"), capped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tail_is_what_survives_the_cap() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let (kept, capped) = clip_tail(&text, 100_000, 10);
        assert!(capped);
        assert!(kept.starts_with("line 90"), "{kept}");
        assert!(kept.ends_with("line 99"), "{kept}");
    }

    #[test]
    fn a_cut_lands_on_a_line_boundary() {
        let text = "aaaaaaaaaa\nbbbbbbbbbb\ncccccccccc\n";
        let (kept, capped) = clip_tail(text, 15, 100);
        assert!(capped);
        // No partial line: whatever survived is whole.
        assert!(text.lines().any(|l| l == kept.lines().next().unwrap()));
    }

    #[test]
    fn the_description_declares_exec_and_says_how_to_use_the_tool() {
        let s = Bash.schema();
        assert_eq!(s.access, Access::Exec);
        assert!(!s.access.is_unattended(), "exec must reach the gate");
        assert_eq!(crate::schema::lint_description(&s.description), vec![]);
    }
}
