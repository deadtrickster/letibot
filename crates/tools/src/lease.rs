//! **Resource leases on shared hosts, and the teardown that removes exactly what a session made**
//! (RH9).
//!
//! # What went wrong, in the operator's own numbers
//!
//! The Stroppy session that this package answers (`docs/remote-hosts.md` § *What an agent
//! hand-rolled without builtins*, item 6) built its own compose-project and port code — `docker ps`
//! ×430 — and still hit: *ephemeral-port collisions, loopback exhaustion, "address already in use"
//! ×18, "too many open files" ×34, a container left 19 h*. Every one of those is a resource two
//! sessions picked independently on a machine neither of them owns.
//!
//! So there are three rules here, and each is structural rather than remembered:
//!
//! 1. **Ports come from a block nobody else has** — [`crate::store::Lease`], taken inside the
//!    store's `BEGIN IMMEDIATE` so two daemons cannot pick the same one, and **outside the host's
//!    own ephemeral range**, because a port inside it is one the kernel will hand to somebody
//!    else's outgoing connection whenever it feels like it.
//! 2. **What a session creates is named under a prefix**, so a teardown can find it again —
//!    [`Lease::prefix`].
//! 3. **A teardown removes only that prefix's own things, and the operator's never-touch list
//!    wins** — [`Blocks::release`]. Nothing here enumerates *everything not mine*: on these hosts
//!    the operator's own workloads live beside every agent's (pgfuzz on lubuntu1,
//!    flowy-dogfood-pg on lubuntu3, the etcd fuzz on lab2x1), and *everything not mine* would take
//!    them with it.
//!
//! # The one thing this cannot do, said plainly
//!
//! A lease is a record, not a reservation. The kernel cannot be asked to keep a port free, so what
//! the block guarantees is **no other session of letibot's took it** and **nothing was listening
//! there when it was taken**. A program on that machine that binds one of those ports without
//! asking is not stopped by anything here.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use letibot_tokencore::store::{Ask, Lease};

use crate::fleet::{LOCAL, shell};
use crate::remote::{Host, Hosts, Paths, Ssh};

/// **How many ports a lease is, when the caller does not say.** Eight: a service fixture is a
/// database, a client and a couple of sidecars, and a block that has to be re-taken because it ran
/// out is a second allocation on a shared machine.
pub const PORTS: u16 = 8;

/// **The prefix every name of a lease starts with.** `letibot-` first, so a person reading
/// `docker ps` on the operator's machine can tell an agent's container from anything else that is
/// there — and so a teardown's own query is a prefix and never a pattern nothing bounds.
pub const PREFIX_HEAD: &str = "letibot";

/// **The store, as this module needs it.** A seam rather than a `Store`: this crate is below the
/// daemon and a tool has no store path, which is [`crate::fleet::ProcessBook`]'s reason one layer
/// down. The daemon registers a book bound to its own store and session
/// (`harnessd::leases::StoreLeases`); everywhere else [`NoLeases`] answers, which is the truth for
/// a run with no daemon behind it.
pub trait Leases: Send + Sync {
    /// Take a block, **atomically against every other caller in every process on the box**.
    fn take(&self, ask: &Ask<'_>) -> Result<Lease, String>;
    /// Give one back. `Ok(())` when it was already released: a close, a call's end and the reaper
    /// can all reach the same lease, and two of them arriving is a fact about the code.
    fn release(&self, id: i64, by: &str) -> Result<(), String>;
}

/// **A book that holds nothing** — a run with no store. A `lease` asked of it is a REFUSAL, not a
/// quiet empty block: a session told it has ports 0–0 would bind nothing and report why thirty
/// seconds later.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLeases;

impl Leases for NoLeases {
    fn take(&self, _ask: &Ask<'_>) -> Result<Lease, String> {
        Err(
            "this run has no session store, so there is nowhere to hold a block of ports in. A \
             session with a daemon behind it has one; a test that wants a lease passes \
             `with_leases`."
                .to_string(),
        )
    }

