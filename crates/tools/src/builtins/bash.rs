//! `bash`: run a command in the workspace, in a cgroup that owns its lifetime.
//! Unlike a shell, the process it starts has an owner, and the two commands a model reaches for
//! most, kill-by-pattern and wait-by-pattern, are answered by tools with no pattern in them.

use std::time::Duration;

use letibot_transcript::{Backgrounding, text::tail_lines};
use serde_json::Value;

use crate::exec::memory::{Ask, Decision, Meminfo, MemoryAsk, MemoryGrant, MemoryPolicy};
use crate::exec::predicate::{Verdict, annotation, refusal};
use crate::exec::terminal;
use crate::exec::{JobState, ProcessHost, Promotion, ScopeKind, SpawnRequest, Waited};
use crate::runtime::{Invocation, InvokeCtx, OperatorRun, Tool, WantedNow};
use crate::schema::{Access, ToolSchema};

/// The `bash` tool. It carries the operator's memory policy because the policy is part of what
/// its description says, and the description is fixed when the session opens.
#[derive(Debug, Default)]
pub struct Bash {
    memory: MemoryPolicy,
    /// Whether this session's model has been told that some modified files are past the
    /// snapshot budget. Said once: it is a fact the model can do nothing about.
    snapshot_budget_said: std::sync::atomic::AtomicBool,
}

impl Bash {
    /// `bash` under the operator's `[memory]` settings.
    pub fn with_memory(memory: MemoryPolicy) -> Bash {
        Bash {
            memory,
            snapshot_budget_said: Default::default(),
        }
    }
}

/// What goes inline before the cap bites. Both are reported when they do.
const MAX_INLINE_BYTES: usize = 30_000;
const MAX_INLINE_LINES: usize = 400;

/// The default kill deadline, two minutes.
/// A foreground command still running after this is killed, and the result says how to ask for more:
/// `timeout_ms` raises it, `background: true` removes it, and the operator's own run has none.
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// The longest a deadline may be asked for.
const MAX_TIMEOUT_MS: u64 = 600_000;

/// The `memory` argument's description, with the operator's two numbers in it. Rendered from the
/// policy rather than from what is free, because the schema is part of the session's stable
/// prefix.
fn memory_arg(p: &MemoryPolicy) -> String {
    format!(
        "The memory cap for this command: a size (`512M`, `4G`) or a fraction of what is free \
         when it starts (`0.25`, `25%`). Omitted, the command runs under {} — {}. Up to {} \
         is granted at once; more than that — or `uncapped` — is put to the operator as a \
         question first, and the command does not start unless they agree. A result says so \
         when the command was killed at its cap or came near it.",
        p.default,
        p.default_from.describe(),
        p.ceiling
    )
}

/// The sentence the description ends with: the default in force and who set it. A build tool is
/// the call the default is most often wrong for, so it is named, with the line that changes it.
fn default_sentence(p: &MemoryPolicy) -> String {
    format!(
        " **A call that names no `memory` runs under {}** ({}). Compilers, linkers and test \
         runners (`cargo`, `rustc`, `go build`, `npm`) usually need gigabytes: ask for it in \
         `memory`, or the project can raise its own default with `[memory] default` in its \
         leticode.toml.",
        p.default,
        p.default_from.describe()
    )
}

/// The note a model call's result carries about memory. Every settled run reports its peak - the
/// model chose the cap and a number it never sees is one it can only guess at next time - and a
/// kill, a run near its cap or a cap that could not be set says what the number means.
fn memory_note(job: &crate::exec::jobs::Job) -> Option<String> {
    use crate::exec::MemoryCap;
    let used = job.memory_used();
    match (job.memory_cap(), used) {
        (MemoryCap::Uncapped { asked, why }, used) => Some(format!(
            "memory: ran UNCAPPED — {} was asked for and could not be set: {why}{}",
            crate::exec::human_bytes(*asked),
            used.map(|u| format!("; it peaked at {}", crate::exec::human_bytes(u.peak)))
                .unwrap_or_default()
        )),
        (_, Some(u)) if u.killed_at_cap() => Some(format!(
            "memory: {}; the kernel killed {} process(es) inside it at the cap",
            u.clause(),
            u.oom_kills
        )),
        (_, Some(u)) if u.near_its_cap() => Some(format!(
            "memory: {} — this command used nearly all of its cap; pass a larger `memory` if \
             it needs more.",
            u.clause()
        )),
        (_, Some(u)) => Some(format!("memory: {}.", u.clause())),
        _ => None,
    }
}

impl Bash {
    /// The cap this call runs under, or the result that says why it did not start. The rule is
    /// [`crate::exec::memory::decide`]'s; this is where an ask above the ceiling is put to the
    /// operator, before anything is spawned.
    fn memory_for(
        &self,
        ctx: &InvokeCtx<'_>,
        command: &str,
        args: &Value,
    ) -> Result<Option<u64>, Box<Invocation>> {
        let ask = Ask::parse(args.get("memory").and_then(|v| v.as_str())).map_err(|e| {
            Box::new(Invocation::failed(
                "bash's `memory` is not an amount",
                format!("nothing was run. {e}."),
            ))
        })?;
        // No `/proc/meminfo` (macOS): there is no memory controller to hold a cap either,
        // and the result says the run was uncapped through the job's own record.
        let Some(mem) = Meminfo::read() else {
            return Ok(None);
        };
        match crate::exec::memory::decide(&self.memory, ask, &mem) {
            Decision::Grant(b) => Ok(Some(b)),
            Decision::Operator { bytes, ceiling } => {
                let q = MemoryAsk {
                    command: command.to_string(),
                    bytes,
                    ceiling,
                    available: mem.available,
                    total: mem.total,
                };
                match ctx.grant_memory(&q) {
                    MemoryGrant::Granted => Ok(bytes),
                    MemoryGrant::Refused(why) => Err(Box::new(Invocation::not_run(
                        "the operator refused the memory this call asked for",
                        format!(
                            "nothing was run. You asked: {}. The operator said no: {why}. \
                             Ask for {} or less and it starts at once, or make the work smaller.",
                            q.summary(),
                            crate::exec::human_bytes(ceiling)
                        ),
                    ))),
                    MemoryGrant::NotAsked => Err(Box::new(Invocation::not_run(
                        "nobody could be asked for the memory this call needs",
                        format!(
                            "nothing was run. {} — and this session has no one to put that \
                             question to. Ask for {} or less and it starts at once.",
                            q.summary(),
                            crate::exec::human_bytes(ceiling)
                        ),
                    ))),
                }
            }
        }
    }
}

