//! **Remote placement, against a fake ssh** — `docs/remote-hosts.md`'s RH1 row.
//!
//! # No network, and no real host, ever
//!
//! The `ssh` these tests run is a script this file writes: it accepts every flag the
//! backend passes (`-i`, the `-o` options, the destination), ignores them, and **executes
//! the last argument locally** — which is the remote command. The "remote machine" is a
//! temporary directory standing in for its home (`HOME` is set to it inside the fake), so
//! `root = "~/letibot-remote"` is expanded **on the far side** exactly as it is over a real
//! connection, and every absolute path the backend spells from then on is one this fake
//! can open.
//!
//! What that buys is the real thing under test: the commands, the base64 channel, the
//! `setsid` launch, the job files and the `git bundle`/`git fetch` round trip are the
//! production ones. What it does not test is ssh itself — a test that needed a second
//! machine could not run in this suite, and would prove the network rather than the
//! placement.
//!
//! `setsid` does not exist on Darwin. The fake remote gets one: a perl one-liner that
//! calls `POSIX::setsid()` and execs, so a job on the "remote" leads its own process group
//! exactly as it does on Linux — which is the property `job_kill`'s group signal depends
//! on, and the one thing a passthrough shim would silently weaken.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_tools::backend::{BackendError, Command, ExecBackend};
use letibot_tools::exec::{JobId, JobState, ScopeKind, SpawnRequest, Waited};
use letibot_tools::remote::{Host, Hosts, Paths, SshBackend, SshSpec};

/// The short id this suite's session gets: the last eight of `s-01abcdef`.
const SHORT: &str = "01abcdef";

/// One fake remote: a temporary home, a repository to send, and an `ssh` that runs
/// commands against the first.
struct Rig {
    dir: PathBuf,
    /// Where the checkout lands — `~/letibot-remote/the-repo/01abcdef`, on the "remote".
    worktree: PathBuf,
    /// The jobs' own directory, on the "remote".
    jobs: PathBuf,
    repo: PathBuf,
    ssh: PathBuf,
    /// **What the fake ssh was handed**, one line per call: its argv, and its environment.
    log: PathBuf,
    /// letibot's own ssh material for this test: a key under the config directory, and the host
    /// key already accepted. `Ssh::connect` refuses without both, which is the point of it.
    paths: Paths,
    host: Host,
}

impl Rig {
    fn new(tag: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "letibot-remote-{tag}-{}-{}",
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

        // **A `setsid` for the boxes that have none** — Linux does, Darwin does not — so a
        // job on the fake remote leads its own process group exactly as it does on a Linux
        // host: `kill -TERM -<pid>` is only safe when it does, and `RemoteProcesses::kill`
        // asks before it signals. Where the box has `setsid`, its own is what runs (the
        // shim directory is first on the fake remote's PATH, so the shim would win) — hence
        // this is written only when there is nothing to shim.
        if !on_path("setsid") {
            write_executable(
                &bin.join("setsid"),
                "#!/usr/bin/env perl\nuse POSIX ();\nPOSIX::setsid();\nexec { $ARGV[0] } @ARGV;\n",
            );
        }

        // **The fake ssh records its argv and its environment**, and refuses to be anything but
        // a passthrough: the flags under test are the ones it is HANDED, so it must not interpret
        // them (a fake that understood `-i` would be testing itself).
        let ssh = bin.join("ssh");
        let log = dir.join("ssh.log");
        write_executable(
            &ssh,
            &format!(
                "#!/bin/bash\n\
                 {{ printf 'ARGV %s\\n' \"$*\"; env | sed 's/^/ENV /'; }} >> '{}'\n\
                 export PATH='{}':$PATH\n\
                 export HOME='{}'\n\
                 for last; do :; done\n\
                 exec bash -lc \"$last\"\n",
                log.display(),
                bin.display(),
                home.display()
            ),
        );
        // **letibot's own key and known host**, in a config directory of this test's own — the
        // two facts a connection is refused without, and neither is the operator's.
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

        let worktree = home.join("letibot-remote").join("the-repo").join(SHORT);
        let jobs = worktree.join(".letibot-remote").join("jobs");
        let host = Host {
            name: "fake".into(),
            hostname: "fake.invalid".into(),
            user: Some("tester".into()),
            key: None,
            path: Vec::new(),
            root: "~/letibot-remote".into(),
        };
        Rig {
            dir,
            worktree,
            jobs,
            repo,
            ssh,
            log,
            paths,
            host,
        }
    }

