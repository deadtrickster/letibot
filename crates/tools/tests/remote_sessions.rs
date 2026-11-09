//! One ssh master per daemon, a refused session that is transport, and a write that is one call,
//! against a fake ssh that can act busy. Every `ssh` runs the last argument locally; only transport
//! is faked. The ledger is files, as a counter under a `mkdir` lock lost increments.

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
    /// The machine this fake is, unique in this process: the gate and the breaker are keyed by a
    /// host's name, and a test that holds either has to be able to say whose it is.
    name: String,
}

/// A name no other fixture in this process holds - the same rule `remote_hosts` states.
fn fixture_name(tag: &str) -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!(
        "fake-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

impl Fake {
    fn new(tag: &str, body: &str) -> Fake {
        let name = fixture_name(tag);
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
        // arguments. `bash -lc "$last"` runs as a CHILD and not an `exec`: a replaced shell runs no `EXIT`
        // trap, so the session's `live` file would never go.
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
                 socket() {{\n\
                   local rest=\"$*\"\n\
                   case \"$rest\" in\n\
                     *ControlPath=*) rest=\"${{rest#*ControlPath=}}\"; printf '%s' \"${{rest%% *}}\" ;;\n\
                     *) printf '(none)' ;;\n\
                   esac\n\
                 }}\n\
                 key=$(socket \"$@\" | tr '/ .' '___')\n\
                 open_on() {{ ls \"{d}/opens/$key\" 2>/dev/null | wc -l | tr -d ' '; }}\n\
                 mkdir -p \"{d}/opens/$key\"\n\
                 live_now() {{ ls '{d}/live' 2>/dev/null | wc -l | tr -d ' '; }}\n\
                 mark() {{ printf '%s;' \"$1\" >> '{d}/events'; }}\n\
                 first_time() {{ [ -f '{d}'/\"$1\" ] && return 1; : > '{d}'/\"$1\"; return 0; }}\n\
                 : > \"{d}/live/$$\"\n\
                 : > \"{d}/opens/$key/$$\"\n\
                 printf 'start %s %s %s %s;' \"$$\" \"$(live_now)\" \"$key\" \"$(open_on)\" >> '{d}/events'\n\
                 trap 'rm -f \"{d}/live/$$\"; rm -f \"{d}/pack/$$\"; rm -f \"{d}/opens/$key/$$\"; printf '\\''end %s;'\\'' \"$$\" >> {d}/events' EXIT\n\
                 {body}\n\
                 bash -lc \"$last\"\n\
                 exit $?\n",
                log = log.display(),
                bin = bin.display(),
                home = home.display(),
                body = body
            ),
        );
        Fake {
            dir,
            program,
            log,
            name,
        }
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

    /// Every session the fake opened, as the connection it multiplexed over and how many were open
    /// on that one connection at that instant: `MaxSessions` counts a connection, not a host.
    fn starts(&self) -> Vec<(String, usize)> {
        self.events()
            .iter()
            .filter_map(|e| e.strip_prefix("start "))
            .filter_map(|rest| {
                let mut field = rest.split_whitespace();
                field.next()?;
                field.next()?;
                let connection = field.next()?.to_string();
                let open = field.next()?.parse().ok()?;
                Some((connection, open))
            })
            .collect()
    }

    /// The most sessions any ONE connection was asked to carry at once - the number sshd's
    /// `MaxSessions` compares against, which is not [`Self::peak`].
    fn peak_per_connection(&self) -> usize {
        self.starts()
            .iter()
            .map(|(_, open)| *open)
            .max()
            .unwrap_or(0)
    }

    /// The connections the fake was asked to multiplex over, in the order it first saw each.
    fn connections(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for (connection, _) in self.starts() {
            if !seen.contains(&connection) {
                seen.push(connection);
            }
        }
        seen
    }

    /// The most `git-upload-pack` sessions a body saw live at once, counting the one asking.
    fn peak_packs(&self) -> usize {
        self.events()
            .iter()
            .filter_map(|e| e.strip_prefix("pack="))
            .filter_map(|n| n.parse().ok())
            .max()
            .unwrap_or(0)
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
    letibot_tools::testing::repo_config(&repo);
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
fn spec_for(fake: &Fake, repo: &Path, paths: &Paths, bound: Duration) -> SshSpec {
    // The identity file and the host key `Ssh::connect` refuses a connection without, for the name
    // this fixture is, since no other fixture places under this one.
    let host = host_named(&fake.name);
    std::fs::create_dir_all(paths.keys()).expect("the keys directory");
    std::fs::write(paths.default_key(&host.name), b"a dedicated key").expect("the key");
    let mut known = std::fs::read_to_string(paths.known_hosts()).unwrap_or_default();
    known.push_str(&format!(
        "{} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n",
        host.hostname
    ));
    std::fs::write(paths.known_hosts(), known).expect("the host key");
    SshSpec {
        host,
        name: "s-01abcdef".into(),
        source: repo.to_path_buf(),
        writable: true,
        exec: true,
        ssh: Some(fake.program.clone()),
        paths: Some(paths.clone()),
        bound: Some(bound),
        place_bound: None,
        egress: Egress::Denied,
        confine: Some(false),
        vm: false,
        env: Vec::new(),
        env_secrets: Vec::new(),
        grants: Vec::new(),
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

/// A write whose far side fails never moves a short temp file over the target: the fake breaks `base64 -d`
/// there, so the target must be byte-identical and the tool must say the write failed. Linux only, because
/// a macOS login shell rebuilds `PATH` with the system directories in front.
#[cfg(target_os = "linux")]
#[test]
fn a_far_side_that_cannot_decode_never_touches_the_target() {
    let fake = Fake::new("decode-broken", "");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(10)))
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
    // A job's files are outside the checkout entirely; this write's temp file is
    // `kept.txt.letibot-<pid>-<n>`.
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
    let b = SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(30)))
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
    let b = SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(10)))
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
    let b = SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(10)))
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
    let b = SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(10)))
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
        SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(20)))
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

