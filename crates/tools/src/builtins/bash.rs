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
//! # Three ways into the background, and a timeout is none of them
//!
//! The operator wanted all three and they are three, not one dressed up:
//!
//! | | how |
//! |---|---|
//! | the model asks | `background: true` — the job starts in the session scope |
//! | **the runtime promotes it** | it outlived [`PROMOTE_AFTER_MS`] in the foreground, so it is **moved** into the session scope mid-flight |
//! | a person promotes it | from a head, through [`ProcessHost::promote`]. See that method for what the head must send |
//!
//! ## The promotion is reactive, and that is the whole design
//!
//! It fires on **elapsed time**, never on the command text. A predictive rule —
//! *"`sleep` and `cargo build` are slow, so background them"* — cannot be written
//! correctly, because `cargo build` is three seconds on a warm tree and four
//! minutes on a cold one, and the same string is both. Guessing from the string is
//! `docs/closed-loop.md`'s open-loop stepper in a new costume: a rule with no
//! feedback path. Measuring elapsed time is the encoder.
//!
//! ## And a promotion the model does not notice is the defect, not the promotion
//!
//! It comes back as [`letibot_transcript::ToolOutcome::Backgrounded`], which is a
//! variant and not a wording. The three outcomes it must not be:
//!
//! | | what the model would conclude |
//! |---|---|
//! | `Ok` with a short body | the command produced nothing |
//! | `Failed` | retry — and now two builds are running |
//! | `Timeout` | it was abandoned. `Timeout` means exactly that, and this job is **still running and still recoverable** |
//!
//! The old code said *"It is now in the `session` scope"* in a `Failed` result and
//! nothing had moved: the job's cgroup was still a child of the turn's, so the
//! turn's end reaped the work the sentence promised would outlive it. The sentence
//! is now produced by [`ProcessHost::promote`]'s measurement instead of asserted
//! next to it.

use std::time::Duration;

use letibot_transcript::Backgrounding;
use serde_json::Value;

use crate::exec::predicate::{Verdict, annotation, refusal};
use crate::exec::{JobState, ProcessHost, ScopeKind, SpawnRequest, Waited};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

pub struct Bash;

/// What goes inline before the cap bites. Both are reported when they do.
const MAX_INLINE_BYTES: usize = 30_000;
const MAX_INLINE_LINES: usize = 400;

