//! `letibot doctor`: what this box gives letibot, measured. One row per check as `ok`, `warn` or
//! `FAIL`, and for each warn or FAIL the command that fixes it. Exit 0 all ok, 1 warnings, 2 a FAIL.
//!
//! Read-only. Secrets are never printed: `providers.toml` is parsed for key NAMES only, the keys
//! directory is stat'ed and never opened, and `~/.claude.json` is read for `mcpServers` names.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use letibot_config_ref as cref;
use letibot_tools::rpc::{Ended, run_bounded};

const USAGE: &str = "\
letibot doctor [--json]
  Check what this box gives letibot: cgroup controllers, the installed binaries, running
  daemons, config files, models, the Claude Code integration, disk space, and firecode.
  Each warn or FAIL row ends with the command that fixes it. Read-only.
  --json   one object: sections, checks, summary and the exit code
  exit     0 all ok, 1 warnings only, 2 any FAIL";

const VERSION_BOUND: Duration = Duration::from_secs(10);
const FIRECODE_BOUND: Duration = Duration::from_secs(120);
const SIZE_BOUND: Duration = Duration::from_secs(30);
const PROBE_BOUND: Duration = Duration::from_secs(2);
const INSTALL: &str =
    "curl -fsSL https://raw.githubusercontent.com/deadtrickster/letibot/main/install.sh | sh";
/// `[exec] disk_floor` when the file sets none.
const DEFAULT_FLOOR: &str = "5%";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
    /// A measurement with no verdict, e.g. a size; never counted.
    Info,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
            Status::Info => "--",
        }
    }
    fn json(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
            Status::Info => "info",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub status: Status,
    pub label: String,
    pub value: String,
    pub fix: Vec<String>,
}

fn row(status: Status, label: impl Into<String>, value: impl Into<String>) -> Check {
    Check {
        status,
        label: label.into(),
        value: value.into(),
        fix: Vec::new(),
    }
}

impl Check {
    fn fix(mut self, lines: impl IntoIterator<Item = impl Into<String>>) -> Check {
        self.fix.extend(lines.into_iter().map(Into::into));
        self
    }
}

#[derive(Debug)]
pub struct Section {
    pub name: &'static str,
    pub checks: Vec<Check>,
}

/// Every path the checks read, so a test points them all at a fixture.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub path: Vec<PathBuf>,
    pub uid: u32,
    pub cgroup_root: PathBuf,
    pub proc_root: PathBuf,
    pub build_dir: PathBuf,
    pub cargo_target_dir: Option<PathBuf>,
    pub room_socket: Option<PathBuf>,
    pub endpoint_env: Option<String>,
    pub store_env: Option<String>,
    pub user: String,
    pub dev_kvm: PathBuf,
    pub group_file: PathBuf,
    pub net_dir: PathBuf,
    pub tmp: PathBuf,
    /// Run `--version`, `systemctl` and `firecode doctor`. Off in tests.
    pub run_programs: bool,
    /// How long bridge CPU is sampled; zero skips it.
    pub cpu_sample: Duration,
}

