//! `monitor` — declare, renew or retire a condition watched **across turns**.
//!
//! One tool and not three, and the arithmetic is the argument. §8.4's ceiling is
//! eight and [`crate::runtime::roles::m2_runner`] was already at eight. Three
//! monitor verbs would have cost the role three seats it does not have, so:
//!
//! - **declaring, renewing and retiring** are one tool taking an `action`, because
//!   all three act on the same named handle and none of them is a different
//!   question — *set the state of this named watch* covers the three.
//! - **listing** is not here at all. It is in `job_list`, next to the jobs, the
//!   scopes and the reap log, because *"what is running and what is watching"* is
//!   one question and the place people already look is the place a watcher must be
//!   visible. T24 requirement 2 is better served by being in the existing listing
//!   than by a second one nobody opens.
//!
//! That leaves one new seat, and `m2_runner` names the trade in its own doc.
//!
//! # The argument this tool does not have
//!
//! There is no `pattern`, no `match`, no `cmdline`, no `command`, no `until`, no
//! `host`. [`no_monitor_tool_grows_a_pattern_argument`] asserts it, the way the
//! job tools already do for themselves.
//!
//! This is not tidiness. A process check self-matched its own shell **seven times
//! in one session** on this box, and the seventh — `pkill -f
//! 'harnessd-rice.sock'` — killed a running command mid-flight, because the
//! string was in the shell's own command line. Prompting has failed against this
//! eight times now. The only thing that has ever worked is that there is nowhere
//! to type the pattern.

use std::time::Duration;

use serde_json::Value;

use crate::exec::monitor::{
    CommandCondition, CustomWatch, DEFAULT_TTL, Fired, MAX_TTL, MonitorError, PortState,
    TimerCondition, Watch,
};
use crate::exec::{JobId, ProcessHost, ScopeId, ScopeKind};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

pub struct Monitor;

fn no_process_host(ctx: &InvokeCtx<'_>) -> Invocation {
    Invocation::failed(
        "this session's backend cannot start processes, so it has nothing to watch",
        format!(
            "the backend is `{}`. This is not a claim that there is nothing \
             happening — there is no scope tree to own a monitor, and a monitor \
             without an owner is the leak monitors are scoped to prevent. A session \
             that runs commands is opened with `HostBackend::executable`.",
            ctx.backend.describe()
        ),
    )
}

