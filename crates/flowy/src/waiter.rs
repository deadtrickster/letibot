//! One waiter per name, enforced — against `flowy inbox` itself.
//!
//! flowy allows many writers under one name and exactly one reader. Its CLI
//! enforces that locally by writing a claim — `$XDG_RUNTIME_DIR/flowy/inbox-<name>.pid`
//! holding a pid, with a `.kind` sidecar saying `tracked` or `forked` — and
//! testing liveness with `kill -0`. `DECISIONS.md` D2 names the consequence for
//! this harness: the interactive session's `flowy-listen-loop` and harnessd must
//! not both hold a seat's reader, *"a test harness that starts harnessd without
//! stopping the Monitor gets `LISTENER REFUSED`, which is the correct outcome and
//! should be asserted rather than worked around."*
//!
//! So this writes **the same file, in the same format, in the same directory**,
//! and reads it the same way. A `flowy inbox --as NAME` started while harnessd
//! holds NAME is refused by flowy's own guard, naming harnessd's pid; a harnessd
//! started while `flowy inbox` holds it is refused here, naming that pid. Two
//! guards that cannot see each other's claim are no guard.
//!
//! The one asymmetry flowy has is kept: a **tracked** waiter stands down a
//! **forked** one. The successor flowy forks at delivery is detached — it hears
//! everything and can wake nobody — and a listener that can wake a session must
//! be able to replace it, or a session ends up with a live listener and silence.

use std::path::{Path, PathBuf};

/// A held claim. Released on drop; only the claim this process wrote is removed.
#[derive(Debug)]
pub struct WaiterClaim {
    path: PathBuf,
    pid: u32,
    pub name: String,
}

#[derive(Debug)]
pub enum ClaimError {
    /// A live tracked waiter holds the name. Carries the pid, because the only
    /// useful thing to say is which one is already theirs.
    Held {
        name: String,
        pid: u32,
        kind: String,
    },
    Io {
        path: PathBuf,
        err: std::io::Error,
    },
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::Held { name, pid, kind } => write!(
                f,
                "LISTENER REFUSED: a waiter for `{name}` is already running (pid {pid}, \
                 {kind}). Two of them share one cursor, so the second would take messages \
                 the first should have delivered — and both would look healthy. Keep that \
                 one, or stop it with `kill {pid}` if it is not the one a harness is \
                 watching"
            ),
            ClaimError::Io { path, err } => {
                write!(f, "cannot record this waiter at {}: {err}", path.display())
            }
        }
    }
}

impl std::error::Error for ClaimError {}

/// Where the claims live: `$XDG_RUNTIME_DIR/flowy`, else `~/.cache/flowy` —
/// flowy's `waiterDir`, so the two guards meet.
pub fn claim_dir() -> Result<PathBuf, std::io::Error> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .ok_or_else(|| std::io::Error::other("neither XDG_RUNTIME_DIR nor HOME is set"))?;
    let dir = base.join("flowy");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// flowy's `unsafeInName`: everything that is not `[A-Za-z0-9._-]` becomes `-`.
fn safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub fn claim_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("inbox-{}.pid", safe(name)))
}

fn kind_path(pid_path: &Path) -> PathBuf {
    let mut p = pid_path.as_os_str().to_owned();
    p.push(".kind");
    PathBuf::from(p)
}

/// The pid recorded at `path`, and whether it is alive. `EPERM` is alive — it
/// exists and belongs to somebody else.
fn live_pid_in(path: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    let pid: i32 = raw.trim().parse().ok()?;
    if pid <= 0 {
        return None;
    }
    // SAFETY: kill with signal 0 sends nothing; it asks whether the pid exists.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) {
        Some(pid as u32)
    } else {
        None
    }
}