impl Env {
    pub fn from_box() -> Env {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Env {
            config_dir: letibot_tools::remote::Paths::from_env()
                .config()
                .to_path_buf(),
            data_dir: letibot_tools::lifecycle::data_dir(),
            path: std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default(),
            uid: unsafe { libc::getuid() },
            cgroup_root: PathBuf::from("/sys/fs/cgroup"),
            proc_root: PathBuf::from("/proc"),
            build_dir: PathBuf::from("/build"),
            cargo_target_dir: std::env::var_os("CARGO_TARGET_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            room_socket: letibot_harnessd::room::socket_path(),
            endpoint_env: std::env::var("LETIBOT_ENDPOINT")
                .ok()
                .filter(|v| !v.is_empty()),
            store_env: std::env::var("LETIBOT_STORE")
                .ok()
                .filter(|v| !v.is_empty()),
            user: std::env::var("USER").unwrap_or_default(),
            dev_kvm: PathBuf::from("/dev/kvm"),
            group_file: PathBuf::from("/etc/group"),
            net_dir: PathBuf::from("/sys/class/net"),
            tmp: PathBuf::from("/tmp"),
            run_programs: true,
            cpu_sample: Duration::from_secs(1),
            home,
        }
    }

    fn which(&self, name: &str) -> Option<PathBuf> {
        self.path
            .iter()
            .map(|d| d.join(name))
            .find(|p| p.is_file() && is_executable(p))
    }

    fn manager_dir(&self) -> PathBuf {
        let u = self.uid;
        self.cgroup_root
            .join(format!("user.slice/user-{u}.slice/user@{u}.service"))
    }
}

fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

pub fn run(args: &[String]) -> ExitCode {
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--json" => json = true,
            other => {
                eprintln!("letibot doctor: no such argument: {other}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let sections = all(&Env::from_box());
    if json {
        println!("{}", to_json(&sections));
    } else {
        print!("{}", render(&sections));
    }
    ExitCode::from(exit_code(&sections))
}

pub fn all(env: &Env) -> Vec<Section> {
    let mut out = Vec::new();
    if env.cgroup_root.join("cgroup.controllers").is_file() {
        out.push(Section {
            name: "cgroups",
            checks: cgroups(env),
        });
    }
    out.push(Section {
        name: "host",
        checks: host(env),
    });
    out.push(Section {
        name: "binaries",
        checks: binaries(env),
    });
    out.push(Section {
        name: "daemons",
        checks: daemons(env),
    });
    out.push(Section {
        name: "config",
        checks: config(env),
    });
    out.push(Section {
        name: "models",
        checks: models(env),
    });
    out.push(Section {
        name: "Claude Code integration",
        checks: claude(env),
    });
    out.push(Section {
        name: "disk",
        checks: disk(env),
    });
    if let Some(fc) = env.which("firecode") {
        let mut checks = vec![taps(&env.net_dir, &fc)];
        checks.extend(firecode(env));
        out.push(Section {
            name: "firecode (optional)",
            checks,
        });
    }
    out
}

fn counts(sections: &[Section]) -> (usize, usize, usize) {
    let all = sections.iter().flat_map(|s| &s.checks);
    let n = |st: Status| all.clone().filter(|c| c.status == st).count();
    (n(Status::Ok), n(Status::Warn), n(Status::Fail))
}

pub fn exit_code(sections: &[Section]) -> u8 {
    match counts(sections) {
        (_, _, f) if f > 0 => 2,
        (_, w, _) if w > 0 => 1,
        _ => 0,
    }
}

pub fn render(sections: &[Section]) -> String {
    let mut out = String::from("letibot doctor\n");
    for s in sections {
        out.push_str(&format!("\n {}\n", s.name));
        for c in &s.checks {
            let value = match c.status {
                Status::Warn | Status::Fail if c.value.is_empty() => "see below".to_string(),
                Status::Warn | Status::Fail => format!("{} - see below", c.value),
                _ => c.value.clone(),
            };
            out.push_str(&format!(
                "  {:<5} {:<38} {}\n",
                c.status.word(),
                c.label,
                value
            ));
        }
    }
    let (ok, warn, fail) = counts(sections);
    out.push_str(&format!(
        "\n  {ok} ok, {warn} warning(s), {fail} failure(s)\n"
    ));
    let todo: Vec<&Check> = sections
        .iter()
        .flat_map(|s| &s.checks)
        .filter(|c| matches!(c.status, Status::Warn | Status::Fail) && !c.fix.is_empty())
        .collect();
    out.push_str("\n  not checked here (needs root, the network or a judgement):\n");
    for (what, doc) in NOT_CHECKED {
        out.push_str(&format!("    {what:<52} {doc}\n"));
    }
    if !todo.is_empty() {
        out.push_str("\n  what to do:\n");
        for c in todo {
            out.push_str(&format!("    {}\n", c.label));
            for l in &c.fix {
                out.push_str(&format!("        {l}\n"));
            }
        }
    }
    out
}

pub fn to_json(sections: &[Section]) -> String {
    let (ok, warn, fail) = counts(sections);
    let secs: Vec<serde_json::Value> = sections
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "checks": s.checks.iter().map(|c| serde_json::json!({
                    "status": c.status.json(),
                    "label": c.label,
                    "value": c.value,
                    "fix": c.fix,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "sections": secs,
        "summary": {"ok": ok, "warn": warn, "fail": fail},
        "exit": exit_code(sections),
        "not_checked": NOT_CHECKED.iter().map(|(w, d)| serde_json::json!({"what": w, "doc": d})).collect::<Vec<_>>(),
    })
    .to_string()
}

// --- cgroups ---------------------------------------------------------------------------

fn words(text: &str) -> BTreeSet<String> {
    text.split_whitespace().map(str::to_string).collect()
}

fn drop_in_lines() -> Vec<String> {
    vec![
        "sudo mkdir -p /etc/systemd/system/user@.service.d".into(),
        "printf '[Service]\\nDelegate=cpu memory pids io\\n' | sudo tee /etc/systemd/system/user@.service.d/delegate-io.conf".into(),
        "sudo systemctl daemon-reload".into(),
        "# takes effect at a login after every session has ended, or at a reboot".into(),
    ]
}

fn echo_io_line(env: &Env) -> String {
    format!(
        "echo +io > {}",
        env.manager_dir().join("cgroup.subtree_control").display()
    )
}

pub fn cgroups(env: &Env) -> Vec<Check> {
    let dir = env.manager_dir();
    let delegate = if env.run_programs {
        systemctl_delegate(env.uid)
    } else {
        None
    };
    cgroups_with(env, &dir, delegate.as_deref())
}

pub fn cgroups_with(env: &Env, dir: &Path, delegate: Option<&str>) -> Vec<Check> {
    let mut out = Vec::new();
    let controllers = std::fs::read_to_string(dir.join("cgroup.controllers")).ok();
    let subtree = std::fs::read_to_string(dir.join("cgroup.subtree_control")).ok();
    let delegated_io = delegate.map(|d| words(d).contains("io"));
    let Some(controllers) = controllers else {
        out.push(
            row(
                Status::Fail,
                "user manager controllers",
                "no user manager cgroup",
            )
            .fix([format!("# expected {}", dir.display())]),
        );
        return out;
    };
    let have = words(&controllers);
    let missing: Vec<&str> = ["cpu", "memory", "pids"]
        .into_iter()
        .filter(|c| !have.contains(*c))
        .collect();
    let shown = controllers.trim().to_string();
    out.push(if !missing.is_empty() {
        row(
            Status::Fail,
            "user manager controllers",
            format!("{shown} (no {})", missing.join(" ")),
        )
        .fix(drop_in_lines())
    } else if !have.contains("io") {
        row(
            Status::Warn,
            "user manager controllers",
            format!("{shown} (no io: IO is sampled, no throttle)"),
        )
        .fix(drop_in_lines())
    } else {
        row(Status::Ok, "user manager controllers", shown)
    });
    let live = subtree.as_deref().map(words).unwrap_or_default();
    out.push(if live.contains("io") {
        row(Status::Ok, "io live in subtree_control", "yes")
    } else {
        let mut fix = Vec::new();
        if !have.contains("io") || delegated_io == Some(false) {
            fix.extend(drop_in_lines());
        }
        fix.push(echo_io_line(env));
        row(
            Status::Warn,
            "io live in subtree_control",
            subtree.as_deref().map_or("unreadable".into(), |s| {
                if s.trim().is_empty() {
                    "empty".to_string()
                } else {
                    s.trim().to_string()
                }
            }),
        )
        .fix(fix)
    });
    out.push(match delegate {
        None => row(
            Status::Info,
            "DelegateControllers",
            "systemctl did not answer",
        ),
        Some(d) if words(d).contains("io") => row(Status::Ok, "DelegateControllers", d.trim()),
        Some(d) => row(
            Status::Warn,
            "DelegateControllers",
            format!("{} (no io)", d.trim()),
        )
        .fix(drop_in_lines()),
    });
    out
}

fn systemctl_delegate(uid: u32) -> Option<String> {
    let bin = PathBuf::from("systemctl");
    let args = [
        "show".to_string(),
        format!("user@{uid}.service"),
        "-p".into(),
        "DelegateControllers".into(),
        "--value".into(),
    ];
    match run_bounded(&bin, &args, VERSION_BOUND) {
        Ended::Answered(o) if o.status.success() => {
            Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
        }
        _ => None,
    }
}

// --- host ------------------------------------------------------------------------------

/// What this command cannot measure read-only and without root, and where it is written up.
pub const NOT_CHECKED: &[(&str, &str)] = &[
    (
        "daemon build against the store schema",
        "letibot --status; docs/install.md",
    ),
    (
        "[fallback] covers every model in use",
        "docs/config/leticode.toml.md [fallback]",
    ),
    (
        "fleet boxes reachable, same version",
        "docs/remote-hosts.md",
    ),
    (
        "firecode copy-out after a state move",
        "one throwaway VM round trip; docs/leticode.md",
    ),
    (
        "reap_event kills of live daemons",
        "letibot gc --log; docs/lifecycle.md",
    ),
    (
        "systemd-run --user --scope answers",
        "docs/install.md \"On Linux\"",
    ),
    (
        "MCP servers connect at Claude Code start",
        "/mcp in the session",
    ),
];

fn group_members(group_file: &Path, group: &str) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(group_file).ok()?;
    text.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == group).then(|| {
            f.nth(2)
                .unwrap_or("")
                .split(',')
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .collect()
        })
    })
}

fn rw_access(p: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    unsafe { libc::access(c.as_ptr(), libc::R_OK | libc::W_OK) == 0 }
}

pub fn kvm(dev: &Path, group_file: &Path, user: &str) -> Check {
    let label = "kvm usable (/dev/kvm)";
    if !dev.exists() {
        return row(Status::Info, label, "no /dev/kvm: no VMs on this box");
    }
    if rw_access(dev) {
        return row(Status::Ok, label, "read and write");
    }
    let member = group_members(group_file, "kvm").is_some_and(|m| m.iter().any(|u| u == user));
    if member {
        row(
            Status::Warn,
            label,
            format!("{user} is in kvm, this login is not"),
        )
        .fix(["# log out of every session and back in"])
    } else {
        row(
            Status::Warn,
            label,
            format!("{user} is not in the kvm group"),
        )
        .fix([format!(
            "sudo usermod -aG kvm {user}   # then log out and back in"
        )])
    }
}

/// `systemctl --user is-enabled drkonqi-coredump-launcher.socket`: its stdout, or `None` when
/// the unit is not installed.
pub fn drkonqi(state: Option<&str>) -> Check {
    let label = "drkonqi coredump socket masked";
    match state.map(str::trim) {
        None | Some("") | Some("not-found") => row(Status::Ok, label, "drkonqi not installed"),
        Some("masked") | Some("masked-runtime") => row(Status::Ok, label, "masked"),
        Some(other) => row(
            Status::Warn,
            label,
            format!("{other}: a crashing job can loop the user manager"),
        )
        .fix(["systemctl --user mask --now drkonqi-coredump-launcher.socket"]),
    }
}

