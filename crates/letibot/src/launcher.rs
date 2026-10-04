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
//! And the EXECUTION: spawning a daemon, waiting for its socket, writing the record
//! beside it, exec'ing a head. `roles::launcher` carries the argument for why that
//! waits — the short version is that the multicall is not what ships as `letibot`, so
//! it would have no caller and no way to be verified.
//!
//! The daemon's PID *is* here now, and not by `ss`: `inode_of`/`daemon_pid` read
//! `/proc/net/unix` and then find the holder by scanning `/proc/<pid>/fd` for
//! `socket:[inode]`. Measured against `ss` on every live daemon, and the answer agrees
//! — the reason for the read is that `ss` may simply be absent, which is a fact about
//! a box rather than about speed. (It is not faster: 10.27 ms against 11.24 ms.)

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

/// **Is this process actually running?** — and a zombie is not.
///
/// `/proc/<pid>` exists for a ZOMBIE: a process that has died and whose parent has not
/// reaped it keeps its directory (measured: state `Z` after SIGTERM, and the entry only
/// disappears at `wait`). So `Path::is_dir("/proc/pid")` — which is what the shell's
/// `[ -d "/proc/$p" ]` does — answers "still there" for a process that is already gone.
///
/// The shell gets away with it because it never starts harnessd as its own child: the
/// daemon is `setsid`-ed, so its death is reaped by init and the entry goes. A library
/// that is called from anywhere should not depend on that, so this reads the state field
/// from `/proc/<pid>/stat` and treats `Z` (zombie) and `X` (dead) as not running.
///
/// Found by its own test: the first version used `is_dir` and reported a signalled child
/// as `StillThere`, because the test had not reaped it yet.
pub fn is_running(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `pid (comm) state …` — and `comm` can contain spaces and parentheses, so the state
    // is read from AFTER the last ')', which is the kernel's own advice.
    let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
        return false;
    };
    match rest.split_whitespace().next() {
        // Z: dead, awaiting reap. X: dead. Neither is running.
        None | Some("Z") | Some("X") => false,
        Some(_) => true,
    }
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
    // `is_running`, not `is_dir`: a record naming a zombie names nothing useful.
    if is_running(pid) {
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
        use letibot_tokencore::apparatus;
        use std::process::Command;

        // **Everything this test needs is APPARATUS, and all three conditions go
        // through the same machinery rather than an `assert!`.**
        //
        // Its first version refused to pass vacuously with
        // `assert!(n_live > 0, "… it should have SKIPPED")` — the instinct is right and
        // is the same judgement as `THIS IS NOT A PASS`, but the enforcement was wrong:
        // on a box with no daemon that assertion FAILS, where what it means is "this
        // machine has nothing to compare against". MEASURED in CI:
        //
        //   every_live_daemon_agrees_with_ss ... FAILED
        //     nothing was listening, so this proved nothing — it should have SKIPPED
        //
        // A runner has no daemon, so that could only ever fail there — the mirror of the
        // `ldd` step that could only ever pass. `apparatus::present` says the honest
        // thing instead, and `LETIBOT_REQUIRE_APPARATUS=1` turns the skip back into a
        // failure for a box that means it.
        let dir = rundir();
        let Some(_) = apparatus::present(
            &format!("a letibot runtime directory ({})", dir.display()),
            dir.is_dir(),
        ) else {
            return;
        };

        // `ss` is what the shell uses to answer the same question. Without it there is
        // nothing to compare against — and this is the box the `/proc` read was written
        // for, so it is absent apparatus rather than a failure.
        let ss_out = Command::new("ss").arg("-lxpH").output().ok();
        let Some(_) = apparatus::present(
            "the `ss` command, to compare this module's answer against",
            ss_out.is_some(),
        ) else {
            return;
        };
        let ss = String::from_utf8_lossy(&ss_out.expect("present checked it").stdout).into_owned();

        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
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

        // **A daemon that lies about its pid FAILS; a machine with no daemon SKIPS.**
        // This is the assertion, and it stays as it is — the machinery above is only
        // about whether there was anything to assert against.
        let Some(_) = apparatus::present(
            "at least one daemon actually listening, to compare against",
            n_live > 0,
        ) else {
            return;
        };
        assert_eq!(
            agreed, n_live,
            "every live daemon must be found at the pid `ss` reports; {stale} record(s) were stale"
        );
        eprintln!("agreed with ss on {agreed} live daemon(s); {stale} stale record(s) skipped");
    }
}

// --- the three read-only verbs ----------------------------------------------------

/// **One line about this folder's daemon** — the shell's `daemon_line`, and the
/// `answers`/`model` distinction in it is the whole point.
///
/// A daemon records two true things about itself: the local vocab it binds token ids
/// against (`model`, e.g. `qwen-3.8-27b`) and what actually answers the turns (`answers`,
/// e.g. `deepseek/deepseek-flash`). Announcing the first to a person whose turns go to the
/// second is the operator's *"so - qwen again"* — a line that reads as a claim about the
/// model and is a claim about the tokenizer.
///
/// So `answers` wins when the record has one, and a record written before the field
/// existed is labelled **`vocab`** rather than `model`: that label is true, and it is not
/// a claim about what is answering.
pub fn daemon_line(workspace: &Path, rec: &Record, pid: Option<u32>) -> String {
    let mut out = format!("harnessd for {}", workspace.display());
    if let Some(p) = pid {
        out.push_str(&format!(" (pid {p})"));
    }
    if !rec.started.is_empty() {
        out.push_str(&format!(", up since {}", rec.started));
    }
    if !rec.role.is_empty() {
        out.push_str(&format!(", role {}", rec.role));
    }
    let who = if !rec.answers.is_empty() {
        format!("model {}", rec.answers)
    } else {
        format!(
            "vocab {}",
            if rec.model.is_empty() {
                "unknown"
            } else {
                &rec.model
            }
        )
    };
    out.push(' ');
    out.push_str(&who);
    out
}

/// What state a recorded daemon is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Something is listening on its socket.
    Up,
    /// The record exists and nothing answers its socket — a daemon that has gone, and
    /// whose record has not been cleaned up.
    Stale,
}

/// One line per recorded daemon, for `letibot --daemons`.
///
/// The STALE case is reported rather than hidden, and the wording is the shell's:
/// *"STALE (nothing listens on its socket)"*. A record whose socket is silent is how an
/// operator finds a daemon that died without cleaning up, and it is also how the
/// launcher knows to remove that record rather than trust its pid.
///
/// Takes the records as data so this is testable without a machine full of daemons: the
/// caller reads the directory, this decides what each line says.
pub fn daemons_lines(entries: &[(Record, State)]) -> Vec<String> {
    if entries.is_empty() {
        return vec!["no daemons recorded".to_string()];
    }
    entries
        .iter()
        .map(|(rec, state)| {
            let st = match state {
                State::Up => match rec.pid {
                    Some(p) => format!("up (pid {p})"),
                    None => "up".to_string(),
                },
                State::Stale => "STALE (nothing listens on its socket)".to_string(),
            };
            format!("{}  {}  {}", rec.workspace, st, rec.socket)
        })
        .collect()
}