impl Tool for Monitor {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "monitor",
            "Watch one condition ACROSS turns and be told when it fires — unlike \
             `job_wait`, which blocks inside this turn. Give `name` (yours, and only \
             one monitor may hold a name at a time) and exactly one of: `job` (a job \
             leaving the running state), `scope` (a cgroup emptying), `path` (a file \
             or directory appearing, vanishing, or changing size or modification \
             time), `port` (a loopback TCP port becoming listenable, or with \
             `port_state: \"closed\"`, stopping), `timer_ms` (a timer), `command` (a \
             command exiting — a health check, not a place to run work), `pid` (a \
             process, by pid — resolved to its start time now, so a reused pid is not \
             it) or `process` (every process of this user whose command line contains \
             the string, found ONCE now — this daemon, its ancestors and its protected \
             processes are removed before matching, so it cannot find itself — and \
             then watched by pid and start time until all are gone; `pkill` lists the \
             same set) or `github` (a pull request or issue, through the operator's `gh`: \
             closed, merged, its checks finishing, a review, a new comment, or CodeRabbit's \
             review). Optionally `owner` to say which scope reaps it (`turn`, \
             `session` — the default — or a named scope that already exists), \
             `ttl_ms`, which is capped, and `repeat`; `action: \"renew\"` extends it \
             and `action: \"retire\"` ends it. `job_list` shows every monitor, who \
             declared it, how long it has left, and for the ones that ended, which of \
             the four endings it was and why.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "What to call this monitor. One monitor may hold a name at a time; a second under the same name is refused, not replaced."},
                    "action": {"type": "string", "description": "`watch` (the default) declares it, `renew` extends its ttl, `retire` ends it."},
                    "job": {"type": "string", "description": "Watch this job id leave the running state."},
                    "scope": {"type": "string", "description": "Watch this scope's cgroup empty: `turn`, `session`, or a named scope."},
                    "path": {"type": "string", "description": "Watch this path, relative to the workspace, appear, vanish, or change size or modification time."},
                    "port": {"type": "integer", "description": "Watch this TCP port on the loopback interface. There is no host argument; it is always this machine."},
                    "port_state": {"type": "string", "description": "`listening` (the default) fires when something starts listening; `closed` fires when nothing is."},
                    "timer_ms": {"type": "integer", "description": "Watch a timer: fire after this many milliseconds. With repeat, it fires every interval."},
                    "pid": {"type": "integer", "description": "Watch one process leave, by pid. Resolved to (pid, start time) now; this daemon and its ancestors are refused."},
                    "process": {"type": "string", "description": "Watch every process of this user whose command line contains this string leave. Found once, now, never including this daemon or its ancestors; then watched by handle. Fires when all are gone."},
                    "command": {"type": "string", "description": "Watch a command: fire when it exits (any exit code). A health check, not a shell you run work in."},
                    "github": {"type": "object", "description": "Watch a GitHub pull request or issue through the operator's `gh`: {\"url\": \"https://github.com/OWNER/REPO/pull/N\", \"until\": \"closed\"|\"merged\"|\"checks\"|\"review\"|\"comment\"|\"coderabbit\"} (or `repo` with `pr`/`issue` instead of `url`); `until` may be a list — ONE watch per pull request, one look, for all of them: a second watch on the same one is refused — optionally `every_s` (how often it looks, default 90, at least 30). A review, comment or CodeRabbit pass counts only after the watch is armed; a state already true (merged, checks done) fires at once. Lives a day by default, up to a week; with `repeat` it fires again for the next one (the next push's checks, the next review).", "properties": {"url": {"type": "string"}, "repo": {"type": "string"}, "pr": {"type": "integer"}, "issue": {"type": "integer"}, "until": {"anyOf": [{"type": "string", "enum": ["closed", "merged", "checks", "review", "comment", "coderabbit"]}, {"type": "array", "items": {"type": "string", "enum": ["closed", "merged", "checks", "review", "comment", "coderabbit"]}}]}, "every_s": {"type": "integer"}}},
                    "owner": {"type": "string", "description": "Which scope reaps this monitor: `turn`, `session` (the default), or the name of a scope that already exists."},
                    "ttl_ms": {"type": "integer", "description": "How long it watches before it expires without firing. Capped; a larger value is refused rather than quietly reduced."},
                    "repeat": {"type": "boolean", "description": "Continuous rather than one-shot: after it fires it re-arms and keeps watching for the next occurrence (a path changing again, a port flapping, a timer on an interval). A one-shot monitor settles the first time its condition is met."}
                },
                "required": ["name"]
            }),
            // It creates a resource that outlives the turn — and, with an
            // `explicit` owner, the session. Clause 4 wants the widest thing it can
            // do, and "leave something watching on the operator's box after this
            // conversation" is that. It is emphatically NOT `Network`: see the
            // module docs on why `port` has no host argument.
            Access::Exec,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(host) = ctx.backend.processes() else {
            return no_process_host(ctx);
        };
        let Some(monitors) = host.monitors() else {
            // An honest absence, not an empty list. `None` here means this host
            // cannot watch anything, which is a different answer from "nothing is
            // being watched" and must not be rendered as it.
            return Invocation::not_run(
                "this session's process host has no monitor mechanism",
                "nothing was declared. This is not a claim that there is nothing to \
                 watch — there is nowhere to put a watcher.",
            );
        };
        let Some(name) = args.get("name").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "monitor needs a name",
                "call `monitor` again with `name` set. The name is how you renew or \
                 retire it later, and it is what makes one waiter per name \
                 enforceable — an unnamed watcher is one nobody can end.",
            );
        };
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("watch");

        match action {
            "retire" => {
                let Some(f) = monitors.retire(name, "the model") else {
                    return unknown_monitor(host, name);
                };
                // R93.F29: this rendered "it had retired by the model after 30s left",
                // and "It never fired" was false of a repeat monitor that had.
                let age = match &f.fired {
                    Fired::Cancelled { after, .. } => format!(" after {:.0}s", after.as_secs_f32()),
                    _ => String::new(),
                };
                return Invocation::ok(format!("retired `{name}` (watching {}){age}.", f.watch));
            }
            "renew" => {
                // A watch with a cap of its own (GitHub's) renews within it, and to its default.
                let own = monitors
                    .get(name)
                    .map(|m| m.watch.max_ttl())
                    .unwrap_or(MAX_TTL);
                let (default, cap) = if own > MAX_TTL {
                    (crate::exec::github::GITHUB_DEFAULT_TTL, own)
                } else {
                    (DEFAULT_TTL, MAX_TTL)
                };
                let ttl = match ttl_within(args, default, cap) {
                    Ok(t) => t,
                    Err(inv) => return *inv,
                };
                return match monitors.renew(name, ttl) {
                    Ok(m) => Invocation::ok(format!(
                        "renewed `{name}` for {:.0}s. It is still watching {}, and it \
                         has not fired.",
                        ttl.as_secs_f32(),
                        m.watch.describe()
                    )),
                    Err(e) => {
                        Invocation::failed(format!("`{name}` was not renewed"), e.to_string())
                    }
                };
            }
            "watch" => {}
            other => {
                return Invocation::failed(
                    format!("`{other}` is not a monitor action"),
                    "there are three and no fourth: `watch` declares one, `renew` \
                     extends its ttl, `retire` ends it. Nothing was done.",
                );
            }
        }

        // --- declare -------------------------------------------------------

        let ttl = match if args.get("github").is_some() {
            ttl_within(
                args,
                crate::exec::github::GITHUB_DEFAULT_TTL,
                crate::exec::github::GITHUB_MAX_TTL,
            )
        } else {
            ttl_of(args)
        } {
            Ok(t) => t,
            Err(inv) => return *inv,
        };

        // Exactly one condition. Two would be one monitor with one firing for two
        // questions, which is `job_wait`'s job/scope rule and the same reasoning:
        // a single answer to two questions tells you neither.
        let asked: Vec<&str> = [
            "job", "scope", "path", "port", "timer_ms", "command", "pid", "process", "github",
        ]
        .into_iter()
        .filter(|k| args.get(*k).is_some())
        .collect();
        if asked.len() != 1 {
            return Invocation::failed(
                if asked.is_empty() {
                    "monitor needs something to watch".to_string()
                } else {
                    format!(
                        "monitor takes one condition, and {} were given",
                        asked.len()
                    )
                },
                "give exactly one of `job` (a job id), `scope` (a cgroup), `path` (a \
                 file or directory), `port` (a loopback TCP port), `timer_ms` (a \
                 timer), `command` (a command whose exit is the condition), `pid` (a \
                 process by pid), `process` (processes found by a string in their \
                 command line, once, never this daemon) or `github` (a pull request or issue). \
                 Nothing was declared.",
            );
        }

        let mut notes: Vec<String> = Vec::new();
        let mut job_handle = None;
        let watch = if let Some(id) = args.get("job").and_then(|v| v.as_str()) {
            let jid = JobId(id.to_string());
            // Resolved to a live handle here, so a monitor can never be declared
            // against an id that does not exist — and so the refusal is clause 1's
            // listing rather than a monitor that watches nothing forever.
            let Some(h) = host.job_handle(&jid) else {
                return super::jobs::unknown_job(&host.jobs(), id);
            };
            job_handle = Some(h);
            Watch::Job(jid)
        } else if let Some(s) = args.get("scope").and_then(|v| v.as_str()) {
            let Some(sid) = super::jobs::resolve_scope(host, s) else {
                return super::jobs::unknown_scope(host, s);
            };
            Watch::Scope(sid)
        } else if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
            // Resolved against the workspace root, and checked **lexically**
            // rather than by stat: the whole point of a path monitor is a path
            // that does not exist yet, so `stat`-based containment would refuse
            // exactly the case it is for.
            let Some(root) = host.workspace() else {
                return Invocation::not_run(
                    "this session's process host does not know where its workspace is",
                    "nothing was declared, because a relative path cannot be resolved \
                     against a root nobody named.",
                );
            };
            if !inside(p) {
                return Invocation::failed(
                    format!("`{p}` leaves this session's workspace"),
                    format!(
                        "nothing was declared. The workspace is `{}`, and a monitor \
                         watching outside it would be this session leaving a watcher \
                         somewhere it cannot account for. Give a path relative to the \
                         workspace root.",
                        root.display()
                    ),
                );
            }
            Watch::Path(root.join(p.trim_start_matches("./")))
        } else if let Some(ms) = args.get("timer_ms").and_then(|v| v.as_u64()) {
            if ms == 0 {
                return Invocation::failed(
                    "`timer_ms` must be positive",
                    "give a positive number of milliseconds. Nothing was declared.",
                );
            }
            Watch::Custom(CustomWatch(TimerCondition::after(
                std::time::Duration::from_millis(ms),
            )))
        } else if let Some(pid) = args.get("pid").and_then(|v| v.as_u64()) {
            let pid = pid as u32;
            if crate::exec::procs::self_and_ancestors().contains(&pid) {
                return Invocation::failed(
                    format!("pid {pid} is this daemon or an ancestor of it"),
                    "watching the process that evaluates your calls for its own exit is \
                     not a condition; nothing was declared.",
                );
            }
            let Some(p) = crate::exec::procs::read(pid).filter(|p| p.state != 'Z') else {
                return Invocation::failed(
                    format!("no process {pid}"),
                    "it is not running (or not yours to read), so there is nothing to watch \
                     leave. Nothing was declared.",
                );
            };
            let what = format!("pid {pid} ({})", p.cmdline);
            Watch::Custom(CustomWatch(crate::exec::procs::ProcessCondition::over(
                vec![p],
                what,
            )))
        } else if let Some(pat) = args.get("process").and_then(|v| v.as_str()) {
            let pat = pat.trim();
            if pat.is_empty() {
                return Invocation::failed(
                    "`process` was empty",
                    "give a string. Nothing was declared.",
                );
            }
            // Resolved ONCE, here. The pattern never reaches the poller.
            let protected: Vec<u32> = host.protected().into_iter().map(|p| p.pid).collect();
            let found = crate::exec::procs::find(pat, &protected);
            if found.is_empty() {
                return Invocation::failed(
                    format!("no process of this user matches `{pat}`"),
                    "not counting this daemon, its ancestors and what it protects, which are \
                     never matched. A monitor over nothing would fire at once and read as \
                     news, so nothing was declared. `pkill` with the same pattern lists what \
                     this would have watched.",
                );
            }
            notes.push(format!(
                "watching {} process(es) found now for `{pat}`, by pid and start time: {}. A \
                 process that starts later and would match is NOT watched — the pattern \
                 was used once, to find these.",
                found.len(),
                found
                    .iter()
                    .map(|p| p.line())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
            let what = format!("`{pat}` ({} found)", found.len());
            Watch::Custom(CustomWatch(crate::exec::procs::ProcessCondition::over(
                found, what,
            )))
        } else if let Some(g) = args.get("github") {
            let w = match github_watch(g) {
                Ok(w) => w,
                Err(inv) => return *inv,
            };
            // **One watch per pull request** — a second on one already watched is refused, and
            // the refusal is the one call that watches all of it. Measured on stroppy: nine
            // watches, three conditions apiece, for three pull requests — nine `gh` calls a look
            // where three would do.
            if let Some(inv) = already_watched(monitors, &w, name, args) {
                return inv;
            }
            w
        } else if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
            let argv: Vec<String> = cmd.split_whitespace().map(|s| s.to_string()).collect();
            if argv.is_empty() {
                return Invocation::failed(
                    "`command` was empty",
                    "give a command. Nothing was declared.",
                );
            }
            Watch::Custom(CustomWatch(CommandCondition::any(argv)))
        } else {
            let Some(port) = args.get("port").and_then(|v| v.as_u64()) else {
                return Invocation::failed(
                    "`port` must be a number",
                    "give the TCP port as an integer. Nothing was declared.",
                );
            };
            if port == 0 || port > 65_535 {
                return Invocation::failed(
                    format!("{port} is not a TCP port"),
                    "ports are 1–65535. Nothing was declared.",
                );
            }
            let want = match args.get("port_state").and_then(|v| v.as_str()) {
                None => PortState::Listening,
                Some(s) => match PortState::parse(s) {
                    Some(w) => w,
                    None => {
                        return Invocation::failed(
                            format!("`{s}` is not a port state"),
                            "there are two: `listening` (something is accepting \
                             connections) and `closed` (nothing is). Nothing was \
                             declared.",
                        );
                    }
                },
            };
            Watch::Port {
                port: port as u16,
                want,
            }
        };

        // T24 requirement 1: an owner, and the default is the session.
        let owner_arg = args.get("owner").and_then(|v| v.as_str());
        let owner = match owner_arg {
            None => match host.scope_for(ScopeKind::Session, None) {
                Ok(s) => s,
                Err(e) => return Invocation::failed("the monitor has no owner", e.to_string()),
            },
            Some(o) => match super::jobs::resolve_scope(host, o) {
                Some(s) => s,
                // An owner must be a scope that ALREADY exists. A monitor that
                // could mint its own `explicit` scope would be a watcher that
                // outlives the session and that nothing created deliberately —
                // the leak with an extra step, which is why `bash` refuses an
                // unnamed explicit scope too.
                None => return super::jobs::unknown_scope(host, o),
            },
        };

        // Clause 1 before the declaration, not after: a port that is ALREADY in the
        // wanted state produces a monitor that fires on its first tick, and the
        // model then reads a firing as news. The harness can see this and the model
        // cannot, so it says so.
        if let Watch::Port { port, want } = &watch {
            let now = crate::exec::host::port_is_listening(*port);
            if (now && *want == PortState::Listening) || (!now && *want == PortState::Closed) {
                notes.push(format!(
                    "port {port} is ALREADY {}, so this monitor fires on its first \
                     look. That is not news about a change — it is the state as it \
                     was when you declared the watch.",
                    want.as_str()
                ));
            }
            // What the harness knows that the model cannot: which pid holds it.
            if let Some(pid) = crate::exec::host::listener_pid(*port)
                && let Some(p) = host.protected().into_iter().find(|p| p.pid == pid)
            {
                notes.push(format!(
                    "port {port} is held by pid {pid}, which this harness manages: \
                     {}. Watching it is fine; stopping it is not something this \
                     session does.",
                    p.why
                ));
            }
        }

        let declared_by = format!("turn {}", ctx.turn_id());
        let repeat = args
            .get("repeat")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        match monitors.declare(
            name,
            owner.clone(),
            watch.clone(),
            job_handle,
            &declared_by,
            ttl,
            repeat,
        ) {
            Ok(m) => {
                let mut inv = Invocation::ok(format!(
                    "`{name}` is watching {}.\n  owner:   {} — {}\n  ttl:     {:.0}s\n  \
                     declared by: {declared_by}\n\nIt watches BETWEEN turns and does \
                     not block this one. `job_list` shows it, and shows why it fired \
                     once it has. It ends in exactly one of four ways: it fires, its \
                     ttl passes without firing, you retire it, or `{}` ends and takes \
                     it. Renew it with action=\"renew\" if its reason outlasts the \
                     ttl.",
                    m.watch.describe(),
                    owner,
                    owner.kind.reaped_when(),
                    ttl.as_secs_f32(),
                    owner,
                ));
                for n in notes {
                    inv = inv.with_note(n);
                }
                inv
            }
            Err(e) if matches!(*e, MonitorError::NameTaken { .. }) => {
                Invocation::failed(format!("`{name}` is taken"), e.to_string())
            }
            Err(e) => Invocation::failed(format!("`{name}` was not declared"), e.to_string()),
        }
    }
}

