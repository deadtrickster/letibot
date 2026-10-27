//! `scenario` — **work on several hosts together**: roles that must come up, a run that uses them,
//! and a teardown that always happens (RH8).
//!
//! `docs/remote-hosts.md`: *"Roles, each `{host, command, ready, after}`: `ready` is a readiness
//! check (a port that accepts, a log line, a command that exits 0) with its own deadline; `after`
//! orders the starts; a role's command may name another's facts (`${server.host}`,
//! `${server.port}`). Started in order, each waited ready; the run's own command (the client, the
//! test) runs when all are ready; on its end, on any role failing, or on an interrupt, **everything
//! is torn down on every host** and the result is per role. docker-compose across machines, without
//! containers."*
//!
//! # The three things that make a service fixture different from a fanout
//!
//! 1. **A start is not a service.** So every role may carry a readiness check, polled **on the host
//!    that runs it** — a port that accepts there, a line in its own log, a client program that
//!    exits 0 — with a deadline of its own. Three spellings because those are the three a fixture
//!    has: `pg_isready`, `Started` in a log, a port.
//! 2. **Order is a dependency, not a sequence.** `after: ["db"]` means *this role starts when `db`
//!    is READY*, which is the only order that means anything across machines — a client that
//!    connected to a started-but-not-listening server is the flake this removes. A cycle is refused
//!    before anything starts.
//! 3. **The teardown is not the caller's job.** On the run's end, on a role that never becomes
//!    ready, on a role that dies, and on any early return, every started process is ended on its own
//!    host — and each one is a `process` record, so a session close or a daemon restart has the same
//!    list (RH7's teardown-follows-ownership rule).
//!
//! # What the row says
//!
//! Per role: did it start, **how long until it was ready**, how it ended, and the tail of its
//! output. A role that never became ready and one that was torn down after a good run are different
//! rows, and the difference is the sentence, not a missing line.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::exec::ProcessHost;
use crate::fleet::{self, Cell, Fate, Fleet, LOCAL, Leg, ProcessBook, Ready, Running};
use crate::lease::Blocks;
use crate::remote::Paths;
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The name this tool is registered and seated by.
pub const TOOL: &str = "scenario";

/// **How long a role may take to become ready when it does not say.** Thirty seconds: long enough
/// for a database's first initialisation on a cold box, short enough that a typo in a readiness
/// check is a sentence and not a turn.
pub const DEFAULT_READY: Duration = Duration::from_secs(30);

/// **How long the whole scenario may run when nobody says.** Five minutes — the roles' own
/// deadlines and the run's bound everything inside it anyway; this is the outer wall.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(300);

/// **The longest wall this tool accepts**, for `fanout`'s reason: the model waits for this call.
pub const MAX_DEADLINE: Duration = Duration::from_secs(3600);

/// The label a run's row carries, refused as a role name so two cells cannot share a row.
const RUN: &str = "run";

/// One role, as the call asked for it.
#[derive(Debug, Clone)]
struct Role {
    name: String,
    host: String,
    command: String,
    cwd: Option<String>,
    ready: Option<Ready>,
    after: Vec<String>,
    /// When this role must be gone. `None` is the whole scenario's own wall.
    deadline: Option<Duration>,
    ready_deadline: Duration,
}

/// The call as a plan: roles in **start order**, and the run that uses them.
#[derive(Debug, Clone)]
struct Planned {
    roles: Vec<Role>,
    run: Option<Role>,
    /// The whole call's wall — what every leg's deadline is capped by.
    until: Instant,
}

/// `scenario` — roles, a run, and a teardown that always happens.
pub struct Scenario {
    file: Option<PathBuf>,
    paths: Option<Paths>,
    ssh: Option<PathBuf>,
    bound: Option<Duration>,
    book: Arc<dyn ProcessBook>,
    /// **The session's leases** (RH9), when it has any. `None` is a run with no store behind it:
    /// an explicit `ready: {"port": N}` still works, and `ready: {"port": "lease"}` is refused
    /// with a sentence rather than started against port 0.
    blocks: Option<Arc<Blocks>>,
}

impl Default for Scenario {
    fn default() -> Self {
        Scenario {
            file: None,
            paths: None,
            ssh: None,
            bound: None,
            book: Arc::new(fleet::NoBook),
            blocks: None,
        }
    }
}

impl Scenario {
    pub fn new() -> Scenario {
        Scenario::default()
    }

    pub fn with_file(mut self, file: impl Into<PathBuf>) -> Scenario {
        self.file = Some(file.into());
        self
    }

    pub fn with_paths(mut self, paths: Paths) -> Scenario {
        self.paths = Some(paths);
        self
    }

    /// Run this program instead of `ssh` — a test's fake, which never touches a network.
    pub fn with_ssh(mut self, program: impl Into<PathBuf>) -> Scenario {
        self.ssh = Some(program.into());
        self
    }

    pub fn with_bound(mut self, bound: Duration) -> Scenario {
        self.bound = Some(bound);
        self
    }

    /// **Where the started processes are written down** — the daemon binds this to its session.
    pub fn with_book(mut self, book: Arc<dyn ProcessBook>) -> Scenario {
        self.book = book;
        self
    }