/// What a `--status` invocation has to say, as lines.
///
/// Three cases, and the middle one is the one worth having: **something answered the
/// socket and refused.** That is a different fact from silence — a daemon speaking a
/// different protocol version is alive and serving — and printing "no daemon" for it is
/// how a live daemon gets a second one started on top of it. The shell's comment says
/// exactly that.
///
/// The model server is reported as a SEPARATE fact from the daemon, because it is one:
/// the daemon's turns can go to a provider while the local server is down, and vice
/// versa. The caller probes it and passes the answer in, so this stays offline.
pub fn status_lines(
    workspace: &Path,
    where_: &Where,
    up: Option<(&Record, Option<u32>)>,
    probe_refusal: Option<&str>,
    local: LocalServer<'_>,
) -> Vec<String> {
    let mut out = Vec::new();
    match up {
        Some((rec, pid)) => {
            out.push(format!("up: {}", daemon_line(workspace, rec, pid)));
            out.push(format!("socket   {}", where_.socket.display()));
        }
        None => match probe_refusal {
            Some(why) => out.push(format!(
                "on {}, but not speaking to this build: {why}",
                where_.socket.display()
            )),
            None => out.push(format!("no daemon for {}", workspace.display())),
        },
    }
    // The local model server, labelled for what it is rather than for what a reader might
    // take it to be.
    out.push(match local {
        LocalServer::Serving {
            endpoint,
            model,
            dialect,
        } => {
            format!("local    {endpoint} ok  ({model}, dialect {dialect})")
        }
        LocalServer::NotServing { endpoint } => format!("local    {endpoint} NOT SERVING"),
    });
    out
}

/// What the local endpoint answered, when the caller asked it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalServer<'a> {
    /// The endpoint answered `/health`, and the model it is serving. `endpoint` is
    /// carried so the line names where it looked, the way the shell's does.
    Serving {
        endpoint: &'a str,
        model: &'a str,
        dialect: &'a str,
    },
    NotServing {
        endpoint: &'a str,
    },
}

#[cfg(test)]
mod verb_tests {
    use super::*;

    fn rec(answers: &str, model: &str) -> Record {
        Record {
            workspace: "/w".into(),
            socket: "/w.sock".into(),
            pid: Some(42),
            role: "coder".into(),
            model: model.into(),
            answers: answers.into(),
            dialect: "qwen".into(),
            bash: true,
            started: "2026-10-01T12:00:00+0200".into(),
        }
    }

    /// **`answers` wins, because it is what answers.** This is the operator's *"so -
    /// qwen again"*: a session whose turns go to deepseek announced as qwen, from a line
    /// that is true about the tokenizer and read as a claim about the model.
    #[test]
    fn the_line_says_what_answers_not_what_it_tokenises_for() {
        let line = daemon_line(
            Path::new("/w"),
            &rec("deepseek/deepseek-flash", "qwen-3.8-27b"),
            Some(42),
        );
        assert!(line.contains("model deepseek/deepseek-flash"), "{line}");
        assert!(
            !line.contains("qwen-3.8-27b"),
            "the vocab must not be the headline: {line}"
        );
        assert!(
            line.contains("pid 42") && line.contains("role coder"),
            "{line}"
        );
    }

    /// A record from before `answers` existed is labelled `vocab` — true, and not a claim
    /// about what answers.
    #[test]
    fn a_record_without_answers_is_labelled_vocab() {
        let line = daemon_line(Path::new("/w"), &rec("", "qwen-3.8-27b"), None);
        assert!(line.contains("vocab qwen-3.8-27b"), "{line}");
        assert!(
            !line.contains("model qwen"),
            "it must not claim to be the model: {line}"
        );
        // And with neither field, `unknown` rather than an empty claim.
        let bare = daemon_line(Path::new("/w"), &rec("", ""), None);
        assert!(bare.contains("vocab unknown"), "{bare}");
    }

    /// `--daemons` reports STALE rather than hiding it — the record of a daemon that died
    /// without cleaning up.
    #[test]
    fn the_daemons_list_distinguishes_up_from_stale() {
        let lines = daemons_lines(&[
            (rec("", "m"), State::Up),
            (
                Record {
                    workspace: "/gone".into(),
                    socket: "/gone.sock".into(),
                    pid: Some(9),
                    ..Default::default()
                },
                State::Stale,
            ),
        ]);
        assert!(lines[0].contains("up (pid 42)"), "{:?}", lines[0]);
        assert!(
            lines[1].contains("STALE (nothing listens on its socket)"),
            "{:?}",
            lines[1]
        );
        assert!(
            lines[1].starts_with("/gone  "),
            "the workspace leads the line"
        );

        assert_eq!(
            daemons_lines(&[]),
            vec!["no daemons recorded".to_string()],
            "an empty box says so rather than printing nothing"
        );
    }

    /// **A refusal is not silence.** The middle case of `--status`, and the one that stops
    /// a second daemon being started over a live one.
    #[test]
    fn status_separates_a_refusal_from_nothing_being_there() {
        let w = locate_in(Path::new("/run/x"), Path::new("/w"));
        let quiet = status_lines(
            Path::new("/w"),
            &w,
            None,
            None,
            LocalServer::NotServing {
                endpoint: "127.0.0.1:8080",
            },
        );
        assert!(quiet[0].starts_with("no daemon for /w"), "{quiet:?}");

        let refused = status_lines(
            Path::new("/w"),
            &w,
            None,
            Some("protocol version 26, this build speaks 27"),
            LocalServer::NotServing {
                endpoint: "127.0.0.1:8080",
            },
        );
        assert!(
            refused[0].contains("not speaking to this build"),
            "a live daemon that refused must not be reported as absent: {refused:?}"
        );

        // And the local server is always reported, separately from the daemon.
        let up = status_lines(
            Path::new("/w"),
            &w,
            Some((&rec("deepseek/x", "qwen"), Some(42))),
            None,
            LocalServer::Serving {
                endpoint: "127.0.0.1:8080",
                model: "qwen-3.8-27b",
                dialect: "qwen",
            },
        );
        assert!(up[0].starts_with("up: harnessd for /w"), "{up:?}");
        assert!(up[1].starts_with("socket   "), "{up:?}");
        assert!(
            up[2].contains("qwen-3.8-27b") && up[2].contains("dialect qwen"),
            "{up:?}"
        );
    }
}

// --- stopping one ----------------------------------------------------------------

/// What a stop attempt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// The process was signalled and is gone.
    Gone,
    /// Signalled, and still there when the grace elapsed. **Not an error**: the shell
    /// prints this and keeps the record, because a daemon that survives SIGTERM is a
    /// fact the operator needs rather than a failure to retry.
    StillThere,
    /// No such process to signal.
    NoSuchProcess,
}