    fn release(&self, _id: i64, _by: &str) -> Result<(), String> {
        Ok(())
    }
}

/// **What a host's own kernel says about its ports** — the two facts the allocation is decided
/// from, gathered in one read-only round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Census {
    /// `(low, high)` from `/proc/sys/net/ipv4/ip_local_port_range`, or the `sysctl` equivalent.
    pub ephemeral: (u16, u16),
    /// **Every port something is listening on there right now.**
    pub listening: BTreeSet<u16>,
}

/// **The script a census runs.** It only reads: two files or two `sysctl`s, and whatever the
/// machine's own `ss` or `netstat` says about listening sockets.
///
/// `ephemeral=` first and the addresses after it, because the parser on this side then has no
/// opinions about the far side's locale — [`crate::remote::probe`]'s own shape, and its own reason.
fn census_script() -> String {
    r#"set +e
if [ -r /proc/sys/net/ipv4/ip_local_port_range ]; then
  set -- $(cat /proc/sys/net/ipv4/ip_local_port_range 2>/dev/null)
  echo "ephemeral=$1 $2"
elif command -v sysctl >/dev/null 2>&1; then
  echo "ephemeral=$(sysctl -n net.inet.ip.portrange.first 2>/dev/null) $(sysctl -n net.inet.ip.portrange.last 2>/dev/null)"
else
  echo "ephemeral="
fi
if command -v ss >/dev/null 2>&1; then
  ss -ltnH 2>/dev/null | awk '{print $4}'
elif command -v netstat >/dev/null 2>&1; then
  netstat -ltn 2>/dev/null | awk 'NR>2 {print $4}'
fi
exit 0
"#
    .to_string()
}

/// **The port out of a socket's local address**, whichever spelling the machine printed.
///
/// Four shapes and no more: `127.0.0.1:5432` (`ss`), `127.0.0.1.5432` (`netstat` on a BSD),
/// `[::]:22` and `*:5432`. The port is after the last `:` or `.`, so one split answers all four —
/// and a line whose tail is not a number (a header, a word) is nothing rather than a zero, because
/// zero would be read as *port 0 is busy*.
fn port_of(address: &str) -> Option<u16> {
    let tail = address.rsplit([':', '.']).next()?.trim();
    tail.parse::<u16>().ok().filter(|p| *p > 0)
}

/// **What a host answered**, read out of [`census_script`]'s output.
fn read_census(said: &str) -> Census {
    let mut ephemeral = (0u16, 0u16);
    let mut listening = BTreeSet::new();
    for line in said.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ephemeral=") {
            let mut it = rest.split_whitespace();
            let lo = it.next().and_then(|v| v.parse::<u16>().ok());
            let hi = it.next().and_then(|v| v.parse::<u16>().ok());
            if let (Some(lo), Some(hi)) = (lo, hi) {
                ephemeral = (lo, hi);
            }
            continue;
        }
        if let Some(p) = port_of(line) {
            listening.insert(p);
        }
    }
    Census {
        ephemeral,
        listening,
    }
}

/// **The lease engine for one session**: a `hosts.toml`, an ssh policy, and the store behind it.
///
/// One per session, built by the daemon beside the process book
/// (`harnessd::harness`'s fleet-tool registration), and given to `scenario` and `fanout`. A test
/// builds one over the fake-ssh rig.
pub struct Blocks {
    hosts: Hosts,
    paths: Paths,
    /// A test's fake `ssh`. `None` is the box's own.
    ssh: Option<PathBuf>,
    /// The bound on this module's own steps, never on a command.
    bound: Duration,
    leases: Arc<dyn Leases>,
    /// **The session that holds what is taken here** — the owner a close, a call's end and the
    /// reaper all go by.
    session: String,
    /// **A test's door onto the ephemeral range.** `None` asks the host, which is what production
    /// does. A test that needs a *deterministic* block cannot move a kernel's range, and a test
    /// that allocated wherever the box happened to have room would assert nothing.
    ephemeral: Option<(u16, u16)>,
}