/// What a foreground command killed at its deadline says: one sentence, one way out. Raising the
/// timeout only makes the wait longer, so `background: true` is the only answer offered.
fn deadline_killed(body: &str, label: &str, secs: f32) -> String {
    let deadline = if secs < 1.0 {
        format!("{:.0}ms", secs * 1000.0)
    } else {
        format!("{secs:.0}s")
    };
    format!(
        "{body}\n\n[`{label}`] Killed at its {deadline} deadline. Re-run it with `background: true`."
    )
}

/// What deadline this call gets: the one place the choice is made, and pure on purpose, because a
/// test that waited 120 s to watch a kill would not be a test. It exists for the model's foreground
/// run and no other: `background: true` asked for none, and the operator's own `!` line has no clock.
fn deadline_for(asked_ms: Option<u64>, background: bool, operator: bool) -> Option<Duration> {
    if background || operator {
        return None;
    }
    Some(Duration::from_millis(
        asked_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(1, MAX_TIMEOUT_MS),
    ))
}

/// The name the caller gave this run, or `None` when nobody said one: `j57` is a counter, and a
/// person watching the transcript cannot tell which job is the release build. It is the agent's own
/// stated intent, never a parse of the command line, and the rule is [`crate::exec::job_name`].
fn slug_arg(args: &Value) -> Option<String> {
    let raw = args.get("slug").and_then(|v| v.as_str())?;
    crate::exec::job_name(raw)
}

/// `wake: "now" | "later"`, the optional argument on `bash` (background) and `task`.
/// Absent is the default: the daemon delivers by its own rules and a caller's `wait` holds on the
/// job. `now` is tell me the moment it ends, `later` is the model parking it (a server left running).
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
pub(crate) fn wake_arg(args: &Value) -> Result<Option<WantedNow>, Invocation> {
    match args.get("wake") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(w)) if w == "later" => Ok(Some(WantedNow::WakeLater)),
        Some(Value::String(w)) if w == "now" => Ok(Some(WantedNow::WakeNow)),
        Some(other) => Err(Invocation::failed(
            format!("`wake` is `now` or `later`, not {other}"),
            "`wake: \"now\"` delivers the completion the moment it ends, even into a running \
             turn; `wake: \"later\"` says you do not need waking for it, so the result is shown at \
             your next turn and a `wait` does not hold on it, which is what a server left running \
             wants; absent is the default. Nothing was started.",
        )),
    }
}

/// How long a foreground run may go before the daemon says it is the wrong shape: one minute, half of
/// [`DEFAULT_TIMEOUT_MS`], so the reminder arrives while the call is still runnable.
const SLOW_FOREGROUND: Duration = Duration::from_secs(60);

/// The reminder a long foreground run owes the seat that reads its result, or `None` when it owes
/// nothing. Pure like [`deadline_for`], for the same reason. The operator's own run is exempt: its
/// stop is their own act, and the seat reading a `!` line is a person, not a model.
fn slow_foreground(elapsed: Duration, operator: bool) -> Option<&'static str> {
    if operator || elapsed < SLOW_FOREGROUND {
        return None;
    }
    Some(concat!(
        "this command ran over a minute in the foreground. Raising `timeout_ms` ",
        "only makes the wait longer; a command you expect to take that long ",
        "belongs in the background — call `bash` again with `background: true`: ",
        "the job starts at once, its completion is delivered to you when it ends, ",
        "and nothing (not you, not the turn) waits for it.",
    ))
}

impl Tool for Bash {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "bash",
            "Run a shell command in the session's workspace and return its output. \
             Give `command`; optionally `cwd` (relative to the workspace), \
             `timeout_ms`, `slug` and `background: true` to start it and return \
             immediately. A foreground command is killed at `timeout_ms` (default \
             120 s); use `background: true` for anything longer. Every run is a job \
             with an id: output is capped inline and the rest is read with \
             `job_output`. A background job's completion is not something you wait \
             for: "
                .to_string()
                + super::DELIVERED
                + " **Give `slug` when the job is one a person \
             will want to recognise** (`release-build`, `fold-tests`): your own name \
             for the work, which is what the transcript calls this job and what the jobs \
             pane draws beside the id — and never a restatement of the command. Leave it \
             out when you have no name and none is invented for you. **So do not build \
             your own clock:** `sleep 200`, a `tail -f` of a log, a `until … ; do sleep \
             5; done` — every one of them is the same mistake in a different spelling, \
             and each spends the wait twice: it cannot end early when the work finishes \
             in twenty seconds, and it cannot be ended cleanly when it fails. A job is \
             stopped with `job_kill`. Every process lands in a cgroup owned by a scope, \
             so a foreground command dies with the turn and a background one with the \
             session unless you name a `scope`. Do not write `pkill`, `pgrep` or a \
             `while ... sleep` wait loop in a command: those match the process running \
             them, and `job_kill` and `job_wait` take a job id instead of a pattern. \
             When this session has a boundary, the command runs in a filesystem view \
             rooted at the workspace: a path outside that view is absent rather than \
             denied, and a result that says a path is not in the view is a fact about \
             the boundary, not about whether anything is there — asking again with a \
             different spelling will not reach it. Every command runs under a memory \
             cap and is killed alone if it reaches it; set `memory` for builds and tests \
             (e.g. \"4G\")."
                + &default_sentence(&self.memory),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The command line, run by /bin/sh in the workspace."},
                    "cwd": {"type": "string", "description": "Directory to run in, relative to the workspace root. Defaults to the root."},
                    "timeout_ms": {"type": "integer", "description": "How long this command may run before it is killed and the result says so. Defaults to 120000 (2 minutes). Ignored when background is true. A command you expect to take over a minute belongs in the background — `background: true` runs it with no deadline and its completion is delivered to you — rather than behind a raised timeout_ms."},
                    "background": {"type": "boolean", "description": "Start the command and return its job id at once instead of waiting."},
                    "wake": {"type": "string", "enum": ["now", "later"], "description": "With background: `later` says you do not need waking for it, so the result is shown at your next turn and a `wait` does not hold on it; use it for a server you leave running. `now`: the moment it ends, even mid-turn. Absent (the default): its end wakes you."},
                    "slug": {"type": "string", "description": "A short name for this job, in your own words — `release-build`, `fold-tests`. It is what the transcript calls this job (and what the jobs pane reads beside the job id), and what `job_output`, `job_kill` and `job_wait` accept in place of the id. It is your stated intent for the work and never a restatement of the command; omit it when you have no name, and no name is derived from the command line."},
                    "scope": {"type": "string", "description": "Which scope owns the process: `turn` (dies at the end of this turn), `session` (dies with the session), or `explicit` (survives the session; requires `scope_name`)."},
                    "scope_name": {"type": "string", "description": "Names an `explicit` scope so it can be listed and ended later."},
                    "memory": {"type": "string", "description": memory_arg(&self.memory)}
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

