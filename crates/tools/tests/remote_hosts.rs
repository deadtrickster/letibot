//! Remote placement against a fake ssh: the commands, the base64 channel, the `setsid` launch, the job
//! files and the `git bundle` round trip are the production ones, and no network is touched because the
//! fake runs the last argument locally against a temporary home.
//!
//! `setsid` does not exist on Darwin, so the fake remote gets a perl shim: a job there must lead its own
//! process group, which is what the group signal depends on.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_tools::backend::{BackendError, Command, ExecBackend};
use letibot_tools::builtins::hosts::HostsTool;
use letibot_tools::exec::{Egress, JobId, JobState, ScopeKind, SpawnRequest, Waited};
use letibot_tools::remote::{Host, Hosts, Mise, Paths, SshBackend, SshSpec};

/// The short id this suite's session gets: the last eight of `s-01abcdef`.
const SHORT: &str = "01abcdef";

/// A name no other fixture in this process holds. The gate and the breaker are per process and
/// keyed by a host's name, so two fakes under one name are one machine to both - and every fixture
/// here stands for a machine of its own.
fn fixture_name(tag: &str) -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!(
        "fake-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// One fake remote: a temporary home, a repository to send, and an `ssh` that runs
/// commands against the first.
struct Rig {
    dir: PathBuf,
    /// Where the checkout lands on the "remote", under its `~/letibot-remote` root.
    worktree: PathBuf,
    /// The jobs' own directory, on the "remote".
    jobs: PathBuf,
    repo: PathBuf,
    ssh: PathBuf,
    /// What the fake ssh was handed, one line per call: its argv and its environment.
    log: PathBuf,
    /// letibot's own ssh material for this test, a key and an accepted host key: a connection
    /// is refused without both, which is the point of it.
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

        // A `setsid` for the boxes that have none, so a job on the fake remote leads its own
        // process group and `kill -TERM -<pid>` cannot reach this test's group. Written only
        // where the box has none, since the shim directory is first on the fake's PATH.
        if !on_path("setsid") {
            write_executable(
                &bin.join("setsid"),
                "#!/usr/bin/env perl\nuse POSIX ();\nPOSIX::setsid();\nexec { $ARGV[0] } @ARGV;\n",
            );
        }

        // The fake records its argv and its environment and is otherwise a passthrough, since the flags
        // under test are the ones it is HANDED and a fake that understood them would be testing itself.
        // It can also be told to HOLD one call, which makes a race between two placements deterministic.
        let ssh = bin.join("ssh");
        let log = dir.join("ssh.log");
        write_executable(
            &ssh,
            &format!(
                "#!/bin/bash\n\
                 [ $# -eq 0 ] && exit 0\n\
                 {{ printf 'ARGV %s\\n' \"$*\"; env | sed 's/^/ENV /'; }} >> '{}'\n\
                 export PATH='{}':$PATH\n\
                 export HOME='{}'\n\
                 for last; do :; done\n\
                 h='{hold}'\n\
                 if [ -f \"$h\" ]; then\n\
                 \x20 needle=$(sed -n 1p \"$h\")\n\
                 \x20 release=$(sed -n 2p \"$h\")\n\
                 \x20 case \"$last\" in *\"$needle\"*)\n\
                 \x20   : > '{hold}.entered'\n\
                 \x20   n=0\n\
                 \x20   while [ ! -f \"$release\" ]; do n=$((n+1)); [ $n -gt 600 ] && break; sleep 0.05; done ;;\n\
                 \x20 esac\n\
                 fi\n\
                 exec bash -lc \"$last\"\n",
                log.display(),
                bin.display(),
                home.display(),
                hold = dir.join("hold").display()
            ),
        );
        // letibot's own key and known host, in a config directory of this test's own: the two
        // facts a connection is refused without, and neither is the operator's.
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

        let worktree = home.join("letibot-remote").join("the-repo").join(SHORT);
        let jobs = worktree.join(".letibot-remote").join("jobs");
        let host = Host {
            name: "fake".into(),
            hostname: "fake.invalid".into(),
            user: Some("tester".into()),
            key: None,
            path: Vec::new(),
            root: "~/letibot-remote".into(),
            // A fixture's host has no mise, or the default probe would find this box's own and
            // wrap every command in it, making one assertion mean two things on two boxes.
            mise: Some(Mise::Off),
            // No reservation and no never-touch list: a fixture's host carries no owner's
            // workloads, and a `flags` list would be a fact about a machine this is not.
            ..Host::default()
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

    /// One spec, with this rig's fake ssh and its own ssh material. The bound is short on purpose: the
    /// fake answers in milliseconds, and a fake meant to hang is measured in a second, not the production
    /// bound.
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
            // This fixture's host confines nothing, and says so: the probe would run on the
            // machine behind the fake, so an unpinned answer would mean two things on two boxes.
            egress: Egress::Denied,
            confine: Some(false),
            vm: false,
        }
    }

    /// Every ssh call this rig made, as `<argv>` and `<env>` lines.
    fn ssh_log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// The backend, and the worktree it says it got. The path is taken from the backend rather
    /// than built here, because the remote resolves its root with `pwd -P` and a symlinked
    /// `/var` would make the two spellings differ for a reason that is not the placement.
    fn place(&mut self) -> SshBackend {
        let b = SshBackend::place(&self.spec()).expect("the placement is arranged");
        self.worktree = PathBuf::from(b.worktree_path());
        self.jobs = self.worktree.join(".letibot-remote").join("jobs");
        b
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // Only the pids this rig wrote down, never a pattern: a job left running by a panic
        // would otherwise keep writing into a tree this drop removes.
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

/// A fake program on the rig's own `PATH`. [`letibot_tools::testing::write_stub`] writes it,
/// so a fork this suite's own session starts cannot leave the file unexecutable.
fn write_executable(path: &Path, text: &str) {
    letibot_tools::testing::write_stub(path, text);
}

/// Is this program on the box's own `PATH`? A shim that shadowed a real `setsid` would test a
/// weaker thing than production runs.
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

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The flags are handed to ssh, and nothing in the call names the operator's ssh material:
/// neither the argv nor the environment may carry a path into the operator's ssh directory,
/// because a `HOME` or an `SSH_*` pointing there hands ssh what the flags refuse.
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
        format!("-i {}", rig.paths.default_key(&rig.host.name).display()),
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

/// Reading and writing a file over ssh, byte for byte, and atomically: a channel that mangles
/// an image or a GGUF writes files nobody can open, and [`ExecBackend::write`] requires that a
/// write which cannot complete leaves the old file alone.
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

    // NUL, invalid UTF-8, every byte: the three things a text channel gets wrong.
    let bytes: Vec<u8> = (0u8..=255).chain([0, 0, 0xff, 0x80]).collect();
    b.write("docs/blob.bin", &bytes).expect("the write lands");
    assert_eq!(b.read("docs/blob.bin").expect("the read"), bytes);

    // A directory is not a file, and a missing path is a miss rather than an I/O error.
    assert!(matches!(b.read("docs"), Err(BackendError::IsADirectory(_))));
    assert!(matches!(
        b.read("docs/nothing-here"),
        Err(BackendError::NotFound(_))
    ));

    // Atomicity, measured: with the directory unwritable the rename cannot happen, and the
    // file that was there is still there, whole.
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

/// A command runs on the far side, in the checkout, and a `cwd` is relative to it.
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
    // An environment addition reaches the command, which is what `build_env` and the daemon's
    // own socket variable ride on.
    let out = b
        .run(&Command {
            argv: vec!["sh".into(), "-c".into(), "echo $A_TEST_VARIABLE".into()],
            cwd: "sub".into(),
            env: vec![("A_TEST_VARIABLE".into(), "a value".into())],
        })
        .expect("it runs");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "a value");

    // The host is named where a person looks: the card's sentence and the description a
    // session shows.
    let label = format!("{} over ssh", rig.host.name);
    assert_eq!(b.host_label().as_deref(), Some(label.as_str()));
    let described = b.describe();
    assert!(
        described.contains(&format!("`{}`", rig.host.name)),
        "{described}"
    );
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

/// `list` and `stat`, and the tree in the order a walk would have produced it.
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

/// A background job outlives the ssh call that started it, is read through its files, and is
/// killed by its own process group. The pid is what proves it outlived the connection: the
/// ssh that started it has returned and the job's own process is alive.
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

    // Running, with the ssh call that made it already returned.
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

    // Its output is a file on that machine, stderr included: ssh carries one channel, so the
    // job keeps one file.
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

    // Killed by its group, which is the job's own: the launch used `setsid`, so the group
    // signal cannot reach this test's own process group.
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

    // A scope end kills what is in it, and the record says nothing survived.
    let ended = host.end_scope(&scope).expect("a scope end is recorded");
    assert!(ended.observed.is_empty(), "the job was already ended");
}

