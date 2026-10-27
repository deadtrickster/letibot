//! **A command on a remote host, inside a boundary** — `docs/remote-hosts.md`'s RH3 row.
//!
//! The local half of Layer 1 has had its own suite since it landed (`tests/confine.rs`): a view,
//! an egress decision, a measured readback, and a refusal when the helper cannot deliver any of
//! it. This suite is the same properties one machine over, and it is written the way RH1's is —
//! a fake `ssh` that runs the command locally against a temporary "remote" — because a test that
//! contacted a real host would be a test nobody can run.
//!
//! # The fake's `bwrap`
//!
//! Two of the questions here are *what argv crosses the connection* and *what the backend does
//! with the answer*, and neither needs a real bubblewrap. So the fake remote gets a `bwrap` shim
//! that answers `--version`, records its argv, **fabricates a namespace readback** (the probe's
//! own vocabulary: `ns <name> <inode>` and `seal <name> <verdict>`), and otherwise execs the
//! command it was handed. That makes the probe's answer a fact of the test rather than of the
//! kernel, so the same assertions hold on a Mac with no `bwrap` at all and on a Linux box with
//! one — and `remote_hosts.rs`'s fixture pins `confine: Some(false)` for the same reason.
//!
//! The kernel itself is measured once, elsewhere: `tests/confine.rs` runs the real `/usr/bin/bwrap`
//! for every local spawn.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_tools::backend::{Command, ExecBackend};
use letibot_tools::exec::{Egress, JobId, JobState, ScopeKind, SpawnRequest};
use letibot_tools::remote::{Host, Paths, SshBackend, SshSpec};

/// The short id this suite's session gets: the last eight of `s-01abcdef`.
const SHORT: &str = "01abcdef";

/// **The readback a real helper would produce**, from a fake that is not a namespace at all.
///
/// Every inode differs from the outer one, so the probe reads the boundary as entered — and the
/// comparison is against numbers this file made up, which is exactly what makes it a test of the
/// *plumbing* and not of the kernel.
const FAKE_READBACK: &str = "\
printf 'ns user user:[4026535001]\\n'
printf 'ns mnt mnt:[4026535002]\\n'
printf 'ns pid pid:[4026535003]\\n'
printf 'ns net net:[4026535004]\\n'
printf 'ns ipc ipc:[4026535005]\\n'
printf 'ns uts uts:[4026535006]\\n'
printf 'ns cgroup cgroup:[4026535007]\\n'
printf 'seal nested held\\n'
printf 'seal nnp 1\\n'";

/// **One fake remote**: a home that stands in for that machine's, a repository to send, an `ssh`
/// that runs a command against the first, and a `bwrap` that answers the probe.
struct Rig {
    dir: PathBuf,
    /// The fake remote's `$HOME` — where the toolchain grants are found.
    home: PathBuf,
    /// Where the checkout lands, on the "remote".
    worktree: PathBuf,
    /// The jobs' own directory, on the "remote".
    jobs: PathBuf,
    repo: PathBuf,
    ssh: PathBuf,
    /// What the fake `bwrap` was handed, one line per call.
    bwrap_log: PathBuf,
    paths: Paths,
    host: Host,
}

impl Rig {
    /// `readback` is the shell fragment the fake helper prints when it is asked to *measure* the
    /// boundary; `mode` says whether it pretends to be a helper at all (`None` is a host with no
    /// `bwrap` on its `PATH`).
    fn new(tag: &str, readback: &str, with_bwrap: bool) -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "letibot-confine-{tag}-{}-{}",
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

        // **A `setsid` for the boxes that have none**, as RH1's rig does: a job has to lead its
        // own process group or `job_kill`'s group signal is not the thing under test.
        if !on_path("setsid") {
            write_executable(
                &bin.join("setsid"),
                "#!/usr/bin/env perl\nuse POSIX ();\nPOSIX::setsid();\nexec { $ARGV[0] } @ARGV;\n",
            );
        }

        let bwrap_log = dir.join("bwrap.log");
        if with_bwrap {
            write_executable(&bin.join("bwrap"), &bwrap_shim(&bwrap_log, readback));
        }

