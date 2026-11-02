//! One ssh master per daemon, a refused session that is transport, and a write that is one call, against
//! a fake ssh that can be told to behave like a busy one.
//!
//! Every `ssh` here ignores its flags and runs the last argument locally with `HOME` in a temporary
//! directory, so the commands, the base64 channel, the far side's size and hash check and the `mv` are
//! the production ones and only the transport is faked. The ledger is files rather than counters, because
//! a counter under a `mkdir` lock lost increments.

use std::path::{Path, PathBuf};
use std::time::Duration;

use letibot_tools::backend::{Command, ExecBackend};
use letibot_tools::exec::Egress;
use letibot_tools::remote::{Host, Mise, Paths, SshBackend, SshSpec};

// ---------------------------------------------------------------------------
// The fake ssh
// ---------------------------------------------------------------------------

/// A fake ssh, with a ledger of the sessions it opened. The body a test passes runs with ssh's command in
/// `$last` and then, unless it exited, that command runs: a body is a behaviour of the transport, not a
/// reimplementation of the remote shell.
struct Fake {
    dir: PathBuf,
    program: PathBuf,
    log: PathBuf,
}

impl Fake {
    fn new(tag: &str, body: &str) -> Fake {
        let dir = std::env::temp_dir().join(format!(
            "letibot-ssh-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let bin = dir.join("bin");
        let home = dir.join("home");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(dir.join("live")).expect("the live directory");
        if !on_path("setsid") {
            write_executable(
                &bin.join("setsid"),
                "#!/usr/bin/env perl\nuse POSIX ();\nPOSIX::setsid();\nexec { $ARGV[0] } @ARGV;\n",
            );
        }
        let program = bin.join("ssh");
        let log = dir.join("ssh.log");
        let d = dir.display().to_string();
        let body = body.replace("@DIR@", &d);
        // `[ $# -eq 0 ] && exit 0` first, because `write_stub` proves the file by exec'ing it with no
        // arguments and a `sleep 300` prelude would be this fixture hanging. `-O check`/`-O exit` are the
        // control-socket verbs and answer `1`, so nothing waits on a socket nothing listens to.
        //
        // `bash -lc "$last"` runs as a CHILD, not an `exec`: a replaced shell runs no `EXIT` trap, so the
        // session's `live` file would never be removed.
        write_executable(
            &program,
            &format!(
                "#!/bin/bash\n\
                 [ $# -eq 0 ] && exit 0\n\
                 case \" $* \" in *\" -O \"*) exit 1;; esac\n\
                 {{ printf 'ARGV %s\\n' \"$*\"; }} >> '{log}'\n\
                 export PATH='{bin}':$PATH\n\
                 export HOME='{home}'\n\
                 for last; do :; done\n\
                 live_now() {{ ls '{d}/live' 2>/dev/null | wc -l | tr -d ' '; }}\n\
                 mark() {{ printf '%s;' \"$1\" >> '{d}/events'; }}\n\
                 first_time() {{ [ -f '{d}'/\"$1\" ] && return 1; : > '{d}'/\"$1\"; return 0; }}\n\
                 : > \"{d}/live/$$\"\n\
                 printf 'start %s %s;' \"$$\" \"$(live_now)\" >> '{d}/events'\n\
                 trap 'rm -f \"{d}/live/$$\"; printf '\\''end %s;'\\'' \"$$\" >> {d}/events' EXIT\n\
                 {body}\n\
                 bash -lc \"$last\"\n\
                 exit $?\n",
                log = log.display(),
                bin = bin.display(),
                home = home.display(),
                body = body
            ),
        );
        Fake { dir, program, log }
    }

    /// Every invocation, one line each: the argv ssh was handed.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("ARGV "))
            .map(str::to_string)
            .collect()
    }

    /// How many invocations carried `needle` anywhere in their argv, which is the count a retry changes.
    /// Counted over the whole log because the fake logs the argv verbatim and a remote command is often
    /// several lines long.
    fn count(&self, needle: &str) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .matches(needle)
            .count()
    }

    /// Every event the ledger recorded, in order: `start <pid> <live>;`, `end <pid>;`, and whatever
    /// a body's `mark` wrote.
    fn events(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("events"))
            .unwrap_or_default()
            .split(';')
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// How many sessions the fake opened, refusals included: a refused session is still one the
    /// client asked for.
    fn sessions(&self) -> usize {
        self.events()
            .iter()
            .filter(|e| e.starts_with("start "))
            .count()
    }

    /// The most sessions open at once, as the fake itself saw them: each `start` records the count
    /// of live files at that instant.
    fn peak(&self) -> usize {
        self.events()
            .iter()
            .filter_map(|e| e.strip_prefix("start "))
            .filter_map(|rest| rest.split_whitespace().nth(1))
            .filter_map(|n| n.parse().ok())
            .max()
            .unwrap_or(0)
    }

