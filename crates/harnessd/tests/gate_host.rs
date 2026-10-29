//! **The gate on a declared host, end to end, against a fake ssh** — the second implementation of
//! [`letibot_harnessd::mergequeue::gate::GateRunner`] run through the real transport: a real
//! `rsync`, a real `git bundle`, real `git worktree`, real `ssh` argv and the real bounded runner.
//!
//! The "host" is a directory on this box. `$LETIBOT_SSH` is a script that skips ssh's own flags
//! and runs what is left — a shell command, or rsync's `--server` side — with `HOME` pointing at
//! the rig, so `root = "~/letibot-remote"` lands under the fixture and nothing here reaches a
//! network or the operator's own `~/letibot-remote`.
//!
//! What it pins, and why each one is here:
//!
//! | fact | measured by |
//! |---|---|
//! | the landing tree arrives, and so does the repository's git metadata | a step that reads a file AND resolves `main` |
//! | the steps run in order and the run stops at the first red one | a green step then a red one, and a third `not_run` |
//! | a red step's own output comes back as the row's evidence | the failing step's `echo` |
//! | `CARGO_TARGET_DIR` is the host's cache, outside the tree | a step that prints it |
//! | the second run of the same repository reuses the directory and sends almost nothing | the bundle size and the tree's presence |
//! | a host that cannot be reached is `Infra` with a sentence | a fake ssh that answers 255 |

use std::path::{Path, PathBuf};

use letibot_harnessd::mergequeue::gate::{GateControl, GateRun, GateVerdict, run_gate};
use letibot_tokencore::store::MergeGateOutcome;
use letibot_tools::exec::memory::Ask;

/// **One rig at a time.** `$HOME`, `$LETIBOT_HOSTS` and `$LETIBOT_SSH` are process-wide and this
/// file's tests run in parallel threads, so without this the second rig to start would point the
/// first one's ssh at its own hosts file — measured here as a green rig failing with the *down*
/// rig's `Connection refused`. Each test holds it for its whole body.
static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn only_rig() -> std::sync::MutexGuard<'static, ()> {
    ENV.lock().unwrap_or_else(|e| e.into_inner())
}