/// A job that exits settles from its exit file, and the status is the command's own.
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

/// The tree: out as a bundle, back as a branch. The local `HEAD` arrives as a checkout on
/// `remote/<short>`, a commit made there comes back as `remote/<host>/<short>`, the branch the
/// work started from is untouched, and an uncommitted edit is named as not carried.
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

    // A commit made remotely: the same git, run where the session would run it.
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
    let came_back = format!("remote/{}/{SHORT}", rig.host.name);
    assert!(said.contains(&came_back), "{said}");
    assert!(
        said.contains("uncommitted change"),
        "a dirty remote tree is not in the branch, and the report says so: {said}"
    );
    assert_eq!(
        git(&rig.repo, &["rev-parse", &came_back]).trim(),
        remote_head,
        "the branch came back at the commit that was made there"
    );
    // The local branch is where it was: the queue, not the remote, moves it.
    assert_eq!(
        git(&rig.repo, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "main"
    );
    assert!(
        git(&rig.repo, &["show", &format!("{came_back}:feature.rs")]).contains("the work"),
        "the commit's own contents came with it"
    );
    // Closing twice says the same thing rather than fetching again.
    assert_eq!(b.close().expect("the second close"), said);
}

/// A folder that is not a repository is refused, and the refusal names the fix.
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
        vm: false,
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

