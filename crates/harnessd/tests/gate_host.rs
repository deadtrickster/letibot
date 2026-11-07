//! The gate on a declared host, end to end, against a fake ssh - the second implementation
//! of `GateRunner` run through the real transport: a real `rsync`, a real `git bundle` real
//! `git worktree`, real `ssh` argv and the real bounded runner.

use std::path::{Path, PathBuf};

use letibot_harnessd::mergequeue::gate::{GateControl, GateRun, GateVerdict, run_gate};
use letibot_tokencore::store::MergeGateOutcome;
use letibot_tools::exec::memory::Ask;

/// One rig at a time. `$HOME`, `$LETIBOT_HOSTS` and `$LETIBOT_SSH` are process-wide and this
/// file's tests run in parallel threads, so without this the second rig to start would point the
/// first one's ssh at its own hosts file. Each test holds it for its whole body.
static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn only_rig() -> std::sync::MutexGuard<'static, ()> {
    ENV.lock().unwrap_or_else(|e| e.into_inner())
}

/// How long a wait may last before it is the thing that failed - a ceiling on *the far side
/// never ended*, never a claim about how long anything took. The merge gate runs every test binary
/// at once, so a bound a loaded box can reach would red `main` for the machine, not for the gate.
const CEILING: std::time::Duration = std::time::Duration::from_secs(180);