    /// How many times a body wrote `mark refused`.
    fn refusals(&self) -> usize {
        self.events().iter().filter(|e| *e == "refused").count()
    }

    /// How many session files were left behind: a session that never reached its trap.
    fn still_live(&self) -> usize {
        std::fs::read_dir(self.dir.join("live"))
            .map(|d| d.count())
            .unwrap_or(0)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A repository for a placement to send, and the ssh material a session on it is reached with.
fn repo_and_paths(dir: &Path) -> (PathBuf, Paths) {
    std::fs::create_dir_all(dir).expect("the rig's directory");
    let paths = Paths::in_home(dir.join("letibot-home"));
    std::fs::create_dir_all(paths.keys()).expect("the keys directory");
    std::fs::write(paths.default_key("fake"), b"a dedicated key").expect("the key");
    std::fs::write(
        paths.known_hosts(),
        "fake.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n",
    )
    .expect("the host key");
    let repo = dir.join("the-repo");
    std::fs::create_dir_all(&repo).expect("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "a test"]);
    std::fs::write(repo.join("README"), b"the first commit\n").expect("a file");
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "the first commit"]);
    (repo, paths)
}

/// The host a placement is made to: no mise, so the probe never runs and no session goes to
/// something the test did not ask for.
fn host_named(name: &str) -> Host {
    Host {
        name: name.to_string(),
        hostname: format!("{name}.invalid"),
        user: Some("tester".to_string()),
        key: None,
        path: Vec::new(),
        root: "~/letibot-remote".into(),
        mise: Some(Mise::Off),
        ..Host::default()
    }
}

/// The spec a test places with, and no boundary (`confine: Some(false)`): the probe is a round trip
/// to the machine behind the fake, which is this box, so the same test would confine on Linux and
/// not on the Mac.
fn spec_for(repo: &Path, paths: &Paths, program: &Path, bound: Duration) -> SshSpec {
    SshSpec {
        host: host_named("fake"),
        name: "s-01abcdef".into(),
        source: repo.to_path_buf(),
        writable: true,
        exec: true,
        ssh: Some(program.to_path_buf()),
        paths: Some(paths.clone()),
        bound: Some(bound),
        egress: Egress::Denied,
        confine: Some(false),
        vm: false,
    }
}

fn write_executable(path: &Path, text: &str) {
    letibot_tools::testing::write_stub(path, text);
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = letibot_tools::testing::git(repo)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A payload with every byte value in it, so a stream that arrives reordered or truncated is
/// visible rather than plausible.
fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}
// ---------------------------------------------------------------------------
// The write
// ---------------------------------------------------------------------------

/// A write whose far side fails never moves a short temp file over the target: the fake breaks `base64
/// -d` there, so the target must be byte-identical and the tool must say the write failed.
///
/// Linux only, because the fake shadows `base64` by putting its `bin` first on `PATH` and a macOS login
/// shell rebuilds `PATH` with the system directories in front.
#[cfg(target_os = "linux")]
#[test]
fn a_far_side_that_cannot_decode_never_touches_the_target() {
    let fake = Fake::new("decode-broken", "");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(
        &repo,
        &paths,
        &fake.program,
        Duration::from_secs(10),
    ))
    .expect("placed");
    // After the placement, and that is the point: the far side's `base64` stops working, so a
    // decode cannot write. Planted first it would break the placement's own bundle.
    write_executable(
        &fake.path("bin").join("base64"),
        "#!/bin/bash\necho 'base64: cannot write' >&2\nexit 1\n",
    );

    // Seeded through the filesystem, not the backend: every remote read goes through the `base64`
    // this test has just broken.
    let target = Path::new(b.worktree_path()).join("kept.txt");
    let before = b"the bytes that were there before\n".to_vec();
    std::fs::write(&target, &before).expect("a file that was there first");