/// A mirror that exists but is not a repository is made complete, not trusted: a directory is
/// not a repository, and a placement that lost a concurrent `git init` leaves exactly that, so
/// skipping the init fails the next hand-off.
#[test]
fn a_mirror_that_is_only_a_directory_is_made_a_repository() {
    let rig = Rig::new("halfmirror");
    // The state a lost `git init` leaves: the directory, no repository in it.
    let mirror = rig
        .dir
        .join("home")
        .join("letibot-remote")
        .join("the-repo.git");
    std::fs::create_dir_all(&mirror).expect("a directory where a mirror belongs");

    let b = SshBackend::place(&rig.spec()).expect("the placement is arranged");
    assert!(
        mirror.join("objects").is_dir(),
        "the mirror was completed: {}",
        mirror.display()
    );
    assert!(
        std::path::Path::new(b.worktree_path())
            .join("README")
            .is_file(),
        "and the checkout is there: {}",
        b.worktree_path()
    );
}

/// Two placements at once never read each other's bundle. One name per mirror would not fail loudly:
/// `git fetch -f <bundle> +HEAD:refs/heads/<mine>` builds the ref from whatever bundle is there, so a
/// loser is handed somebody else's commit and says nothing.
#[test]
fn two_placements_at_once_never_read_each_others_bundle() {
    let rig = Rig::new("at-once");
    let first = rig.spec();
    let start = git(&rig.repo, &["rev-parse", "HEAD"]).trim().to_string();

    // Hold the first placement at its hand-off. The needle is its own staged ref, which no
    // other command of the placement carries.
    let hold = rig.dir.join("hold");
    let release = rig.dir.join("release");
    std::fs::write(&hold, format!("letibot/{SHORT}\n{}\n", release.display())).expect("the hold");

    let handle = {
        let spec = first.clone();
        std::thread::spawn(move || SshBackend::place(&spec).map(|b| b.worktree_path().to_string()))
    };

    // Wait until it is inside the hand-off, i.e. its bundle is on the far side.
    let entered = rig.dir.join("hold.entered");
    let waited = Instant::now();
    while !entered.exists() && waited.elapsed() < Duration::from_secs(60) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        entered.exists(),
        "the first placement never reached its hand-off"
    );

    // HEAD moves while it is held, so the two placements send different trees.
    std::fs::write(rig.repo.join("later.txt"), b"later\n").expect("a file");
    git(&rig.repo, &["add", "later.txt"]);
    git(
        &rig.repo,
        &[
            "commit",
            "-q",
            "-m",
            "made while the other placement was in flight",
        ],
    );
    let later = git(&rig.repo, &["rev-parse", "HEAD"]).trim().to_string();
    assert_ne!(start, later, "the second placement sends a different tree");

    // The second placement, start to finish, with its own bundle.
    let mut second = rig.spec();
    second.name = "s-02beef00".into();
    let other = SshBackend::place(&second).expect("the second placement is arranged");
    assert_eq!(
        git(Path::new(other.worktree_path()), &["rev-parse", "HEAD"]).trim(),
        later,
        "the second placement got the tree it sent"
    );

    // Let the first one finish, and ask what it was handed.
    std::fs::write(&release, b"go\n").expect("the release");
    let mine = handle
        .join()
        .expect("the first placement thread")
        .expect("the first placement is arranged");
    assert_eq!(
        git(Path::new(&mine), &["rev-parse", "HEAD"]).trim(),
        start,
        "a placement is handed ITS OWN commit, never the one another placement sent through the \
         same file"
    );

    // And the mirror keeps no bundle afterwards, or one file per placement stays for ever.
    let mirror = rig
        .dir
        .join("home")
        .join("letibot-remote")
        .join("the-repo.git");
    let left: Vec<String> = std::fs::read_dir(&mirror)
        .expect("the mirror")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("incoming"))
        .collect();
    assert!(left.is_empty(), "no bundle is left in the mirror: {left:?}");
}