// ---------------------------------------------------------------------------
// The git master's own sessions
// ---------------------------------------------------------------------------

/// Twelve sessions on one host close at once, and each brings its branch back with a `git fetch` over the
/// git master. That ssh is git's own, so the call gate cannot count it: twelve at once is the eleventh
/// session sshd refuses, and a branch that did not come back is a subagent's work left behind.
#[test]
fn a_fanout_of_fetches_never_asks_one_git_master_for_the_eleventh() {
    let fake = Fake::new(
        "git-master",
        "if [[ $last == *git-upload-pack* ]]; then\n\
           mkdir -p '@DIR@/pack'\n\
           : > \"@DIR@/pack/$$\"\n\
           n=$(ls '@DIR@/pack' 2>/dev/null | wc -l | tr -d ' ')\n\
           mark pack=$n\n\
           if [ $n -gt 10 ]; then\n\
             mark refused\n\
             rm -f \"@DIR@/pack/$$\"\n\
             echo 'channel 11: open failed: administratively prohibited: open failed' >&2\n\
             exit 255\n\
           fi\n\
           sleep 0.3\n\
         fi",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let mut backends = Vec::new();
    for i in 0..12 {
        let mut spec = spec_for(&fake, &repo, &paths, Duration::from_secs(120));
        spec.name = format!("s-{i:02}abcdef");
        backends.push(SshBackend::place(&spec).unwrap_or_else(|e| panic!("placement {i}: {e}")));
    }
    let backends = std::sync::Arc::new(backends);
    let mut threads = Vec::new();
    for i in 0..12 {
        let b = backends.clone();
        threads.push(std::thread::spawn(move || b[i].close()));
    }
    let mut said = Vec::new();
    for (i, t) in threads.into_iter().enumerate() {
        said.push(t.join().unwrap_or_else(|_| panic!("thread {i} panicked")));
    }
    for (i, s) in said.iter().enumerate() {
        let Some(s) = s else {
            panic!("session {i}'s close said nothing");
        };
        assert!(
            s.contains("came back here"),
            "session {i}'s branch came back: {s}"
        );
    }
    assert_eq!(
        fake.refusals(),
        0,
        "the git master never had to refuse a session: twelve closes asked for twelve fetches, \
         and the most it was asked for at once was {}",
        fake.peak_packs()
    );
    assert!(
        fake.peak_packs() <= 8,
        "at most eight fetches at once, which is MaxSessions minus the two the master keeps: \
         peak was {}",
        fake.peak_packs()
    );
    assert!(
        fake.peak_packs() >= 3,
        "the load was real: twelve closes overlapped, and the peak was {}",
        fake.peak_packs()
    );
}

/// A fetch whose session the multiplexer refused never reached the remote, and a fetch is idempotent:
/// retried on a fresh connection, a full master costs a round trip and not a session's branch.
#[test]
fn a_refused_fetch_is_retried_and_the_branch_still_comes_back() {
    let fake = Fake::new(
        "fetch-refused",
        "if [[ $last == *git-upload-pack* ]] && first_time refused; then\n\
           mark refused\n\
           echo 'mux_client_request_session: session request failed' >&2\n\
           exit 255\n\
         fi",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(30)))
        .expect("placed");

    let said = b.close().expect("a close says where the work went");
    assert_eq!(fake.refusals(), 1, "exactly one fetch was refused");
    assert!(
        said.contains("came back here"),
        "a refused session is transport, so the fetch was retried and the branch came back: {said}"
    );
    git(&repo, &["rev-parse", "--verify", b.back_branch()]);
}

