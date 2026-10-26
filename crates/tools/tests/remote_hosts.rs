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
use letibot_tools::builtins::hosts::HostsTool;
use letibot_tools::exec::{Egress, JobId, JobState, ScopeKind, SpawnRequest, Waited};
use letibot_tools::remote::{Host, Hosts, Mise, Paths, SshBackend, SshSpec};

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
            // **A fixture's host has no mise**, and says so rather than letting the default probe
            // find this box's own (`/opt/homebrew/bin/mise`) and wrap every command in it: these
            // tests assert the shape of the command line, and a machine-dependent wrapper would
            // make the same assertion mean two things on two boxes. `the_mise_wrapper_*` below
            // are the tests that do have one.
            mise: Some(Mise::Off),
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
    ///
    /// **The bound is short on purpose**: every one of these tests runs against a fake that answers
    /// in milliseconds, and a fake that is *meant* to hang has to be measured in a second rather
    /// than in [`letibot_tools::remote::STEP_BOUND`].
    fn spec(&self) -> SshSpec {
        SshSpec {
            host: self.host.clone(),
            name: "s-01abcdef".into(),
            source: self.repo.clone(),
            writable: true,
            exec: true,
            ssh: Some(self.ssh.clone()),
            paths: Some(self.paths.clone()),
            bound: Some(Duration::from_secs(5)),
            // **This fixture's host confines nothing, and says so.** The probe runs on the machine
            // behind the fake — the machine running the test — so an unpinned answer would make
            // the same assertion mean two things on two boxes (confined on Linux with `bwrap`,
            // unconfined on a Mac). `remote_confine.rs` is the suite that tests the boundary.
            egress: Egress::Denied,
            confine: Some(false),
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
        bound: Some(Duration::from_secs(5)),
        egress: Egress::Denied,
        confine: Some(false),
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

// ---------------------------------------------------------------------------
// Nothing hangs, and the retry follows idempotency
// ---------------------------------------------------------------------------
//
// The operator: *"the tooling shouldnt hang, that is important"*. Everything below is measured
// against a fake ssh that is written to misbehave in one specific way — it goes silent, or it
// drops the connection, or it answers only the second time — because those are the three shapes
// a real machine fails in and none of them is reachable from a fake that always answers.

/// **A fake ssh whose behaviour this test writes.**
///
/// It logs every invocation, sets up a `HOME` that is the "remote", and then runs `body` with the
/// last argument (the command ssh would run) in `$last`. A body that `exec`s runs the command
/// locally; a body that sleeps is a host that has stopped answering. `@DIR@` in a body is the
/// fake's own directory, so a body's bookkeeping never lands in the test process's working
/// directory.
struct Fake {
    dir: PathBuf,
    program: PathBuf,
    log: PathBuf,
}

impl Fake {
    fn new(tag: &str, body: &str) -> Fake {
        let dir = std::env::temp_dir().join(format!(
            "letibot-fake-ssh-{tag}-{}-{}",
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
        if !on_path("setsid") {
            write_executable(
                &bin.join("setsid"),
                "#!/usr/bin/env perl\nuse POSIX ();\nPOSIX::setsid();\nexec { $ARGV[0] } @ARGV;\n",
            );
        }
        let program = bin.join("ssh");
        let log = dir.join("ssh.log");
        let body = body.replace("@DIR@", &dir.display().to_string());
        write_executable(
            &program,
            &format!(
                "#!/bin/bash\n\
                 {{ printf 'ARGV %s\\n' \"$*\"; }} >> '{log}'\n\
                 export PATH='{bin}':$PATH\n\
                 export HOME='{home}'\n\
                 for last; do :; done\n\
                 {body}\n",
                log = log.display(),
                bin = bin.display(),
                home = home.display(),
            ),
        );
        Fake { dir, program, log }
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("ARGV "))
            .map(str::to_string)
            .collect()
    }

    /// How many invocations carried `needle` anywhere in their argv — the count a retry changes.
    fn count(&self, needle: &str) -> usize {
        self.calls().iter().filter(|l| l.contains(needle)).count()
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

fn host_named(name: &str) -> Host {
    Host {
        name: name.to_string(),
        hostname: format!("{name}.invalid"),
        user: Some("tester".to_string()),
        key: None,
        path: Vec::new(),
        root: "~/letibot-remote".into(),
        // See `Rig::new`: a fixture's host has no mise unless the test is about mise.
        mise: Some(Mise::Off),
    }
}

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
    }
}

/// **A host that stops answering does not hold any operation that is not a command.**
///
/// The fake answers everything until a marker file appears, so the placement itself is real; after
/// that every call sleeps for ever, and each operation has to come back inside its own bound with
/// a sentence naming the host and the step. The bound is two seconds here and twenty in
/// production — short enough to measure, long enough that the placement before the marker appears
/// is not racing it on a busy box.
#[test]
fn every_step_that_is_not_a_command_stops_at_its_bound() {
    let bound = Duration::from_secs(2);
    // The marker lives outside the fake's own directory so that `Drop` cannot race it, and it is
    // per-process so two runs of this suite cannot cross.
    let silent = PathBuf::from(format!("/tmp/letibot-silent-{}.marker", std::process::id()));
    let _ = std::fs::remove_file(&silent);
    let fake = Fake::new(
        "silent",
        &format!(
            "if [ -f '{m}' ]; then sleep 30; fi\n\
             exec bash -lc \"$last\"",
            m = silent.display()
        ),
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(&repo, &paths, &fake.program, bound)).expect("placed");

    // Standing in for a host that has gone quiet mid-session.
    std::fs::write(&silent, b"stop answering").expect("the marker");

    /// One operation under test: its name, and how to run it and describe what came back. The
    /// lifetime is the backend's — every closure below borrows it.
    type Step<'a> = (&'static str, Box<dyn Fn() -> String + 'a>);

    let cases: Vec<Step> = vec![
        (
            "read",
            Box::new(|| {
                format!(
                    "{:?}",
                    b.read("README")
                        .expect_err("a read that never answers is refused")
                )
            }),
        ),
        (
            "list",
            Box::new(|| {
                format!(
                    "{:?}",
                    b.list(".")
                        .expect_err("a listing that never answers is refused")
                )
            }),
        ),
        (
            "list_tree",
            Box::new(|| {
                format!(
                    "{:?}",
                    b.list_tree(".", 10, &|_| false)
                        .expect_err("a walk that never answers is refused")
                )
            }),
        ),
        (
            "write",
            Box::new(|| {
                format!(
                    "{:?}",
                    b.write("written.txt", b"bytes")
                        .expect_err("a write that never answers is refused")
                )
            }),
        ),
    ];
    // `stat` is the one that answers `None` for a host that said nothing — a miss and a silence
    // are the same fact to its caller — so what is asserted for it is that it RETURNS, inside the
    // bound, rather than a sentence it has nowhere to put.
    let began = Instant::now();
    let none = b.stat("README");
    assert!(
        none.is_none() && began.elapsed() < bound * 3 + Duration::from_secs(2),
        "`stat` returned {:?} after {:?} against a host that never answers",
        none,
        began.elapsed()
    );
    for (step, run) in cases {
        let began = Instant::now();
        let said = run();
        let took = began.elapsed();
        assert!(
            took < bound * 3 + Duration::from_secs(2),
            "`{step}` took {took:?} against a host that never answers"
        );
        assert!(
            said.contains("fake"),
            "the refusal for `{step}` must name the host: {said}"
        );
        assert!(
            said.contains("no answer within")
                || said.contains("lost the connection")
                || said.contains("is unavailable"),
            "the refusal for `{step}` must say it never answered — or, once the third failure has \
             opened the breaker ([`letibot_tools::rpc`], measured by \
             `three_failed_calls_open_the_breaker_and_the_next_fails_at_once`), that the host is \
             not being dialled at all: {said}"
        );
    }
    let _ = std::fs::remove_file(&silent);
}

/// **A job's output and a job's kill are bounded too**, and a kill that could not read back says
/// the process's fate is unknown rather than counting it as removed.
#[test]
fn a_jobs_output_and_kill_stop_at_their_bound() {
    let bound = Duration::from_secs(2);
    let silent = PathBuf::from(format!(
        "/tmp/letibot-silent-job-{}.marker",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&silent);
    let fake = Fake::new(
        "silentjob",
        &format!(
            "if [ -f '{m}' ]; then sleep 30; fi\n\
             exec bash -lc \"$last\"",
            m = silent.display()
        ),
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(&repo, &paths, &fake.program, bound)).expect("placed");
    let host = b.processes_arc().expect("this backend starts processes");
    let id = host
        .spawn(&SpawnRequest {
            command: "sleep 60".into(),
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
    let pid = host.job(&id).expect("known").pid;
    assert!(pid > 1, "the job has a pid before it goes quiet");

    std::fs::write(&silent, b"stop answering").expect("the marker");

    let began = Instant::now();
    let out = host.output(&id, 0, 64);
    assert!(
        began.elapsed() < bound * 6 + Duration::from_secs(3),
        "job_output took {:?} against a host that never answers",
        began.elapsed()
    );
    let said = format!("{out:?}");
    assert!(said.contains("fake"), "{said}");
    assert!(
        said.contains("no answer within") || said.contains("lost the connection"),
        "{said}"
    );

    let began = Instant::now();
    let reaping = host.kill_job_as(&id, "the test asking");
    assert!(
        began.elapsed() < bound * 10 + Duration::from_secs(5),
        "job_kill took {:?} against a host that never answers",
        began.elapsed()
    );
    let reaping = reaping.expect("a kill is recorded even when nothing answers");
    assert!(
        !reaping.removed,
        "nothing was read back, so the process must NOT be reported as removed: {reaping:?}"
    );
    assert!(
        reaping
            .note
            .as_deref()
            .is_some_and(|n| n.contains("UNKNOWN") && n.contains("fake")),
        "the record has to say what it does not know: {reaping:?}"
    );
    // And the process on this box is really gone — the test's own cleanup, not the backend's.
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output();
}

/// **A command whose connection drops is reported as an UNKNOWN outcome, and is never re-sent.**
///
/// `run` is not idempotent: the command may have run, partly run, or still be running. So the
/// sentence says exactly that — and the fake's log proves it was shipped once.
#[test]
fn a_command_that_loses_its_connection_is_reported_unknown_and_never_re_sent() {
    let fake = Fake::new(
        "drops",
        "case \"$last\" in *drops-here*) echo 'ssh: connection reset by peer' >&2; exit 255 ;; esac\n\
         exec bash -lc \"$last\"",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(
        &repo,
        &paths,
        &fake.program,
        Duration::from_secs(5),
    ))
    .expect("placed");

    let refused = b
        .run(&Command {
            argv: vec!["echo".into(), "drops-here".into()],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect_err("a dropped connection is not a result");
    let said = refused.to_string();
    assert!(
        said.contains("the connection to fake dropped")
            && said.contains("check before running it again"),
        "the sentence must say the outcome is unknown: {said}"
    );
    assert!(
        said.contains("may have run, partly run, or still be running"),
        "{said}"
    );
    assert!(said.contains("NOT retried"), "{said}");
    assert_eq!(
        fake.count("drops-here"),
        1,
        "a non-idempotent call must never be sent twice: {:#?}",
        fake.calls()
    );
}

/// **An idempotent step that loses the connection is asked exactly once more.**
#[test]
fn a_read_that_loses_its_connection_is_asked_once_more() {
    let fake = Fake::new(
        "retry",
        "c=@DIR@/count-read\n\
         case \"$last\" in *'base64 < '*) n=$(cat $c 2>/dev/null || echo 0); n=$((n+1)); echo $n > $c; if [ \"$n\" = 1 ]; then echo 'ssh: broken pipe' >&2; exit 255; fi ;; esac\n\
         exec bash -lc \"$last\"",
    );
    let _ = std::fs::remove_file(fake.path("count-read"));
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(
        &repo,
        &paths,
        &fake.program,
        Duration::from_secs(5),
    ))
    .expect("placed");

    let bytes = b
        .read("README")
        .expect("the second attempt answers, so the read succeeds");
    assert_eq!(bytes, b"the first commit\n");
    assert_eq!(
        fake.count("base64 < "),
        2,
        "a read is asked exactly once more: {:#?}",
        fake.calls()
    );
}

/// **A write that loses its connection is retried as a whole, and nothing is written twice.**
///
/// The chunk calls are never retried on their own — a chunk sent twice appends itself twice — so
/// the second attempt rewrites the temporary file from the top and the bytes land exactly once.
#[test]
fn a_write_that_drops_mid_way_lands_its_bytes_exactly_once() {
    let fake = Fake::new(
        "writeretry",
        "c=@DIR@/count-write\n\
         case \"$last\" in *'base64 -d > '*) n=$(cat $c 2>/dev/null || echo 0); n=$((n+1)); echo $n > $c; if [ \"$n\" = 1 ]; then echo 'ssh: broken pipe' >&2; exit 255; fi ;; esac\n\
         exec bash -lc \"$last\"",
    );
    let _ = std::fs::remove_file(fake.path("count-write"));
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(
        &repo,
        &paths,
        &fake.program,
        Duration::from_secs(5),
    ))
    .expect("placed");

    // Two chunks' worth, so a duplicated chunk would be visible as extra bytes.
    let bytes: Vec<u8> = (0..(60 * 1024)).map(|i| (i % 251) as u8).collect();
    b.write("big.bin", &bytes)
        .expect("the whole write is retried and lands");
    let landed = b.read("big.bin").expect("it reads back");
    assert_eq!(landed.len(), bytes.len(), "the bytes landed exactly once");
    assert_eq!(landed, bytes);
}

/// **A stale `ControlMaster` socket does not hold the next call.**
///
/// The socket outlives the connection it belongs to, and ssh would try to multiplex through it and
/// wait. It is checked with a few seconds on the question, and a socket nobody answers on is
/// removed before the call dials afresh.
#[test]
fn a_stale_control_master_is_checked_and_dropped() {
    let fake = Fake::new("stale", "exec bash -lc \"$last\"");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(
        &repo,
        &paths,
        &fake.program,
        Duration::from_secs(5),
    ))
    .expect("placed");

    let stale = letibot_tools::remote::control_path("fake", std::process::id());
    if let Some(dir) = stale.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&stale, b"a socket nothing is listening on").expect("a stale socket");

    let began = Instant::now();
    let bytes = b
        .read("README")
        .expect("the call dials afresh rather than waiting on the socket");
    assert_eq!(bytes, b"the first commit\n");
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "a stale master held the call for {:?}",
        began.elapsed()
    );
    assert!(
        !stale.exists(),
        "the socket nothing answered on was removed: {}",
        stale.display()
    );
    assert!(
        fake.calls().iter().any(|c| c.contains("-O check")),
        "the socket was asked about before it was dropped: {:#?}",
        fake.calls()
    );
    let _ = std::fs::remove_file(&stale);
}

/// **The session's own job directory is not part of the tree.**
///
/// `.letibot-remote/` appears the moment a job runs, and it used to appear on close as
/// `?? .letibot-remote/` — a change the operator was told was theirs to look at. The checkout's own
/// exclude file says otherwise, and `git status` is what proves it.
#[test]
fn the_sessions_own_job_directory_is_excluded_from_the_tree() {
    let mut rig = Rig::new("exclude");
    let b = rig.place();
    let host = b.processes_arc().expect("this backend starts processes");
    let id = host
        .spawn(&SpawnRequest {
            command: "echo the-work".into(),
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
    let _ = host.wait_job(&id, Duration::from_secs(30));
    assert!(
        rig.jobs.is_dir(),
        "the job really ran: {}",
        rig.jobs.display()
    );

    // **`git` itself says so**, which is the only proof that matters: the exclude file git READS
    // is the one the placement wrote, whatever a link-heavy worktree layout does with `--git-path`.
    let status = git(&rig.worktree, &["status", "--porcelain"]);
    assert!(
        !status.contains(".letibot-remote"),
        "the job directory is ignored by git itself: {status}"
    );
    let exclude = git(&rig.worktree, &["rev-parse", "--git-path", "info/exclude"]);
    let exclude = PathBuf::from(exclude.trim());
    let exclude = if exclude.is_absolute() {
        exclude
    } else {
        rig.worktree.join(exclude)
    };
    let said = std::fs::read_to_string(&exclude).expect("the exclude file the checkout got");
    assert!(
        said.lines().any(|l| l.trim() == ".letibot-remote/"),
        "{} names the job directory: {said}",
        exclude.display()
    );

    // And the close does not report it as work left uncommitted.
    let close = b.close().expect("a close says where the work went");
    assert!(
        !close.contains("uncommitted change") && !close.contains(".letibot-remote"),
        "a job's own files are not a change the operator has to review: {close}"
    );
}

/// **`hosts {probe: true}` asks every declared host at once**, and reports each one's live facts.
///
/// The overlap is measured rather than assumed: each fake call records when it began and ended,
/// and the two hosts' intervals must intersect — which they cannot if the probes ran one after
/// the other.
#[test]
fn the_hosts_tool_probes_two_hosts_in_parallel() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-hosts-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let fake = Fake::new(
        "probe",
        "t() { perl -MTime::HiRes=time -e 'printf \"%.3f\", time'; }\n\
         printf 'START %s %s\\n' \"$(t)\" \"$*\" >> @DIR@/SPAN\n\
         sleep 0.4\n\
         printf 'END %s %s\\n' \"$(t)\" \"$*\" >> @DIR@/SPAN\n\
         exec bash -lc \"$last\"",
    );
    let paths = Paths::in_home(dir.join("home"));
    std::fs::create_dir_all(paths.keys()).expect("the keys directory");
    for name in ["h1", "h2"] {
        std::fs::write(paths.default_key(name), b"a dedicated key").expect("a key");
    }
    std::fs::write(
        paths.known_hosts(),
        "h1.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n\
         h2.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n",
    )
    .expect("the host keys");
    let file = dir.join("hosts.toml");
    std::fs::write(
        &file,
        "[host.h1]\nhostname = \"h1.invalid\"\nuser = \"tester\"\nmise = false\n\n\
         [host.h2]\nhostname = \"h2.invalid\"\nuser = \"tester\"\nmise = false\n",
    )
    .expect("a hosts file");

    let tool = HostsTool::new()
        .with_file(&file)
        .with_paths(paths)
        .with_ssh(&fake.program)
        .with_bounds(Duration::from_secs(10), Duration::from_secs(8));
    let began = Instant::now();
    let answer = tool.answer(&serde_json::json!({"probe": true}));
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "a probe took {:?}",
        began.elapsed()
    );
    let said = answer.payload;
    assert_eq!(said.matches("REACHABLE").count(), 2, "{said}");
    assert!(said.contains("`h1`") && said.contains("`h2`"), "{said}");
    // The facts are the machine's own — this box's, since the fake runs the script here.
    assert!(said.contains("cores:"), "{said}");
    assert!(
        !said.contains("TIMED OUT") && !said.contains("NOT REACHED"),
        "{said}"
    );
    // **Rule 7's view is on the row**: the last calls this process made to the host, by name.
    assert!(said.contains("calls:    last "), "{said}");
    assert!(said.contains("a probe of h1"), "{said}");
    assert!(said.contains("slowest"), "{said}");

    // **The overlap**: both spans intersect, which one-after-another cannot produce.
    let spans = std::fs::read_to_string(fake.path("SPAN")).expect("the probe spans");
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    for line in spans.lines() {
        let mut it = line.split_whitespace();
        let kind = it.next().unwrap_or("");
        let at: f64 = it.next().unwrap_or("0").parse().unwrap_or(0.0);
        match kind {
            "START" => starts.push(at),
            "END" => ends.push(at),
            _ => {}
        }
    }
    assert_eq!(starts.len(), 2, "two probes began: {spans}");
    assert_eq!(ends.len(), 2, "two probes ended: {spans}");
    starts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ends.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert!(
        starts[1] < ends[0],
        "the two probes overlapped in time: starts {starts:?}, ends {ends:?}\n{spans}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A host that will not answer is reported, not fatal**, and one dead host does not hold the
/// hosts that do answer.
#[test]
fn an_unreachable_host_is_reported_and_does_not_hold_the_others() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-hosts-unreachable-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    // `h1` refuses the connection the way ssh does — 255 with nothing on stdout.
    //
    // **`u1`, not `h1`**: two connection failures count towards the process-wide breaker
    // ([`letibot_tools::rpc`]), and a host name another test in this binary also uses would carry
    // that streak into it. Three failures would open the breaker and turn that test's *reachable*
    // into *not reached*, from a state no line of it mentions.
    let fake = Fake::new(
        "unreachable",
        "case \"$*\" in *u1.invalid*) echo 'ssh: connect to host u1.invalid port 22: Connection refused' >&2; exit 255 ;; esac\n\
         exec bash -lc \"$last\"",
    );
    let paths = Paths::in_home(dir.join("home"));
    std::fs::create_dir_all(paths.keys()).expect("the keys directory");
    for name in ["u1", "u2"] {
        std::fs::write(paths.default_key(name), b"a dedicated key").expect("a key");
    }
    std::fs::write(
        paths.known_hosts(),
        "u1.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n\
         u2.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n",
    )
    .expect("the host keys");
    let file = dir.join("hosts.toml");
    std::fs::write(
        &file,
        "[host.u1]\nhostname = \"u1.invalid\"\nuser = \"tester\"\nmise = false\n\n\
         [host.u2]\nhostname = \"u2.invalid\"\nuser = \"tester\"\nmise = false\n",
    )
    .expect("a hosts file");
    let tool = HostsTool::new()
        .with_file(&file)
        .with_paths(paths)
        .with_ssh(&fake.program)
        .with_bounds(Duration::from_secs(10), Duration::from_secs(8));
    let said = tool.answer(&serde_json::json!({"probe": true})).payload;
    assert_eq!(said.matches("REACHABLE").count(), 1, "{said}");
    assert!(said.contains("NOT REACHED"), "{said}");
    assert!(
        said.contains("lost the connection") && said.contains("u1.invalid"),
        "the unreachable host is reported with what ssh said: {said}"
    );
    assert!(
        said.contains("calls:    last "),
        "and with the record of what its calls have been doing — which is the host whose history \
         a reader most needs: {said}"
    );
    assert!(
        said.contains("`u2`") && said.contains("cores:"),
        "the host that answers is still reported in full: {said}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A host whose key is missing is refused before anything connects** — and the answer carries the
/// two commands that make the key.
#[test]
fn a_host_with_no_key_is_refused_before_anything_connects() {
    let dir = std::env::temp_dir().join(format!(
        "letibot-hosts-nokey-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let fake = Fake::new("nokey", "exec bash -lc \"$last\"");
    let paths = Paths::in_home(dir.join("home"));
    std::fs::create_dir_all(paths.keys()).expect("the keys directory");
    // Only h2 has a key.
    std::fs::write(paths.default_key("h2"), b"a dedicated key").expect("a key");
    std::fs::write(
        paths.known_hosts(),
        "h1.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n\
         h2.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n",
    )
    .expect("the host keys");
    let file = dir.join("hosts.toml");
    std::fs::write(
        &file,
        "[host.h1]\nhostname = \"h1.invalid\"\nuser = \"tester\"\nmise = false\n\n\
         [host.h2]\nhostname = \"h2.invalid\"\nuser = \"tester\"\nmise = false\n",
    )
    .expect("a hosts file");
    let tool = HostsTool::new()
        .with_file(&file)
        .with_paths(paths)
        .with_ssh(&fake.program)
        .with_bounds(Duration::from_secs(10), Duration::from_secs(8));
    let said = tool.answer(&serde_json::json!({"probe": true})).payload;
    assert!(said.contains("NOT REACHED"), "{said}");
    assert!(
        said.contains("needs the key") && said.contains("ssh-keygen"),
        "the refusal names the missing file and the commands that make it: {said}"
    );
    assert_eq!(
        fake.count("h1.invalid"),
        0,
        "nothing connected to the host with no key: {:#?}",
        fake.calls()
    );
    assert!(
        fake.count("h2.invalid") >= 1,
        "the host that could be reached was still probed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// The RPC practices — rules 5 to 8, and what ssh's two refusals mean
// ---------------------------------------------------------------------------
//
// `docs/remote-hosts.md`: *"a remote is an RPC, and the client follows the practices"*. Rules 1–4
// are above (deadlines, failure detection, retries by idempotency, an unknown outcome said as
// unknown); these are the rest, and each one is measured against the same fake ssh.

/// **The spec these tests place with** — this fake ssh, this host, three seconds on every call
/// that is not a command.
fn place_spec_for_test(program: &Path, repo: &Path, paths: &Paths, host: Host) -> SshSpec {
    // **The two facts a connection is refused without**, for THIS host's name: the breaker and
    // back-pressure tests use a name of their own, so `repo_and_paths`'s `fake` material would not
    // open them.
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
        ssh: Some(program.to_path_buf()),
        paths: Some(paths.clone()),
        bound: Some(Duration::from_secs(3)),
        egress: Egress::Denied,
        confine: Some(false),
    }
}

/// **Rule 5: three connection failures in a row and the host is not dialled any more.**
///
/// The measured shape of the defect this replaces: a host that has gone away costs every call its
/// own timeout, one after another, and a session that keeps asking pays for each. Here the third
/// failure opens the breaker, the fourth call fails **at once** with a sentence naming the reason,
/// the time the host was last reached and when the next probe is, and the fake's own log proves no
/// ssh was spawned for it.
#[test]
fn three_failed_calls_open_the_breaker_and_the_next_fails_at_once() {
    // A unique host name: the breaker is process-wide, and a name another test also uses would
    // carry its streak (and its opened state) into that test.
    let host = format!("breaker-{}", std::process::id());
    let fake = Fake::new(
        "breaker",
        "if [ -f @DIR@/down ]; then echo 'ssh: connect to host nowhere port 22: Operation timed out' >&2; exit 255; fi\n\
         exec bash -lc \"$last\"",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&place_spec_for_test(
        &fake.program,
        &repo,
        &paths,
        host_named(&host),
    ))
    .expect("placed");

    // The host goes away, and every call from here is a connection failure.
    std::fs::write(fake.path("down"), b"gone").expect("the marker");

    // A step of this backend's own work: two attempts, then the third call opens the breaker.
    for _ in 0..2 {
        let _ = b
            .read("README")
            .expect_err("a dropped connection is a refusal");
    }
    let before = fake.calls().len();
    assert!(before >= 3, "the failures really were attempted: {before}");

    let began = Instant::now();
    let refused = b.read("README").expect_err("an open breaker refuses");
    let took = began.elapsed();
    let said = refused.to_string();
    assert!(
        took < Duration::from_millis(500),
        "a call to an unavailable host must not wait: {took:?}"
    );
    assert!(said.contains(&host), "the refusal names the host: {said}");
    assert!(
        said.contains("is unavailable") && said.contains("connection failures"),
        "the refusal says why: {said}"
    );
    assert!(
        said.contains("last reached"),
        "and when it was last reached: {said}"
    );
    assert!(
        said.contains("probe"),
        "and that one probe will decide whether it is back: {said}"
    );
    assert_eq!(
        fake.calls().len(),
        before,
        "nothing was dialled for the refused call: {:#?}",
        fake.calls()
    );

    // **Rule 7's record is how a report sees the same thing.**
    let report = letibot_tools::rpc::report(&host);
    assert!(report.contains("dropped"), "{report}");
    let _ = std::fs::remove_dir_all(&fake.dir);
}

/// **Rule 6: a command that has to be abandoned takes its remote process group with it.**
///
/// The fake ssh *is* the remote here, so a `sleep 300 &` under it is a process on this box in a
/// session of its own — which is exactly the shape a real command has on a real host. The command
/// is wrapped so its group is written down, the call is killed at its bound, and the cancel reaches
/// that group: the child that would have outlived the ssh is gone.
#[test]
fn a_cancelled_command_leaves_nothing_running_on_the_far_side() {
    let host = format!("cancel-{}", std::process::id());
    let fake = Fake::new("cancel", "exec bash -lc \"$last\"");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    // A short bound on the connection: `run` keeps it, so the command is abandoned in a second
    // rather than in twenty.
    let b = SshBackend::place(&place_spec_for_test(
        &fake.program,
        &repo,
        &paths,
        host_named(&host),
    ))
    .expect("placed");
    let pidfile = fake.path("child-pid");
    let refused = b
        .run(&Command {
            // The whole point: a child that is NOT in the ssh's own process group, so killing the
            // local ssh does not reach it. Only the recorded group does.
            argv: vec![
                "sh".into(),
                "-c".into(),
                format!("sleep 300 & echo $! > {}; wait", pidfile.display()),
            ],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect_err("a command that outlives its bound is abandoned");
    let said = refused.to_string();
    assert!(
        said.contains("may have run, partly run, or still be running"),
        "the outcome is reported as unknown: {said}"
    );

    let child: u32 = std::fs::read_to_string(&pidfile)
        .expect("the command started and wrote its child's pid")
        .trim()
        .parse()
        .expect("a pid");
    // **The cancel is the whole proof, and the group is why.** The `setsid` wrapper put the command
    // in a session of its own, so the kill that ended the local ssh cannot have reached this child;
    // nothing but the recorded group id could. And the run only returned after
    // [`Ssh::cancel_group`] signalled it, so the child is gone by now.
    let mut gone = !alive(child);
    for _ in 0..40 {
        if gone {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        gone = !alive(child);
    }
    // A last-resort cleanup, so a failing assertion does not leave a `sleep 300` behind.
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &child.to_string()])
        .output();
    assert!(
        gone,
        "the cancelled command's group was ended on the far side"
    );
    // And the file that recorded it is gone, so nothing can signal a stale id later.
    assert!(
        !b.worktree_path().is_empty() && !pidfile_is_stale(&b),
        "the group file is removed when the group is ended"
    );
    let _ = std::fs::remove_dir_all(&fake.dir);
}

/// **A job's wait is a heartbeat, and it does not fill the record ring.**
///
/// A status poll is one ssh call every 120 ms for as long as a job is waited on, so a single
/// `job_wait` of half a minute is [`letibot_tools::rpc::RECORD_RING`] entries — and it would evict
/// every call a reader could act on. A poll that *succeeds* says only *the machine is still
/// there*, which is the one thing a report does not need; a poll that fails is still recorded.
#[test]
fn a_jobs_polling_does_not_fill_the_record_ring() {
    let host = format!("polls-{}", std::process::id());
    let fake = Fake::new("polls", "exec bash -lc \"$last\"");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&place_spec_for_test(
        &fake.program,
        &repo,
        &paths,
        host_named(&host),
    ))
    .expect("placed");
    let processes = b.processes_arc().expect("this backend starts processes");
    let id = processes
        .spawn(&SpawnRequest {
            command: "sleep 1".into(),
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
    // A second and a half of waiting: about a dozen polls, plus the launch's own pid poll.
    let _ = processes.wait_job(&id, Duration::from_secs(20));

    let mine = letibot_tools::rpc::recent(&host, letibot_tools::rpc::RECORD_RING);
    let polls = mine
        .iter()
        .filter(|r| r.step == "asking about a job")
        .count();
    assert_eq!(
        polls, 0,
        "a successful poll is a heartbeat and is not written down"
    );
    // …and the count is what a launch plus a wait really was — under a hundred — rather than the
    // hundreds a recorded heartbeat would have added. Only the count is asserted: the ring is
    // shared by every test in this binary, so *which* records survive is another host's traffic's
    // business, and a presence assertion here would be a flake dressed as a measurement.
    assert!(
        mine.len() < 100,
        "the polls did not put a single extra entry in: {}",
        mine.len()
    );
    let _ = std::fs::remove_dir_all(&fake.dir);
}

/// Is there a run-group file left in the checkout? The cancel deletes the one it used.
fn pidfile_is_stale(b: &letibot_tools::remote::SshBackend) -> bool {
    let dir = Path::new(b.worktree_path()).join(".letibot-remote");
    std::fs::read_dir(&dir)
        .map(|d| {
            d.flatten()
                .any(|e| e.path().extension().is_some_and(|x| x == "pgid"))
        })
        .unwrap_or(false)
}

/// **A host whose mise insists on a directory that exists** — which is what a real mise does, and
/// what the placement did not do: the wrapper was pointed at the checkout before `git worktree add`
/// had made it, so on every machine whose toolchain comes from mise the placement died at `making
/// the mirror` with `mise ERROR Directory specified with --cd does not exist: …` (measured on the
/// orchestrator's gate, 2026-10-26 — and mise prints the path with `$HOME` abbreviated to `~`,
/// which makes it read like a quoting bug and is not one).
///
/// The stub here is the real behaviour in eight lines: refuse a `-C` that is not a directory, record
/// every `-C` it was given, then run the command. It is `$HOME/.local/bin/mise` in the fake remote's
/// own home, so the **default** probe finds it — no `mise` line in `hosts.toml` — and every step of a
/// placement goes through it.
#[test]
fn a_placement_through_a_mise_that_insists_its_directory_exists() {
    let host = format!("mise-real-{}", std::process::id());
    let fake = Fake::new("misereal", "exec bash -lc \"$last\"");
    let mise = fake.dir.join("home/.local/bin/mise");
    std::fs::create_dir_all(mise.parent().expect("a bin")).expect("the mise directory");
    let seen = fake.path("cds");
    write_executable(
        &mise,
        &format!(
            "#!/bin/bash\n\
             # exec -C <dir> -- <cmd...>\n\
             if [ \"$1\" != exec ] || [ \"$2\" != -C ]; then echo 'mise: unexpected argv' >&2; exit 2; fi\n\
             d=\"$3\"\n\
             printf '%s\\n' \"$d\" >> '{seen}'\n\
             if [ ! -d \"$d\" ]; then\n\
               echo \"mise ERROR Directory specified with --cd does not exist: $d\" >&2\n\
               exit 1\n\
             fi\n\
             shift 4\n\
             exec \"$@\"\n",
            seen = seen.display()
        ),
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    // **No `mise` line**: the default is what a real `hosts.toml` gets, and the probe finds this
    // stub at `$HOME/.local/bin/mise` — the operator's own layout.
    let b = SshBackend::place(&place_spec_for_test(
        &fake.program,
        &repo,
        &paths,
        Host {
            mise: None,
            ..host_named(&host)
        },
    ))
    .expect("the placement is arranged through a mise that checks its directory");

    let worktree = b.worktree_path();
    let cds = std::fs::read_to_string(&seen).expect("the stub recorded every -C");
    let dirs: Vec<&str> = cds.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        !dirs.is_empty(),
        "every step of the placement went through mise: {cds}"
    );
    // **Every `-C` was a path that exists on the remote** — which the stub itself enforced, and
    // which is asserted here so a future change that stops using the stub still fails this test.
    for d in &dirs {
        assert!(
            d.starts_with('/') && Path::new(d).is_dir(),
            "`-C {d}` is an absolute directory that exists: {dirs:?}"
        );
    }
    // The wrapper is pointed at the ROOT until the checkout exists, and at the checkout after.
    assert!(
        dirs.iter()
            .all(|d| !d.ends_with("01abcdef") || Path::new(d).is_dir()),
        "the checkout is only named once it is there: {dirs:?}"
    );
    assert!(
        dirs.contains(&worktree),
        "and the work that follows the checkout runs IN it: {dirs:?}"
    );

    // It is not only the placement: a tool call goes through the same wrapper too, and it runs in a
    // directory that exists.
    let _ = b.read("README").expect("the read lands");
    b.write("docs/kept.txt", b"bytes").expect("the write lands");
    let cds = std::fs::read_to_string(&seen).expect("the stub recorded every -C");
    assert!(
        cds.lines().all(|d| Path::new(d).is_dir()),
        "every `-C` after the placement exists too: {cds}"
    );
    let _ = std::fs::remove_dir_all(&fake.dir);
}

/// **Rule 7: every call is recorded, and the report says what it did.**
///
/// One host, three shapes of call: a read that worked, a command that worked, and a command that
/// dropped. The records carry the host, the step, the duration and the outcome — and the report is
/// the row `hosts {probe: true}` shows.
#[test]
fn every_call_is_recorded_and_the_report_names_host_step_duration_and_outcome() {
    let host = format!("records-{}", std::process::id());
    let fake = Fake::new(
        "records",
        "case \"$last\" in *drops-now*) echo 'ssh: connection reset by peer' >&2; exit 255 ;; esac\n\
         exec bash -lc \"$last\"",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&place_spec_for_test(
        &fake.program,
        &repo,
        &paths,
        host_named(&host),
    ))
    .expect("placed");

    let _ = b.read("README").expect("the read lands");
    let _ = b
        .run(&Command {
            argv: vec!["true".into()],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect("the command runs");
    let _ = b
        .run(&Command {
            argv: vec!["echo".into(), "drops-now".into()],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect_err("this one drops");

    let records = letibot_tools::rpc::recent(&host, 8);
    let steps: Vec<(&str, &str)> = records
        .iter()
        .map(|r| (r.step.as_str(), r.outcome.as_str()))
        .collect();
    assert!(
        steps
            .iter()
            .any(|(s, o)| s.starts_with("reading") && *o == "ok"),
        "the read is recorded as ok: {steps:?}"
    );
    assert!(
        steps
            .iter()
            .any(|(s, o)| *s == "this command ran" && *o == "ok"),
        "the command is recorded as ok: {steps:?}"
    );
    assert!(
        steps
            .iter()
            .any(|(s, o)| *s == "this command ran" && *o == "dropped"),
        "the dropped command is recorded as dropped: {steps:?}"
    );
    // No payload: a record is a name, a time and a verdict, and a `bash` body is none of them.
    assert!(
        !records.iter().any(|r| r.step.contains("drops-now")),
        "a record must not carry the command: {records:#?}"
    );
    let report = letibot_tools::rpc::report(&host);
    assert!(report.contains("last "), "{report}");
    assert!(report.contains("slowest"), "{report}");
    assert!(report.contains("dropped"), "{report}");
    let _ = std::fs::remove_dir_all(&fake.dir);
}

/// **Rule 8: a host takes a bounded number of ssh sessions, and a call beyond it queues under its
/// own deadline.**
///
/// The fake holds each call for 300 ms and counts how many are inside at once. Sixteen steps are
/// sent at once against a host whose bound is
/// [`letibot_tools::rpc::MAX_SESSIONS_PER_HOST`]; the most that may ever be in flight is that bound
/// (a client that opened sixteen would have sshd refuse the eleventh, which reads as a fault on
/// the host), and every call still answers inside its own deadline.
#[test]
fn concurrent_calls_to_one_host_queue_at_the_session_bound() {
    let host = format!("busy-{}", std::process::id());
    let fake = Fake::new(
        "busy",
        "n=$(cat @DIR@/live 2>/dev/null || echo 0); n=$((n+1)); echo $n > @DIR@/live\n\
         m=$(cat @DIR@/most 2>/dev/null || echo 0); if [ \"$n\" -gt \"$m\" ]; then echo $n > @DIR@/most; fi\n\
         sleep 0.3\n\
         n=$(cat @DIR@/live); echo $((n-1)) > @DIR@/live\n\
         exec bash -lc \"$last\"",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = std::sync::Arc::new(
        SshBackend::place(&place_spec_for_test(
            &fake.program,
            &repo,
            &paths,
            host_named(&host),
        ))
        .expect("placed"),
    );

    let mut threads = Vec::new();
    for _ in 0..(letibot_tools::rpc::MAX_SESSIONS_PER_HOST * 2) {
        let b = b.clone();
        threads.push(std::thread::spawn(move || b.stat("README").is_some()));
    }
    let answered = threads
        .into_iter()
        .filter(|_| true)
        .map(|t| t.join().expect("the thread ran"))
        .filter(|hit| *hit)
        .count();
    assert_eq!(
        answered,
        letibot_tools::rpc::MAX_SESSIONS_PER_HOST * 2,
        "every call answered inside its own deadline"
    );
    let most: usize = std::fs::read_to_string(fake.path("most"))
        .expect("the fake counted")
        .trim()
        .parse()
        .expect("a number");
    assert!(
        most <= letibot_tools::rpc::MAX_SESSIONS_PER_HOST,
        "{most} ssh calls were in flight at once against a bound of {}",
        letibot_tools::rpc::MAX_SESSIONS_PER_HOST
    );
    let _ = std::fs::remove_dir_all(&fake.dir);
}

/// **An auth refusal is definite: said in those words, never retried, exactly one invocation.**
///
/// Measured live on 2026-10-26: three hosts whose key was not installed answered `Permission denied
/// (publickey)`, and the client called it *lost the connection, it may not have happened* and asked
/// **twice**. The machine answered; the key is not there; nothing about a second offer changes it.
#[test]
fn an_auth_refusal_is_definite_and_is_never_asked_twice() {
    let host = format!("auth-{}", std::process::id());
    let fake = Fake::new(
        "auth",
        "case \"$last\" in *plainly-refused*) \
           echo 'tester@nowhere: Permission denied (publickey).' >&2; exit 255 ;; esac\n\
         exec bash -lc \"$last\"",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&place_spec_for_test(
        &fake.program,
        &repo,
        &paths,
        host_named(&host),
    ))
    .expect("placed");

    let refused = b
        .read("plainly-refused")
        .expect_err("a refused key is not a result");
    let said = refused.to_string();
    assert!(
        said.contains("key is not authorised") && said.contains("host key changed"),
        "the refusal is the one the contract names: {said}"
    );
    assert!(said.contains("ssh-copy-id"), "and it names the fix: {said}");
    assert!(
        !said.contains("lost the connection") && !said.contains("may not have happened"),
        "a refusal is not a lost connection: {said}"
    );
    assert_eq!(
        fake.count("plainly-refused"),
        1,
        "an idempotent step would normally be retried; a refusal never is: {:#?}",
        fake.calls()
    );
    // The breaker is not opened by it: the host is up and will keep answering until a key is
    // installed.
    assert_eq!(
        letibot_tools::rpc::standing(&host),
        letibot_tools::rpc::Standing::Closed,
        "a refusal does not make a host unavailable"
    );
    let _ = std::fs::remove_dir_all(&fake.dir);
}

/// **A host key that does not verify is the other definite refusal, and its fix is the other file.**
#[test]
fn a_host_key_mismatch_is_refused_with_the_scan_command() {
    let host = format!("hostkey-{}", std::process::id());
    let fake = Fake::new(
        "hostkey",
        "if [ -f @DIR@/mismatch ]; then\n\
           echo 'No ED25519 host key is known for nowhere and you have requested strict checking.' >&2\n\
           echo 'Host key verification failed.' >&2\n\
           exit 255\n\
         fi\n\
         exec bash -lc \"$last\"",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&place_spec_for_test(
        &fake.program,
        &repo,
        &paths,
        host_named(&host),
    ))
    .expect("placed");
    std::fs::write(fake.path("mismatch"), b"changed").expect("the marker");
    let said = b.read("README").expect_err("it is refused").to_string();
    assert!(said.contains("host key changed"), "{said}");
    assert!(said.contains("ssh-keyscan"), "the fix is a scan: {said}");
    let _ = std::fs::remove_dir_all(&fake.dir);
}

/// **A script that writes `$HOME/.local/bin/mise` is the host's mise**, and every remote command
/// goes through `<mise> exec -C <the checkout> -- bash -lc '<cmd>'`.
///
/// The default (no `mise` line in `hosts.toml`) is *probe `~/.local/bin/mise`, then `PATH`, once,
/// and cache the answer*: the fake's home has one, so the probe finds it — and the wrapper is
/// asserted on the argv the next command was handed, not on a field.
#[test]
fn the_default_probe_finds_mise_and_every_command_runs_through_it() {
    let host = format!("mise-{}", std::process::id());
    let fake = Fake::new("mise", "exec bash -lc \"$last\"");
    let mise = fake.dir.join("home/.local/bin/mise");
    std::fs::create_dir_all(mise.parent().expect("a bin")).expect("the mise directory");
    write_executable(
        &mise,
        &format!(
            "#!/bin/bash\n\
             printf 'MISE %s\\n' \"$*\" >> '{}'\n\
             shift 4\n\
             exec \"$@\"\n",
            fake.path("mise.log").display()
        ),
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&place_spec_for_test(
        &fake.program,
        &repo,
        &paths,
        Host {
            mise: None,
            ..host_named(&host)
        },
    ))
    .expect("placed");

    let log = std::fs::read_to_string(fake.path("mise.log")).expect("mise was run");
    let worktree = b.worktree_path();
    assert!(
        log.lines()
            .any(|l| l.starts_with("MISE exec -C ") && l.contains(worktree)),
        "the wrapper names `mise exec -C <the checkout>`: {log}"
    );
    // **A multi-line script makes one MISE line span several lines**, so this asks the lines mise
    // actually began: every one of them wraps a login shell.
    assert!(
        !log.is_empty()
            && log
                .lines()
                .filter(|l| l.starts_with("MISE "))
                .all(|l| l.starts_with("MISE exec -C ") && l.contains("-- bash -lc ")),
        "every wrapped command is `mise exec -C <dir> -- bash -lc <script>`: {log}"
    );
    // The calls the placement made BEFORE the checkout existed ran in `$HOME` — the honest answer
    // for a directory that does not exist yet. The argv is where that is visible: the shell on the
    // far side expands it, so the fake's log shows a path and the ssh call shows `"$HOME"`.
    assert!(
        fake.calls()
            .iter()
            .any(|c| c.contains("exec -C \"$HOME\" -- bash -lc ")),
        "a command before the checkout was named ran in the remote's home: {:#?}",
        fake.calls()
    );
    // The probe itself is asked once and cached: one `finding mise` record, not one per call.
    let probes = letibot_tools::rpc::recent(&host, 256)
        .iter()
        .filter(|r| r.step == "finding mise")
        .count();
    assert_eq!(
        probes, 1,
        "the answer is probed once per process, not per call"
    );
    let _ = std::fs::remove_dir_all(&fake.dir);
}

/// **`mise = false` turns the wrapper off**, and a host that names a program uses that program
/// with its `~` expanded **there**.
#[test]
fn mise_false_is_no_wrapper_and_mise_at_a_path_is_that_path() {
    // Off.
    let host = format!("nomise-{}", std::process::id());
    let fake = Fake::new("nomise", "exec bash -lc \"$last\"");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&place_spec_for_test(
        &fake.program,
        &repo,
        &paths,
        host_named(&host),
    ))
    .expect("placed");
    let _ = b.read("README").expect("the read lands");
    assert!(
        !fake.calls().iter().any(|c| c.contains(" exec -C ")),
        "`mise = false` means no wrapper and no probe: {:#?}",
        fake.calls()
    );

    // A path, spelled with a `~` that is the REMOTE's home.
    let host = format!("miseat-{}", std::process::id());
    let fake = Fake::new("miseat", "exec bash -lc \"$last\"");
    let mise = fake.dir.join("home/.local/bin/mise");
    std::fs::create_dir_all(mise.parent().expect("a bin")).expect("the mise directory");
    write_executable(
        &mise,
        &format!(
            "#!/bin/bash\n\
             printf 'MISE %s\\n' \"$*\" >> '{}'\n\
             shift 4\n\
             exec \"$@\"\n",
            fake.path("mise.log").display()
        ),
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&place_spec_for_test(
        &fake.program,
        &repo,
        &paths,
        Host {
            mise: Some(Mise::At("~/.local/bin/mise".into())),
            ..host_named(&host)
        },
    ))
    .expect("placed");
    let _ = b.read("README").expect("the read lands");
    let log = std::fs::read_to_string(fake.path("mise.log")).expect("mise was run");
    assert!(
        log.lines().any(|l| l.starts_with("MISE exec -C ")),
        "a named program is the wrapper: {log}"
    );
    // **`~` is expanded on the remote**: the argv says `"$HOME/.local/bin/mise"`, and the shell
    // there turns it into the fake home's copy — which is the file that wrote this log.
    assert!(
        fake.calls()
            .iter()
            .any(|c| c.contains("\"$HOME/.local/bin/mise\" exec -C ")),
        "the program is spelled with the REMOTE's home: {:#?}",
        fake.calls()
    );
    let _ = std::fs::remove_dir_all(&fake.dir);
}