pub fn failed_units(n: Option<usize>) -> Check {
    let label = "user manager failed units";
    match n {
        None => row(Status::Info, label, "systemctl --user did not answer"),
        Some(n) if n < 1000 => row(Status::Ok, label, n.to_string()),
        Some(n) => row(Status::Warn, label, format!("{n}: a crash loop")).fix([
            "systemctl --user mask --now drkonqi-coredump-launcher.socket",
            "# then log out of every session, or reboot",
        ]),
    }
}

pub fn load(loadavg: Option<&str>, cores: usize) -> Check {
    let label = "load against cores";
    let Some(l) = loadavg
        .and_then(|t| t.split_whitespace().next())
        .and_then(|v| v.parse::<f64>().ok())
    else {
        return row(Status::Info, label, "no /proc/loadavg");
    };
    let shown = format!("{l:.0} on {cores} threads");
    if l <= 2.0 * cores as f64 {
        row(Status::Ok, label, shown)
    } else {
        row(Status::Warn, label, shown)
            // The gate's test step is `cargo nextest run --workspace` now; the other spelling is a
            // by-hand run of the shape it replaced.
            .fix([
                "pgrep -fa 'cargo (nextest run )?--workspace'   # more than one full gate is too many",
            ])
    }
}

/// The mount holding `path`, from a `/proc/mounts` text: (fstype, mount point).
fn mount_of(mounts: &str, path: &Path) -> Option<(String, PathBuf)> {
    mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (_, at, ty) = (f.next()?, f.next()?, f.next()?);
            Some((ty.to_string(), PathBuf::from(at.replace("\\040", " "))))
        })
        .filter(|(_, at)| path.starts_with(at))
        .max_by_key(|(_, at)| at.as_os_str().len())
}

fn mem_total(meminfo: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
        .map(|k| k * 1024)
}

pub fn tmp_check(mounts: Option<&str>, meminfo: Option<&str>, tmp: &Path) -> Check {
    let label = format!("{} not a large tmpfs", tmp.display());
    let Some((ty, _)) = mounts.and_then(|m| mount_of(m, tmp)) else {
        return row(Status::Info, label, "mount unknown");
    };
    if ty != "tmpfs" {
        return row(Status::Ok, label, ty);
    }
    let (Some((_, total)), Some(ram)) = (space(tmp), meminfo.and_then(mem_total)) else {
        return row(Status::Info, label, "tmpfs");
    };
    if total * 2 <= ram {
        row(Status::Ok, label, format!("tmpfs {}", human(total)))
    } else {
        row(
            Status::Warn,
            label,
            format!("tmpfs {} of {} RAM", human(total), human(ram)),
        )
        .fix(["# put /tmp on disk, or cap its tmpfs below half of RAM (size= in fstab)"])
    }
}

fn systemctl_user(args: &[&str]) -> Option<(bool, String)> {
    let mut a = vec!["--user".to_string()];
    a.extend(args.iter().map(|s| s.to_string()));
    match run_bounded(Path::new("systemctl"), &a, VERSION_BOUND) {
        Ended::Answered(o) => Some((
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        )),
        _ => None,
    }
}

pub fn host(env: &Env) -> Vec<Check> {
    let read = |n: &str| std::fs::read_to_string(env.proc_root.join(n)).ok();
    let mut out = vec![kvm(&env.dev_kvm, &env.group_file, &env.user)];
    if env.run_programs {
        let state = systemctl_user(&["is-enabled", "drkonqi-coredump-launcher.socket"])
            .map(|(_, o)| o)
            .filter(|o| !o.trim().is_empty());
        out.push(drkonqi(state.as_deref()));
        let failed = systemctl_user(&["--failed", "--no-legend", "--plain"])
            .map(|(_, o)| o.lines().filter(|l| !l.trim().is_empty()).count());
        out.push(failed_units(failed));
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    out.push(load(read("loadavg").as_deref(), cores));
    out.push(tmp_check(
        read("mounts").as_deref(),
        read("meminfo").as_deref(),
        &env.tmp,
    ));
    out
}

// --- binaries --------------------------------------------------------------------------

const VERSIONED: &[&str] = &["harnessd", "letibot-tui", "letibot-mcp", "letibot-multi"];

/// `name 1.2.3` and an optional commit: a hex word of 7 to 40 characters after the version.
pub fn parse_version(out: &str) -> Option<(String, Option<String>)> {
    let line = out.lines().next()?;
    let mut w = line.split_whitespace().skip(1);
    let version = w.next()?.to_string();
    let commit = w
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .find(|t| (7..=40).contains(&t.len()) && t.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_string);
    Some((version, commit))
}

pub fn binaries(env: &Env) -> Vec<Check> {
    let mut out = Vec::new();
    let mut seen: Vec<(String, String, Option<String>)> = Vec::new();
    for name in VERSIONED {
        let Some(p) = env.which(name) else {
            let st = if *name == "letibot-mcp" {
                Status::Warn
            } else {
                Status::Fail
            };
            out.push(row(st, format!("{name} on PATH"), "not found").fix([INSTALL]));
            continue;
        };
        let ver = if env.run_programs {
            match run_bounded(&p, &["--version".to_string()], VERSION_BOUND) {
                Ended::Answered(o) => parse_version(&String::from_utf8_lossy(&o.stdout)),
                _ => None,
            }
        } else {
            None
        };
        let shown = match &ver {
            Some((v, Some(c))) => format!("{} {v} {c}", p.display()),
            Some((v, None)) => format!("{} {v}", p.display()),
            None if env.run_programs => format!("{} (no --version answer)", p.display()),
            None => p.display().to_string(),
        };
        if let Some((v, c)) = ver {
            seen.push((name.to_string(), v, c));
        }
        out.push(row(Status::Ok, format!("{name} on PATH"), shown));
    }
    if env.run_programs {
        out.push(versions_agree(&seen));
    }
    out.push(launcher_is_script(env.which("letibot").as_deref()));
    out.push(askpass_is_multicall(
        env.which("letibot-askpass").as_deref(),
        env.which("letibot-multi").as_deref(),
    ));
    out
}

pub fn versions_agree(seen: &[(String, String, Option<String>)]) -> Check {
    let versions: BTreeSet<&str> = seen.iter().map(|(_, v, _)| v.as_str()).collect();
    let commits: BTreeSet<&str> = seen.iter().filter_map(|(_, _, c)| c.as_deref()).collect();
    if versions.len() <= 1 && commits.len() <= 1 {
        let v = versions.iter().next().copied().unwrap_or("?");
        let c = commits
            .iter()
            .next()
            .map_or(String::new(), |c| format!(" {c}"));
        return row(Status::Ok, "versions agree", format!("{v}{c}"));
    }
    let list: Vec<String> = seen
        .iter()
        .map(|(n, v, c)| match c {
            Some(c) => format!("{n} {v} {c}"),
            None => format!("{n} {v}"),
        })
        .collect();
    row(Status::Fail, "versions agree", list.join(", ")).fix([
        "# the binaries come from different builds; install them together".to_string(),
        INSTALL.to_string(),
    ])
}

fn head_bytes(p: &Path, n: usize) -> Vec<u8> {
    let mut buf = vec![0; n];
    let got = std::fs::File::open(p)
        .and_then(|mut f| f.read(&mut buf))
        .unwrap_or(0);
    buf.truncate(got);
    buf
}

pub fn launcher_is_script(p: Option<&Path>) -> Check {
    let label = "letibot is the launcher script";
    let Some(p) = p else {
        return row(Status::Fail, label, "no letibot on PATH").fix([INSTALL]);
    };
    let head = head_bytes(p, 4);
    if head.starts_with(b"\x7fELF") {
        row(
            Status::Fail,
            label,
            format!("{} is an ELF binary", p.display()),
        )
        .fix([
            "# a build product was copied over the launcher; put the script back".to_string(),
            INSTALL.to_string(),
        ])
    } else if head.starts_with(b"#!") {
        row(Status::Ok, label, p.display().to_string())
    } else {
        row(
            Status::Fail,
            label,
            format!("{} is neither a script nor ELF", p.display()),
        )
        .fix([INSTALL])
    }
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if ma.len() != mb.len() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let na = match fa.read(&mut ba) {
            Ok(n) => n,
            Err(_) => return false,
        };
        if na == 0 {
            return true;
        }
        if fb.read_exact(&mut bb[..na]).is_err() || ba[..na] != bb[..na] {
            return false;
        }
    }
}

