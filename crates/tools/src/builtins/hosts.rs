//! `hosts` — **which machines the operator declared, and which of them is least loaded**.
//!
//! # Why this is a tool and not a sentence in the prompt
//!
//! `task`'s `where` argument has taken `host:NAME` since RH1, and until this tool there was no
//! way for a model to learn *which* names exist: the refusal for a wrong one listed them, so the
//! only way to find out was to guess wrong. Putting the list in the prompt is the other obvious
//! answer and it is worse in two ways — the file is the operator's and changes without a restart,
//! so a prompt would go stale, and the prompt is a cached prefix, so a list in it would be
//! re-prefilled every time a machine was added. A tool reads the file **at the call** and its
//! answer is as fresh as the file.
//!
//! # Two answers, one tool
//!
//! * no arguments — one line per declared host: the name, `user@hostname`, its `root`, and whether
//!   the two things a connection is refused without are in place — its dedicated key file, and a
//!   host key accepted in letibot's own known-hosts file. **No ssh call is made**, so this answer
//!   is instant and works with every machine switched off.
//! * `probe: true` — one bounded ssh call per host, **in parallel**, reporting whether it is
//!   reachable or the refusal/timeout sentence, and its live facts: cores, load average, memory
//!   total and available, free disk under its root, and whether `git` and `cargo` are on its
//!   `PATH` (with the host's `path` list in front, which is how a session's own commands see it).
//!   That is the answer to *which machine should this work go to*.
//!
//! A probe never writes anything on the far side: [`crate::remote::probe`] runs one script of
//! `uname`, `nproc`, `/proc/loadavg`, `/proc/meminfo`, `df -Pk`, `command -v` and nothing else.
//!
//! # Read-only, and it never asks
//!
//! [`Access::Read`]: it reads two files here (the operator's `hosts.toml` and letibot's own
//! known-hosts file) and asks machines questions. Nothing it can do changes anything on any host,
//! so nothing it does raises a permission card — a model asking *where can I put this* should not
//! cost the operator a decision.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::remote::{Facts, Host, Hosts, Paths, hosts_file, identity_for, probe};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The name this tool is registered and seated by.
pub const TOOL: &str = "hosts";

/// **How long the whole probe may take**, whatever the machines do.
///
/// A probe of four hosts is four ssh calls in parallel, and this is the promise to the caller: the
/// tool returns inside this, with a host that did not answer reported as timed out rather than
/// waited on. [`HOST_BOUND`] is the per-host share; the total is enforced by the collecting loop,
/// so a host whose ssh ignores its own bound cannot hold the answer.
pub const PROBE_TOTAL: Duration = Duration::from_secs(10);

/// The bound on one host's probe. Under [`PROBE_TOTAL`] on purpose, so a slow host's own bound
/// fires before the whole probe's does and the report says *that host* timed out.
pub const HOST_BOUND: Duration = Duration::from_secs(8);

pub struct HostsTool {
    /// The file to read. `None` is [`hosts_file`] **asked at the call** — the operator's file, or
    /// `$LETIBOT_HOSTS` — which is what makes a call see a file edited a second ago. A test names
    /// one instead of setting the variable, because a test that set `$LETIBOT_HOSTS` would race
    /// every other test in the same process.
    file: Option<PathBuf>,
    /// letibot's own ssh material (the keys, the known-hosts file), when the caller says. `None`
    /// is [`Paths::from_env`].
    paths: Option<Paths>,
    /// The ssh program, when the caller names one (a test's fake).
    ssh: Option<PathBuf>,
    total: Duration,
    per_host: Duration,
}

impl Default for HostsTool {
    fn default() -> Self {
        HostsTool {
            file: None,
            paths: None,
            ssh: None,
            total: PROBE_TOTAL,
            per_host: HOST_BOUND,
        }
    }
}

impl HostsTool {
    pub fn new() -> HostsTool {
        HostsTool::default()
    }

    /// Read this file instead of the operator's — a test's door.
    pub fn with_file(mut self, file: impl Into<PathBuf>) -> HostsTool {
        self.file = Some(file.into());
        self
    }