impl std::fmt::Debug for Blocks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Blocks")
            .field("session", &self.session)
            .finish()
    }
}

impl Blocks {
    /// A lease engine for one session, with no store behind it.
    pub fn new(session: impl Into<String>) -> Blocks {
        Blocks {
            hosts: Hosts::load(),
            paths: Paths::from_env(),
            ssh: None,
            bound: crate::remote::STEP_BOUND,
            leases: Arc::new(NoLeases),
            session: session.into(),
            ephemeral: None,
        }
    }

    /// The session whose leases these are.
    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn with_file(mut self, file: impl AsRef<std::path::Path>) -> Blocks {
        self.hosts = Hosts::load_from(file);
        self
    }

    pub fn with_paths(mut self, paths: Paths) -> Blocks {
        self.paths = paths;
        self
    }

    /// Run this program instead of `ssh` — a test's fake, which never touches a network.
    pub fn with_ssh(mut self, program: impl Into<PathBuf>) -> Blocks {
        self.ssh = Some(program.into());
        self
    }

    pub fn with_bound(mut self, bound: Duration) -> Blocks {
        self.bound = bound;
        self
    }

    pub fn with_leases(mut self, leases: Arc<dyn Leases>) -> Blocks {
        self.leases = leases;
        self
    }

    /// **Say what the host's ephemeral range is instead of asking it** — a test's door, and the
    /// only way a test can assert *this exact block* rather than *some block above whatever this
    /// box's kernel says today*.
    pub fn with_ephemeral(mut self, ephemeral: (u16, u16)) -> Blocks {
        self.ephemeral = Some(ephemeral);
        self
    }

    /// The hosts file this engine reads — what a refusal lists.
    pub fn declared(&self) -> Vec<String> {
        self.hosts.names()
    }

    /// The host, or the refusal naming the ones there are. `local` is this box and needs no entry:
    /// a lease on it is a lease on the machine the tools already run on, whose ports are just as
    /// shared.
    fn host(&self, name: &str) -> Result<Host, String> {
        if name == LOCAL {
            return Ok(Host {
                name: LOCAL.to_string(),
                ..Host::default()
            });
        }
        self.hosts
            .get(name)
            .cloned()
            .ok_or_else(|| format!("{} Nothing was leased.", self.hosts.refusal(name)))
    }

    /// **The prefix a lease's names start with** — `letibot-<session>-<n>`, where the session is
    /// cut down to what a compose project name and a container name both accept.
    ///
    /// The number is the lease's own id, so two blocks on one host from one session cannot share a
    /// prefix: the teardown of one would otherwise take the other's containers with it.
    pub fn prefix(host: Option<&str>, session: &str, id: i64) -> String {
        let mut who: String = session
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        // Docker and compose both want lower case, and a leading digit is allowed but a leading
        // separator is not.
        who = who.to_ascii_lowercase();
        who = who.trim_matches('-').to_string();
        if who.len() > 24 {
            who.truncate(24);
        }
        let who = if who.is_empty() { "s".to_string() } else { who };
        match host {
            Some(h) if h != LOCAL => {
                let h: String = h
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect::<String>()
                    .to_ascii_lowercase();
                format!("{PREFIX_HEAD}-{}-{who}-{id}", h.trim_matches('-'))
            }
            _ => format!("{PREFIX_HEAD}-{who}-{id}"),
        }
    }