    let big = payload(200 * 1024);
    let err = b
        .write("kept.txt", &big)
        .expect_err("a far side that cannot decode is a failed write");
    let said = format!("{err}");
    assert!(
        said.contains("not touched") || said.contains("refused"),
        "the sentence says the target was not touched: {said}"
    );
    assert_eq!(
        std::fs::read(&target).expect("it is still there"),
        before,
        "the target is byte-identical: nothing short of the whole payload was moved over it"
    );
    // And no debris: the temp file the attempt made is gone with the session that made it.
    let listed = b
        .run(&Command {
            argv: vec!["ls".into(), "-a".into()],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect("the checkout lists");
    // `.letibot-remote/` is the session's own job directory and is not debris; this write's temp
    // file is `kept.txt.letibot-<pid>-<n>`.
    let listing = String::from_utf8_lossy(&listed.stdout).into_owned();
    let leftovers: Vec<&str> = listing
        .lines()
        .filter(|l| l.starts_with("kept.txt.letibot-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no half-written temp file was left behind: {leftovers:?}"
    );
}

/// A write is ONE ssh call, whatever its size: the bytes ride on the stdin of a single session,
/// so the count does not grow with the payload.
#[test]
fn a_write_is_one_session_whatever_its_size() {
    let fake = Fake::new("one-call", "");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(
        &repo,
        &paths,
        &fake.program,
        Duration::from_secs(30),
    ))
    .expect("placed");

    let big = payload(400 * 1024);
    let before = fake.count("base64 -d");
    b.write("big.bin", &big).expect("it lands");
    assert_eq!(
        fake.count("base64 -d") - before,
        1,
        "one session carried 400 KiB: {:#?}",
        fake.calls()
    );
    assert_eq!(b.read("big.bin").expect("it reads back"), big);

    // An empty file is the same path, not a special case: the size the far side counts is zero and
    // the hash of nothing is a hash like any other.
    let before = fake.count("base64 -d");
    b.write("empty.bin", b"").expect("an empty write lands");
    assert_eq!(fake.count("base64 -d") - before, 1);
    assert_eq!(
        b.read("empty.bin").expect("it reads back"),
        Vec::<u8>::new()
    );
}

// ---------------------------------------------------------------------------
// A refused session is transport, and it is retried
// ---------------------------------------------------------------------------

/// A session the multiplexer refused is retried on a fresh connection, and the write lands. Two things
/// have to be true: the write succeeds, and what ssh said is never reported as the command's own exit,
/// because `exit 255` for a session that never existed sends the model looking for a bug in its command.
#[test]
fn a_refused_session_is_retried_on_a_fresh_connection() {
    let fake = Fake::new(
        "refused",
        "if [[ \"$last\" == *'landed.bin'* ]] && first_time refused; then\n\
           mark refused\n\
           echo 'mux_client_request_session: session request failed' >&2\n\
           exit 255\n\
         fi",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(
        &repo,
        &paths,
        &fake.program,
        Duration::from_secs(10),
    ))
    .expect("placed");

    let bytes = payload(8 * 1024);
    let before = fake.count("base64 -d");
    b.write("landed.bin", &bytes)
        .expect("a refused session is retried, and the retry lands");
    assert_eq!(
        fake.count("base64 -d") - before,
        2,
        "two sessions carried the write: the one that was refused and the one that ran"
    );
    assert_eq!(fake.refusals(), 1, "exactly one session was refused");
    assert_eq!(b.read("landed.bin").expect("it reads back"), bytes);
}

/// A `run` whose session was refused runs exactly once: a refused session provably had no channel, and the
/// fake proves it, since the command appends a line to a file and after a refusal and a retry the file
/// holds one line.
#[test]
fn a_run_whose_session_was_refused_runs_exactly_once() {
    let fake = Fake::new(
        "run-refused",
        "if [[ \"$last\" == *'the-model-ran-here'* ]] && first_time refused; then\n\
           mark refused\n\
           echo 'channel 3: open failed: administratively prohibited: open failed' >&2\n\
           exit 255\n\
         fi",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(
        &repo,
        &paths,
        &fake.program,
        Duration::from_secs(10),
    ))
    .expect("placed");

    let ran = fake.path("ran");
    let out = b
        .run(&Command {
            argv: vec![
                "sh".into(),
                "-c".into(),
                format!(
                    "echo the-model-ran-here >> '{}'; echo the-model-ran-here",
                    ran.display()
                ),
            ],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect("a refused session is not the command's failure");
    assert_eq!(out.exit, 0, "the command's own exit, not ssh's");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("the-model-ran-here"),
        "the answer is the command's output: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let lines = std::fs::read_to_string(&ran).unwrap_or_default();
    assert_eq!(
        lines.lines().count(),
        1,
        "the command ran exactly once: {lines:?}"
    );
    assert_eq!(fake.refusals(), 1);
}

// ---------------------------------------------------------------------------
// A stream that is cut
// ---------------------------------------------------------------------------

/// A stream cut mid-write leaves the target byte-identical and the failure is a transport one. Both
/// attempts are cut, so the write must fail; a third write, with the cuts spent, must land whole, so the
/// failure left nothing behind.
#[test]
fn a_cut_stream_leaves_the_target_byte_identical() {
    // Two sessions are cut, one per attempt. A third write is not cut at all.
    let fake = Fake::new(
        "cut",
        "if [[ \"$last\" == *'cut.bin'* ]] && { first_time cut1 || first_time cut2; }; then\n\
           head -c 64 | bash -lc \"$last\"\n\
           exit $?\n\
         fi",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(
        &repo,
        &paths,
        &fake.program,
        Duration::from_secs(10),
    ))
    .expect("placed");

    let target = Path::new(b.worktree_path()).join("cut.bin");
    let before = b"what was there before the cut\n".to_vec();
    std::fs::write(&target, &before).expect("a file that was there first");

    let big = payload(300 * 1024);
    let err = b
        .write("cut.bin", &big)
        .expect_err("a stream cut twice is a failed write");
    let said = format!("{err}");
    assert!(
        said.contains("stream arrived as") || said.contains("not touched"),
        "the far side's own size check is what refused it: {said}"
    );
    assert_eq!(
        std::fs::read(&target).expect("it is still there"),
        before,
        "the target is byte-identical: a short stream was never moved over it"
    );

    // And the next write, with no cut left to take, lands whole.
    b.write("cut.bin", &big)
        .expect("the retry after the cuts lands");
    assert_eq!(std::fs::read(&target).expect("it is there"), big);
}

// ---------------------------------------------------------------------------
// Twelve at once
// ---------------------------------------------------------------------------

/// Twelve threads through one host, with the host enforcing `MaxSessions`-2: every call gets a reply and
/// the host never has to refuse one. The ceiling is eight because a master keeps two of ten for itself.
#[test]
fn twelve_threads_through_one_host_all_get_an_answer() {
    let fake = Fake::new(
        "twelve",
        "if [ \"$(live_now)\" -gt 8 ]; then\n\
           mark refused\n\
           echo 'channel 0: open failed: administratively prohibited: open failed' >&2\n\
           exit 255\n\
         fi",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = std::sync::Arc::new(
        SshBackend::place(&spec_for(
            &repo,
            &paths,
            &fake.program,
            Duration::from_secs(20),
        ))
        .expect("placed"),
    );

    let mut threads = Vec::new();
    for i in 0..12 {
        let b = b.clone();
        threads.push(std::thread::spawn(move || {
            let bytes = payload(32 * 1024 + i);
            b.write(&format!("f-{i}.bin"), &bytes)
                .unwrap_or_else(|e| panic!("thread {i} write: {e}"));
            assert_eq!(
                b.read(&format!("f-{i}.bin")).expect("it reads back"),
                bytes,
                "thread {i} read back what it wrote"
            );
            let out = b
                .run(&Command {
                    // A command that takes a moment, so the twelve really do overlap: a write
                    // and a `true` are over before the next thread starts, and a bound nothing
                    // pushed against proves nothing.
                    argv: vec![
                        "sh".into(),
                        "-c".into(),
                        format!("sleep 0.3; echo thread-{i}"),
                    ],
                    cwd: String::new(),
                    env: Vec::new(),
                })
                .unwrap_or_else(|e| panic!("thread {i} run: {e}"));
            assert_eq!(out.exit, 0);
            assert!(
                String::from_utf8_lossy(&out.stdout).contains(&format!("thread-{i}")),
                "thread {i}'s command answered"
            );
        }));
    }
    for (i, t) in threads.into_iter().enumerate() {
        t.join().unwrap_or_else(|_| panic!("thread {i} panicked"));
    }

    assert_eq!(
        fake.refusals(),
        0,
        "the host never had to refuse a session: {} of them, peaking at {} at once",
        fake.sessions(),
        fake.peak()
    );
    assert!(
        fake.peak() <= 8,
        "at most eight sessions at once, which is MaxSessions minus the two the master keeps: \
         peak was {}",
        fake.peak()
    );
    assert!(
        fake.peak() >= 5,
        "the load was real: twelve threads asked at once and the peak was {}",
        fake.peak()
    );
    assert!(
        fake.sessions() >= 24,
        "the load was real — twelve writes and twelve runs at least: {} sessions",
        fake.sessions()
    );
    assert_eq!(
        fake.still_live(),
        0,
        "every session reached its own trap: none was left holding a place"
    );
}

// ---------------------------------------------------------------------------
// One master per daemon
// ---------------------------------------------------------------------------

/// The socket a daemon's calls multiplex over is its own, and git's is a second one. A socket named only
/// by host is shared by every daemon on the box, so the eleventh session is refused; naming it by the
/// daemon's pid divides nothing, and git's is separate because a `git fetch` cannot be counted here.
#[test]
fn one_daemon_has_one_master_and_git_has_another() {
    let mine = letibot_tools::remote::control_path("lab2x1", 4242);
    assert_eq!(
        mine,
        letibot_tools::remote::control_path("lab2x1", 4242),
        "one daemon, one socket"
    );
    assert_ne!(
        letibot_tools::remote::control_path("lab2x1", 4243),
        mine,
        "another daemon's calls do not ride on this one's connection"
    );
    assert_ne!(
        letibot_tools::remote::git_control_path("lab2x1", 4242),
        mine,
        "git's ssh has a socket of its own, so a fetch cannot be the ninth session"
    );
    assert!(
        mine.as_os_str().len() <= 100,
        "under the socket path cap: {}",
        mine.display()
    );
}
