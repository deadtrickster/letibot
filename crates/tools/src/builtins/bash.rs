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
//! removes it. **The operator's own `!` line has no deadline at all** — see
//! [`deadline_for`] for the measurement and the rule.
//!
//! And a foreground command that runs past [`SLOW_FOREGROUND`] — one minute — is
//! **told on**, in both directions: the wait loop's progress line while it runs
//! (the operator's), and a note on the result (the model's, in band, on the next
//! round) saying that a command this long belongs in the background rather than
//! behind a raised `timeout_ms`. The rule is measured, not worded: it was written
//! after a model asked for `timeout_ms: 600000` instead of `background: true`.
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
use crate::exec::terminal;
use crate::exec::{JobState, ProcessHost, Promotion, ScopeKind, SpawnRequest, Waited};
use crate::runtime::{Invocation, InvokeCtx, OperatorRun, Tool};
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
/// removes the deadline. The operator's own run has none to begin with — see
/// [`deadline_for`].
///
/// opencode's own default is 120000 ms, and leticode is its parity port: a command
/// that the model did not ask to run longer than the default must not run forever.
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// The longest a deadline may be asked for. Matches opencode's 600000 ms ceiling.
const MAX_TIMEOUT_MS: u64 = 600_000;

/// **What deadline this call gets — the one place the choice is made.**
///
/// Pure, and on purpose: which run may be killed by a clock is a fact about
/// inputs, not about a process, and a test that waited 120 s to watch a kill
/// would not be a test at all. The same discipline [`crate::exec::term::argv`]
/// keeps — the choice asserted without a process starting.
///
/// Three facts go in, one answer comes out:
///
/// * **the model's foreground run** — `Some`, from `timeout_ms` when it asked
///   and [`DEFAULT_TIMEOUT_MS`] when it did not, clamped to
///   [`MAX_TIMEOUT_MS`]. The deadline exists for this case and only this one:
///   a command the model did not ask to run longer must not run forever.
/// * **`background: true`** — `None`. The model asked for exactly no deadline.
/// * **the operator's own run** (`ctx.tty`) — `None`, whatever `timeout_ms`
///   says. The deadline's whole reason is a run nobody is watching; the
///   operator's `!` line is watched by the person who typed it, its output is
///   on their screen, and their own acts — Ctrl+C, `!term`, closing the head —
///   are its stop. Measured 2026-10-09: `! sudo apt install mc` sat at
///   `Continue? [Y/n]` while its person read the package list, and was killed
///   at 120 s with the install never run. A person spending a minute reading a
///   question is using the feature, not exceeding a deadline. `timeout_ms` in
///   an operator call would mean nothing today — the daemon mints the call as
///   `{"command": …}` and nothing else — and it is ignored on purpose: a
///   future head that grew a way to pass one would be re-arming the measured
///   bug by hand.
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

/// **How long a foreground run may go before the daemon says it is the wrong
/// shape** — one minute.
///
/// The operator's rule, in their words: *"so if tool took more than a minute we
/// should remind model to use background jobs"* — written after watching a model
/// raise `timeout_ms` to 600000 for a `cargo test` it could have backgrounded. The
/// number has three anchors rather than one taste: it is the operator's sentence;
/// it is half of [`DEFAULT_TIMEOUT_MS`], so under the default deadline the
/// reminder always arrives while the call is still runnable (a reminder that
/// could only land after the kill is a post-mortem, and the kill's own sentence
/// already teaches this); and it is 120 beats of the wait loop's 500 ms poll, so
/// no merely slow tick can fire it.
const SLOW_FOREGROUND: Duration = Duration::from_secs(60);

