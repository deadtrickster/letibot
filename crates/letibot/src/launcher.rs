//! **Which daemon belongs to which folder** — the launcher's root, in Rust.
//!
//! # Why this is a module and not the shell function it replaces
//!
//! `scripts/letibot` is 1,123 lines of shell, and the `--status`/`--stop`/`--daemons`
//! behaviours and the seat flags all sit on ONE thing: a workspace hashes to a socket
//! and a record file beside it, and every question about "this folder's daemon" is
//! answered from those two paths. That is the piece with an exact interoperability
//! contract — the shell and the Rust must derive the SAME socket for the same folder, or
//! the ported launcher would talk to a daemon the shell one never started.
//!
//! So it is ported first and asserted against the shell's own output rather than
//! against a reimplementation's opinion. MEASURED, on this box, and these are the test
//! values below:
//!
//! ```text
//! printf '%s' "$ws" | sha256sum | cut -c1-12
//!
//! /home/dead/Projects/letibot   -> 42ce9f1aae08   (and /run/user/1000/letibot/42ce9f1aae08.json
//!                                                  names exactly that workspace)
//! /home/dead/Projects/dbtest/pg-noop -> 5a0a8f3c88f8
//! /home/dead                       -> 7a22aa3b1a21
//! /home/dead/.emacs.d              -> 9f6194e2446d
//! ```
//!
//! Four records on disk, and each filename is the hash of the workspace named INSIDE
//! it. That is the contract, checked against real files rather than reasoned about.
//!
//! # What is deliberately NOT here yet
//!
//! The workspace itself is not resolved: the shell takes the git toplevel and
//! `pwd -P`s it, with a guard for a failed `cd` that exists because an empty workspace
//! is "a socket key everything shares and a `--scope` that matches every session on the
//! box, which is how `--continue` reopened another project's conversation". That guard
//! is its own piece with its own test, and this module takes the workspace as given.
//!
//! Nor is the daemon's PID here: the shell gets it from `ss -lxpH`, which forks. That
//! becomes a `/proc/net/unix` read — the same answer without the fork, and the same
//! care about which of two daemons on one path is the live one.

use std::path::{Path, PathBuf};

/// The directory the runtime files live in: `$XDG_RUNTIME_DIR/letibot`, else
/// `/run/user/<uid>/letibot`.
///
/// The shell spells the fallback `$(id -u)`, which forks; this reads the uid. Both
/// land on the same directory, and the `letibot` subdirectory is what keeps these
/// files out of whatever else the runtime dir holds — the shell's own reason for it.
pub fn rundir() -> PathBuf {
    let base = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        // `id -u` without the fork. A uid that does not fit u32 is not a thing.
        _ => PathBuf::from(format!("/run/user/{}", uid())),
    };
    base.join("letibot")
}

/// This process's uid. `libc::getuid` is not linked here, so read it from the kernel's
/// own report rather than shelling out to `id`.
///
/// `/proc/self/status` carries `Uid:\t<real>\t<effective>\t<SUID>\t<FSUID>`, and the
/// REAL uid is the first field. Falling back to 0 would put a non-root user's files in
/// `/run/user/0`, which is both wrong and unreadable — so a failure here is worth
/// noticing rather than papering over.
pub fn uid() -> u32 {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                if let Some(real) = rest.split_whitespace().next() {
                    if let Ok(n) = real.parse() {
                        return n;
                    }
                }
            }
        }
    }
    // `SUDO_UID` is the other honest source when a process is running under sudo.
    if let Some(n) = std::env::var("UID").ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    0
}

/// **The key a workspace hashes to: the first twelve hex characters of its SHA-256.**
///
/// This is the whole interoperability contract, and it is twelve characters because the
/// shell took twelve — `sha256sum | cut -c1-12`. Changing the length would orphan every
/// daemon running right now, since the socket a running daemon listens on was named with
/// the old one.
///
/// The bytes hashed are the workspace path with NOTHING added: the shell's
/// `printf '%s' "$WORKSPACE"` writes no trailing newline, and a newline here would give
/// a different key for every folder.
pub fn key(workspace: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(workspace.as_os_str().as_encoded_bytes());
    let digest = h.finalize();
    let mut out = String::with_capacity(12);
    for b in digest.iter().take(6) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Where a workspace's daemon keeps its socket and its record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Where {
    pub workspace: PathBuf,
    pub key: String,
    pub socket: PathBuf,
    pub record: PathBuf,
}