/// A fetch waits for a git slot for as long as the CONNECTION was given, not for the transfer's five
/// minutes: the gate's contract is never past the caller's own deadline, and a close that waits it
/// out is the hang coming back through the door marked back-pressure.
#[test]
fn a_fetch_waits_for_a_git_slot_as_long_as_the_connection_is_given() {
    let fake = Fake::new("slot-bound", "");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b =
        SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(5))).expect("placed");
    // Every slot of this process's git gate, held: the fetch cannot have one until these drop.
    let _held: Vec<_> = (0..letibot_tools::rpc::MAX_SESSIONS_PER_HOST)
        .map(|_| letibot_tools::rpc::enter_git(&fake.name, Duration::from_secs(5)))
        .map(|slot| slot.expect("a git slot"))
        .collect();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(b.close());
    });
    // Fifteen seconds, not five minutes: the connection is given five (a loaded box needs that to
    // place) and the slot wait has to respect it. A close that waits out the transfer's bound fails here instead of hanging.
    let said = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("the close came back inside a few seconds")
        .expect("a close says where the work went");
    assert!(
        said.contains("without getting one"),
        "the report says it never got a slot: {said}"
    );
}

// ---------------------------------------------------------------------------
// A refused fetch and the socket it dials
// ---------------------------------------------------------------------------

/// A fake that refuses the first `git-upload-pack` session only, and answers everything else: the
/// transport a retry is for.
fn refuses_the_first_pack(tag: &str) -> Fake {
    Fake::new(
        tag,
        "if [[ $last == *git-upload-pack* ]] && first_time refused; then\n\
           mark refused\n\
           echo 'mux_client_request_session: session request failed' >&2\n\
           exit 255\n\
         fi",
    )
}