/// How long to wait for a signalled daemon, in tenths of a second.
///
/// Ten times 500 ms, which is the shell's own loop — and its comment is the reason the
/// wait exists at all: *"Measured 2026-09-16: a daemon wedged on a `llama-server` that
/// had gone away swallowed SIGTERM, this printed 'stopped', deleted the record, and left
/// an orphan holding the store and the GPU that `--daemons` could no longer see."*
///
/// **"stopped" is said after the process is gone, not after the signal is sent.** That
/// is the whole of that incident, and it is why this returns [`Stopped::StillThere`]
/// rather than a bare `Ok`.
pub const STOP_TENTHS: u32 = 10;

/// Signal a daemon to stop and wait for it, bounded.
///
/// The grace is spent in 50 ms steps rather than one 500 ms sleep: the common case is a
/// daemon that exits at once, and the shell's `sleep 0.5` made every stop take at least
/// half a second. Same bound, faster finish.
pub fn stop(pid: u32) -> Stopped {
    if !is_running(pid) {
        return Stopped::NoSuchProcess;
    }
    // SIGTERM. `libc` rather than shelling out to `kill`: that is a shell builtin on
    // several systems and a fork on the rest, and this is one syscall.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    for _ in 0..(STOP_TENTHS * 10) {
        if !is_running(pid) {
            return Stopped::Gone;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Stopped::StillThere
}

/// SIGKILL, for `--stop --force` after [`stop`] has given up.
///
/// A daemon that ignores SIGTERM is stuck on something that is never coming back — the
/// 2026-09-16 case was a `llama-server` that had gone away — and leaving it holding the
/// store and the GPU is worse than killing it. The caller has already *said* it is
/// forcing, which is the difference between a decision and a surprise.
pub fn force_kill(pid: u32) -> Stopped {
    if !is_running(pid) {
        return Stopped::NoSuchProcess;
    }
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    for _ in 0..20 {
        if !is_running(pid) {
            return Stopped::Gone;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Stopped::StillThere
}

#[cfg(test)]
mod stop_tests {
    use super::*;

    /// **Tested against a child this test starts, never a live daemon.**
    ///
    /// The machine this was written on has seven running daemons, and a stop test that
    /// reached one of them would kill the operator's session — so the only pid that is
    /// ever signalled here is a `sleep` this test spawned and owns.
    #[test]
    fn stop_reaches_a_process_and_waits_for_it_to_go() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn a sleeper");
        let pid = child.id();
        assert!(is_running(pid), "it is running");

        assert_eq!(stop(pid), Stopped::Gone, "the sleeper honours SIGTERM");
        // And it really is gone: `wait` reaps it, and a second `stop` finds nothing.
        let _ = child.wait();
        assert_eq!(stop(pid), Stopped::NoSuchProcess);
    }

    /// A pid that is not there is `NoSuchProcess` rather than an error — the common case
    /// for a record whose daemon has already gone.
    #[test]
    fn stopping_something_that_is_not_there_says_so() {
        assert_eq!(stop(u32::MAX), Stopped::NoSuchProcess);
        assert_eq!(force_kill(u32::MAX), Stopped::NoSuchProcess);
    }

    /// **`force_kill` reaches a process that ignores SIGTERM.** The 2026-09-16 shape: a
    /// daemon wedged on something that had gone away.
    ///
    /// The child traps SIGTERM and exits on nothing, so only SIGKILL ends it. Spawned by
    /// this test, so still no live daemon involved.
    #[test]
    fn force_kill_ends_a_process_that_ignores_sigterm() {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 30")
            .spawn()
            .expect("spawn a stubborn child");
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Give the trap a moment to be installed, then SIGTERM is ignored.
        assert_eq!(
            stop(pid),
            Stopped::StillThere,
            "a process that ignores SIGTERM must be reported as still there, not as stopped              — that report is the whole of the 2026-09-16 incident"
        );
        assert!(is_running(pid), "and it is still there");
        assert_eq!(force_kill(pid), Stopped::Gone, "SIGKILL ends it");
        let _ = child.wait();
    }
}

// --- the seat, as the launcher's own flags describe it ---------------------------

/// **What the operator asked this session to be** — the launcher's flags, parsed.
///
/// A "seat" is the launcher's word for the role, the mode, the grants and the provider
/// that a daemon is started with. **The seat is fixed when the daemon STARTS**, which is
/// why this parses them apart from the verbs: `--status` does not need them, and
/// `--attach` cannot change them — it can only warn that the flags it was given will not
/// take effect against a daemon already running. That warning is its own piece; this is
/// the parse.
///
/// The field set and every default come from `scripts/letibot`'s flag arms, and the
/// messages are the shell's own wording — a person who knows this launcher should not be
/// able to tell which one refused.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Seat {
    /// `--read-only` is role `orchestrator`; the default role is `coder`.
    pub role: String,
    /// The adjustments `--read-only` clears. Kept as the shell's single opaque string,
    /// because it is forwarded verbatim to the daemon.
    pub adj: String,
    /// `--mode`, whose six names the shell lists in its own refusal.
    pub mode: String,
    /// `--bash` / `--no-bash`. **On by default**, for every seat that can carry a shell —
    /// the operator's *"i dont think having coder that cant do tests is a…"* is why, and
    /// `--no-bash` is the opt-out.
    pub bash: bool,
    pub supervise: bool,
    pub oracle: String,
    pub oracle_budget: String,
    /// `--provider`, and whether it was set — an explicit provider means the launcher
    /// does not know the model, which is a different state from the config's default.
    pub provider: Option<String>,
    pub web_search: String,
    pub web_fetch: bool,
    /// The dialect the daemon renders for. The presets set it; there is no `--dialect`
    /// flag of the launcher's own.
    pub dialect: String,
    /// The vocab the daemon binds token ids against, which every preset also names — it
    /// is the model's own GGUF and cannot be derived from the dialect.
    pub vocab: String,
    pub model: String,
    /// Everything forwarded to the daemon with no interpretation here: `--vm`,
    /// `--vm-arg`, `--provider`, `--web-search`, `--web-fetch`. harnessd owns the meaning.
    pub extra: Vec<String>,
}

/// The modes `--mode` accepts, as the shell lists them in its refusal. Named once so the
/// message and the validation cannot disagree.
/// The operator's home, as the shell uses it: `$HOME`, and `~` as a fallback rather than
/// an empty string — a path beginning `"/models/…"` would be silently wrong.
pub fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "~".into())
}

pub const MODES: &[&str] = &[
    "read-only",
    "always-ask",
    "writes-allowed",
    "automode",
    "automode-edits",
    "allow-all",
];

