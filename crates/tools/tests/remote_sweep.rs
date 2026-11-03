//! R115: what a placed session STARTED on its host is ended when the session stops.
//!
//! Shells from a stopped package were still running on lab2x1 two hours later, in placement
//! directories that had already been removed, because nothing on a host belongs to a cgroup:
//! `letibot --stop` ends the daemon's cgroups, and a host's processes are the host's. The handle is
//! the mark every command of a session carries in its environment - so it is on everything the
//! command forked, at any depth - and the sweep is the one act that reads it.
//!
//! Every `ssh` here runs the last argument locally, so the commands, the job launcher, the sweep
//! script and the `kill` are the production ones and only the transport is faked.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_tools::backend::{Command, ExecBackend};
use letibot_tools::exec::{Egress, ScopeKind, SpawnRequest};
use letibot_tools::remote::{Host, Mise, Paths, SESSION_MARK, Ssh, SshBackend, SshSpec};

/// A fake ssh: it ignores its flags and runs the last argument with `HOME` in a temporary
/// directory, so `~` expands on the far side as it does over a real connection.
struct Fake {
    dir: PathBuf,
    program: PathBuf,
}

impl Fake {
    fn new(tag: &str) -> Fake {
        let dir = std::env::temp_dir().join(format!(
            "letibot-sweep-{tag}-{}-{}",
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
        // A job on the fake remote must lead its own process group, as one on a Linux host does.
        if !on_path("setsid") {
            write_executable(
                &bin.join("setsid"),
                "#!/usr/bin/env perl\nuse POSIX ();\nPOSIX::setsid();\nexec { $ARGV[0] } @ARGV;\n",
            );
        }
        let program = bin.join("ssh");
        // `[ $# -eq 0 ] && exit 0` first: `write_stub` proves the file by exec'ing it with no
        // arguments, and that run has to be harmless.
        write_executable(
            &program,
            &format!(
                "#!/bin/bash\n\
                 [ $# -eq 0 ] && exit 0\n\
                 case \" $* \" in *\" -O \"*) exit 1;; esac\n\
                 export PATH='{bin}':$PATH\n\
                 export HOME='{home}'\n\
                 for last; do :; done\n\
                 bash -lc \"$last\"\n\
                 exit $?\n",
                bin = bin.display(),
                home = home.display()
            ),
        );
        Fake { dir, program }
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

/// The host a placement is made to: no mise, so the probe never runs and no call goes through a
/// toolchain the test did not ask for, and no boundary, because the probe would answer differently
/// on Linux and on a Mac.
fn spec_for(repo: &Path, paths: &Paths, program: &Path) -> SshSpec {
    SshSpec {
        host: Host {
            name: "fake".into(),
            hostname: "fake.invalid".into(),
            user: Some("tester".into()),
            key: None,
            path: Vec::new(),
            root: "~/letibot-remote".into(),
            mise: Some(Mise::Off),
            ..Host::default()
        },
        name: "s-01abcdef".into(),
        source: repo.to_path_buf(),
        writable: true,
        exec: true,
        ssh: Some(program.to_path_buf()),
        paths: Some(paths.clone()),
        bound: Some(Duration::from_secs(10)),
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

/// Is that pid still there? `kill -0` asks the kernel, not this test's opinion.
fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn read_pid(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .parse()
        .expect("the command wrote its child's pid")
}

/// A neighbour's process: another session on the same host, in the same checkout, with a mark of
/// its own. The sweep must not touch it - a sweep keyed on the host or on the directory would.
struct Neighbour(std::process::Child);

impl Neighbour {
    fn start(worktree: &Path, mark: &str) -> Neighbour {
        Neighbour(
            std::process::Command::new("sleep")
                .arg("300")
                .env(SESSION_MARK, mark)
                .current_dir(worktree)
                .spawn()
                .expect("the neighbour starts"),
        )
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Neighbour {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Both doors that end a session: its own close, and the daemon's last act.
///
/// One test and not two, because the last act's list is per PROCESS: two tests in one binary run
/// in parallel threads of that process, so a second test's placement would be swept by the first
/// one's `placements_down` - a test of the sweeper that races itself.
#[test]
fn a_stopped_session_leaves_no_process_of_its_own_running_on_the_host() {
    // --- the session's own close -------------------------------------------------------------
    let fake = Fake::new("close");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(&repo, &paths, &fake.program))
        .expect("the placement is arranged");
    let worktree = PathBuf::from(b.worktree_path());

    // The two shapes a package leaves work in: a background job of the session's, and a child a
    // foreground call forked that outlived that call. Both are in the placement's own directory,
    // which is what the removed-directory case in the report looked like.
    let host = b.processes().expect("a placed session can start processes");
    let job = host
        .spawn(&SpawnRequest {
            command: "sleep 300".into(),
            slug: Some("a-package-job".into()),
            cwd: String::new(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: Vec::new(),
            tty: false,
            memory: None,
        })
        .expect("the job starts");
    let job_pid = host.job(&job).expect("the job is known").pid;
    b.run(&Command {
        argv: vec![
            "sh".into(),
            "-c".into(),
            // Its output goes nowhere, or the shell keeps ssh's stdout open for as long as the child
            // lives and this call waits out the child's whole sleep.
            "sleep 300 >/dev/null 2>&1 & echo $! > forked.pid".into(),
        ],
        cwd: String::new(),
        env: Vec::new(),
    })
    .expect("the call runs");
    let forked = read_pid(&worktree.join("forked.pid"));
    let neighbour = Neighbour::start(&worktree, "s-99999999");
    assert!(
        alive(job_pid) && alive(forked) && alive(neighbour.pid()),
        "the fixture's processes are running before the stop: job {job_pid}, forked {forked}, \
         neighbour {}",
        neighbour.pid()
    );

    let said = b.close().expect("a close says what became of the session");
    // The sweep TERMs, waits and then KILLs, so a process that has just gone can still read as alive
    // for the instant the kernel needs to reap it: ask, but ask for a moment.
    let gone = |pid: u32| {
        let began = Instant::now();
        while began.elapsed() < Duration::from_secs(5) {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    };
    assert!(
        gone(job_pid),
        "the package's background job is gone: {job_pid} — {said}"
    );
    assert!(
        gone(forked),
        "the child a foreground call forked is gone too: {forked} — {said}"
    );
    assert!(
        said.contains("process(es) this session started"),
        "and the close says what it ended: {said}"
    );
    assert!(
        alive(neighbour.pid()),
        "another session's process in the same checkout is untouched: {}",
        neighbour.pid()
    );

    // --- the daemon's last act ---------------------------------------------------------------
    // `letibot --stop` closes no backend: a session served on its own thread holds its backend
    // there, and the stop path never reaches `close_backend`. This is the door it does reach.
    let stopping = Fake::new("stop");
    let (repo, paths) = repo_and_paths(&stopping.dir.join("rig"));
    let stop = SshBackend::place(&spec_for(&repo, &paths, &stopping.program))
        .expect("the second placement is arranged");
    let elsewhere = PathBuf::from(stop.worktree_path());
    let job = stop
        .processes()
        .expect("a placed session can start processes")
        .spawn(&SpawnRequest {
            command: "sleep 300".into(),
            slug: None,
            cwd: String::new(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: Vec::new(),
            tty: false,
            memory: None,
        })
        .expect("the job starts");
    let pid = stop
        .processes()
        .expect("a placed session can start processes")
        .job(&job)
        .expect("the job is known")
        .pid;
    let kept = Neighbour::start(&elsewhere, "s-88888888");
    assert!(alive(pid) && alive(kept.pid()), "the fixture is running");

    let ended = letibot_tools::remote::placements_down();
    assert!(gone(pid), "the stop ended the job the session left: {pid}");
    assert!(
        ended
            .iter()
            .any(|said| said.contains("process(es) this session started")),
        "and the last act says what it ended: {ended:?}"
    );
    assert!(
        alive(kept.pid()),
        "and nothing else: another session's process is still there — {}",
        kept.pid()
    );
}

/// Whether that child has ENDED, asked by REAPING it: an unreaped child is a zombie and `kill -0`
/// still answers for one, so [`alive`] alone cannot prove a process was left alone.
fn ended(child: &mut std::process::Child) -> bool {
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(2) {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    false
}

/// The sweep ends the processes carrying ITS mark and none whose mark merely STARTS with it. A
/// placement's id is `{session}-{pid}-{n}` and `n` is a decimal counter, so `-1` is a prefix of
/// `-12`: a sweep that matched a prefix ended another placement's work on the same machine, and the
/// call it killed came back as `exit 143` having done nothing.
#[test]
fn a_sweep_ends_the_processes_with_its_own_mark_and_not_one_whose_mark_starts_with_it() {
    let fake = Fake::new("prefix");
    let (_repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let tag = std::process::id();
    let mine = format!("s-prefix-{tag}-1");
    let theirs = format!("s-prefix-{tag}-12");
    assert!(theirs.starts_with(&mine), "the fixture is about a prefix");

    let mut own = Neighbour::start(&fake.dir, &mine);
    let mut neighbour = Neighbour::start(&fake.dir, &theirs);
    assert!(
        alive(own.pid()) && alive(neighbour.pid()),
        "both are running before the sweep"
    );

    let name = format!("sweep-prefix-{tag}");
    std::fs::write(paths.default_key(&name), b"a dedicated key").expect("the key");
    let ssh = Ssh::connect(
        Host {
            name,
            hostname: "fake.invalid".into(),
            user: Some("tester".into()),
            root: "~/letibot-remote".into(),
            mise: Some(Mise::Off),
            ..Host::default()
        },
        Some(fake.program.clone()),
        paths,
    )
    .expect("the connection opens")
    .with_mark(&mine);

    let said = ssh.end_session_processes().expect("the sweep ran");
    assert!(
        said.starts_with("1 process(es)"),
        "the sweep found its own process and no other: {said}"
    );
    assert!(
        ended(&mut own.0),
        "the process carrying the sweep's own mark is ended: {said}"
    );
    assert!(
        !ended(&mut neighbour.0),
        "and the one whose mark only starts with it is untouched: {said}"
    );
}
