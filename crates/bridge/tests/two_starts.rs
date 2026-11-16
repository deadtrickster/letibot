//! Two starts in one folder race past `is_up`. The loser's daemon exits on the folder claim and
//! the winner's socket answers, so the loser's call must join it (and open its own session) or
//! refuse. Taking the winner's daemon as its own handed the caller the winner's first session.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_bridge::daemon;
use letibot_tools::builtins::task::Placement;

fn scratch(tag: &str) -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "lb-race-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u32)
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// The winner: binds the socket, writes the placement record a real daemon writes after its bind,
/// and stays while the test process lives.
const WINNER: &str = r#"
import os, socket, sys, time
sock, pidfile, owner = sys.argv[1], sys.argv[2], int(sys.argv[3])
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(sock)
s.listen(8)
st = os.stat(sock)
with open(sock[: -len(".sock")] + ".placement", "w") as f:
    f.write("socket %d:%d\nstamp %d.%d\nplacement host\n"
            % (st.st_dev, st.st_ino, st.st_ctime_ns // 10**9, st.st_ctime_ns % 10**9))
with open(pidfile, "w") as f:
    f.write(str(os.getpid()))
while True:
    try:
        os.kill(owner, 0)
    except ProcessLookupError:
        break
    time.sleep(0.2)
"#;

/// A `harnessd` that loses the race: another daemon binds first, and this one exits.
fn losing_daemon(path: &Path, winner: &Path, pidfile: &Path) {
    letibot_tools::testing::write_stub(
        path,
        &format!(
            r#"#!/bin/sh
[ $# -eq 0 ] && exit 0
sock=""
while [ $# -gt 0 ]; do
  case "$1" in
    --socket) sock="$2"; shift 2 ;;
    *) shift ;;
  esac
done
python3 {winner} "$sock" {pidfile} {owner} &
i=0
while [ ! -S "$sock" ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i + 1)); done
exit 1
"#,
            winner = winner.display(),
            pidfile = pidfile.display(),
            owner = std::process::id(),
        ),
    );
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}

#[test]
fn two_starts_in_one_folder_both_survive_or_the_second_refuses() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "python3, which the winning daemon binds its socket with",
        on_path("python3"),
    ) else {
        return;
    };
    let dir = scratch("two");
    let (home, rundir, bin, worktree) = (
        dir.join("home"),
        dir.join("run"),
        dir.join("bin"),
        dir.join("ws"),
    );
    for d in [&home, &rundir, &bin, &worktree] {
        std::fs::create_dir_all(d).expect("a fixture directory");
    }
    let winner = dir.join("winner.py");
    std::fs::write(&winner, WINNER).expect("the winner script");
    let pidfile = dir.join("winner.pid");
    losing_daemon(&bin.join("harnessd"), &winner, &pidfile);
    let room = bin.join("room");
    letibot_tools::testing::write_stub(&room, "#!/bin/sh\nexit 0\n");
    // SAFETY: one test in this binary, set before any thread reads the environment.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("XDG_RUNTIME_DIR", &rundir);
        std::env::set_var("LETIBOT_BIN", &bin);
        std::env::set_var("LETIBOT_ROOM_SUPERVISOR", &room);
        std::env::remove_var("LETIBOT_SOCKET");
        std::env::remove_var("LETIBOT_LOG");
        std::env::remove_var("LETIBOT_STORE");
    }

    let got = daemon::ensure(&worktree, None, "writes-allowed", &Placement::Host);

    let deadline = Instant::now() + Duration::from_secs(10);
    let winner_pid = loop {
        if let Some(p) = std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|t| t.trim().parse::<i32>().ok())
        {
            break p;
        }
        assert!(Instant::now() < deadline, "the winner never started");
        std::thread::sleep(Duration::from_millis(50));
    };
    match &got {
        Ok(d) => {
            assert!(
                !d.started,
                "the socket is another start's daemon, so this call must join it and open its own \
                 session, not hand back that daemon's first session as its own: {d:?}"
            );
            assert_eq!(d.placement, Some(Placement::Host), "{d:?}");
            assert!(
                !d.socket.with_extension("json").exists(),
                "no record naming the daemon that exited"
            );
        }
        Err(why) => assert!(!why.is_empty()),
    }
    // SAFETY: SIGTERM to the pid this test's stub started.
    unsafe {
        libc::kill(winner_pid, libc::SIGTERM);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
