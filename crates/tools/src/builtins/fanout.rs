//! `fanout` — **the same work on many hosts, separately** (map), with the answer as the reduce
//! (RH8).
//!
//! `docs/remote-hosts.md`: *"`{hosts: [..] | {host: command}, command, deadline}`: one supervised
//! job per host in parallel; the result is one row per host — exit status, duration, output tail,
//! or why it never ran (an unreachable host is a row, not a failure of the whole)."*
//!
//! # What this replaces, in the operator's own session
//!
//! The Stroppy session's `fleet.sh` — 641 lines, `for h in …` ×137, `fleet.sh` ×235 — and its own
//! audit, which found cells of the plan that never ran and were never mentioned. Two rules follow
//! and both are in the answer rather than in a comment: **every planned cell is a row**
//! ([`crate::fleet::cells`]), and **an unreachable host is a row, not a failure of the whole**
//! ([`crate::fleet::Fate::Never`]).
//!
//! # Not a shell loop, and what that buys
//!
//! Each branch is a job of the *host's own* job table — the launch, the pid file, the exit file,
//! the `setsid` process group and the group kill are RH1's code — and each is written into the
//! supervisor's `process` table, so the daemon finds it after a restart and a session's close kills
//! it on the host it runs on. The alternative (`ssh … 'nohup … &'`, a `ps` poll, a log tail) is the
//! thing this package exists to delete: 1,038 Bash calls in one session's main transcript, ~20,500
//! across its children.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::exec::ProcessHost;
use crate::fleet::{self, Cell, Fate, Fleet, Leg, ProcessBook, Running};
use crate::lease::Blocks;
use crate::remote::Paths;
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The name this tool is registered and seated by.
pub const TOOL: &str = "fanout";

/// **How long a fanout may run when nobody says.** Ten minutes: long enough for a test suite on
/// four machines, short enough that a forgotten call does not hold a fleet for an hour.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(600);

/// **The longest deadline this tool accepts.** An hour. A caller that wants longer wants
/// `background`-shaped work it can come back to, and a tool that would hold a turn for a day is a
/// tool whose answer nobody is waiting for.
pub const MAX_DEADLINE: Duration = Duration::from_secs(3600);

/// **The most ports one branch may ask its block for.** A branch's services listen on a handful;
/// a number past this is a typo that would empty the host's upper range for everybody else.
pub const MAX_LEASE_PORTS: u16 = 256;

/// `fanout` — one command per host, in parallel, one row per host.
pub struct Fanout {
    /// A test's doors, in [`crate::builtins::hosts::HostsTool`]'s shape: the file to read, this
    /// box's ssh material, and the program to run instead of `ssh`.
    file: Option<PathBuf>,
    paths: Option<Paths>,
    ssh: Option<PathBuf>,
    bound: Option<Duration>,
    book: Arc<dyn ProcessBook>,
    /// **The session's leases** (RH9) — what a `lease` argument takes its blocks from.
    blocks: Option<Arc<Blocks>>,
}

impl Default for Fanout {
    fn default() -> Self {
        Fanout {
            file: None,
            paths: None,
            ssh: None,
            bound: None,
            book: Arc::new(fleet::NoBook),
            blocks: None,
        }
    }
}

impl Fanout {
    pub fn new() -> Fanout {
        Fanout::default()
    }

    /// Read this hosts file instead of the operator's.
    pub fn with_file(mut self, file: impl Into<PathBuf>) -> Fanout {
        self.file = Some(file.into());
        self
    }

    pub fn with_paths(mut self, paths: Paths) -> Fanout {
        self.paths = Some(paths);
        self
    }

    /// Run this program instead of `ssh` — a test's fake, which never touches a network.
    pub fn with_ssh(mut self, program: impl Into<PathBuf>) -> Fanout {
        self.ssh = Some(program.into());
        self
    }

    pub fn with_bound(mut self, bound: Duration) -> Fanout {
        self.bound = Some(bound);
        self
    }