struct Rig {
    dir: PathBuf,
    repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The fake ssh: skip the flags and the destination, then run the rest. One argument means a
/// shell command (the daemon's own calls, wrapped in `bash -lc`); several mean rsync's `--server`
/// side, which is exec'd as it stands.
const FAKE_SSH: &str = "#!/bin/bash\n\
     export HOME=\"$FAKE_HOME\"\n\
     export PATH=\"$FAKE_PATH\"\n\
     while [ $# -gt 0 ]; do\n\
       case \"$1\" in\n\
         -i|-o|-p|-l|-F|-e|-c) shift 2;;\n\
         -*) shift;;\n\
         *) break;;\n\
       esac\n\
     done\n\
     shift\n\
     if [ $# -gt 1 ]; then exec \"$@\"; fi\n\
     exec bash -lc \"$1\"\n";

impl Rig {
    fn new(name: &str, ssh_body: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "letibot-gate-host-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        let home = dir.join("home");
        std::fs::create_dir_all(&home).expect("home");
        let dir = dir.canonicalize().unwrap_or(dir);
        let bin = dir.join("bin");
        let home = dir.join("home");
        let ssh = bin.join("ssh");
        letibot_tools::testing::write_stub(&ssh, ssh_body);
        // The operator's file — the only place a host may come from. `mise = false` because this
        // "host" is a directory on the box running the test, and a fake ssh inherits this
        // process's `PATH`: on a box with mise the default probe would wrap every call in it.
        let hosts = dir.join("hosts.toml");
        std::fs::write(
            &hosts,
            "[host.fake]\nhostname = \"fake.invalid\"\nuser = \"tester\"\n\
             root = \"~/letibot-remote\"\nmise = false\n",
        )
        .expect("a hosts file");
        let paths = letibot_tools::remote::Paths::in_home(&home);
        std::fs::create_dir_all(paths.keys()).expect("the keys directory");
        std::fs::write(paths.default_key("fake"), b"a dedicated key").expect("the key");
        std::fs::write(
            paths.known_hosts(),
            "fake.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n",
        )
        .expect("the host key");
        // SAFETY (Rust 2024's `unsafe` on `set_var`): this test binary is one file, these are read
        // by nothing else in it, and they are set before any thread that would read them starts.
        unsafe {
            std::env::set_var("LETIBOT_HOSTS", &hosts);
            std::env::set_var("LETIBOT_SSH", &ssh);
            std::env::set_var("HOME", &home);
            std::env::set_var("FAKE_HOME", &home);
            std::env::set_var("FAKE_PATH", std::env::var("PATH").unwrap_or_default());
        }
        // A repository with a `main`, and a branch one commit ahead of it: the shape a landing
        // tree is in when the gate runs.
        let repo = dir.join("the-repo");
        std::fs::create_dir_all(&repo).expect("repo");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "a test"]);
        std::fs::write(repo.join("README"), b"the first commit\n").expect("a file");
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-q", "-m", "the first commit"]);
        git(&repo, &["checkout", "-q", "-b", "agent/thing"]);
        std::fs::write(repo.join("BRANCH"), b"the branch's own file\n").expect("a file");
        git(&repo, &["add", "BRANCH"]);
        git(&repo, &["commit", "-q", "-m", "the branch's commit"]);
        Rig { dir, repo }
    }

    /// The landing tree as the queue leaves it: detached at the branch's tip, a `main` it can name.
    fn landing(&self) -> PathBuf {
        git(&self.repo, &["checkout", "-q", "--detach", "agent/thing"]);
        self.repo.clone()
    }

    fn gate(&self, steps: &[&str], seconds: u64) -> GateRun {
        let stop = std::sync::atomic::AtomicBool::new(false);
        let alive = || true;
        let ctl = GateControl {
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(seconds),
            stop: &stop,
            alive: &alive,
        };
        let steps: Vec<String> = steps.iter().map(|s| s.to_string()).collect();
        run_gate(Some("fake"), &self.landing(), &steps, &ctl, Ask::Uncapped)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// **The tree and the repository's metadata both arrive** — the two transports, measured by one
/// step that could not pass without either: a file from the working tree, `main` from the bundle,
/// and `git diff main HEAD` naming the branch's own file.
///
/// And the build cache is the host's, outside the tree, so a `git clean` or an `rsync --delete`
/// cannot take it.
#[test]
fn the_tree_and_the_repository_arrive_and_the_steps_run_there() {
    let _only = only_rig();
    let rig = Rig::new("green", FAKE_SSH);
    let run = rig.gate(
        &[
            "cat README",
            "git rev-parse --verify main >/dev/null && git diff --name-only main HEAD",
            "echo \"cache=$CARGO_TARGET_DIR\"; git status --porcelain -- '*.rs'",
        ],
        120,
    );
    assert_eq!(run.verdict, GateVerdict::Passed, "{run:#?}");
    // The working tree is the landing tree's, byte for byte. (Every row's output ends with the
    // `[gate memory: …]` line, which is the operator's and not the step's.)
    assert!(
        run.steps[0].output.starts_with("the first commit\n"),
        "{run:#?}"
    );
    // `main` resolves and `HEAD` is the landing commit: the bundle, not just the files.
    assert_eq!(
        run.steps[1].output.lines().next(),
        Some("BRANCH"),
        "{run:#?}"
    );
    // The cache is a directory of the gate's own, and it is not inside the tree that gets reset.
    let out = &run.steps[2].output;
    let cache = out
        .lines()
        .find_map(|l| l.strip_prefix("cache="))
        .expect("the cache line");
    assert!(
        cache.contains("/letibot-remote/gate/") && cache.ends_with("/target"),
        "the host's own cache, named by the gate: {cache}"
    );
    assert!(!cache.contains("/tree/"), "{cache}");
    // A tree that arrived clean is clean: nothing untracked, nothing modified.
    assert!(
        out.contains("cache=") && !out.contains("?? ") && !out.contains(" M "),
        "git status says the tree is the commit: {out}"
    );
    assert_eq!(run.steps[0].outcome, MergeGateOutcome::Passed);
}

/// **A step that fails on the host is red, the run stops there, and its own output is the row's
/// evidence** — the mapping the queue's notice is built from.
#[test]
fn a_step_that_fails_on_the_host_is_red_and_the_next_one_never_runs() {
    let _only = only_rig();
    let rig = Rig::new("red", FAKE_SSH);
    let run = rig.gate(
        &[
            "echo hello",
            "echo 'the host said no'; exit 3",
            "echo never",
        ],
        120,
    );
    let GateVerdict::Failed(why) = &run.verdict else {
        panic!("{run:#?}");
    };
    assert!(
        why.contains("`echo 'the host said no'; exit 3` failed"),
        "{why}"
    );
    assert!(why.contains("the host said no"), "{why}");
    assert_eq!(run.steps[0].outcome, MergeGateOutcome::Passed);
    assert_eq!(run.steps[1].outcome, MergeGateOutcome::Failed);
    assert!(
        run.steps[1].output.contains("the host said no"),
        "{:#?}",
        run.steps[1]
    );
    // The exit is recorded, so a red step can be told from one that judged nothing.
    assert!(
        run.steps[1].output.contains("[step: exit code 3]"),
        "{:#?}",
        run.steps[1]
    );
    // A step the gate never reached is `not_run`, not `passed`.
    assert_eq!(run.steps[2].outcome, MergeGateOutcome::NotRun);
    assert!(run.steps[2].output.is_empty());
    // And the host step's row says where it ran, rather than claiming a cgroup this daemon
    // never made.
    assert!(
        run.steps[0].output.contains("the step ran on `fake`"),
        "{:#?}",
        run.steps[0]
    );
}

/// **A step still running at the deadline is killed on the host and the row is `timed_out`** —
/// and nothing is left running there. The deadline is the gate's own, one second in this test.
#[test]
fn a_step_that_outlives_the_deadline_is_killed_on_the_host() {
    let _only = only_rig();
    let rig = Rig::new("deadline", FAKE_SSH);
    let started = std::time::Instant::now();
    let run = rig.gate(&["echo started; sleep 60; echo finished"], 2);
    assert!(matches!(run.verdict, GateVerdict::TimedOut(_)), "{run:#?}");
    assert_eq!(run.steps[0].outcome, MergeGateOutcome::TimedOut);
    assert!(
        run.steps[0].output.contains("started"),
        "{:#?}",
        run.steps[0]
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(45),
        "the gate waited {:?}, which is the far side's own 60 s rather than the deadline",
        started.elapsed()
    );
    // The far side's group was killed: no `sleep 60` of this gate is left behind.
    let out = std::process::Command::new("ps")
        .args(["-eo", "command"])
        .output()
        .expect("ps");
    let listing = String::from_utf8_lossy(&out.stdout);
    let mine: Vec<&str> = listing
        .lines()
        .filter(|l| l.contains("sleep 60") && l.contains(&rig.dir.display().to_string()))
        .collect();
    assert!(mine.is_empty(), "stranded on the host: {mine:?}");
}

/// **A host that cannot be reached is a verdict with a sentence, in seconds** — not a hang and
/// not a red branch. The fake ssh answers the way a dead machine's does.
#[test]
fn an_unreachable_host_is_unjudged_with_a_sentence() {
    let _only = only_rig();
    let rig = Rig::new(
        "down",
        "#!/bin/sh\necho 'ssh: connect to host fake.invalid port 22: Connection refused' >&2\n\
         exit 255\n",
    );
    let started = std::time::Instant::now();
    let run = rig.gate(&["echo hello"], 300);
    let GateVerdict::Infra(why) = &run.verdict else {
        panic!("{run:#?}");
    };
    assert!(why.contains("UNJUDGED, not red"), "{why}");
    assert!(why.contains("Connection refused"), "{why}");
    assert!(
        why.contains("fake"),
        "the sentence names the machine: {why}"
    );
    assert_eq!(run.steps[0].outcome, MergeGateOutcome::NotRun);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "a dead host must not cost the deadline: {:?}",
        started.elapsed()
    );
}

