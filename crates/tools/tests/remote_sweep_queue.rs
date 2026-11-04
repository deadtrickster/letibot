//! The last act's sweep when the host's connection queue is full. `placements_down` runs before
//! `daemon.shutdown()`, so all slots may be held by the session's own `bash` calls. A sweep that got
//! no slot never ran and must stay on the list. Own binary: the list is per process.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_tools::backend::ExecBackend;
use letibot_tools::exec::{Egress, ScopeKind, SpawnRequest};
use letibot_tools::remote::{Host, Mise, Paths, SshBackend, SshSpec};

/// A fake ssh: it ignores its flags and runs the last argument with `HOME` in a temporary
/// directory, so `~` expands on the far side as it does over a real connection.
struct Fake {
    dir: PathBuf,
    program: PathBuf,
}

impl Fake {
    fn new(tag: &str) -> Fake {
        let dir = std::env::temp_dir().join(format!(
            "letibot-sweep-queue-{tag}-{}-{}",
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

/// The host a placement is made to: no mise, and no boundary, because the probe would answer
/// differently on Linux and on a Mac. The bound is short because the sweep inherits it, and the
/// queue this test fills is what the sweep must wait out.
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
        bound: Some(Duration::from_secs(3)),
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

/// A process this test started on the fake remote, killed when the test ends: a failed assertion
/// must not leave a five-minute sleep behind.
struct Started(u32);

impl Drop for Started {
    fn drop(&mut self) {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &format!("-{}", self.0)])
            .output();
    }
}

#[test]
fn a_sweep_that_got_no_slot_is_asked_again() {
    let fake = Fake::new("queue");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(&repo, &paths, &fake.program))
        .expect("the placement is arranged");
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
    let pid = host.job(&job).expect("the job is known").pid;
    let _guard = Started(pid);
    assert!(alive(pid), "the fixture's job is running: {pid}");

    // Eight calls of the session's own, in flight when the stop reaches the sweep: the queue is
    // `MAX_SESSIONS_PER_HOST` deep and every slot is one of those calls'.
    let held: Vec<_> = (0..letibot_tools::rpc::MAX_SESSIONS_PER_HOST)
        .map(|_| {
            letibot_tools::rpc::enter("fake", Duration::from_secs(60)).expect("the queue has room")
        })
        .collect();

    let first = letibot_tools::remote::placements_down();
    assert!(
        first
            .iter()
            .any(|said| said.contains("no slot in the connection queue")),
        "the sweep could not run while the session's calls held the host: {first:?}"
    );
    assert!(
        alive(pid),
        "and it never ran, so the job it would have ended is still there: {pid}"
    );

    // The calls the stop interrupted are gone, and the next door to ask is the one that has to
    // find the placement again.
    drop(held);
    let second = letibot_tools::remote::placements_down();
    assert!(
        second
            .iter()
            .any(|said| said.contains("process(es) this session started")),
        "a sweep that never got a slot was never asked again: {second:?}"
    );
    let began = Instant::now();
    let mut gone = false;
    while began.elapsed() < Duration::from_secs(5) {
        if !alive(pid) {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(gone, "the second ask ended the job the session left: {pid}");
}