/// The `ControlPath=` in one invocation's argv: the connection that session multiplexed over.
fn control_path_in(line: &str) -> Option<String> {
    let at = line.find("ControlPath=")? + "ControlPath=".len();
    let rest = &line[at..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// The connection each session whose argv names `needle` multiplexed over, one entry per session.
fn control_paths(fake: &Fake, needle: &str) -> Vec<String> {
    fake.calls()
        .iter()
        .filter(|line| line.contains(needle))
        .filter_map(|line| control_path_in(line))
        .collect()
}

/// The same names, once each, in a stable order.
fn distinct(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.dedup();
    names
}

/// The `ControlPath=` each fetch's ssh was handed, in the order the fake saw them.
fn fetch_sockets(fake: &Fake) -> Vec<String> {
    control_paths(fake, "git-upload-pack")
}

/// The git socket belongs to the DAEMON: every session on that host multiplexes over it, so a fetch
/// that was refused must not remove it. The name is planted where ssh would have left it, and a
/// refused fetch that retried must leave it where it was.
#[test]
fn a_refused_fetch_leaves_the_shared_git_socket_alone() {
    let fake = refuses_the_first_pack("git-socket-kept");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(30)))
        .expect("placed");
    // A plain file, because the fake is no ssh: this test is about who removes the name, and the
    // name is the one the daemon's own fetches multiplex over.
    let socket = letibot_tools::remote::git_control_path(&fake.name, std::process::id());
    std::fs::write(&socket, b"the socket every session of this daemon shares").expect("planted");

    let said = b.close().expect("a close says where the work went");
    assert_eq!(fake.refusals(), 1, "exactly one fetch was refused");
    assert!(
        socket.exists(),
        "the daemon's git socket is still there, not removed by one fetch: {}",
        socket.display()
    );
    assert!(
        said.contains("came back here"),
        "and the refused fetch was retried, so the branch came back: {said}"
    );
    git(&repo, &["rev-parse", "--verify", b.back_branch()]);
}

/// A refusal says the master behind that socket may be the thing that is full, so the retry dials a
/// socket of its own. Two attempts over one socket would ask the same full master again, which is
/// what the retry exists not to do.
#[test]
fn a_refused_fetch_retries_over_a_socket_of_its_own() {
    let fake = refuses_the_first_pack("git-socket-retry");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(30)))
        .expect("placed");

    let said = b.close().expect("a close says where the work went");
    assert_eq!(fake.refusals(), 1, "exactly one fetch was refused");
    let sockets = fetch_sockets(&fake);
    assert_eq!(
        sockets.len(),
        2,
        "two attempts, one ssh each: {sockets:?} from {:#?}",
        fake.calls()
    );
    assert_ne!(
        sockets[0], sockets[1],
        "the retry dialled a socket of its own: {sockets:?}"
    );
    assert!(
        said.contains("came back here"),
        "and the branch came back over it: {said}"
    );
    git(&repo, &["rev-parse", "--verify", b.back_branch()]);
}

// ---------------------------------------------------------------------------
// Six sessions on one host at once: the entry's own proof
// ---------------------------------------------------------------------------

/// `sshd` with its default `MaxSessions`: the eleventh session on ONE connection is refused, in the
/// words the real server's channel refusal prints. Counting per connection and not per host is the
/// whole of it - ten sessions of one daemon are fine, eleven is the session it drops.
const SSHD_REFUSES_THE_ELEVENTH: &str = "sleep 0.05\n\
 if [ \"$(open_on)\" -gt 10 ]; then\n\
   mark refused\n\
   echo 'channel 0: open failed: administratively prohibited: open failed' >&2\n\
   exit 255\n\
 fi";