pub fn askpass_is_multicall(askpass: Option<&Path>, multi: Option<&Path>) -> Check {
    let label = "letibot-askpass is the multicall";
    let (Some(a), Some(m)) = (askpass, multi) else {
        return row(
            Status::Fail,
            label,
            if askpass.is_none() {
                "no letibot-askpass on PATH"
            } else {
                "no letibot-multi on PATH"
            },
        )
        .fix([INSTALL]);
    };
    let (Ok(ma), Ok(mm)) = (std::fs::metadata(a), std::fs::metadata(m)) else {
        return row(Status::Fail, label, "cannot stat").fix([INSTALL]);
    };
    let link = format!(
        "ln -sf letibot-multi {}",
        a.parent()
            .unwrap_or(Path::new("."))
            .join("letibot-askpass")
            .display()
    );
    if ma.dev() == mm.dev() && ma.ino() == mm.ino() {
        row(Status::Ok, label, "same inode")
    } else if same_bytes(a, m) {
        row(Status::Ok, label, "same bytes")
    } else {
        row(Status::Fail, label, "a different binary").fix([link])
    }
}

// --- daemons ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Proc {
    pid: u32,
    ppid: u32,
    /// utime + stime, in clock ticks.
    cpu: u64,
    exe: String,
    argv: Vec<String>,
    cwd: Option<PathBuf>,
}