    /// Check keys and host keys against this ssh material instead of this box's.
    pub fn with_paths(mut self, paths: Paths) -> HostsTool {
        self.paths = Some(paths);
        self
    }

    /// Run this program instead of `ssh` — a test's fake, which never touches a network.
    pub fn with_ssh(mut self, program: impl Into<PathBuf>) -> HostsTool {
        self.ssh = Some(program.into());
        self
    }

    /// Shorten the wall clock on a probe. Production never calls it; a test with a fake ssh that
    /// never answers should measure a second, not ten.
    pub fn with_bounds(mut self, total: Duration, per_host: Duration) -> HostsTool {
        self.total = total;
        self.per_host = per_host;
        self
    }

    fn file(&self) -> PathBuf {
        self.file.clone().unwrap_or_else(hosts_file)
    }

    fn paths(&self) -> Paths {
        self.paths.clone().unwrap_or_else(Paths::from_env)
    }
}

impl Tool for HostsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            TOOL,
            "The machines this box may place work on — the operator's `hosts.toml`, read now, so \
             a host added a minute ago is here. With no arguments: one line per host (name, \
             user@hostname, root) and whether its key and its host key are in place, which needs no \
             connection. With `probe: true`: one bounded connection per host, in parallel, and the \
             live facts — cores, load average, memory available, free disk, and whether `git` and \
             `cargo` are on its PATH — or the refusal or timeout sentence for a host that cannot be \
             reached. `names` narrows either answer. Read-only, and it never asks: nothing is \
             written on any host. This is how a `where: \"host:NAME\"` is chosen; the list is never \
             in the prompt, because the file changes and the prompt is a cache.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "probe": {"type": "boolean", "description": "Connect to each host and report its live facts — cores, load average, memory, free disk, and whether `git`/`cargo` are on its PATH — or the refusal or timeout sentence. Off by default: a listing needs no connection and works with every machine off."},
                    "names": {"type": "array", "items": {"type": "string"}, "description": "Only these hosts, by the name `where: \"host:NAME\"` takes."},
                    "timeout_s": {"type": "integer", "description": "With `probe`: how long the whole probe may take, in seconds. Never more than the tool's own bound (10 s); hosts that have not answered by then are reported as timed out."}
                }
            }),
            // Reads the operator's file and letibot's known-hosts file, and asks machines
            // questions. It cannot change anything anywhere, so there is nothing to gate.
            Access::Read,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        self.answer(args)
    }
}

impl HostsTool {
    /// **The whole tool, without a call context.** It reads two files here and asks machines
    /// questions there, so there is nothing a backend, a gate or a session could add to the
    /// answer — which is why this takes `args` and nothing else, and why a test can drive it
    /// without a runtime.
    pub fn answer(&self, args: &Value) -> Invocation {
        let file = self.file();
        // **Read at the call, never cached.** The file is the operator's and they edit it while a
        // session is running: a cache would be a wrong answer with no symptom, and a prompt would
        // be the same thing with a slower refresh.
        let hosts = match std::fs::read_to_string(&file) {
            Ok(text) => match Hosts::parse(&text) {
                Ok(h) => h,
                // A file that does not parse is a real answer and has to name the line: a typo in
                // this file places a session on a machine nobody named.
                Err(why) => {
                    return Invocation::failed(
                        "the hosts file does not parse",
                        format!(
                            "{}: {why}. Nothing can be placed until the file parses; the grammar \
                             is `[host.NAME]` with `hostname`, `user`, `key`, `path` and `root`.",
                            file.display()
                        ),
                    );
                }
            },
            Err(_) => Hosts::default(),
        };
        let paths = self.paths();

        // **The names asked for**, or all of them. A name that is not in the file is refused with
        // the ones that are, the same sentence a `where: "host:…"` gives — one refusal, in one
        // shape, so a model reads one thing twice rather than two things once.
        let wanted: Option<Vec<String>> = args
            .get("names")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty());
        let mut chosen: Vec<Host> = Vec::new();
        for name in wanted.iter().flatten() {
            match hosts.get(name) {
                Some(h) => chosen.push(h.clone()),
                None => {
                    return Invocation::failed(
                        "there is no such host",
                        format!("{}. Nothing was probed.", hosts.refusal(name)),
                    );
                }
            }
        }
        if wanted.is_none() {
            for name in hosts.names() {
                if let Some(h) = hosts.get(&name) {
                    chosen.push(h.clone());
                }
            }
        }