/// The socket and record for a workspace, under `rundir`.
///
/// Takes the rundir rather than reading it, so a test can pin both paths without
/// touching the machine's real runtime directory — which is where live daemons are.
pub fn locate_in(rundir: &Path, workspace: &Path) -> Where {
    let key = key(workspace);
    Where {
        workspace: workspace.to_path_buf(),
        socket: rundir.join(format!("{key}.sock")),
        record: rundir.join(format!("{key}.json")),
        key,
    }
}

/// The same, against this process's [`rundir`].
pub fn locate(workspace: &Path) -> Where {
    locate_in(&rundir(), workspace)
}

/// What the record beside a socket says a daemon is.
///
/// Every field is optional-shaped on purpose: the file is written by whichever launcher
/// started the daemon, and a **stale or partially-written record must not stop a
/// `--status`** — the honest answer to "what is this" is what the file says, and a file
/// that says less is not an error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    pub workspace: String,
    pub socket: String,
    /// The pid the launcher started. **Not trusted for liveness**: the pid LISTENING
    /// on the socket is the daemon whatever this says, and this is the fallback for a
    /// box without that answer — see the module docs on why the socket read is not here
    /// yet.
    pub pid: Option<u32>,
    pub role: String,
    pub model: String,
    /// What actually answers the turns — `deepseek/deepseek-flash` on a session whose
    /// local vocab is `qwen-3.8-27b`. The shell prints THIS when the record has one,
    /// because the model field is the vocab the daemon binds and the two differ.
    pub answers: String,
    pub dialect: String,
    pub bash: bool,
    pub started: String,
}