/// Whether a relative path stays inside the workspace, decided lexically.
///
/// The same rule [`crate::runtime::GateCall::path_is_inside`] applies, and for
/// the same reason it is lexical there: this has to be answerable about a path
/// that does not exist, because *"tell me when this file appears"* is the most
/// ordinary thing a path monitor is for.
fn inside(path: &str) -> bool {
    if path.starts_with('/') {
        return false;
    }
    let mut depth: i32 = 0;
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => depth += 1,
        }
    }
    depth > 0
}

/// **The refusal of a second watch on a pull request already watched** — naming the watch that
/// has it, and the one `monitor` call that covers both its conditions and these.
fn already_watched(
    monitors: &crate::exec::monitor::Monitors,
    w: &Watch,
    name: &str,
    args: &Value,
) -> Option<Invocation> {
    let (target, mine) = w.github()?;
    let (held, theirs) = monitors.list().into_iter().find_map(|m| {
        let (t, theirs) = m.watch.github()?;
        (t == target && m.name != name).then(|| (m.name.clone(), theirs))
    })?;
    let mut all: Vec<&str> = theirs.clone();
    for u in &mine {
        if !all.contains(u) {
            all.push(u);
        }
    }
    let mut github = args.get("github").cloned().unwrap_or_default();
    github["until"] = serde_json::json!(all);
    let arm = serde_json::json!({"name": held, "github": github, "repeat": true});
    let retire = serde_json::json!({"name": held, "action": "retire"});
    Some(Invocation::failed(
        format!(
            "`{target}` is already watched, by `{held}` ({}) — nothing was declared",
            theirs.join(", ")
        ),
        format!(
            "one watch serves several conditions with one look, so a pull request gets one \
             watch. To watch {} together: retire `{held}` with `monitor {retire}`, then arm \
             `monitor {arm}`. Or leave `{held}` as it is.",
            all.join(", ")
        ),
    ))
}