/// Run something that blocks, and fail only when the fact it is about never comes. A gate run
/// is synchronous, so waiting for it IS the fact; [`CEILING`] is what a reader is told when it did
/// not come.
fn within<T: Send>(what: &str, f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        let run = s.spawn(f);
        let began = std::time::Instant::now();
        while !run.is_finished() {
            assert!(began.elapsed() < CEILING, "{what}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        match run.join() {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

/// Every process on this box whose command line names the rig's directory - the "host" is a
/// directory here, so this is what *still running on the host* looks like: a step's launcher is
/// `bash <rig>/…/run/step-N.sh`, and the step itself is that launcher's group.
fn running_under(rig: &Rig) -> Vec<String> {
    let out = std::process::Command::new("ps")
        .args(["-eo", "command"])
        .output()
        .expect("ps");
    let listing = String::from_utf8_lossy(&out.stdout);
    let mine = rig.dir.display().to_string();
    listing
        .lines()
        .filter(|l| l.contains(&mine))
        .map(str::to_string)
        .collect()
}

struct Rig {
    dir: PathBuf,
    repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = letibot_tools::testing::git(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The fake ssh: skip the flags and the destination, then run the rest. One argument means a shell
/// command (the daemon's own calls, wrapped in `bash -lc`); several mean rsync's `--server` side
/// which is exec'd as it stands.
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

/// The same fake ssh with one more job: it answers the apparatus probe from `$FAKE_APPARATUS`
/// instead of running it. A real probe would ask about the box the test happens to run on - a Mac
/// answers *no cgroup, no bwrap* - so the assertion would be about the machine, not the fixture.
const FAKE_SSH_ANSWERS: &str = "#!/bin/bash\n\
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
     case \"$1\" in *letibot-apparatus-probe*) printf '%s\\n' \"$FAKE_APPARATUS\"; exit 0;; esac\n\
     exec bash -lc \"$1\"\n";

impl Rig {
    fn new(name: &str, ssh_body: &str) -> Rig {
        let dir = letibot_sessionlog::testing::scratch(&format!("letibot-gate-host-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        let home = dir.join("home");
        std::fs::create_dir_all(&home).expect("home");
        let dir = dir.canonicalize().unwrap_or(dir);
        let bin = dir.join("bin");
        let home = dir.join("home");
        let ssh = bin.join("ssh");
        letibot_tools::testing::write_stub(&ssh, ssh_body);
        // The operator's file - the only place a host may come from. `mise = false` because this
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
        letibot_tools::testing::repo_config(&repo);
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

    /// What the "host" answers when it is asked what apparatus it has - set before the gate
    /// runs, read by [`FAKE_SSH_ANSWERS`]. Process-wide, which is why every test here holds
    /// [`only_rig`].
    fn answers(&self, said: &str) {
        // SAFETY (Rust 2024's `unsafe` on `set_var`): one rig at a time (`ENV`), set before the
        // gate starts, and read only by the fake ssh this test started.
        unsafe { std::env::set_var("FAKE_APPARATUS", said) };
    }

    /// A program the "host" will find on its `PATH` - the fake ssh exports `$FAKE_PATH` as the
    /// far side's `PATH`, so this is how a test stands a command in front of one the box has.
    fn host_program(&self, name: &str, body: &str) -> PathBuf {
        let path = self.dir.join("bin").join(name);
        letibot_tools::testing::write_stub(&path, body);
        let path_now = std::env::var("PATH").unwrap_or_default();
        // SAFETY: as `answers` above - one rig, before the gate starts.
        unsafe {
            std::env::set_var(
                "FAKE_PATH",
                format!("{}:{path_now}", self.dir.join("bin").display()),
            )
        };
        path
    }

    /// The n-th launcher the host was asked to run - the step, as the machine received it. The
    /// "host" is a directory here, so a test can read what a real gate could only see in a row.
    fn launcher(&self, n: usize) -> String {
        let run = self
            .dir
            .join("home/letibot-remote/gate")
            .join(letibot_harnessd::mergequeue::gate_host::dir_name(
                &self.repo,
            ))
            .join("run");
        let mut steps: Vec<(u64, PathBuf)> = std::fs::read_dir(&run)
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        let k = name.strip_prefix("step-")?.strip_suffix(".sh")?;
                        Some((k.parse::<u64>().ok()?, e.path()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        steps.sort();
        let Some((_, path)) = steps.get(n) else {
            panic!(
                "the host was asked to run {} step(s), not {n} + 1",
                steps.len()
            );
        };
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn gate(&self, steps: &[&str], seconds: u64) -> GateRun {
        let stop = std::sync::atomic::AtomicBool::new(false);
        let alive = || true;
        let ctl = GateControl {
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(seconds),
            budget: std::time::Duration::from_secs(seconds),
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

/// The tree and the repository's metadata both arrive - the two transports, measured by one
/// step that could not pass without either: a file from the working tree, `main` from the
/// bundle and `git diff main HEAD` naming the branch's own file.
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

/// A step that fails on the host is red, the run stops there, and its own output is the row's
/// evidence - the mapping the queue's notice is built from.
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

/// A step still running at the deadline is killed on the host and the row is `timed_out` -
/// and nothing is left running there. "At the deadline, not at the far side's end" is a
/// fact about the STEP, not a stopwatch over the box
#[test]
fn a_step_that_outlives_the_deadline_is_killed_on_the_host() {
    let _only = only_rig();
    let rig = Rig::new("deadline", FAKE_SSH);
    let step = "echo started; i=0; while [ $i -lt 120 ]; do i=$((i+1)); echo \"beat $i\"; \
                sleep 1; done; echo finished";
    let run = within(
        "the gate never returned from a step that only its deadline could end: the step is still \
         running and its own end is 120 s away, so the gate is waiting for the far side",
        // **Ninety seconds, and the deadline covers the TRANSFER too.** At 15 s the gate gave up
        // getting the tree onto the host - `NotRun` and `Infra`, "the gate's deadline passed before
        // the tree was on the host" - measured on the merge gate, which is a loaded box. The step
        // still runs its own 120 s, so a step killed at 90 is still killed at the GATE's clock and
        // not at the far side's, which is the whole of what this asserts.
        || rig.gate(&[step], 90),
    );
    assert!(matches!(run.verdict, GateVerdict::TimedOut(_)), "{run:#?}");
    assert_eq!(run.steps[0].outcome, MergeGateOutcome::TimedOut);
    // The sentence names the step's own bound - the gate's deadline, here ninety seconds, and
    // not any poll's: a step is minutes and a question about the host is seconds, and a verdict
    // that does not say which clock ran out cannot be acted on.
    let GateVerdict::TimedOut(why) = &run.verdict else {
        unreachable!()
    };
    assert!(
        why.contains("the gate's own deadline passed (90 s)"),
        "{why}"
    );
    let out = &run.steps[0].output;
    assert!(out.contains("started"), "the step's own words: {out}");
    // It never reached its own end - a step that printed `finished` is one the gate waited for.
    assert!(
        !out.contains("finished"),
        "the step reached its own end, so the gate waited for the far side rather than for its \
         deadline: {out}"
    );
    // And the step's own count says the kill came from the deadline. `beat N` is the far side's
    // own arithmetic, one per second it lived, so a loaded box can only make this number SMALLER
    // the assertion cannot fire for the machine.
    let beats = out
        .lines()
        .filter_map(|l| l.trim().strip_prefix("beat "))
        .filter_map(|n| n.trim().parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    // **The number that made this pass at a 2 s deadline was `< 20`, and at ninety it would fail
    // for the wrong reason:** the step lives as long as the deadline allows and prints one `beat N` a
    // second, so a healthy run prints about ninety of them. What the count proves is that the step
    // did NOT reach its own 120 s - which is the distinction this test exists for.
    assert!(
        beats < 110,
        "the step ran {beats} of its own seconds before the gate ended it, and the step's own end is \
         120 s: something other than the gate's deadline ended this step: {out}"
    );
    // The far side's group was killed, and this waits for that fact rather than assuming it: a
    // killed group takes a moment to be reaped.
    let began = std::time::Instant::now();
    loop {
        let stranded = running_under(&rig);
        if stranded.is_empty() {
            break;
        }
        assert!(
            began.elapsed() < CEILING,
            "the far side's process group was not killed — still running on the host: {stranded:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// A fake ssh whose polls are slow. The steps are `echo`s and finish in milliseconds; what
/// takes the time is the QUESTION, because the machine was busy compiling something else. The first
/// poll sleeps twelve seconds (longer than the ten a poll was once allowed), the second 35.
const SLOW_POLL_SSH: &str = "#!/bin/bash\n\
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
     case \"$1\" in\n\
       *@@letibot-gate-*)\n\
         n=$(cat \"$HOME/polls\" 2>/dev/null || echo 0)\n\
         n=$((n+1))\n\
         echo $n > \"$HOME/polls\"\n\
         case $n in 1) sleep 12;; 2) sleep 35;; esac\n\
         ;;\n\
     esac\n\
     exec bash -lc \"$1\"\n";

/// A host busy enough to be slow to answer is not a host that failed the gate, twice over *
/// a poll that takes twelve seconds is an answer, not a failure - the bound is a question
/// about the machine, and a busy machine takes seconds to fork a shell * a poll that misses
#[test]
fn a_slow_poll_is_asked_again_and_does_not_kill_the_step() {
    let _only = only_rig();
    let rig = Rig::new("slowpoll", SLOW_POLL_SSH);
    // The gate's deadline is far above what these two steps cost, the 12 s and the 35 s the
    // fake host spends answering included: what this test measures is the RETRY, and a deadline a
    // loaded box could reach would measure the box instead.
    let run = rig.gate(&["echo the first step", "echo the second step"], 300);
    assert_eq!(run.verdict, GateVerdict::Passed, "{run:#?}");
    assert_eq!(run.steps[0].outcome, MergeGateOutcome::Passed);
    assert!(
        run.steps[0].output.starts_with("the first step"),
        "{:#?}",
        run.steps[0]
    );
    assert_eq!(run.steps[1].outcome, MergeGateOutcome::Passed);
    assert!(
        run.steps[1].output.starts_with("the second step"),
        "{:#?}",
        run.steps[1]
    );
    // The miss was asked again - the host's own count of the polls it was given. Its second
    // answer sleeps 35 s, past the 30 s a poll is allowed, so a third ask can only happen if the
    // miss was retried rather than read as a verdict about the step.
    let asked = std::fs::read_to_string(rig.dir.join("home/polls"))
        .map(|s| s.trim().parse::<u32>().unwrap_or(0))
        .unwrap_or(0);
    assert!(
        asked >= 3,
        "the 35 s poll was never asked again: the host was asked {asked} poll(s) in all and the \
         gate answered {:?} — a missed poll was read as a verdict about the step",
        run.verdict
    );
}

/// The gate's log says where a first run's time went - transport, checkout, and whether the
/// build cache was this gate's own. A reader who cannot see that reads a slow build as a slow
/// machine.
#[test]
fn the_first_rows_note_says_what_the_transport_cost_and_that_the_cache_was_cold() {
    let _only = only_rig();
    let rig = Rig::new("costnote", FAKE_SSH);
    // A step that puts something IN the cache, because an empty `target/` is not a cache - the
    // question `open` asks is whether this gate is the one that makes it.
    let first = rig.gate(
        &["echo one > \"$CARGO_TARGET_DIR/made-by-the-first-gate\"; echo one"],
        120,
    );
    assert_eq!(first.verdict, GateVerdict::Passed, "{first:#?}");
    let note = first.steps[0]
        .output
        .lines()
        .find(|l| l.starts_with("[gate on fake:"))
        .unwrap_or_else(|| panic!("no cost note in {:#?}", first.steps[0]));
    assert!(note.contains("the repository "), "{note}");
    assert!(note.contains("the checkout "), "{note}");
    assert!(note.contains("the tree "), "{note}");
    // The transport is said beside the budget it is paid out of, so *two seconds* reads as cheap
    // and not as *most of the gate* - this rig gives the gate two minutes.
    assert!(note.contains("out of the gate's 2 min"), "{note}");
    assert!(note.contains("build cache COLD"), "{note}");
    assert!(!note.contains("not reached"), "{note}");
    // The second gate reuses the cache it made, and the note says so rather than claiming another
    // cold build.
    let second = rig.gate(&["echo two"], 120);
    assert_eq!(second.verdict, GateVerdict::Passed, "{second:#?}");
    assert!(
        second.steps[0]
            .output
            .contains("[gate on fake: the repository "),
        "{:#?}",
        second.steps[0]
    );
    assert!(
        second.steps[0].output.contains("build cache warm"),
        "{:#?}",
        second.steps[0]
    );
    assert!(
        !second.steps[0].output.contains("build cache COLD"),
        "{:#?}",
        second.steps[0]
    );
}

/// A host that cannot be reached is a verdict with a sentence, at once - not a hang and not a
/// red branch. The fake ssh answers the way a dead machine's does, and the gate must read that
/// refusal rather than wait out the deadline it was given.
#[test]
fn an_unreachable_host_is_unjudged_with_a_sentence() {
    let _only = only_rig();
    let rig = Rig::new(
        "down",
        "#!/bin/sh\necho 'ssh: connect to host fake.invalid port 22: Connection refused' >&2\n\
         exit 255\n",
    );
    let run = within(
        "a host that refuses the connection was not judged: the gate is waiting out its own \
         15 min deadline instead of reading the refusal",
        || rig.gate(&["echo hello"], 900),
    );
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
}

/// The second gate of one repository on one host reuses everything the first one made - the
/// same directory, the same worktree and the same build cache - and sends only what is new.
/// The first run's bundle carries the whole history
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
    // And one new commit on top is still a warm run. It goes on the BRANCH, because `landing`
    // detaches at `agent/thing`'s tip and a commit made on a detached HEAD is not that tip.
    git(&rig.repo, &["checkout", "-q", "agent/thing"]);
    std::fs::write(rig.repo.join("SECOND"), b"another\n").expect("a file");
    git(&rig.repo, &["add", "SECOND"]);
    git(&rig.repo, &["commit", "-q", "-m", "another commit"]);
    let third = rig.gate(&["cat SECOND"], 120);
    assert_eq!(third.verdict, GateVerdict::Passed, "{third:#?}");
    assert_eq!(third.steps[0].output.lines().next(), Some("another"));
}

// -------------------------------------------------------------------------------------------
// The demand a step makes of the MACHINE `LETIBOT_REQUIRE_APPARATUS=1` is the project's
// and it is right: a test that skips must not read as a pass.

/// A machine that cannot meet the demand runs the step without it, and the row says so. The
/// prefix comes off that ONE step and nothing else about it moves, and the evidence a
/// person reads leads with the fact
#[test]
fn a_machine_that_cannot_meet_the_demand_runs_the_step_without_it_and_says_so() {
    let _only = only_rig();
    let rig = Rig::new("apparatus", FAKE_SSH_ANSWERS);
    rig.answers("cgroup=no\nbwrap=yes\n");
    let run = rig.gate(
        &[
            "echo the first step",
            "LETIBOT_REQUIRE_APPARATUS=1 sh -c 'echo demand=[$LETIBOT_REQUIRE_APPARATUS]'",
        ],
        120,
    );
    assert_eq!(run.verdict, GateVerdict::Passed, "{run:#?}");
    // What the machine was actually asked to run: the assignment is gone from the step's head
    // and nothing else about it moved. (Asserted on the launcher rather than on the environment
    // because a gate run under `LETIBOT_REQUIRE_APPARATUS=1` would export it to every child.)
    let launcher = rig.launcher(1);
    assert!(
        launcher.contains("( sh -c 'echo demand=[$LETIBOT_REQUIRE_APPARATUS]' )"),
        "{launcher}"
    );
    assert!(
        !launcher.contains("LETIBOT_REQUIRE_APPARATUS=1"),
        "{launcher}"
    );
    // The row leads with the fact, and says it in the tree's own idiom.
    let first = &run.steps[0].output;
    assert!(
        first.starts_with("[gate on `fake`: `LETIBOT_REQUIRE_APPARATUS=1` was TAKEN OFF 1 of 2"),
        "{first}"
    );
    assert!(
        first.contains("a cgroup v2 subtree this user may write (the process-lifetime tree)"),
        "{first}"
    );
    assert!(first.contains("THIS IS NOT A PASS"), "{first}");
    // The step that made no demand is not touched, and no step is wrapped.
    assert!(
        rig.launcher(0).contains("( echo the first step )"),
        "{}",
        rig.launcher(0)
    );
}

/// A machine that HAS the apparatus runs the project's step exactly as it was written, and the
/// row says nothing - a gate on a box with everything is byte for byte what it was.
#[test]
fn a_machine_with_the_apparatus_runs_the_demand_as_the_project_wrote_it() {
    let _only = only_rig();
    let rig = Rig::new("hasapparatus", FAKE_SSH_ANSWERS);
    rig.answers("cgroup=here\nbwrap=yes\n");
    let run = rig.gate(
        &["LETIBOT_REQUIRE_APPARATUS=1 echo 'the demand is on the step'"],
        120,
    );
    assert_eq!(run.verdict, GateVerdict::Passed, "{run:#?}");
    let launcher = rig.launcher(0);
    assert!(
        launcher.contains("( LETIBOT_REQUIRE_APPARATUS=1 echo 'the demand is on the step' )"),
        "{launcher}"
    );
    assert!(!launcher.contains("systemd-run"), "{launcher}");
    let out = &run.steps[0].output;
    assert!(out.starts_with("the demand is on the step"), "{out}");
    assert!(!out.contains("TAKEN OFF"), "{out}");
    assert!(!out.contains("delegated scope"), "{out}");
}

/// A machine that has no cgroup of its own to give, but can make a delegated scope, gets
/// the demand MET - the step is started inside `systemd-run --user --scope -p Delegate=yes`
/// and the prefix stays where the project put it
#[test]
fn a_machine_that_can_make_a_delegated_scope_keeps_the_demand_and_gets_one() {
    let _only = only_rig();
    let rig = Rig::new("ownscope", FAKE_SSH_ANSWERS);
    rig.answers("cgroup=own-scope\nbwrap=yes\n");
    // A stand-in for `systemd-run`, on the fake host's `PATH`: it records that it was asked and
    // runs what it was given, which is the contract the launcher depends on. With no arguments it
    // does nothing, which is what `write_stub`'s probe requires of every stub.
    rig.host_program(
        "systemd-run",
        "#!/bin/sh\n\
         [ $# -gt 0 ] || exit 0\n\
         [ -n \"$FAKE_HOME\" ] && echo \"$@\" > \"$FAKE_HOME/systemd-run-was-asked\"\n\
         while [ $# -gt 0 ]; do case \"$1\" in --) shift; break;; *) shift;; esac; done\n\
         exec \"$@\"\n",
    );
    let run = rig.gate(
        &["LETIBOT_REQUIRE_APPARATUS=1 echo 'the demand stayed'"],
        120,
    );
    assert_eq!(run.verdict, GateVerdict::Passed, "{run:#?}");
    // The prefix is still on the step, and the step ran inside the scope.
    let launcher = rig.launcher(0);
    assert!(
        launcher.contains(
            "( exec systemd-run --user --scope -p Delegate=yes --quiet -- bash -c \
             'LETIBOT_REQUIRE_APPARATUS=1 echo '\\''the demand stayed'\\''' )"
        ),
        "{launcher}"
    );
    let asked = std::fs::read_to_string(rig.dir.join("home/systemd-run-was-asked"))
        .expect("the scope was asked for");
    assert!(asked.contains("-p Delegate=yes"), "{asked}");
    // The row leads with what the gate did, and the step's own words follow it.
    let out = &run.steps[0].output;
    assert!(out.contains("\nthe demand stayed"), "{out}");
    assert!(out.contains("delegated scope of its own"), "{out}");
    assert!(!out.contains("TAKEN OFF"), "{out}");
}

/// The net under all of it: a step that refuses to skip is UNJUDGED, not red, whatever the
/// probe said. Twenty-one targets failing with one sentence is a fact about the machine, and a gate
/// that reds for it is a gate whose red is ignored.
#[test]
fn a_step_that_refused_to_skip_is_unjudged_and_not_red() {
    let _only = only_rig();
    let rig = Rig::new("refused", FAKE_SSH_ANSWERS);
    // The machine claims to have everything, so the demand stays and the step refuses.
    rig.answers("cgroup=here\nbwrap=yes\n");
    let refusal = [
        "thread 'a_test' panicked at crates/tools/tests/confine.rs:44:5:",
        "LETIBOT_REQUIRE_APPARATUS is set, and a cgroup v2 subtree this user may write (the \
         process-lifetime tree) is not on this machine.",
        "This run does not accept a skip. Either run it where the apparatus is, or unset \
         LETIBOT_REQUIRE_APPARATUS to let it skip with a reason.",
        "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
        "error: test failed, to rerun pass `--test confine`",
        "error: 1 target failed:",
        "    `-p letibot-tools --test confine`",
    ];
    let step = format!(
        "LETIBOT_REQUIRE_APPARATUS=1 printf '%s\\n' {}; exit 101",
        refusal
            .iter()
            .map(|l| format!("'{l}'"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let run = rig.gate(&[&step], 120);
    let GateVerdict::Infra(why) = &run.verdict else {
        panic!("a refusal to skip must not be a verdict about the code: {run:#?}");
    };
    assert!(
        why.starts_with("the gate could not run here: a cgroup v2 subtree this user may write"),
        "{why}"
    );
    assert!(why.contains("`fake`"), "{why}");
    assert!(why.contains("UNJUDGED, not red"), "{why}");
    // The row is `not_run`: a step that judged nothing is neither green nor red.
    assert_eq!(run.steps[0].outcome, MergeGateOutcome::NotRun, "{run:#?}");
    // And the step's own words are kept, so a reader can see what it said.
    assert!(
        run.steps[0]
            .output
            .contains("LETIBOT_REQUIRE_APPARATUS is set"),
        "{run:#?}"
    );
}
