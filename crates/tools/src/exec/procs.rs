//! Finding processes by what they look like, without ever finding yourself.
//!
//! The one finder behind the `pkill` tool and the monitor's `process` pattern.
//! Its whole reason to exist is the failure this tree has paid for eight times
//! (`monitor.rs`, module docs): a pattern matched against `ps` output matches
//! the shell that is running the `ps`, because the shell's own command line
//! carries the pattern. Every one of those was a shell. There is no shell here:
//! the finder reads `/proc` in-process, and **this process, every ancestor of
//! it, pid 1, and everything the process host has declared protected are
//! removed from the candidates before the pattern is looked at**. Self-match is
//! not refused; it is unspellable, because the self is not in the set.
//!
//! # A handle, not a pattern, is what gets acted on
//!
//! [`find`] answers with [`ProcInfo`]s — pid **and start time**. A pid is
//! reused; a (pid, start time) pair is not. [`alive`] and [`kill`] take the
//! pair, so a process that exited and whose pid came back as something else is
//! reported gone, not killed. The pattern is for finding; the handle is for
//! acting.
//!
//! # Only this user's processes
//!
//! `/proc/<pid>/status` `Uid` is compared to the caller's own uid. Another
//! user's process is not listed, not because it could be killed (it could not)
//! but because a listing that names what you cannot act on reads as a claim
//! that you can.

use std::path::Path;
use std::time::Duration;

/// One process, as `/proc` describes it. `start` is `starttime` from `stat`
/// in clock ticks since boot — the half of the handle that survives pid reuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: u32,
    pub start: u64,
    pub comm: String,
    pub cmdline: String,
    /// How long ago it started.
    pub age: Duration,
    /// `/proc/<pid>/stat`'s state letter. `Z` is a zombie: dead, and still in
    /// `/proc` until its parent reaps it — which, after a kill from here, may
    /// be never soon. [`alive`] treats it as gone.
    pub state: char,
    /// CPU time consumed so far, user plus system, in clock ticks.
    pub cpu_ticks: u64,
    /// Resident set, in kilobytes.
    pub rss_kb: u64,
}

impl ProcInfo {
    /// CPU consumed over the process's lifetime, as a percentage of one core:
    /// the `top`-shaped number for "what is this box busy with".
    pub fn cpu_percent(&self) -> f64 {
        let secs = self.age.as_secs_f64();
        if secs <= 0.0 {
            return 0.0;
        }
        (self.cpu_ticks as f64 / clock_ticks() as f64) / secs * 100.0
    }
}

impl ProcInfo {
    /// `12345  2h03m  sleep 300` — for a listing.
    pub fn line(&self) -> String {
        format!(
            "{:>7}  {:>8}  {}",
            self.pid,
            age_word(self.age),
            if self.cmdline.is_empty() {
                format!("[{}]", self.comm)
            } else {
                self.cmdline.clone()
            }
        )
    }
}

pub fn age_word(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s < 86_400 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d{:02}h", s / 86_400, (s % 86_400) / 3600)
    }
}