/// A name that is not in the file is refused, with the ones that are: a refusal that does not
/// name the near-miss is the one that costs an hour.
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

// Nothing hangs, and the retry follows idempotency. Every test below runs against a fake ssh written to
// misbehave in one way: it goes silent, drops the connection, or answers only the second time.

/// A fake ssh whose behaviour the test writes: it logs every invocation, sets `HOME` to the
/// "remote", and runs `body` with the last argument (the command ssh would run) in `$last`. A
/// body that sleeps is a host that stopped answering; `@DIR@` is the fake's own directory.
struct Fake {
    dir: PathBuf,
    program: PathBuf,
    log: PathBuf,
    /// The machine this fake is, unique in this process: a test that holds a gate holds the gate of
    /// the machine it named, and no other fixture's.
    name: String,
}

impl Fake {
    fn new(tag: &str, body: &str) -> Fake {
        let name = fixture_name(tag);
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
        // `[ $# -eq 0 ] && exit 0` before the log and before `body`: `write_stub` proves the file
        // by running it with no arguments, and a body here is a host's behaviour, so a `sleep
        // 300` prelude would be this fixture hanging and would add a line no test made.
        write_executable(
            &program,
            &format!(
                "#!/bin/bash\n\
                 [ $# -eq 0 ] && exit 0\n\
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
        Fake {
            dir,
            program,
            log,
            name,
        }
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("ARGV "))
            .map(str::to_string)
            .collect()
    }

    /// How many invocations carried `needle` anywhere in their argv: the count a retry changes.
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
    letibot_tools::testing::repo_config(&repo);
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
        ..Host::default()
    }
}

fn spec_for(fake: &Fake, repo: &Path, paths: &Paths, bound: Duration) -> SshSpec {
    // The two facts `Ssh::connect` refuses a connection without, for the name this fixture is: the
    // identity file a name nobody has placed with does not have yet, and the host key of a machine
    // nobody has accepted.
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
        egress: Egress::Denied,
        confine: Some(false),
        vm: false,
    }
}

/// One machine's full gate does not stop a placement on another: the queue in [`letibot_tools::rpc`]
/// is per process and keyed by a host's name, so fixtures sharing one name are starved by each other's
/// calls - a placement waits the connection's two seconds and is refused a slot it never had.
#[test]
fn a_full_gate_on_one_machine_does_not_stop_a_placement_on_another() {
    let other = Fake::new("holder", "exec bash -lc \"$last\"");
    let _held: Vec<_> = (0..letibot_tools::rpc::MAX_SESSIONS_PER_HOST)
        .map(|_| {
            letibot_tools::rpc::enter(&other.name, Duration::from_secs(5))
                .expect("a slot of its own")
        })
        .collect();

    let fake = Fake::new("placing", "exec bash -lc \"$last\"");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b = SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(2)))
        .expect("this machine's gate is its own, so the placement is not queued behind another's");
    // And it works, not merely opens: a placement that got a slot has a checkout behind it.
    assert_eq!(b.read("README").expect("it reads"), b"the first commit\n");
}