    /// One spec, with this rig's fake ssh and its own ssh material.
    fn spec(&self) -> SshSpec {
        SshSpec {
            host: self.host.clone(),
            name: "s-01abcdef".into(),
            source: self.repo.clone(),
            writable: true,
            exec: true,
            ssh: Some(self.ssh.clone()),
            paths: Some(self.paths.clone()),
        }
    }

    /// **Every ssh call this rig made**, as `<argv>` and `<env>` lines.
    fn ssh_log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// **The backend, and the worktree it says it got.** The path is taken from the
    /// backend rather than built here: the remote resolves `~/letibot-remote` with `pwd -P`,
    /// and on this box that turns `/var/...` into `/private/var/...` — the same directory
    /// under two names, and a comparison of the two would fail for a reason that has nothing
    /// to do with the placement.
    fn place(&mut self) -> SshBackend {
        let b = SshBackend::place(&self.spec()).expect("the placement is arranged");
        self.worktree = PathBuf::from(b.worktree_path());
        self.jobs = self.worktree.join(".letibot-remote").join("jobs");
        b
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // **Only the pids this rig wrote down**, never a pattern: a job left running by a
        // panic would otherwise keep writing into a tree this drop removes.
        if let Ok(entries) = std::fs::read_dir(&self.jobs) {
            for e in entries.flatten() {
                if e.path().extension().is_some_and(|x| x == "pid")
                    && let Ok(pid) = std::fs::read_to_string(e.path())
                        .unwrap_or_default()
                        .trim()
                        .parse::<u32>()
                {
                    let _ = std::process::Command::new("kill")
                        .args(["-KILL", &pid.to_string()])
                        .output();
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn write_executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).expect("writing a fake program");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// Is this program on the box's own `PATH`? Used to decide whether the fake remote needs a
/// shim for it — a shim that shadowed a real `setsid` would be a weaker thing tested than
/// the one production runs.
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
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

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// **The flags are handed to ssh, and nothing in the call names the operator's ssh material.**
///
/// This is the fake-ssh measurement the contract's policy is pinned by: every flag is on the
/// command line `/tmp/letibot/ssh-…`-style, and neither the argv nor the environment the call runs
/// with has a path into the operator's own ssh directory. The env half matters on its own — a
/// `HOME` or an `SSH_*` that pointed there would hand ssh the same material the flags refuse.
#[test]
fn the_ssh_call_carries_the_policy_and_no_operator_ssh_material() {
    let mut rig = Rig::new("flags");
    let b = rig.place();
    let _ = b.run(&Command {
        argv: vec!["true".into()],
        cwd: String::new(),
        env: Vec::new(),
    });
    let log = rig.ssh_log();
    assert!(!log.is_empty(), "the fake ssh recorded nothing");
    for want in [
        "-F /dev/null".to_string(),
        format!(
            "-o UserKnownHostsFile={}",
            rig.paths.known_hosts().display()
        ),
        "-o GlobalKnownHostsFile=/dev/null".to_string(),
        "-o StrictHostKeyChecking=yes".to_string(),
        "-o BatchMode=yes".to_string(),
        "-o IdentitiesOnly=yes".to_string(),
        "-o IdentityAgent=none".to_string(),
        "-o ControlMaster=auto".to_string(),
        "-o ControlPath=".to_string(),
        format!("-i {}", rig.paths.default_key("fake").display()),
    ] {
        assert!(
            log.contains(&want),
            "`{want}` was not handed to ssh:\n{log}"
        );
    }
    let theirs = rig.paths.user_ssh().display().to_string();
    assert!(
        !log.contains(&theirs),
        "nothing in the argv or the environment names `{theirs}`:\n{log}"
    );
}

/// **Reading and writing a file over ssh, byte for byte, and atomically.**
///
/// The bytes matter: a NUL and invalid UTF-8 go through base64 because a channel that
/// mangles an image or a GGUF is a channel that writes files nobody can open. The
/// atomicity is the interface's own requirement ([`ExecBackend::write`]): a write that
/// cannot complete must leave the old file alone rather than truncate it.
#[test]
fn a_binary_round_trip_and_a_write_that_cannot_land_leaves_the_old_file() {
    let mut rig = Rig::new("rw");
    let b = rig.place();
    assert!(
        b.worktree_path().starts_with('/')
            && b.worktree_path()
                .ends_with(&format!("/letibot-remote/the-repo/{SHORT}")),
        "`root = ~/letibot-remote` was expanded on the REMOTE, to an absolute path: {}",
        b.worktree_path()
    );

    // NUL, invalid UTF-8, every byte — the three things a text channel gets wrong.
    let bytes: Vec<u8> = (0u8..=255).chain([0, 0, 0xff, 0x80]).collect();
    b.write("docs/blob.bin", &bytes).expect("the write lands");
    assert_eq!(b.read("docs/blob.bin").expect("the read"), bytes);

    // A directory is not a file, and a missing path is a miss rather than an I/O error.
    assert!(matches!(b.read("docs"), Err(BackendError::IsADirectory(_))));
    assert!(matches!(
        b.read("docs/nothing-here"),
        Err(BackendError::NotFound(_))
    ));

    // **Atomicity, measured**: with the directory unwritable the rename cannot happen, and
    // the file that was there is still there, whole.
    std::fs::write(rig.worktree.join("docs/keep.txt"), b"old").expect("the old file");
    let dir = rig.worktree.join("docs");
    set_mode(&dir, 0o500);
    let refused = b.write("docs/keep.txt", b"new");
    set_mode(&dir, 0o700);
    assert!(
        refused.is_err(),
        "a write into an unwritable directory must fail rather than half-land"
    );
    assert_eq!(
        b.read("docs/keep.txt").expect("the old file"),
        b"old",
        "the failed write must not have truncated what was there"
    );
}

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(p).expect("metadata").permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(p, perms).expect("chmod");
}

/// **A command runs on the far side, in the checkout**, and a `cwd` is relative to it.
#[test]
fn a_command_runs_in_the_checkout_and_names_the_host() {
    let mut rig = Rig::new("run");
    let b = rig.place();
    let out = b
        .run(&Command {
            argv: vec!["pwd".into()],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect("it runs");
    assert_eq!(out.exit, 0);
    assert_eq!(
        Path::new(String::from_utf8_lossy(&out.stdout).trim()),
        b.worktree_path(),
        "the cwd is the remote checkout"
    );

    std::fs::create_dir_all(rig.worktree.join("sub")).expect("sub");
    // **An environment addition reaches the command**, which is what `build_env` and the
    // daemon's own `LETIBOT_SOCKET` ride on.
    let out = b
        .run(&Command {
            argv: vec!["sh".into(), "-c".into(), "echo $A_TEST_VARIABLE".into()],
            cwd: "sub".into(),
            env: vec![("A_TEST_VARIABLE".into(), "a value".into())],
        })
        .expect("it runs");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "a value");

    // **The host is named where a person looks**: the card's own sentence, and the
    // description a session shows.
    assert_eq!(b.host_label().as_deref(), Some("fake over ssh"));
    let described = b.describe();
    assert!(described.contains("`fake`"), "{described}");
    assert!(described.contains("NOT CONFINED"), "{described}");
    assert_eq!(
        b.workspace_path().as_deref(),
        Some(b.worktree_path()),
        "a session's workspace is the checkout, not this machine's root"
    );
    assert!(
        b.root_path().is_none(),
        "a remote path is not a boundary fact about this machine"
    );
}

/// **`list` and `stat`, and the tree in the order a walk would have produced it.**
#[test]
fn list_stat_and_the_walk_agree_with_a_local_tree() {
    let mut rig = Rig::new("list");
    let b = rig.place();
    std::fs::create_dir_all(rig.worktree.join("src/deep")).expect("a tree");
    std::fs::write(rig.worktree.join("src/a.rs"), b"aaa").expect("a file");
    std::fs::write(rig.worktree.join("src/deep/b.rs"), b"bb").expect("a file");

    let entries = b.list("src").expect("it lists");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["a.rs", "deep"],
        "sorted by name, one directory deep"
    );
    assert_eq!(entries[0].bytes, 3);
    assert!(!entries[0].is_dir);
    assert!(entries[1].is_dir);

    let tree = b.list_tree("src", 100, &|_| false).expect("it walks");
    let walked: Vec<&str> = tree.0.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        walked,
        vec!["a.rs", "deep", "b.rs"],
        "breadth-first, as `list` would have produced it: {walked:?}"
    );
    assert!(!tree.1, "nothing was cut");

    assert_eq!(b.stat("src/a.rs").expect("it stats").bytes, 3);
    assert!(!b.stat("src/a.rs").expect("it stats").is_dir);
    assert!(b.stat("src/deep").expect("it stats").is_dir);
    assert!(b.stat("src/nowhere").is_none(), "a miss is a miss");
    assert!(matches!(b.list("nowhere"), Err(BackendError::NotFound(_))));
}

/// **A background job outlives the ssh call that started it**, is read through its files,
/// and is killed by its own process group.
///
/// This is the whole of the contract's "Jobs and processes": the launch returns, the job
/// keeps running on that machine, `job_output` reads what it wrote, `job_kill` ends it. The
/// assertion that it *outlived* the connection is the pid — the ssh that started it has
/// returned (it is gone), and the job's own pid is alive.
#[test]
fn a_background_job_outlives_its_ssh_and_is_read_and_killed() {
    let mut rig = Rig::new("job");
    let b = rig.place();
    let host = b.processes_arc().expect("this backend starts processes");
    let scope = host
        .scope_for(ScopeKind::Session, None)
        .expect("a session scope");
    let id = host
        .spawn(&SpawnRequest {
            command: "echo started-marker; echo to-stderr >&2; sleep 300".into(),
            slug: Some("a-long-job".into()),
            cwd: String::new(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![("A_JOB_VARIABLE".into(), "set".into())],
            tty: false,
            memory: None,
        })
        .expect("the job starts");

    // **It is running, and the ssh call that made it has returned.**
    let view = host.job(&id).expect("the job is known");
    assert!(view.pid > 1, "the remote wrote a pid: {view:?}");
    assert!(
        view.state.is_running(),
        "still running after its launch returned: {:?}",
        view.state
    );
    assert!(
        alive(view.pid),
        "the job's process on the remote is alive after the ssh that started it exited"
    );
    assert_eq!(view.slug.as_deref(), Some("a-long-job"));
    assert!(view.command.contains("started-marker"));

    // Its output is a file on that machine — `<worktree>/.letibot-remote/jobs/<id>.out` —
    // and the model's stderr is in it: ssh carries one channel, so the job keeps one file.
    let n = id.0.trim_start_matches('j').to_string();
    let out_file = rig.jobs.join(format!("{n}.out"));
    let asked = Instant::now();
    while asked.elapsed() < Duration::from_secs(20)
        && !std::fs::read_to_string(&out_file)
            .map(|t| t.contains("to-stderr"))
            .unwrap_or(false)
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    let written = std::fs::read_to_string(&out_file).expect("the job's output file");
    assert!(written.contains("started-marker"), "{written}");
    assert!(written.contains("to-stderr"), "{written}");
    assert!(
        rig.jobs.join(format!("{n}.pid")).is_file(),
        "the pid is written down, which is what a restarted daemon finds the job by"
    );
    let slice = host.output(&id, 0, usize::MAX).expect("its output");
    assert!(slice.text().contains("started-marker"), "{}", slice.text());

    // **Killed by its group**, and the group is really the job's own: the launch used
    // `setsid`, so `kill -TERM -<pid>` cannot reach this test's own process group.
    let reaping = host
        .kill_job_as(&id, "the test asking")
        .expect("the kill is recorded");
    assert!(
        reaping.observed.iter().any(|p| p.pid == view.pid),
        "what was there before the kill is on the record: {reaping:?}"
    );
    assert!(reaping.clean(), "nothing survived: {reaping:?}");
    assert!(
        !alive(view.pid),
        "the job's process is gone from the remote"
    );
    assert!(matches!(
        host.job(&id).expect("still known").state,
        JobState::Killed { .. }
    ));

    // **A scope end kills what is in it**, and the record says nothing survived.
    let ended = host.end_scope(&scope).expect("a scope end is recorded");
    assert!(ended.observed.is_empty(), "the job was already ended");
}

/// **A job that exits settles from its exit file**, and the status is the command's own.
#[test]
fn a_finished_job_reports_the_commands_own_exit_status() {
    let mut rig = Rig::new("exit");
    let b = rig.place();
    let host = b.processes_arc().expect("this backend starts processes");
    let id: JobId = host
        .spawn(&SpawnRequest {
            command: "echo before-the-exit; exit 7".into(),
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
    let waited = host
        .wait_job(&id, Duration::from_secs(30))
        .expect("the wait");
    assert!(matches!(waited, Waited::Happened { .. }), "{waited:?}");
    let state = host.job(&id).expect("known").state;
    assert!(
        matches!(state, JobState::Exited { code: 7 }),
        "the exit status is the command's own: {state:?}"
    );
}

/// **The tree: out as a bundle, back as a branch.**
///
/// The contract's §"The tree", measured end to end: the local `HEAD` arrives as a checkout
/// on `remote/<short>`, a commit made "there" comes back as `remote/<host>/<short>`, the
/// local branch the work started from is untouched, and a local edit that was never
/// committed is named in the placement's own notes because it did **not** go.
#[test]
fn the_head_goes_out_and_the_branch_comes_back() {
    let mut rig = Rig::new("git");
    // An uncommitted local edit: it must be named, and it must not be carried.
    std::fs::write(rig.repo.join("uncommitted.txt"), b"not carried\n").expect("a local edit");

    let b = rig.place();
    assert!(
        rig.worktree.join("README").is_file(),
        "the checkout is on the remote: {}",
        rig.worktree.display()
    );
    assert!(
        !rig.worktree.join("uncommitted.txt").exists(),
        "uncommitted local changes are NOT carried"
    );
    let notes = b.placement_notes().join("\n");
    assert!(notes.contains("remote/"), "{notes}");
    assert!(
        notes.contains("NOT carried") && notes.contains("uncommitted.txt"),
        "the placement says what stayed here, with the list: {notes}"
    );
    assert_eq!(b.branch(), format!("remote/{SHORT}"));
    assert_eq!(
        git(&rig.worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        format!("remote/{SHORT}")
    );

    // **A commit made "remotely"** — the same git, run where the session would run it.
    std::fs::write(rig.worktree.join("feature.rs"), b"// the work\n").expect("a file");
    git(&rig.worktree, &["add", "feature.rs"]);
    git(
        &rig.worktree,
        &["commit", "-q", "-m", "the work the child did"],
    );
    let remote_head = git(&rig.worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    // And one left uncommitted there, which the branch does not hold.
    std::fs::write(rig.worktree.join("scratch.txt"), b"still only there\n").expect("a file");

    let said = b.close().expect("a session close says where its work went");
    assert!(said.contains(&format!("remote/fake/{SHORT}")), "{said}");
    assert!(
        said.contains("uncommitted change"),
        "a dirty remote tree is not in the branch, and the report says so: {said}"
    );
    assert_eq!(
        git(&rig.repo, &["rev-parse", &format!("remote/fake/{SHORT}")]).trim(),
        remote_head,
        "the branch came back at the commit that was made there"
    );
    // The local branch is where it was: the queue, not the remote, moves it.
    assert_eq!(
        git(&rig.repo, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "main"
    );
    assert!(
        git(
            &rig.repo,
            &["show", &format!("remote/fake/{SHORT}:feature.rs")]
        )
        .contains("the work"),
        "the commit's own contents came with it"
    );
    // Closing twice says the same thing rather than fetching again.
    assert_eq!(b.close().expect("the second close"), said);
}

/// **A folder that is not a repository is refused, and names the fix.**
#[test]
fn a_plain_folder_is_refused_for_remote_placement() {
    let rig = Rig::new("norepo");
    let plain = rig.dir.join("a-plain-folder");
    std::fs::create_dir_all(&plain).expect("a folder");
    let refused = SshBackend::place(&SshSpec {
        host: rig.host.clone(),
        name: "s-01abcdef".into(),
        source: plain.clone(),
        writable: true,
        exec: true,
        ssh: Some(rig.ssh.clone()),
        paths: Some(rig.paths.clone()),
    })
    .expect_err("a folder is not a repository");
    let said = refused.to_string();
    assert!(said.contains("not a git repository"), "{said}");
    assert!(said.contains("Nothing was placed"), "{said}");
    assert!(
        !rig.worktree.exists(),
        "nothing was arranged on the host either: {}",
        rig.worktree.display()
    );
}

/// **A name that is not in the file is refused, with the ones that are.**
///
/// The text a model or an operator is actually shown, because a refusal that does not name
/// the near-miss is the refusal that costs an hour.
#[test]
fn the_hosts_file_is_the_only_place_a_host_can_come_from() {
    let file = std::env::temp_dir().join(format!(
        "letibot-hosts-{}-{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(
        &file,
        "[host.lab2x1]\nhostname = \"lab2x1\"\nuser = \"dead\"\nroot = \"~/letibot-remote\"\n",
    )
    .expect("a hosts file");
    let hosts = Hosts::load_from(&file);
    let _ = std::fs::remove_file(&file);
    assert_eq!(hosts.names(), vec!["lab2x1".to_string()]);
    let refused = hosts.refusal("lab2");
    assert!(refused.contains("lab2x1"), "{refused}");
    assert!(refused.contains("Nothing was placed"), "{refused}");
    assert!(
        refused.contains("[host.lab2]") || refused.contains("host.NAME"),
        "{refused}"
    );
}