        if hosts.is_empty() && wanted.is_none() {
            return Invocation::ok(format!(
                "**No hosts are declared.** {} does not exist, or has no `[host.…]` blocks. A \
                 host is declared by the operator, in that file and never in a project:\n\n    \
                 [host.lab2x1]\n    hostname = \"lab2x1\"      # what ssh connects to\n    user     \
                 = \"dead\"\n    key      = \"~/.config/letibot/keys/lab2x1\"   # the default\n    \
                 path     = [\"~/.cargo/bin\"]                   # in front of the remote PATH\n    \
                 root     = \"~/letibot-remote\"                 # where remote worktrees live\n\n\
                 Until then every child runs in your own boundary: leave `where` out.",
                file.display()
            ));
        }

        let probe_it = args.get("probe").and_then(|v| v.as_bool()).unwrap_or(false);
        if !probe_it {
            return Invocation::ok(listing(&file, &chosen, &paths));
        }
        let total = args
            .get("timeout_s")
            .and_then(|v| v.as_u64())
            .map(|s| Duration::from_secs(s.clamp(1, PROBE_TOTAL.as_secs())))
            .unwrap_or(self.total);
        Invocation::ok(self.probe(&file, &chosen, total, &paths))
    }
}

/// **The listing**: what is declared, and what would have to be true for a connection to work.
///
/// No connection is made — the two checks are file reads — so this answer works with every
/// machine off, and it is the one that says *why* a placement would fail before anybody waits for
/// a timeout.
fn listing(file: &std::path::Path, hosts: &[Host], paths: &Paths) -> String {
    let mut out = format!(
        "**{} host(s) declared** in {} — the operator's file, never a project's. No connection \
         was made; `probe: true` asks each one for its load and its toolchain.\n",
        hosts.len(),
        file.display()
    );
    for host in hosts {
        let key = identity_for(&host.name, host.key.as_deref(), paths);
        let known = crate::remote::host_key_known(&paths.known_hosts(), &host.hostname);
        out.push_str(&format!(
            "\n`{}`  {}  root {}\n  key:      {}\n  host key: {}\n",
            host.name,
            host.destination(),
            host.root,
            match &key {
                Ok(path) => format!("ok — {}", path.display()),
                // The whole refusal, on its own lines: it carries the two commands that make the
                // key, which is the only thing a caller can do about it.
                Err(why) => format!("NOT USABLE — {why}"),
            },
            if known {
                format!("ok — in {}", paths.known_hosts().display())
            } else {
                format!(
                    "not accepted — `{}` has no entry for `{}`. Add it once, yourself:\n    \
                     mkdir -p {} && ssh-keyscan -t ed25519 {} >> {}",
                    paths.known_hosts().display(),
                    host.hostname,
                    paths.config().display(),
                    host.hostname,
                    paths.known_hosts().display()
                )
            },
        ));
    }
    out.push_str(
        "\nA host is usable when both say ok. To run work there: `task { where: \"host:NAME\", … }` \
         or `task_start`, and `leticode --host NAME` for a session of your own.",
    );
    out
}