    /// **Where the started processes are written down** — the daemon binds this to its own session
    /// ([`crate::fleet::ProcessBook`]). Everywhere else nothing is written, which is the truth for
    /// a run with no store.
    pub fn with_book(mut self, book: Arc<dyn ProcessBook>) -> Fanout {
        self.book = book;
        self
    }

    /// **The session's leases** (RH9) — what `lease: true` takes a block from, one per branch.
    pub fn with_leases(mut self, blocks: Arc<Blocks>) -> Fanout {
        self.blocks = Some(blocks);
        self
    }

    /// The fleet this call uses: the test's doors applied over the real thing.
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

impl Tool for Fanout {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            TOOL,
            "Run one command on MANY hosts at once (map), and answer one row per host: its exit \
             status, how long it took, the tail of its output — or why it never ran (no such host, \
             a key that is not installed, a host that is down). `hosts` is a list of names from \
             `hosts.toml` or an object mapping each host to its own command; `command` is the \
             default when the list has none. `local` is this session's own box and takes no ssh. \
             The work is concurrent; the rows are strict: a host that never answered is a row \
             saying so, never a missing line, and the other hosts still report. Every branch is a \
             supervised job written to the process table, so a restart finds it and the session's \
             close kills it on that host. `deadline_s` (default 600, max 3600) kills a branch that \
             outlives it and its row says stopped.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "hosts": {
                        "description": "A list of host names, or an object of name → command (each host may run its own). Names come from the operator's `hosts.toml`; `local` is this session's own box.",
                        "anyOf": [
                            {"type": "array", "items": {"type": "string"}},
                            {"type": "object", "additionalProperties": {"type": "string"}}
                        ]
                    },
                    "command": {"type": "string", "description": "The command both lists run; required when `hosts` is a list, ignored for hosts that name their own."},
                    "deadline_s": {"type": "integer", "description": "Seconds each branch may run before it is killed and its row says stopped. Default 600, at most 3600. Seconds rather than milliseconds because a fanout's deadline is minutes."},
                    "cwd": {"type": "string", "description": "Where the commands run, on each host: absolute, or relative to the work directory this call made there (`<host root>/fanout-<call>`)."},
                    "lease": {"description": "Give each branch a block of ports from this session's lease ON ITS OWN HOST, outside that host's ephemeral range: `true` is one port, a number is that many. The block travels to the command as `LETIBOT_LEASE_PORT_FIRST`, `…_PORT_LAST`, `…_PORTS` and `…_PREFIX` — name the containers and compose projects you start under that prefix, and the teardown removes exactly those. Every block is released when the call ends.", "type": ["boolean", "integer"]}
                },
                "required": ["hosts"]
            }),
            // Runs commands on other machines under the session's own gate — one card for the
            // whole call, whose arguments name every host it reaches and every command it runs.
            Access::Exec,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let work = format!("fanout-{}", sanitise(ctx.call_id()));
        self.answer(args, ctx.backend.processes_arc(), &work)
    }
}

impl Fanout {
    /// **The whole tool, without a call context** — the daemon's path is [`Tool::invoke`], and this
    /// is the same thing with the two facts a `ctx` would have supplied: this session's own backend
    /// (for a `local` leg) and the work directory name. It is what a test drives, exactly as
    /// [`crate::builtins::hosts::HostsTool::answer`] is, so a test measures the tool and not a
    /// harness grown around it.
    pub fn answer(
        &self,
        args: &Value,
        local: Option<Arc<dyn ProcessHost>>,
        work: &str,
    ) -> Invocation {
        let plan = match self.plan(args) {
            Ok(p) => p,
            Err(why) => return Invocation::failed(why.reason, why.body),
        };
        // **A block per branch, when the call asked for one** (RH9). A fanout's branches usually
        // land on DIFFERENT machines and often several on one, so the block is taken per leg and
        // per host — and every block goes back when the call answers, whether it worked or not.
        let ports = match lease_ports(args) {
            Ok(p) => p,
            Err(why) => return Invocation::failed(why.reason, why.body),
        };
        let mut plan = plan;
        let mut held: Vec<letibot_tokencore::store::Lease> = Vec::new();
        if let Some(ports) = ports {
            let Some(blocks) = &self.blocks else {
                return Invocation::failed(
                    "`lease` and this run has no session store",
                    "A block of ports is a row in the session store, and this run has none behind \
                     it — so there is nothing to hold one in and nothing for a teardown to release.",
                );
            };
            for leg in &mut plan {
                match blocks.take(&leg.host, ports) {
                    Ok(lease) => {
                        leg.env = lease.env();
                        held.push(lease);
                    }
                    Err(why) => {
                        // A branch that could not get a block is a ROW, not a failure of the call:
                        // the other machines can still do their work. Its `never ran` carries why.
                        leg.env = vec![(
                            "LETIBOT_LEASE_REFUSED".to_string(),
                            crate::fleet::one_line(&why, 160),
                        )];
                    }
                }
            }
        }
        let fleet = self.fleet(work.to_string()).with_local(local);
        self.run(&fleet, plan, self.blocks.as_deref(), &held)
    }
}