fn procs(proc_root: &Path) -> Vec<Proc> {
    let Ok(rd) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let d = e.path();
        let Ok(exe) = std::fs::read_link(d.join("exe")) else {
            continue;
        };
        let argv = std::fs::read(d.join("cmdline"))
            .map(|b| {
                b.split(|c| *c == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let stat = std::fs::read_to_string(d.join("stat")).unwrap_or_default();
        let after: Vec<&str> = stat
            .rfind(')')
            .map(|i| stat[i + 1..].split_whitespace().collect())
            .unwrap_or_default();
        let field = |i: usize| {
            after
                .get(i)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
        };
        out.push(Proc {
            pid,
            ppid: field(1) as u32,
            cpu: field(11) + field(12),
            exe: exe.to_string_lossy().into_owned(),
            argv,
            cwd: std::fs::read_link(d.join("cwd")).ok(),
        });
    }
    out.sort_by_key(|p| p.pid);
    out
}

fn base(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

fn is_daemon(p: &Proc) -> bool {
    let exe = p.exe.strip_suffix(" (deleted)").unwrap_or(&p.exe);
    base(exe) == "harnessd"
        || p.argv.first().is_some_and(|a| base(a) == "harnessd")
        || (base(exe) == "letibot-multi" && p.argv.get(1).is_some_and(|a| a == "daemon"))
}

fn folder_of(p: &Proc) -> String {
    let mut it = p.argv.iter();
    while let Some(a) = it.next() {
        if a == "--room" {
            return "room coordinator".into();
        }
        if a == "--workspace"
            && let Some(w) = it.next()
        {
            return w.clone();
        }
        if let Some(w) = a.strip_prefix("--workspace=") {
            return w.to_string();
        }
    }
    p.cwd
        .as_ref()
        .map_or("?".into(), |c| c.display().to_string())
}

/// Seconds since the epoch the process started, from `stat` field 22 and `btime`.
fn started_at(proc_root: &Path, pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    let after = &stat[stat.rfind(')')? + 1..];
    let ticks: u64 = after.split_whitespace().nth(19)?.parse().ok()?;
    let btime: u64 = std::fs::read_to_string(proc_root.join("stat"))
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    let hz = match unsafe { libc::sysconf(libc::_SC_CLK_TCK) } {
        n if n > 0 => n as u64,
        _ => 100,
    };
    Some(btime + ticks / hz)
}

fn mtime_secs(p: &Path) -> Option<u64> {
    std::fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

pub fn stale_daemons(proc_root: &Path, installed: Option<&Path>) -> Check {
    let label = "daemons run the installed binary";
    let built = installed.and_then(mtime_secs);
    let daemons: Vec<Proc> = procs(proc_root).into_iter().filter(is_daemon).collect();
    let mut stale = Vec::new();
    for p in &daemons {
        let why = if p.exe.ends_with(" (deleted)") {
            Some("binary deleted")
        } else if let (Some(b), Some(s)) = (built, started_at(proc_root, p.pid)) {
            (s < b).then_some("older than the installed binary")
        } else {
            None
        };
        if let Some(why) = why {
            stale.push((p.pid, folder_of(p), why));
        }
    }
    if daemons.is_empty() {
        return row(Status::Info, label, "no harnessd running");
    }
    if stale.is_empty() {
        return row(
            Status::Ok,
            label,
            format!("{} running, all current", daemons.len()),
        );
    }
    let mut fix = vec!["# restart each; a running turn is interrupted".to_string()];
    for (pid, folder, why) in &stale {
        fix.push(if folder == "room coordinator" {
            format!("letibot rooms restart   # pid {pid}, {why}")
        } else {
            format!("cd {folder} && letibot --restart   # pid {pid}, {why}")
        });
    }
    row(
        Status::Warn,
        label,
        format!("{} of {} need a restart", stale.len(), daemons.len()),
    )
    .fix(fix)
}

pub fn room_reachable(socket: Option<&Path>) -> Check {
    let label = "room coordinator reachable";
    let Some(s) = socket else {
        return row(Status::Warn, label, "no room socket path (no data dir)");
    };
    match std::os::unix::net::UnixStream::connect(s) {
        Ok(_) => row(Status::Ok, label, s.display().to_string()),
        Err(e) => row(Status::Warn, label, format!("{}: {e}", s.display())).fix([
            "letibot rooms status",
            "letibot rooms install   # if it is not installed",
        ]),
    }
}

/// A daemon outside a `letibot-harnessd-*.scope` runs with the launcher's memory caps off.
pub fn daemons_in_scope(proc_root: &Path) -> Check {
    let label = "daemons in their own scope";
    let mut out_of = Vec::new();
    let mut n = 0;
    for p in procs(proc_root).iter().filter(|p| is_daemon(p)) {
        let folder = folder_of(p);
        if folder == "room coordinator" {
            continue;
        }
        n += 1;
        let cg = std::fs::read_to_string(proc_root.join(p.pid.to_string()).join("cgroup"))
            .unwrap_or_default();
        if !cg.contains("/letibot-harnessd-") {
            out_of.push((p.pid, folder));
        }
    }
    if n == 0 {
        return row(Status::Info, label, "no daemon running");
    }
    if out_of.is_empty() {
        return row(Status::Ok, label, format!("{n} of {n}"));
    }
    let mut fix = vec!["# started outside the launcher; restart through it".to_string()];
    fix.extend(
        out_of
            .iter()
            .map(|(pid, f)| format!("cd {f} && letibot --restart   # pid {pid}")),
    );
    row(
        Status::Warn,
        label,
        format!("{} of {n} outside one (memory caps off)", out_of.len()),
    )
    .fix(fix)
}

pub fn store_env(v: Option<&str>) -> Check {
    let label = "LETIBOT_STORE";
    match v {
        None => row(Status::Ok, label, "unset"),
        Some(s) if s.ends_with(".db") => row(
            Status::Warn,
            label,
            format!("{s} names a file; the store is a directory"),
        )
        .fix(["unset LETIBOT_STORE   # and in tmux: tmux set-environment -gu LETIBOT_STORE"]),
        Some(s) if Path::new(s).is_dir() => row(Status::Ok, label, s.to_string()),
        Some(s) => {
            row(Status::Warn, label, format!("{s} is not a directory")).fix(["unset LETIBOT_STORE"])
        }
    }
}

pub fn daemons(env: &Env) -> Vec<Check> {
    vec![
        stale_daemons(&env.proc_root, env.which("harnessd").as_deref()),
        daemons_in_scope(&env.proc_root),
        room_reachable(env.room_socket.as_deref()),
        store_env(env.store_env.as_deref()),
    ]
}

// --- config ----------------------------------------------------------------------------

/// Keys of `doc` the reference for `f` does not list, as dotted paths.
pub fn unknown_keys(f: &cref::File, doc: &toml::Table) -> Vec<String> {
    let mut out = Vec::new();
    walk(f, "", doc, &mut out);
    out
}

/// A table at `path`, or one below it such as `host.<name>` below `host`.
fn has_table(f: &cref::File, path: &str) -> bool {
    let below = format!("{path}.x");
    f.tables
        .iter()
        .any(|t| cref::probe::matches(t.path, path) || cref::probe::matches(t.path, &below))
}

fn walk(f: &cref::File, path: &str, t: &toml::Table, out: &mut Vec<String>) {
    let tables: Vec<&cref::Table> = f
        .tables
        .iter()
        .filter(|t| cref::probe::matches(t.path, path))
        .collect();
    for (k, v) in t {
        let full = if path.is_empty() {
            k.clone()
        } else {
            format!("{path}.{k}")
        };
        let listed = tables
            .iter()
            .any(|t| t.keys.iter().any(|key| cref::probe::matches(key.key, k)));
        match v {
            toml::Value::Table(sub) if has_table(f, &full) => walk(f, &full, sub, out),
            toml::Value::Array(a)
                if !a.is_empty()
                    && a.iter().all(toml::Value::is_table)
                    && has_table(f, &format!("{full}[]")) =>
            {
                for item in a {
                    if let toml::Value::Table(sub) = item {
                        walk(f, &format!("{full}[]"), sub, out);
                    }
                }
            }
            _ if listed => {}
            _ => out.push(full),
        }
    }
}

/// Parse one TOML config file. `secret` keeps the parser's message out of the row, since it may
/// quote the offending value.
pub fn config_file(dir: &Path, name: &str, secret: bool) -> (Check, Option<toml::Table>) {
    let label = format!("{name} parses");
    let path = dir.join(name);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (row(Status::Info, label, "absent"), None);
        }
        Err(e) => return (row(Status::Fail, label, format!("unreadable: {e}")), None),
    };
    let doc = match text.parse::<toml::Table>() {
        Ok(d) => d,
        Err(e) => {
            let line = e.span().map(|s| {
                text.as_bytes()[..s.start.min(text.len())]
                    .iter()
                    .filter(|b| **b == b'\n')
                    .count()
                    + 1
            });
            let at = line.map_or(String::new(), |l| format!(" at line {l}"));
            // A secret file's message is kept only when it names a key, never a value.
            let msg = e.message().lines().last().unwrap_or("").trim().to_string();
            let why = if !secret || msg.starts_with("duplicate key") {
                format!("not TOML{at}: {msg}")
            } else {
                format!("not TOML{at}")
            };
            // providers.toml is read line by line, so the daemon still loads a file TOML refuses.
            let st = if name == "providers.toml" {
                Status::Warn
            } else {
                Status::Fail
            };
            return (
                row(st, label, format!("{why}; unknown keys not checked"))
                    .fix([format!("$EDITOR {}", path.display())]),
                None,
            );
        }
    };
    let real = match name {
        "leticode.toml" => {
            leticode_config::LeticodeConfig::load_as(&path, leticode_config::Level::User).err()
        }
        "hosts.toml" => letibot_tools::remote::Hosts::parse(&text).err(),
        _ => None,
    };
    if let Some(why) = real {
        return (
            row(Status::Fail, label, why).fix([format!("$EDITOR {}", path.display())]),
            Some(doc),
        );
    }
    let Some(reference) = cref::find(name) else {
        return (row(Status::Ok, label, "parses"), Some(doc));
    };
    let unknown = unknown_keys(reference, &doc);
    let c = if unknown.is_empty() {
        row(Status::Ok, label, "no unknown keys")
    } else {
        row(
            Status::Warn,
            label,
            format!("unknown: {}", unknown.join(", ")),
        )
        .fix([
            format!("letibot --config-reference {name}   # the keys it reads"),
            format!("$EDITOR {}", path.display()),
        ])
    };
    (c, Some(doc))
}

pub fn mode_check(path: &Path, label: &str, want: u32, missing: Status) -> Check {
    match std::fs::symlink_metadata(path) {
        Err(_) => row(missing, label, "absent"),
        Ok(m) => {
            let mode = m.permissions().mode() & 0o777;
            if mode & 0o077 == 0 {
                row(Status::Ok, label, format!("{mode:04o}"))
            } else {
                row(
                    Status::Fail,
                    label,
                    format!("{mode:04o}, readable by others"),
                )
                .fix([format!("chmod {want:o} {}", path.display())])
            }
        }
    }
}

/// The generated reference block at the top of `path` is the current one.
pub fn header_current(path: &Path, name: &str) -> Check {
    let label = format!("{name} reference header");
    let (Ok(text), Some(f)) = (std::fs::read_to_string(path), cref::find(name)) else {
        return row(Status::Info, label, "absent");
    };
    match cref::splice(&text, f) {
        Ok(new) if new == text => row(Status::Ok, label, "current"),
        Ok(_) if !text.contains(cref::BEGIN) => row(Status::Warn, label, "none")
            .fix([format!("letibot --config-header {}", path.display())]),
        Ok(_) => row(Status::Warn, label, "stale")
            .fix([format!("letibot --config-header {}", path.display())]),
        Err(why) => row(Status::Warn, label, why),
    }
}

pub fn config(env: &Env) -> Vec<Check> {
    let dir = &env.config_dir;
    let mut out = Vec::new();
    for (name, secret) in [
        ("providers.toml", true),
        ("leticode.toml", false),
        ("hosts.toml", false),
    ] {
        out.push(config_file(dir, name, secret).0);
    }
    out.push(mode_check(
        &dir.join("providers.toml"),
        "providers.toml mode 0600",
        0o600,
        Status::Info,
    ));
    out.push(mode_check(
        &dir.join("hosts.toml"),
        "hosts.toml mode 0600",
        0o600,
        Status::Info,
    ));
    for name in ["providers.toml", "leticode.toml"] {
        out.push(header_current(&dir.join(name), name));
    }
    out.push(mode_check(
        &dir.join("keys"),
        "keys/ mode 0700",
        0o700,
        Status::Info,
    ));
    out
}

// --- models ----------------------------------------------------------------------------

fn table<'a>(doc: &'a toml::Table, key: &str) -> Option<&'a toml::Table> {
    doc.get(key).and_then(toml::Value::as_table)
}

fn str_at<'a>(t: &'a toml::Table, key: &str) -> Option<&'a str> {
    t.get(key).and_then(toml::Value::as_str)
}

/// `host:port` of a URL or a bare address.
fn host_port(url: &str) -> String {
    let s = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    s.split('/').next().unwrap_or(s).to_string()
}

fn on_box(addr: &str) -> bool {
    addr.starts_with("127.") || addr.starts_with("localhost:") || addr.starts_with("[::1]")
}