impl HostsTool {
    /// **Every chosen host asked at once**, each bounded, and the whole thing bounded.
    ///
    /// A host per thread, one bounded call each ([`crate::remote::probe`]), and a collecting loop
    /// with the total on it. A host that has not answered when the total passes is reported as
    /// timed out — the thread itself ends when its own bound fires, so nothing is left running
    /// past the answer this returns.
    fn probe(
        &self,
        file: &std::path::Path,
        hosts: &[Host],
        total: Duration,
        paths: &Paths,
    ) -> String {
        let began = Instant::now();
        let (tx, rx) = std::sync::mpsc::channel::<(String, Result<Facts, String>)>();
        let per_host = self.per_host.min(total);
        let mut handles = Vec::new();
        for host in hosts {
            let tx = tx.clone();
            let program = self.ssh.clone();
            let paths = paths.clone();
            let host = host.clone();
            handles.push(std::thread::spawn(move || {
                let answer = probe(&host, program, paths, per_host);
                let _ = tx.send((host.name.clone(), answer));
            }));
        }
        drop(tx);

        let mut answers: BTreeMap<String, Result<Facts, String>> = BTreeMap::new();
        while answers.len() < hosts.len() {
            let left = total.saturating_sub(began.elapsed());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(left) {
                Ok((name, answer)) => {
                    answers.insert(name, answer);
                }
                Err(_) => break,
            }
        }
        let mut out = format!(
            "**Probed {} host(s)** from {} in {} ms. Nothing was written on any of them.\n",
            hosts.len(),
            file.display(),
            began.elapsed().as_millis()
        );
        for host in hosts {
            match answers.remove(&host.name) {
                Some(Ok(facts)) => out.push_str(&format_facts(host, &facts)),
                Some(Err(why)) => out.push_str(&format!(
                    "\n`{}` ({})  NOT REACHED\n  {}\n",
                    host.name,
                    host.destination(),
                    why
                )),
                // **The total passed with this one still out.** Said as a timeout of the whole
                // probe rather than of the host, because that is what this side knows.
                None => out.push_str(&format!(
                    "\n`{}` ({})  TIMED OUT\n  no answer within {} s, so this call stopped \
                     waiting for it. Nothing on that machine was changed, and nothing is still \
                     being waited on here.\n",
                    host.name,
                    host.destination(),
                    total.as_secs_f32().max(0.0)
                )),
            }
        }
        for handle in handles {
            // A thread whose answer arrived has already ended; one that did not is inside its own
            // bound and will end on its own, so this never blocks past the total by more than the
            // per-host bound it was given.
            let _ = handle.join();
        }
        out.push_str(
            "\nPick the least loaded one with `git` and the disk for a build, and place work there \
             with `task { where: \"host:NAME\", … }`.",
        );
        out
    }
}