/// A host that stops answering does not hold any operation that is not a command: the fake
/// answers until a marker appears, and every call after it must come back inside its own bound
/// naming the host and the step. The bound is two seconds here, twenty in production.
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
    let b = SshBackend::place(&spec_for(&fake, &repo, &paths, bound)).expect("placed");

    // Standing in for a host that has gone quiet mid-session.
    std::fs::write(&silent, b"stop answering").expect("the marker");

    /// One operation under test: its name, and how to run it and describe what came back. The
    /// lifetime is the backend's, since every closure below borrows it.
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
    // `stat` answers `None` for a host that said nothing, because a miss and a silence are the
    // same fact to its caller, so what is asserted is that it returns inside the bound.
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

/// A job's output and a job's kill are bounded too, and a kill that could not read back says
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
    let b = SshBackend::place(&spec_for(&fake, &repo, &paths, bound)).expect("placed");
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
    // And the process on this box is really gone: the test's own cleanup, not the backend's.
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output();
}

/// A command whose connection drops is reported as an unknown outcome and is never re-sent:
/// `run` is not idempotent, so the command may have run, partly run, or still be running, and
/// the fake's log proves it was shipped once.
#[test]
fn a_command_that_loses_its_connection_is_reported_unknown_and_never_re_sent() {
    let fake = Fake::new(
        "drops",
        "case \"$last\" in *drops-here*) echo 'ssh: connection reset by peer' >&2; exit 255 ;; esac\n\
         exec bash -lc \"$last\"",
    );
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b =
        SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(5))).expect("placed");

    let refused = b
        .run(&Command {
            argv: vec!["echo".into(), "drops-here".into()],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect_err("a dropped connection is not a result");
    let said = refused.to_string();
    assert!(
        said.contains(&format!("the connection to {} dropped", fake.name))
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

/// An idempotent step that loses the connection is asked exactly once more.
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
    let b =
        SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(5))).expect("placed");

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

/// A write that loses its connection is retried as a whole, and nothing is written twice: the
/// chunk calls are never retried on their own, since a chunk sent twice appends itself twice,
/// so the second attempt rewrites the temporary file from the top.
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
    let b =
        SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(5))).expect("placed");

    // Two chunks' worth, so a duplicated chunk would be visible as extra bytes.
    let bytes: Vec<u8> = (0..(60 * 1024)).map(|i| (i % 251) as u8).collect();
    b.write("big.bin", &bytes)
        .expect("the whole write is retried and lands");
    let landed = b.read("big.bin").expect("it reads back");
    assert_eq!(landed.len(), bytes.len(), "the bytes landed exactly once");
    assert_eq!(landed, bytes);
}

/// A stale `ControlMaster` socket does not hold the next call: the socket outlives the
/// connection it belongs to, and ssh would multiplex through it and wait. A socket nobody
/// answers on is removed before the call dials afresh.
#[test]
fn a_stale_control_master_is_checked_and_dropped() {
    let fake = Fake::new("stale", "exec bash -lc \"$last\"");
    let (repo, paths) = repo_and_paths(&fake.dir.join("rig"));
    let b =
        SshBackend::place(&spec_for(&fake, &repo, &paths, Duration::from_secs(5))).expect("placed");

    let stale = letibot_tools::remote::control_path(&fake.name, std::process::id());
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

/// The session's own job directory is not part of the tree: `.letibot-remote/` appears the
/// moment a job runs, and an unexcluded one reads as uncommitted work. The checkout's own
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

    // `git` itself says so, which is the only proof that matters: the exclude file git READS
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

/// `hosts {probe: true}` asks every declared host at once, and reports each one's live facts.
/// The overlap is measured rather than assumed: each fake call records when it began and ended,
/// and two intervals that intersect cannot have run one after the other.
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
    // The facts are the machine's own, this box's, since the fake runs the script here.
    assert!(said.contains("cores:"), "{said}");
    assert!(
        !said.contains("TIMED OUT") && !said.contains("NOT REACHED"),
        "{said}"
    );
    // The record is on the row: the last calls this process made to the host, by name.
    assert!(said.contains("calls:    last "), "{said}");
    assert!(said.contains("a probe of h1"), "{said}");
    assert!(said.contains("slowest"), "{said}");

    // The overlap: both spans intersect, which one-after-another cannot produce.
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

/// A host that will not answer is reported, not fatal, and one dead host does not hold the
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
    // `u1` refuses the connection the way ssh does, 255 with nothing on stdout, and it is a name of its
    // own: two connection failures count towards the process-wide breaker, so a shared name would carry
    // that streak into another test.
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

/// A host whose key is missing is refused before anything connects, and the answer carries the
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

// The RPC practices, rules 5 to 8, and what ssh's two refusals mean. Rules 1 to 4 are above:
// deadlines, failure detection, retries by idempotency, and an unknown outcome said as unknown.

/// The spec these tests place with: this fake ssh, this host, three seconds on every call that
/// is not a command.
fn place_spec_for_test(program: &Path, repo: &Path, paths: &Paths, host: Host) -> SshSpec {
    // The two facts a connection is refused without, for this host's own name: the breaker and
    // back-pressure tests use a name of their own, so `fake` material would not open them.
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
        vm: false,
    }
}