/// `tracked` unless the sidecar says `forked`. Unreadable is tracked: refuse
/// rather than kill something whose nature is unknown.
fn kind_in(pid_path: &Path) -> String {
    match std::fs::read_to_string(kind_path(pid_path)) {
        Ok(s) if s.trim() == "forked" => "forked".into(),
        _ => "tracked".into(),
    }
}

impl WaiterClaim {
    /// Claim `name` in `dir` for this process, or refuse naming the holder.
    pub fn hold_in(dir: &Path, name: &str) -> Result<WaiterClaim, ClaimError> {
        let path = claim_path(dir, name);
        if let Some(held) = live_pid_in(&path) {
            let kind = kind_in(&path);
            if kind == "forked" {
                // A tracked waiter stands down a forked one — see the module docs.
                // SAFETY: SIGTERM to a pid we read from flowy's own claim file.
                unsafe {
                    libc::kill(held as i32, libc::SIGTERM);
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            } else if held != std::process::id() {
                return Err(ClaimError::Held {
                    name: name.to_string(),
                    pid: held,
                    kind,
                });
            }
        }
        let pid = std::process::id();
        let io = |err| ClaimError::Io {
            path: path.clone(),
            err,
        };
        std::fs::write(&path, format!("{pid}\n")).map_err(io)?;
        let _ = std::fs::write(kind_path(&path), "tracked\n");
        Ok(WaiterClaim {
            path,
            pid,
            name: name.to_string(),
        })
    }

    /// Claim in flowy's own directory.
    pub fn hold(name: &str) -> Result<WaiterClaim, ClaimError> {
        let dir = claim_dir().map_err(|err| ClaimError::Io {
            path: PathBuf::from("$XDG_RUNTIME_DIR/flowy"),
            err,
        })?;
        WaiterClaim::hold_in(&dir, name)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WaiterClaim {
    fn drop(&mut self) {
        // Only the claim this process wrote. A stale lock and the live one that
        // took its file over come apart here, and the pid test is what keeps the
        // guard from disabling itself at the moment it is needed.
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return;
        };
        if raw.trim() != self.pid.to_string() {
            return;
        }
        let _ = std::fs::remove_file(kind_path(&self.path));
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "letibot-waiter-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writes_flowys_file_format_and_releases_on_drop() {
        let d = tmp();
        let c = WaiterClaim::hold_in(&d, "seat/one").unwrap();
        let p = d.join("inbox-seat-one.pid");
        assert_eq!(
            std::fs::read_to_string(&p).unwrap().trim(),
            std::process::id().to_string()
        );
        assert_eq!(
            std::fs::read_to_string(kind_path(&p)).unwrap().trim(),
            "tracked"
        );
        drop(c);
        assert!(!p.exists());
    }

    #[test]
    fn a_live_tracked_holder_is_refused_naming_its_pid() {
        let d = tmp();
        // A process that is certainly alive and is not us: pid 1.
        let p = claim_path(&d, "x");
        std::fs::write(&p, "1\n").unwrap();
        let err = WaiterClaim::hold_in(&d, "x").unwrap_err();
        let s = err.to_string();
        assert!(s.starts_with("LISTENER REFUSED"), "{s}");
        assert!(s.contains("pid 1,"), "{s}");
    }

    #[test]
    fn a_stale_claim_is_taken_over() {
        let d = tmp();
        let p = claim_path(&d, "y");
        // A pid nothing is running under: far above pid_max, inside i32.
        std::fs::write(&p, "2147483000\n").unwrap();
        let c = WaiterClaim::hold_in(&d, "y").unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap().trim(),
            std::process::id().to_string()
        );
        drop(c);
    }

    #[test]
    fn dropping_does_not_remove_a_claim_somebody_else_took_over() {
        let d = tmp();
        let c = WaiterClaim::hold_in(&d, "z").unwrap();
        std::fs::write(c.path(), "1\n").unwrap();
        let p = c.path().to_path_buf();
        drop(c);
        assert_eq!(std::fs::read_to_string(&p).unwrap().trim(), "1");
    }
}
