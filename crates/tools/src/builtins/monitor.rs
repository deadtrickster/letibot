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

use crate::exec::monitor::{DEFAULT_TTL, MAX_TTL, MonitorError, PortState, Watch};
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
             one monitor may hold a name at a time) and exactly one of `job` (a job \
             leaving the running state), `scope` (a cgroup emptying), `path` (a file \
             or directory appearing, vanishing, or changing size or modification \
             time) or `port` (a loopback TCP port becoming listenable, or with \
             `port_state: \"closed\"`, stopping). Optionally `owner` to say which \
             scope reaps it (`turn`, `session` — the default — or a named scope that \
             already exists) and `ttl_ms`, which is capped; `action: \"renew\"` \
             extends it and `action: \"retire\"` ends it. `job_list` shows every \
             monitor, who declared it, how long it has left, and for the ones that \
             ended, which of the four endings it was and why. There is deliberately \
             no way to watch a shell command or a process name: such a condition \
             matches the shell evaluating it, which has killed a running command on \
             this box, and a job id or a cgroup cannot.",
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
                    "owner": {"type": "string", "description": "Which scope reaps this monitor: `turn`, `session` (the default), or the name of a scope that already exists."},
                    "ttl_ms": {"type": "integer", "description": "How long it watches before it expires without firing. Capped; a larger value is refused rather than quietly reduced."}
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
                let Some(m) = monitors.retire(name, "the model") else {
                    return unknown_monitor(host, name);
                };
                return Invocation::ok(format!(
                    "retired `{name}`, which was watching {}.\n  declared by: {}\n  \
                     it had {} left\n\nIt never fired: retiring is not the condition \
                     happening, and nothing above is evidence about {}.",
                    m.watch.describe(),
                    m.declared_by,
                    m.settled()
                        .map(|f| f.word())
                        .unwrap_or_else(|| "an unknown amount of time".into()),
                    m.watch.describe(),
                ));
            }
            "renew" => {
                let ttl = match ttl_of(args) {
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
                    Err(e) => Invocation::failed(format!("`{name}` was not renewed"), e.to_string()),
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

        let ttl = match ttl_of(args) {
            Ok(t) => t,
            Err(inv) => return *inv,
        };

        // Exactly one condition. Two would be one monitor with one firing for two
        // questions, which is `job_wait`'s job/scope rule and the same reasoning:
        // a single answer to two questions tells you neither.
        let asked: Vec<&str> = ["job", "scope", "path", "port"]
            .into_iter()
            .filter(|k| args.get(*k).is_some())
            .collect();
        if asked.len() != 1 {
            return Invocation::failed(
                if asked.is_empty() {
                    "monitor needs something to watch".to_string()
                } else {
                    format!("monitor takes one condition, and {} were given", asked.len())
                },
                "give exactly one of `job` (a job id), `scope` (a cgroup), `path` (a \
                 file or directory) or `port` (a loopback TCP port). There is \
                 deliberately no argument for a shell command or a process name: a \
                 condition written as a pattern matches the process evaluating it, \
                 and a handle cannot. Nothing was declared.",
            );
        }

        let mut job_handle = None;
        let watch = if let Some(id) = args.get("job").and_then(|v| v.as_str()) {
            let jid = JobId(id.to_string());
            // Resolved to a live handle here, so a monitor can never be declared
            // against an id that does not exist — and so the refusal is clause 1's
            // listing rather than a monitor that watches nothing forever.
            let Some(h) = host.job_handle(&jid) else {
                return super::jobs::unknown_job(host, id);
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
        let mut notes: Vec<String> = Vec::new();
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
        match monitors.declare(
            name,
            owner.clone(),
            watch.clone(),
            job_handle,
            &declared_by,
            ttl,
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

/// The TTL, bounded — and **refused rather than clamped** when it is over.
///
/// Clamping would leave the caller believing a number the monitor does not have,
/// which is the silent-rewrite failure clause 1 forbids. The refusal names the
/// cap so the retry succeeds.
fn ttl_of(args: &Value) -> Result<Duration, Box<Invocation>> {
    let Some(ms) = args.get("ttl_ms").and_then(|v| v.as_u64()) else {
        return Ok(DEFAULT_TTL);
    };
    let d = Duration::from_millis(ms.max(1));
    if d > MAX_TTL {
        return Err(Box::new(Invocation::failed(
            format!("a ttl of {:.0}s is past the cap", d.as_secs_f32()),
            format!(
                "the cap is {:.0}s and nothing was declared. A monitor is bounded so \
                 that one whose reason has passed dies without anybody remembering \
                 it, and a ttl nobody caps is not a bound. Ask for {} ms or less, and \
                 renew it with action=\"renew\" if the reason is still live.",
                MAX_TTL.as_secs_f32(),
                MAX_TTL.as_millis()
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
    if let Some(done) = monitors.history().into_iter().find(|m| m.name == asked) {
        body.push_str(&format!(
            "\n`{asked}` has already ended: {}. It is in `job_list`'s record.\n",
            done.settled().map(|f| f.word()).unwrap_or_default()
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
            "command",
            "name_regex",
            "grep",
            "regex",
            "predicate",
            "until",
            "shell",
            "process",
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

    #[test]
    fn a_ttl_over_the_cap_is_refused_and_names_the_cap() {
        let over = serde_json::json!({"ttl_ms": MAX_TTL.as_millis() as u64 + 1});
        let e = ttl_of(&over).unwrap_err();
        assert!(e.payload.contains("cap is"), "{}", e.payload);
        assert!(e.payload.contains("nothing was declared"), "{}", e.payload);
        // And the default is what an absent argument gets, not the cap.
        assert_eq!(ttl_of(&serde_json::json!({})).unwrap(), DEFAULT_TTL);
    }
}