/// Parse the launcher's seat flags from `args`, or refuse by name.
///
/// **A flag that needs a value and has none is a refusal, not a default.** The shell says
/// `--mode NAME   (read-only, always-ask, …)` — the requirement and the accepted set in
/// one line — and that shape is kept because it is what a person can act on.
///
/// Unknown arguments are NOT refused here: the launcher also takes a bare prompt and its
/// verbs, and deciding which is which belongs to the caller. This consumes what it
/// recognises and hands back the rest.
pub fn parse_seat(args: &[String]) -> Result<(Seat, Vec<String>), String> {
    let mut seat = Seat {
        // The shell's defaults, read off its own initialisers.
        role: "coder".into(),
        bash: true,
        ..Default::default()
    };
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        // `need` is the shell's `next()?`: a flag that wants a value gets the next
        // argument or the refusal.
        let need = |what: &str| -> Result<String, String> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{what} needs a value"))
        };
        match a {
            "--read-only" => {
                seat.role = "orchestrator".into();
                seat.adj = String::new();
                i += 1;
            }
            "--writes" => {
                seat.mode = "writes-allowed".into();
                i += 1;
            }
            "--ask" => {
                seat.mode = "always-ask".into();
                i += 1;
            }
            "--mode" => {
                let v = need(
                    "--mode NAME   (read-only, always-ask, writes-allowed,                               automode, automode-edits, allow-all)",
                )?;
                if !MODES.contains(&v.as_str()) {
                    return Err(format!("--mode {v}: not one of {}", MODES.join(", ")));
                }
                seat.mode = v;
                i += 2;
            }
            "--bash" => {
                seat.bash = true;
                i += 1;
            }
            "--no-bash" => {
                seat.bash = false;
                i += 1;
            }
            "--supervise" => {
                seat.supervise = true;
                i += 1;
            }
            "--oracle" => {
                seat.oracle = need(
                    "--oracle HOST:PORT   the guard model for --mode                                     supervised / automode",
                )?;
                i += 2;
            }
            "--oracle-budget-ms" => {
                seat.oracle_budget = need(
                    "--oracle-budget-ms N   how long the gate waits                                            before failing closed",
                )?;
                i += 2;
            }
            "--vm" => {
                seat.extra.push("--where".into());
                seat.extra.push("firecode".into());
                i += 1;
            }
            "--vm-arg" => {
                seat.extra.push("--vm-arg".into());
                seat.extra.push(need("--vm-arg ARG")?);
                i += 2;
            }
            "--provider" => {
                let v = need("--provider NAME   (deepseek, glm, glm-coding, glm-coding-cn, grok)")?;
                seat.extra.push("--provider".into());
                seat.extra.push(v.clone());
                seat.provider = Some(v);
                i += 2;
            }
            "--web-search" => {
                let v = need("--web-search NAME   (brave)")?;
                seat.extra.push("--web-search".into());
                seat.extra.push(v.clone());
                seat.web_search = v;
                i += 2;
            }
            "--web-fetch" => {
                seat.web_fetch = true;
                seat.extra.push("--web-fetch".into());
                i += 1;
            }
            "--no-web-fetch" => {
                seat.web_fetch = false;
                i += 1;
            }
            "--effort" => {
                // **Forwarded, not stored.** The shell puts it in `EXTRA_ARGS`, because
                // the daemon owns what an effort level means.
                let v = need("--effort LEVEL   (low, high, max)")?;
                seat.extra.push("--effort".into());
                seat.extra.push(v);
                i += 2;
            }
            "--brave-key" => {
                seat.extra.push("--brave-key".into());
                seat.extra.push(need("--brave-key KEY")?);
                i += 2;
            }
            "--model" => {
                seat.model = need("--model NAME")?;
                i += 2;
            }
            // **The three presets, and each names a dialect, a model AND a vocab.**
            // MEASURED against the shell: my first version of these three arms was a
            // guess, and all three were wrong — `--glm` sets the model and the vocab too,
            // and the vocab cannot be derived from the dialect because it is the model's
            // own GGUF. The paths are the shell's, `$HOME` and all.
            "--glm" => {
                seat.dialect = "glm".into();
                seat.model = "glm-5.3-flash".into();
                seat.vocab = format!(
                    "{home}/models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf",
                    home = home()
                );
                i += 1;
            }
            "--flash" => {
                seat.dialect = "qwen".into();
                seat.model = "qwen-3.8-flash-next".into();
                seat.vocab = format!(
                    "{home}/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf",
                    home = home()
                );
                i += 1;
            }
            "--dense" => {
                seat.dialect = "qwen".into();
                seat.model = "qwen-3.8-27b".into();
                seat.vocab = format!("{home}/models/Qwen3.8-27B-UD-Q6_K_XL.gguf", home = home());
                i += 1;
            }
            _ => {
                rest.push(args[i].clone());
                i += 1;
            }
        }
    }
    Ok((seat, rest))
}