/// Three connection failures in a row and the host is not dialled any more: the fourth call fails at once
/// naming the reason and when the host was last reached, and the fake's log proves no ssh was spawned.
#[test]
fn three_failed_calls_open_the_breaker_and_the_next_fails_at_once() {
    // A unique host name: the breaker is process-wide, and a shared name would carry its streak,
    // and its opened state, into another test.
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

    // The record is how a report sees the same thing.
    let report = letibot_tools::rpc::report(&host);
    assert!(report.contains("dropped"), "{report}");
    let _ = std::fs::remove_dir_all(&fake.dir);
}

/// A command that has to be abandoned takes its remote process group with it: the command writes its
/// group down, the call is killed at its bound, and the cancel reaches that group.
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
    // The cancel is the whole proof, and the group is why. The `setsid` wrapper put the
    // command in a session of its own, so the kill that ended the local ssh cannot have reached
    // this child, and the run only returned after [`Ssh::cancel_group`] signalled the group.
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

/// A job's wait is a heartbeat and does not fill the record ring, since a status poll is one ssh call
/// every 120 ms and one `job_wait` would evict every call a reader could act on. A failed poll is still
/// recorded.
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
    // Only the count is asserted: the ring is shared by every test in this binary, so which
    // records survive is another host's traffic, and a presence assertion here would be a flake.
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