/// Six sessions placed on one machine at once all place, work and come back, and the host never
/// refuses one - because every one of them multiplexed over ONE connection, which is the thing that
/// needs a bound. The fake counts sessions per connection, as `sshd` does.
#[test]
fn six_sessions_at_once_on_one_host_all_place_over_one_connection() {
    let fake = Fake::new("six-at-once", SSHD_REFUSES_THE_ELEVENTH);
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));

    // Six placements at once, the way six packages start together. Every spec is built before any
    // thread runs: `spec_for` rewrites the known-hosts file, and a placement that read it mid-write
    // would be refused a host key that is there.
    let specs: Vec<SshSpec> = (0..6)
        .map(|i| {
            let mut spec = spec_for(&fake, &repo, &paths, Duration::from_secs(120));
            spec.name = format!("s-{i:02}abcdef");
            spec
        })
        .collect();
    let mut placers = Vec::new();
    for (i, spec) in specs.into_iter().enumerate() {
        placers.push(std::thread::spawn(move || {
            SshBackend::place(&spec).unwrap_or_else(|e| panic!("session {i} was not placed: {e}"))
        }));
    }
    let placed: Vec<SshBackend> = placers
        .into_iter()
        .enumerate()
        .map(|(i, t)| {
            t.join()
                .unwrap_or_else(|_| panic!("placement {i} panicked"))
        })
        .collect();
    let placed = std::sync::Arc::new(placed);

    // Then all six work at once, each with TWO things in flight - a write and a command - which is
    // six packages' writes and polls together: TWELVE sessions on one connection, more than the
    // server allows, so the bound is the only thing that keeps the host from refusing one.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(13));
    let mut workers = Vec::new();
    for k in 0..12 {
        let b = placed.clone();
        let barrier = barrier.clone();
        let i = k % 6;
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            if k < 6 {
                let bytes = payload(256 * 1024 + i);
                b[i].write(&format!("at-once-{i}.bin"), &bytes)
                    .unwrap_or_else(|e| panic!("session {i} write: {e}"));
                assert_eq!(
                    b[i].read(&format!("at-once-{i}.bin"))
                        .expect("it reads back"),
                    bytes,
                    "session {i} read back what it wrote"
                );
                return;
            }
            let out = b[i]
                .run(&Command {
                    argv: vec![
                        "sh".into(),
                        "-c".into(),
                        format!("sleep 0.3; echo session-{i}"),
                    ],
                    cwd: String::new(),
                    env: Vec::new(),
                })
                .unwrap_or_else(|e| panic!("session {i} run: {e}"));
            assert_eq!(out.exit, 0, "session {i}'s command answered");
            assert!(
                String::from_utf8_lossy(&out.stdout).contains(&format!("session-{i}")),
                "session {i}'s own output came back"
            );
        }));
    }
    barrier.wait();
    for (k, t) in workers.into_iter().enumerate() {
        t.join().unwrap_or_else(|_| panic!("worker {k} panicked"));
    }

    // And all six close at once, which is the fetch each brings its branch back with.
    let mut closers = Vec::new();
    for i in 0..6 {
        let b = placed.clone();
        closers.push(std::thread::spawn(move || b[i].close()));
    }
    for (i, t) in closers.into_iter().enumerate() {
        let said = t
            .join()
            .unwrap_or_else(|_| panic!("close {i} panicked"))
            .unwrap_or_else(|| panic!("session {i}'s close said nothing"));
        assert!(
            said.contains("came back here"),
            "session {i}'s branch came back: {said}"
        );
    }

    let bound = letibot_tools::rpc::MAX_SESSIONS_PER_HOST;
    assert_eq!(
        fake.refusals(),
        0,
        "six sessions on one host at once and the host never had to refuse one: {} sessions, \
         the most on any ONE connection {}",
        fake.sessions(),
        fake.peak_per_connection()
    );
    assert!(
        fake.peak_per_connection() <= bound,
        "at most {bound} sessions on one connection, the bound and two below the server's \
         MaxSessions of 10: the peak was {} over {:?}",
        fake.peak_per_connection(),
        fake.connections()
    );
    assert!(
        fake.peak_per_connection() >= 6,
        "the load was real: twelve sessions asked at once and the peak on one connection was {}",
        fake.peak_per_connection()
    );

    // The master is SHARED, which is the reason a bound is needed at all: one connection carried
    // every call of all six sessions, and git's fetches a second one of their own.
    let calls = distinct(
        fake.calls()
            .iter()
            .filter(|line| !line.contains("git-upload-pack"))
            .filter_map(|line| control_path_in(line))
            .collect(),
    );
    let fetches = distinct(fetch_sockets(&fake));
    assert_eq!(
        calls.len(),
        1,
        "every call of all six sessions multiplexed over ONE connection: {calls:?}"
    );
    assert_eq!(
        fetches.len(),
        1,
        "and every fetch over one git connection: {fetches:?}"
    );
    assert_ne!(
        calls[0], fetches[0],
        "git's connection is its own, so a fetch is never the ninth session of the call one"
    );
    assert_eq!(
        fake.still_live(),
        0,
        "every session reached its own trap: none was left holding a place"
    );
}