        // A one-shot run has nobody to hand a background job to: `letibot "a question"` starts a
        // daemon of its own and exits with it, so the job would outlive the only thing that could
        // deliver its result. Refused rather than run in the foreground, and the refusal says so.
        if ctx.one_shot
            && args
                .get("background")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        {
            return Invocation::not_run(
                "this run is one-shot, so `background: true` is refused",
                "A one-shot run (`letibot \"…\"`) prints its answer and exits, and its daemon \
                 exits with it — so a job left in the background would be work nobody ever \
                 reads the result of, still running after the answer was printed. Nothing was \
                 started.\n\n\
                 Run it in the FOREGROUND instead: raise `timeout_ms` (the deadline is the \
                 kill point, and the default is two minutes), which is what a one-shot can \
                 wait for because there is nothing else for it to do. A background run is for \
                 a session that stays up — a head attached to a daemon, or a subagent of one.",
            );
        }

        // A program that wants to own the terminal is refused before it is started, and this is the
        // first thing decided, because it is a fact about the command text alone. Only where a
        // terminal is handed out, which is the operator's own run: a model's `bash` keeps its pipe.
        if ctx.tty
            && let Some(r) = terminal::wants_the_terminal(command)
        {
            return Invocation::not_run(r.why(), r.body());
        }

        // The workspace, not the backend's root: `root_path()` is `/` for an unconfined session,
        // which is the right answer to is-this-inside-the-boundary and the wrong one for where-is-
        // the-tree-I-am-watching, so seeded from `/` a shell that rewrites a file hands no diff.
        let root = ctx
            .backend
            .workspace_path()
            .or_else(|| ctx.backend.root_path())
            .map(std::path::PathBuf::from);
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

        // The memory cap, decided before anything starts. The operator's own run has none: they
        // are watching it, and their own acts are its stop.
        let memory = if ctx.tty {
            None
        } else {
            match self.memory_for(ctx, command, args) {
                Ok(m) => m,
                Err(inv) => return *inv,
            }
        };

        // Before anything is started, because the diagnosis is only useful if it arrives instead
        // of the damage.
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
        // The caller's own name for this work, or nothing. Read here rather than derived from
        // `command` anywhere: see [`slug_arg`].
        let slug = slug_arg(args);
        // When this job's completion reaches the model. Read before anything starts, so a value
        // that is neither word refuses with nothing run.
        let wake = match wake_arg(args) {
            Ok(w) => w,
            Err(refused) => return refused,
        };
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

        // A foreground command is temporary in the turn; a background one is not, and giving it
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

        // The foreground deadline IS the kill point: a command that outlives `timeout_ms` is
        // killed, not left running. [`deadline_for`] is the one place the choice is made, and the
        // operator flag it takes is the same `ctx.tty` the pty and the shell key on.
        let timeout = deadline_for(
            args.get("timeout_ms").and_then(|v| v.as_u64()),
            background,
            ctx.tty,
        );