/// A host whose mise refuses a `-C` that is not a directory, which is what a real mise does: a wrapper
/// pointed at the checkout before `git worktree add` made it fails every step of a placement.
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
             [ $# -eq 0 ] && exit 0\n\
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
    // No `mise` line, so the default probe runs: it finds this stub at `$HOME/.local/bin/mise`,
    // which is the operator's own layout.
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
    // Every `-C` was a path that exists on the remote, which the stub itself enforced, asserted
    // here so a change that stops using the stub still fails this test.
    for d in &dirs {
        assert!(
            d.starts_with('/') && Path::new(d).is_dir(),
            "`-C {d}` is an absolute directory that exists: {dirs:?}"
        );
    }
    // The wrapper is pointed at the root until the checkout exists, and at the checkout after.
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

/// Every call is recorded, and the report says what it did: the records carry the host, the step,
/// the duration and the outcome, and the report is the row `hosts {probe: true}` shows.
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

/// A host takes a bounded number of ssh sessions and a call beyond it queues under its own deadline: a
/// client opening one per call would have sshd refuse the eleventh, which reads as a fault on the host.
#[test]
fn concurrent_calls_to_one_host_queue_at_the_session_bound() {
    let host = format!("busy-{}", std::process::id());
    let fake = Fake::new(
        "busy",
        "d=@DIR@/live; mkdir -p \"$d\"; : > \"$d/$$\"\n\
         echo $(ls \"$d\" | wc -l) >> @DIR@/counts\n\
         sleep 0.3\n\
         rm -f \"$d/$$\"\n\
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
    // One file per call in flight and one appended line each: a `cat`-then-`echo` counter loses a
    // decrement under load, and an inflated count reads as more sessions than the gate let through.
    let most: usize = std::fs::read_to_string(fake.path("counts"))
        .expect("the fake counted")
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .max()
        .expect("a number");
    assert!(
        most <= letibot_tools::rpc::MAX_SESSIONS_PER_HOST,
        "{most} ssh calls were in flight at once against a bound of {}",
        letibot_tools::rpc::MAX_SESSIONS_PER_HOST
    );
    let _ = std::fs::remove_dir_all(&fake.dir);
}

/// An auth refusal is definite: said in those words, never retried, exactly one invocation. The
/// machine answered and the key is not there, so nothing about a second offer changes it, and
/// treating it as a lost connection costs a duplicate of every call.
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

/// A host key that does not verify is the other definite refusal, and its fix is the other file.
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

/// A script at `$HOME/.local/bin/mise` is the host's mise, and every remote command goes through
/// `<mise> exec -C <the checkout> -- bash -lc '<cmd>'`: the default is to probe that path once and
/// cache the answer, and the wrapper is asserted on the argv the next command was handed.
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
             [ $# -eq 0 ] && exit 0\n\
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
    // A multi-line script makes one MISE line span several lines, so this asks the lines mise
    // actually began: every one of them wraps a login shell.
    assert!(
        !log.is_empty()
            && log
                .lines()
                .filter(|l| l.starts_with("MISE "))
                .all(|l| l.starts_with("MISE exec -C ") && l.contains("-- bash -lc ")),
        "every wrapped command is `mise exec -C <dir> -- bash -lc <script>`: {log}"
    );
    // The calls made BEFORE the checkout existed ran in `$HOME`, the honest answer for a
    // directory that does not exist yet: the far shell expands it, so the ssh argv says `$HOME`
    // while the fake's log shows a path.
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

/// `mise = false` turns the wrapper off, and a host that names a program uses that program with
/// its `~` expanded there.
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

    // A path, spelled with a `~` that is the remote's home.
    let host = format!("miseat-{}", std::process::id());
    let fake = Fake::new("miseat", "exec bash -lc \"$last\"");
    let mise = fake.dir.join("home/.local/bin/mise");
    std::fs::create_dir_all(mise.parent().expect("a bin")).expect("the mise directory");
    write_executable(
        &mise,
        &format!(
            "#!/bin/bash\n\
             [ $# -eq 0 ] && exit 0\n\
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
    // `~` is expanded on the remote: the argv says `"$HOME/.local/bin/mise"`, and the shell there
    // turns it into the fake home's copy, which is the file that wrote this log.
    assert!(
        fake.calls()
            .iter()
            .any(|c| c.contains("\"$HOME/.local/bin/mise\" exec -C ")),
        "the program is spelled with the REMOTE's home: {:#?}",
        fake.calls()
    );
    let _ = std::fs::remove_dir_all(&fake.dir);
}

/// The `notes` tool writes where the session's other tools write: a note added in a placed
/// session lands in the remote checkout, and the daemon's own workspace gets no `.letibot/notes`
/// at all. The note is read back with `std::fs` from the far path - the fake remote is a real
/// local path, and a test that asked the tool whether it wrote is a test of the tool's opinion.
#[test]
fn a_note_written_in_a_placed_session_lands_in_the_remote_checkout() {
    let mut rig = Rig::new("notes");
    let backend = rig.place();
    let worktree = rig.worktree.clone();
    let local = std::env::temp_dir().join(format!(
        "letibot-notes-local-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let global = local.join("global");
    std::fs::create_dir_all(&global).expect("the box-wide notes dir");

    let mut h = letibot_tools::testing::harness_over(Box::new(backend), local.clone(), global);
    let r = h.call(
        "notes",
        r#"{"action":"add","name":"placed","text":"a note from the host"}"#,
    );
    assert!(r.is_grounded(), "{}", r.render());
    assert_eq!(
        std::fs::read_to_string(worktree.join(".letibot/notes/placed.md"))
            .expect("the note is in the remote checkout"),
        "a note from the host\n"
    );
    assert!(
        !local.join(".letibot").exists(),
        "the daemon's own workspace got no notes directory: {}",
        local.display()
    );

    // And `read` over the same harness finds it where it was written, not where the scope is.
    let r = h.call("notes", r#"{"action":"read","name":"placed"}"#);
    assert!(r.is_grounded(), "{}", r.render());
    assert!(
        r.render().contains("a note from the host"),
        "{}",
        r.render()
    );
    let _ = std::fs::remove_dir_all(&local);
}

/// **The note corpus is one round trip per page, not one per note.** Over a placed session every
/// read is an ssh call: a `search` over twelve notes was fourteen of them, which is the cost the
/// same commit's stat-in-the-listing change was made to avoid.
#[test]
fn a_placed_session_reads_its_note_corpus_in_pages() {
    let mut rig = Rig::new("notes-pages");
    let backend = rig.place();
    let worktree = rig.worktree.clone();
    let local = std::env::temp_dir().join(format!(
        "letibot-notes-pages-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let global = local.join("global");
    std::fs::create_dir_all(&global).expect("the box-wide notes dir");
    // Seeded through the filesystem: this test counts ssh calls, and a write would be part of the
    // number it asserts on.
    std::fs::create_dir_all(worktree.join(".letibot/notes")).expect("the notes dir");
    for i in 0..12 {
        std::fs::write(
            worktree.join(format!(".letibot/notes/note-{i}.md")),
            format!("note {i}\n"),
        )
        .expect("a note on the far side");
    }

    let mut h = letibot_tools::testing::harness_over(Box::new(backend), local.clone(), global);
    let before = rig.ssh_log().matches("ARGV ").count();
    let r = h.call("notes", r#"{"action":"search","query":"note"}"#);
    assert!(r.is_grounded(), "{}", r.render());
    let calls = rig.ssh_log().matches("ARGV ").count() - before;
    assert!(
        calls <= 4,
        "the corpus was read one note at a time: {calls} ssh call(s) for 12 notes"
    );
    let _ = std::fs::remove_dir_all(&local);
}

/// **A session that is already stopped can still be collected.** The branch came home only while
/// the backend that made the placement was still reachable, so a package whose daemon was stopped
/// left its work on the host with nothing here to fetch it. A collect needs nothing of the daemon:
/// the host, the repository and the session id are the names the placement was made by.
#[test]
fn a_stopped_session_is_collected_by_the_names_its_placement_used() {
    let mut rig = Rig::new("collect");
    let b = rig.place();
    std::fs::write(rig.worktree.join("late.rs"), b"// the work\n").expect("a file");
    git(&rig.worktree, &["add", "late.rs"]);
    git(
        &rig.worktree,
        &["commit", "-q", "-m", "the work the package did"],
    );
    let remote_head = git(&rig.worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let back = format!("remote/{}/{SHORT}", rig.host.name);
    // The state `letibot --stop` leaves: the session was never closed, so nothing fetched.
    drop(b);
    let absent = letibot_tools::testing::git(&rig.repo)
        .args(["rev-parse", "--verify", &back])
        .output()
        .expect("git runs");
    assert!(
        !absent.status.success(),
        "the fixture has the branch before the collect, so it would prove nothing"
    );

    let said = letibot_tools::remote::collect(&letibot_tools::remote::CollectSpec {
        host: rig.host.clone(),
        session: "s-01abcdef".into(),
        repo: rig.repo.clone(),
        ssh: Some(rig.ssh.clone()),
        paths: rig.paths.clone(),
        vm: false,
    })
    .expect("the collect reached the host");
    assert!(
        said.contains(&back) && said.contains("came back"),
        "the collect says which branch came home: {said}"
    );
    assert_eq!(
        git(&rig.repo, &["rev-parse", &back]).trim(),
        remote_head,
        "the branch is at the commit the session made: {said}"
    );
    assert!(
        git(&rig.repo, &["show", &format!("{back}:late.rs")]).contains("the work"),
        "and the commit's own contents came with it"
    );
}

/// A collect that names a host with nothing of that session on it says which path it looked for,
/// rather than a `git fetch` refusal about a repository this box cannot see.
#[test]
fn a_collect_of_nothing_names_the_host_and_the_path_it_looked_for() {
    let mut rig = Rig::new("collect-nothing");
    let _b = rig.place();
    let refused = letibot_tools::remote::collect(&letibot_tools::remote::CollectSpec {
        host: rig.host.clone(),
        // Never placed here, so the checkout this id names does not exist.
        session: "s-99ffffff".into(),
        repo: rig.repo.clone(),
        ssh: Some(rig.ssh.clone()),
        paths: rig.paths.clone(),
        vm: false,
    })
    .expect_err("there is nothing to collect");
    let said = refused.to_string();
    assert!(
        said.contains("nothing at") && said.contains("99ffffff"),
        "the refusal names the path it looked for: {said}"
    );
}
