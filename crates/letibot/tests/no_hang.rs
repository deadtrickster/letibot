//! The launcher never waits on a user manager that does not answer. Each case puts a
//! `systemctl` or `systemd-run` that hangs first on `PATH`, while the room's own socket answers.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// How long a case may take before the thing it proves never happened. Not a measurement of the
/// box: a loaded box stretches every step of the launcher, and each of these cases is about a wait
/// that must END, not about how quickly it ends.
const NEVER: Duration = Duration::from_secs(60);

/// A launcher run as the shell sees it: its own status and how long ITS process took.
///
/// `Command::output()` is not that number. It reads both pipes to EOF, so a child the launcher
/// leaves behind holds the run open for as long as it lives - which is the same hang a caller
/// would meet as `x=$(letibot ...)`. Here the clock stops when the launcher is reaped, and the
/// streams are collected separately with a bound of their own.
struct Ran {
    status: ExitStatus,
    took: Duration,
    /// When both pipes closed, or `None` when a child the launcher left behind still held one: the
    /// launcher is gone and its caller's `x=$(letibot ...)` would still be reading.
    eof: Option<Duration>,
    stdout: String,
    stderr: String,
}

/// Collect a pipe on a thread, into a buffer the caller can read while it is still filling, so a
/// stream a leftover child never closes costs no more than the grace below.
fn collect(
    mut f: impl std::io::Read + Send + 'static,
) -> (Arc<Mutex<Vec<u8>>>, mpsc::Receiver<()>) {
    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let (done, rx) = mpsc::channel();
    let sink = buf.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match f.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => sink.lock().unwrap().extend_from_slice(&chunk[..n]),
            }
        }
        let _ = done.send(());
    });
    (buf, rx)
}