/// The on-box model servers: `$LETIBOT_ENDPOINT`, and each `[model.NAME]` url on this box.
pub fn local_servers(
    providers: Option<&toml::Table>,
    env_endpoint: Option<&str>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if let Some(e) = env_endpoint {
        out.push(("LETIBOT_ENDPOINT".into(), host_port(e)));
    }
    if let Some(models) = providers.and_then(|p| table(p, "model")) {
        for (name, v) in models {
            let Some(t) = v.as_table() else { continue };
            if let Some(u) = str_at(t, "url").or_else(|| str_at(t, "endpoint")) {
                let a = host_port(u);
                if on_box(&a) && !out.iter().any(|(_, x)| *x == a) {
                    out.push((name.clone(), a));
                }
            }
        }
    }
    out
}

/// `GET /health` answered 200.
pub fn health(addr: &str) -> Result<(), String> {
    use std::net::ToSocketAddrs;
    let sa = addr
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("no address")?;
    let mut s =
        std::net::TcpStream::connect_timeout(&sa, PROBE_BOUND).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(PROBE_BOUND)).ok();
    s.set_write_timeout(Some(PROBE_BOUND)).ok();
    write!(s, "GET /health HTTP/1.0\r\nHost: {addr}\r\n\r\n").map_err(|e| e.to_string())?;
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).map_err(|e| e.to_string())?;
    let head = String::from_utf8_lossy(&buf[..n]);
    let code = head.split_whitespace().nth(1).unwrap_or("");
    if code == "200" {
        Ok(())
    } else {
        Err(format!(
            "answered {}",
            if code.is_empty() { "nothing" } else { code }
        ))
    }
}

pub fn embeddings(leticode: Option<&toml::Table>, data_dir: &Path) -> Check {
    let label = "embeddings model";
    let Some(e) = leticode.and_then(|d| table(d, "embeddings")) else {
        return row(Status::Info, label, "mode off");
    };
    let mode = str_at(e, "mode").unwrap_or("off");
    if mode != "internal" {
        return row(Status::Info, label, format!("mode {mode}"));
    }
    let dir = match str_at(e, "model") {
        Some(m) => PathBuf::from(m),
        None => {
            let rev =
                str_at(e, "builtin_revision").unwrap_or(letibot_embeddings::builtin::REVISION);
            data_dir
                .join("models")
                .join(letibot_embeddings::BUILTIN_MODEL)
                .join(rev)
        }
    };
    let missing: Vec<&str> = ["config.json", "tokenizer.json", "model.safetensors"]
        .into_iter()
        .filter(|f| !dir.join(f).is_file())
        .collect();
    if missing.is_empty() {
        return row(Status::Ok, label, dir.display().to_string());
    }
    let download = e
        .get("download")
        .and_then(toml::Value::as_bool)
        .unwrap_or(true);
    let fix = if str_at(e, "model").is_some() {
        format!("# put {} in {}", missing.join(", "), dir.display())
    } else if download {
        "letibot --restart   # in any folder: the next daemon fetches it".to_string()
    } else {
        "# set download = true under [embeddings] in leticode.toml, then: letibot --restart"
            .to_string()
    };
    row(
        Status::Warn,
        label,
        format!("{} lacks {}", dir.display(), missing.join(", ")),
    )
    .fix([fix])
}

pub fn models(env: &Env) -> Vec<Check> {
    let mut out = Vec::new();
    let providers = config_file(&env.config_dir, "providers.toml", true).1;
    let servers = local_servers(providers.as_ref(), env.endpoint_env.as_deref());
    if servers.is_empty() {
        out.push(row(Status::Info, "local model server", "none configured"));
    }
    for (name, addr) in servers {
        let label = format!("local model {name}");
        out.push(match health(&addr) {
            Ok(()) => row(Status::Ok, label, format!("{addr} /health ok")),
            Err(why) => row(Status::Warn, label, format!("{addr}: {why}")).fix([format!(
                "# start the server on {addr}, then check: curl -sf http://{addr}/health"
            )]),
        });
    }
    let leticode = config_file(&env.config_dir, "leticode.toml", false).1;
    out.push(embeddings(leticode.as_ref(), &env.data_dir));
    out
}

// --- Claude Code -----------------------------------------------------------------------