        let cwd = match ctx.backend.workdir(cwd) {
            Ok(c) => c,
            Err(e) => {
                return Invocation::failed(format!("cwd `{cwd}`: {e}"), String::new());
            }
        };
        // The world their console gives this run, and only this run. The pairs go on the REQUEST,
        // because `confine` reads `req.env` to decide what to put back inside a confined view: added
        // later they would reach an unconfined child and be dropped for a confined one.
        let mut env: Vec<(String, String)> = Vec::new();
        if ctx.tty {
            env.extend(crate::exec::console::env());
        }
        // So a helper the command runs can say what it is running for: `letibot-askpass` puts it
        // on the password card.
        env.push(("LETIBOT_COMMAND".to_string(), command.to_string()));
        let req = SpawnRequest {
            command: command.to_string(),
            // The name the caller stated, if it stated one. It rides on the job whether or not this
            // call asked for the background: a foreground run the runtime promotes is the same
            // work, and a name that appeared only after the promotion would be a name nobody gave.
            slug,
            cwd,
            scope,
            scope_name,
            background,
            env,
            // The operator's own run meets their console; a model's does not. `ls` colours only
            // under a tty, so an operator run gets a pty and their rc file, and the model's call
            // keeps its pipe: escape codes around every name are a cost it cannot see.
            tty: ctx.tty,
            memory,
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

        // The deadline is armed on the HOST, and the host is what fires it: that is what stops the
        // timeout being enforced inside the thing it guards. The loop below still checks the clock,
        // because it can say how far past the deadline the run went.
        if let Some(timeout) = timeout {
            host.arm_deadline(&id, timeout);
        }

        // What this job is called, in the one spelling the tree uses: the stated name beside the id
        // the daemon minted, or the bare id when nobody named it. Read off the VIEW rather than the
        // request, because the host makes a name unique within the session.
        let view = host.job(&id);
        let slug = view
            .as_ref()
            .and_then(|v| v.slug.clone())
            .or_else(|| req.slug.clone());
        let label = crate::exec::job_label(slug.as_deref(), &id.0);

        if background {
            // Where this job's own output goes changes what the sentences below may SAY, and is
            // not a footnote: a note that argues with the line beneath it is worse than none,
            // because a model believes the line it has seen before.
            let redirected = crate::builtins::output_redirect_path(command);
            // Where the whole of it is, said at the one moment the caller learns the handle: a reader
            // told only about the 8 MiB window loses a long build's first half. Not when the command
            // redirects its own stdout, since that job's window and file are empty by construction.
            let kept = match (
                &redirected,
                view.as_ref().and_then(|v| v.output_file.clone()),
            ) {
                (None, Some(f)) => format!(
                    "  output file: {} (from byte {}) — every byte it prints goes there too, \
                     and that file is not bounded by the window's 8 MiB\n",
                    f.path, f.from
                ),
                _ => String::new(),
            };
            // `Backgrounded`, not `Ok`, even though the model asked. The outcome names a fact about
            // the world that does not depend on who wanted it, and `Ok` here would make a result
            // grounding for an answer nothing has produced.
            let read_it = match &redirected {
                Some(path) => format!(
                    "Its own window will be EMPTY however long it runs — the command sends \
                     stdout to `{path}` — so read `{path}` with `read` when the completion \
                     tells you it ended, and do not read the window looking for progress."
                ),
                None => format!(
                    "`job_output` with job=\"{id}\" reads what it has written so far; \
                     `job_kill` stops it."
                ),
            };
            // The handles a caller may pass: the caller's own word for the work, spelled back with
            // the id so that a string a person can read resolves to this job. Absent when nobody
            // said one, and never a derived one.
            let named = match &slug {
                Some(s) => format!(
                    "  name: {s} — `job_output`, `job_kill` and `job_wait` take this or `{id}`\n"
                ),
                None => String::new(),
            };
            // `wake: "now"` is said to the daemon here, where the handle first exists.
            if let Some(w) = wake {
                ctx.want_completion(&id.0, w);
            }
            // One delivery sentence, the handle, and the way to read it ([`super::DELIVERED`]);
            // the envelope adds "STILL RUNNING".
            let mut inv = Invocation::backgrounded(
                id.0.clone(),
                Duration::ZERO,
                Backgrounding::Asked,
                format!(
                    "{read_it}{}",
                    crate::builtins::todo::attach_now(ctx, "job", &id.0)
                ),
                format!(
                    "started `{label}` in the background (pid {}).\n{named}{kept}  scope: {} — {}\n\n{} \
                     So do not build your own clock: no `sleep`, no polling loop.",
                    view.as_ref().map(|v| v.pid).unwrap_or(0),
                    scope.as_str(),
                    scope.reaped_when(),
                    super::DELIVERED,
                ),
            );
            for n in notes {
                inv = inv.with_note(n);
            }
            return inv;
        }

        // Foreground. Progress reports WORK DONE (bytes produced) and never "still alive": a liveness
        // indicator that moves when nothing has happened measures the wrong thing. The operator's run
        // is handed to the daemon before it is waited on, which is what makes `!send` work.
        let answerable = (ctx.tty && ctx.operator_runs_wired())
            .then(|| host.job_handle(&id).map(|j| j.stdin()))
            .flatten()
            .filter(|s| s.is_open());
        if let Some(stdin) = &answerable {
            ctx.operator_run(OperatorRun::Answerable {
                job: &id.0,
                command,
                stdin: stdin.clone(),
            });
        }
        let waited_from = std::time::Instant::now();
        let foreground = wait_with_progress(ctx, host, &id, timeout);
        // The run is over, so whatever card was up for it comes down: after the wait and before every
        // branch below, because a state, a promotion and a deadline all mean this tool is no longer
        // waiting, and a card that outlived its command would be a person answering nothing.
        if answerable.is_some() {
            ctx.operator_run(OperatorRun::Ended { job: &id.0 });
        }

        let Ok(out) = host.output(&id, 0, usize::MAX) else {
            return Invocation::failed(
                format!("`{id}` started but its output could not be read"),
                "this is a harness defect, not a command failure. The process is in \
                 its scope and will be reaped with it.",
            );
        };
        let full = out.text();
        let (body, capped) = tail_lines(&full, MAX_INLINE_LINES, MAX_INLINE_BYTES);

        // Absence must be legible: a path the mount namespace does not contain gives a bare `ENOENT`,
        // which a model reads as the file does not exist. Scanned over the WHOLE capture, because a
        // path in the dropped head is the one whose diagnosis got lost, and nothing here stats.
        for n in host.absence_notes(&full) {
            notes.push(n);
        }
        let outside = host.outside_paths(&full);
        let launcher_failed = host.launcher_failure(&full);

        // A person moved it mid-flight (Ctrl+B). The outcome is `Backgrounded` with the operator
        // named, not a deadline kill: the command is still running, and the handle is the way back.
        if let Foreground::Promoted(p) = foreground {
            // The call was blocked on this command, so its result is one the model is waiting
            // for: delivered the moment it ends, never held for a break.
            ctx.want_completion(&id.0, WantedNow::Awaited);
            // A promotion is a backgrounding, so the redirect rule is this one's too: the same
            // sentence must not tell its reader to open a window that will stay empty. And this is
            // where the file is said, because it is the only moment the caller learns it.
            let kept = match &p.output_file {
                Some(f) => format!(
                    " Every byte it printed before the move, and every byte it prints from here \
                     on, is also going into `{}` — that file is not bounded by the window's 8 MiB, \
                     so the output of a long run is not lost past it.",
                    f.path
                ),
                None => String::new(),
            };
            let next = match crate::builtins::output_redirect_path(command) {
                Some(path) => format!(
                    "carry on — `{label}`'s completion will be delivered to you on its own when it ends, so there is nothing to wait for. Its own window will be EMPTY: the command sends stdout to `{path}` — read that file with `read` when the completion tells you it ended, and do not build your own clock in the meantime.{kept}"
                ),
                None => format!(
                    "carry on — `{label}`'s completion will be delivered to you on its own when it ends, so there is nothing to wait for. `job_output` with job=\"{id}\" reads what it has written so far.{kept}"
                ),
            } + &crate::builtins::todo::attach_now(ctx, "job", &id.0);
            let file_line = match &p.output_file {
                Some(f) => format!(
                    "  output file: {} (from byte {} of its output)\n",
                    f.path, f.from
                ),
                None => String::new(),
            };
            let mut inv = Invocation::backgrounded(
                id.0.clone(),
                p.ran_for,
                p.how.clone(),
                &next,
                format!(
                    "{body}\n\n[`{label}` was moved to the background — you did not ask \
                     for it]\n  command: {command}\n{file_line}  owned by: {} — {}\n\nIts \
                     completion will reach you on its own the moment it ends — this call was \
                     waiting on it, so it is not held for a break; do not wait on it.\n",
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
                    out.denominator(&id, &label)
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
        // What the kernel counted for this job's memory, read at its settle.
        let job = host.job_handle(&id);
        let used = job.as_ref().and_then(|j| j.memory_used());
        let killed_at_cap = used.is_some_and(|u| u.killed_at_cap());
        let mut inv = match &state {
            // The deadline: the command outlived `timeout_ms`, so it is killed and not promoted.
            // The second way it arrives here is the host's watchdog having already ended the run,
            // settled as `Killed` with the deadline's own reason; both render the same sentence.
            JobState::Killed { by } if by == crate::exec::DEADLINE_KILL => {
                // `Some` by construction: the watchdog settles `DEADLINE_KILL` only for a deadline
                // it was armed with, and nothing arms one when [`deadline_for`] said `None`.
                let reaped = host.kill_job(&id);
                let mut inv = Invocation::timed_out(deadline_killed(
                    &body,
                    &label,
                    timeout.map(|t| t.as_secs_f32()).unwrap_or(0.0),
                ));
                if let Err(e) = &reaped {
                    inv = inv.with_note(format!("the kill did not complete: {e}"));
                }
                inv
            }
            JobState::Running => match timeout {
                Some(timeout) => {
                    let reaped = host.kill_job(&id);
                    let mut inv = Invocation::timed_out(deadline_killed(
                        &body,
                        &label,
                        timeout.as_secs_f32(),
                    ));
                    if let Err(e) = &reaped {
                        inv = inv.with_note(format!("the kill did not complete: {e}"));
                    }
                    inv
                }
                // No deadline was armed, so this is not a timeout and the run is not stopped: a
                // `Running` with no deadline is a fact about this daemon, and killing here would
                // end a person's run by no clock.
                None => Invocation::failed(
                    format!("`{label}` is still running"),
                    format!(
                        "{body}\n\n[the wait for `{label}` ended while it was still running \
                         — no deadline applies to it, so it was NOT stopped]\n  \
                         command: {command}\n\nIt is the operator's own run: their console has its \
                         output, their own acts are its stop. `job_output` with \
                         job=\"{id}\" reads what it has written so far; `job_kill` ends it.",
                    ),
                ),
            },
            JobState::NotScoped => Invocation::failed(
                format!("`{label}` could not join its scope, so the command was NOT run"),
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
                    "`{label}`'s boundary did not come up, so nothing can be concluded from its exit"
                ),
                format!(
                    "{body}\n\n{}\n\nThis is `not_run` in substance: the confinement is \
                     the thing that failed, and running the command without it is not \
                     an available outcome.",
                    launcher_failed.clone().unwrap_or_default()
                ),
            ),
            // Killed at its memory cap, checked before the exit code is read as the command's answer,
            // because `[exit 137]` is not one. A run that exited 0 with a process killed inside it
            // keeps its own answer, and the note below says what was killed.
            s if killed_at_cap && !matches!(s, JobState::Exited { code: 0 }) => {
                let u = used.expect("killed_at_cap reads it");
                Invocation::failed(
                    format!("`{label}` was killed at its memory cap"),
                    format!(
                        "{body}\n\n[the kernel killed `{label}` when it reached its memory cap \
                         — {}]\n  command: {command}\n\nIt did not crash on its own; it ran out \
                         of the memory this call was given: it {}. Or make the work smaller.",
                        u.clause(),
                        match u.cap {
                            Some(cap) => crate::exec::memory::ask_after_kill(
                                &self.memory,
                                cap,
                                Meminfo::read().as_ref()
                            ),
                            None => "ran uncapped, so the kill was not a cap's".into(),
                        }
                    ),
                )
            }
            JobState::Exited { code: 0 } => Invocation::ok(body),
            // A non-zero exit is the COMMAND's answer, not a harness failure, and it
            // is `ok` with the code stated. Reporting it as `failed` would make a
            // `grep` that found nothing indistinguishable from a broken tool.
            JobState::Exited { code } => Invocation::ok(format!(
                "{body}\n\n[exit {code}] the command ran and exited non-zero. That is \
                 the command's answer, not a harness failure."
            )),
            other => Invocation::failed(
                format!("`{label}` {}", other.word()),
                format!("{body}\n\nThe process did not exit on its own."),
            ),
        };

        // The numbers behind the clause, for the tool call's metrics row.
        if let Some(u) = job.as_deref().and_then(|j| j.memory_used()) {
            crate::exec::scope::note_memory(u);
        }
        if !ctx.tty
            && let Some(note) = job.as_deref().and_then(memory_note)
        {
            inv = inv.with_note(note);
        }

        // A run that spent over a minute in the foreground says so on its way out, appended to the
        // result the model reads. Skipped for the two deadline arms, whose payloads already say
        // "re-run it in the background", and for a promotion, whose body says it was moved.
        let deadline_answered = matches!(
            &state,
            JobState::Killed { by, .. } if by == crate::exec::DEADLINE_KILL
        ) || (matches!(&state, JobState::Running) && timeout.is_some());
        if let Some(note) = slow_foreground(waited_from.elapsed(), ctx.tty)
            && !deadline_answered
        {
            inv = inv.with_note(note);
        }

        // And what it changed. Attached as an edit when it is one file, which is the shape a
        // script-that-edits has, and named otherwise, because one result carries one card. Either way
        // the operator is told: a change nobody can see is a change nobody reviewed.
        if let (Some(root), Some(base)) = (root.as_deref(), before.as_ref()) {
            // Names first, and only names: one `git status` and a stat each. The diff below is
            // computed for the one file it is drawn for, never for every file that moved.
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
            if base.skipped > 0
                && !self
                    .snapshot_budget_said
                    .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
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
                out.denominator(&id, &label)
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
#[allow(clippy::large_enum_variant)] // one Promotion per move; boxing it buys nothing
enum Foreground {
    State(JobState),
    /// A person moved the command to the background from a head. The promotion is
    /// the record; the job is still running and recoverable.
    Promoted(Promotion),
}

/// What has already been said about this run, so one question is one card and one inability is one
/// sentence. A blocked run stays blocked and an unreadable one stays unreadable, so a loop that
/// reported either every tick would put a hundred identical lines on the screen for one fact.
#[derive(Default)]
struct Told {
    /// What the last card was about. Keyed on the pair (bytes produced, the line shown) and not on
    /// the line alone: a program that asks the same question twice after saying something in
    /// between has asked twice, and the second ask is a second thing to answer.
    raised: Option<(u64, Option<String>)>,
    /// The reading the beat before took. The card is raised only from one that has stood still
    /// since then: the capture trails the pipe, so the beat a block is first seen on can hold
    /// nothing of what the program already wrote, or half a line of it.
    seen: Option<(u64, Option<String>)>,
    /// Whether the daemon has already said this run is slow. Once per run: the
    /// condition does not un-cross, and a line every 500 ms for one fact is the
    /// liveness signal this loop refuses to be.
    slow: bool,
    /// Whether the daemon has already said it cannot tell whether this run is waiting. Once
    /// per run: it is a disclosure about a condition, and the condition does not change while
    /// the run lasts.
    unreadable: bool,
}

impl Told {
    /// Whether a card may be raised from this beat's reading: only from one that is the beat
    /// before's, because the capture trails the pipe and nothing revises a card once it is up,
    /// and never from the one the last card was about.
    fn speaks_now(&mut self, reading: &(u64, Option<String>)) -> bool {
        let stood_still = self.seen.as_ref() == Some(reading);
        self.seen = Some(reading.clone());
        if !stood_still || self.raised.as_ref() == Some(reading) {
            return false;
        }
        self.raised = Some(reading.clone());
        true
    }
}

/// Wait, emitting progress that measures work rather than a heartbeat, and honour a head's Ctrl+B by
/// promoting the command mid-flight. `timeout` of `None` is no deadline, so the loop ends only on the
/// run's own state; it also asks, once a beat, whether the run is waiting for an answer.
fn wait_with_progress(
    ctx: &mut InvokeCtx<'_>,
    host: &dyn ProcessHost,
    id: &crate::exec::JobId,
    timeout: Option<Duration>,
) -> Foreground {
    let started = std::time::Instant::now();
    let step = Duration::from_millis(500);
    let mut last_reported = 0u64;
    let mut told = Told::default();
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
        let left = timeout.map(|t| t.saturating_sub(started.elapsed()));
        if left == Some(Duration::ZERO) {
            break;
        }
        let tick = left.map_or(step, |l| step.min(l));
        match host.wait_job(id, tick) {
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
            ask_if_waiting(ctx, host, id, &v, &mut told);
        }
        // Past a minute in the foreground, say so once, in this loop's own register: the decision is
        // [`slow_foreground`]'s and the boundary is [`SLOW_FOREGROUND`]'s. This is the operator's
        // live line; the model's copy is appended to the call's result by [`Bash::invoke`].
        if !told.slow && slow_foreground(started.elapsed(), ctx.tty).is_some() {
            told.slow = true;
            ctx.progress(format!(
                "`{id}`: {:.0}s in the foreground and still running — this is what \
                 `background: true` is for (Ctrl+O moves it there now)",
                started.elapsed().as_secs_f32()
            ));
        }
    }
    Foreground::State(host.job(id).map(|v| v.state).unwrap_or(JobState::Running))
}

/// How long a run must have written nothing before it is read as waiting. The poll above is half a
/// second, so this is one beat: too short and a program between two lines of a slow build looks like
/// a question, too long and a person waits for a card the program has blocked on for a second.
const QUIET: Duration = Duration::from_millis(250);

/// Is this run waiting for an answer, and if so, tell the daemon.
/// Three conditions: alive, quiet for a beat (the card's condition, and only the card's), and a
/// process of the run blocked reading the stdin this daemon holds. No byte of the output is read.
fn ask_if_waiting(
    ctx: &mut InvokeCtx<'_>,
    host: &dyn ProcessHost,
    id: &crate::exec::JobId,
    v: &crate::exec::JobView,
    told: &mut Told,
) {
    // A runtime nobody wired has no card to raise, and the read below is a whole ring of
    // output: skipped rather than paid for.
    if !ctx.operator_runs_wired() || !v.state.is_running() {
        return;
    }
    let quiet = match v.since_last_output {
        Some(d) => d >= QUIET,
        None => v.elapsed >= QUIET,
    };
    // The device is OURS, and that comparison is what keeps a `grep` blocked on `ls`'s pipe in
    // `! ls | grep foo` from looking like a program waiting for a line: the run's fd 0 is its own
    // terminal, and [`Stdin::input_end`] is the name the kernel gives both sides.
    let handle = host.job_handle(id);
    let ours = handle.as_ref().and_then(|j| j.stdin().input_end());
    let pids = host.job_pids(id);
    let verdict = crate::exec::ask::waiting_for_an_answer(&pids, ours.as_ref());
    // And nothing has been typed at it for a beat: a program just answered is blocked reading its
    // terminal again within microseconds, so without this the card would be raised about a question
    // the person answered a moment ago. `None` is never written to, the quietest case there is.
    const ANSWERED: Duration = Duration::from_millis(1500);
    let just_answered = handle
        .as_ref()
        .and_then(|j| j.stdin().since_last_input())
        .is_some_and(|d| d < ANSWERED);
    match verdict {
        // The signal was read, and it says yes, and the card still needs both beats: a card claims
        // some process of this run is blocked reading the answer we hold, and a run still drawing is
        // genuinely not blocked. A program between two lines of a slow build is not asking anything.
        crate::exec::ask::Waiting::Yes if quiet && !just_answered => {}
        // The signal was read and it says yes, but the run is still writing or was just
        // answered: not a card.
        crate::exec::ask::Waiting::Yes => return,
        // The signal was read, and it says no: every process of the run was looked at and none is
        // reading our pipe. Nothing to say.
        crate::exec::ask::Waiting::No => return,
        // The signal could not be read, which is not the card's kind of fact: it is the absence of a
        // reading, a fact about permissions rather than about the clock, and exactly as true at a
        // 100 ms write interval. One sentence, once per run, naming which of the two facts it is.
        crate::exec::ask::Waiting::Unreadable => {
            if !told.unreadable {
                told.unreadable = true;
                ctx.operator_run(OperatorRun::Unreadable { job: &id.0, quiet });
            }
            return;
        }
    }
    // The last line, and only to show. Bounded by the ring: the question is at the end of what a
    // program wrote, so a program that wrote a megabyte before asking has its answer in the last few
    // kilobytes.
    const SHOW_BYTES: u64 = 4096;
    let shown = host
        .output(
            id,
            v.produced.saturating_sub(SHOW_BYTES),
            SHOW_BYTES as usize,
        )
        .ok()
        .and_then(|s| crate::exec::ask::last_line(&s.text()));
    let reading = (v.produced, shown);
    if !told.speaks_now(&reading) {
        return;
    }
    ctx.operator_run(OperatorRun::Waiting {
        job: &id.0,
        question: reading.1.as_deref(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The capture trails the pipe, so the beat a block is first seen on can hold nothing of what
    /// the program already wrote - and a card is never revised once it is up. The first reading
    /// warms the decision and the second makes it; a run that wrote nothing still gets its card.
    #[test]
    fn a_card_is_raised_from_a_reading_that_stood_still_for_a_beat() {
        let line = (16u64, Some("Continue? [Y/n]".to_string()));
        let mut told = Told::default();
        assert!(!told.speaks_now(&line), "no card from the first beat alone");
        assert!(
            told.speaks_now(&line),
            "the same reading on the next beat does"
        );
        assert!(
            !told.speaks_now(&line),
            "and the reading a card was raised from is not raised twice"
        );

        let empty = (0u64, None);
        let mut silent = Told::default();
        assert!(!silent.speaks_now(&empty));
        assert!(
            silent.speaks_now(&empty),
            "a program that wrote nothing is a card too, once the beat has shown it is not a lag"
        );
    }

    /// A name is never invented from the command, which is the whole point of the parameter: a
    /// guessed slug is worse than none, since the command is already on the row. A test where
    /// the command is the shape a deriver would reach for must answer no name at all.
    #[test]
    fn no_slug_is_ever_derived_from_the_command() {
        let deriver_bait = serde_json::json!({
            "command": "W=/tmp/build; cd /tmp && cargo test --workspace --release",
            "background": true,
        });
        assert_eq!(slug_arg(&deriver_bait), None);

        // Every shape of *nobody said one*: the key absent, null, an empty string, and
        // whitespace. All four are the same answer, because all four are the same claim.
        for args in [
            serde_json::json!({"command": "cargo test"}),
            serde_json::json!({"command": "cargo test", "slug": null}),
            serde_json::json!({"command": "cargo test", "slug": ""}),
            serde_json::json!({"command": "cargo test", "slug": "   \t "}),
        ] {
            assert_eq!(slug_arg(&args), None, "{args}");
        }
    }

    /// A stated name is the caller's own words, spelled as a name: one stated name has to be one
    /// row in `/job` and one cell in the pane, so runs of whitespace become `-` and the length is
    /// capped. Nothing else moves: no case change, no fallback word.
    #[test]
    fn a_stated_name_is_flattened_into_a_name_and_kept_otherwise() {
        let args = |v: &str| serde_json::json!({"command": "cargo test", "slug": v});
        assert_eq!(
            slug_arg(&args("release-build")),
            Some("release-build".to_string())
        );
        assert_eq!(
            slug_arg(&args("  Release Build  ")),
            Some("Release-Build".to_string()),
            "whitespace is not a name's business, and the caller's case is"
        );
        assert_eq!(
            slug_arg(&args("fold\ntests")),
            Some("fold-tests".to_string()),
            "a name that would draw as two rows"
        );
        assert_eq!(
            slug_arg(&args("a\u{7}b")),
            Some("a-b".to_string()),
            "a control character is a break, not a letter: it cannot be drawn and it is not \
             part of a name"
        );
        let long = "x".repeat(200);
        let capped = slug_arg(&args(&long)).unwrap();
        assert_eq!(capped.chars().count(), crate::exec::MAX_JOB_NAME);
        assert!(capped.chars().all(|c| c == 'x'));
        // And the cap does not leave a dangling separator behind.
        let words = format!("{} tail", "word ".repeat(20));
        let capped = slug_arg(&args(&words)).unwrap();
        assert!(!capped.ends_with('-'), "{capped:?}");
        assert!(capped.chars().count() <= crate::exec::MAX_JOB_NAME);
    }

    /// The rule is [`crate::exec::job_name`]'s and this argument is a reader of it. The rule lives
    /// outside this file so the daemon's own names for the work it starts could be the same rule
    /// rather than a second spelling of one: a name has to read the same in the pane and `/job`.
    #[test]
    fn the_slug_argument_reads_the_shared_rule_and_does_not_re_spell_it() {
        for raw in [
            "release-build",
            "  Release Build  ",
            "fold\ntests",
            "a\u{7}b",
            "",
            "   \t ",
            &"x".repeat(200),
            &format!("{} tail", "word ".repeat(20)),
        ] {
            assert_eq!(
                slug_arg(&serde_json::json!({"command": "cargo test", "slug": raw})),
                crate::exec::job_name(raw),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn the_tail_is_what_survives_the_cap() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let (kept, capped) = tail_lines(&text, 10, 100_000);
        assert!(capped);
        assert!(kept.starts_with("line 90"), "{kept}");
        assert!(kept.ends_with("line 99"), "{kept}");
    }

    #[test]
    fn a_cut_lands_on_a_line_boundary() {
        let text = "aaaaaaaaaa\nbbbbbbbbbb\ncccccccccc\n";
        let (kept, capped) = tail_lines(text, 100, 15);
        assert!(capped);
        // No partial line: whatever survived is whole.
        assert!(text.lines().any(|l| l == kept.lines().next().unwrap()));
    }

    #[test]
    fn a_models_run_gets_the_default_and_what_it_asked_for() {
        assert_eq!(
            deadline_for(None, false, false),
            Some(Duration::from_millis(DEFAULT_TIMEOUT_MS))
        );
        assert_eq!(
            deadline_for(Some(30_000), false, false),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn an_asked_deadline_is_clamped_to_one_ms_and_the_ceiling() {
        assert_eq!(
            deadline_for(Some(0), false, false),
            Some(Duration::from_millis(1))
        );
        assert_eq!(
            deadline_for(Some(u64::MAX), false, false),
            Some(Duration::from_millis(600_000))
        );
    }

    /// `wake` is `now`, `later` or absent, and anything else is refused: a misspelt `now` that
    /// silently meant the default would be a result the model waits for and never gets.
    #[test]
    fn wake_is_now_or_later_and_the_result_says_which() {
        assert_eq!(wake_arg(&serde_json::json!({})).ok(), Some(None));
        assert_eq!(
            wake_arg(&serde_json::json!({"wake": "later"})).ok(),
            Some(Some(WantedNow::WakeLater)),
            "`later` is the model parking the job, not the default"
        );
        assert_eq!(
            wake_arg(&serde_json::json!({"wake": "now"})).ok(),
            Some(Some(WantedNow::WakeNow))
        );
        for bad in [serde_json::json!("soon"), serde_json::json!(true)] {
            assert!(
                wake_arg(&serde_json::json!({ "wake": bad })).is_err(),
                "{bad} was not refused"
            );
        }
    }

    #[test]
    fn a_deadline_kill_names_one_way_out() {
        // The kill says background, and never "a larger timeout_ms", which the slow-run note on
        // the same tool calls a longer wait.
        let text = deadline_killed("out", "j3", 120.0);
        assert!(
            text.ends_with(
                "[`j3`] Killed at its 120s deadline. Re-run it with `background: true`."
            )
        );
        assert!(deadline_killed("out", "j3", 0.3).contains("its 300ms deadline"));
        assert!(!text.contains("timeout_ms"), "{text}");
    }

    #[test]
    fn a_background_run_has_no_deadline() {
        assert_eq!(deadline_for(None, true, false), None);
    }

    #[test]
    fn the_operators_own_run_has_no_deadline_whatever_the_call_says() {
        // The measured case: the daemon mints the operator's call with `command` and nothing
        // else, so `asked_ms` is `None`, and the run died at its silent default anyway.
        assert_eq!(deadline_for(None, false, true), None);
        // A `timeout_ms` no head can even pass today is still not a deadline: the person's run is
        // the person's.
        assert_eq!(deadline_for(Some(5_000), false, true), None);
    }

    /// The boundary is one minute, and both sides of it are pinned without waiting a real minute:
    /// the decision is pure, so a nanosecond either side of [`SLOW_FOREGROUND`] stands in for it.
    #[test]
    fn a_foreground_run_owes_the_reminder_past_one_minute_and_not_before() {
        assert_eq!(slow_foreground(Duration::from_secs(59), false), None);
        assert_eq!(
            slow_foreground(SLOW_FOREGROUND - Duration::from_millis(1), false),
            None,
            "the reminder fires MORE than a minute in, not at the minute"
        );
        let note = slow_foreground(SLOW_FOREGROUND, false)
            .expect("a run that reaches the minute owes its reader the reminder");
        // The advice names the tool's own feature and the escape hatch it replaces: a model that
        // raised `timeout_ms` has to be told why that was the wrong knob, not merely that another
        // exists.
        assert!(note.contains("background: true"), "{note}");
        assert!(note.contains("timeout_ms"), "{note}");
        assert!(note.contains("completion is delivered"), "{note}");
    }

    /// The operator's own run is exempt: a person is watching it, and the seat that reads its
    /// result is that person. The reminder is for a model deciding how to spell its next call.
    #[test]
    fn the_operators_own_run_is_not_reminded_even_past_the_minute() {
        assert_eq!(slow_foreground(Duration::from_secs(600), true), None);
        // And the contrast: a model's run of the same age owes it.
        assert!(slow_foreground(Duration::from_secs(600), false).is_some());
    }

    /// The description states the default in force and which file set it: the project's
    /// `leticode.toml` when it spoke, the built-in otherwise.
    #[test]
    fn the_description_names_the_default_in_force_and_where_it_came_from() {
        use crate::exec::memory::{Amount, PolicySource};
        let built_in = Bash::default().schema();
        assert!(
            built_in
                .description
                .contains("runs under 64 MiB** (the built-in default)"),
            "{}",
            built_in.description
        );
        let project = Bash::with_memory(MemoryPolicy {
            default: Amount::Bytes(4 << 30),
            default_from: PolicySource::Project,
            ..MemoryPolicy::default()
        })
        .schema();
        assert!(
            project.description.contains(
                "runs under 4 GiB** (`[memory] default` in this project's leticode.toml)"
            ),
            "{}",
            project.description
        );
        let arg = project.parameters["properties"]["memory"]["description"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            arg.contains("runs under 4 GiB — `[memory] default` in this project's leticode.toml"),
            "{arg}"
        );
    }

    /// A job as the note sees it: a cap, and what the kernel counted for it once it settled.
    fn settled(
        cap: crate::exec::MemoryCap,
        used: Option<crate::exec::scope::MemoryUse>,
    ) -> crate::exec::jobs::Job {
        use crate::exec::jobs::{Job, JobId, Lifetime, Stdin};
        use crate::exec::scope::{ScopeId, ScopeKind};
        let scope = |name: &str| ScopeId {
            kind: ScopeKind::Turn,
            name: name.into(),
            path: std::path::PathBuf::from("/nonexistent"),
        };
        let job = Job::new(
            JobId::next(),
            "cargo build".into(),
            None,
            Lifetime {
                scope: scope("turn"),
                owner: scope("session"),
                background: None,
            },
            "/w".into(),
            0,
            4096,
            Stdin::none(),
        )
        .with_memory_cap(cap);
        if let Some(used) = used {
            job.record_memory(used);
        }
        job
    }

    /// The peak reaches the model on a call that ended nowhere near its cap: it chose the cap, and
    /// the next ask is a number only if the last one came back.
    #[test]
    fn a_finished_call_reports_its_peak() {
        use crate::exec::MemoryCap;
        use crate::exec::scope::MemoryUse;
        let job = settled(
            MemoryCap::Capped(64 << 20),
            Some(MemoryUse {
                cap: Some(64 << 20),
                peak: 12 << 20,
                oom_kills: 0,
            }),
        );
        assert_eq!(
            memory_note(&job).as_deref(),
            Some("memory: peak 12 MiB of a 64 MiB cap."),
            "an ordinary call says what it cost"
        );
    }

    /// A box with no user systemd runs the call uncapped, and the result says so - the ask that
    /// could not be honoured, the reason, and what the run peaked at - never a silent uncapped run.
    #[test]
    fn an_uncapped_call_names_the_ask_and_the_reason() {
        use crate::exec::MemoryCap;
        use crate::exec::scope::MemoryUse;
        let job = settled(
            MemoryCap::Uncapped {
                asked: 512 << 20,
                why: "no memory controller".into(),
            },
            Some(MemoryUse {
                cap: None,
                peak: 200 << 20,
                oom_kills: 0,
            }),
        );
        let note = memory_note(&job).expect("a cap that could not be set is said");
        assert!(note.contains("ran UNCAPPED"), "{note}");
        assert!(note.contains("512 MiB was asked for"), "{note}");
        assert!(note.contains("no memory controller"), "{note}");
        assert!(note.contains("it peaked at 200 MiB"), "{note}");
    }

    #[test]
    fn the_description_declares_exec_and_says_how_to_use_the_tool() {
        let s = Bash::default().schema();
        assert_eq!(s.access, Access::Exec);
        assert!(!s.access.is_unattended(), "exec must reach the gate");
        assert_eq!(crate::schema::lint_description(&s.description), vec![]);
    }
}