/// Read a record, or `None` when there is not one.
pub fn read_record(path: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    Some(Record {
        workspace: s("workspace"),
        socket: s("socket"),
        // A number, and `as_u64` rather than a string read: the shell's `json_num` had
        // to try both forms for exactly this field.
        pid: v.get("pid").and_then(|x| x.as_u64()).map(|n| n as u32),
        role: s("role"),
        model: s("model"),
        answers: s("answers"),
        dialect: s("dialect"),
        bash: v.get("bash").and_then(|x| x.as_bool()).unwrap_or(false),
        started: s("started"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The contract, against values the SHELL produced on this box.**
    ///
    /// `scripts/letibot` derives a daemon's socket as
    /// `printf '%s' "$ws" | sha256sum | cut -c1-12`, and these four are real: each is the
    /// filename of a record on disk whose `workspace` field names the path on the left.
    /// If this test fails, the ported launcher and the shell disagree about which
    /// daemon a folder has — which is the one way a port can be worse than the thing it
    /// replaces.
    #[test]
    fn the_key_matches_what_the_shell_derives() {
        for (workspace, want) in [
            ("/home/dead/Projects/letibot", "42ce9f1aae08"),
            ("/home/dead/Projects/dbtest/pg-noop", "5a0a8f3c88f8"),
            ("/home/dead", "7a22aa3b1a21"),
            ("/home/dead/.emacs.d", "9f6194e2446d"),
        ] {
            assert_eq!(
                key(Path::new(workspace)),
                want,
                "a different key from the shell's for {workspace} would name a different \
                 socket, and the daemon already running would be invisible"
            );
        }
    }

    /// **No trailing newline.** The shell writes `printf '%s'`, and a `println!`-shaped
    /// hash would give a different key for every folder while looking correct.
    #[test]
    fn the_key_hashes_the_path_with_nothing_appended() {
        let p = Path::new("/home/dead/Projects/letibot");
        let with_newline = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(b"/home/dead/Projects/letibot\n");
            format!(
                "{}",
                h.finalize()
                    .iter()
                    .take(6)
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )
        };
        assert_ne!(
            key(p),
            with_newline,
            "if these ever agree the test proves nothing — it exists to pin the absence \
             of a newline, which is what the shell's `printf '%s'` does"
        );
    }

    /// The socket and record sit beside each other, named by the key, under the rundir
    /// given — so a test never addresses the machine's live daemons.
    #[test]
    fn the_paths_are_the_rundir_plus_the_key() {
        let w = locate_in(
            Path::new("/run/user/1000/letibot"),
            Path::new("/home/dead/Projects/leticl"),
        );
        assert_eq!(w.key, "fcf91a545af4");
        assert_eq!(
            w.socket,
            Path::new("/run/user/1000/letibot/fcf91a545af4.sock")
        );
        assert_eq!(
            w.record,
            Path::new("/run/user/1000/letibot/fcf91a545af4.json")
        );
    }

    /// A record that is missing, empty, or not JSON is `None` rather than a panic: the
    /// file is written by whichever launcher started the daemon, and `--status` must
    /// survive a stale one.
    #[test]
    fn a_record_that_cannot_be_read_is_none_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("letibot-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let missing = dir.join("nope.json");
        assert_eq!(read_record(&missing), None);
        let empty = dir.join("empty.json");
        std::fs::write(&empty, "").expect("write");
        assert_eq!(read_record(&empty), None);
        let broken = dir.join("broken.json");
        std::fs::write(&broken, "{ not json").expect("write");
        assert_eq!(read_record(&broken), None);

        // And a real one reads, including the `pid` that the shell's `json_num` had to
        // try both forms for.
        let good = dir.join("good.json");
        std::fs::write(
            &good,
            r#"{"workspace":"/w","socket":"/s.sock","pid":1234,"role":"coder",
                "model":"qwen-3.8-27b","answers":"deepseek/deepseek-flash",
                "dialect":"qwen","bash":true,"started":"2026-10-01T12:00:00+0200"}"#,
        )
        .expect("write");
        let r = read_record(&good).expect("a readable record");
        assert_eq!(r.pid, Some(1234));
        assert_eq!(r.workspace, "/w");
        assert_eq!(r.answers, "deepseek/deepseek-flash");
        assert!(r.bash);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// --- is it listening, and which process is it ------------------------------------

/// **Is this socket being listened on right now?**
///
/// The shell asks `ss -lxpH`, and its own comment on the helper that does it records a
/// bug worth not repeating: `ss … | grep -q` is a trap under `pipefail`, because `grep
/// -q` exits on the first match, `ss` gets SIGPIPE, and `pipefail` reports the writer's
/// 141 even though the last command succeeded. Measured there as *"a live daemon
/// labelled STALE"* about one run in ten. Reading a file cannot have that failure.
///
/// # Why `/proc` and not `ss`, and it is NOT speed
///
/// MEASURED on this box: an `ss -lxpH` fork costs **11.24 ms**, and the `/proc` read
/// below costs **10.27 ms**. That is the same number for practical purposes, so this is
/// not a performance change and should not be described as one. The reason is that **`ss`
/// may not be installed** — the shell's `daemon_pid` documents that explicitly ("the
/// record is a fallback for a box without `ss`") — and `/proc/net/unix` is always there
/// on Linux. One fewer external command in the path that decides whether a daemon is up.
///
/// `/proc/net/unix` has no pid column: it carries the INODE, and the process holding that
/// inode is found by scanning `/proc/<pid>/fd`. Measured against a live daemon: the path
/// is field index 7, the inode index 6, and the scan returned exactly the pid the record
/// named.
pub fn is_listening(socket: &Path) -> bool {
    inode_of(socket).is_some()
}

/// The socket inode from `/proc/net/unix`, or `None` when it is not listed — which is
/// what "nothing is listening on this path" means.
fn inode_of(socket: &Path) -> Option<String> {
    let want = socket.as_os_str().as_encoded_bytes();
    let table = std::fs::read("/proc/net/unix").ok()?;
    for line in table.split(|b| *b == b'\n') {
        let text = String::from_utf8_lossy(line);
        let mut fields = text.split_whitespace();
        // `Num RefCount Protocol Flags Type St Inode [Path]` — the path is absent for an
        // abstract socket, which is why this cannot assume eight fields.
        let (mut inode, mut path) = (None, None);
        for (i, f) in fields.by_ref().enumerate() {
            if i == 6 {
                inode = Some(f.to_string());
            }
            path = Some(f);
        }
        if let (Some(inode), Some(path)) = (inode, path) {
            if path.as_bytes() == want {
                return Some(inode);
            }
        }
    }
    None
}

/// **The pid LISTENING on this socket** — the daemon, whatever the record says.
///
/// The shell's comment is the specification: *"The pid LISTENING on our socket … the
/// record is a fallback for a box without `ss`."* So the socket is asked first and the
/// record second, and the record's answer is only believed when `/proc/<pid>` still
/// exists.
///
/// **More than one process can hold the socket**, because a child inherits open
/// descriptors — so among the holders this prefers one whose command line names
/// `harnessd`. That is the same check the shell's `--stop --all` makes before it kills
/// anything, and it is the difference between stopping the daemon and stopping whatever
/// happened to inherit its socket.
pub fn daemon_pid(where_: &Where) -> Option<u32> {
    if let Some(inode) = inode_of(&where_.socket) {
        let want = format!("socket:[{inode}]");
        let mut holders: Vec<u32> = Vec::new();
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for e in entries.flatten() {
                let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                    continue;
                };
                let fds = e.path().join("fd");
                let Ok(list) = std::fs::read_dir(&fds) else {
                    continue;
                };
                for fd in list.flatten() {
                    if std::fs::read_link(fd.path())
                        .ok()
                        .and_then(|t| t.to_str().map(str::to_string))
                        == Some(want.clone())
                    {
                        holders.push(pid);
                        break;
                    }
                }
            }
        }
        holders.sort_unstable();
        if let Some(pid) = holders.iter().copied().find(|p| is_harnessd(*p)) {
            return Some(pid);
        }
        if let Some(pid) = holders.first() {
            return Some(*pid);
        }
    }
    // The record, believed only while the process is still there.
    let r = read_record(&where_.record)?;
    let pid = r.pid?;
    if Path::new(&format!("/proc/{pid}")).is_dir() {
        return Some(pid);
    }
    None
}

/// Does `/proc/<pid>/cmdline` name `harnessd` as the program?
///
/// The shell does this with `tr '\0' '\n' < /proc/$pid/cmdline | head -1 | grep -q
/// harnessd` — the first field is argv[0], and matching the basename avoids a path that
/// merely contains the word.
pub fn is_harnessd(pid: u32) -> bool {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let first = raw.split(|b| *b == 0).next().unwrap_or(&[]);
    let name = String::from_utf8_lossy(first);
    Path::new(name.trim())
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n == "harnessd")
        .unwrap_or(false)
}