fn run_measured(cmd: &mut Command) -> Ran {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the launcher starts");
    let (out, out_done) = collect(child.stdout.take().expect("a pipe"));
    let (err, err_done) = collect(child.stderr.take().expect("a pipe"));
    let t = Instant::now();
    let status = child.wait().expect("the launcher is reaped");
    let took = t.elapsed();
    // The launcher has exited, so everything it wrote is already in the pipes: this is the drain
    // finishing, not a wait on anything it left behind.
    let grace = Instant::now() + Duration::from_secs(2);
    let mut eof: Option<Duration> = None;
    for rx in [out_done, err_done] {
        let mut closed = false;
        while Instant::now() < grace {
            match rx.try_recv() {
                Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
                    closed = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        eof = match (eof, closed) {
            (Some(at), true) => Some(at.max(t.elapsed())),
            (_, true) => Some(t.elapsed()),
            (_, false) => None,
        };
    }
    let text = |b: &Arc<Mutex<Vec<u8>>>| String::from_utf8_lossy(&b.lock().unwrap()).into_owned();
    Ran {
        status,
        took,
        eof,
        stdout: text(&out),
        stderr: text(&err),
    }
}

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
    let mut cmd = letibot_testenv::command(bin.join("letibot-multi"));
    cmd.args(["rooms", "is-on"])
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("LETIBOT_ROOM_SOCKET", socket);
    let ran = run_measured(&mut cmd);
    (ran.status.success(), ran.took, ran.stderr)
}

/// `bash <script> <args>` over a fixture PATH, home and runtime dir, with nothing inherited from
/// this test's own environment.
#[allow(clippy::too_many_arguments)]
fn launcher(
    script: &Path,
    args: &[&str],
    wt: &Path,
    bin: &Path,
    home: &Path,
    rundir: &Path,
    room: &Path,
    head: Option<&Path>,
) -> Ran {
    let mut cmd = letibot_testenv::command("bash");
    cmd.arg(script)
        .args(args)
        .current_dir(wt)
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", rundir)
        .env("LETIBOT_BIN", bin)
        .env("LETIBOT_ROOM_SOCKET", room);
    if let Some(h) = head {
        cmd.env("LETIBOT_HEAD", h);
    }
    run_measured(&mut cmd)
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
        took < NEVER,
        "is-on never came back on a hung systemctl: {took:?}"
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
    assert!(took < NEVER, "is-on never came back: {took:?}");
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
    let ran = launcher(
        &script,
        &["--new"],
        &wt,
        &bin,
        &home,
        &rundir,
        &room,
        Some(&bin.join("head-stub")),
    );
    let (took, err, stdout) = (ran.took, ran.stderr, ran.stdout);
    assert!(ran.status.success(), "the launcher refused:\n{err}");
    assert!(
        stdout.starts_with("head --socket"),
        "the head was started:\n{stdout}\n{err}"
    );
    assert!(
        took < NEVER,
        "the launcher never came back on a systemd-run that hangs:\n{err}"
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

    // The daemon was handed the probe's answer file, and the probe still answers into it. The
    // stub's own `sleep` is past the launcher's bound, so this waits for the answer's arrival and
    // not for the stub.
    let until = Instant::now() + NEVER;
    let late = loop {
        if let Ok(p) = std::fs::read_to_string(home.join("late-env"))
            && !p.is_empty()
            && Path::new(&p).is_file()
        {
            break p;
        }
        assert!(Instant::now() < until, "the probe never answered:\n{err}");
        std::thread::sleep(Duration::from_millis(20));
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
    /// When the launcher's caller stopped reading: see [`Ran::eof`].
    eof: Option<Duration>,
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
    let ran = launcher(
        &script,
        &["say", "hi"],
        &wt,
        &bin,
        &home,
        &rundir,
        &room,
        None,
    );
    OneShot {
        took: ran.took,
        eof: ran.eof,
        status: ran.status.code(),
        stdout: ran.stdout,
        stderr: ran.stderr,
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
    /// Nothing the abandoned join started is still running either.
    fn no_children_left(&self) -> bool {
        let pids = std::fs::read_to_string(self.home.join("child-pids")).unwrap_or_default();
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
        r.took < NEVER,
        "the one-shot never came back on a scope that never joined:\n{}",
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
fn a_join_wrapper_that_forks_does_not_hold_the_callers_stdio() {
    // A `systemd-run` that is a wrapper: the scope runs in a child and the wrapper waits. The
    // given-up join has to take that child with it, or `x=$(letibot ...)` reads until it ends.
    let r = one_shot(
        "fork",
        "sleep 20 &\necho $! >>\"$HOME/child-pids\"\nwait",
        false,
    );
    assert_eq!(r.stdout.trim(), "ran", "{}", r.stderr);
    assert_eq!(r.status, Some(3), "{}", r.stderr);
    assert!(r.stderr.contains("made no scope"), "{}", r.stderr);
    let until = Instant::now() + NEVER;
    while !(r.no_join_left() && r.no_children_left()) {
        assert!(
            Instant::now() < until,
            "the abandoned join is killed, children and all"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        r.eof.is_some(),
        "the launcher's caller is still reading from a child it left behind"
    );
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
    assert!(r.took < NEVER, "{:?}", r.took);
}

#[test]
fn a_probe_that_never_answers_does_not_hang_the_one_shot() {
    let r = one_shot("never", "exec sleep 20", true);
    assert!(
        r.took < NEVER,
        "the one-shot never came back:\n{}",
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
    // The stub must answer `write_stub`'s probe: it is run once with no arguments to prove it is
    // executable, and a stub that blocks on that costs the test its whole `sleep`.
    letibot_tools::testing::write_stub(
        &bin.join("systemd-run"),
        "#!/bin/sh\n[ -n \"$LETIBOT_STUB_PROBE\" ] && exit 0\n\
         for a; do last=$a; done\n[ \"$last\" = true ] && exit 0\n\
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
    let ran = launcher(
        &script,
        &["--new"],
        &wt,
        &bin,
        &home,
        &rundir,
        &room,
        Some(&bin.join("head-stub")),
    );
    let (took, err, stdout) = (ran.took, ran.stderr, ran.stdout);
    assert!(ran.status.success(), "the launcher refused:\n{err}");
    assert!(stdout.starts_with("head --socket"), "{stdout}\n{err}");
    assert!(
        took < NEVER,
        "the launcher never came back: {took:?}\n{err}"
    );
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