/// Read one process. `None` when it is gone or not ours to read.
pub fn read(pid: u32) -> Option<ProcInfo> {
    let dir = Path::new("/proc").join(pid.to_string());
    let stat = std::fs::read_to_string(dir.join("stat")).ok()?;
    // `pid (comm) state ppid …` — comm may hold spaces and parens, so split at
    // the LAST `)`.
    let close = stat.rfind(')')?;
    let comm = stat[stat.find('(')? + 1..close].to_string();
    let rest: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    // After `)`: state(0) ppid(1) pgrp session tty tpgid flags minflt cminflt
    // majflt cmajflt utime stime cutime cstime priority nice threads itrealvalue
    // starttime(19)
    let state = rest.first()?.chars().next()?;
    let ppid: u32 = rest.get(1)?.parse().ok()?;
    let start: u64 = rest.get(19)?.parse().ok()?;
    let utime: u64 = rest.get(11).and_then(|v| v.parse().ok()).unwrap_or(0);
    let stime: u64 = rest.get(12).and_then(|v| v.parse().ok()).unwrap_or(0);
    // rss(21) is in pages; a page is 4 KiB on every box this runs on.
    let rss_pages: u64 = rest.get(21).and_then(|v| v.parse().ok()).unwrap_or(0);
    let cmdline = std::fs::read(dir.join("cmdline"))
        .map(|b| {
            b.split(|&c| c == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let age = age_of(start);
    Some(ProcInfo {
        pid,
        ppid,
        start,
        comm,
        cmdline,
        age,
        state,
        cpu_ticks: utime + stime,
        rss_kb: rss_pages * 4,
    })
}

fn uid_of(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|s| s.parse().ok())
}

fn clock_ticks() -> u64 {
    // SAFETY: sysconf reads a constant.
    let hz = unsafe { libc_sysconf_clk_tck() };
    if hz <= 0 { 100 } else { hz as u64 }
}

// `libc` is not a dependency of this crate (its manifest says so, on purpose);
// `_SC_CLK_TCK` is 2 on Linux and sysconf is in libc, which is always linked.
unsafe extern "C" {
    fn sysconf(name: i32) -> i64;
}
unsafe fn libc_sysconf_clk_tck() -> i64 {
    unsafe { sysconf(2) }
}

fn boot_uptime() -> Option<Duration> {
    let up = std::fs::read_to_string("/proc/uptime").ok()?;
    let secs: f64 = up.split_whitespace().next()?.parse().ok()?;
    Some(Duration::from_secs_f64(secs))
}

fn age_of(start_ticks: u64) -> Duration {
    let Some(up) = boot_uptime() else {
        return Duration::ZERO;
    };
    let started = Duration::from_secs_f64(start_ticks as f64 / clock_ticks() as f64);
    up.saturating_sub(started)
}

/// This process and every ancestor, to the root. What the finder never returns.
pub fn self_and_ancestors() -> Vec<u32> {
    let mut out = Vec::new();
    let mut pid = std::process::id();
    let mut depth = 0;
    while pid > 1 && depth < 64 {
        out.push(pid);
        let Some(p) = read(pid) else { break };
        pid = p.ppid;
        depth += 1;
    }
    out
}

/// Every process of this user whose command line (or comm) contains `pattern`
/// as a substring — minus this process, its ancestors, pid 1 and `exclude`.
/// Case-sensitive: a pattern is a name, and `Sleep` is not `sleep`.
pub fn find(pattern: &str, exclude: &[u32]) -> Vec<ProcInfo> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Vec::new();
    }
    let mut out = all(exclude);
    out.retain(|p| p.cmdline.contains(pattern) || p.comm.contains(pattern));
    out
}

/// Every live process of this user, minus this process, its ancestors, pid 1
/// and `exclude`. The listing `ps` answers questions over; [`find`] is this
/// with a substring.
pub fn all(exclude: &[u32]) -> Vec<ProcInfo> {
    let me = self_and_ancestors();
    // SAFETY: getuid cannot fail.
    let my_uid = unsafe { getuid() };
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pid <= 1 || me.contains(&pid) || exclude.contains(&pid) {
            continue;
        }
        if uid_of(pid) != Some(my_uid) {
            continue;
        }
        let Some(p) = read(pid) else { continue };
        if p.state == 'Z' {
            continue;
        }
        out.push(p);
    }
    out.sort_by_key(|p| p.pid);
    out
}

unsafe extern "C" {
    fn getuid() -> u32;
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Is this exact process — pid AND start time — still there and not a zombie?
pub fn alive(pid: u32, start: u64) -> bool {
    read(pid).is_some_and(|p| p.start == start && p.state != 'Z')
}

/// The signals a caller may send, by name. No numbers: a number is how `-9`
/// gets typed at the wrong thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
    Int,
    Hup,
}

impl Signal {
    pub fn parse(s: &str) -> Option<Signal> {
        match s
            .trim()
            .trim_start_matches("SIG")
            .to_ascii_lowercase()
            .as_str()
        {
            "term" | "terminate" => Some(Signal::Term),
            "kill" => Some(Signal::Kill),
            "int" | "interrupt" => Some(Signal::Int),
            "hup" | "hangup" => Some(Signal::Hup),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Signal::Term => "TERM",
            Signal::Kill => "KILL",
            Signal::Int => "INT",
            Signal::Hup => "HUP",
        }
    }

    fn number(&self) -> i32 {
        match self {
            Signal::Term => 15,
            Signal::Kill => 9,
            Signal::Int => 2,
            Signal::Hup => 1,
        }
    }
}

/// Why a kill was not sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillRefused {
    /// The pid is not that process any more.
    Gone,
    /// This process or an ancestor. Not a check: the finder never lists these,
    /// so reaching here means a pid was typed by hand.
    Self_,
    Os(String),
}

/// Send `sig` to the exact process. Refuses self and ancestors whatever the
/// caller says, and a pid whose start time no longer matches.
pub fn kill_exact(pid: u32, start: u64, sig: Signal) -> Result<(), KillRefused> {
    if pid <= 1 || self_and_ancestors().contains(&pid) {
        return Err(KillRefused::Self_);
    }
    if !alive(pid, start) {
        return Err(KillRefused::Gone);
    }
    // SAFETY: kill with a real signal to a pid we just confirmed is the process
    // the caller named.
    let rc = unsafe { kill(pid as i32, sig.number()) };
    if rc != 0 {
        return Err(KillRefused::Os(std::io::Error::last_os_error().to_string()));
    }
    Ok(())
}