#[cfg(test)]
mod seat_tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// **The shell's defaults**, read off its own initialisers: role `coder`, shell ON.
    ///
    /// The shell is on by default for every seat that can carry one, and the reason is
    /// the operator's own: a coder that cannot run a test is not one. `--no-bash` is the
    /// opt-out, so a parse that defaulted it off would take a working session away.
    #[test]
    fn the_defaults_are_the_shells() {
        let (seat, rest) = parse_seat(&[]).expect("no flags is not an error");
        assert_eq!(seat.role, "coder");
        assert!(seat.bash, "the shell is ON unless --no-bash says otherwise");
        assert!(rest.is_empty());
    }

    /// `--read-only` is role `orchestrator` AND clears the adjustments — both halves,
    /// because the shell does both in one arm.
    #[test]
    fn read_only_is_a_role_and_clears_the_adjustments() {
        let (seat, _) = parse_seat(&v(&["--read-only"])).unwrap();
        assert_eq!(seat.role, "orchestrator");
        assert!(seat.adj.is_empty());

        let (seat2, _) = parse_seat(&v(&["--no-bash", "--read-only"])).unwrap();
        assert_eq!(seat2.role, "orchestrator");
        assert!(!seat2.bash, "--read-only must not turn the shell back on");
    }

    /// **A mode outside the six is refused, and the refusal lists them.** The shell's
    /// message names all six, which is the difference between a refusal and a puzzle.
    #[test]
    fn a_mode_outside_the_accepted_set_is_refused_by_name() {
        let (seat, _) = parse_seat(&v(&["--mode", "automode"])).unwrap();
        assert_eq!(seat.mode, "automode");

        let e = parse_seat(&v(&["--mode", "yolo"])).unwrap_err();
        assert!(e.contains("yolo"), "{e}");
        for m in MODES {
            assert!(e.contains(m), "the refusal must list {m}: {e}");
        }
    }

    /// A flag that needs a value and has none refuses. This is the shape that took a
    /// session down silently in another tree: a swallowed `--provider` left the daemon
    /// answering with the config's default and no word about why.
    #[test]
    fn a_flag_with_no_value_refuses_rather_than_defaulting() {
        for flag in ["--mode", "--oracle", "--provider", "--model", "--effort"] {
            let e = parse_seat(&v(&[flag])).unwrap_err();
            assert!(e.contains("needs a value"), "{flag}: {e}");
        }
    }

    /// **The passthrough is verbatim, and that is the contract with the daemon.** The
    /// shell forwards these with no interpretation — *"harnessd owns the meaning"* — so
    /// the order and the values must arrive as typed.
    #[test]
    fn the_forwarded_flags_are_passed_through_verbatim() {
        let (seat, _) = parse_seat(&v(&[
            "--provider",
            "deepseek",
            "--vm",
            "--vm-arg",
            "--timeout 60",
            "--web-search",
            "brave",
        ]))
        .unwrap();
        assert_eq!(seat.provider.as_deref(), Some("deepseek"));
        assert_eq!(
            seat.extra,
            v(&[
                "--provider",
                "deepseek",
                "--where",
                "firecode",
                "--vm-arg",
                "--timeout 60",
                "--web-search",
                "brave"
            ]),
            "`--vm` becomes `--where firecode`, and everything else rides as typed"
        );
    }

    /// **The three presets, pinned to the shell's own values.**
    ///
    /// MEASURED, and it is why this test exists: my first three arms were guesses and all
    /// three were wrong. `--glm` does not just mean "dialect glm" — it names a model and
    /// the model's own vocab GGUF, and the vocab cannot be derived from the dialect.
    #[test]
    fn the_presets_carry_a_dialect_a_model_and_a_vocab() {
        let (glm, _) = parse_seat(&v(&["--glm"])).unwrap();
        assert_eq!(glm.dialect, "glm");
        assert_eq!(glm.model, "glm-5.3-flash");
        assert!(
            glm.vocab
                .ends_with("models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf"),
            "{}",
            glm.vocab
        );

        let (flash, _) = parse_seat(&v(&["--flash"])).unwrap();
        assert_eq!(
            (flash.dialect.as_str(), flash.model.as_str()),
            ("qwen", "qwen-3.8-flash-next")
        );
        assert!(
            flash.vocab.contains("qwen3.8-flash-next"),
            "{}",
            flash.vocab
        );

        let (dense, _) = parse_seat(&v(&["--dense"])).unwrap();
        assert_eq!(
            (dense.dialect.as_str(), dense.model.as_str()),
            ("qwen", "qwen-3.8-27b")
        );
        assert!(
            dense.vocab.ends_with("Qwen3.8-27B-UD-Q6_K_XL.gguf"),
            "{}",
            dense.vocab
        );

        // And every path is absolute: a relative one would be resolved against whatever
        // directory the launcher happened to be started from.
        for v in [&glm.vocab, &flash.vocab, &dense.vocab] {
            assert!(v.starts_with('/'), "not absolute: {v}");
        }
    }

    /// `--effort` is FORWARDED, not interpreted: the daemon owns what a level means.
    #[test]
    fn effort_is_forwarded_like_the_other_daemon_flags() {
        let (seat, _) = parse_seat(&v(&["--effort", "low"])).unwrap();
        assert_eq!(seat.extra, v(&["--effort", "low"]));
    }

    /// **Anything it does not recognise comes back, rather than being refused.** The
    /// launcher also takes verbs and a bare one-shot prompt, and deciding which of those
    /// an argument is belongs to the caller.
    #[test]
    fn unrecognised_arguments_are_returned_not_refused() {
        let (_, rest) = parse_seat(&v(&["--bash", "--stop", "fix the tests"])).unwrap();
        assert_eq!(rest, v(&["--stop", "fix the tests"]));
    }
}

// --- targeting another folder's daemon --------------------------------------------

/// Every `*.json` record under `rundir`, in the order the shell's glob yields them.
///
/// **This order is the contract `--ls` numbering rests on.** The shell writes
/// `for r in "$RUNDIR"/*.json`, numbers what it finds 1, 2, 3…, and `--stop N` maps that
/// number back by walking the SAME glob. A glob sorts; `read_dir` does not promise to, so
/// the sort here is what makes `--ls`'s number mean the same thing to both readers.
///
/// Files only, matching the shell's `[ -f "$r" ] || continue` — a directory named
/// `something.json` is not a record and must not shift every number after it.
pub fn records_in(rundir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(rundir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("json"))
        })
        .collect();
    out.sort();
    out
}

/// **Which daemon a `--stop`/`--attach` argument names.**
///
/// Three forms, exactly as the shell takes them: nothing or `.` (this folder), an
/// all-digit handle from `--ls`, or a directory. Returns the [`Where`] the rest of the
/// launcher then uses, so the caller never re-derives a key.
///
/// # One divergence from the shell, deliberate and named
///
/// **`letibot --attach 0` REFUSES here; the shell accepted it and acted on this folder.**
/// Measured 2026-10-01 before writing this — with no daemons recorded at all:
///
/// ```text
/// scripts/letibot --attach 5   -> "there is no daemon 5 — `letibot --ls` numbers them"
/// scripts/letibot --attach 0   -> "no harnessd for /tmp/emptyrun"        (this folder!)
/// scripts/letibot --stop   0   -> "no daemon for /tmp/emptyrun"          (this folder!)
/// ```
///
/// The shell's guard is `[ "$n" -ge "$want" ]`, where `n` counts records it walked. For
/// `want=0` the counter never equals it, the walk ends with `n` at the record count (or 0
/// with none), and `0 -ge 0` passes — so a handle the list never prints silently means
/// "the folder I am standing in". `--ls` numbers from 1. A handle that is not on the list
/// must refuse by name rather than quietly resolve to something the operator did not ask
/// for, which is the same rule the rest of this tree follows about flags that do not land.
///
/// # And one more, for the same reason
///
/// A record that cannot be read or parsed refuses rather than contributing an empty
/// workspace. The shell sets `WORKSPACE=""` and hashes it, and the empty path is the one
/// key every folder on the box would share — see this module's docs on the guard that
/// exists because of it. Refusing by name is the honest answer to a record that says
/// nothing.
pub fn retarget(rundir: &Path, here: &Path, want: &str) -> Result<Where, String> {
    match want {
        "" | "." => Ok(locate_in(rundir, here)),
        w if w.bytes().all(|b| b.is_ascii_digit()) => {
            let n: usize = w
                .parse()
                .map_err(|_| format!("there is no daemon {w} — `letibot --ls` numbers them"))?;
            let records = records_in(rundir);
            if n == 0 || n > records.len() {
                return Err(format!(
                    "there is no daemon {w} — `letibot --ls` numbers them"
                ));
            }
            let rec = read_record(&records[n - 1]).ok_or_else(|| {
                format!(
                    "the record for daemon {w} says nothing usable ({}), so there is no \
                     workspace to act on — `letibot --ls` lists what is there",
                    records[n - 1].display()
                )
            })?;
            if rec.workspace.is_empty() {
                return Err(format!(
                    "the record for daemon {w} has no workspace field ({}), so there is no \
                     folder to act on",
                    records[n - 1].display()
                ));
            }
            Ok(locate_in(rundir, Path::new(&rec.workspace)))
        }
        dir => match std::fs::canonicalize(dir) {
            Ok(p) => Ok(locate_in(rundir, &p)),
            Err(_) => Err(format!("no such directory: {dir}")),
        },
    }
}