/// **A `github` watch, read** — `{"url": "https://github.com/o/r/pull/12", "until": "checks"}`, or
/// `repo` with `pr` or `issue` in place of the URL; `every_s` how often to look. See
/// `exec::github`.
fn github_watch(g: &Value) -> Result<Watch, Box<Invocation>> {
    use crate::exec::github::{EVERY, EVERY_MIN, GithubCondition, Target, Until};
    const SHAPE: &str = "give `github` as {\"url\": \"https://github.com/OWNER/REPO/pull/N\", \
                         \"until\": \"…\"} (an issue URL works too), or {\"repo\": \"OWNER/REPO\", \
                         \"pr\": N} / {\"repo\": …, \"issue\": N} with `until`. `until` is one of, \
                         or a list of, `closed`, `merged`, `checks`, `review`, `comment`, \
                         `coderabbit`. Nothing was declared.";
    let target = if let Some(url) = g.get("url").and_then(|u| u.as_str()) {
        Target::from_url(url).ok_or_else(|| {
            Box::new(Invocation::failed(
                format!("`{url}` is not a pull request or issue URL"),
                SHAPE,
            ))
        })?
    } else {
        let repo = g.get("repo").and_then(|r| r.as_str()).unwrap_or("").trim();
        let (number, pr) = match (
            g.get("pr").and_then(|n| n.as_u64()),
            g.get("issue").and_then(|n| n.as_u64()),
        ) {
            (Some(n), None) => (n, true),
            (None, Some(n)) => (n, false),
            _ => {
                return Err(Box::new(Invocation::failed(
                    "`github` needs one of `pr` or `issue`",
                    SHAPE,
                )));
            }
        };
        if repo.split('/').filter(|p| !p.is_empty()).count() != 2 {
            return Err(Box::new(Invocation::failed(
                format!("`{repo}` is not OWNER/REPO"),
                SHAPE,
            )));
        }
        Target {
            repo: repo.to_string(),
            number,
            pr,
        }
    };
    // **One `until`, or several**: one look at a pull request answers all of them, so a
    // `["checks", "review", "closed"]` is one watch and one `gh` call rather than three.
    let words: Vec<&str> = match g.get("until") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).collect(),
        _ => Vec::new(),
    };
    let mut untils: Vec<Until> = Vec::new();
    for w in &words {
        let Some(u) = Until::parse(w) else {
            return Err(Box::new(Invocation::failed(
                format!("`{w}` is not something a GitHub watch waits for"),
                SHAPE,
            )));
        };
        if !untils.contains(&u) {
            untils.push(u);
        }
    }
    if untils.is_empty() {
        return Err(Box::new(Invocation::failed(
            "`github` needs `until`",
            SHAPE,
        )));
    }
    if let Some(u) = untils.iter().find(|u| u.needs_pr()).filter(|_| !target.pr) {
        return Err(Box::new(Invocation::failed(
            format!("an issue has no {}", u.word()),
            "`merged`, `checks`, `review` and `coderabbit` are a pull request's; an issue can \
             be watched for `closed` or `comment`. Nothing was declared.",
        )));
    }
    let every = g
        .get("every_s")
        .and_then(|s| s.as_u64())
        .map(Duration::from_secs)
        .unwrap_or(EVERY)
        .max(EVERY_MIN);
    Ok(Watch::Custom(CustomWatch(GithubCondition::new(
        target, untils, every,
    ))))
}