    /// **The session's leases** (RH9) — what a role's `ready: {"port": "lease"}` takes its port
    /// from, and what the roles' containers are named under so the teardown can find them.
    pub fn with_leases(mut self, blocks: Arc<Blocks>) -> Scenario {
        self.blocks = Some(blocks);
        self
    }

    /// The fleet this call uses: the test's doors over the real thing.
    pub fn fleet(&self, work: impl Into<String>) -> Fleet {
        let mut fleet = Fleet::new(work).with_book(Arc::clone(&self.book));
        if let Some(file) = &self.file {
            fleet = fleet.with_file(file.clone());
        }
        if let Some(paths) = &self.paths {
            fleet = fleet.with_paths(paths.clone());
        }
        if let Some(ssh) = &self.ssh {
            fleet = fleet.with_ssh(ssh.clone());
        }
        if let Some(bound) = self.bound {
            fleet = fleet.with_bound(bound);
        }
        fleet
    }
}

impl Tool for Scenario {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            TOOL,
            "Bring up several services on several hosts, run one client/test against them, then tear \
             everything down. `roles` maps a name to `{host, command, ready?, after?, deadline_s?}`. \
             `ready` is ONE of `{port: N}`, `{log: \"<regex>\"}` on the role's own output, or \
             `{check: \"<cmd>\"}` exiting 0, polled on that role's host (default 30 s). `after: \
             [names]` starts a role only once those are READY; a cycle is refused before anything \
             starts. `run` is `{command, host?, deadline_s?}` — the client — and may use \
             `${role.host}` and `${role.port}`, as may a role's command. Teardown is not optional: \
             the run's end, a role that never comes up, or a refusal ends every started process on \
             its host, each a supervised record. One row per role and one for the run.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "roles": {
                        "type": "object",
                        "description": "Role name → {host, command, ready?, after?, deadline_s?, ready_deadline_s?, cwd?}. `host` is a `hosts.toml` name or `local`.",
                        "additionalProperties": {
                            "type": "object",
                            "properties": {
                                "host": {"type": "string", "description": "A `hosts.toml` name, or `local` for this session's own box."},
                                "command": {"type": "string", "description": "What starts the service. May use `${other.host}` and `${other.port}`."},
                                "ready": {
                                    "description": "One check: `{port: N}`, `{log: \"<regex>\"}` on the role's own output, or `{check: \"<cmd>\"}`.",
                                    "type": "object"
                                },
                                "after": {
                                    "description": "Role names that must be READY before this one starts.",
                                    "type": "array",
                                    "items": {"type": "string"}
                                },
                                "deadline_s": {"type": "integer", "description": "Seconds this role may run before it is ended."},
                                "ready_deadline_s": {"type": "integer", "description": "Seconds its readiness check may take. Default 30."},
                                "cwd": {"type": "string", "description": "Where its command runs: absolute, or relative to this call's work directory on that host."}
                            },
                            "required": ["host", "command"]
                        }
                    },
                    "run": {
                        "type": "object",
                        "description": "The client or the test: {command, host?, deadline_s?}. `host` defaults to `local`.",
                        "properties": {
                            "command": {"type": "string", "description": "What runs once every role is ready. May use `${role.host}` and `${role.port}`."},
                            "host": {"type": "string", "description": "A `hosts.toml` name, or `local` (the default)."},
                            "deadline_s": {"type": "integer", "description": "Seconds the run may take before it is ended."},
                            "cwd": {"type": "string", "description": "Where it runs."}
                        },
                        "required": ["command"]
                    },
                    "deadline_s": {"type": "integer", "description": "Seconds the whole scenario may run. Default 300, at most 3600."}
                },
                "required": ["roles"]
            }),
            // Starts services on other machines under the session's own gate — one card for the
            // whole scenario, whose arguments name every host it reaches and every command.
            Access::Exec,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let work = format!(
            "scenario-{}",
            ctx.call_id()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                })
                .collect::<String>()
        );
        self.answer(args, ctx.backend.processes_arc(), &work)
    }
}