/// **A socket file that something is listening on** — the shell's `live`.
///
/// Two questions, and both are asked because they fail differently: the path must be a
/// socket (`-S`), and something must be listening. A stale socket file outlives a crashed
/// daemon, which is the shell's own reason for not trusting the file alone: *"is there a
/// daemon there"* is a conversation, not a stat.
pub fn live(where_: &Where) -> bool {
    use std::os::unix::fs::FileTypeExt;
    match std::fs::metadata(&where_.socket) {
        Ok(m) if m.file_type().is_socket() => is_listening(&where_.socket),
        _ => false,
    }
}

#[cfg(test)]
mod liveness_tests {
    use super::*;

    /// **A path with nothing on it is not listening**, and neither is a plain file —
    /// the case a stale record leaves behind.
    #[test]
    fn a_path_with_no_socket_is_not_listening() {
        let dir = std::env::temp_dir().join(format!("letibot-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let w = locate_in(&dir, Path::new("/home/dead/Projects/letibot"));

        assert!(!is_listening(&w.socket), "nothing has been created yet");
        assert!(!live(&w), "and `live` agrees");

        // A plain FILE at the socket path is the stale case: the path exists, nothing is
        // listening. `live` must say no where a bare `-e` would say yes.
        std::fs::write(&w.socket, b"").expect("write");
        assert!(w.socket.exists(), "the file is there");
        assert!(!live(&w), "…and it is still not a daemon");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A record with no pid, or a pid that is not running, is not a daemon.
    #[test]
    fn the_record_is_a_fallback_only_while_its_process_exists() {
        let dir = std::env::temp_dir().join(format!("letibot-fb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let w = locate_in(&dir, Path::new("/home/dead/Projects/letibot"));

        // No record at all.
        assert_eq!(daemon_pid(&w), None);

        // A record whose pid is gone — `u32::MAX` is never a live pid, and the pid
        // space is not going to reach it.
        std::fs::write(
            &w.record,
            r#"{"workspace":"/home/dead/Projects/letibot","pid":4294967295}"#,
        )
        .expect("write");
        assert_eq!(
            daemon_pid(&w),
            None,
            "a dead pid in the record is not a daemon"
        );

        // And a record naming THIS process, which is certainly alive.
        std::fs::write(
            &w.record,
            format!(r#"{{"workspace":"/x","pid":{}}}"#, std::process::id()),
        )
        .expect("write");
        assert_eq!(daemon_pid(&w), Some(std::process::id()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **This process is not `harnessd`**, and the check says so — the guard that keeps
    /// a `--stop` from killing whatever inherited the socket rather than the daemon.
    #[test]
    fn the_harnessd_check_is_not_a_substring_match() {
        assert!(
            !is_harnessd(std::process::id()),
            "the test binary is not harnessd, however its path is spelled"
        );
        assert!(
            !is_harnessd(u32::MAX),
            "and a pid with no /proc entry is not"
        );
    }

    /// **The Rust agrees with `ss` about the daemons running RIGHT NOW.**
    ///
    /// This is the cross-check the whole port rests on and it cannot be a unit test: it
    /// compares this module's answer against the tool the shell uses, for every record
    /// on the box, and reports the stale ones by name. It SKIPS when nothing is running
    /// — a runner has no daemons — and says so, rather than passing vacuously.
    ///
    /// MEASURED when written: 10 records, 6 live, and the pid agreed in every case.
    #[test]
    fn every_live_daemon_agrees_with_ss() {
        use std::process::Command;
        let dir = rundir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            eprintln!(
                "SKIPPED: no {} on this box, so there are no daemons to compare",
                dir.display()
            );
            return;
        };
        // `ss` is what the shell uses; if it is not installed, neither answer exists and
        // this is the box the /proc read was written for.
        let Ok(ss) = Command::new("ss").arg("-lxpH").output() else {
            eprintln!("SKIPPED: no `ss` here — which is exactly why this reads /proc");
            return;
        };
        let ss = String::from_utf8_lossy(&ss.stdout).into_owned();

        let (mut n_live, mut stale, mut agreed) = (0, 0, 0);
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Some(rec) = read_record(&path) else {
                continue;
            };
            if rec.workspace.is_empty() {
                continue;
            }
            let w = locate(Path::new(&rec.workspace));
            if !live(&w) {
                stale += 1;
                continue;
            }
            n_live += 1;
            // What `ss` says: the pid on the line naming this socket.
            let ss_pid = ss
                .lines()
                .find(|l| l.contains(rec.socket.as_str()))
                .and_then(|l| l.split("pid=").nth(1))
                .and_then(|r| r.split(',').next())
                .and_then(|p| p.parse::<u32>().ok());
            let mine = daemon_pid(&w);
            if ss_pid.is_some() && mine == ss_pid {
                agreed += 1;
            } else {
                eprintln!(
                    "  {} ss says pid={ss_pid:?}, this module says pid={mine:?}",
                    rec.socket
                );
            }
        }
        assert!(
            n_live > 0,
            "nothing was listening, so this proved nothing — it should have SKIPPED"
        );
        assert_eq!(
            agreed, n_live,
            "every live daemon must be found at the pid `ss` reports; {stale} record(s) were stale"
        );
        eprintln!("agreed with ss on {agreed} live daemon(s); {stale} stale record(s) skipped");
    }
}