/// **How long a command stays in the foreground before the runtime backgrounds
/// it.** 15 seconds.
///
/// Chosen from a measurement rather than from taste. Every command the tool set
/// exists to run was timed on this box, warm, while it was under an ordinary
/// working load (load average 1.4):
///
/// | command | wall |
/// |---|---|
/// | `ls -R crates`, `find`, `grep -rn`, `wc` | < 0.02 s |
/// | `cargo check -p letibot-tools` | 1.9 s |
/// | `cargo check --workspace` | 2.1 s |
/// | `cargo clippy --workspace --all-targets` | 2.5 s |
/// | `cargo test -p letibot-tools` | 5.7 s |
/// | `python3 tests/fidelity/run_gate.py` | 6.5 s |
///
/// So the routine set tops out at 6.5 s, and 15 s clears the slowest of them by
/// **2.3×** — enough headroom that a busy box does not start promoting the
/// ordinary work, which is the failure mode of a threshold set too tight. The
/// other end is the operator's own example: *"if model does something like `sleep
/// 90` it is a background no matter what"*. 15 s is 6× under that, so the case
/// that motivated this promotes with room to spare.
///
/// What the number trades is legible in both directions and neither cost is
/// hidden: too low and a command that would have finished inline costs the model
/// an extra `job_wait` round trip; too high and the turn blocks. The previous
/// default was 120 s, which would have let `sleep 90` block the turn for a minute
/// and a half and never promote at all.
///
/// `timeout_ms` overrides it per call, which is the model saying how much
/// foreground patience it has for this one command.
pub const PROMOTE_AFTER_MS: u64 = 15_000;
/// The longest a foreground wait may be asked for. Past this the honest answer is
/// to let it go to the background and come back for it.
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
             inline and the rest is read with `job_output`, a running job is watched \
             with `job_wait` and stopped with `job_kill`. Every process lands in a cgroup \
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
                    "timeout_ms": {"type": "integer", "description": "How long to keep this command in the foreground before the runtime moves it to the background and returns its job id. Ignored when background is true."},
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

        // The foreground deadline **is** the promotion point. One number and one
        // name: two thresholds with overlapping meaning is how a model ends up
        // reasoning about the wrong one.
        let timeout = Duration::from_millis(
            args.get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(PROMOTE_AFTER_MS)
                .clamp(1, MAX_TIMEOUT_MS),
        );

        let req = SpawnRequest {
            command: command.to_string(),
            cwd: cwd.to_string(),
            scope,
            scope_name,
            background,
            env: vec![],
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
                    "call `job_wait` with job=\"{id}\" and a `timeout_ms`, or \
                     `job_output` with job=\"{id}\""
                ),
                format!(
                    "started `{id}` in the background.\n  command: {command}\n  \
                     pid: {}\n  scope: {} — {}\n\nIt is running now. `job_wait` with \
                     job=\"{id}\" blocks until it finishes (a deadline is required, and \
                     its expiry is reported as its own outcome, not as completion); \
                     `job_output` with job=\"{id}\" reads what it has written so far; \
                     `job_kill` stops it.",
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
        let state = wait_with_progress(ctx, host, &id, timeout);

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
        let mut launcher_failed = None;
        if let Some(c) = host.confinement() {
            for n in c.absence_notes(&full) {
                notes.push(n);
            }
            launcher_failed = c.launcher_failure(&full);
        }

        let mut inv = match &state {
            // **The promotion.** Reactive: it fired because the command outlived
            // the threshold, and the threshold knows nothing about what the
            // command is. Nothing is killed — killing would throw away work the
            // model asked for — and nothing blocks, because the turn is what the
            // operator is waiting on.
            JobState::Running => {
                let promoted =
                    host.promote(&id, ScopeKind::Session, None, Backgrounding::Promoted);
                let next = format!(
                    "call `job_wait` with job=\"{id}\" and a `timeout_ms` to block \
                     until it finishes, or `job_output` with job=\"{id}\" to read what \
                     it has written so far"
                );
                match promoted {
                    Ok(p) => {
                        let mut inv = Invocation::backgrounded(
                            id.0.clone(),
                            p.ran_for,
                            Backgrounding::Promoted,
                            &next,
                            format!(
                                "{body}\n\n[this is what `{id}` had written when it was \
                                 moved, not its finished output]\n\
                                 command:  {command}\n\
                                 owned by: {} — {}\n",
                                p.to,
                                p.to.kind.reaped_when()
                            ),
                        );
                        // **A partial migration is said out loud.** Some processes
                        // still being reaped by the turn is precisely the case
                        // where "it outlives this turn" would be a false sentence,
                        // and it is the sentence the old code asserted without
                        // measuring anything.
                        if !p.complete() {
                            inv = inv.with_note(format!(
                                "the promotion is PARTIAL. {} Those processes are \
                                 still reaped when this turn ends; the rest outlive \
                                 it.",
                                p.summary()
                            ));
                        }
                        inv
                    }
                    // The move failed, so the job is still owned by the turn and
                    // WILL die with it. Reporting `Backgrounded` here would be the
                    // announced-but-not-done failure: a handle the model would keep
                    // for a process about to be reaped.
                    Err(e) => Invocation::failed(
                        format!(
                            "`{id}` outran its {:.0}s foreground deadline and could \
                             NOT be moved out of the turn scope",
                            timeout.as_secs_f32()
                        ),
                        format!(
                            "{body}\n\n{e}\n\nIt is still running, and it is still \
                             owned by this turn — so it is reaped when the turn ends \
                             and the handle `{id}` will stop being useful. Read what \
                             it has now with `job_output` job=\"{id}\", or stop it \
                             with `job_kill`."
                        ),
                    ),
                }
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
                format!("`{id}`'s boundary did not come up, so nothing can be concluded from its exit"),
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
        inv
    }
}

/// Wait, emitting progress that is a measurement of work rather than a heartbeat.
fn wait_with_progress(
    ctx: &mut InvokeCtx<'_>,
    host: &dyn ProcessHost,
    id: &crate::exec::JobId,
    timeout: Duration,
) -> JobState {
    let started = std::time::Instant::now();
    let step = Duration::from_millis(500);
    let mut last_reported = 0u64;
    loop {
        let left = timeout.saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        match host.wait_job(id, step.min(left)) {
            Ok(Waited::Happened {
                state: Some(s), ..
            }) => return s,
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
    host.job(id).map(|v| v.state).unwrap_or(JobState::Running)
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