impl Scenario {
    /// **The whole tool, without a call context** — the daemon's path is [`Tool::invoke`], and this
    /// is the same thing with the two facts a `ctx` would have supplied: this session's own backend
    /// (for a `local` role) and the work directory name. It is what a test drives.
    pub fn answer(
        &self,
        args: &Value,
        local: Option<Arc<dyn ProcessHost>>,
        work: &str,
    ) -> Invocation {
        let mut plan = match plan(args) {
            Ok(p) => p,
            Err((reason, body)) => return Invocation::failed(reason, body),
        };
        // **The lease, before anything is substituted or started** (RH9). A role that asked for
        // `ready: {"port": "lease"}` gets its port here, one block per host, so every `${role.port}`
        // in the call resolves to a number that is this session's own — and a host that has no
        // block left is a refusal that names it rather than a service on a port somebody else holds.
        let mut held: Vec<letibot_tokencore::store::Lease> = Vec::new();
        if let Some(blocks) = &self.blocks {
            // `assign` puts back whatever it had already taken when it cannot take the rest, so a
            // plan that never ran holds nothing.
            match assign(blocks, &mut plan) {
                Ok(taken) => held = taken,
                Err((reason, body)) => return Invocation::failed(reason, body),
            }
        } else if plan
            .roles
            .iter()
            .chain(plan.run.iter())
            .any(|r| matches!(r.ready, Some(Ready::Lease)))
        {
            return Invocation::failed(
                "`ready: {\"port\": \"lease\"}` and this run has no session store",
                "A port from a lease is a row in the session store, and this run has none behind \
                 it — so there is nothing to hold a block in and nothing for a teardown to release. \
                 Give the port yourself: `ready: {\"port\": 5432}`.",
            );
        }
        // **A `${…}` that names a role this call does not have, or a fact that role does not have,
        // is refused NOW** — a substitution that failed at start time would be a service starting
        // with a literal `$` in its command. It happens after the lease because a lease port is a
        // fact `${role.port}` resolves to.
        let facts = facts_of(&plan);
        for role in plan.roles.iter().chain(plan.run.iter()) {
            if let Err(e) = substitute(&role.command, &facts) {
                return Invocation::failed(format!("`{}`: {e}", role.name), String::new());
            }
            if let Some(cwd) = &role.cwd
                && let Err(e) = substitute(cwd, &facts)
            {
                return Invocation::failed(format!("`{}`: {e}", role.name), String::new());
            }
        }
        let fleet = self.fleet(work.to_string()).with_local(local);
        run(&fleet, &plan, self.blocks.as_deref(), &held, &facts)
    }
}