        // **The toolchain the view is expected to hold**, and the one config file a commit needs:
        // the probe reports what exists, so these have to exist for the grants to appear.
        std::fs::create_dir_all(home.join(".cargo")).expect("a cargo home");
        std::fs::create_dir_all(home.join(".rustup")).expect("a rustup home");
        std::fs::write(home.join(".gitconfig"), b"[user]\n\tname = a test\n").expect("a gitconfig");

        let ssh = bin.join("ssh");
        write_executable(
            &ssh,
            &format!(
                "#!/bin/bash\n\
                 export PATH='{bin}':$PATH\n\
                 export HOME='{home}'\n\
                 for last; do :; done\n\
                 exec bash -lc \"$last\"\n",
                bin = bin.display(),
                home = home.display()
            ),
        );
        let paths = Paths::in_home(dir.join("letibot-home"));
        // **A host of this rig's own**, for the reason RH6's breaker test gives: the probe's answer
        // is cached per host name for the life of the process, so two rigs sharing a name would
        // share one answer — and a test whose whole point is *this host has no helper* would read
        // the answer a different rig got.
        let name = format!("conf-{tag}");
        let hostname = format!("{name}.invalid");
        std::fs::create_dir_all(paths.keys()).expect("the keys directory");
        std::fs::write(paths.default_key(&name), b"a dedicated key").expect("the key");
        std::fs::write(
            paths.known_hosts(),
            format!("{hostname} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n"),
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
            name,
            hostname,
            user: Some("tester".into()),
            key: None,
            path: Vec::new(),
            root: "~/letibot-remote".into(),
            mise: Some(letibot_tools::remote::Mise::Off),
            ..Host::default()
        };
        Rig {
            dir,
            home,
            worktree,
            jobs,
            repo,
            ssh,
            bwrap_log,
            paths,
            host,
        }
    }

    fn spec(&self, egress: Egress) -> SshSpec {
        SshSpec {
            host: self.host.clone(),
            name: "s-01abcdef".into(),
            source: self.repo.clone(),
            writable: true,
            exec: true,
            ssh: Some(self.ssh.clone()),
            paths: Some(self.paths.clone()),
            bound: Some(Duration::from_secs(10)),
            egress,
            // **Asked**, which is production's own value: `None`. The fake remote answers with
            // its `bwrap`, and a rig built without one answers that it has none.
            confine: None,
        }
    }

    fn place(&mut self, egress: Egress) -> SshBackend {
        let b = SshBackend::place(&self.spec(egress))
            .unwrap_or_else(|e| panic!("the placement is arranged: {e}"));
        self.worktree = PathBuf::from(b.worktree_path());
        self.jobs = self.worktree.join(".letibot-remote").join("jobs");
        b
    }

    /// **What the helper was handed**, one line per invocation.
    fn bwrap_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.bwrap_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .filter(|l| l.starts_with("ARGV "))
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // **Only the pids this rig wrote down**, never a pattern.
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

/// The fake helper: `--version`, the argv it was handed, a fabricated readback for the probe's own
/// question, and otherwise the command after `--`, run with the `--clearenv` + `--setenv`
/// environment and in the `--chdir` it was given.
///
/// **The two flags it implements are the two whose semantics an assertion needs**: what the command
/// sees as `$HOME` and on `$PATH` is decided by them, and a fake that ignored them would let a
/// wrong environment pass. The binds and the namespaces it does not implement — those are asserted
/// as argv, because that is where their meaning is.
fn bwrap_shim(log: &Path, readback: &str) -> String {
    format!(
        "#!/bin/bash\n\
         if [ \"$1\" = \"--version\" ]; then echo 'bubblewrap 0.11.1 (fake)'; exit 0; fi\n\
         printf 'ARGV %s\\n' \"$*\" >> '{log}'\n\
         args=(); cmd=(); envp=(); cleared=0; seen=0\n\
         shift 0\n\
         argv=(\"$@\"); i=0\n\
         while [ $i -lt ${{#argv[@]}} ]; do\n\
           a=\"${{argv[$i]}}\"\n\
           if [ $seen -eq 1 ]; then cmd+=(\"$a\"); i=$((i+1)); continue; fi\n\
           case \"$a\" in\n\
             --) seen=1 ;;\n\
             --chdir) i=$((i+1)); args+=(\"--chdir\" \"${{argv[$i]}}\") ;;\n\
             --clearenv) cleared=1 ;;\n\
             --setenv) i=$((i+1)); k=\"${{argv[$i]}}\"; i=$((i+1)); envp+=(\"$k=${{argv[$i]}}\") ;;\n\
             *) args+=(\"$a\") ;;\n\
           esac\n\
           i=$((i+1))\n\
         done\n\
         case \"${{cmd[*]}}\" in\n\
           *'readlink /proc/self/ns/'*)\n\
             {readback}\n\
             exit 0 ;;\n\
         esac\n\
         cwd=. ; i=0\n\
         while [ $i -lt ${{#args[@]}} ]; do\n\
           if [ \"${{args[$i]}}\" = \"--chdir\" ]; then cwd=\"${{args[$((i+1))]}}\"; fi\n\
           i=$((i+1))\n\
         done\n\
         cd \"$cwd\" 2>/dev/null || true\n\
         if [ $cleared -eq 1 ]; then exec env -i \"${{envp[@]}}\" \"${{cmd[@]}}\"; fi\n\
         exec \"${{cmd[@]}}\"\n",
        log = log.display(),
        readback = readback
    )
}

