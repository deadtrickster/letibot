//! A session whose tools run in a firecode VM on another machine, against a fake `firecode` and
//! `ssh`. The `ssh` runs locally with a login `PATH` from the home's own `.bash_profile`, so nothing
//! installed here can answer. This proves the placement's shape, not firecode.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_tools::backend::{Command, ExecBackend};
use letibot_tools::exec::{Egress, ScopeKind, SpawnRequest};
use letibot_tools::remote::{Host, Mise, Paths, SshBackend, SshSpec};

/// The short id this suite's session gets: the last eight of `s-01abcdef`.
const SHORT: &str = "01abcdef";

/// One fake remote: a home, a repository to send, an `ssh`, and a `firecode` that answers.
struct Rig {
    dir: PathBuf,
    /// Where the checkout lands on the "remote".
    worktree: PathBuf,
    /// Where its job files go: beside the checkout, not in it.
    jobs: PathBuf,
    repo: PathBuf,
    ssh: PathBuf,
    /// What the fake `firecode` was asked, one line per call.
    asked: PathBuf,
    paths: Paths,
    host: Host,
}

impl Rig {
    fn new(tag: &str, firecode: bool) -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "letibot-remote-vm-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        // A host name of this rig's own: what a host answered about its `firecode` is cached per
        // name for the life of the process, so two rigs sharing one name would each be told the
        // other's answer.
        let name = format!("fake-{tag}");
        let hostname = format!("{name}.invalid");
        let bin = dir.join("bin");
        let home = dir.join("home");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::create_dir_all(&home).expect("home");
        // The login shell's `PATH` is the fixture's and nothing else: `bash -lc` reads `/etc/profile`
        // first and that file is the box's, so a `~/.bash_profile` runs after it and replaces what the box
        // put there.
        std::fs::write(
            home.join(".bash_profile"),
            format!(
                "PATH={bin}:/usr/bin:/bin\nexport PATH\n",
                bin = bin.display()
            ),
        )
        .expect("the fake home's login profile");
        let asked = dir.join("firecode.log");

        // The device node the probe asks about: a fixture cannot make `/dev/kvm`, so the fake
        // remote says where to look.
        let kvm = dir.join("kvm");
        std::fs::write(&kvm, b"a stand-in for the device node").expect("the kvm stand-in");

        if firecode {
            write_executable(&bin.join("firecode"), &firecode_body(&asked, true));
        }

        let ssh = bin.join("ssh");
        write_executable(
            &ssh,
            &format!(
                "#!/bin/bash\n\
                 [ $# -eq 0 ] && exit 0\n\
                 export PATH='{bin}':$PATH\n\
                 export HOME='{home}'\n\
                 export LETIBOT_KVM_DEVICE='{kvm}'\n\
                 for last; do :; done\n\
                 exec bash -lc \"$last\"\n",
                bin = bin.display(),
                home = home.display(),
                kvm = kvm.display()
            ),
        );

        let paths = Paths::in_home(dir.join("letibot-home"));
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
        letibot_tools::testing::repo_config(&repo);
        std::fs::write(repo.join("README"), b"the first commit\n").expect("a file");
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-q", "-m", "the first commit"]);

        let worktree = home.join("letibot-remote").join("the-repo").join(SHORT);
        let jobs = home
            .join("letibot-remote")
            .join(".letibot-jobs")
            .join(SHORT);
        Rig {
            dir,
            worktree,
            jobs,
            repo,
            ssh,
            asked,
            paths,
            // A fixture's host has no mise: asking a real one where its `cargo` is would be
            // measuring the box the test runs on.
            host: Host {
                name,
                hostname,
                user: Some("tester".to_string()),
                key: None,
                path: Vec::new(),
                root: "~/letibot-remote".into(),
                mise: Some(Mise::Off),
                ..Host::default()
            },
        }
    }

    fn spec(&self, vm: bool) -> SshSpec {
        SshSpec {
            host: self.host.clone(),
            name: "s-01abcdef".into(),
            source: self.repo.clone(),
            writable: true,
            exec: true,
            ssh: Some(self.ssh.clone()),
            paths: Some(self.paths.clone()),
            bound: Some(Duration::from_secs(10)),
            place_bound: None,
            egress: Egress::Denied,
            // A VM is the boundary, so nothing is probed for `bwrap`: `Some(false)` says do not
            // ask this box what it can confine.
            confine: Some(false),
            vm,
            env: Vec::new(),
            env_secrets: Vec::new(),
            grants: Vec::new(),
        }
    }

    /// What the fake `firecode` was asked, one entry per call, the verb first.
    fn asked(&self) -> Vec<String> {
        std::fs::read_to_string(&self.asked)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix("ARGV ").map(str::to_string))
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn write_executable(path: &Path, text: &str) {
    letibot_tools::testing::write_stub(path, text);
}