/// A host whose own `MaxSessions` is lower than the bound refuses the second session on a
/// connection. The refusal must be reported by name - never as the command's exit, never as
/// "it may be running there".
#[test]
fn a_cap_below_the_bound_is_reported_by_name() {
    let fake = Fake::new(
        "low-cap",
        "if [[ \"$last\" == *'the-held-open-command'* ]]; then\n\
           if [ \"$(open_on)\" -gt 1 ]; then\n\
             mark refused\n\
             echo 'channel 0: open failed: administratively prohibited: open failed' >&2\n\
             exit 255\n\
           fi\n\
           sleep 0.4\n\
         fi",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = std::sync::Arc::new(
        SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(20)))
            .expect("placed"),
    );

    // Six at once on a connection the server lets one session onto. The barrier makes them ask at
    // one instant, so the five losers are the second session while the winner is still inside.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(7));
    let mut threads = Vec::new();
    for i in 0..6 {
        let b = b.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            b.run(&Command {
                argv: vec!["sh".into(), "-c".into(), "the-held-open-command".into()],
                cwd: String::new(),
                env: Vec::new(),
            })
            .map(|_| i)
            .map_err(|e| format!("{e}"))
        }));
    }
    barrier.wait();
    let said: Vec<String> = threads
        .into_iter()
        .filter_map(|t| {
            t.join()
                .unwrap_or_else(|_| panic!("a thread panicked"))
                .err()
        })
        .collect();

    assert!(
        !said.is_empty(),
        "at least one of six lost the session: the server lets one on and {} refusals were seen",
        fake.refusals()
    );
    for s in &said {
        assert!(
            s.contains("ssh's multiplexer lost the session"),
            "the sentence says the session was lost, not the command: {s}"
        );
        assert!(
            s.contains("the server refused the channel (MaxSessions)"),
            "and names the cap it was refused by: {s}"
        );
        assert!(
            s.contains("did NOT run"),
            "a session that never opened cannot be *running there*: {s}"
        );
        assert!(
            !s.contains("may have run"),
            "nothing reached the far side, so nothing may have: {s}"
        );
    }
}

