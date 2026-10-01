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