/// **The call as a plan**, or the refusal that names what to fix. Everything a malformed call can
/// get wrong is refused here, before a single process is started: that is the difference between a
/// refusal and a teardown.
fn plan(args: &Value) -> Result<Planned, (String, String)> {
    let deadline = match seconds(args.get("deadline_s")) {
        Ok(Some(0)) => {
            return Err((
                "`deadline_s` is 0".into(),
                "A wall of zero seconds would tear the scenario down as it started. Leave it out \
                 for the default (300 s)."
                    .into(),
            ));
        }
        Ok(Some(s)) if s > MAX_DEADLINE.as_secs() => {
            return Err((
                format!(
                    "`deadline_s` is {s}, past the {}-second ceiling",
                    MAX_DEADLINE.as_secs()
                ),
                "A scenario is a call in a turn. A fixture that must outlive the turn is a \
                 `fanout` of long jobs or a `bash` in the background, not a scenario."
                    .into(),
            ));
        }
        Ok(Some(s)) => Duration::from_secs(s),
        Ok(None) => DEFAULT_DEADLINE,
        Err(e) => return Err((e, "`deadline_s` is a whole number of seconds.".into())),
    };
    let until = Instant::now() + deadline;

    let Some(roles) = args.get("roles").and_then(|v| v.as_object()) else {
        return Err((
            "`roles` is missing or is not an object".into(),
            "Give the services to bring up as `roles: {\"db\": {\"host\": \"lab2x1\", \"command\": \
             \"postgres …\", \"ready\": {\"port\": 5432}}}`."
                .into(),
        ));
    };
    if roles.is_empty() {
        return Err((
            "`roles` is empty".into(),
            "A scenario with no roles has nothing to bring up — give it at least one service, or \
             use `bash` for a single command."
                .into(),
        ));
    }

    let mut named: BTreeMap<String, Role> = BTreeMap::new();
    // **The order the caller wrote them in**, which is the only tie-break for roles that do not wait
    // on each other. A `BTreeMap`'s own order is alphabetical, and a scenario whose rows came back
    // sorted rather than started would be a plan the model did not write. `serde_json` keeps a
    // call's key order (`preserve_order`), so this is the call's order and not this build's.
    let mut declared: Vec<String> = Vec::with_capacity(roles.len());
    for (name, spec) in roles {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err((
                "a role has an empty name".into(),
                "Every role is named: the name is what `after` and `${name.port}` refer to, and \
                 what its row is labelled."
                    .into(),
            ));
        }
        if name == RUN {
            return Err((
                format!("a role is called `{RUN}`"),
                "`run` is the row the client's own command answers with, so a role of that name \
                 would have its row overwritten. Rename the role."
                    .into(),
            ));
        }
        let Some(spec) = spec.as_object() else {
            return Err((
                format!("role `{name}` is not an object"),
                "Each role is `{host, command, ready?, after?, deadline_s?}`.".into(),
            ));
        };
        let host = spec
            .get("host")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                (
                    format!("role `{name}` has no `host`"),
                    format!(
                        "Every role says where it runs: a `hosts.toml` name, or `{LOCAL}` for this \
                         session's own box. `hosts` shows the declared names."
                    ),
                )
            })?
            .to_string();
        let command = spec
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                (
                    format!("role `{name}` has no `command`"),
                    "Every role names the command that starts it — the service, not its readiness \
                     check (that is `ready`)."
                        .into(),
                )
            })?
            .to_string();
        let ready = match spec.get("ready") {
            None | Some(Value::Null) => None,
            Some(v) => match Ready::parse(v) {
                Ok(r) => Some(r),
                Err(e) => {
                    return Err((
                        format!("role `{name}`'s readiness check is not one"),
                        format!(
                            "{e}. Give `ready: {{\"port\": 5432}}`, `ready: {{\"log\": \
                                 \"listening\"}}` or `ready: {{\"check\": \"pg_isready\"}}`."
                        ),
                    ));
                }
            },
        };
        let after = match spec.get("after") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(one)) => vec![one.trim().to_string()],
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            Some(other) => {
                return Err((
                    format!("role `{name}`'s `after` is `{other}`"),
                    "`after` is a list of role names that must be ready first, e.g. `after: \
                     [\"db\"]`."
                        .into(),
                ));
            }
        };
        let deadline_role = match seconds(spec.get("deadline_s")) {
            Ok(Some(0)) => None,
            Ok(Some(s)) => Some(Duration::from_secs(s)),
            Ok(None) => None,
            Err(e) => return Err((format!("role `{name}`: {e}"), String::new())),
        };
        let ready_deadline = match seconds(spec.get("ready_deadline_s")) {
            Ok(Some(0)) => {
                return Err((
                    format!("role `{name}`'s `ready_deadline_s` is 0"),
                    "A readiness check with no time to run can only fail. Leave it out for 30 s."
                        .into(),
                ));
            }
            Ok(Some(s)) => Duration::from_secs(s),
            Ok(None) => DEFAULT_READY,
            Err(e) => return Err((format!("role `{name}`: {e}"), String::new())),
        };
        named.insert(
            name.clone(),
            Role {
                name: name.clone(),
                host,
                command,
                cwd: spec
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                ready,
                after,
                deadline: deadline_role,
                ready_deadline,
            },
        );
        declared.push(name);
    }

    // **`after` is checked against the roles that exist**, before ordering: a name that is not a
    // role would otherwise be an order that silently does not exist.
    for role in named.values() {
        for dep in &role.after {
            if !named.contains_key(dep) {
                return Err((
                    format!("role `{}` is after `{dep}`, which is not a role", role.name),
                    format!(
                        "The roles are: {}. `after` names roles in this call.",
                        named.keys().cloned().collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
            if dep == &role.name {
                return Err((
                    format!("role `{}` is after itself", role.name),
                    "A role cannot wait for its own readiness — nothing would ever start.".into(),
                ));
            }
        }
    }

    // **The order, and the cycle.** Kahn's algorithm, one role at a time: the FIRST role (in the
    // caller's order) whose `after` set is already placed is started next, and the check is made
    // again from the top — so a role that waited for the one just placed goes before the roles
    // that waited for nothing. Whatever is left when no role qualifies is a cycle, and the refusal
    // names the roles in it: a scenario that would hang is refused before its first process exists.
    let mut order: Vec<Role> = Vec::with_capacity(named.len());
    let mut left: BTreeMap<String, Role> = named.clone();
    while !left.is_empty() {
        let next = declared
            .iter()
            .filter(|n| left.contains_key(*n))
            .find(|n| {
                left[*n]
                    .after
                    .iter()
                    .all(|d| order.iter().any(|o| &o.name == d))
            })
            .cloned();
        let Some(next) = next else {
            let mut stuck: Vec<String> = left.keys().cloned().collect();
            stuck.sort();
            return Err((
                format!("`after` is a cycle: {}", stuck.join(" → ")),
                "Each of these waits for another to be ready, so none of them can start. Break the \
                 cycle, or drop an `after`."
                    .into(),
            ));
        };
        if let Some(role) = left.remove(&next) {
            order.push(role);
        }
    }

    let run = match args.get("run") {
        None | Some(Value::Null) => None,
        Some(Value::String(command)) => Some(Role {
            name: RUN.into(),
            host: LOCAL.into(),
            command: command.trim().to_string(),
            cwd: None,
            ready: None,
            after: Vec::new(),
            deadline: None,
            ready_deadline: DEFAULT_READY,
        }),
        Some(Value::Object(spec)) => {
            let command = spec
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    (
                        "`run` has no `command`".to_string(),
                        "The run is the client or the test: `run: {\"command\": \"pytest -x\"}`, \
                         with `host` for a machine other than this one."
                            .to_string(),
                    )
                })?
                .to_string();
            let deadline_run = match seconds(spec.get("deadline_s")) {
                Ok(Some(0)) => None,
                Ok(Some(s)) => Some(Duration::from_secs(s)),
                Ok(None) => None,
                Err(e) => return Err((format!("`run`: {e}"), String::new())),
            };
            Some(Role {
                name: RUN.into(),
                host: spec
                    .get("host")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(LOCAL)
                    .to_string(),
                command,
                cwd: spec
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                ready: None,
                after: Vec::new(),
                deadline: deadline_run,
                ready_deadline: DEFAULT_READY,
            })
        }
        Some(other) => {
            return Err((
                format!("`run` is `{other}`"),
                "`run` is a command string, or `{command, host?, deadline_s?}`.".into(),
            ));
        }
    };

    if run.as_ref().is_some_and(|r| r.command.is_empty()) {
        return Err((
            "`run`'s command is empty".into(),
            "Give the client or the test to run once every role is ready.".into(),
        ));
    }

    // **The `${…}` check is not here, and it cannot be.** A role's port may come from the
    // session's lease (RH9), and the lease is taken after the plan is read — so the facts a
    // command is resolved against do not exist yet. [`Scenario::answer`] does that check, before a
    // single process is started, which is the promise that matters: a refusal, not a teardown.
    Ok(Planned {
        roles: order,
        run,
        until,
    })
}

/// **The facts a plan resolves against**: role name → (host, port).
///
/// One function, because two readers need the same map — the substitution that fills a command in
/// and the lease assignment that rewrites a `Ready::Lease` into a port — and two spellings of it
/// is how a `${role.port}` ends up resolving to one number in one place and another next door.
fn facts_of(plan: &Planned) -> BTreeMap<String, (String, Option<u16>)> {
    let mut facts: BTreeMap<String, (String, Option<u16>)> = BTreeMap::new();
    for role in plan.roles.iter().chain(plan.run.iter()) {
        facts.insert(
            role.name.clone(),
            (
                role.host.clone(),
                role.ready.as_ref().and_then(|r| r.port()),
            ),
        );
    }
    facts
}

/// **Take a port for every role that asked the lease for one**, one block per host (RH9).
///
/// One round of allocation per host and not one per role: a block of four is one row and one lock,
/// and four blocks of one would be four chances for another session to slip between them. The
/// ports are handed out in the caller's own role order, so the same call twice asks for the same
/// shape.
///
/// Every role it rewrites is a `Ready::Lease` → `Ready::Port`, which is also what makes
/// `${role.port}` resolvable — see [`facts_of`], read after this.
fn assign(
    blocks: &Blocks,
    plan: &mut Planned,
) -> Result<Vec<letibot_tokencore::store::Lease>, (String, String)> {
    // How many a host needs, in the order the roles were declared.
    let mut wants: Vec<(String, u16)> = Vec::new();
    for role in plan.roles.iter().chain(plan.run.iter()) {
        if matches!(role.ready, Some(Ready::Lease)) {
            match wants.iter_mut().find(|(h, _)| *h == role.host) {
                Some((_, n)) => *n += 1,
                None => wants.push((role.host.clone(), 1)),
            }
        }
    }
    let mut held = Vec::new();
    let mut ports: BTreeMap<String, Vec<u16>> = BTreeMap::new();
    for (host, count) in wants {
        let lease = match blocks.take(&host, count) {
            Ok(l) => l,
            Err(why) => {
                // Everything already taken goes back: a block held for a plan that never ran is
                // the leak this package is against.
                for taken in &held {
                    let _ = blocks.release(taken, "the scenario could not take its ports");
                }
                return Err((
                    format!("`{host}` could not give a block of {count} port(s)"),
                    format!(
                        "{why}\n\nNothing was started. A role that asks for \
                         `ready: {{\"port\": \"lease\"}}` takes its port from this session's \
                         block on its own host — that is how two sessions on one machine stop \
                         colliding."
                    ),
                ));
            }
        };
        let taken: Vec<u16> = (lease.port_first..=lease.port_last)
            .take(count as usize)
            .collect();
        ports.insert(host, taken);
        held.push(lease);
    }
    for role in plan.roles.iter_mut().chain(plan.run.iter_mut()) {
        if matches!(role.ready, Some(Ready::Lease)) {
            let Some(next) = ports.get_mut(&role.host).and_then(|v| {
                if v.is_empty() {
                    None
                } else {
                    Some(v.remove(0))
                }
            }) else {
                return Err((
                    format!("`{}` asked for a leased port and none was left", role.name),
                    "This build's own arithmetic, not the call's: the block was smaller than the \
                     roles asking for one."
                        .to_string(),
                ));
            };
            role.ready = Some(Ready::Port(next));
        }
    }
    Ok(held)
}

/// `Some(seconds)` from a JSON number, `None` when it is absent — or the sentence saying why the
/// value is not a number of seconds.
fn seconds(v: Option<&Value>) -> Result<Option<u64>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("`{v}` is not a whole number of seconds")),
    }
}