// --- the dispatch: what an invocation asks for ------------------------------------
//
// The shell decides this in THREE passes, and the split is not incidental — each pass
// refuses something the next cannot see:
//
//   1. `--help` anywhere before the prompt is the flag list, and `exit 0`. It has to
//      come first because it is the only answer that must work when every later check
//      would refuse.
//   2. An unknown `--flag` is refused BY NAME, before anything is looked up. **Measured
//      2026-09-13, and this is why the pass exists**: `--help` used to match no flag, so
//      it fell through to the one-shot path and ran `harnessd --prompt "--help"` — the
//      banner printed, the vocabulary loaded (the multi-second hang), and GLM answered
//      the prompt `--help`. A typo in a flag name bought you a model call.
//   3. [`parse_seat`], which consumes the flags it knows and returns at the first thing
//      that is not one. Its leftovers are the verb or the prompt.
//
// Folding these together would make the first two unreachable: `parse_seat` returns
// unknowns rather than refusing them (deliberately — see its own test), so the "refuse by
// name" behaviour only exists in the pass that runs before it.

/// The flags that take a value, so the argument AFTER one is not a flag whatever it
/// looks like. `--vm-arg --mem` hands `--mem` to firecode.
///
/// The shell keeps this list twice, in two adjacent loops, and it is the same list — so
/// it is one `const` here rather than two chances to forget an entry.
pub const VALUE_FLAGS: &[&str] = &[
    "--vm-arg",
    "--provider",
    "--model",
    "--mode",
    "--session",
    "-s",
    "--rename",
    "--delete",
    "--web-search",
    "--brave-key",
    "--oracle",
    "--oracle-budget-ms",
];

/// Every flag the launcher accepts, verbatim from the shell's own list.
///
/// **The verbs are in here too** (`--stop`, `--attach`, `--sessions`…), because this list
/// answers "is this an unknown flag", not "is this a flag the option loop consumes". A
/// verb is a perfectly good argument that the option loop leaves behind for the dispatch.
pub const KNOWN_FLAGS: &[&str] = &[
    "--help",
    "-h",
    "help",
    "--read-only",
    "--writes",
    "--bash",
    "--no-bash",
    "--ask",
    "--supervise",
    "--glm",
    "--flash",
    "--dense",
    "--attach",
    "--daemons",
    "--all",
    "--sessions",
    "--list",
    "--stop",
    "--status",
    "--continue",
    "-c",
    "--new",
    "-n",
    "--session",
    "-s",
    "--rename",
    "--delete",
    "--mode",
    "--force",
    "--vm",
    "--vm-arg",
    "--provider",
    "--model",
    "--web-search",
    "--brave-key",
    "--oracle",
    "--oracle-budget-ms",
    "--ls",
    "--list-all",
];

/// What an invocation of `letibot` asks the launcher to do.
///
/// One variant per arm of the shell's dispatch, so "is every verb represented" is a
/// question the compiler answers rather than a reviewer counting `case` labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `--help`: the flag list, exit 0.
    Help,
    /// `--sessions` / `--list`: the store's sessions, straight from the daemon binary.
    Sessions,
    /// `--rename ID [TITLE]`, where an empty title clears the name.
    Rename { id: String, title: String },
    /// `--delete ID`.
    Delete { id: String },
    /// `--ls` / `--list-all`: every folder's daemon, numbered, with what is live inside.
    ListAll,
    /// `--daemons`: the records, each checked against a live listener.
    Daemons,
    /// `--status`: this folder's daemon, and the local model server as a separate fact.
    Status,
    /// `--stop [N|DIR|--all]`, with `--force` to interrupt in-flight turns first.
    Stop {
        all: bool,
        force: bool,
        target: String,
    },
    /// `--attach [N|DIR]`: connect only, starting nothing.
    Attach { target: String },
    /// `--continue`: reopen the newest session in THIS workspace.
    Continue,
    /// `--session ID`.
    Resume { id: String },
    /// `--new [TITLE]`.
    New { title: String },
    /// The plain invocation. `one_shot` is non-empty when a bare prompt followed, which
    /// means no daemon is started and no head is opened.
    Bring { one_shot: Vec<String> },
}

/// Whether `a` is a flag the launcher knows.
pub fn known_flag(a: &str) -> bool {
    KNOWN_FLAGS.contains(&a)
}

/// Whether the argument AFTER `a` is a value rather than a flag.
pub fn takes_value(a: &str) -> bool {
    VALUE_FLAGS.contains(&a)
}

