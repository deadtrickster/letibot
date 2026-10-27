//! **A fake ssh, and the "remote machines" it stands in for** — the rig `fanout` and `scenario`
//! are tested against (RH1's pattern, reused).
//!
//! # No network, and no real host, ever
//!
//! The `ssh` this writes accepts every flag the client passes (`-i`, the `-o` options, the
//! destination), ignores them, and **executes the last argument locally** — which is the remote
//! command. The "remote machine" is this test's temporary home, exported as `HOME` inside the
//! fake, so `root = "~/letibot-remote"` is expanded **on the far side** exactly as it is over a
//! real connection, and every absolute path the client spells from then on is one this fake can
//! open.
//!
//! What that buys is the real thing under test: the command text, the base64 channel, the `setsid`
//! launch, the job files and the group kill are the production ones. What it does not test is ssh
//! itself — a test that reached a second machine from here could not run in this suite, and would
//! prove the network rather than the fleet.
//!
//! # The hosts a rig hands out
//!
//! * `fake1`, `fake2`, … — the same box, the same home, one entry in `hosts.toml` each. Two hosts
//!   that are really one filesystem is the right shape here: what is under test is *one job per
//!   host, one row per host*, not that machines differ.
//! * `gone` — a host whose ssh says `Could not resolve hostname` and exits 255, which is rule 5's
//!   failure as a HOST rather than as a job. It is added on request ([`Rig::unreachable`]) because
//!   the circuit breaker is process-wide and keyed by name: a suite that always declared a dead
//!   host would open that breaker for every other test in the same binary.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_tools::remote::Paths;

/// One fake remote: a temporary home, a `hosts.toml`, and an `ssh` that runs commands against it.
pub struct Rig {
    dir: PathBuf,
    /// The "remote" home: what `~` means on the far side.
    home: PathBuf,
    bin: PathBuf,
    ssh: PathBuf,
    log: PathBuf,
    paths: Paths,
    hosts: PathBuf,
    names: Vec<String>,
}

impl Rig {
    /// A rig declaring `names`, each a `[host.<name>]` block pointing at the same fake home.
    pub fn new(tag: &str, names: &[&str]) -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "letibot-fleet-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let bin = dir.join("bin");
        let home = dir.join("home");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::create_dir_all(&home).expect("home");

        // **A `setsid` for boxes that have none** — Linux does, Darwin does not — so a job on the
        // fake remote leads its own process group exactly as it does on a Linux host: the group
        // kill is only safe when it does, and `RemoteProcesses::kill` asks before it signals.
        if !on_path("setsid") {
            write_executable(
                &bin.join("setsid"),
                "#!/usr/bin/env perl\nuse POSIX ();\nPOSIX::setsid();\nexec { $ARGV[0] } @ARGV;\n",
            );
        }

        let ssh = bin.join("ssh");
        let log = dir.join("ssh.log");
        // One name in the argv is a machine that is down; everything else runs the command HERE,
        // with `HOME` set to the fake remote's home so `~` means what it would over there.
        // **Matched on the HOSTNAME, not on the bare word**: a work directory is named after the
        // call, and a case glob over `$*` that matched `gone` anywhere would take a healthy host
        // down because its job directory was called `fanout-gone`. Measured here first.
        let script = format!(
            "#!/bin/bash\n\
             printf 'ARGV %s\\n' \"$*\" >> {log}\n\
             case \"$*\" in\n\
             \x20 *gone.invalid*)\n\
             \x20   echo 'ssh: Could not resolve hostname gone.invalid: Name or service not known' >&2\n\
             \x20   exit 255;;\n\
             esac\n\
             export PATH={bin}:\"$PATH\"\n\
             export HOME={home}\n\
             for last; do :; done\n\
             exec bash -lc \"$last\"\n",
            log = sh(&log.display().to_string()),
            bin = sh(&bin.display().to_string()),
            home = sh(&home.display().to_string()),
        );
        write_executable(&ssh, &script);