/// The TTL, bounded — and **refused rather than clamped** when it is over.
///
/// Clamping would leave the caller believing a number the monitor does not have,
/// which is the silent-rewrite failure clause 1 forbids. The refusal names the
/// cap so the retry succeeds.
fn ttl_of(args: &Value) -> Result<Duration, Box<Invocation>> {
    ttl_within(args, DEFAULT_TTL, MAX_TTL)
}

/// [`ttl_of`] for a watch with caps of its own — a GitHub watch lives for days
/// (`exec::github`).
fn ttl_within(args: &Value, default: Duration, cap: Duration) -> Result<Duration, Box<Invocation>> {
    let Some(ms) = args.get("ttl_ms").and_then(|v| v.as_u64()) else {
        return Ok(default);
    };
    let d = Duration::from_millis(ms.max(1));
    if d > cap {
        return Err(Box::new(Invocation::failed(
            format!("a ttl of {:.0}s is past the cap", d.as_secs_f32()),
            format!(
                "the cap is {:.0}s and nothing was declared. A monitor is bounded so \
                 that one whose reason has passed dies without anybody remembering \
                 it, and a ttl nobody caps is not a bound. Ask for {} ms or less, and \
                 renew it with action=\"renew\" if the reason is still live.",
                cap.as_secs_f32(),
                cap.as_millis()
            ),
        )));
    }
    Ok(d)
}