/// A refusal that names what to fix — clause 1 in the same call.
struct Refusal {
    reason: String,
    body: String,
}

fn refuse(reason: impl Into<String>, body: impl Into<String>) -> Refusal {
    Refusal {
        reason: reason.into(),
        body: body.into(),
    }
}

impl Fanout {
    /// **The plan**: one leg per host, in the order the caller wrote them, with the reasons a
    /// malformed call is refused before anything is started.
    fn plan(&self, args: &Value) -> Result<Vec<Leg>, Refusal> {
        let deadline = match args.get("deadline_s") {
            None | Some(Value::Null) => DEFAULT_DEADLINE,
            Some(v) => match v.as_u64() {
                Some(0) => {
                    return Err(refuse(
                        "`deadline_s` is 0",
                        "A deadline of zero would kill every branch the moment it started. Leave \
                         `deadline_s` out for the default (600 s), or give it seconds.",
                    ));
                }
                Some(s) if s > MAX_DEADLINE.as_secs() => {
                    return Err(refuse(
                        format!(
                            "`deadline_s` is {s}, past the {}-second ceiling",
                            MAX_DEADLINE.as_secs()
                        ),
                        "A fanout is a call in a turn: the model waits for its answer. Work that \
                         must run for longer belongs in the background, where its settlement \
                         reaches the session when it ends.",
                    ));
                }
                Some(s) => Duration::from_secs(s),
                None => {
                    return Err(refuse(
                        format!("`deadline_s` is `{v}`, which is not a number of seconds"),
                        "Give `deadline_s` as a whole number of seconds, or leave it out for the \
                         default (600).",
                    ));
                }
            },
        };
        let until = Instant::now() + deadline;
        let cwd = args
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let Some(hosts) = args.get("hosts") else {
            return Err(refuse(
                "`hosts` is missing",
                "Give the hosts to reach: `hosts: [\"lab2x1\", \"lab3\"]` with a `command`, or \
                 `hosts: {\"lab2x1\": \"cargo test\", \"lab3\": \"go test ./...\"}`.",
            ));
        };
        // **What the caller asked for, as pairs**, before any of it becomes a leg: the list form
        // takes the default command, the object form carries its own.
        let asked: Vec<(String, Option<String>)> = match hosts {
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    let Some(name) = item.as_str() else {
                        return Err(refuse(
                            format!("`hosts` holds `{item}`, which is not a name"),
                            "A list of hosts is a list of strings. For a per-host command use the \
                             object form: `hosts: {\"lab2x1\": \"cargo test\"}`.",
                        ));
                    };
                    out.push((name.to_string(), command.clone()));
                }
                out
            }
            Value::Object(map) => {
                let mut out = Vec::with_capacity(map.len());
                for (name, cmd) in map {
                    let Some(cmd) = cmd.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                        return Err(refuse(
                            format!("`{name}`'s command is not a command"),
                            "In the object form every value is a command string: `hosts: \
                             {\"lab2x1\": \"cargo test\"}`.",
                        ));
                    };
                    out.push((name.clone(), Some(cmd.to_string())));
                }
                out
            }
            other => {
                return Err(refuse(
                    format!("`hosts` is `{other}`, not a list or an object"),
                    "Give `hosts: [\"lab2x1\"]` with a `command`, or `hosts: {\"lab2x1\": \"cargo \
                     test\"}`.",
                ));
            }
        };
        let mut plan: Vec<Leg> = Vec::with_capacity(asked.len());
        for (name, own) in asked {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Err(refuse(
                    "a host name is empty",
                    "Every entry in `hosts` names a machine. An empty name would start nothing and \
                     report nothing, which is the one thing this tool refuses to do.",
                ));
            }
            if plan.iter().any(|l| l.label == name) {
                return Err(refuse(
                    format!("`{name}` is in `hosts` twice"),
                    "One row per host is what a fanout answers with, so a host listed twice has one \
                     row and two commands — and which one the row is about would be a guess. List \
                     it once and put both commands in one command line.",
                ));
            }
            let Some(command) = own else {
                return Err(refuse(
                    format!("`{name}` has no command"),
                    "A list of hosts needs a `command`, or each host needs its own: `hosts: \
                     {\"lab2x1\": \"cargo test\"}`.",
                ));
            };
            plan.push(Leg {
                label: name.clone(),
                host: name,
                command,
                cwd: cwd.clone(),
                deadline: Some(until),
                env: Vec::new(),
            });
        }
        if plan.is_empty() {
            return Err(refuse(
                "`hosts` is empty, so there is nothing to fan out",
                "A fanout with no hosts has no rows, and an answer with no rows is the silent \
                 omission this tool exists to prevent. Name at least one host.",
            ));
        }
        Ok(plan)
    }

    /// **Run the plan**, and answer one row per planned cell.
    fn run(
        &self,
        fleet: &Fleet,
        plan: Vec<Leg>,
        blocks: Option<&Blocks>,
        held: &[letibot_tokencore::store::Lease],
    ) -> Invocation {
        let started = Instant::now();
        let mut done: Vec<Cell> = Vec::new();
        let mut running: Vec<Running> = Vec::new();
        for leg in &plan {
            match fleet.start(leg) {
                Ok(run) => running.push(run),
                Err(why) => done.push(Cell::never(leg, why)),
            }
        }
        // **One pass over the unsettled legs, then a breath.** Each pass asks every host that has
        // not answered yet, so one slow machine costs its own round trip and not the whole
        // fanout's — and the loop is bounded by the plan's own deadline, which every leg carries.
        let mut fates: Vec<(String, Fate)> = Vec::new();
        loop {
            let mut waiting = false;
            for run in &running {
                if fates.iter().any(|(l, _)| l == &run.leg.label) {
                    continue;
                }
                match fleet.settled(run) {
                    Some((state, took)) => fates.push((
                        run.leg.label.clone(),
                        Fate::Ended {
                            state: state.clone(),
                            took,
                        },
                    )),
                    None => {
                        waiting = true;
                        // **The deadline, enforced here as well as on the record.** The supervisor
                        // owns the record's deadline, and this loop must not answer *still
                        // running* while it waits for a tick it does not control: the row is this
                        // call's answer, and it is the call's deadline that decides when a branch
                        // that never answered is done being waited for.
                        if run.leg.deadline.is_some_and(|at| Instant::now() >= at) {
                            fleet.stop(run, fleet::EXPIRED);
                            fates.push((
                                run.leg.label.clone(),
                                Fate::Stopped {
                                    why: format!("{} passed", fleet::EXPIRED),
                                    took: run.started.elapsed(),
                                },
                            ));
                        }
                    }
                }
            }
            if !waiting || fates.len() >= running.len() {
                break;
            }
            std::thread::sleep(Duration::from_millis(120));
        }
        for run in &running {
            let fate = match fates.iter().find(|(l, _)| l == &run.leg.label) {
                Some((_, f)) => f.clone(),
                // **Unreachable by the loop above, and written as a kill anyway**: a row that says
                // `stopped` must be about a process something stopped, so the leg is ended here
                // before it is described. A claim-versus-fact guard, not a branch anybody expects.
                None => {
                    fleet.stop(run, "the fanout ending");
                    Fate::Stopped {
                        why: "the fanout ended before it settled".to_string(),
                        took: run.started.elapsed(),
                    }
                }
            };
            fleet.record_end(run, &fate);
            done.push(fleet.cell(run, None, fate));
        }
        let rows = fleet::cells(&plan, &done);
        // **Every block the call took goes back**, after the branches have settled: a branch's
        // containers are the reason the block existed, and a release that came first would leave
        // them up under a block the next session is about to be given.
        let mut released = Vec::new();
        if let Some(blocks) = blocks {
            for lease in held {
                released.extend(blocks.release(lease, "the fanout ended"));
            }
        }
        Invocation::ok(table(fleet, &plan, &rows, started.elapsed(), &released))
    }
}

