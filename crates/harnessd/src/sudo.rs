//! `sudo` inside a session: the password is asked of the head, never of a tty.
//!
//! Forty `sudo` attempts by agents on this box in 22 days, none of which could
//! succeed — every one died on *a terminal is required to authenticate*, and two
//! tried to fake the terminal (`SUDO_USE.md`). The operator's ask, 2026-09-14:
//! *"if model wants a sudo i must be able to enter password safely and let it
//! run"*.
//!
//! # The shape
//!
//! 1. Every command of a host session runs with `SUDO_ASKPASS` pointing at
//!    `letibot-askpass` (beside this binary) and a `sudo` **shim** ahead of
//!    `PATH` that execs the real sudo with `-A`, so sudo uses the helper rather
//!    than looking for a terminal it does not have. `sudo -n` is untouched: a
//!    probe still answers *no*, honestly.
//! 2. The command itself has already passed the gate — `sudo` is on the
//!    always-ask list, so the operator saw *"sudo apt install x"* and admitted
//!    it before any password is asked.
//! 3. The helper attaches to the daemon as a head of kind `askpass` and sends
//!    one frame; the daemon shows every attached head a card with the command
//!    and a masked field; the answer travels head → daemon → helper → sudo's
//!    stdin and nowhere else. Not logged, not stored, not in the transcript,
//!    never seen by the model. Two minutes, then sudo is told no password came.
//!
//! The shim and the helper are the two files this module writes and finds.

use std::path::{Path, PathBuf};

/// Where the two pieces are.
#[derive(Debug, Clone)]
pub struct Plumbing {
    /// The helper sudo runs.
    pub askpass: PathBuf,
    /// The directory holding the `sudo` shim, for the front of `PATH`.
    pub shims: PathBuf,
}

/// The real sudo, found on the pinned `PATH` minus our own shim directory.
fn real_sudo() -> Option<PathBuf> {
    ["/usr/bin/sudo", "/bin/sudo", "/usr/local/bin/sudo"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// The shim directory: `$XDG_RUNTIME_DIR/letibot/shims` (or `/tmp/letibot-<uid>/shims`).
fn shim_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d).join("letibot").join("shims"),
        None => std::env::temp_dir().join("letibot-shims"),
    }
}

/// Find the helper beside this binary and write the shim. Idempotent; a missing
/// helper or sudo is an error the caller discloses, not a panic.
pub fn install() -> Result<Plumbing, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    // `$LETIBOT_ASKPASS` names the helper outright (a test binary has none
    // beside it); otherwise it is beside this binary.
    let askpass = match std::env::var_os("LETIBOT_ASKPASS").map(PathBuf::from) {
        Some(p) if p.is_file() => p,
        Some(p) => return Err(format!("$LETIBOT_ASKPASS = {} is not a file", p.display())),
        None => exe
            .parent()
            .map(|d| d.join("letibot-askpass"))
            .filter(|p| p.is_file())
            .ok_or_else(|| {
                format!(
                    "letibot-askpass is not beside {} — build it (`cargo build --release -p \
                     letibot-harnessd`)",
                    exe.display()
                )
            })?,
    };
    let sudo = real_sudo().ok_or_else(|| "no sudo on this box".to_string())?;
    let shims = shim_dir();
    std::fs::create_dir_all(&shims).map_err(|e| format!("{}: {e}", shims.display()))?;
    write_shim(&shims.join("sudo"), &sudo)?;
    Ok(Plumbing { askpass, shims })
}

fn write_shim(path: &Path, real: &Path) -> Result<(), String> {
    let body = format!(
        "#!/bin/sh\n\
         # letibot: sudo asks the head for the password (SUDO_ASKPASS), never a tty.\n\
         # -n is left alone, so a probe still answers no; everything else gets -A.\n\
         for a in \"$@\"; do\n\
         \x20 case \"$a\" in -n|--non-interactive) exec {real} \"$@\" ;; esac\n\
         done\n\
         exec {real} -A \"$@\"\n",
        real = real.display()
    );
    if std::fs::read_to_string(path).ok().as_deref() == Some(body.as_str()) {
        return Ok(());
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    wait_until_executable(path)
}

/// **Do not return until `path` can be exec'd**, because "written, chmod'd and renamed into
/// place" is not the same thing.
///
/// `execve` answers `ETXTBSY` for a file **any process holds open for writing**, and the holder
/// is not hypothetical: a thread forked while the write descriptor was open keeps it until its
/// own `exec`, and this daemon forks constantly. It is a fact about the INODE, so the rename
/// above does not help — `deny_write_access` is asked of the inode, not of the name. Only an
/// exec that succeeds proves nobody holds a writer, and nobody can get one afterwards.
///
/// The probe is `sudo --version`, which is the one sudo invocation that answers without a
/// password, a terminal or a question for the head; its output goes nowhere.
/// `letibot_tools::testing::write_stub` is the same probe for a fixture's stub.
fn wait_until_executable(path: &Path) -> Result<(), String> {
    for _ in 0..100 {
        match std::process::Command::new(path)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            // Whatever it answered, the file was handed to the kernel: that is the proof.
            _ => return Ok(()),
        }
    }
    Err(format!("{} stayed busy for two seconds", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A script this test just wrote, exec'd with no retry of its own.**
    ///
    /// `write_shim` does not return until the file can be exec'd (see `wait_until_executable`),
    /// so a `Text file busy` here would be that guarantee broken rather than the test being
    /// early — which is why this is not the retry loop it used to be.
    fn run(cmd: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new(cmd)
            .args(args)
            .output()
            .expect("the shim runs")
    }

    #[test]
    fn the_shim_adds_dash_a_and_leaves_a_probe_alone() {
        let d = std::env::temp_dir().join(format!("letibot-sudo-shim-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let fake = d.join("real-sudo");
        letibot_tools::testing::write_stub(&fake, "#!/bin/sh\nprintf '%s\\n' \"$*\"\n");
        let shim = d.join("sudo");
        write_shim(&shim, &fake).unwrap();
        let out = run(&shim, &["apt", "install", "x"]);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "-A apt install x"
        );
        let out = run(&shim, &["-n", "true"]);
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "-n true");
        // Idempotent: a second write changes nothing.
        write_shim(&shim, &fake).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }
}
