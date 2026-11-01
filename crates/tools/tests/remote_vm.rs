//! **A session whose tools run in a firecode VM on another machine** — R100, against a fake
//! `firecode` and a fake `ssh`.
//!
//! # No network, no real host, and no VM
//!
//! The `ssh` is a script this file writes: it takes the flags the backend hands it, ignores
//! them, and runs the last argument locally with `HOME` set to a temporary directory standing
//! in for the far machine's home — **and with a login `PATH` written by that home's own
//! `.bash_profile`**, so nothing the box has installed (a real `firecode`, a toolchain) can
//! answer a question this suite is asking of a fixture. The `firecode` is a second script on
//! that home's `bin`, which records what it was asked and behaves like the client: `in` runs the
//! command it was given, `up` and `down` succeed, and `down` leaves the guest's tree in a sibling
//! directory the way the real one does when something changed.
//!
//! What that proves is the placement's own shape — the order of `up`, `in` and `down`, that a
//! command of the session's reaches the guest and nothing of it reaches the host, that a commit
//! made inside the guest comes home, and that a host which cannot boot a guest is refused by
//! name. What it cannot prove is firecode itself: whether a real `up` boots, whether a real
//! `down` delivers, and what a real guest's copy-out costs are the live proof's business.

use std::path::{Path, PathBuf};
use std::time::Duration;

use letibot_tools::backend::{Command, ExecBackend};
use letibot_tools::exec::Egress;
use letibot_tools::remote::{Host, Mise, Paths, SshBackend, SshSpec};

/// The short id this suite's session gets: the last eight of `s-01abcdef`.
const SHORT: &str = "01abcdef";

/// One fake remote: a home, a repository to send, an `ssh`, and a `firecode` that answers.
struct Rig {
    dir: PathBuf,
    /// Where the checkout lands on the "remote".
    worktree: PathBuf,
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
        // **A host name of this rig's own.** What a host answered about its `firecode` is cached
        // per name for the life of the process, so two rigs sharing one name would each be told
        // the other's answer — the same rule `remote_hosts.rs` follows for the breaker.
        let name = format!("fake-{tag}");
        let hostname = format!("{name}.invalid");
        let bin = dir.join("bin");
        let home = dir.join("home");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::create_dir_all(&home).expect("home");
        // **The login shell's PATH is the FIXTURE's, and there is nothing else on it.** `bash -lc`
        // reads `/etc/profile` first and that file is the BOX's: on a machine with a real
        // `firecode` in `~/bin`, `command -v firecode` found it and the refusal this suite tests
        // never happened (MEASURED on the gate, 2026-11-01). A `~/.bash_profile` runs after
        // `/etc/profile`, so this replaces whatever the box put there with the fixture's own
        // `bin` and the system directories — and a test's answer cannot depend on what is
        // installed on the machine running it.
        std::fs::write(
            home.join(".bash_profile"),
            format!(
                "PATH={bin}:/usr/bin:/bin\nexport PATH\n",
                bin = bin.display()
            ),
        )
        .expect("the fake home's login profile");
        let asked = dir.join("firecode.log");

        // **The device node the probe asks about.** A box has one or it does not, and this
        // fixture cannot make `/dev/kvm` — so the fake remote says where to look, which is the
        // door `$LETIBOT_KVM_DEVICE` exists for.
        let kvm = dir.join("kvm");
        std::fs::write(&kvm, b"a stand-in for the device node").expect("the kvm stand-in");

        if firecode {
            write_executable(
                &bin.join("firecode"),
                &format!(
                    "#!/bin/bash\n\
                     [ $# -eq 0 ] && exit 0\n\
                     printf 'ARGV %s\\n' \"$*\" >> '{asked}'\n\
                     verb=$1; shift\n\
                     case \"$verb\" in\n\
                     \x20 in)\n\
                     \x20   for last; do :; done\n\
                     \x20   exec bash -lc \"$last\" ;;\n\
                     \x20 down)\n\
                     \x20   project=\n\
                     \x20   while [ $# -gt 0 ]; do [ \"$1\" = --project ] && project=$2; shift; done\n\
                     \x20   [ -n \"$project\" ] && [ ! -e \"$project-landed\" ] && \
                     \x20     cp -a \"$project\" \"$project-landed\" ;;\n\
                     esac\n\
                     exit 0\n",
                    asked = asked.display()
                ),
            );
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
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "a test"]);
        std::fs::write(repo.join("README"), b"the first commit\n").expect("a file");
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-q", "-m", "the first commit"]);

        let worktree = home.join("letibot-remote").join("the-repo").join(SHORT);
        Rig {
            dir,
            worktree,
            repo,
            ssh,
            asked,
            paths,
            // A fixture's host has no mise: the wrapper is a person's toolchain on their own
            // machine, and a test that asked a real one where its `cargo` is would be measuring
            // the box it runs on.
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
            egress: Egress::Denied,
            // A VM is the boundary, so nothing is probed for `bwrap`: the fake remote has none
            // and `Some(false)` is what says *do not ask this box what it can confine*.
            confine: Some(false),
            vm,
        }
    }

    /// **What the fake `firecode` was asked**, one entry per call, the verb first.
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

/// A tree copied wholesale — the guest's own copy of the checkout.
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
    let out = std::process::Command::new("git")
        // **A fixture's repository does not inherit the box's signing key**: this one signs
        // every commit by habit, and a key a sandbox has not got is not this test's subject.
        .arg("-c")
        .arg("commit.gpgsign=false")
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

/// **`up`, then `in` for the session's own calls, then `down`** — in that order, and with the
/// command inside the guest rather than on the host.
#[test]
fn a_vm_placement_boots_the_guest_and_runs_every_call_in_it() {
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

/// **A commit made inside the guest comes home.** The guest holds a copy, so its work reaches
/// the branch only because `down` leaves that copy beside the checkout and the close takes it
/// onto the checkout the branch comes back from.
///
/// The fixture's stand-in for the guest's copy is that sibling directory: the test makes it with
/// the guest's work in it, and the fake's `down` leaves a copy that is already there alone.
#[test]
fn a_commit_made_in_the_guest_comes_back_as_the_branch() {
    let rig = Rig::new("commit", true);
    let b = SshBackend::place(&rig.spec(true)).expect("the placement is arranged");

    // **The guest's tree, a copy of the checkout at the same path.** It is a git worktree, so a
    // copy of it resolves the same gitdir the original does.
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
    /// The repository a session on this rig is opened in — the local one, which is where the
    /// branch comes back to.
    fn local_checkout(&self) -> PathBuf {
        self.repo.clone()
    }
}

/// **A host with no `firecode` is refused by name**, before anything is arranged there.
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

/// **The same host, without `--vm`, places as it always did** — the refusal is about the guest
/// and not about the machine, and a host placement must not be made to pay for the probe.
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