/// The answer: a header with the count that matters, then one line per planned cell.
fn table(
    fleet: &Fleet,
    plan: &[Leg],
    rows: &[Cell],
    took: Duration,
    released: &[String],
) -> String {
    let failed = rows.iter().filter(|c| !c.fate.ok()).count();
    let never = rows
        .iter()
        .filter(|c| matches!(c.fate, Fate::Never { .. }))
        .count();
    let mut s = format!(
        "fanout · {} planned, {} answered · {:.1}s · work dir `{}` on each host\n",
        plan.len(),
        rows.len(),
        took.as_secs_f32(),
        fleet.work()
    );
    for row in rows {
        s.push_str(&format!("  {}\n", row.line()));
    }
    if failed == 0 {
        s.push_str("every cell exited 0\n");
    } else {
        s.push_str(&format!("{failed} of {} cells did not exit 0", rows.len()));
        if never > 0 {
            let declared = fleet.declared();
            if !declared.is_empty() {
                s.push_str(&format!(
                    " ({never} never ran; this file declares {})",
                    declared.join(", ")
                ));
            } else {
                s.push_str(&format!(
                    " ({never} never ran; no hosts are declared at all)"
                ));
            }
        }
        s.push('\n');
    }
    // **What went back, and what could not be touched** — a block held for ever is a block the
    // next session on that machine cannot have, so the answer says what it released.
    for line in released {
        s.push_str(&format!("  lease: {line}\n"));
    }
    s
}