/// **The reminder a long foreground run owes the seat that reads its result**, or
/// `None` when it owes nothing.
///
/// Pure like [`deadline_for`], and for the same reason: the boundary is a fact
/// about inputs — how long the run has been going, and whose run it is — and a
/// test that waited a real minute to watch a sentence appear would not be a test
/// at all. The wiring is two lines that reuse registers the loop already has: the
/// progress line the wait loop emits (the operator's, live, once), and the note
/// [`Bash::invoke`] appends to the result (the model's, on the next round, in
/// band).
///
/// `operator` is the operator's own run ([`InvokeCtx::tty`]), and it is exempt:
/// that run is watched by the person who typed it, its stop is their own act, and
/// the seat that reads a `!` line's result is a person, not a model deciding how
/// to spell its next call.
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
             `timeout_ms`, and `background: true` to start it and return \
             immediately. A command you did NOT mark background is moved to the \
             background by the runtime once it has run longer than `timeout_ms`, and \
             the result says so, names its job id and says which call gets its \
             output — it is still running, it did not fail, and starting it again \
             would give you two. Every run is a job with an id: output is capped \
             inline and the rest is read with `job_output`, and a job's completion \
             **arrives on its own** when it ends — you are told, unprompted, so you \
             do not sit and wait on it. **That is the rule, and it is about clocks rather \
             than about verbs: you are woken when the job finishes, so do not build your \
             own clock.** `sleep 200`, a `tail -f` of a log, a `until … ; do sleep 5; \
             done` — every one of them is the same mistake in a different spelling, and \
             each spends the wait twice: it cannot be woken early when the work finishes \
             in twenty seconds, and it cannot be ended cleanly when it fails. A job is \
             stopped with `job_kill`. Every process lands in a cgroup \
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
                    "timeout_ms": {"type": "integer", "description": "How long this command may run before it is killed and the result says so. Defaults to 120000 (2 minutes); set it higher for a command you know takes longer. Ignored when background is true. A command you expect to take over a minute belongs in the background — `background: true` runs it with no deadline and wakes you when it ends — rather than behind a raised timeout_ms."},
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

        // **A program that wants to own the terminal is refused before it is
        // started** — and this is the FIRST thing decided, ahead of the backend, the
        // scope and the predicate, because it is a fact about the command text alone.
        //
        // Only where a terminal is handed out, which is the operator's own run:
        // `InvokeCtx::tty` is `!gated` and nothing else, so this is the `!` line and
        // the door's calls. A model's `bash` call keeps its pipe, where `nano` reads
        // EOF and exits rather than hanging — a different defect, and `exec::terminal`
        // says so in its own list of misses.
        //
        // The operator's report is the reason this exists at all: *"what if I do
        // `! sudo ls /root` … and we also have to think about things like nano. what
        // happens when I run something long and running and that wants to own
        // everything"*. The pty that makes `ls --color=auto` colourise is a **capture**
        // — one transcript row, `/dev/null` on stdin — so a program that takes the
        // screen draws escapes into that row and waits for a keystroke that cannot
        // arrive. `exec::terminal` carries the rule, what it looks at, and what it
        // will therefore miss.
        if ctx.tty
            && let Some(r) = terminal::wants_the_terminal(command)
        {
            return Invocation::not_run(r.why(), r.body());
        }

        // **What the workspace looked like before this command.** A shell that
        // rewrites a file hands the head nothing, so the change lands with no
        // diff and no record; the sweep after the command is what turns it back
        // into an edit card. See `crate::detect` — one `git status` on a clean
        // tree, and nothing read.
        // **The workspace, not the backend's root** (R18). `root_path()` is `/` for an
        // unconfined session — the right answer to *is this path inside the boundary*,
        // and the wrong one for *where is the tree I am watching*, which is the only
        // question a `git status` can answer. Seeded from `/` the sweep finds no
        // repository, so a shell that rewrites a file hands the head no diff and the
        // detection this exists for silently does nothing on exactly the sessions that
        // can write anything.
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
        // [`deadline_for`] is the one place the choice is made; the third fact
        // it takes is the operator's own run, the same `ctx.tty` the pty and
        // the shell already key on — one flag because it is one fact.
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
        // **The world their console gives this run, and only this run.**
        //
        // `exec::console` carries the decision and what it changes. The pairs go on the
        // REQUEST rather than being added by the host after the wrap is built, and that
        // is not tidiness: `confine` reads `req.env` to decide what to put back inside a
        // confined view (`--clearenv` then a keep-list then these), so a pair added later
        // would reach an unconfined child and be dropped for a confined one — the pager
        // fix would hold in one session and not in the other.
        //
        // A model's call gets none of it: `ctx.tty` is `!gated` and nothing else, and
        // its environment is R10 layer 1. See `exec::console` for why each pair here is
        // about a terminal the model's run does not have.
        let mut env: Vec<(String, String)> = Vec::new();
        if ctx.tty {
            env.extend(crate::exec::console::env());
        }
        // So a helper the command runs can say what it is running for —
        // `letibot-askpass` puts it on the password card.
        env.push(("LETIBOT_COMMAND".to_string(), command.to_string()));
        let req = SpawnRequest {
            command: command.to_string(),
            cwd,
            scope,
            scope_name,
            background,
            env,
            // **The operator's own run meets their console; a model's does not.**
            //
            // The operator reported it more than once: *"i run `! ls -la` and the
            // output is plain, while in a proper terminal directory names are
            // highlighted"*. The pty is half the answer — `ls` colours only when
            // `isatty(1)` is true, and a pipe is not a terminal, so the fix is to give
            // the run one; see `exec::pty` for the measurement, the cost, and why the
            // environment alone cannot do it. The other half is that the alias which
            // asks for the colour is shell state in an rc file, so the run is also
            // handed to their shell, interactive, with their environment on it — see
            // `exec::console`, which is where the decision and its consequences live.
            // The model's call keeps its pipe and the host's `/bin/sh -c`: the payload
            // is tokens it reads, `ESC[01;34m` around every directory name is a cost it
            // pays and cannot see, and the text a gate judged is text this shell reads.
            tty: ctx.tty,
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

        // **The deadline is armed on the HOST, and the host is what fires it.**
        //
        // This is the one line that stops the timeout being enforced inside the thing it
        // guards. The loop below still checks the clock — it is the fast path and the one
        // that can say how far past the deadline the run went — but it is no longer the
        // only thing that would end the run, so a run whose thread never comes back round
        // that loop is still ended, by the run's own cgroup and by a thread that is not
        // the run's. See [`crate::exec::host::Deadlines`].
        //
        // A run with no deadline has nothing armed for it: `background: true`
        // (the model asking for exactly that) and the operator's own run (a
        // person's command, whose stop is their own act) both come back `None`
        // from [`deadline_for`].
        if let Some(timeout) = timeout {
            host.arm_deadline(&id, timeout);
        }

        if background {
            let view = host.job(&id);
            // **Where this job's own output goes** — R41, requirement one, and it changes
            // what the sentences below may SAY rather than only adding a footnote beside
            // them.
            //
            // Measured on the first cut of this: the note explained that the window would be
            // empty while the payload under it went on telling the model that `job_output`
            // "reads what it has written so far". A note that argues with the line beneath it
            // is worse than no note — the model believes the line it has seen before.
            //
            // The operator's own case: *"letibot just started a background build and then
            // `sleep 200`"* — `cargo build --release … > /tmp/release-build.log 2>&1`, whose
            // capture is empty by construction. **The redirect is in the text**, so this is
            // knowable now, needs nothing to run, and is the moment to say it.
            let redirected = crate::builtins::output_redirect_path(command);
            // `Backgrounded`, not `Ok`, even though the model asked. The outcome names a fact
            // about the world — *this command is running and has not answered yet* — and that
            // fact does not depend on who wanted it. Reporting `Ok` here would make a `bash
            // --background` result grounding for an answer nothing has produced, which is
            // exactly what `is_grounded` exists to prevent, and it would leave a head with two
            // shapes to render for one situation.
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
            // **The rule, not a verb list.** R7 closed `job_wait` and the model went on
            // waiting with `sleep 200; tail -3 log`, so what has to be said is *why* the wait
            // is unnecessary rather than which words are forbidden.
            // One line per sentence, and one `concat!` rather than a wrapped literal:
            // a `\` continuation strips, and the version of this that shipped without
            // one put `a                          `sleep`` in front of a model that
            // reads every character.
            let clock = concat!(
                "**You are woken when it finishes, so do not build your own clock** — a ",
                "`sleep`, a `tail` in a loop or a poll are the same mistake in three ",
                "spellings, and each spends the wait twice: it cannot be woken early when ",
                "the work finishes in twenty seconds, and it cannot be ended when it fails.",
            );
            let mut inv = Invocation::backgrounded(
                id.0.clone(),
                Duration::ZERO,
                Backgrounding::Asked,
                format!(
                    "carry on — `{id}`'s completion is delivered to you on its own when it ends, so there is nothing to wait for. {read_it}"
                ),
                format!(
                    "started `{id}` in the background.\n  command: {command}\n  \
                     pid: {}\n  scope: {} — {}\n\nIt is running now, and **its \
                     completion will reach you by itself when it ends — do not wait for \
                     it, and do not poll.** {clock}\n\n{read_it} Carry on with something \
                     else; when the job finishes you are told, unprompted, with its command, \
                     how it ended and where its output is.",
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
        //
        // **The operator's own run is handed to the daemon before it is waited on, and
        // taken back after.** Two reports, and the first is the one that matters most: it
        // says *this run has a stdin and here is the way in*, which is what makes `!send`
        // work **when no card is ever raised at all** — the miss this whole feature is
        // allowed to have because a person can still answer. See [`OperatorRun`].
        //
        // Only for the operator's own run: a model's `bash` call has `/dev/null` for stdin,
        // so `stdin.is_open()` is false for it and nothing is reported. The flag and the
        // handle are checked together rather than either alone, because *a model's run that
        // somehow got a pipe* would be the failure this must not have.
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
        // **The run is over, so whatever card was up for it comes down.** After the wait
        // and before every branch below, because the three of them (a state, a promotion, a
        // deadline) are all "this tool is no longer waiting" — and a card that outlived its
        // command would be a person typing an answer into a program that had already exited.
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
            // **A promotion is a backgrounding, so R41's redirect rule is this one's too.**
            // A command the OPERATOR moved to the background (Ctrl+B) has the same capture as
            // one the model asked for, and the same sentence must not tell its reader to open a
            // window that will stay empty.
            let next = match crate::builtins::output_redirect_path(command) {
                Some(path) => format!(
                    "carry on — `{id}`'s completion will be delivered to you on its own when it ends, so there is nothing to wait for. Its own window will be EMPTY: the command sends stdout to `{path}` — read that file with `read` when the completion tells you it ended, and do not build your own clock in the meantime."
                ),
                None => format!(
                    "carry on — `{id}`'s completion will be delivered to you on its own when it ends, so there is nothing to wait for. `job_output` with job=\"{id}\" reads what it has written so far."
                ),
            };
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
            //
            // **And the second way a deadline arrives here.** `JobState::Running` is the
            // loop's own check — it looked at the clock and the run was still going. This
            // arm is the host's watchdog having already ended the run, which is the case
            // where the loop did *not* look: the job is settled as `Killed` with the
            // deadline's own reason, and reporting that as a plain failure would be a
            // timeout rendered as an error, which is F5 with the sign flipped. Both paths
            // render the same sentence, because they are the same fact.
            JobState::Killed { by } if by == crate::exec::DEADLINE_KILL => {
                // `Some` by construction: the watchdog settles `DEADLINE_KILL`
                // only for a deadline it was armed with, and nothing arms one
                // when [`deadline_for`] said `None` — the `unwrap_or` is for
                // the type, not a case.
                let reaped = host.kill_job(&id);
                let mut inv = Invocation::timed_out(format!(
                    "{body}\n\n[the command `{id}` was killed after {:.0}s — it outlived \
                     its deadline]\n  command: {command}\n\nIt did not fail; it was \
                     stopped. To run it longer, call `bash` again with a larger \
                     `timeout_ms`, or `background: true` to run it with no deadline.",
                    timeout.map(|t| t.as_secs_f32()).unwrap_or(0.0)
                ));
                if let Err(e) = &reaped {
                    inv = inv.with_note(format!("the kill did not complete: {e}"));
                }
                inv
            }
            JobState::Running => match timeout {
                Some(timeout) => {
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
                // **No deadline was armed, so this is not a timeout and the run
                // is NOT stopped.** `Running` with no deadline arrives one way:
                // the wait ended without the run ending — a promotion the host
                // refused, or a wait that errored — and both are facts about
                // this daemon's machinery, not about the command. Killing here
                // would be the measured bug by another door: the person's `!`
                // run ended by a clock nobody armed for it. The run is the
                // operator's own, still going; its stop is their act — Ctrl+C
                // at their console, `!term`, closing the head — or `job_kill`
                // by id, and saying so with the id is the one honest answer
                // this call can return.
                None => Invocation::failed(
                    format!("`{id}` is still running"),
                    format!(
                        "{body}\n\n[the wait for `{id}` ended while it was still running \
                         — no deadline applies to it, so it was NOT stopped]\n  \
                         command: {command}\n\nIt is the operator's own run: their console has its \
                         output, their own acts are its stop. `job_output` with \
                         job=\"{id}\" reads what it has written so far; `job_kill` ends it.",
                    ),
                ),
            },
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

        // **A run that spent over a minute in the foreground says so on its way
        // out** — appended to the result, which is the channel the model reads, so
        // the reminder lands on the next round in band, with no new delivery path.
        // The operator saw the live line during the wait (the loop's progress
        // register); this is the model's copy, and it names the tool's own feature
        // rather than the escape hatch the model reached for — the measured defect
        // was a `timeout_ms` of 600000 for a build that belonged in the background.
        //
        // Skipped for the two deadline arms, whose payloads already end with
        // *"call `bash` again with a larger `timeout_ms`, or `background: true`"* —
        // a second note would give the same advice twice on one card. And for a
        // promotion, whose body already says the command was moved because the
        // model did not ask for it.
        let deadline_answered = matches!(
            &state,
            JobState::Killed { by, .. } if by == crate::exec::DEADLINE_KILL
        ) || (matches!(&state, JobState::Running) && timeout.is_some());
        if let Some(note) = slow_foreground(waited_from.elapsed(), ctx.tty)
            && !deadline_answered
        {
            inv = inv.with_note(note);
        }

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

/// **What has already been said about this run**, so one question is one card and one
/// inability is one sentence.
///
/// A run that is blocked stays blocked for as long as nobody answers, and a run whose
/// processes cannot be read stays unreadable for as long as it runs — so a loop that reported
/// either every tick would put a hundred identical lines on the screen for one fact.
#[derive(Default)]
struct Told {
    /// **What the last card was about.** Keyed on the pair (bytes produced, the line shown)
    /// and not on the line alone: a program that asks the same question twice **after saying
    /// something in between** has asked twice, and the second ask is a second thing to answer.
    raised: Option<(u64, Option<String>)>,
    /// Whether the daemon has already said this run is slow. Once per run: the
    /// condition does not un-cross, and a line every 500 ms for one fact is the
    /// liveness signal this loop refuses to be.
    slow: bool,
    /// Whether the daemon has already said it cannot tell whether this run is waiting. Once
    /// per run: it is a disclosure about a condition, and the condition does not change while
    /// the run lasts.
    unreadable: bool,
}

/// Wait, emitting progress that is a measurement of work rather than a heartbeat,
/// and honour a head's Ctrl+B by promoting the command mid-flight.
///
/// `timeout` is [`Option`]: `None` is no deadline, and the loop then has no
/// clock to break on — it ends only on the run's own state, a promotion, or a
/// wait that errored. That is the operator's own run; see [`deadline_for`].
///
/// **And ask, once a beat, whether this run is waiting for an answer.** That is the
/// operator's `!` line and nothing else — see [`OperatorRun`] — and it is here because
/// this loop is the only thing that has the three facts the question needs at once: the
/// job is **alive**, it has been **quiet** for a beat, and the loop is already polling.
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
        // **Past a minute in the foreground, say so — once, in this loop's own
        // register.** The decision is [`slow_foreground`]'s and the boundary is
        // [`SLOW_FOREGROUND`]'s; this is the operator's live line, drawn on the
        // same card the bytes count above draws on. The model's copy is appended
        // to the call's result by [`Bash::invoke`], which is the channel it reads.
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

/// **How long a run must have written nothing before it is read as waiting.**
///
/// The poll above is half a second, so this is one beat: at the tick after a program stops
/// writing, its last byte is at least `QUIET` old. Named rather than inlined because it is
/// the one tunable here and it is a judgement — too short and a program between two lines
/// of a slow build looks like a question, too long and a person waits for a card that a
/// program has been blocked on for a second already.
const QUIET: Duration = Duration::from_millis(250);

/// **Is this run waiting for an answer?** — and if so, tell the daemon.
///
/// Three conditions, and the first two are the caller's because the caller holds the clock
/// and the job state:
///
/// 1. **Alive.** A program that printed something and exited is not waiting for anything,
///    and this is also what keeps `! printf 'are you sure?'` from raising a card: the
///    `wait_job` above returns `Happened` for it and this function is never reached.
/// 2. **Quiet for a beat.** A program that is asking and still drawing is a program that
///    is not blocked. `since_last_output` is `None` for a program that has written nothing
///    at all — `! cat`, blocked before its first byte, which is a real case — and the
///    elapsed time is the same beat for it. **This condition is the CARD's and only the
///    card's** — see below.
/// 3. **Some process of the run has its stdin on the pipe this daemon holds and is blocked
///    in a read on that descriptor.** That is [`crate::exec::ask`], and it is the whole of
///    the detection: **not one byte of the run's output is consulted to decide anything.**
///
/// # The third answer, which is not the second
///
/// [`crate::exec::ask::Waiting::Unreadable`] is *"this daemon could not look"* — a process of
/// the run belongs to another uid (the `sudo` case), or a `/proc` this session's daemon may
/// not open. **No card is raised on it**, and that is deliberate: a card is a reading of the
/// process, and one raised here would be a guess that is wrong for every long quiet command
/// that is not asking anything.
///
/// But it is not nothing, and saying nothing is what the operator met: `! sudo apt install
/// mc`, the password given, `apt` waiting at `Continue? [Y/n]` as root where the daemon may
/// not read it — so no card, no sentence, and the run holding the daemon's one worker until
/// its deadline. *"the command appears queued and the daemon hangs."* The report below is the
/// one thing that can honestly be said there, and it names the way in — `!send`, which needs
/// no signal at all and works under every miss the card has. Said **once per run**: it is a
/// condition, not an event.
///
/// # Why the beat gates the card and not this
///
/// The operator's report has a second half, and it is the reason this function's `match` is
/// ordered the way it is:
///
/// > *"so i start it completely fresh and do apt install and harnessd hangs without printing
/// > anything to me. I suppose it waits for y or n but doesnt show me anything"*
///
/// In that run `apt` was **streaming its progress** — their own transcript's words — and the
/// daemon said nothing, because the `Unreadable` arm used to sit *behind* the quiet check: a
/// run writing every 100 ms has no 250 ms gap for a 500 ms tick to land in, so the ask never
/// happened. The two facts are not the same kind of fact. The card is a **reading** and a run
/// that is still drawing is genuinely not blocked, so the beat belongs to it. `Unreadable` is
/// the **absence** of a reading and is about permissions, not about the clock — exactly as
/// true at a 100 ms write interval as at a 2 s one. And from the person's seat it is the only
/// thing that can tell *working* from *blocked* at all: a `!` line's own output does not land
/// until the run ends, so a streaming run and a hung one look identical until something says
/// otherwise. So the beat is checked where it decides a **card**, and the inability is
/// reported either way, carrying the beat so the sentence can say which of the two it is.
///
/// The text that travels with a card is the last line of the output, for the card to
/// **show** — see [`crate::exec::ask::last_line`] and the module header's argument for why
/// nothing anywhere may decide by it.
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
    // **The device is OURS**, and the comparison is what keeps a `grep` blocked on `ls`'s
    // pipe in `! ls | grep foo` from looking like a program waiting for a line. The run's
    // fd 0 is its own terminal — `pipe:[<inode>]` is the pty-less fallback — and
    // [`Stdin::input_end`] is the name the kernel will give both sides.
    let handle = host.job_handle(id);
    let ours = handle.as_ref().and_then(|j| j.stdin().input_end());
    let pids = host.job_pids(id);
    let verdict = crate::exec::ask::waiting_for_an_answer(&pids, ours.as_ref());
    // **And nothing has been typed at it for a beat.** The second half of the claim, and it
    // is new with the terminal: a program that has just been answered is blocked reading its
    // terminal again within microseconds, so without this the card would be raised about a
    // question the person answered a moment ago — every time they answered one. `None` is
    // *never written to*, which is the quietest case there is.
    const ANSWERED: Duration = Duration::from_millis(1500);
    let just_answered = handle
        .as_ref()
        .and_then(|j| j.stdin().since_last_input())
        .is_some_and(|d| d < ANSWERED);
    match verdict {
        // **The signal was read, and it says yes** — and the card still needs both beats. The
        // output beat is what the beat is FOR: a card claims *some process of this run is
        // blocked reading the answer we hold*, and a run that is still drawing is genuinely
        // not blocked. A program between two lines of a slow build is not asking anything.
        crate::exec::ask::Waiting::Yes if quiet && !just_answered => {}
        // The signal was read and it says yes, but the run is still writing or was just
        // answered: not a card.
        crate::exec::ask::Waiting::Yes => return,
        // **The signal was read, and it says no** — every process of the run was looked at
        // and none is reading our pipe. Nothing to say.
        crate::exec::ask::Waiting::No => return,
        // **The signal could not be read**, and this one is NOT the card's kind of fact.
        //
        // The beat above is right for the card and wrong here: `Unreadable` is the *absence*
        // of a reading — *one of this run's processes is not mine to look at* — and that is a
        // fact about permissions, not about the clock. It is exactly as true at a 100 ms write
        // interval as at a 2 s one, and gating it behind the beat is what made the daemon say
        // nothing at all for the operator's second report: `apt` **streams its progress**, so
        // a run they could not see the output of never went quiet long enough to be told
        // about. One sentence, once per run, whether the run is quiet or not — and it says
        // which of the two facts it is, so the person is not told *quiet for a beat* about a
        // run that is writing.
        crate::exec::ask::Waiting::Unreadable => {
            if !told.unreadable {
                told.unreadable = true;
                ctx.operator_run(OperatorRun::Unreadable { job: &id.0, quiet });
            }
            return;
        }
    }
    // **The last line, and only to show.** Bounded by the ring: the question is at the end
    // of what a program wrote, and a program that wrote a megabyte before asking has its
    // answer in the last few kilobytes of it.
    const SHOW_BYTES: u64 = 4096;
    let shown = host
        .output(
            id,
            v.produced.saturating_sub(SHOW_BYTES),
            SHOW_BYTES as usize,
        )
        .ok()
        .and_then(|s| crate::exec::ask::last_line(&s.text()));
    if told.raised == Some((v.produced, shown.clone())) {
        return;
    }
    told.raised = Some((v.produced, shown.clone()));
    ctx.operator_run(OperatorRun::Waiting {
        job: &id.0,
        question: shown.as_deref(),
    });
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

    #[test]
    fn a_background_run_has_no_deadline() {
        assert_eq!(deadline_for(None, true, false), None);
    }

    #[test]
    fn the_operators_own_run_has_no_deadline_whatever_the_call_says() {
        // The measured case: the daemon mints the operator's call with
        // `command` and nothing else, so `asked_ms` is `None` — and the run
        // died at its silent default anyway.
        assert_eq!(deadline_for(None, false, true), None);
        // A `timeout_ms` no head can even pass today is still not a deadline:
        // the person's run is the person's. See `deadline_for` for the rule.
        assert_eq!(deadline_for(Some(5_000), false, true), None);
    }

    /// The boundary is the operator's own rule — *"if tool took more than a
    /// minute"* — and both sides of it are pinned without waiting a real minute:
    /// the decision is pure, so a nanosecond either side of [`SLOW_FOREGROUND`]
    /// stands in for the minute.
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
        // The advice names the tool's own feature AND the escape hatch it replaces
        // — the measured model raised `timeout_ms`, so the sentence has to say why
        // that was the wrong knob, not merely name the right one.
        assert!(note.contains("background: true"), "{note}");
        assert!(note.contains("timeout_ms"), "{note}");
        assert!(note.contains("completion is delivered"), "{note}");
    }

    /// The operator's own run is exempt: a person is watching it, and the seat
    /// that reads its result is that person — the reminder is for a model
    /// deciding how to spell its next call.
    #[test]
    fn the_operators_own_run_is_not_reminded_even_past_the_minute() {
        assert_eq!(slow_foreground(Duration::from_secs(600), true), None);
        // And the contrast: a model's run of the same age owes it.
        assert!(slow_foreground(Duration::from_secs(600), false).is_some());
    }

    #[test]
    fn the_description_declares_exec_and_says_how_to_use_the_tool() {
        let s = Bash.schema();
        assert_eq!(s.access, Access::Exec);
        assert!(!s.access.is_unattended(), "exec must reach the gate");
        assert_eq!(crate::schema::lint_description(&s.description), vec![]);
    }
}