/// The readback a helper that entered **nothing** produces: the caller's own inodes, read from
/// the shell that is running the helper. That is what a kernel refusing the namespaces looks
/// like, and it is the answer the probe has to refuse rather than believe.
const SAME_INODES: &str = "\
for n in user mnt pid net ipc uts cgroup; do \
  printf 'ns %s %s\\n' \"$n\" \"$(readlink /proc/self/ns/$n)\"; done\n\
printf 'seal nested held\\n'\n\
printf 'seal nnp 1\\n'";

fn write_executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).expect("writing a fake program");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

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

/// **A command of the model's runs inside the helper, with the view the local policy builds.**
///
/// This is the whole of RH3's item 2 in one assertion set: the checkout writable at its own path,
/// the toolchain read-only, `$HOME` a directory of letibot's own and not the operator's, the
/// command after `--`, and — the one that is easy to lose — **no `--new-session`**, because the
/// remote process group is what an interrupt kills.
#[test]
fn a_confined_command_runs_inside_the_helper_with_the_local_policys_view() {
    let mut rig = Rig::new("view", FAKE_READBACK, true);
    let b = rig.place(Egress::Denied);
    assert!(b.is_confined(), "the fake host answered the probe");
    let described = b.describe();
    assert!(described.contains("CONFINED"), "{described}");
    assert!(!described.contains("NOT CONFINED"), "{described}");
    assert_eq!(
        b.root_path().as_deref(),
        Some(b.worktree_path()),
        "a confined session's file tools are bounded by the checkout, as a local one's are"
    );

    let out = b
        .run(&Command {
            argv: vec!["pwd".into()],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect("the command runs");
    assert_eq!(out.exit, 0, "{:?}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        Path::new(String::from_utf8_lossy(&out.stdout).trim()),
        b.worktree_path(),
        "the helper was told `--chdir` the checkout, and ran the command there"
    );

    let calls = rig.bwrap_calls();
    assert_eq!(calls.len(), 2, "a probe and then the command: {calls:#?}");
    // **The command's own argv**, not the probe's: the probe measures the `--new-session` argv
    // because that is what a local spawn asks for, and the command must not be that one.
    let argv = calls.last().expect("the command's call");
    for want in [
        // The namespaces, and the two seals, exactly as a local spawn asks for them.
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup",
        "--unshare-net",
        "--disable-userns",
        "--assert-userns-disabled",
        "--die-with-parent",
        // The view: system roots read-only and `-try` (a merged-`/usr` host has no `/lib`).
        "--ro-bind-try /usr /usr",
        "--proc /proc",
        "--dev /dev",
        "--tmpfs /tmp",
        // The project, read-write, at its own path.
        &format!("--bind {} {}", b.worktree_path(), b.worktree_path()),
        // The operator's toolchain, read-only — and the one file a commit needs.
        &format!(
            "--ro-bind {}/.cargo {}/.cargo",
            rig.home.display(),
            rig.home.display()
        ),
        &format!(
            "--ro-bind {}/.rustup {}/.rustup",
            rig.home.display(),
            rig.home.display()
        ),
        &format!(
            "--ro-bind {}/.gitconfig {}/.gitconfig",
            rig.home.display(),
            rig.home.display()
        ),
        // `$HOME` is letibot's own directory on that host, NOT the operator's. The view resolves
        // it, so it is spelled through the canonical root: on a Mac the temp dir is `/var/…` and
        // its real path `/private/var/…`, and the argv carries the real one.
        &format!(
            "--setenv HOME {}/letibot-remote/.letibot-home/{SHORT}",
            std::fs::canonicalize(&rig.home)
                .unwrap_or_else(|_| rig.home.clone())
                .display()
        ),
        // The keep-list is expanded by the FAR shell — a `$PATH` taken from this box is a list of
        // directories that do not exist there. The fake ssh puts its own `bin` in front of the
        // fake remote's `PATH`, and this process's `PATH` has no such directory, so a value that
        // carries it can only have come from that shell.
        // The mirror the checkout is a worktree of, writable — without it `git` inside the view
        // has no repository (measured: the first packages placed after RH3 could not commit).
        &format!(
            "--bind {0}/letibot-remote/the-repo.git {0}/letibot-remote/the-repo.git",
            std::fs::canonicalize(&rig.home)
                .unwrap_or_else(|_| rig.home.clone())
                .display()
        ),
        "--clearenv",
        "--setenv PATH ",
        // The command, after the `--` that stops the helper reading it as an option.
        "--",
    ] {
        assert!(
            argv.contains(want),
            "`{want}` is not in the helper's argv:\n{argv}"
        );
    }
    assert!(
        argv.contains(&format!("{}:", rig.dir.join("bin").display()))
            || argv.contains(&rig.dir.join("bin").display().to_string()),
        "the keep-list's PATH is the fake remote's own, not this box's:\n{argv}"
    );
    assert!(
        !argv.contains("--new-session"),
        "a remote command must stay in the process group an interrupt kills:\n{argv}"
    );
    assert!(
        argv.ends_with("pwd"),
        "the command's own argv is what runs, and last:\n{argv}"
    );
    // **The operator's ssh directory is not in the view** — never bound, at any path, under any
    // name. RH5's hard refusal is about letibot never reading it, and this is the half that would
    // make it readable to a command.
    let their_ssh = rig.paths.user_ssh().display().to_string();
    assert!(
        !argv.contains(&their_ssh) && !argv.contains(".ssh"),
        "nothing in the view names the operator's ssh directory:\n{argv}"
    );
    assert!(
        !argv.contains(&format!(
            "--bind {} {}",
            rig.home.display(),
            rig.home.display()
        )),
        "`$HOME` itself is never writable, nor bound at its own path:\n{argv}"
    );

    // **`$HOME` inside is the session's own**, so a command that writes to `~` does not write to
    // the operator's home. The fake helper does not implement the namespace, so what is asserted
    // is the environment it was handed.
    let round = b
        .run(&Command {
            argv: vec!["sh".into(), "-c".into(), "echo $HOME".into()],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect("it runs");
    assert_eq!(round.exit, 0);
    let home_in_view = String::from_utf8_lossy(&round.stdout).trim().to_string();
    assert!(
        home_in_view.ends_with(&format!(".letibot-home/{SHORT}")),
        "the helper's HOME is the session's own: {home_in_view}"
    );
    assert_ne!(home_in_view, rig.home.display().to_string());
}

/// **The session's network rule is the boundary's**, and it is a decision rather than a default:
/// a point that seats network calls shares the host's namespace, and one that does not gets an
/// empty one.
#[test]
fn the_network_namespace_follows_the_sessions_rule() {
    let mut denied = Rig::new("netdenied", FAKE_READBACK, true);
    let b = denied.place(Egress::Denied);
    b.run(&Command {
        argv: vec!["true".into()],
        cwd: String::new(),
        env: Vec::new(),
    })
    .expect("it runs");
    assert!(
        denied.bwrap_calls()[0].contains("--unshare-net"),
        "no network, by the session's rule: {:#?}",
        denied.bwrap_calls()
    );

    let mut shared = Rig::new("netshared", FAKE_READBACK, true);
    let b = shared.place(Egress::Host {
        why: "the mode seats network calls".into(),
    });
    b.run(&Command {
        argv: vec!["true".into()],
        cwd: String::new(),
        env: Vec::new(),
    })
    .expect("it runs");
    assert!(
        !shared.bwrap_calls()[0].contains("--unshare-net"),
        "the host's network, because the mode seats network calls: {:#?}",
        shared.bwrap_calls()
    );
    let described = b.describe();
    assert!(described.contains("CONFINED"), "{described}");
}

/// **A host with no `bwrap` at all**: the probe's answer is the fact, and the refusal names what is
/// missing. No fake host can produce this one — the machine behind the fake has `/usr/bin/bwrap`
/// on its `PATH` and `command -v` would find it — so the answer is built the way the probe builds
/// it, from what a host with no helper prints.
#[test]
fn a_host_with_no_helper_is_refused_and_says_which_helper() {
    use letibot_tools::exec::confine::{measured_from_probe, read_probe};
    use letibot_tools::exec::{Egress, ViewSpec};
    let probe = read_probe("helper=\nhome=/home/someone\n");
    assert!(probe.helper.is_empty(), "the host said it has none");
    let refused = measured_from_probe(
        &probe,
        ViewSpec::project_only("/home/someone/letibot-remote/the-repo/01abcdef"),
        Egress::Denied,
    )
    .expect_err("a host with no helper confines nothing");
    let said = refused.to_string();
    assert!(said.contains("`bwrap` is not on `PATH`"), "{said}");
    assert!(said.contains("apt install bubblewrap"), "{said}");
    assert!(
        said.contains("nothing was run in a way that claimed to be"),
        "a refusal must not be mistakable for a boundary: {said}"
    );
}

/// **A host that cannot confine runs exactly as it did before RH3, and says so** — unconfined,
/// every call a decision, with the probe's own reason on the placement and in `describe`.
///
/// The reason here is the measured one (the helper ran and entered nothing), because that is the
/// failure a fake host can produce honestly; `a_host_with_no_helper_is_refused_and_says_which_helper`
/// above is the other one, and both end in the same fallback.
#[test]
fn a_host_that_cannot_confine_falls_back_and_names_the_reason() {
    let mut rig = Rig::new("nobwrap", SAME_INODES, true);
    let b = rig.place(Egress::Denied);
    assert!(
        !b.is_confined(),
        "a host that entered nothing confines nothing"
    );
    assert!(
        b.boundary_refusal()
            .is_some_and(|w| w.contains("did not enter every required namespace")),
        "the reason is the measured one: {:?}",
        b.boundary_refusal()
    );
    assert_eq!(
        b.root_path(),
        None,
        "with no view there is no boundary fact, and `root_path` must not invent one"
    );
    let described = b.describe();
    assert!(described.contains("NOT CONFINED"), "{described}");
    let notes = b.placement_notes().join("\n");
    assert!(
        notes.contains("UNCONFINED") && notes.contains("required namespace"),
        "the placement says it, in the operator's own words: {notes}"
    );

    let out = b
        .run(&Command {
            argv: vec!["pwd".into()],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect("an unconfined command still runs");
    assert_eq!(out.exit, 0);
    assert!(
        rig.bwrap_calls().len() <= 1,
        "nothing wrapped the command beyond the probe itself: {:#?}",
        rig.bwrap_calls()
    );
}

/// **A helper that runs but enters nothing is refused, not believed.** The readback is compared
/// against the caller's own inodes, which is the only honest test of *did the namespace take*, and
/// this is the answer `kernel.apparmor_restrict_unprivileged_userns=1` produces.
#[test]
fn a_probe_whose_namespaces_did_not_take_falls_back_with_that_reason() {
    let mut rig = Rig::new("samens", SAME_INODES, true);
    let b = rig.place(Egress::Denied);
    assert!(!b.is_confined());
    let why = b.boundary_refusal().unwrap_or_default();
    assert!(
        why.contains("did not enter every required namespace"),
        "the refusal is the measured one: {why}"
    );
    assert!(why.contains("user"), "{why}");
}

/// **A background job runs inside the boundary too, and `job_kill` still ends its group.**
///
/// The second half is the reason [`letibot_tools::exec::Session::Kept`] exists: `--new-session`
/// would move the sandbox out of the process group the launcher recorded, and a group kill would
/// then end the launcher and leave the work running.
#[test]
fn a_job_runs_inside_the_boundary_and_its_group_is_killed() {
    let mut rig = Rig::new("job", FAKE_READBACK, true);
    let b = rig.place(Egress::Denied);
    let host = b.processes_arc().expect("this backend starts processes");
    let id: JobId = host
        .spawn(&SpawnRequest {
            command: "echo job-inside-marker; sleep 300".into(),
            slug: Some("a-confined-job".into()),
            cwd: String::new(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![("A_JOB_VARIABLE".into(), "set".into())],
            tty: false,
            memory: None,
        })
        .expect("the job starts");

    // The launcher is a file on that machine, and it is where the argv can be read back.
    let n = id.0.trim_start_matches('j').to_string();
    let launcher = std::fs::read_to_string(rig.jobs.join(format!("{n}.sh"))).expect("the launcher");
    assert!(launcher.contains("bwrap"), "{launcher}");
    assert!(
        launcher.contains(&format!("--chdir {}", b.worktree_path()))
            || launcher.contains(&format!("--chdir '{}'", b.worktree_path())),
        "the job's cwd is the boundary's chdir: {launcher}"
    );
    assert!(
        launcher.contains("--setenv A_JOB_VARIABLE set"),
        "a job's environment reaches the inside of the boundary: {launcher}"
    );
    assert!(
        !launcher.contains("--new-session"),
        "a job must stay in the group `job_kill` signals: {launcher}"
    );

    let waited = host.wait_job(&id, Duration::from_secs(2)).expect("a wait");
    let _ = waited;
    let out = host.output(&id, 0, usize::MAX).expect("its output");
    assert!(
        out.text().contains("job-inside-marker"),
        "the job really ran: {}",
        out.text()
    );
    let view = host.job(&id).expect("known");
    assert!(view.pid > 1, "the launcher wrote its pid: {view:?}");
    let started = Instant::now();
    let reaping = host
        .kill_job_as(&id, "the test asking")
        .expect("the kill is recorded");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the group kill took {:?}",
        started.elapsed()
    );
    assert!(reaping.clean(), "nothing survived: {reaping:?}");
    assert!(matches!(
        host.job(&id).expect("known").state,
        JobState::Killed { .. }
    ));
}

/// **A file tool on a confined session can see what a command can see, and nothing else.**
///
/// This is the half that RH5's refusal cannot reach on a placed session: that rule resolves a path
/// on *this* box, so a path on the far machine resolves to nothing here and passes. The view is
/// the answer — a directory that is not in it produces `Outside` rather than a read.
///
/// And it is the VIEW's rule, not the checkout's: `/etc/passwd` is bound read-only into a
/// command's sandbox, so `read` reaches it too. One answer to one question, rather than a command
/// that can `cat` a file its own `read` tool denies.
#[test]
fn the_boundary_bounds_the_file_tools_and_not_only_the_commands() {
    use letibot_tools::backend::BackendError;
    let mut rig = Rig::new("rootpath", FAKE_READBACK, true);
    let b = rig.place(Egress::Denied);
    assert_eq!(b.root_path().as_deref(), Some(b.worktree_path()));
    assert_eq!(b.workspace_path().as_deref(), Some(b.worktree_path()));

    // Inside the view: the checkout, which is writable.
    b.write("notes.txt", b"inside")
        .expect("the checkout is writable");
    assert_eq!(b.read("notes.txt").expect("and readable"), b"inside");
    // Inside, and granted read-only: the toolchain, at its own path on that machine.
    std::fs::write(rig.home.join(".cargo/README-of-the-grant"), b"x").expect("a file");
    let granted = format!("{}/.cargo/README-of-the-grant", rig.home.display());
    assert!(
        b.read(&granted).is_ok(),
        "a grant is in the view, so the tools see it too"
    );
    // Inside, and bound read-only as a system root — a command can read it, so the tools can.
    assert!(b.read("/etc/hostname").is_ok() || b.read("/etc/passwd").is_ok());
    // **Outside the view**: refused, and named as a boundary rather than as a miss.
    //
    // `/srv` and not something under the fixture's own `/tmp`: `/tmp` is one of the
    // **replaced** roots — a fresh tmpfs inside the sandbox, so a path there is absent for a
    // different reason and `ViewSpec::outside` deliberately says *not outside* (it exists and can
    // be used, it is simply empty). A refusal that said *outside* about `/tmp` would be the wrong
    // note; this is what the right one looks like.
    let outside = "/srv/notes-the-view-does-not-hold".to_string();
    let refused = b
        .read(&outside)
        .expect_err("a path the view does not contain is outside, not a miss");
    assert!(
        matches!(&refused, BackendError::Outside(p) if p == &outside),
        "a location the view does not hold is a boundary answer, not `no such file`: {refused:?}"
    );
    assert!(b.list(&outside).is_err() && b.stat(&outside).is_none());
    // And the unconfined half of the same fact: no view, no claim about the far machine's
    // filesystem.
    let mut plain = Rig::new("rootpathnone", SAME_INODES, true);
    let unbounded = plain.place(Egress::Denied);
    assert_eq!(unbounded.root_path(), None);
    let theirs = format!("{}/notes-the-view-does-not-hold", plain.home.display());
    std::fs::write(&theirs, b"the operator's own file").expect("a file outside");
    assert!(
        unbounded.read(&theirs).is_ok(),
        "with no boundary there is no view, so nothing is refused for being outside one"
    );
}

/// **The card is told where the call lands, and it is told the truth.**
///
/// Until RH3 the host arm of `boundary_facts` was one fixed sentence — *"the host filesystem is not
/// sandboxed; §11.4's boundary arrives with firecode"* — which was false for a confined session.
/// The three answers are now three different sentences, and the difference between them is the
/// difference between *the answer you are about to give is worth less* and *worth more*.
#[test]
fn the_gate_is_told_about_the_boundary_and_not_shown_a_fixed_sentence() {
    use letibot_tools::GateCall;
    use letibot_tools::adjudicate::AdjudicatedGate;
    use letibot_tools::schema::Access;

    let mut rig = Rig::new("gate", FAKE_READBACK, true);
    let b = rig.place(Egress::Denied);
    let boundary = b.boundary().expect("the fake host confines");

    let args = serde_json::json!({ "command": "echo a-value > notes.txt" });
    let call = GateCall {
        name: "bash",
        access: Access::Exec,
        args: &args,
        turn_id: "t-1",
        call_id: "c-1",
        workspace: b.worktree_path(),
        target_exists: None,
        scripts: &[],
    };

    // 1. A boundary: the card says the view, and says what it means.
    let mut gate = AdjudicatedGate::closed().with_boundary(Some(boundary), None);
    let req = gate.request_for(&call);
    let said = req.boundary_facts.join("\n");
    assert!(said.contains("INSIDE a measured boundary"), "{said}");
    assert!(
        said.contains(b.worktree_path()),
        "the card names the view's own project: {said}"
    );
    assert!(
        said.contains("ABSENT"),
        "and says what a path outside it is: absent, not denied: {said}"
    );
    assert!(
        said.contains("no network at all"),
        "and the session's egress rule: {said}"
    );

    // 2. Asked for and refused: the card carries the probe's reason.
    let mut gate = AdjudicatedGate::closed().with_boundary(
        None,
        Some("the helper ran but did not enter every required namespace".into()),
    );
    let req = gate.request_for(&call);
    let said = req.boundary_facts.join("\n");
    assert!(
        said.contains("NOT sandboxed") && said.contains("did not enter every required namespace"),
        "{said}"
    );

    // 3. No question asked: the old sentence, which is true for a session that never wanted one.
    let mut gate = AdjudicatedGate::closed();
    let req = gate.request_for(&call);
    let said = req.boundary_facts.join("\n");
    assert!(said.contains("is not sandboxed"), "{said}");
    assert!(!said.contains("INSIDE a measured boundary"), "{said}");
}

/// **What a confined session is not asked about, and what it still is** — the inside/outside
/// boundary, through the gate a placed session actually uses.
///
/// This is the honest form of the sentence *"a remote command that ran confined is adjudicated as
/// confined"*, and the reason it is written as a pair: **there is one gate and it has no
/// confinement input at all.** A local confined session and a placed one reach the same rules, so
/// *parity with a local confined command* is a property of the code being shared rather than of a
/// second lowering — and what RH3 had to fix was the **fact** (a hardcoded `backend_confined`,
/// and a card that recited *"the host filesystem is not sandboxed"* over a session that had a
/// boundary).
///
/// So what is measured here is the line the confinement makes meaningful:
///
/// * a shell command that only **looks**, and only at paths layer A cannot place as outside, is
///   `Access::Read` — clause 4 — and is admitted at `writes-allowed` with nobody asked;
/// * the same shell reading a path the classifier CAN name as outside keeps exec access, and exec
///   asks at every point (the operator's rule of 2026-09-11). That is what stops a `cat` of a
///   credential from riding clause 4.
#[test]
fn an_inside_read_is_admitted_unasked_and_an_outside_one_still_asks() {
    use letibot_tools::adjudicate::AdjudicatedGate;
    use letibot_tools::intent::Surroundings;
    use letibot_tools::mode::Mode;
    use letibot_tools::schema::Access;
    use letibot_tools::{Gate, GateCall, GateDecision};

    let mut rig = Rig::new("clause4", FAKE_READBACK, true);
    let b = rig.place(Egress::Denied);
    let workspace = b.worktree_path().to_string();
    let surroundings = Surroundings::from_env(workspace.clone()).with_pinned_shell(
        "the exec backend spawns /bin/sh -c, non-interactive, with the environment cleared",
    );
    let mut gate = AdjudicatedGate::closed()
        .with_mode(Mode::WRITES_ALLOWED)
        .with_surroundings(surroundings)
        .with_boundary(b.boundary(), None);

    let decide = |gate: &mut AdjudicatedGate, command: &str| {
        let args = serde_json::json!({ "command": command });
        let call = GateCall {
            name: "bash",
            access: Access::Exec,
            args: &args,
            turn_id: "t-1",
            call_id: "c-1",
            workspace: &workspace,
            target_exists: None,
            scripts: &[],
        };
        gate.admit(&call)
    };

    // Inside the view, and only looking: clause 4, nobody asked.
    assert!(
        matches!(decide(&mut gate, "cat README"), GateDecision::Admit),
        "a read inside the workspace is admitted without asking at writes-allowed"
    );
    // Outside the view, and layer A can name it: still exec, so it still asks — and this gate has
    // nobody behind it, which is what makes the difference visible rather than a card.
    assert!(
        !matches!(decide(&mut gate, "cat ~/.bashrc"), GateDecision::Admit),
        "a read of a path the classifier can name as outside keeps exec access and asks"
    );
    assert!(
        !matches!(decide(&mut gate, "cat /etc/shadow"), GateDecision::Admit),
        "and so does a system path"
    );
}

/// **The placement is where the mode's answer comes from**, and a host that confines nothing must
/// not read as one — the fact `harnessd` takes as `backend_confined`.
#[test]
fn confined_is_a_fact_read_off_the_backend() {
    let mut confined = Rig::new("factyes", FAKE_READBACK, true);
    let b = confined.place(Egress::Denied);
    let boundary = b
        .boundary()
        .expect("a confined backend reports the boundary that produced its argv");
    assert!(
        boundary.mechanism.contains("bubblewrap"),
        "the mechanism is named: {}",
        boundary.mechanism
    );
    assert_eq!(
        boundary.view.project,
        PathBuf::from(b.worktree_path()),
        "the view's project is the checkout"
    );
    assert!(
        boundary.complete(),
        "the three required namespaces were measured: {}",
        boundary.describe()
    );

    let mut unconfined = Rig::new("factno", SAME_INODES, true);
    let u = unconfined.place(Egress::Denied);
    assert!(
        u.boundary().is_none(),
        "an unconfined backend has no boundary to report, whatever a caller hoped"
    );
    assert!(
        u.boundary_refusal().is_some_and(|w| !w.is_empty()),
        "and it says why, so the card can"
    );
}