    /// **What a host says about its own ports** — one read-only round trip, or one local `sh` for
    /// [`LOCAL`].
    ///
    /// A host that did not answer its ephemeral range is a REFUSAL rather than a guess: allocating
    /// *below* a range this side invented would hand out ports the kernel is free to reassign, and
    /// that is the collision the block exists to remove.
    pub fn census(&self, host: &str) -> Result<Census, String> {
        let script = census_script();
        let said = if host == LOCAL {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .output()
                .map_err(|e| format!("could not ask this box about its ports: {e}"))?;
            String::from_utf8_lossy(&out.stdout).into_owned()
        } else {
            let h = self.host(host)?;
            let ssh = Ssh::connect(h, self.ssh.clone(), self.paths.clone())
                .map_err(|e| format!("`{host}` could not be reached: {e}"))?
                .with_bound(self.bound);
            let (said, _, _) = ssh
                .sh_once(&format!("reading {host}'s ports"), &script)
                .map_err(|e| format!("`{host}` could not be reached: {e}"))?;
            said
        };
        let census = read_census(&said);
        if census.ephemeral.1 == 0 {
            return Err(format!(
                "`{host}` did not say what its ephemeral port range is, so no block above it can \
                 be chosen — and a port inside it is one the kernel gives away to somebody else's \
                 outgoing connection. It answered: {}",
                crate::fleet::one_line(&said, 200)
            ));
        }
        Ok(Census {
            ephemeral: self.ephemeral.unwrap_or(census.ephemeral),
            listening: census.listening,
        })
    }

    /// **Take a block on one host**, or the refusal naming what is there.
    pub fn take(&self, host: &str, ports: u16) -> Result<Lease, String> {
        let census = self.census(host)?;
        let place = if host == LOCAL { None } else { Some(host) };
        // The prefix needs the lease's id, which the store assigns — so the block is taken with a
        // placeholder and renamed below. Two writes to one row would be the alternative, and a row
        // whose prefix is filled in later is a row a teardown could find half-written.
        let provisional = Self::prefix(place, &self.session, 0);
        let lease = self.leases.take(&Ask {
            session: &self.session,
            host: place,
            prefix: &provisional,
            ports: ports.max(1),
            ephemeral: census.ephemeral,
            busy: &census.listening,
            now_ms: letibot_tokencore::store::Store::lease_now_ms(),
        })?;
        Ok(Lease {
            prefix: Self::prefix(place, &self.session, lease.id),
            ..lease
        })
    }

    /// **Give a block back and remove exactly what it created** — the one teardown, called by a
    /// scenario's end, a fanout's end, a session's close and the supervisor's reaper.
    ///
    /// The order is the rule: **what was removed is decided first, and the record is released
    /// second.** A release that happened first would leave a lease nobody holds whose containers
    /// are still up, and the next session's block could be the one that collides with them.
    ///
    /// Returns what was said, one line per host, so a caller can put it in a row.
    pub fn release(&self, lease: &Lease, by: &str) -> Vec<String> {
        let mut said = Vec::new();
        let host = lease.host.as_deref().unwrap_or(LOCAL);
        match self.teardown(host, &lease.prefix) {
            Ok(lines) => said.extend(lines),
            Err(why) => said.push(why),
        }
        if let Err(why) = self.leases.release(lease.id, by) {
            said.push(format!(
                "the block {} was not released: {why} — it stays held until the next daemon asks \
                 again",
                lease.name()
            ));
        }
        // The `released_ms` is the record; this line is the row's own account of it.
        for line in &mut said {
            if !line.starts_with("the block ") {
                *line = format!("{} — {}", lease.name(), line);
            }
        }
        said
    }