/// **`${role.host}` and `${role.port}`**, and the refusal that names what could have been meant.
///
/// Two facts and no more: they are the two a client needs to be pointed at a service, and every
/// other field of a role is a thing this build would have to guess at. A `${…}` this cannot resolve
/// is an error rather than left alone — a shell would expand the empty string and the service would
/// start against `:`, which is a failure thirty seconds later with no breadcrumb.
fn substitute(
    text: &str,
    facts: &BTreeMap<String, (String, Option<u16>)>,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let Some(end) = after.find('}') else {
            return Err(format!(
                "`{rest}` has a `${{` that is never closed — the spelling is `${{role.host}}` or \
                 `${{role.port}}`"
            ));
        };
        let field = &after[..end];
        let (role, what) = field.split_once('.').ok_or_else(|| {
            format!(
                "`${{{field}}}` names no fact — the spelling is `${{role.host}}` or \
                 `${{role.port}}`"
            )
        })?;
        let Some((host, port)) = facts.get(role) else {
            return Err(format!(
                "`${{{field}}}` names `{role}`, which is not a role here. The roles are: {}",
                facts.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        };
        match what {
            "host" => out.push_str(host),
            "port" => match port {
                Some(p) => out.push_str(&p.to_string()),
                None => {
                    return Err(format!(
                        "`${{{field}}}` asks for `{role}`'s port, and `{role}` declares no port — \
                         a port is known from `ready: {{\"port\": N}}`. A `log` or `check` \
                         readiness check does not name one."
                    ));
                }
            },
            other => {
                return Err(format!(
                    "`${{{field}}}` asks for `{role}`'s `{other}`; this build resolves `.host` and \
                     `.port` and nothing else"
                ));
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// **Run the plan.** One exit: every path tears down what it started, every planned cell has a
/// row, and a block of ports taken for this call goes back with it.
fn run(
    fleet: &Fleet,
    plan: &Planned,
    blocks: Option<&Blocks>,
    held: &[letibot_tokencore::store::Lease],
    facts: &BTreeMap<String, (String, Option<u16>)>,
) -> Invocation {
    let started_at = Instant::now();
    let mut done: Vec<Cell> = Vec::new();
    let mut started: Vec<(Running, Option<Duration>)> = Vec::new();
    let mut plan_legs: Vec<Leg> = Vec::with_capacity(plan.roles.len() + 1);
    let mut failed: Option<String> = None;

    // The plan as legs, so the answer is the plan's shape whatever happens next. A role's deadline
    // and the whole scenario's are the same instant at most: whichever is sooner.
    for role in &plan.roles {
        let leg = Leg {
            label: role.name.clone(),
            host: role.host.clone(),
            command: role.command.clone(),
            cwd: role.cwd.clone(),
            deadline: Some(match role.deadline {
                Some(d) => (Instant::now() + d).min(plan.until),
                None => plan.until,
            }),
            env: Vec::new(),
        };
        plan_legs.push(leg);
    }
    if let Some(run) = &plan.run {
        plan_legs.push(Leg {
            label: run.name.clone(),
            host: run.host.clone(),
            command: run.command.clone(),
            cwd: run.cwd.clone(),
            deadline: Some(match run.deadline {
                Some(d) => (Instant::now() + d).min(plan.until),
                None => plan.until,
            }),
            env: Vec::new(),
        });
    }

    for role in &plan.roles {
        let command = match substitute(&role.command, facts) {
            Ok(c) => c,
            Err(why) => {
                failed = Some(format!("`{}`: {why}", role.name));
                break;
            }
        };
        let leg = Leg {
            label: role.name.clone(),
            host: role.host.clone(),
            command,
            cwd: role.cwd.as_ref().and_then(|c| substitute(c, facts).ok()),
            deadline: Some(match role.deadline {
                Some(d) => (Instant::now() + d).min(plan.until),
                None => plan.until,
            }),
            // **The block's own facts, in the environment** (RH9). A role that asked the lease for
            // its port has them substituted into its command as well; the environment is how a
            // service's *children* — a `docker compose` it starts, a shell it forks — see the same
            // prefix, which is what a teardown looks for.
            env: env_for(held, &role.host),
        };
        let run = match fleet.start(&leg) {
            Ok(r) => r,
            Err(why) => {
                done.push(Cell::never(&leg, why.clone()));
                failed = Some(format!("`{}` did not start: {why}", role.name));
                break;
            }
        };
        // **Readiness, on the role's own host, with the role's own deadline** — and never past the
        // whole scenario's, because the outer wall is what the caller is waiting for.
        let ready_after = match &role.ready {
            None => Some(Duration::ZERO),
            Some(check) => {
                let until = (Instant::now() + role.ready_deadline).min(plan.until);
                match fleet.await_ready(&run, check, until) {
                    Ok(took) => Some(took),
                    Err(why) => {
                        // **The row says what it was waiting for and never got.** It is a
                        // `stopped` row and not a `never ran` one, and the difference is the
                        // fact: the role *did* start, something *did* end it (the teardown
                        // below), and what went wrong was its readiness. `never ran` is kept for
                        // a leg that was never started at all.
                        done.push(Cell {
                            label: leg.label.clone(),
                            host: leg.host.clone(),
                            command: leg.command.clone(),
                            ready_after: None,
                            fate: Fate::Stopped {
                                why: why.clone(),
                                took: run.started.elapsed(),
                            },
                            tail: fleet.tail(&run),
                            log: run.log.clone(),
                        });
                        fleet.record_end(
                            &run,
                            &Fate::Stopped {
                                why: why.clone(),
                                took: run.started.elapsed(),
                            },
                        );
                        started.push((run, None));
                        failed = Some(format!("`{}` never became ready: {why}", role.name));
                        break;
                    }
                }
            }
        };
        started.push((run, ready_after));
    }

    // **The run**, only when every role came up. A client started against a fixture that failed is
    // a second failure dressed as the first.
    if failed.is_none()
        && let Some(run_role) = &plan.run
    {
        let command =
            substitute(&run_role.command, facts).unwrap_or_else(|_| run_role.command.clone());
        let leg = Leg {
            label: run_role.name.clone(),
            host: run_role.host.clone(),
            command,
            cwd: run_role
                .cwd
                .as_ref()
                .and_then(|c| substitute(c, facts).ok()),
            deadline: Some(match run_role.deadline {
                Some(d) => (Instant::now() + d).min(plan.until),
                None => plan.until,
            }),
            env: env_for(held, &run_role.host),
        };
        match fleet.start(&leg) {
            Ok(running) => {
                let within = leg
                    .deadline
                    .map(|at| at.saturating_duration_since(Instant::now()))
                    .unwrap_or(plan.until.saturating_duration_since(Instant::now()));
                let fate = match fleet.wait(&running, within) {
                    crate::exec::Waited::Happened {
                        state: Some(state), ..
                    } => Fate::Ended {
                        state,
                        took: running.started.elapsed(),
                    },
                    crate::exec::Waited::Happened { state: None, .. } => Fate::Stopped {
                        why: "it ended without a status".to_string(),
                        took: running.started.elapsed(),
                    },
                    _ => {
                        fleet.stop(&running, fleet::EXPIRED);
                        Fate::Stopped {
                            why: format!("{} passed", fleet::EXPIRED),
                            took: running.started.elapsed(),
                        }
                    }
                };
                if !fate.ok() {
                    failed = Some(format!("the run did not succeed ({})", fate.word()));
                }
                fleet.record_end(&running, &fate);
                done.push(fleet.cell(&running, None, fate));
            }
            Err(why) => {
                done.push(Cell::never(&leg, why.clone()));
                failed = Some(format!("the run did not start: {why}"));
            }
        }
    }

    // **The teardown: every role, reversed, on its own host.** Reversed because the roles that were
    // after others were started last, and a fixture comes down in the order a person would stop it.
    let by = match &failed {
        Some(why) => format!("the scenario failed: {}", fleet::one_line(why, 120)),
        None => "the scenario ended".to_string(),
    };
    for (running, ready_after) in started.into_iter().rev() {
        // What it was BEFORE the teardown decides its row: a role that had already died ended, and
        // one that was still up was stopped by this call — and the two are not the same sentence.
        let before = fleet.settled(&running);
        fleet.stop(&running, &by);
        let fate = match before {
            Some((state, took)) => Fate::Ended { state, took },
            None => Fate::Stopped {
                why: by.clone(),
                took: running.started.elapsed(),
            },
        };
        fleet.record_end(&running, &fate);
        // **One row per cell**, and a role that already has one (the readiness check that failed)
        // keeps it: that row is the explanation, and a second one saying `the scenario ended` would
        // replace a reason with a shrug.
        if !done.iter().any(|c| c.label == running.leg.label) {
            done.push(fleet.cell(&running, ready_after, fate));
        }
    }

    // **The blocks go back, and exactly what they created comes down first** (RH9). After the
    // roles, because a role's own processes are what made the containers: ending a process tells
    // nothing on the far side that its `docker run` should stop.
    let mut released: Vec<String> = Vec::new();
    if let Some(blocks) = blocks {
        for lease in held {
            released.extend(blocks.release(lease, &by));
        }
    }

    let rows = fleet::cells(&plan_legs, &done);
    let table = answer(
        fleet,
        &rows,
        started_at.elapsed(),
        failed.as_deref(),
        &released,
    );
    match failed {
        Some(why) => Invocation::failed(format!("the scenario did not finish: {why}"), table),
        None => Invocation::ok(table),
    }
}

/// **The block's facts, as the environment of a leg on `host`** — empty when this call took no
/// block there.
fn env_for(held: &[letibot_tokencore::store::Lease], host: &str) -> Vec<(String, String)> {
    let place = if host == LOCAL { None } else { Some(host) };
    held.iter()
        .find(|l| l.host.as_deref() == place)
        .map(|l| l.env())
        .unwrap_or_default()
}

/// The answer: the count, one row per planned cell, and the one line about the whole.
fn answer(
    fleet: &Fleet,
    rows: &[Cell],
    took: Duration,
    failed: Option<&str>,
    released: &[String],
) -> String {
    let never = rows
        .iter()
        .filter(|c| matches!(c.fate, Fate::Never { .. }))
        .count();
    let mut s = format!(
        "scenario · {} planned, {} answered · {:.1}s · work dir `{}` on each host\n",
        rows.len(),
        rows.len(),
        took.as_secs_f32(),
        fleet.work()
    );
    for row in rows {
        s.push_str(&format!("  {}\n", row.line()));
    }
    match failed {
        Some(why) => s.push_str(&format!("it did not finish: {why}\n")),
        None => {
            if never == 0 {
                s.push_str(
                    "every role came up and the run exited 0; everything started was torn down\n",
                )
            } else {
                s.push_str(&format!(
                    "the run exited 0, and {never} cell(s) never ran; everything started was torn down\n"
                ))
            }
        }
    }
    // **What went back, and what could not be touched** — a refusal here is the operator's list
    // doing its job, and a release nobody mentions is a block held for ever.
    for line in released {
        s.push_str(&format!("  lease: {line}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The facts a plan resolves against: role name → (host, port).
    fn facts() -> BTreeMap<String, (String, Option<u16>)> {
        let mut f = BTreeMap::new();
        f.insert("db".to_string(), ("lab2x1".to_string(), Some(5432)));
        f.insert("cache".to_string(), ("lab3".to_string(), None));
        f
    }

    /// **Both facts, spliced where they are written**, and the rest of the text untouched.
    #[test]
    fn a_fact_resolves_in_the_middle_of_a_command() {
        let out = substitute("pg -h ${db.host} -p ${db.port} --tag=${db.host}", &facts())
            .expect("both facts are known");
        assert_eq!(out, "pg -h lab2x1 -p 5432 --tag=lab2x1");
    }

    /// **A role that is not there is named, with the roles that are** — the refusal a miss needs.
    #[test]
    fn an_unknown_role_is_refused_with_the_roles_that_exist() {
        let e = substitute("x ${nope.host}", &facts()).unwrap_err();
        assert!(e.contains("`nope`"), "{e}");
        assert!(e.contains("cache, db"), "the roles it does have: {e}");
    }

    /// **A port that no role declares is refused rather than left empty**: a client pointed at
    /// `host:` fails thirty seconds later with no breadcrumb.
    #[test]
    fn a_port_a_role_does_not_declare_is_refused() {
        let e = substitute("x ${cache.port}", &facts()).unwrap_err();
        assert!(e.contains("declares no port"), "{e}");
        assert!(e.contains("ready: {\"port\": N}"), "{e}");
    }

    /// **A fact this build does not have** — not everything about a role is a fact, and saying
    /// which two are is the whole of the refusal.
    #[test]
    fn a_fact_this_build_does_not_resolve_is_named() {
        let e = substitute("x ${db.user}", &facts()).unwrap_err();
        assert!(e.contains(".host` and `.port`"), "{e}");
        assert!(
            substitute("x ${db.host", &facts()).is_err(),
            "an unclosed `${{`"
        );
        assert!(
            substitute("x ${dbhost}", &facts()).is_err(),
            "no dot, no fact"
        );
    }

    /// **The plan's order is the graph's, and the ties are the caller's.** `client` is declared
    /// first and starts second; `b` and `a` do not wait on anything, so they keep their order.
    #[test]
    fn after_orders_the_starts_and_ties_follow_the_caller() {
        // `serde_json` keeps a call's key order (`preserve_order`), so this is the caller's order
        // and not this build's.
        let text = r#"{
            "roles": {
                "client": {"host": "h", "command": "c", "after": ["web"]},
                "web": {"host": "h", "command": "w", "ready": {"port": 80}},
                "b": {"host": "h", "command": "b"},
                "a": {"host": "h", "command": "a"}
            }
        }"#;
        let args: Value = serde_json::from_str(text).expect("the call");
        let planned = plan(&args).expect("the plan");
        let names: Vec<&str> = planned.roles.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["web", "client", "b", "a"],
            "`client` waits for `web`; the rest keep the caller's order"
        );
    }

    /// **A cycle is refused by the names in it**, before any process exists.
    #[test]
    fn a_cycle_is_refused_by_the_names_in_it() {
        let text = r#"{
            "roles": {
                "a": {"host": "h", "command": "a", "after": ["b"]},
                "b": {"host": "h", "command": "b", "after": ["a"]}
            }
        }"#;
        let args: Value = serde_json::from_str(text).expect("the call");
        let (reason, _) = plan(&args).unwrap_err();
        assert!(reason.contains("cycle"), "{reason}");
        assert!(reason.contains('a') && reason.contains('b'), "{reason}");
    }

    /// **A role may not be called `run`** — one row, one cell, and `run` is already the client's.
    #[test]
    fn a_role_called_run_is_refused() {
        let args = serde_json::json!({
            "roles": {"run": {"host": "h", "command": "x"}}
        });
        let (reason, body) = plan(&args).unwrap_err();
        assert!(reason.contains("`run`"), "{reason}");
        assert!(body.contains("Rename the role"), "{body}");
    }

    /// **`after` naming a role that is not there** is refused, with the roles that are — an order
    /// that silently does not exist is a scenario that starts in the wrong order and passes.
    #[test]
    fn after_naming_no_role_is_refused_with_the_roles_there_are() {
        let args = serde_json::json!({
            "roles": {
                "a": {"host": "h", "command": "a"},
                "b": {"host": "h", "command": "b", "after": ["zz"]}
            }
        });
        let (reason, body) = plan(&args).unwrap_err();
        assert!(reason.contains("`zz`"), "{reason}");
        assert!(body.contains('a') && body.contains('b'), "{body}");
    }

    /// **A readiness check is exactly one of the three**, and the refusal says so — a check that
    /// is three checks cannot say which one passed.
    #[test]
    fn a_readiness_check_names_one_thing() {
        assert!(Ready::parse(&serde_json::json!({"port": 1})).is_ok());
        assert!(Ready::parse(&serde_json::json!({"log": "ready"})).is_ok());
        assert!(Ready::parse(&serde_json::json!({"check": "true"})).is_ok());
        let both = Ready::parse(&serde_json::json!({"port": 1, "log": "x"})).unwrap_err();
        assert!(both.contains("ONE of"), "{both}");
        let none = Ready::parse(&serde_json::json!({})).unwrap_err();
        assert!(none.contains("names none"), "{none}");
        let bad = Ready::parse(&serde_json::json!({"log": "a(b"})).unwrap_err();
        assert!(bad.contains("not a regular expression"), "{bad}");
        assert!(
            Ready::parse(&serde_json::json!({"port": 0})).is_err(),
            "port 0"
        );
    }
}