        // **letibot's own key and known host**, in a config directory of this test's own — the two
        // facts a connection is refused without, and neither is the operator's.
        let paths = Paths::in_home(dir.join("letibot-home"));
        std::fs::create_dir_all(paths.keys()).expect("the keys directory");
        std::fs::write(paths.known_hosts(), b"").expect("the host key file");
        let hosts = dir.join("hosts.toml");
        let mut rig = Rig {
            dir,
            home,
            bin,
            ssh,
            log,
            paths,
            hosts,
            names: Vec::new(),
        };
        rig.declare(names);
        rig
    }

    /// Add `[host.NAME]` blocks (idempotent), each with its key and its accepted host key.
    pub fn declare(&mut self, names: &[&str]) {
        let mut text = std::fs::read_to_string(&self.hosts).unwrap_or_default();
        let mut known = std::fs::read_to_string(self.paths.known_hosts()).unwrap_or_default();
        for name in names {
            if self.names.iter().any(|n| n == name) {
                continue;
            }
            text.push_str(&format!(
                "[host.{name}]\nhostname = \"{name}.invalid\"\nuser = \"tester\"\n\
                 root = \"~/letibot-remote\"\nmise = false\n\n"
            ));
            known.push_str(&format!(
                "{name}.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n"
            ));
            std::fs::write(self.paths.default_key(name), b"a dedicated key").expect("the key");
            self.names.push(name.to_string());
        }
        std::fs::write(&self.hosts, text).expect("hosts.toml");
        std::fs::write(self.paths.known_hosts(), known).expect("known hosts");
    }

    /// Add a host whose ssh refuses to resolve it — [`Rig::declare`] plus the name the fake fails on.
    pub fn unreachable(&mut self) {
        self.declare(&["gone"]);
    }

    pub fn hosts_file(&self) -> PathBuf {
        self.hosts.clone()
    }

    pub fn ssh_program(&self) -> PathBuf {
        self.ssh.clone()
    }

    pub fn paths(&self) -> Paths {
        self.paths.clone()
    }

    /// The far side's `root` — `<home>/letibot-remote`, after `~` is expanded there.
    pub fn remote_root(&self) -> PathBuf {
        self.home.join("letibot-remote")
    }

    /// The `bin` directory the fake ssh puts first on the far side's `PATH`.
    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// **Every ssh call this rig made**, in order — one line per call, its whole argv.
    pub fn ssh_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix("ARGV ").map(str::to_string))
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // **Only the pids this rig wrote down**, never a pattern: a job a panic left running would
        // otherwise keep writing into a tree this drop removes.
        for jobs in jobs_dirs(&self.remote_root()) {
            let Ok(entries) = std::fs::read_dir(&jobs) else {
                continue;
            };
            for e in entries.flatten() {
                if e.path().extension().is_some_and(|x| x == "pid")
                    && let Ok(pid) = std::fs::read_to_string(e.path())
                        .unwrap_or_default()
                        .trim()
                        .parse::<u32>()
                {
                    let _ = std::process::Command::new("kill")
                        .args(["-KILL", &format!("-{pid}")])
                        .output();
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Every `<root>/<work>/.letibot-remote/jobs` under a fake remote root.
fn jobs_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for e in entries.flatten() {
        let jobs = e.path().join(".letibot-remote").join("jobs");
        if jobs.is_dir() {
            out.push(jobs);
        }
    }
    out
}

/// **Is this port listening on this box right now?** — the assertion a teardown is measured by,
/// asked of the kernel rather than of the tool's own row.
pub fn listening(port: u16) -> bool {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "(exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null && echo yes || echo no"
        ))
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim() == "yes",
        Err(_) => false,
    }
}

/// A port nothing is listening on right now: bound once by the test, then released, so the service
/// under test can take it.
pub fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = l.local_addr().expect("the address").port();
    drop(l);
    port
}

/// Poll `what` for up to `within`, answering whether it became true.
pub fn until(within: Duration, mut what: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < within {
        if what() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    what()
}

/// One shell word, single-quoted — the paths in the fake's text are the test's temp directory.
fn sh(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn write_executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).expect("writing a fake program");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}