/// Clause 1 for a monitor name that is not there: what IS watching, and what has
/// already settled — because a name that fired a minute ago is the most likely
/// thing somebody is asking about.
fn unknown_monitor(host: &dyn ProcessHost, asked: &str) -> Invocation {
    let Some(monitors) = host.monitors() else {
        return Invocation::failed(format!("no monitor called `{asked}`"), String::new());
    };
    let live = monitors.list();
    let mut body = if live.is_empty() {
        "nothing is watching right now.".to_string()
    } else {
        let mut s = format!("{} monitor(s) watching:\n", live.len());
        for m in &live {
            s.push_str(&format!(
                "  {} — {} — owned by {}\n",
                m.name,
                m.watch.describe(),
                m.owner
            ));
        }
        s
    };
    if let Some(done) = monitors.firings().into_iter().find(|f| f.name == asked) {
        body.push_str(&format!(
            "\n`{asked}` has already ended: {}. It is in `job_list`'s record.\n",
            done.fired.word()
        ));
    }
    Invocation::failed(format!("no monitor called `{asked}` is watching"), body)
}

/// The scopes and jobs a monitor could name, for `EXPLAIN`-style callers.
pub fn watchable(host: &dyn ProcessHost) -> (Vec<JobId>, Vec<ScopeId>) {
    (
        host.jobs().into_iter().map(|j| j.id).collect(),
        host.scopes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The deliverable this module exists for.**
    ///
    /// If this fails, somebody has added the argument that brings the whole
    /// failure mode back — and it comes back as `pkill -f` with a JSON wrapper,
    /// which is how a running command got killed mid-flight on this box.
    #[test]
    fn no_monitor_tool_grows_a_pattern_argument() {
        let s = Monitor.schema();
        let names = s.param_names().join(",");
        for banned in [
            "pattern",
            "match",
            "cmdline",
            // `command` is now a legitimate condition — run a command and fire on
            // its exit — not a process-name pattern. The rest stay banned.
            "name_regex",
            "grep",
            "regex",
            "predicate",
            "until",
            "shell",
            // `process` is no longer banned, and the reason it was is the reason it
            // is not: the hazard was a pattern matched by a SHELL against output
            // that included the shell. `process` is resolved by the in-process
            // finder (`exec::procs::find`), which removes this daemon, its
            // ancestors and its protected pids before matching, and the monitor
            // then holds (pid, start time) handles — the pattern is never polled.
            // Not a pattern, but the same shape of mistake: a host argument turns
            // a loopback check into an arbitrary network reach, and then every
            // monitor call has to be network-gated.
            "host",
            "url",
        ] {
            assert!(
                !names.contains(banned),
                "`monitor` grew a `{banned}` argument. A condition written as a \
                 pattern matches the process evaluating it — measured seven times in \
                 one session on this box, the seventh killing a running command. A \
                 monitor keys on a handle or a cgroup, never a pattern.",
            );
        }
    }

    #[test]
    fn monitor_declares_exec_and_not_network_and_lints_clean() {
        let s = Monitor.schema();
        // Exec: it leaves something watching after the turn.
        assert_eq!(s.access, Access::Exec);
        assert!(!s.access.is_unattended(), "a monitor must reach the gate");
        // NOT network: the port watch is loopback-only and has no host argument,
        // which is what keeps a cgroup watch from being network-gated.
        assert_ne!(s.access, Access::Network);
        assert_eq!(crate::schema::lint_description(&s.description), vec![]);
    }

    /// **The description names every condition the schema takes** (R93.F28): it listed
    /// six, and said there was "deliberately no way to watch a shell command" beside a
    /// `command` property.
    #[test]
    fn the_description_names_every_condition_in_the_schema() {
        let s = Monitor.schema();
        for k in [
            "job", "scope", "path", "port", "timer_ms", "command", "pid", "process", "github",
        ] {
            assert!(s.param_names().contains(&k), "schema lacks `{k}`");
            assert!(
                s.description.contains(&format!("`{k}`")),
                "description does not name `{k}`"
            );
        }
        assert!(!s.description.contains("no way to watch a shell command"));
    }

    #[test]
    fn a_ttl_over_the_cap_is_refused_and_names_the_cap() {
        let over = serde_json::json!({"ttl_ms": MAX_TTL.as_millis() as u64 + 1});
        let e = ttl_of(&over).unwrap_err();
        assert!(e.payload.contains("cap is"), "{}", e.payload);
        assert!(e.payload.contains("nothing was declared"), "{}", e.payload);
        // And the default is what an absent argument gets, not the cap.
        assert_eq!(ttl_of(&serde_json::json!({})).unwrap(), DEFAULT_TTL);
    }

    /// **A `github` watch is read from a URL or a repo and number**, refuses what an issue cannot
    /// have, and lives within its own week rather than the ordinary hour.
    #[test]
    fn a_github_watch_is_read_and_bounded_by_its_own_cap() {
        use crate::exec::github::{GITHUB_DEFAULT_TTL, GITHUB_MAX_TTL};
        let w = github_watch(&serde_json::json!({
            "url": "https://github.com/stroppy-io/stroppy/pull/412", "until": "checks"
        }))
        .unwrap();
        assert_eq!(
            w.describe(),
            "pull request stroppy-io/stroppy#412 until its checks finishing"
        );
        assert_eq!(w.max_ttl(), GITHUB_MAX_TTL);
        let w = github_watch(&serde_json::json!({"repo": "o/r", "issue": 7, "until": "comment"}))
            .unwrap();
        assert_eq!(w.describe(), "issue o/r#7 until a new comment");
        let e = github_watch(&serde_json::json!({"repo": "o/r", "issue": 7, "until": "checks"}))
            .unwrap_err();
        assert!(e.payload.contains("pull request's"), "{}", e.payload);
        assert!(
            github_watch(&serde_json::json!({"url": "https://github.com/o/r/pull/1"})).is_err()
        );
        assert!(
            github_watch(&serde_json::json!({"repo": "nope", "pr": 1, "until": "closed"})).is_err()
        );
        let day = ttl_within(&serde_json::json!({}), GITHUB_DEFAULT_TTL, GITHUB_MAX_TTL).unwrap();
        assert_eq!(day, GITHUB_DEFAULT_TTL);
        let two_days = serde_json::json!({"ttl_ms": 2 * 24 * 3600 * 1000u64});
        assert!(ttl_within(&two_days, GITHUB_DEFAULT_TTL, GITHUB_MAX_TTL).is_ok());
        assert!(
            ttl_of(&two_days).is_err(),
            "the ordinary cap still holds for other watches"
        );
    }

    /// **A pull request already watched gets no second watch** — the refusal names the one that
    /// has it and gives the single call for both: stroppy armed nine watches for three pull
    /// requests, three conditions apiece.
    #[test]
    fn a_second_watch_on_a_watched_pull_request_is_refused_with_the_one_call() {
        use crate::exec::monitor::{DEFAULT_TTL, Monitors};
        use crate::exec::scope::{ScopeId, ScopeKind};
        let ms = std::sync::Arc::new(Monitors::new());
        let scope = ScopeId {
            kind: ScopeKind::Session,
            name: "s".into(),
            path: std::env::temp_dir(),
        };
        let first = github_watch(&serde_json::json!({
            "repo": "stroppy-io/stroppy", "pr": 226, "until": "review"
        }))
        .unwrap();
        ms.declare(
            "pr226-review",
            scope,
            first,
            None,
            "turn 1",
            DEFAULT_TTL,
            false,
        )
        .unwrap();
        let args = serde_json::json!({
            "name": "pr226-checks",
            "github": {"repo": "stroppy-io/stroppy", "pr": 226, "until": ["checks", "closed"]}
        });
        let second = github_watch(&args["github"]).unwrap();
        let inv = already_watched(&ms, &second, "pr226-checks", &args).expect("refused");
        assert!(
            inv.payload
                .contains("is already watched, by `pr226-review` (review)")
                || inv.payload.contains("pr226-review"),
            "{}",
            inv.payload
        );
        assert!(
            inv.payload
                .contains(r#""until":["review","checks","closed"]"#),
            "the one call covers all of it: {}",
            inv.payload
        );
        let other = github_watch(&serde_json::json!({
            "repo": "stroppy-io/stroppy", "pr": 227, "until": "checks"
        }))
        .unwrap();
        assert!(
            already_watched(&ms, &other, "pr227", &serde_json::json!({})).is_none(),
            "another pull request is its own watch"
        );
    }
}