/// **The second gate of one repository on one host reuses everything the first one made** — the
/// same directory, the same worktree and the same build cache — and sends only what is new.
///
/// The first run's bundle carries the whole history; the second one's is a handful of commits,
/// which is what makes a warm gate on a host worth having at all.
#[test]
fn the_second_run_of_one_repository_is_warm() {
    let _only = only_rig();
    let rig = Rig::new("warm", FAKE_SSH);
    let first = rig.gate(&["echo \"cache=$CARGO_TARGET_DIR\"; echo first"], 120);
    assert_eq!(first.verdict, GateVerdict::Passed, "{first:#?}");
    let cache = first.steps[0]
        .output
        .lines()
        .find_map(|l| l.strip_prefix("cache="))
        .expect("the cache line")
        .to_string();
    // What the first gate left on the "host": a bare store, a worktree and the cache directory.
    let gate_dir = Path::new(&cache).parent().expect("the gate's directory");
    assert!(gate_dir.join("src.git").is_dir(), "{gate_dir:?}");
    assert!(gate_dir.join("tree").is_dir(), "{gate_dir:?}");
    assert!(gate_dir.join("target").is_dir(), "{gate_dir:?}");
    // A second run of the same tip: the host already has the commit, so no bundle is sent at all.
    let second = rig.gate(&["echo second"], 120);
    assert_eq!(second.verdict, GateVerdict::Passed, "{second:#?}");
    assert_eq!(second.steps[0].output.lines().next(), Some("second"));
    // And one new commit on top is still a warm run. It goes on the BRANCH, because `landing()`
    // detaches at `agent/thing`'s tip and a commit made on a detached HEAD is not that tip.
    git(&rig.repo, &["checkout", "-q", "agent/thing"]);
    std::fs::write(rig.repo.join("SECOND"), b"another\n").expect("a file");
    git(&rig.repo, &["add", "SECOND"]);
    git(&rig.repo, &["commit", "-q", "-m", "another commit"]);
    let third = rig.gate(&["cat SECOND"], 120);
    assert_eq!(third.verdict, GateVerdict::Passed, "{third:#?}");
    assert_eq!(third.steps[0].output.lines().next(), Some("another"));
}