    /// **Remove exactly what `prefix` created on `host`, and nothing else.**
    ///
    /// Two round trips and no more: what is there (one `docker ps` and one `docker compose ls`),
    /// then the removals. A host with no `docker` is a no-op with a sentence, because a machine
    /// that runs no containers is an ordinary machine and a failed teardown there would be a
    /// refusal about nothing.
    ///
    /// **Every name is checked against the never-touch list before anything is removed**, and a
    /// name that matches is left alone and reported with the pattern. That is the second wall
    /// behind the prefix, and the order matters: the prefix says *what is mine*, the never-touch
    /// list says *what is not*, and the operator's list wins.
    pub fn teardown(&self, host: &str, prefix: &str) -> Result<Vec<String>, String> {
        let named = if host == LOCAL {
            None
        } else {
            Some(self.host(host)?)
        };
        let (said, _, _) = self.sh(host, &list_script())?;
        let mut said = said;
        if said.contains("no-docker") {
            return Ok(vec![format!(
                "`{host}` has no docker, so there is nothing to remove by name there"
            )]);
        }
        let (containers, projects) = read_listing(&said);
        let mut out = Vec::new();
        let mut remove = Vec::new();
        for name in containers.iter().filter(|n| n.starts_with(prefix)) {
            match named.as_ref().and_then(|h| h.never_touch_hit(name)) {
                Some(pattern) => out.push(format!(
                    "refused: the container `{name}` matches never_touch `{pattern}` on `{host}` — \
                     it was NOT removed, and neither was anything else that pattern covers"
                )),
                None => remove.push(name.clone()),
            }
        }
        // **The compose project is the prefix itself**, so it is the project that would be
        // downed — and it is asked of the never-touch list the same way, as a name.
        let mut down = None;
        if projects.iter().any(|p| p == prefix) {
            match named.as_ref().and_then(|h| h.never_touch_hit(prefix)) {
                Some(pattern) => out.push(format!(
                    "refused: the compose project `{prefix}` matches never_touch `{pattern}` on \
                     `{host}` — it was NOT brought down"
                )),
                None => down = Some(prefix.to_string()),
            }
        }
        if remove.is_empty() && down.is_none() {
            if !said.contains("no-docker") {
                // Nothing of ours is there: the containers went with the process, or the call
                // never got as far as starting one.
                said.clear();
                out.push(format!("nothing of `{prefix}` is left on `{host}`"));
            }
            return Ok(out);
        }
        let (said_removed, _, code) = self.sh(host, &remove_script(&remove, down.as_deref()))?;
        if code != 0 {
            out.push(format!(
                "removing what `{prefix}` made on `{host}` exited {code}: {}",
                crate::fleet::one_line(&said_removed, 200)
            ));
            return Ok(out);
        }
        for name in &remove {
            out.push(format!("removed the container `{name}` on `{host}`"));
        }
        if let Some(p) = &down {
            out.push(format!(
                "brought the compose project `{p}` down on `{host}`"
            ));
        }
        Ok(out)
    }

    /// One script on one host — the local box through `sh`, a named one through the tree's one ssh
    /// policy.
    fn sh(&self, host: &str, script: &str) -> Result<(String, String, i32), String> {
        if host == LOCAL {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(script)
                .output()
                .map_err(|e| format!("could not run a step on this box: {e}"))?;
            return Ok((
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
                out.status.code().unwrap_or(-1),
            ));
        }
        let h = self.host(host)?;
        let ssh = Ssh::connect(h, self.ssh.clone(), self.paths.clone())
            .map_err(|e| format!("`{host}` could not be reached: {e}"))?
            .with_bound(self.bound);
        let (said, stderr, code) = ssh
            .sh_once(&format!("tidying up on {host}"), script)
            .map_err(|e| format!("`{host}` could not be reached: {e}"))?;
        Ok((said, String::from_utf8_lossy(&stderr).into_owned(), code))
    }
}

/// **What is on a host's docker, by name** — and nothing about what is *not*.
///
/// `command -v docker` first: a machine with no docker is an ordinary machine, and asking a shell
/// to run a missing program is a 127 that reads like a fault. `no-docker` is printed as a word
/// rather than as an error so the caller can say it in a sentence.
fn list_script() -> String {
    r#"set +e
if ! command -v docker >/dev/null 2>&1; then echo no-docker; exit 0; fi
echo '--containers'
docker ps -a --format '{{.Names}}' 2>/dev/null
echo '--projects'
docker compose ls --format '{{.Name}}' 2>/dev/null
exit 0
"#
    .to_string()
}