/// One host's line, from what it answered. Absent facts say *the host did not answer that* rather
/// than a zero, because a zero reads like a full disk.
fn format_facts(host: &Host, f: &Facts) -> String {
    let gib = |kb: Option<u64>| match kb {
        Some(kb) => format!("{:.1} GiB", kb as f64 / 1024.0 / 1024.0),
        None => "unknown".to_string(),
    };
    let free = match f.disk_free_kb {
        Some(kb) => format!(
            "{:.1} GiB free under {}",
            kb as f64 / 1024.0 / 1024.0,
            f.disk_path
        ),
        None => format!("free space under {} unknown", f.disk_path),
    };
    format!(
        "\n`{}` ({})  REACHABLE{}\n  cores:    {}\n  load:     {}\n  memory:   {} available of {}\n  \
         disk:     {}\n  tools:    {}\n",
        host.name,
        host.destination(),
        if f.uname.is_empty() {
            String::new()
        } else {
            format!("  as `{}`", f.uname)
        },
        if f.cores == 0 {
            "unknown".to_string()
        } else {
            f.cores.to_string()
        },
        if f.load.is_empty() {
            "unknown (no `/proc/loadavg`)".to_string()
        } else {
            f.load.clone()
        },
        gib(f.mem_avail_kb),
        gib(f.mem_total_kb),
        free,
        {
            let mut tools = Vec::new();
            match &f.git {
                Some(p) => tools.push(format!("git {p}")),
                None => tools.push("no git on PATH".to_string()),
            }
            match &f.cargo {
                Some(p) => tools.push(format!("cargo {p}")),
                None => tools.push("no cargo on PATH".to_string()),
            }
            tools.join(", ")
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str) -> Host {
        Host {
            name: name.to_string(),
            hostname: format!("{name}.invalid"),
            user: Some("dead".to_string()),
            key: None,
            path: Vec::new(),
            root: "~/letibot-remote".to_string(),
        }
    }

    /// A test's own home, config directory and hosts file, with a key for the hosts that have one
    /// and both host keys accepted — or not, per `known`.
    fn fixture(tag: &str, file: &str, keys: &[&str], known: &[&str]) -> (PathBuf, Paths) {
        let dir = std::env::temp_dir().join(format!(
            "letibot-hosts-tool-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let paths = Paths::in_home(dir.join("home"));
        std::fs::create_dir_all(paths.keys()).expect("the keys directory");
        for name in keys {
            std::fs::write(paths.default_key(name), b"a dedicated key").expect("the key");
        }
        let entries: String = known
            .iter()
            .map(|n| format!("{n}.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n"))
            .collect();
        std::fs::write(paths.known_hosts(), entries).expect("the known hosts");
        let hosts = dir.join("hosts.toml");
        std::fs::write(&hosts, file).expect("a hosts file");
        (hosts, paths)
    }

    /// **The listing names every host, and says which are ready without connecting.**
    #[test]
    fn the_listing_names_each_host_and_what_is_missing() {
        let (file, paths) = fixture(
            "listing",
            "[host.lab2x1]\nhostname = \"lab2x1.invalid\"\nuser = \"dead\"\n\n\
             [host.lub1]\nhostname = \"lub1.invalid\"\nuser = \"dead\"\n",
            &["lab2x1"],
            &["lab2x1"],
        );
        let hosts = Hosts::parse(
            "[host.lab2x1]\nhostname = \"lab2x1.invalid\"\nuser = \"dead\"\n\n\
             [host.lub1]\nhostname = \"lub1.invalid\"\nuser = \"dead\"\n",
        )
        .expect("it parses");
        let chosen: Vec<Host> = hosts
            .names()
            .iter()
            .filter_map(|n| hosts.get(n).cloned())
            .collect();
        let said = listing(&file, &chosen, &paths);
        assert!(said.contains("**2 host(s) declared**"), "{said}");
        assert!(said.contains("`lab2x1`"), "{said}");
        assert!(said.contains("`lub1`"), "{said}");
        assert!(said.contains("dead@lab2x1.invalid"), "{said}");
        assert!(said.contains("No connection was made"), "{said}");
        // The one with both in place, and the one without a key and without a host key.
        assert_eq!(said.matches("key:      ok").count(), 1, "{said}");
        assert!(said.contains("NOT USABLE"), "{said}");
        assert!(said.contains("no file there"), "{said}");
        assert!(
            said.contains("ssh-keyscan -t ed25519 lub1.invalid"),
            "{said}"
        );
        // Nothing that would connect is named as something this call did.
        assert!(!said.contains("REACHABLE"), "{said}");
        let _ = std::fs::remove_dir_all(file.parent().expect("a directory"));
    }

    /// **The tool reads the file at the call**, which is the whole reason it is a tool: an edit
    /// between two calls is seen by the second.
    #[test]
    fn an_edit_between_two_calls_is_seen_by_the_second() {
        let (file, paths) = fixture(
            "fresh",
            "[host.one]\nhostname = \"one.invalid\"\n",
            &[],
            &[],
        );
        let tool = HostsTool::new().with_file(&file).with_paths(paths);
        let first = tool.answer(&serde_json::json!({})).payload;
        assert!(first.contains("`one`"), "{first}");
        assert!(!first.contains("`two`"), "{first}");

        std::fs::write(
            &file,
            "[host.one]\nhostname = \"one.invalid\"\n[host.two]\nhostname = \"two.invalid\"\n",
        )
        .expect("the file, edited");
        let second = tool.answer(&serde_json::json!({})).payload;
        assert!(
            second.contains("`two`"),
            "the file is read at the call: {second}"
        );
        assert!(second.contains("**2 host(s) declared**"), "{second}");
        let _ = std::fs::remove_dir_all(file.parent().expect("a directory"));
    }

    /// **No host declared is an answer, not a failure** — and it names the shape that would fix it.
    #[test]
    fn an_empty_file_says_the_file_is_the_place_a_host_is_declared() {
        let (file, paths) = fixture("empty", "", &[], &[]);
        let tool = HostsTool::new().with_file(&file).with_paths(paths);
        let said = tool.answer(&serde_json::json!({})).payload;
        assert!(said.contains("No hosts are declared"), "{said}");
        assert!(said.contains("[host.lab2x1]"), "{said}");
        assert!(said.contains("leave `where` out"), "{said}");
        let _ = std::fs::remove_dir_all(file.parent().expect("a directory"));
    }

    /// **A file that does not parse is refused with the line**, not reported as no hosts: a typo in
    /// this file is what would place a session on a machine nobody named.
    #[test]
    fn a_broken_file_names_the_line() {
        let (file, paths) = fixture(
            "broken",
            "[host.one]\nhostnam = \"one.invalid\"\n",
            &[],
            &[],
        );
        let tool = HostsTool::new().with_file(&file).with_paths(paths);
        let said = tool.answer(&serde_json::json!({}));
        assert!(
            matches!(said.outcome, letibot_transcript::ToolOutcome::Failed { .. }),
            "a file that does not parse is a refusal: {:?}",
            said.outcome
        );
        assert!(said.payload.contains("hostnam"), "{}", said.payload);
        assert!(
            said.payload.contains("until the file parses"),
            "the refusal says what has to change: {}",
            said.payload
        );
        let _ = std::fs::remove_dir_all(file.parent().expect("a directory"));
    }

    /// **A name that is not declared is refused with the ones that are** — the same sentence a
    /// `where: "host:…"` gives.
    #[test]
    fn a_name_that_is_not_declared_is_refused_with_the_ones_that_are() {
        let (file, paths) = fixture(
            "wrongname",
            "[host.lab2x1]\nhostname = \"lab2x1.invalid\"\n",
            &[],
            &[],
        );
        let tool = HostsTool::new().with_file(&file).with_paths(paths);
        let said = tool.answer(&serde_json::json!({"names": ["lab2"]})).payload;
        assert!(said.contains("lab2x1"), "{said}");
        assert!(said.contains("Nothing was placed"), "{said}");
        let _ = std::fs::remove_dir_all(file.parent().expect("a directory"));
    }

    /// The facts, as the host prints them — and a host that answers half of them.
    #[test]
    fn the_facts_are_read_from_key_value_lines() {
        let said = "uname=lab2x1\ncores=16\nload=0.42 0.31 0.22\nmem_total_kb=65793512\n\
                    mem_avail_kb=40265318\ndisk_path=/home/dead/letibot-remote\n\
                    disk_free_kb=214748364\ngit=/usr/bin/git\ncargo=/home/dead/.cargo/bin/cargo\n";
        let f = crate::remote::facts_of(said);
        assert_eq!(f.uname, "lab2x1");
        assert_eq!(f.cores, 16);
        assert_eq!(f.load, "0.42 0.31 0.22");
        assert_eq!(f.mem_total_kb, Some(65793512));
        assert_eq!(f.mem_avail_kb, Some(40265318));
        assert_eq!(f.disk_path, "/home/dead/letibot-remote");
        assert_eq!(f.disk_free_kb, Some(214748364));
        assert_eq!(f.cargo.as_deref(), Some("/home/dead/.cargo/bin/cargo"));
        // A host with nothing to say about a fact says nothing, and it is reported absent.
        let bare = crate::remote::facts_of("uname=x\ncores=0\nmem_total_kb=\ngit=\n");
        assert_eq!(bare.mem_total_kb, None);
        assert_eq!(bare.git, None);
        assert_eq!(bare.disk_free_kb, None);
        assert!(
            format_facts(&host("x"), &bare).contains("no git on PATH"),
            "an absent tool is said, not implied"
        );
    }
}