/// A monitor condition over process **handles**: fires when every one of them
/// is gone. The handles come from [`find`] at declaration — a pattern is
/// consumed there and never looked at again — or from a pid the caller named,
/// resolved to its start time at declaration. This is the `until ! pgrep -f X`
/// loop the transcripts are full of, keyed on what it should have been keyed on.
#[derive(Debug)]
pub struct ProcessCondition {
    handles: Vec<ProcInfo>,
    what: String,
}

impl ProcessCondition {
    /// Watch these. Empty is refused by the caller, not here: a monitor over
    /// nothing would fire at once and read as news.
    pub fn over(handles: Vec<ProcInfo>, what: impl Into<String>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(ProcessCondition {
            handles,
            what: what.into(),
        })
    }

    pub fn handles(&self) -> &[ProcInfo] {
        &self.handles
    }
}

impl super::monitor::Condition for ProcessCondition {
    fn met(&self) -> Option<String> {
        let still: Vec<&ProcInfo> = self
            .handles
            .iter()
            .filter(|p| alive(p.pid, p.start))
            .collect();
        if !still.is_empty() {
            return None;
        }
        Some(format!(
            "{}: all {} gone — {}",
            self.what,
            self.handles.len(),
            self.handles
                .iter()
                .map(|p| format!("{} ({})", p.pid, p.cmdline))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    fn describe(&self) -> String {
        format!(
            "{} — process(es) {} leaving (by pid and start time; a reused pid is not the \
             same process)",
            self.what,
            self.handles
                .iter()
                .map(|p| p.pid.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn a_process_condition_fires_when_its_handles_are_gone_not_when_a_pid_is_reused() {
        use super::super::monitor::Condition;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .arg("letibot-sh")
            .arg("letibot-procs-cond")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let handle = read(child.id()).unwrap();
        let c = ProcessCondition::over(vec![handle.clone()], "the sleeper");
        assert!(c.met().is_none());
        let _ = child.kill();
        let _ = child.wait();
        let why = c.met().unwrap();
        assert!(why.starts_with("the sleeper: all 1 gone"), "{why}");
        // A handle with the right pid and a wrong start time was never that
        // process: gone from the start.
        let stale = ProcInfo {
            start: handle.start + 1,
            ..handle
        };
        assert!(ProcessCondition::over(vec![stale], "x").met().is_some());
    }

    #[test]
    fn the_finder_never_returns_itself_or_an_ancestor_even_on_a_pattern_they_carry() {
        // The test binary's own path carries the crate's name, and so does the
        // `cargo test -p letibot-tools …` that is our ancestor. A `pgrep -f`
        // for it would list both; this must list neither, whatever else it
        // finds on the box.
        let me = self_and_ancestors();
        assert!(me.contains(&std::process::id()));
        let own_cmdline = read(std::process::id()).unwrap().cmdline;
        assert!(own_cmdline.contains("letibot_tools"), "{own_cmdline}");
        let found = find("letibot_tools", &[]);
        assert!(
            found.iter().all(|p| !me.contains(&p.pid)),
            "found ourselves or an ancestor: {found:?}"
        );
        // And `exclude` removes what the caller names.
        let marker = format!("letibot-procs-excl-{}", std::process::id());
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .arg("letibot-sh")
            .arg(&marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(find(&marker, &[]).len(), 1);
        assert!(find(&marker, &[child.id()]).is_empty());
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_found_child_is_killed_by_its_handle_and_a_stale_handle_is_refused() {
        let marker = format!("letibot-procs-kill-{}", std::process::id());
        // The marker rides as a positional parameter the shell never reads, so
        // it is on the command line and not in the command.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .arg("letibot-sh")
            .arg(&marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let found = find(&marker, &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        let p = &found[0];
        assert_eq!(p.pid, child.id());
        assert!(alive(p.pid, p.start));
        kill_exact(p.pid, p.start, Signal::Term).unwrap();
        let _ = child.wait();
        assert!(!alive(p.pid, p.start));
        assert_eq!(
            kill_exact(p.pid, p.start, Signal::Term),
            Err(KillRefused::Gone)
        );
        assert_eq!(
            kill_exact(std::process::id(), 0, Signal::Term),
            Err(KillRefused::Self_)
        );
    }

    #[test]
    fn signals_are_named_not_numbered_in_the_listing() {
        assert_eq!(Signal::parse("SIGKILL"), Some(Signal::Kill));
        assert_eq!(Signal::parse("term"), Some(Signal::Term));
        assert_eq!(Signal::parse("usr1"), None);
        assert_eq!(
            Signal::parse("9"),
            None,
            "a number is how -9 gets typed at the wrong thing"
        );
        assert_eq!(Signal::Kill.as_str(), "KILL");
        assert_eq!(age_word(Duration::from_secs(7384)), "2h03m");
    }
}