/// The container names and the compose project names a [`list_script`] answered with.
fn read_listing(said: &str) -> (Vec<String>, Vec<String>) {
    let mut containers = Vec::new();
    let mut projects = Vec::new();
    let mut into = None;
    for line in said.lines() {
        match line.trim() {
            "--containers" => {
                into = Some(&mut containers);
                continue;
            }
            "--projects" => {
                into = Some(&mut projects);
                continue;
            }
            _ => (),
        }
        let name = line.trim();
        if name.is_empty() {
            continue;
        }
        if let Some(list) = &mut into {
            list.push(name.to_string());
        }
    }
    (containers, projects)
}

/// **The removals, one line each** — `docker rm -f` per name and `docker compose -p … down` for
/// the project. Every argument is single-quoted by [`shell`]: a container name is the caller's
/// text, and half of what a name can contain is quotes.
fn remove_script(containers: &[String], project: Option<&str>) -> String {
    let mut s = String::from(
        "set +e\nif ! command -v docker >/dev/null 2>&1; then echo no-docker; exit 0; fi\n",
    );
    if !containers.is_empty() {
        s.push_str("docker rm -f");
        for name in containers {
            s.push(' ');
            s.push_str(&shell(name));
        }
        s.push('\n');
    }
    if let Some(p) = project {
        s.push_str(&format!("docker compose -p {} down\n", shell(p)));
    }
    s.push_str("exit 0\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_census_reads_the_range_and_every_listening_spelling() {
        let said =
            "ephemeral=32768 60999\n127.0.0.1:5432\n[::]:22\n*:8080\n127.0.0.1.6379\nLocal\n";
        let c = read_census(said);
        assert_eq!(c.ephemeral, (32768, 60999));
        assert_eq!(
            c.listening.iter().copied().collect::<Vec<_>>(),
            vec![22, 5432, 6379, 8080]
        );
    }

    /// **A header is not port zero**, and a host that answered nothing is `(0, 0)` — which
    /// [`Blocks::census`] turns into a refusal rather than an allocation from port 1 upwards.
    #[test]
    fn a_machine_that_says_nothing_has_no_range_and_no_ports() {
        let c = read_census("Local Address:Port\nNetid State\n");
        assert_eq!(c.ephemeral, (0, 0));
        assert!(c.listening.is_empty());
    }

    #[test]
    fn a_listing_is_split_into_containers_and_projects() {
        let said = "--containers\nletibot-a-1-db\npgfuzz-1\n--projects\nletibot-a-1\n";
        let (c, p) = read_listing(said);
        assert_eq!(c, vec!["letibot-a-1-db", "pgfuzz-1"]);
        assert_eq!(p, vec!["letibot-a-1"]);
    }

    /// The prefix is a name docker and compose both accept, and it carries the lease's number so
    /// two blocks from one session cannot share it.
    #[test]
    fn a_prefix_is_a_name_and_carries_the_number() {
        assert_eq!(
            Blocks::prefix(Some("lab2x1"), "s-1793…abc", 3),
            "letibot-lab2x1-s-1793-abc-3"
        );
        assert_eq!(Blocks::prefix(None, "s-1", 4), "letibot-s-1-4");
        // Long ids are cut, because a container name that overruns what docker accepts is a name
        // nothing can find again.
        let long = Blocks::prefix(Some("h"), &"x".repeat(80), 1);
        assert_eq!(long, format!("letibot-h-{}-1", "x".repeat(24)));
    }

    /// **The removals quote every name**, and a name with a quote in it cannot become two
    /// commands.
    #[test]
    fn a_removal_script_quotes_its_names() {
        let s = remove_script(&["a; rm -rf /".to_string()], Some("pro j"));
        assert!(s.contains("docker rm -f 'a; rm -rf /'"), "{s}");
        assert!(s.contains("docker compose -p 'pro j' down"), "{s}");
    }
}