fn read_json(p: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

pub fn plugin_enabled(settings: &Path) -> Check {
    let label = "leticode plugin enabled";
    let fix = ["claude plugin install leticode@leticode"];
    let Some(v) = read_json(settings) else {
        return row(
            Status::Warn,
            label,
            format!("{} unreadable", settings.display()),
        )
        .fix(fix);
    };
    let on: Vec<String> = v
        .get("enabledPlugins")
        .and_then(|p| p.as_object())
        .map(|m| {
            m.iter()
                .filter(|(k, v)| k.starts_with("leticode@") && v.as_bool() == Some(true))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    if on.is_empty() {
        row(Status::Warn, label, "not in enabledPlugins").fix(fix)
    } else {
        row(Status::Ok, label, on.join(", "))
    }
}

pub fn no_user_server(claude_json: &Path) -> Check {
    let label = "no user-scope leticode MCP server";
    let Some(v) = read_json(claude_json) else {
        return row(
            Status::Info,
            label,
            format!("{} unreadable", claude_json.display()),
        );
    };
    let dup = v
        .get("mcpServers")
        .and_then(|m| m.as_object())
        .is_some_and(|m| m.contains_key("leticode"));
    if dup {
        row(
            Status::Fail,
            label,
            "mcpServers.leticode duplicates the plugin",
        )
        .fix(["claude mcp remove -s user leticode"])
    } else {
        row(Status::Ok, label, "none")
    }
}

/// The text of a `claude` alias or function in `aliases`, if it has one.
pub fn claude_alias(aliases: &str) -> Option<String> {
    let mut lines = aliases.lines();
    while let Some(l) = lines.next() {
        let t = l.trim_start();
        if t.starts_with("alias claude=") {
            return Some(t.to_string());
        }
        if t.starts_with("claude()") || t.starts_with("function claude") {
            let mut body = t.to_string();
            for l in lines.by_ref() {
                body.push('\n');
                body.push_str(l);
                if l.trim_start().starts_with('}') {
                    break;
                }
            }
            return Some(body);
        }
    }
    None
}

pub fn channel_flag(source: &str, text: &str) -> Check {
    let label = "claude channel flag";
    let flag = "--dangerously-load-development-channels";
    let fix = format!("# in {source}: {flag} plugin:leticode@leticode");
    if text.contains("server:leticode") {
        row(
            Status::Fail,
            label,
            format!("{source} loads server:leticode"),
        )
        .fix([fix])
    } else if text.contains(flag) && text.contains("plugin:leticode@") {
        row(Status::Ok, label, format!("{source}: plugin:leticode"))
    } else {
        row(
            Status::Warn,
            label,
            format!("{source} loads no leticode channel"),
        )
        .fix([fix])
    }
}

pub fn claude(env: &Env) -> Vec<Check> {
    let settings = env.home.join(".claude/settings.json");
    if !env.home.join(".claude").is_dir() && !env.home.join(".claude.json").is_file() {
        return vec![row(Status::Info, "Claude Code", "not set up for this user")];
    }
    let mut out = vec![
        plugin_enabled(&settings),
        no_user_server(&env.home.join(".claude.json")),
    ];
    let alias = std::fs::read_to_string(env.home.join(".bash_aliases"))
        .ok()
        .and_then(|t| claude_alias(&t));
    let wrapper = env.which("claude").filter(|p| {
        std::fs::metadata(p).is_ok_and(|m| m.len() < 64 * 1024) && head_bytes(p, 2) == b"#!"
    });
    match (alias, wrapper) {
        (Some(a), _) => out.push(channel_flag("~/.bash_aliases", &a)),
        (None, Some(w)) => {
            let text = std::fs::read_to_string(&w).unwrap_or_default();
            out.push(channel_flag(&w.display().to_string(), &text));
        }
        (None, None) => out.push(row(
            Status::Info,
            "claude channel flag",
            "no alias or wrapper visible",
        )),
    }
    out.extend(bridges(&env.proc_root, env.cpu_sample));
    out
}

// --- bridges ---------------------------------------------------------------------------

fn exe_base(p: &Proc) -> &str {
    base(p.exe.strip_suffix(" (deleted)").unwrap_or(&p.exe))
}

fn is_claude(p: &Proc) -> bool {
    exe_base(p) == "claude" || p.argv.first().is_some_and(|a| base(a) == "claude")
}

fn is_mcp(p: &Proc) -> bool {
    exe_base(p) == "letibot-mcp" || p.argv.first().is_some_and(|a| base(a) == "letibot-mcp")
}

/// The nearest `claude` above `pid`.
fn claude_of(all: &[Proc], pid: u32) -> Option<u32> {
    let mut at = pid;
    for _ in 0..64 {
        let p = all.iter().find(|p| p.pid == at)?;
        if is_claude(p) {
            return Some(p.pid);
        }
        at = p.ppid;
        if at <= 1 {
            return None;
        }
    }
    None
}

fn cwd_text(p: Option<&Proc>) -> String {
    p.and_then(|p| p.cwd.as_ref())
        .map_or("?".into(), |c| c.display().to_string())
}

/// Bridge processes under each Claude Code session. `sample` is how long CPU is measured over;
/// zero skips that row.
pub fn bridges(proc_root: &Path, sample: Duration) -> Vec<Check> {
    let all = procs(proc_root);
    let claudes: Vec<&Proc> = all.iter().filter(|p| is_claude(p)).collect();
    let servers: Vec<&Proc> = all
        .iter()
        .filter(|p| is_mcp(p) && p.argv.len() <= 1)
        .collect();
    let waiters: Vec<&Proc> = all
        .iter()
        .filter(|p| is_mcp(p) && p.argv.get(1).is_some_and(|a| a == "room-wait"))
        .collect();
    let mut out = Vec::new();

    let mut doubled = Vec::new();
    for c in &claudes {
        let n = servers.iter().filter(|s| s.ppid == c.pid).count();
        if n > 1 {
            doubled.push(format!("claude {} in {} has {n}", c.pid, cwd_text(Some(c))));
        }
    }
    out.push(if doubled.is_empty() {
        row(
            Status::Ok,
            "one bridge per Claude session",
            format!("{} session(s)", claudes.len()),
        )
    } else {
        row(
            Status::Fail,
            "one bridge per Claude session",
            doubled.join("; "),
        )
        .fix([
            "claude mcp remove -s user leticode",
            "# then restart each of those Claude sessions",
        ])
    });

    let old: Vec<String> = servers
        .iter()
        .filter(|s| s.exe.ends_with(" (deleted)") || s.exe.contains("/target/"))
        .map(|s| format!("{} ({})", s.pid, s.exe))
        .collect();
    out.push(if old.is_empty() {
        row(
            Status::Ok,
            "bridges run an installed binary",
            format!("{} running", servers.len()),
        )
    } else {
        row(
            Status::Warn,
            "bridges run an installed binary",
            old.join(", "),
        )
        .fix(["# a build dir or a deleted binary: restart the Claude session that owns each"])
    });

    if !sample.is_zero() && !servers.is_empty() {
        let before: Vec<(u32, u64)> = servers.iter().map(|s| (s.pid, s.cpu)).collect();
        let t0 = Instant::now();
        std::thread::sleep(sample);
        let after = procs(proc_root);
        let secs = t0.elapsed().as_secs_f64();
        let hz = match unsafe { libc::sysconf(libc::_SC_CLK_TCK) } {
            n if n > 0 => n as f64,
            _ => 100.0,
        };
        let hot: Vec<String> = before
            .iter()
            .filter_map(|(pid, c0)| {
                let c1 = after.iter().find(|p| p.pid == *pid)?.cpu;
                let pct = (c1.saturating_sub(*c0)) as f64 / hz / secs * 100.0;
                (pct > 50.0).then(|| format!("{pid} at {pct:.0}%"))
            })
            .collect();
        out.push(if hot.is_empty() {
            row(Status::Ok, "no bridge spinning", "all under 50% CPU")
        } else {
            row(Status::Warn, "no bridge spinning", hot.join(", "))
                .fix(["# restart the owning Claude session on the current build"])
        });
    }

    let bare: Vec<String> = claudes
        .iter()
        .filter(|c| !waiters.iter().any(|w| w.cwd.is_some() && w.cwd == c.cwd))
        .map(|c| cwd_text(Some(c)))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    out.push(if claudes.is_empty() {
        row(
            Status::Info,
            "room waiter per Claude folder",
            "no Claude session",
        )
    } else if bare.is_empty() {
        row(
            Status::Ok,
            "room waiter per Claude folder",
            format!("{} waiter(s)", waiters.len()),
        )
    } else {
        row(
            Status::Warn,
            "room waiter per Claude folder",
            format!("none in {}", bare.join(", ")),
        )
        .fix(["# arm the waiter in each: the leticode:room-waiter skill"])
    });

    let mut loops: std::collections::BTreeMap<u32, usize> = Default::default();
    for p in &all {
        if p.argv.iter().any(|a| a.contains("until [ -s"))
            && let Some(c) = claude_of(&all, p.pid)
        {
            *loops.entry(c).or_default() += 1;
        }
    }
    let many: Vec<String> = loops
        .iter()
        .filter(|(_, n)| **n >= 5)
        .map(|(c, n)| format!("claude {c}: {n}"))
        .collect();
    out.push(if many.is_empty() {
        row(Status::Ok, "no runaway wait loops", "under 5 per session")
    } else {
        row(Status::Warn, "no runaway wait loops", many.join(", "))
            .fix(["# restart that Claude session (R235)"])
    });
    out
}

// --- disk ------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Floor {
    Bytes(u64),
    Fraction(f64),
}

/// `20G`, `512M`, `5%`, `0.05` or a byte count.
pub fn parse_floor(s: &str) -> Option<Floor> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        return p
            .trim()
            .parse::<f64>()
            .ok()
            .map(|f| Floor::Fraction(f / 100.0));
    }
    let (num, mul) = match s.chars().last()? {
        'K' | 'k' => (&s[..s.len() - 1], 1u64 << 10),
        'M' | 'm' => (&s[..s.len() - 1], 1 << 20),
        'G' | 'g' => (&s[..s.len() - 1], 1 << 30),
        'T' | 't' => (&s[..s.len() - 1], 1 << 40),
        _ => (s, 1),
    };
    let n: f64 = num.trim().parse().ok()?;
    if mul == 1 && n < 1.0 {
        Some(Floor::Fraction(n))
    } else {
        Some(Floor::Bytes((n * mul as f64) as u64))
    }
}

impl Floor {
    fn bytes(self, total: u64) -> u64 {
        match self {
            Floor::Bytes(b) => b,
            Floor::Fraction(f) => (total as f64 * f) as u64,
        }
    }
}

pub fn human(b: u64) -> String {
    let units = ["B", "K", "M", "G", "T", "P"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < units.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b}B")
    } else {
        format!("{v:.1}{}", units[i])
    }
}

/// (free, total) bytes of the filesystem holding `p`, or its nearest existing ancestor.
fn space(p: &Path) -> Option<(u64, u64)> {
    let mut at = p;
    while !at.exists() {
        at = at.parent()?;
    }
    let c = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let f = s.f_frsize as u64;
    Some((s.f_bavail as u64 * f, s.f_blocks as u64 * f))
}

