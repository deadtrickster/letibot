//! The launcher never waits on a user manager that does not answer. Each case puts a
//! `systemctl` or `systemd-run` that hangs first on `PATH`, while the room's own socket answers.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn scratch(tag: &str) -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("lb-nohang-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// The binary under test, copied beside the stubs: `current_exe()` resolves a link.
fn letibot_multi(bin: &Path) -> PathBuf {
    let me = bin.join("letibot-multi");
    std::fs::copy(env!("CARGO_BIN_EXE_letibot-multi"), &me).expect("the binary is copied");
    std::fs::set_permissions(&me, std::fs::Permissions::from_mode(0o755)).expect("executable");
    me
}

fn is_on(bin: &Path, home: &Path, socket: &Path) -> (bool, Duration, String) {
    let t = Instant::now();
    let out = letibot_testenv::command(bin.join("letibot-multi"))
        .args(["rooms", "is-on"])
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("LETIBOT_ROOM_SOCKET", socket)
        .output()
        .expect("is-on runs");
    (
        out.status.success(),
        t.elapsed(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn is_on_answers_from_the_socket_while_systemctl_hangs() {
    let dir = scratch("ison");
    let (home, bin) = (dir.join("home"), dir.join("bin"));
    let units = home.join(".config/systemd/user");
    for d in [&home, &bin, &units] {
        std::fs::create_dir_all(d).unwrap();
    }
    letibot_multi(&bin);
    letibot_tools::testing::write_stub(
        &bin.join("systemctl"),
        "#!/bin/sh\n[ -n \"$LETIBOT_STUB_PROBE\" ] && exit 0\nexec sleep 1000\n",
    );
    for f in ["letibot-rooms.socket", "letibot-rooms.service"] {
        std::fs::write(units.join(f), "[Unit]\n").unwrap();
    }
    let socket = dir.join("r.sock");

    // Nothing listens: the manager is asked, does not answer, and the bound ends the wait.
    let (ok, took, err) = is_on(&bin, &home, &socket);
    assert!(!ok, "nothing serves the room: {err}");
    assert!(
        took < Duration::from_secs(4),
        "is-on waited {took:?} on a hung systemctl"
    );
    assert!(err.contains("did not answer"), "{err}");

    // A coordinator answers: is-on says so from the socket, without the manager.
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    std::thread::spawn(move || {
        use std::io::{BufRead, Write};
        for s in listener.incoming().flatten() {
            let mut line = String::new();
            let mut r = std::io::BufReader::new(s.try_clone().unwrap());
            let _ = r.read_line(&mut line);
            let _ = (&s).write_all(b"{\"type\":\"bye\",\"why\":\"hello first\"}\n");
        }
    });
    let (ok, took, err) = is_on(&bin, &home, &socket);
    assert!(ok, "the room answered: {err}");
    assert!(took < Duration::from_secs(3), "is-on took {took:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_launcher_starts_the_daemon_when_systemd_run_hangs() {
    // A short root: the daemon's socket path must fit `sun_path`.
    let dir = PathBuf::from(format!("/tmp/lbnh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (home, bin, rundir, wt) = (
        dir.join("home"),
        dir.join("bin"),
        dir.join("run"),
        dir.join("wt"),
    );
    for d in [&home, &bin, &rundir, &wt] {
        std::fs::create_dir_all(d).unwrap();
    }
    letibot_multi(&bin);
    // Answers after the launcher's bound, so the backgrounded probe ends inside the test.
    letibot_tools::testing::write_stub(
        &bin.join("systemd-run"),
        "#!/bin/sh\n[ -n \"$LETIBOT_STUB_PROBE\" ] && exit 0\nsleep 6\nexit 0\n",
    );
    letibot_tools::testing::write_stub(
        &bin.join("systemctl"),
        "#!/bin/sh\n[ -n \"$LETIBOT_STUB_PROBE\" ] && exit 0\nexec sleep 1000\n",
    );
    letibot_tools::testing::write_stub(&bin.join("head-stub"), "#!/bin/sh\necho head \"$@\"\n");
    // Binds the socket it is given and records the late-answer path the launcher handed it.
    letibot_tools::testing::write_stub(
        &bin.join("harnessd"),
        r#"#!/bin/sh
[ $# -eq 0 ] && exit 0
sock=""
while [ $# -gt 0 ]; do
  case "$1" in
    --socket) sock="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf '%s' "$LETIBOT_SCOPE_LATE" >"$HOME/late-env"
exec python3 -c '
import socket, sys, time
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(sys.argv[1])
s.listen(8)
time.sleep(30)
' "$sock"
"#,
    );
    // A room that is up, so the launcher has nothing to start for it.
    let room = dir.join("room.sock");
    let _room = std::os::unix::net::UnixListener::bind(&room).expect("bind the room");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/letibot")
        .canonicalize()
        .unwrap();
    let t = Instant::now();
    let out = letibot_testenv::command("bash")
        .arg(&script)
        .arg("--new")
        .current_dir(&wt)
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &rundir)
        .env("LETIBOT_BIN", &bin)
        .env("LETIBOT_HEAD", bin.join("head-stub"))
        .env("LETIBOT_ROOM_SOCKET", &room)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the launcher runs");
    let took = t.elapsed();
    let err = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "the launcher refused:\n{err}");
    assert!(
        stdout.starts_with("head --socket"),
        "the head was started:\n{stdout}\n{err}"
    );
    assert!(
        took < Duration::from_secs(5),
        "the launcher waited {took:?} on systemd:\n{err}"
    );
    assert_eq!(
        err.matches("the systemd user manager is not answering")
            .count(),
        1,
        "one degraded line:\n{err}"
    );
    // Quiet by default: the degraded line, the daemon's start, and nothing routine.
    assert!(!err.contains("skipped"), "{err}");
    assert!(err.lines().count() <= 3, "startup said too much:\n{err}");

    // The daemon was handed the probe's answer file, and the probe still answers into it.
    let until = Instant::now() + Duration::from_secs(15);
    let late = loop {
        if let Ok(p) = std::fs::read_to_string(home.join("late-env"))
            && !p.is_empty()
            && Path::new(&p).is_file()
        {
            break p;
        }
        assert!(Instant::now() < until, "no late answer:\n{err}");
        std::thread::sleep(Duration::from_millis(200));
    };
    let answer = std::fs::read_to_string(&late).unwrap();
    assert!(answer.starts_with("0\t"), "{answer:?}");
    let said = letibot_harnessd::late_scope::sentence(&answer).expect("the daemon reads it");
    assert!(said.contains("accepted a scope"), "{said}");
    eprintln!("launcher stderr:\n{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A one-shot run by the launcher against a stub `systemd-run`, with a harnessd that says it ran
/// and exits 3. The stub answers the probe (`... true`) at once unless `probe_hangs`.
struct OneShot {
    dir: PathBuf,
    home: PathBuf,
    rundir: PathBuf,
    status: Option<i32>,
    took: Duration,
    stdout: String,
    stderr: String,
}

fn one_shot(tag: &str, scope: &str, probe_hangs: bool) -> OneShot {
    let dir = PathBuf::from(format!("/tmp/lbnh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (home, bin, rundir, wt) = (
        dir.join("home"),
        dir.join("bin"),
        dir.join("run"),
        dir.join("wt"),
    );
    for d in [&home, &bin, &rundir.join("letibot"), &wt] {
        std::fs::create_dir_all(d).unwrap();
    }
    letibot_multi(&bin);
    // An unanswered probe and a join gate from a run two hours ago.
    for old in ["scope-probe-1", "scope-join-1-1"] {
        let p = rundir.join("letibot").join(old);
        std::fs::create_dir_all(&p).unwrap();
        let _ = std::process::Command::new("touch")
            .args(["-d", "2 hours ago"])
            .arg(&p)
            .status();
    }
    // Past the launcher's bound, and short, so the probe it leaves running ends with the test.
    let probe = if probe_hangs {
        "exec sleep 20"
    } else {
        "exit 0"
    };
    letibot_tools::testing::write_stub(
        &bin.join("systemd-run"),
        &format!(
            "#!/bin/sh\n[ -n \"$LETIBOT_STUB_PROBE\" ] && exit 0\n\
             for a; do last=$a; done\n[ \"$last\" = true ] && {{ {probe}; }}\n\
             echo $$ >>\"$HOME/scope-pids\"\n{scope}\n"
        ),
    );
    letibot_tools::testing::write_stub(
        &bin.join("harnessd"),
        "#!/bin/sh\n[ $# -eq 0 ] && exit 0\necho ran\nexit 3\n",
    );
    let room = dir.join("room.sock");
    let _room = std::os::unix::net::UnixListener::bind(&room).expect("bind the room");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/letibot")
        .canonicalize()
        .unwrap();
    let t = Instant::now();
    let out = letibot_testenv::command("bash")
        .arg(&script)
        .arg("say hi")
        .current_dir(&wt)
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &rundir)
        .env("LETIBOT_BIN", &bin)
        .env("LETIBOT_ROOM_SOCKET", &room)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the launcher runs");
    OneShot {
        took: t.elapsed(),
        status: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        dir,
        home,
        rundir,
    }
}

impl OneShot {
    /// No `systemd-run` the launcher started is still running.
    fn no_join_left(&self) -> bool {
        let pids = std::fs::read_to_string(self.home.join("scope-pids")).unwrap_or_default();
        pids.lines()
            .all(|p| !Path::new(&format!("/proc/{}", p.trim())).exists())
    }
    fn swept(&self) -> bool {
        ["scope-probe-1", "scope-join-1-1"]
            .iter()
            .all(|o| !self.rundir.join("letibot").join(o).exists())
    }
}

impl Drop for OneShot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_hanging_real_scope_does_not_hang_the_one_shot() {
    let r = one_shot("hang", "exec sleep 20", false);
    assert!(
        r.took < Duration::from_secs(15),
        "the one-shot waited {:?} on a scope that never joined:\n{}",
        r.took,
        r.stderr
    );
    assert_eq!(
        r.stdout.trim(),
        "ran",
        "the one-shot ran unscoped:\n{}",
        r.stderr
    );
    assert_eq!(r.status, Some(3), "its own status:\n{}", r.stderr);
    assert!(r.stderr.contains("made no scope"), "{}", r.stderr);
    assert!(r.no_join_left(), "the given-up join is killed");
    assert!(r.swept(), "hour-old probe and join dirs are swept");
}

#[test]
fn a_scope_that_joins_runs_the_one_shot_in_it_with_its_status() {
    // The real `systemd-run --scope` runs the command in place once the scope is made.
    let join = "while [ $# -gt 0 ]; do case \"$1\" in --unit|--slice|-p) shift 2 ;; -*) shift ;; *) break ;; esac; done\n\
                echo scoped >>\"$HOME/scoped\"\nexec \"$@\"";
    let r = one_shot("join", join, false);
    assert_eq!(r.stdout.trim(), "ran", "{}", r.stderr);
    assert_eq!(
        r.status,
        Some(3),
        "the scoped command's status:\n{}",
        r.stderr
    );
    assert!(r.home.join("scoped").is_file(), "it ran in the scope");
    assert!(!r.stderr.contains("made no scope"), "{}", r.stderr);
    assert!(r.took < Duration::from_secs(10), "{:?}", r.took);
}

#[test]
fn a_probe_that_never_answers_does_not_hang_the_one_shot() {
    let r = one_shot("never", "exec sleep 20", true);
    assert!(
        r.took < Duration::from_secs(10),
        "{:?}:\n{}",
        r.took,
        r.stderr
    );
    assert_eq!(r.stdout.trim(), "ran", "{}", r.stderr);
    assert_eq!(r.status, Some(3), "{}", r.stderr);
    assert!(
        r.stderr
            .contains("the systemd user manager is not answering"),
        "{}",
        r.stderr
    );
}

#[test]
fn a_hanging_real_scope_does_not_hang_the_daemon_start() {
    let dir = PathBuf::from(format!("/tmp/lbnh-dhang-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (home, bin, rundir, wt) = (
        dir.join("home"),
        dir.join("bin"),
        dir.join("run"),
        dir.join("wt"),
    );
    for d in [&home, &bin, &rundir, &wt] {
        std::fs::create_dir_all(d).unwrap();
    }
    letibot_multi(&bin);
    letibot_tools::testing::write_stub(
        &bin.join("systemd-run"),
        "#!/bin/sh\nfor a; do last=$a; done\n[ \"$last\" = true ] && exit 0\n\
         echo $$ >>\"$HOME/scope-pids\"\nexec sleep 20\n",
    );
    letibot_tools::testing::write_stub(
        &bin.join("systemctl"),
        "#!/bin/sh\n[ -n \"$LETIBOT_STUB_PROBE\" ] && exit 0\nexec sleep 1000\n",
    );
    letibot_tools::testing::write_stub(&bin.join("head-stub"), "#!/bin/sh\necho head \"$@\"\n");
    letibot_tools::testing::write_stub(
        &bin.join("harnessd"),
        r#"#!/bin/sh
[ $# -eq 0 ] && exit 0
sock=""
while [ $# -gt 0 ]; do
  case "$1" in
    --socket) sock="$2"; shift 2 ;;
    *) shift ;;
  esac
done
exec python3 -c '
import socket, sys, time
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(sys.argv[1])
s.listen(8)
time.sleep(10)
' "$sock"
"#,
    );
    let room = dir.join("room.sock");
    let _room = std::os::unix::net::UnixListener::bind(&room).expect("bind the room");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/letibot")
        .canonicalize()
        .unwrap();
    let t = Instant::now();
    let out = letibot_testenv::command("bash")
        .arg(&script)
        .arg("--new")
        .current_dir(&wt)
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &rundir)
        .env("LETIBOT_BIN", &bin)
        .env("LETIBOT_HEAD", bin.join("head-stub"))
        .env("LETIBOT_ROOM_SOCKET", &room)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the launcher runs");
    let took = t.elapsed();
    let err = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "the launcher refused:\n{err}");
    assert!(stdout.starts_with("head --socket"), "{stdout}\n{err}");
    assert!(took < Duration::from_secs(15), "waited {took:?}:\n{err}");
    let log = std::fs::read_dir(home.join("logs"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect::<String>();
    assert!(
        log.contains("made no scope"),
        "the daemon's log says so:\n{log}"
    );
    let pids = std::fs::read_to_string(home.join("scope-pids")).unwrap_or_default();
    assert!(
        pids.lines()
            .all(|p| !Path::new(&format!("/proc/{}", p.trim())).exists()),
        "the given-up join is killed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