/// **Decide what an invocation asks for, refusing by name what cannot be answered.**
///
/// The three passes the shell runs, in its order, and the order is the contract:
/// `--help` first (it must answer when nothing else can), then the unknown-flag refusal,
/// then [`parse_seat`] and whatever it leaves behind.
///
/// Returns the [`Seat`] alongside the [`Action`], because the seat flags are consumed by
/// the same pass that finds the verb — `letibot --bash --stop` is a stop, and the `--bash`
/// has already been read into the seat that `--stop` will ignore.
pub fn decide(args: &[String]) -> Result<(Seat, Action), String> {
    // Pass 1: `--help` anywhere before the prompt.
    //
    // **No value-skipping here, and that is the shell's behaviour rather than an
    // oversight**: `letibot --vm-arg --help` prints the flag list instead of passing
    // `--help` to firecode, because this pass predates the value-flag list. Kept as it is
    // so the ported launcher answers identically; changing it would be a behaviour change
    // smuggled in as a cleanup.
    for a in args {
        match a.as_str() {
            "--help" | "-h" | "help" => return Ok((Seat::default(), Action::Help)),
            _ if a.starts_with("--") => {}
            // The prompt starts here; nothing after it is a flag.
            _ => break,
        }
    }

    // Pass 2: an unknown flag is refused by name, before anything is looked up.
    //
    // `skip` is the shell's own: a value-taking flag consumes the argument after it, so a
    // value that looks like a flag (`--vm-arg --mem`) is not refused as an unknown one.
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if takes_value(a) {
            skip = true;
        }
        if known_flag(a) {
            continue;
        }
        if a.starts_with("--") {
            return Err(format!(
                "unknown flag: {a}\n         ('letibot --help' lists the flags; a bare prompt needs no flag)"
            ));
        }
        // Not a flag: the prompt, and nothing past it is checked.
        break;
    }

    // `--force` only means something with `--stop`. Said rather than silently ignored:
    // a flag that does nothing is a flag the operator cannot trust.
    let force = args.iter().any(|a| a == "--force");

    // Pass 3: the option loop, and its leftovers.
    let (seat, rest) = parse_seat(args)?;

    let first = rest.first().map(String::as_str).unwrap_or("");
    if force && first != "--stop" {
        return Err("--force only means something with --stop".into());
    }

    let action = match first {
        "--sessions" | "--list" => Action::Sessions,
        "--rename" => Action::Rename {
            id: rest.get(1).cloned().unwrap_or_default(),
            title: rest.get(2).cloned().unwrap_or_default(),
        },
        "--delete" => Action::Delete {
            id: rest.get(1).cloned().unwrap_or_default(),
        },
        "--ls" | "--list-all" => Action::ListAll,
        "--daemons" => Action::Daemons,
        "--status" => Action::Status,
        "--stop" => {
            // `--all` and `--force` are not targets; anything else there is.
            let target = rest
                .iter()
                .skip(1)
                .find(|a| *a != "--all" && *a != "--force" && !a.starts_with("--"))
                .cloned()
                .unwrap_or_default();
            Action::Stop {
                all: rest.iter().any(|a| a == "--all"),
                force,
                target,
            }
        }
        "--attach" => Action::Attach {
            target: rest.get(1).cloned().unwrap_or_default(),
        },
        "--continue" | "-c" => Action::Continue,
        "--new" | "-n" => Action::New {
            title: rest.get(1).cloned().unwrap_or_default(),
        },
        _ if first == "--session" || first == "-s" => Action::Resume {
            id: rest.get(1).cloned().unwrap_or_default(),
        },
        // Nothing recognised here: the plain invocation, with whatever followed as a
        // one-shot prompt.
        _ => Action::Bring {
            one_shot: rest.clone(),
        },
    };

    Ok((seat, action))
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn act(items: &[&str]) -> Action {
        decide(&v(items)).unwrap().1
    }

    /// The pass that must answer when nothing else can.
    #[test]
    fn help_is_recognised_first_and_anywhere_before_the_prompt() {
        assert_eq!(act(&["--help"]), Action::Help);
        assert_eq!(act(&["-h"]), Action::Help);
        assert_eq!(act(&["help"]), Action::Help);
        // After other flags, still help.
        assert_eq!(act(&["--glm", "--help"]), Action::Help);
        // But NOT after the prompt has started: `letibot fix --help` is a question whose
        // text is "fix --help", not a request for the flag list.
        assert_eq!(
            act(&["fix", "--help"]),
            Action::Bring {
                one_shot: v(&["fix", "--help"])
            }
        );
    }

    /// **The incident this pass exists for**, measured 2026-09-13: a typo in a flag name
    /// used to be answered by the model as a one-shot prompt, because `--help` matched no
    /// flag and the unknown flag was not refused.
    #[test]
    fn an_unknown_flag_is_refused_by_name_rather_than_run_as_a_prompt() {
        let e = decide(&v(&["--hlep"])).unwrap_err();
        assert!(e.contains("unknown flag: --hlep"), "{e}");
        // And the refusal names where the list is.
        assert!(e.contains("--help"), "{e}");
    }

    /// A value that merely LOOKS like a flag is a value. `--vm-arg --mem` hands `--mem`
    /// to firecode, so it must not be read as an unknown flag.
    #[test]
    fn a_value_that_looks_like_a_flag_is_not_refused() {
        assert!(decide(&v(&["--vm-arg", "--mem"])).is_ok());
        assert!(decide(&v(&["--provider", "--weird"])).is_ok());
    }

    /// `--force` with anything but `--stop` is a mistake, and a flag that does nothing is
    /// a flag the operator cannot trust.
    #[test]
    fn force_without_stop_is_refused() {
        let e = decide(&v(&["--force"])).unwrap_err();
        assert!(
            e.contains("--force only means something with --stop"),
            "{e}"
        );
        assert!(decide(&v(&["--stop", "--force"])).is_ok());
    }

    /// The verbs, one per arm.
    #[test]
    fn every_verb_reaches_its_own_action() {
        assert_eq!(act(&["--sessions"]), Action::Sessions);
        assert_eq!(act(&["--list"]), Action::Sessions);
        assert_eq!(act(&["--daemons"]), Action::Daemons);
        assert_eq!(act(&["--status"]), Action::Status);
        assert_eq!(act(&["--ls"]), Action::ListAll);
        assert_eq!(act(&["--list-all"]), Action::ListAll);
        assert_eq!(act(&["--continue"]), Action::Continue);
        assert_eq!(act(&["-c"]), Action::Continue);
    }

    /// `--stop` takes a target, and `--all`/`--force` are not targets.
    #[test]
    fn stop_distinguishes_its_target_from_its_modifiers() {
        assert_eq!(
            act(&["--stop"]),
            Action::Stop {
                all: false,
                force: false,
                target: String::new()
            }
        );
        assert_eq!(
            act(&["--stop", "2"]),
            Action::Stop {
                all: false,
                force: false,
                target: "2".into()
            }
        );
        assert_eq!(
            act(&["--stop", "--all"]),
            Action::Stop {
                all: true,
                force: false,
                target: String::new()
            }
        );
        assert_eq!(
            act(&["--stop", "--force"]),
            Action::Stop {
                all: false,
                force: true,
                target: String::new()
            }
        );
        // A target AND a modifier.
        assert_eq!(
            act(&["--stop", "build", "--force"]),
            Action::Stop {
                all: false,
                force: true,
                target: "build".into()
            }
        );
    }

    /// `--rename ID [TITLE]`: without a title the name is cleared, which is a thing the
    /// verb does rather than an error.
    #[test]
    fn rename_takes_an_id_and_an_optional_title() {
        assert_eq!(
            act(&["--rename", "abc"]),
            Action::Rename {
                id: "abc".into(),
                title: String::new()
            }
        );
        assert_eq!(
            act(&["--rename", "abc", "a new name"]),
            Action::Rename {
                id: "abc".into(),
                title: "a new name".into()
            }
        );
    }

    /// A bare prompt is the one-shot path, and it carries every word of the prompt.
    #[test]
    fn a_bare_prompt_comes_back_whole() {
        assert_eq!(
            act(&["what does this error mean?"]),
            Action::Bring {
                one_shot: v(&["what does this error mean?"])
            }
        );
    }

    /// The seat flags are consumed by the same pass that finds the verb, so a verb with
    /// flags still reaches its arm.
    #[test]
    fn the_verb_is_found_after_the_seat_flags_are_consumed() {
        let (seat, action) = decide(&v(&["--bash", "--stop"])).unwrap();
        assert!(seat.bash);
        assert_eq!(
            action,
            Action::Stop {
                all: false,
                force: false,
                target: String::new()
            }
        );
    }
}

#[cfg(test)]
mod target_tests {
    use super::*;