pub fn free_check(label: &str, path: &Path, floor: Floor, floor_src: &str) -> Check {
    let label = format!("free on {label}");
    let Some((free, total)) = space(path) else {
        return row(Status::Info, label, format!("{} not there", path.display()));
    };
    let min = floor.bytes(total);
    let shown = format!(
        "{} free of {} (floor {floor_src})",
        human(free),
        human(total)
    );
    if free >= min {
        row(Status::Ok, label, shown)
    } else {
        row(Status::Fail, label, shown).fix([
            "letibot holds   # what each owner keeps on disk".to_string(),
            format!("du -sh {}/* | sort -h | tail", path.display()),
        ])
    }
}

pub fn targets(build_dir: &Path, proc_root: &Path, bound: Duration) -> Vec<Check> {
    let dirs = crate::targets::scan(build_dir, proc_root, bound);
    if dirs.is_empty() {
        return vec![row(
            Status::Info,
            format!("{}/target-*", build_dir.display()),
            "none",
        )];
    }
    dirs.iter()
        .map(|d| {
            let shown = if d.whole {
                human(d.bytes)
            } else {
                format!("at least {}", human(d.bytes))
            };
            let label = d.path.display().to_string();
            if d.users.is_empty() {
                row(Status::Warn, label, format!("{shown}, no live gate"))
                    .fix(["letibot gc --targets --now".to_string()])
            } else {
                row(Status::Ok, label, format!("{shown}, in use"))
            }
        })
        .collect()
}

pub fn disk_floor(leticode: Option<&toml::Table>) -> (Floor, String) {
    let raw = leticode
        .and_then(|d| table(d, "exec"))
        .and_then(|e| e.get("disk_floor"))
        .and_then(|v| match v {
            toml::Value::String(s) => Some(s.clone()),
            toml::Value::Integer(i) => Some(i.to_string()),
            toml::Value::Float(f) => Some(f.to_string()),
            _ => None,
        });
    match raw.as_deref().and_then(parse_floor) {
        Some(f) => (f, raw.unwrap_or_default()),
        None => (
            parse_floor(DEFAULT_FLOOR).expect("the default parses"),
            format!("{DEFAULT_FLOOR}, default"),
        ),
    }
}

pub fn disk(env: &Env) -> Vec<Check> {
    let leticode = config_file(&env.config_dir, "leticode.toml", false).1;
    let (floor, src) = disk_floor(leticode.as_ref());
    let mut out = vec![
        free_check("data dir", &env.data_dir, floor, &src),
        free_check(
            &env.build_dir.display().to_string(),
            &env.build_dir,
            floor,
            &src,
        ),
    ];
    if let Some(t) = &env.cargo_target_dir {
        out.push(free_check("$CARGO_TARGET_DIR", t, floor, &src));
    }
    out.extend(targets(&env.build_dir, &env.proc_root, SIZE_BOUND));
    out.push(gate_lock(
        &env.build_dir.join("gate.lock"),
        &env.proc_root,
        now_secs(),
    ));
    out.push(zero_refs(&env.home.join("Projects")));
    out
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Processes holding `file` open.
fn holders(proc_root: &Path, file: &Path) -> Vec<u32> {
    procs(proc_root)
        .iter()
        .filter(|p| {
            std::fs::read_dir(proc_root.join(p.pid.to_string()).join("fd")).is_ok_and(|fds| {
                fds.flatten()
                    .any(|f| std::fs::read_link(f.path()).is_ok_and(|l| l == file))
            })
        })
        .map(|p| p.pid)
        .collect()
}

pub fn gate_lock(lock: &Path, proc_root: &Path, now: u64) -> Check {
    let label = "gate lock";
    if !lock.exists() {
        return row(Status::Info, label, format!("no {}", lock.display()));
    }
    let held = holders(proc_root, lock);
    let old: Vec<(u32, u64)> = held
        .iter()
        .filter_map(|pid| Some((*pid, now.saturating_sub(started_at(proc_root, *pid)?))))
        .filter(|(_, age)| *age > 2 * 3600)
        .collect();
    if held.is_empty() {
        row(Status::Ok, label, "free")
    } else if old.is_empty() {
        row(Status::Ok, label, format!("held by {}", held.len()))
    } else {
        let mut fix =
            vec!["# a gate past 2h is stuck; end it and never remove the lock".to_string()];
        fix.extend(
            old.iter()
                .map(|(pid, age)| format!("kill {pid}   # {}h", age / 3600)),
        );
        row(
            Status::Warn,
            label,
            format!("held over 2h by {}", old.len()),
        )
        .fix(fix)
    }
}

fn zero_files(dir: &Path, out: &mut usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let Ok(m) = e.metadata() else { continue };
        if m.is_dir() {
            zero_files(&e.path(), out);
        } else if m.len() == 0 {
            *out += 1;
        }
    }
}

/// Zero-byte files under `.git/refs` of each checkout one or two levels under `projects`.
pub fn zero_refs(projects: &Path) -> Check {
    let label = "no zero-byte git refs";
    let mut repos = Vec::new();
    let level = |d: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(d)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    for a in level(projects) {
        repos.push(a.clone());
        repos.extend(level(&a));
    }
    let mut bad = Vec::new();
    for r in repos {
        let refs = r.join(".git/refs");
        if !refs.is_dir() {
            continue;
        }
        let mut n = 0;
        zero_files(&refs, &mut n);
        if n > 0 {
            bad.push((refs, n));
        }
    }
    if bad.is_empty() {
        return row(Status::Ok, label, "none");
    }
    let shown: Vec<String> = bad
        .iter()
        .map(|(r, n)| format!("{} ({n})", r.display()))
        .collect();
    row(Status::Warn, label, shown.join(", ")).fix(
        bad.iter()
            .map(|(r, _)| format!("find {} -type f -size 0 -delete", r.display())),
    )
}

// --- firecode --------------------------------------------------------------------------

/// The `N ok, M warning(s), K failure(s)` line of `firecode doctor`.
pub fn firecode_summary(out: &str) -> Option<String> {
    out.lines()
        .map(str::trim)
        .rev()
        .find(|l| l.contains(" ok,") && l.contains("failure"))
        .map(str::to_string)
}

pub fn taps(net_dir: &Path, firecode: &Path) -> Check {
    let n = std::fs::read_dir(net_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("fccode"))
                .count()
        })
        .unwrap_or(0);
    if n > 0 {
        row(Status::Ok, "firecode taps", format!("{n} tap(s)"))
    } else {
        row(Status::Warn, "firecode taps", "none (gone after a reboot)")
            .fix([format!("sudo {} net-setup --count 14", firecode.display())])
    }
}

fn firecode(env: &Env) -> Option<Check> {
    let bin = env.which("firecode")?;
    if !env.run_programs {
        return None;
    }
    let started = SystemTime::now();
    let label = "firecode doctor";
    Some(
        match run_bounded(&bin, &["doctor".to_string()], FIRECODE_BOUND) {
            Ended::Answered(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                let sum = firecode_summary(&text).unwrap_or_else(|| "no summary line".into());
                if o.status.success() {
                    row(Status::Ok, label, sum)
                } else {
                    row(Status::Warn, label, sum).fix(["firecode doctor   # its own what-to-do"])
                }
            }
            Ended::TimedOut => row(
                Status::Warn,
                label,
                format!(
                    "no answer in {}s",
                    started.elapsed().map_or(0, |d| d.as_secs())
                ),
            )
            .fix(["firecode doctor"]),
            Ended::Unstartable(e) => row(Status::Warn, label, e).fix(["firecode doctor"]),
        },
    )
}

#[cfg(test)]
mod tests;