/// **How many ports each branch asked for** — `None` when the call did not ask for a block.
///
/// `lease: true` is one port (the common case: one server per branch), and a number is that many,
/// capped so a typo cannot ask for the whole upper half of the host's range.
fn lease_ports(args: &Value) -> Result<Option<u16>, Refusal> {
    match args.get("lease") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(None),
        Some(Value::Bool(true)) => Ok(Some(1)),
        Some(v) => match v.as_u64() {
            Some(0) => Err(refuse(
                "`lease` is 0",
                "A block of zero ports is no block at all. Leave `lease` out, give it `true` for \
                 one port, or name how many each branch needs.",
            )),
            Some(n) if n > MAX_LEASE_PORTS as u64 => Err(refuse(
                format!("`lease` is {n}, past the {MAX_LEASE_PORTS}-port ceiling"),
                "A block is for the ports one branch's services listen on. If the work needs more \
                 than that, it is more than one branch.",
            )),
            Some(n) => Ok(Some(n as u16)),
            None => Err(refuse(
                format!("`lease` is `{v}`, which is not a number of ports"),
                "`lease: true` takes one port from this session's block on each host, and a whole \
                 number takes that many.",
            )),
        },
    }
}

/// A call id as a directory name: letters, digits, `-` and `_`, everything else `-`.
fn sanitise(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::JobState;
    use crate::fleet::cells;

    /// The plan, or the refusal's two halves — the shape the tool answers with.
    fn plan(args: Value) -> Result<Vec<Leg>, (String, String)> {
        Fanout::new().plan(&args).map_err(|r| (r.reason, r.body))
    }

    /// **A call with nothing to do is refused, in the same call.** An empty `hosts` list would
    /// answer with no rows, and a fanout with no rows is the silent omission this tool exists to
    /// prevent.
    #[test]
    fn a_fanout_with_no_hosts_is_refused() {
        let (reason, body) = plan(serde_json::json!({"hosts": [], "command": "true"})).unwrap_err();
        assert!(reason.contains("empty"), "{reason}");
        assert!(body.contains("Name at least one host"), "{body}");
    }

    /// **A host listed twice** is one row and two commands — refused, with the reason.
    #[test]
    fn a_host_listed_twice_is_refused() {
        let (reason, _) =
            plan(serde_json::json!({"hosts": ["a", "a"], "command": "true"})).unwrap_err();
        assert!(reason.contains("twice"), "{reason}");
    }

    /// **A list with no command** says what to add.
    #[test]
    fn a_list_of_hosts_with_no_command_is_refused() {
        let (reason, body) = plan(serde_json::json!({"hosts": ["a"]})).unwrap_err();
        assert!(reason.contains("`a` has no command"), "{reason}");
        assert!(body.contains("needs a `command`"), "{body}");
    }

    /// **A deadline past the ceiling is refused rather than clamped**: a call that would hold a
    /// turn for a day is a call whose answer nobody is waiting for.
    #[test]
    fn a_deadline_past_the_ceiling_is_refused() {
        let (reason, _) = plan(serde_json::json!({
            "hosts": ["a"], "command": "true", "deadline_s": 100_000
        }))
        .unwrap_err();
        assert!(reason.contains("ceiling"), "{reason}");
    }

    /// **Both spellings produce one leg per host**, each carrying the command it will run and the
    /// deadline the call decided — which is what the record's `deadline_ms` is computed from.
    #[test]
    fn both_spellings_name_a_host_and_a_command_for_every_leg() {
        let listed = plan(serde_json::json!({"hosts": ["a", "b"], "command": "cargo test"}))
            .expect("the list form");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].label, "a");
        assert_eq!(listed[1].host, "b");
        assert!(listed.iter().all(|l| l.command == "cargo test"));
        assert!(listed.iter().all(|l| l.deadline.is_some()));

        let mapped = plan(serde_json::json!({
            "hosts": {"a": "true", "b": "false"}
        }))
        .expect("the object form");
        assert_eq!(mapped.len(), 2);
        assert!(mapped.iter().any(|l| l.label == "a" && l.command == "true"));
        assert!(
            mapped
                .iter()
                .any(|l| l.label == "b" && l.command == "false")
        );
    }

    /// **Every planned cell has a row, by construction** — including the one whose leg never
    /// produced a result. This is the audit the Stroppy fleet runner lacked, as a type.
    #[test]
    fn every_planned_cell_has_a_row() {
        let plan = plan(serde_json::json!({"hosts": ["a", "b", "c"], "command": "true"}))
            .expect("the plan");
        let answered = vec![Cell {
            label: "a".into(),
            host: "a".into(),
            command: "true".into(),
            ready_after: None,
            fate: Fate::Ended {
                state: JobState::Exited { code: 0 },
                took: Duration::from_millis(5),
            },
            tail: String::new(),
            log: None,
        }];
        let rows = cells(&plan, &answered);
        assert_eq!(rows.len(), plan.len(), "one row per planned cell");
        assert_eq!(rows[0].label, "a");
        for row in &rows[1..] {
            assert!(
                matches!(row.fate, Fate::Never { .. }),
                "a cell with no result says so: {row:?}"
            );
            assert!(row.line().contains("never ran"), "{}", row.line());
        }
    }

    /// **The description is prompt, and the lint is the rule.** A path, an endpoint, a content
    /// claim or a version in it is a claim the model acts on after it stops being true — and
    /// `Registry::register` REFUSES such a tool, so this is also the test that a session can open.
    #[test]
    fn the_description_passes_the_lint() {
        for tool in [
            &Fanout::new() as &dyn Tool,
            &crate::builtins::scenario::Scenario::new() as &dyn Tool,
        ] {
            let schema = tool.schema();
            assert_eq!(
                crate::schema::lint_description(&schema.description),
                vec![],
                "{}'s description says what the data contains",
                schema.name
            );
            assert!(schema.description.len() < 800, "{} is long", schema.name);
        }
    }
}