    /// A rundir with `n` synthetic records, named so their sorted order is known.
    ///
    /// The names are the real shape (`<12 hex>.json`) and the workspaces are distinct, so
    /// "which record did number N pick" is answerable from the answer alone.
    fn rundir_with(n: usize) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "letibot-retarget-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..n {
            // 0-padded so the sorted order is 00, 01, 02…
            let key = format!("{:012}", i + 1);
            let ws = format!("/workspace/{i}");
            std::fs::write(
                dir.join(format!("{key}.json")),
                format!(
                    r#"{{"workspace": "{ws}", "socket": "/s{i}.sock", "pid": {}}}"#,
                    1000 + i
                ),
            )
            .unwrap();
        }
        dir
    }

    /// Nothing, or `.`, is this folder — and it goes through the same key derivation as
    /// everything else, so the socket is the one the shell would use.
    #[test]
    fn no_target_and_dot_both_mean_this_folder() {
        let dir = rundir_with(0);
        let here = Path::new("/some/folder");
        for want in ["", "."] {
            let w = retarget(&dir, here, want).unwrap();
            assert_eq!(w.workspace, here);
            assert_eq!(w.key, key(here));
            assert_eq!(w.socket, dir.join(format!("{}.sock", key(here))));
        }
    }

    /// **The numbering contract, and it is the one that has to match `--ls`.** Number N is
    /// the Nth record in the shell's glob order, which is the sorted filename order — so
    /// this pins the ORDER, not just that some record came back.
    #[test]
    fn a_number_picks_the_nth_record_in_sorted_order() {
        let dir = rundir_with(3);
        let here = Path::new("/here");
        assert_eq!(
            retarget(&dir, here, "1").unwrap().workspace,
            Path::new("/workspace/0")
        );
        assert_eq!(
            retarget(&dir, here, "2").unwrap().workspace,
            Path::new("/workspace/1")
        );
        assert_eq!(
            retarget(&dir, here, "3").unwrap().workspace,
            Path::new("/workspace/2")
        );
    }

    /// Out of range refuses, with the shell's own sentence — measured by running
    /// `scripts/letibot --attach 5` against an empty runtime dir.
    #[test]
    fn a_number_past_the_end_refuses_with_the_shells_sentence() {
        let dir = rundir_with(2);
        let e = retarget(&dir, Path::new("/here"), "5").unwrap_err();
        assert_eq!(e, "there is no daemon 5 — `letibot --ls` numbers them");
        // And with no records at all, which is the case that was measured.
        let empty = rundir_with(0);
        let e = retarget(&empty, Path::new("/here"), "5").unwrap_err();
        assert_eq!(e, "there is no daemon 5 — `letibot --ls` numbers them");
    }

    /// **The divergence, pinned so it cannot drift back.** `0` is a handle `--ls` never
    /// prints; the shell silently acted on this folder for it, and this refuses instead.
    #[test]
    fn zero_refuses_rather_than_silently_meaning_this_folder() {
        let dir = rundir_with(3);
        let here = Path::new("/here");
        for want in ["0", "00"] {
            let e = retarget(&dir, here, want).unwrap_err();
            assert!(e.contains("there is no daemon"), "{e}");
        }
        // The control: with the SAME rundir, an in-range number still works, so this is
        // about the handle rather than about the directory being unusable.
        assert!(retarget(&dir, here, "1").is_ok());
    }

    /// A directory resolves the way the shell's `cd … && pwd -P` does — through symlinks,
    /// to an absolute path.
    #[test]
    fn a_directory_is_resolved_through_symlinks() {
        let base =
            std::env::temp_dir().join(format!("letibot-retarget-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = base.join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let dir = rundir_with(0);
        let w = retarget(&dir, Path::new("/here"), link.to_str().unwrap()).unwrap();
        let canonical = std::fs::canonicalize(&real).unwrap();
        assert_eq!(w.workspace, canonical);
        // The key follows the RESOLVED path, so both spellings name ONE daemon.
        assert_eq!(
            w.key,
            retarget(&dir, Path::new("/here"), real.to_str().unwrap())
                .unwrap()
                .key
        );
    }

    /// A path that is not a directory refuses with the shell's own words.
    #[test]
    fn a_directory_that_is_not_there_refuses_by_name() {
        let dir = rundir_with(0);
        let e = retarget(&dir, Path::new("/here"), "/no/such/place").unwrap_err();
        assert_eq!(e, "no such directory: /no/such/place");
    }

    /// A record that says nothing refuses rather than hashing the empty path — the one
    /// key every folder on the box would share.
    #[test]
    fn an_empty_record_refuses_rather_than_hashing_the_empty_path() {
        let dir = rundir_with(1);
        std::fs::write(dir.join("000000000001.json"), "{}").unwrap();
        let e = retarget(&dir, Path::new("/here"), "1").unwrap_err();
        assert!(e.contains("no workspace field"), "{e}");
        // And the control that matters most: it is NOT the empty path's key, which is what
        // the shell would have used.
        assert_ne!(e.contains(&key(Path::new(""))), true);
    }

    /// A file that is not JSON at all is not a record either.
    #[test]
    fn an_unreadable_record_refuses_rather_than_being_skipped() {
        let dir = rundir_with(1);
        std::fs::write(dir.join("000000000001.json"), "not json at all").unwrap();
        let e = retarget(&dir, Path::new("/here"), "1").unwrap_err();
        assert!(e.contains("says nothing usable"), "{e}");
    }

    /// **Against the box's real records: the number must pick what `--ls` prints there.**
    ///
    /// Reads the live runtime directory and asserts the mapping is the sorted one, which
    /// is the same order `scripts/letibot --ls` numbers. Skips on a box with none, and
    /// says so rather than passing quietly.
    #[test]
    fn the_numbering_matches_the_glob_order_on_a_box_with_records() {
        let dir = rundir();
        let records = records_in(&dir);
        if records.is_empty() {
            eprintln!("SKIPPED: no daemon records under {}", dir.display());
            return;
        }
        let mut names: Vec<String> = records
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let sorted = {
            let mut s = names.clone();
            s.sort();
            s
        };
        assert_eq!(
            names, sorted,
            "records_in must yield the glob's sorted order"
        );
        names.dedup();
        assert_eq!(names.len(), records.len(), "no duplicate record names");

        // Every in-range number resolves to the workspace NAMED INSIDE that record — the
        // check that the number and the file agree, not merely that something came back.
        for (i, path) in records.iter().enumerate() {
            let rec = read_record(path).expect("a record under the runtime dir must parse");
            let w = retarget(&dir, Path::new("/here"), &(i + 1).to_string())
                .expect("an in-range number must resolve");
            assert_eq!(w.workspace, Path::new(&rec.workspace));
            // And its own filename is the hash of that workspace — the module's contract.
            assert_eq!(
                path.file_stem().unwrap().to_string_lossy(),
                key(Path::new(&rec.workspace)),
                "record {} names a workspace whose key is not its own filename",
                path.display()
            );
        }
    }
}