/// The fake `firecode`'s body, and the one difference that is a defect: `forwards_stdin = false` is the
/// real client, which sends the command over the vsock and never the caller's stdin.
fn firecode_body(asked: &Path, forwards_stdin: bool) -> String {
    let guest = if forwards_stdin {
        "  exec bash -lc \"$last\" ;;"
    } else {
        "  cat >/dev/null; bash -lc \"$last\" </dev/null; exit $? ;;"
    };
    format!(
        "#!/bin/bash\n\
         [ $# -eq 0 ] && exit 0\n\
         printf 'ARGV %s\\n' \"$*\" >> '{asked}'\n\
         verb=$1; shift\n\
         case \"$verb\" in\n\
         \x20 in)\n\
         \x20   for last; do :; done\n\
         {guest}\n\
         \x20 down)\n\
         \x20   project=\n\
         \x20   while [ $# -gt 0 ]; do [ \"$1\" = --project ] && project=$2; shift; done\n\
         \x20   [ -n \"$project\" ] && [ ! -e \"$project-landed\" ] && \
         \x20     cp -a \"$project\" \"$project-landed\" ;;\n\
         esac\n\
         exit 0\n",
        asked = asked.display(),
        guest = guest
    )
}

/// A tree copied wholesale: the guest's own copy of the checkout.
fn copy_tree(from: &Path, to: &Path) {
    let out = std::process::Command::new("cp")
        .arg("-a")
        .arg(from)
        .arg(to)
        .output()
        .expect("cp runs");
    assert!(
        out.status.success(),
        "cp -a {}: {}",
        from.display(),
        String::from_utf8_lossy(&out.stderr)
    );
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

/// `up`, then `in` for the session's own calls, then `down`, in that order, and with the command
/// inside the guest rather than on the host.
#[test]
fn a_vm_placement_boots_the_guest_and_runs_every_call_in_it() {
    let _turn = turns();
    let rig = Rig::new("order", true);
    let b = SshBackend::place(&rig.spec(true)).expect("the placement is arranged");
    assert_eq!(
        b.worktree_path(),
        rig.worktree.display().to_string(),
        "the checkout is where a host placement would have put it"
    );

    let asked = rig.asked();
    assert_eq!(asked.len(), 1, "the boot is one call: {asked:?}");
    assert_eq!(
        asked[0],
        format!("up --project {}", rig.worktree.display()),
        "the guest is booted on the session's own checkout"
    );

    let out = b
        .run(&Command {
            argv: vec!["pwd".into()],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect("the command runs");
    assert_eq!(out.exit, 0, "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        Path::new(String::from_utf8_lossy(&out.stdout).trim()),
        rig.worktree.as_path(),
        "the command ran in the tree the guest holds"
    );
    let asked = rig.asked();
    assert!(
        asked.iter().any(|a| a.starts_with(&format!(
            "in --project {} --cwd {} ",
            rig.worktree.display(),
            rig.worktree.display()
        ))),
        "every call of the session is a command in THAT guest: {asked:?}"
    );

    b.close().expect("a close says where the work went");
    let asked = rig.asked();
    let up = asked.iter().position(|a| a.starts_with("up "));
    let inside = asked.iter().position(|a| a.starts_with("in "));
    let down = asked.iter().position(|a| a.starts_with("down "));
    assert!(
        up < inside && inside < down,
        "booted, used, brought down: {asked:?}"
    );
}

/// A commit made inside the guest comes home: the guest holds a copy, so its work reaches the branch
/// only because `down` leaves that copy beside the checkout and the close takes it onto the checkout the
/// branch comes back from.
#[test]
fn a_commit_made_in_the_guest_comes_back_as_the_branch() {
    let _turn = turns();
    let rig = Rig::new("commit", true);
    let b = SshBackend::place(&rig.spec(true)).expect("the placement is arranged");

    // The guest's tree, a copy of the checkout at the same path: it is a git worktree, so a copy
    // resolves the same gitdir the original does.
    let guest = PathBuf::from(format!("{}-landed", rig.worktree.display()));
    copy_tree(&rig.worktree, &guest);
    std::fs::write(guest.join("feature.rs"), b"// the work\n").expect("a file");
    git(&guest, &["add", "feature.rs"]);
    git(&guest, &["commit", "-q", "-m", "the work"]);
    let inside = git(&guest, &["rev-parse", "HEAD"]).trim().to_string();
    assert_ne!(
        inside,
        git(&rig.worktree, &["rev-parse", "HEAD"]).trim(),
        "the guest is ahead of the checkout, which is the state this takes work out of"
    );

    let said = b.close().expect("a close says where the work went");
    assert!(
        said.contains("the guest's commits are on"),
        "the close says the guest's work was collected: {said}"
    );
    assert!(
        said.contains(&format!("remote/fake-commit/{SHORT}")),
        "and the branch still comes back the way every remote placement's does: {said}"
    );
    assert_eq!(
        git(
            &rig.repo,
            &["rev-parse", &format!("remote/fake-commit/{SHORT}")]
        )
        .trim(),
        inside,
        "the commit made in the guest is the commit on the branch that came back"
    );
    assert_eq!(
        git(
            &rig.local_checkout(),
            &["rev-parse", "--abbrev-ref", "HEAD"]
        )
        .trim(),
        "main",
        "the local repository's own checkout is where it was"
    );
}

impl Rig {
    /// The repository a session on this rig is opened in, which is where the branch comes back to.
    fn local_checkout(&self) -> PathBuf {
        self.repo.clone()
    }

    /// The same rig, with a `firecode in` that behaves like the real client: the caller's stdin
    /// is read and dropped rather than handed to the guest.
    fn vsock(tag: &str) -> Rig {
        let rig = Rig::new(tag, true);
        write_executable(
            &rig.dir.join("bin").join("firecode"),
            &firecode_body(&rig.asked, false),
        );
        rig
    }
}

/// A host with no `firecode` is refused by name, before anything is arranged there.
#[test]
fn a_host_without_firecode_is_refused_by_name() {
    let rig = Rig::new("nofirecode", false);
    let refused = SshBackend::place(&rig.spec(true))
        .expect_err("a host that cannot boot a guest must not place a session in one")
        .to_string();
    assert!(refused.contains("fake"), "{refused}");
    assert!(refused.contains("firecode VM"), "{refused}");
    assert!(
        refused.contains("there is no `firecode`"),
        "the refusal names what is missing: {refused}"
    );
    assert!(
        refused.contains("Leave `--vm` out"),
        "and what to do instead: {refused}"
    );
    assert!(
        !rig.worktree.exists(),
        "nothing was arranged on the host: {}",
        rig.worktree.display()
    );
}

/// The same host without `--vm` places as it always did: the refusal is about the guest, not the
/// machine, and a host placement must not pay for the probe.
#[test]
fn a_host_placement_never_asks_for_a_guest() {
    let rig = Rig::new("noprobe", false);
    let b = SshBackend::place(&rig.spec(false)).expect("the placement is arranged");
    assert!(
        rig.worktree.join("README").is_file(),
        "the checkout is there"
    );
    assert!(
        rig.asked().is_empty(),
        "no `firecode` was asked anything: {:?}",
        rig.asked()
    );
    let said = b.describe();
    assert!(!said.contains("firecode VM"), "{said}");
}

/// The checkout a VM placement makes is a CLONE, not a linked worktree: the guest works on a copy of
/// the directory, and a worktree's `.git` is a file naming a path in the mirror the copy does not carry.
#[test]
fn a_vm_placements_checkout_is_a_clone_and_a_host_placements_is_a_worktree() {
    let _turn = turns();
    let vm = Rig::new("clonevm", true);
    let placed = SshBackend::place(&vm.spec(true)).expect("the placement is arranged");
    assert!(
        vm.worktree.join(".git").is_dir(),
        "a VM's checkout holds its own repository: {}",
        vm.worktree.display()
    );
    let dir = git(&vm.worktree, &["rev-parse", "--absolute-git-dir"])
        .trim()
        .to_string();
    assert!(
        Path::new(&dir).starts_with(&vm.worktree),
        "and that repository is inside the checkout, so the copy the guest works on is one: {dir}"
    );
    assert_eq!(
        git(&vm.worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        format!("remote/{SHORT}"),
        "on the branch the bundle went over as"
    );
    placed.close().expect("a close says where the work went");

    // The other arm, because a build that made everything a clone would pass the assertions above.
    let host = Rig::new("clonehost", false);
    let placed = SshBackend::place(&host.spec(false)).expect("the placement is arranged");
    assert!(
        host.worktree.join(".git").is_file(),
        "a host's checkout is a linked worktree, and its `.git` is the file that says so"
    );
    let dir = git(&host.worktree, &["rev-parse", "--absolute-git-dir"])
        .trim()
        .to_string();
    assert!(
        !Path::new(&dir).starts_with(&host.worktree),
        "and its repository is the mirror's, which is the path a guest's copy cannot carry: {dir}"
    );
    placed.close().expect("a close says where the work went");
}

/// A background job starts in a guest whose client forwards no stdin. The launcher is staged by
/// `Ssh::write_abs`, whose payload travels on the ssh's stdin, so the guest would wait for an end that
/// cannot come.
#[test]
fn a_background_job_starts_in_a_guest_whose_client_forwards_no_stdin() {
    let _turn = turns();
    let rig = Rig::vsock("jobstdin");
    let b = SshBackend::place(&rig.spec(true)).expect("the placement is arranged");
    let host = b.processes_arc().expect("this backend starts processes");
    let began = Instant::now();
    let id = host
        .spawn(&SpawnRequest {
            command: "echo started-marker; echo to-stderr >&2".into(),
            slug: Some("a-guest-job".into()),
            cwd: String::new(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: Vec::new(),
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("the launcher reaches the guest and the job starts");
    assert!(
        began.elapsed() < Duration::from_secs(30),
        "a job's start is not a wait: {:?}",
        began.elapsed()
    );

    let n = id.0.trim_start_matches('j').to_string();
    let out_file = rig.jobs.join(format!("{n}.out"));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut written = String::new();
    while Instant::now() < deadline {
        written = std::fs::read_to_string(&out_file).unwrap_or_default();
        if written.contains("to-stderr") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        written.contains("started-marker") && written.contains("to-stderr"),
        "the job ran in the guest and its output is {}: {written:?}",
        out_file.display()
    );
    // And the same bytes through the tool call a model would read them with.
    let slice = host.output(&id, 0, usize::MAX).expect("its output");
    assert!(slice.text().contains("started-marker"), "{}", slice.text());
    // The job is done with and its guest is brought down by the hook, which is quicker than a
    // session's close: nothing was committed, so that one waits out its copy-out window.
    letibot_tools::remote::guests_down();
}

/// A guest that nothing closed is brought down by the process's last act, once, and the line says where
/// the copy came out: a forced exit closes no backend, so a VM whose daemon ended that way would outlive
/// it with the calls stuck inside.
#[test]
fn a_guest_nothing_closed_is_brought_down_once_by_the_last_act() {
    let _turn = turns();
    let rig = Rig::vsock("lastact");
    let b = SshBackend::place(&rig.spec(true)).expect("the placement is arranged");
    let said = letibot_tools::remote::guests_down();
    assert_eq!(said.len(), 1, "one guest, one line: {said:?}");
    assert!(
        said[0].contains("was brought down"),
        "the line says the VM came down: {}",
        said[0]
    );
    assert!(
        said[0].contains(&format!("{}-landed", rig.worktree.display())),
        "and where the copy-out was delivered: {}",
        said[0]
    );
    let downs = rig
        .asked()
        .into_iter()
        .filter(|a| a.starts_with("down "))
        .count();
    assert_eq!(downs, 1, "down exactly once: {:?}", rig.asked());
    assert!(
        letibot_tools::remote::guests_down().is_empty(),
        "a guest is brought down once, so the second call has nothing to say"
    );
    drop(b);
}

/// A session that closed itself is not brought down a second time: its own close did it, and it
/// is what takes the guest off the hook's list.
#[test]
fn a_guest_its_own_session_closed_is_not_brought_down_again() {
    let _turn = turns();
    let rig = Rig::new("closedonce", true);
    let b = SshBackend::place(&rig.spec(true)).expect("the placement is arranged");
    // A commit in the guest's copy, so the close collects it instead of waiting out its window
    // for a copy that will never differ.
    let guest = PathBuf::from(format!("{}-landed", rig.worktree.display()));
    copy_tree(&rig.worktree, &guest);
    std::fs::write(guest.join("work.rs"), b"// the work\n").expect("a file");
    git(&guest, &["add", "work.rs"]);
    git(&guest, &["commit", "-q", "-m", "the work"]);
    b.close().expect("a close says where the work went");
    assert!(
        letibot_tools::remote::guests_down().is_empty(),
        "the close took the guest off the list: {:?}",
        rig.asked()
    );
    let downs = rig
        .asked()
        .into_iter()
        .filter(|a| a.starts_with("down "))
        .count();
    assert_eq!(downs, 1, "down exactly once: {:?}", rig.asked());
}

/// The perimeter's pairs reach a command INSIDE the guest - `firecode in` has no env flag, so a
/// pair is spelled in front of the command it wraps - and a loader variable is refused at the door.
#[test]
fn a_perimeters_env_reaches_the_guests_command() {
    let _turn = turns();
    let rig = Rig::new("env", true);
    let mut spec = rig.spec(true);
    spec.env = vec![
        ("A_PERIMETER_PAIR".into(), "a value".into()),
        ("BASH_ENV".into(), "/tmp/a-loader-nobody-meant".into()),
    ];
    let b = SshBackend::place(&spec).expect("the placement is arranged");
    let out = b
        .run(&Command {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "printf %s \"$A_PERIMETER_PAIR\"".into(),
            ],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect("the command runs in the guest");
    assert_eq!(out.exit, 0, "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "a value",
        "the pair did not reach the guest command's environment"
    );
    let asked = rig.asked().join("\n");
    assert!(
        asked.contains("A_PERIMETER_PAIR"),
        "the pair is not in the guest command firecode was handed: {asked}"
    );
    assert!(
        !asked.contains("a-loader-nobody-meant"),
        "a refused pair reached the host's own argv: {asked}"
    );
    let notes = b.placement_notes().join("\n");
    assert!(
        notes.contains("BASH_ENV") && notes.contains("not passed"),
        "the placement does not say it dropped the loader variable: {notes}"
    );
    // Off the process's guest list, which the last-act tests count.
    drop(b);
    letibot_tools::remote::guests_down();
}

/// A value whose provenance is a secret is refused rather than put in the guest's command string:
/// `firecode in` carries no descriptor, so the only other way in is the HOST's argv, which `ps`
/// there reads. Nothing is run, and both the sentence and the fake's record are checked.
#[test]
fn a_perimeters_secret_value_is_refused_inside_a_vm_and_never_reaches_an_argv() {
    const GIVEN: &str = "r121fixtureVMSECRETvalue0123456789";
    let _turn = turns();
    let rig = Rig::new("envsecret", true);
    let mut spec = rig.spec(true);
    spec.env_secrets = vec![("A_PLAINLY_NAMED_PAIR".into(), GIVEN.into())];
    let b = SshBackend::place(&spec).expect("the placement is arranged");
    let refused = b
        .run(&Command {
            argv: vec!["sh".into(), "-c".into(), "true".into()],
            cwd: String::new(),
            env: Vec::new(),
        })
        .expect_err("a VM has no channel for a secret, so nothing runs");
    let said = refused.to_string();
    assert!(
        said.contains("A_PLAINLY_NAMED_PAIR") && said.contains("Nothing was run"),
        "the refusal names the variable and says nothing ran: {said}"
    );
    let asked = rig.asked();
    assert!(
        !asked.iter().any(|a| a.contains(GIVEN)),
        "the secret is in what firecode was handed: {asked:?}"
    );
    assert_eq!(
        asked.iter().filter(|a| a.starts_with("in ")).count(),
        0,
        "a refused call reached the guest anyway: {asked:?}"
    );
    drop(b);
    letibot_tools::remote::guests_down();
}

/// One `firecode`-booting test at a time: the guests this process booted are one list, so a test
/// that inspects it must not run beside one that has a live guest.
fn turns() -> std::sync::MutexGuard<'static, ()> {
    static TURNS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TURNS.lock().unwrap_or_else(|e| e.into_inner())
}
