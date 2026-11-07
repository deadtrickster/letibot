//! One SIGTERM stops a serving daemon, even when its stderr can no longer be written. A full
//! disk is the case on the operator's box; a pipe whose reader is gone fails the same write.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long the daemon may take to bind its socket.
const PATIENCE: Duration = Duration::from_secs(60);

/// Above the daemon's own serve grace, so a pass is the orderly stop or the grace and a
/// failure is a daemon that only SIGKILL would end.
const BUDGET: Duration = Duration::from_secs(45);

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start(dir: &Path) -> (Daemon, PathBuf) {
    let home = dir.join("home");
    let config = dir.join("config/letibot");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::create_dir_all(home.join("logs")).expect("logs dir");
    // Nothing listens on the discard port: no request this test makes leaves loopback.
    std::fs::write(
        config.join("providers.toml"),
        "[deepseek]\nkey = \"sk-not-used\"\nurl = \"http://127.0.0.1:9/v1/chat/completions\"\n",
    )
    .expect("providers.toml");
    let workspace = dir.join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let socket = dir.join(format!("sigterm-{}.sock", std::process::id()));
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root");
    let child = Command::new(env!("CARGO_BIN_EXE_harnessd"))
        .arg("--socket")
        .arg(&socket)
        .arg("--store")
        .arg(dir.join("sessions.db"))
        .arg("--workspace")
        .arg(&workspace)
        .arg("--session")
        .arg("s-daemon-sigterm")
        .arg("--role")
        .arg("leticode")
        .arg("--provider")
        .arg("deepseek")
        .current_dir(repo)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_RUNTIME_DIR", dir)
        .env("LETIBOT_LANDING_ROOT", home.join("landing"))
        .env("LETIBOT_NOTES_REPO", home.join("notes"))
        .env("LETIBOT_LOGS_ROOT", home.join("logs"))
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("GLM_API_KEY")
        .env_remove("LETIBOT_PERMISSION")
        .env_remove("LETIBOT_MODE")
        .env_remove("LETIBOT_MODEL")
        .env_remove("LETIBOT_VOCAB")
        .env_remove("LETIBOT_ENDPOINT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("harnessd starts");
    (
        Daemon {
            child,
            dir: dir.to_path_buf(),
        },
        socket,
    )
}

#[test]
fn one_sigterm_stops_a_daemon_whose_stderr_cannot_be_written() {
    let dir = letibot_sessionlog::testing::scratch("letibot-daemon-sigterm");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let (mut d, socket) = start(&dir);

    // Read the log until the socket is bound, keeping what was said for a failure message.
    let stderr = d.child.stderr.take().expect("the stderr pipe");
    let mut said = Vec::new();
    let mut lines = BufReader::new(stderr).lines();
    let deadline = Instant::now() + PATIENCE;
    while !socket.exists() {
        assert!(
            Instant::now() < deadline,
            "the daemon never bound its socket:\n{}",
            said.join("\n")
        );
        match lines.next() {
            Some(Ok(l)) => said.push(l),
            _ => panic!("the daemon exited at start:\n{}", said.join("\n")),
        }
    }
    // The reader goes: every later write to stderr fails with EPIPE, as on a full disk.
    drop(lines);
    // A daemon that dies of the failed write is the same defect, said before the signal.
    std::thread::sleep(Duration::from_secs(2));
    if let Ok(Some(status)) = d.child.try_wait() {
        panic!(
            "the daemon died once its stderr failed ({status}), before any signal:\n{}",
            said.join("\n")
        );
    }

    // SAFETY: `kill` with a child pid this test owns and a valid signal number.
    let rc = unsafe { libc::kill(d.child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "kill -TERM");
    let started = Instant::now();
    while started.elapsed() < BUDGET {
        if let Ok(Some(_)) = d.child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "one SIGTERM did not stop the daemon within {BUDGET:?} once its stderr failed. Its log \
         before that:\n{}",
        said.join("\n")
    );
}