/// The entry's own *Done when*, on a real machine: six sessions placed on one host at once all place, work and
/// come back, with no call refused, dropped or timed out. It needs a real sshd, so it runs only when asked, with
/// `LETIBOT_R99_HOST` naming a host from `hosts.toml`:
///
/// ```sh
/// LETIBOT_R99_HOST=lab2x1 cargo test --offline -p letibot-tools --test remote_sessions \
///   -- --ignored six_packages_on_a_real_host_at_once_are_never_refused
/// ```
///
/// `LETIBOT_R99_N` is the number of sessions (six, what the entry measured) and `LETIBOT_R99_SOURCE`
/// the repository they are placed from, this tree by default. Each close leaves its branch here as
/// `remote/<host>/<short id>`, exactly as a real placement does.
#[ignore = "live: places six sessions on a real host - LETIBOT_R99_HOST=<name> and --ignored"]
#[test]
fn six_packages_on_a_real_host_at_once_are_never_refused() {
    let name = std::env::var("LETIBOT_R99_HOST")
        .expect("LETIBOT_R99_HOST: a host name from the operator's hosts.toml");
    let n: usize = match std::env::var("LETIBOT_R99_N") {
        Ok(v) => v.parse().expect("LETIBOT_R99_N: a count of sessions"),
        Err(_) => 6,
    };
    let source = std::env::var_os("LETIBOT_R99_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let hosts = letibot_tools::remote::Hosts::load();
    let host = hosts
        .get(&name)
        .unwrap_or_else(|| {
            panic!(
                "`{name}` is not a host in {}",
                letibot_tools::remote::hosts_file().display()
            )
        })
        .clone();
    let repo = PathBuf::from(git(&source, &["rev-parse", "--show-toplevel"]).trim());
    let paths = Paths::from_env();

    // One connection per host per process, so these are one fact and not one per session: that is
    // what makes `MaxSessions` a bound on a daemon rather than on a session.
    let pid = std::process::id();
    let calls = letibot_tools::remote::control_path(&name, pid);
    let fetches = letibot_tools::remote::git_control_path(&name, pid);
    assert_ne!(calls, fetches, "git rides a connection of its own");

    let mut placers = Vec::new();
    for i in 0..n {
        let spec = SshSpec {
            host: host.clone(),
            name: format!("s-r99-{i}"),
            source: repo.clone(),
            writable: true,
            exec: true,
            ssh: None,
            paths: Some(paths.clone()),
            bound: Some(Duration::from_secs(120)),
            place_bound: None,
            egress: Egress::Denied,
            // Production's own answer: ask the host whether it can confine, and use what it says.
            confine: None,
            vm: false,
            env: Vec::new(),
            env_secrets: Vec::new(),
            grants: Vec::new(),
        };
        placers.push(std::thread::spawn(move || {
            SshBackend::place(&spec)
                .map_err(|e| format!("{e}"))
                .map(|b| (i, b))
        }));
    }
    let mut placed = Vec::new();
    for t in placers {
        let (i, b) = t
            .join()
            .unwrap_or_else(|_| panic!("placement panicked"))
            .unwrap_or_else(|e| panic!("{n} sessions at once and one was not placed: {e}"));
        placed.push((i, b));
    }
    let placed = std::sync::Arc::new(placed);

    // A write big enough to be a bundle's shape, read back; a command that overlaps its neighbours;
    // then every branch fetched home at once, which is the git connection's own load.
    let mut workers = Vec::new();
    for slot in 0..n {
        let placed = placed.clone();
        workers.push(std::thread::spawn(move || {
            let (i, b) = &placed[slot];
            let bytes = payload(2 * 1024 * 1024 + *i);
            b.write(&format!("at-once-{i}.bin"), &bytes)
                .map_err(|e| format!("{e}"))?;
            let back = b
                .read(&format!("at-once-{i}.bin"))
                .map_err(|e| format!("{e}"))?;
            if back != bytes {
                return Err(format!("session {i} read back {} bytes", back.len()));
            }
            b.run(&Command {
                argv: vec!["sh".into(), "-c".into(), format!("sleep 0.5; echo {i}")],
                cwd: String::new(),
                env: Vec::new(),
            })
            .map_err(|e| format!("{e}"))?;
            Ok(())
        }));
    }
    for (slot, t) in workers.into_iter().enumerate() {
        t.join()
            .unwrap_or_else(|_| panic!("session {slot} panicked"))
            .unwrap_or_else(|e| panic!("session {slot}: {e}"));
    }

    let mut closers = Vec::new();
    for slot in 0..n {
        let placed = placed.clone();
        closers.push(std::thread::spawn(move || placed[slot].1.close()));
    }
    for t in closers {
        let said = t
            .join()
            .unwrap_or_else(|_| panic!("a close panicked"))
            .unwrap_or_else(|| panic!("a close said nothing"));
        assert!(
            said.contains("came back here"),
            "a branch did not come back: {said}"
        );
    }

    // The one oracle for *was a session refused*: this process records a call that did not answer,
    // and a refusal that was retried still leaves its record. A heartbeat that answered is not one.
    let failed: Vec<String> = letibot_tools::rpc::recent(&name, letibot_tools::rpc::RECORD_RING)
        .into_iter()
        .filter(|r| r.outcome != letibot_tools::rpc::Outcome::Ok)
        .map(|r| format!("{} {} {} ms", r.step, r.outcome.as_str(), r.millis))
        .collect();
    assert!(
        failed.is_empty(),
        "{n} sessions on {name} at once and not one call of theirs was refused, dropped or timed \
         out: {failed:#?}"
    );
    assert!(
        matches!(
            letibot_tools::rpc::standing(&name),
            letibot_tools::rpc::Standing::Closed
        ),
        "not one connection failure in a row, so the breaker never opened"
    );
    println!(
        "{n} sessions placed, worked and came back on {name} at once, with no refused session; \
         their calls shared {} and their fetches {}",
        calls.display(),
        fetches.display()
    );
}
