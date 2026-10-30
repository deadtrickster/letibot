//! **What a host gate can be wrong about, as values** — the invocation it builds, the exit status
//! it reads, and the row it writes.
//!
//! Nothing here reaches a network: an `ssh` is built through [`Ssh::connect`] against a temporary
//! key and known-hosts file, so the argv and the `rsync -e` string are the ones production would
//! run. The end-to-end half — a fake ssh that really runs the steps — is
//! `crates/harnessd/tests/gate_host.rs`, which is a test binary of its own because it has to own
//! `$HOME` and `$LETIBOT_HOSTS`.

use super::*;
use crate::mergequeue::gate::{GateRunner, gate_host, run_gate};

/// A temporary home with the key and the host key `Ssh::connect` insists on, and a host named
/// `lab` declared in a `hosts.toml` of this rig's own.
fn rig(name: &str) -> (PathBuf, Ssh) {
    let dir = std::env::temp_dir().join(format!(
        "letibot-gate-host-unit-{}-{}",
        std::process::id(),
        name
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a rig");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("a home");
    let paths = Paths::in_home(&home);
    std::fs::create_dir_all(paths.keys()).expect("the keys directory");
    std::fs::write(paths.default_key("lab"), b"a dedicated key").expect("the key");
    std::fs::write(
        paths.known_hosts(),
        "lab.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n",
    )
    .expect("the host key");
    let text = "[host.lab]\nhostname = \"lab.invalid\"\nuser = \"tester\"\n\
                root = \"~/letibot-remote\"\nmise = false\n";
    let hosts = Hosts::parse(text).expect("a hosts file");
    let host = hosts.get("lab").expect("the host").clone();
    let ssh = Ssh::connect(host, Some(PathBuf::from("/usr/bin/ssh")), paths).expect("a connection");
    (dir, ssh)
}

/// **The `rsync` invocation, as a value** — the flags that make a copy a copy of the *tree* and
/// not of the last gate's leftovers, and the `-e` that carries the host's declared key.
#[test]
fn the_rsync_invocation_carries_the_key_the_deletes_and_the_two_exclusions() {
    let (dir, ssh) = rig("rsync");
    let argv = rsync_argv(
        &ssh,
        Path::new("/tmp/landing/letibot-abc123"),
        "/home/dead/letibot-remote/gate/letibot-abc123/tree",
    );
    assert_eq!(
        &argv[..6],
        &[
            "-a".to_string(),
            "--delete".to_string(),
            "--exclude=/.git".to_string(),
            "--exclude=/target".to_string(),
            "-e".to_string(),
            argv[5].clone(),
        ]
    );
    // The source has a trailing slash — *the contents of this directory*, not the directory.
    assert_eq!(argv[6], "/tmp/landing/letibot-abc123/");
    // The destination is the host's own root, absolute, and NOT quoted: rsync protects its remote
    // arguments itself (`protect_args`), and quoting as well measured as a literal `\'…\'` on the
    // far side.
    assert_eq!(
        argv[7],
        "tester@lab.invalid:/home/dead/letibot-remote/gate/letibot-abc123/tree"
    );
    // `-e`: this box's ssh, the declared key, and the ControlMaster the rest of the fleet path
    // multiplexes over — nothing invented. Every word is quoted, because rsync runs the string
    // through a shell and a path in it may hold a space.
    let rsh = &argv[5];
    assert!(rsh.starts_with("'/usr/bin/ssh' "), "{rsh}");
    assert!(rsh.contains(ssh.key().to_string_lossy().as_ref()), "{rsh}");
    assert!(
        rsh.contains(ssh.control_socket().to_string_lossy().as_ref()),
        "{rsh}"
    );
    assert!(
        rsh.contains("'StrictHostKeyChecking=yes'"),
        "the host key is checked on the host gate too: {rsh}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The launcher, as a value**: the step's own text lands in a file (so it needs no quoting), the
/// subshell is a subshell, and the build cache is the host's, outside the tree.
#[test]
fn the_launcher_runs_the_step_in_a_subshell_with_the_hosts_own_cache() {
    let script = launcher_script(
        "cargo test --workspace -- --nocapture",
        "/h/gate/x/tree",
        "/h/gate/x/target",
        "/h/gate/x/run/step-0.pid",
        "/h/gate/x/run/step-0.out",
        "/h/gate/x/run/step-0.exit",
    );
    assert!(
        script.contains(
            "( cargo test --workspace -- --nocapture ) > '/h/gate/x/run/step-0.out' 2>&1"
        ),
        "{script}"
    );
    assert!(
        script.contains("export CARGO_TARGET_DIR='/h/gate/x/target'"),
        "{script}"
    );
    assert!(
        script.contains("cd '/h/gate/x/tree' || exit 97"),
        "{script}"
    );
    assert!(
        script.contains("echo $? > '/h/gate/x/run/step-0.exit'"),
        "{script}"
    );
    // A step containing a quote of its own is written verbatim: nothing is spliced or escaped.
    let quoted = launcher_script("echo \"it's fine\"", "/t", "/c", "/p", "/o", "/e");
    assert!(quoted.contains("( echo \"it's fine\" )"), "{quoted}");
}

/// **A start is detached and a kill is a group kill with the local gate's numbers.**
#[test]
fn the_start_is_detached_and_the_kill_is_a_group_kill() {
    let start = launch_script("/h/run/step-0.sh", "/h/run/step-0.exit");
    assert!(
        start.contains("setsid bash '/h/run/step-0.sh' </dev/null >/dev/null 2>&1 &"),
        "{start}"
    );
    assert!(
        start.contains("nohup bash '/h/run/step-0.sh'"),
        "a host without setsid still outlives the call: {start}"
    );
    assert!(start.starts_with("rm -f '/h/run/step-0.exit'"), "{start}");
    let kill = kill_script("/h/run/step-0.pid");
    assert!(kill.contains("kill -TERM -\"$p\""), "{kill}");
    assert!(kill.contains("kill -KILL -\"$p\""), "{kill}");
    assert!(
        kill.contains("[ $i -lt 50 ]"),
        "five seconds of grace: {kill}"
    );
}

/// **The status is read before the pid, and the marker comes first** — the order
/// `a-finishing-job-and-a-killed-job-answer-the-same-poll` says is the only one that can tell a
/// step that finished from a step that was killed, and a marker so a login shell's chatter is not
/// parsed as an exit code.
#[test]
fn the_poll_reads_the_status_before_the_pid_and_leads_with_its_marker() {
    let poll = poll_script(
        "@@letibot-gate-1-0@@",
        "/h/run/step-0.pid",
        "/h/run/step-0.exit",
        "/h/run/step-0.out",
        4096,
    );
    let exit_at = poll
        .find("if [ -f '/h/run/step-0.exit' ]")
        .expect("the status");
    let pid_at = poll.find("if [ -f '/h/run/step-0.pid' ]").expect("the pid");
    assert!(exit_at < pid_at, "the status file is read first: {poll}");
    assert!(
        poll.contains("printf '@@letibot-gate-1-0@@ %s %s\\n'"),
        "{poll}"
    );
    // The bytes come from where this side left off, so a poll never re-sends a step's output.
    assert!(poll.contains("tail -c +4097 '/h/run/step-0.out'"), "{poll}");
}

/// **The worktree is reset the way the local landing tree is** — three commands, and a first
/// `worktree add` when there is none yet.
#[test]
fn the_worktree_is_reset_like_the_landing_tree() {
    let script = worktree_script("/h/gate/x/src.git", "/h/gate/x/tree", "0f2d9550");
    assert!(
        script.contains("git checkout --detach --force '0f2d9550'"),
        "{script}"
    );
    assert!(script.contains("git reset --hard '0f2d9550'"), "{script}");
    assert!(script.contains("git clean -fd"), "{script}");
    assert!(
        script.contains(
            "git -C '/h/gate/x/src.git' worktree add --detach '/h/gate/x/tree' '0f2d9550'"
        ),
        "{script}"
    );
}

/// **A signal read off a shell's exit status is a signal, not a code** — 137 is `SIGKILL`, and a
/// step that died of one judged nothing.
#[test]
fn a_shell_status_of_128_plus_n_is_the_signal_and_not_a_red_branch() {
    assert_eq!(exit_of(0), StepExit::Code(0));
    assert_eq!(exit_of(101), StepExit::Code(101));
    assert_eq!(exit_of(137), StepExit::Signal(9));
    assert_eq!(exit_of(143), StepExit::Signal(15));
    assert_eq!(exit_of(130), StepExit::Signal(2));
    // 128 itself is not a signal's spelling (`SIGKILL` would be 137): it is a code.
    assert_eq!(exit_of(128), StepExit::Code(128));
    // And through the shared judge: a `SIGKILL` with no failing code is the gate's own verdict
    // (unjudged), and the same number read as a code would have been a red branch.
    let killed = ended(
        137,
        "warning: function `answer_frames` is never used\n".to_string(),
    );
    let judged = judge(
        "cargo test",
        killed,
        StepMemory::Elsewhere("lab".into()),
        0,
        Duration::from_secs(30 * 60),
    );
    assert_eq!(judged.outcome, MergeGateOutcome::Killed);
    assert!(
        matches!(&judged.verdict, Some(GateVerdict::Infra(w)) if w.contains("UNJUDGED, not red")),
        "{:?}",
        judged.verdict
    );
    // A code that failed is red, exactly as it is here.
    let red = ended(101, "test result: FAILED. 1 passed; 1 failed\n".to_string());
    let judged = judge(
        "cargo test",
        red,
        StepMemory::Elsewhere("lab".into()),
        0,
        Duration::from_secs(30 * 60),
    );
    assert_eq!(judged.outcome, MergeGateOutcome::Failed);
    assert!(matches!(judged.verdict, Some(GateVerdict::Failed(_))));
    // And a host step's row says its memory was not measured, and where it ran.
    assert!(
        judged
            .output
            .contains("[gate memory: not measured — the step ran on `lab`"),
        "{}",
        judged.output
    );
}

/// **A host step's evidence line is dropped for the model, like any other quiet one** (R93.F8) —
/// the operator's row keeps *where it ran*, the model is not handed the plumbing.
#[test]
fn the_host_line_is_for_the_operator_and_not_the_model() {
    let judged = judge(
        "cargo clippy",
        ended(0, "ok\n".to_string()),
        StepMemory::Elsewhere("lubuntu2".into()),
        0,
        Duration::from_secs(30 * 60),
    );
    assert_eq!(judged.outcome, MergeGateOutcome::Passed);
    assert!(
        judged.output.contains("the step ran on `lubuntu2`"),
        "{}",
        judged.output
    );
    assert_eq!(
        crate::mergequeue::gate::for_the_model(&judged.output),
        "ok\n"
    );
}

/// **A host name nobody declared is a verdict with a sentence**, not a hang and not a red branch:
/// the queue writes `blocked`, the tip is unjudged, and the sentence names the file and the hosts
/// it does know.
#[test]
fn an_undeclared_host_is_unjudged_with_a_sentence_that_names_the_file() {
    let wt = std::env::temp_dir().join(format!("letibot-gate-host-none-{}", std::process::id()));
    std::fs::create_dir_all(&wt).expect("a tree");
    let stop = std::sync::atomic::AtomicBool::new(false);
    let alive = || true;
    let ctl = GateControl {
        deadline: Instant::now() + Duration::from_secs(30),
        budget: Duration::from_secs(30),
        stop: &stop,
        alive: &alive,
    };
    let steps = vec!["echo hi".to_string(), "echo there".to_string()];
    let run = run_on_host("no-such-host-anywhere", &wt, &steps, &ctl);
    let GateVerdict::Infra(why) = &run.verdict else {
        panic!("{run:#?}");
    };
    assert!(
        why.contains("no host `no-such-host-anywhere`") && why.contains("hosts.toml"),
        "{why}"
    );
    assert!(why.contains("UNJUDGED, not red"), "{why}");
    assert!(why.contains("`[queue] gate_host`"), "{why}");
    // Every step is `not_run`: a gate that never ran is not a gate that passed.
    assert_eq!(run.steps.len(), 2);
    assert!(
        run.steps
            .iter()
            .all(|s| s.outcome == MergeGateOutcome::NotRun)
    );
    let _ = std::fs::remove_dir_all(&wt);
}

/// **The seam itself**: an unset host is this machine, and a set one is not — the dispatch that
/// makes `[queue] gate_host` do anything at all.
#[test]
fn an_unset_host_runs_the_gate_here_and_a_set_one_does_not() {
    assert_eq!(gate_host(), None, "nothing set this process's host");
    let wt = std::env::temp_dir().join(format!("letibot-gate-host-seam-{}", std::process::id()));
    std::fs::create_dir_all(&wt).expect("a tree");
    let stop = std::sync::atomic::AtomicBool::new(false);
    let alive = || true;
    let ctl = GateControl {
        deadline: Instant::now() + Duration::from_secs(60),
        budget: Duration::from_secs(60),
        stop: &stop,
        alive: &alive,
    };
    let steps = vec!["echo here".to_string()];
    let local = run_gate(
        None,
        &wt,
        &steps,
        &ctl,
        letibot_tools::exec::memory::Ask::Uncapped,
    );
    assert_eq!(local.verdict, GateVerdict::Passed, "{local:#?}");
    assert!(local.steps[0].output.starts_with("here\n"));
    // The same run, told to use a machine nobody declared: it does not run the step here.
    let host = run_gate(
        Some("no-such-host-anywhere"),
        &wt,
        &steps,
        &ctl,
        letibot_tools::exec::memory::Ask::Uncapped,
    );
    assert!(matches!(host.verdict, GateVerdict::Infra(_)), "{host:#?}");
    assert_eq!(host.steps[0].outcome, MergeGateOutcome::NotRun);
    // And the name a row carries is the host's, so the queue's green line says which machine
    // judged the target.
    let named = crate::mergequeue::gate::HostGate {
        host: "lab2x1".into(),
    };
    assert_eq!(named.name(), "lab2x1");
    assert_eq!(crate::mergequeue::gate::LocalGate.name(), "local");
    let _ = std::fs::remove_dir_all(&wt);
}

/// **A host that is declared but unreachable is a sentence in seconds, not a hang** — the whole
/// point of the bound. The `ssh` here is a script that answers the way a dead host's does (exit
/// 255), so this measures the mapping from *ssh said 255* to *the gate could not run*, without a
/// network.
#[test]
fn an_unreachable_host_is_a_sentence_about_the_machine() {
    let dir = std::env::temp_dir().join(format!("letibot-gate-host-down-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a rig");
    let bin = dir.join("ssh");
    letibot_tools::testing::write_stub(
        &bin,
        "#!/bin/sh\necho 'ssh: connect to host lab.invalid port 22: Connection refused' >&2\n\
         exit 255\n",
    );
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("a home");
    let paths = Paths::in_home(&home);
    std::fs::create_dir_all(paths.keys()).expect("the keys directory");
    std::fs::write(paths.default_key("lab"), b"a dedicated key").expect("the key");
    std::fs::write(
        paths.known_hosts(),
        "lab.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n",
    )
    .expect("the host key");
    let hosts = Hosts::parse(
        "[host.lab]\nhostname = \"lab.invalid\"\nroot = \"~/letibot-remote\"\nmise = false\n",
    )
    .expect("a hosts file");
    let ssh = Ssh::connect(
        hosts.get("lab").expect("the host").clone(),
        Some(bin.clone()),
        paths,
    )
    .expect("a connection");
    let answer = run(
        "making the gate's root",
        "lab",
        ssh.program(),
        &ssh.argv(&ssh.remote_command("mkdir -p \"$HOME\"/letibot-remote")),
        Duration::from_secs(10),
        None,
        true,
    )
    .expect("ssh answered");
    let why = answer.refusal("making the gate's root", "lab");
    assert!(why.contains("did not reach lab"), "{why}");
    assert!(why.contains("ssh exited 255"), "{why}");
    assert!(why.contains("Connection refused"), "{why}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A call that ran out of this box's clock is asked again; a call that never started is not** —
/// the distinction the whole host gate now rests on, as a value.
///
/// A poll that is not answered in time says *the machine was busy forking a shell*, and the answer
/// it will give is still true when it comes, so it is [`Missed::slow`] and the step goes on. A call
/// that could not be started says nothing will ever answer, so it is [`Missed::gone`] and the step
/// is UNJUDGED. Reading the first as the second is what killed a four-minute step on lab2x1 and
/// halted the queue on 2026-10-30.
#[test]
fn a_call_that_ran_out_of_the_clock_is_slow_and_one_that_never_started_is_gone() {
    let slow = run(
        "asking how `lab` is doing",
        "unit-gate-slow-clock",
        Path::new("/bin/sleep"),
        &["5".to_string()],
        Duration::from_secs(1),
        None,
        false,
    )
    .expect_err("a sleep that outlives its bound is a miss");
    assert!(!slow.unreached, "the clock, not the machine: {}", slow.why);
    assert!(
        slow.why.contains("did not answer within 1 s"),
        "{}",
        slow.why
    );
    let gone = run(
        "asking how `lab` is doing",
        "unit-gate-gone-clock",
        Path::new("/nonexistent/ssh"),
        &[],
        Duration::from_secs(1),
        None,
        false,
    )
    .expect_err("a program that is not there is a miss");
    assert!(gone.unreached, "nothing will ever answer: {}", gone.why);
}

/// **The sentence a deadline carries names the bound the last poll was given** — the last poll
/// before a gate's deadline is bounded by what is left of it, and a verdict that says *not answered
/// within 30 s* about a poll that was given four is a sentence about the wrong clock.
#[test]
fn the_deadline_sentence_names_the_bound_the_last_poll_was_given() {
    let quiet = said_with_misses("the tail".into(), 0, "never", POLL_BOUND);
    assert_eq!(
        quiet, "the tail",
        "a step whose polls all answered says nothing"
    );
    let said = said_with_misses(
        "the tail".into(),
        3,
        "asking how `lab` is doing did not answer within 4 s",
        Duration::from_secs(4),
    );
    assert!(said.starts_with("the tail\n\n[gate: 3 poll(s)"), "{said}");
    assert!(said.contains("not answered within 4 s"), "{said}");
    assert!(said.contains("did not answer within 4 s"), "{said}");
}

/// The directory one repository gets on a host is the one it gets on this box.
#[test]
fn the_host_directory_is_named_like_the_landing_directory() {
    let repo = std::env::temp_dir().join(format!("letibot-gate-host-name-{}", std::process::id()));
    std::fs::create_dir_all(&repo).expect("a repo");
    let _ = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&repo)
        .status();
    let name = dir_name(&repo);
    // `<the directory's own name>-<12 hex>`, exactly as `git::landing_dir_in` builds it.
    let hex = name.rsplit_once('-').map(|(_, h)| h).unwrap_or_default();
    assert!(
        name.starts_with("letibot-gate-host-name-") && hex.len() == 12,
        "{name}"
    );
    assert!(
        hex.chars().all(|c| c.is_ascii_hexdigit()),
        "the suffix is a hash: {name}"
    );
    // The same function the local landing directory uses, so the two cannot drift.
    assert_eq!(
        crate::mergequeue::git::landing_dir_in(Path::new("."), &repo)
            .file_name()
            .unwrap(),
        std::ffi::OsStr::new(&name)
    );
    let _ = std::fs::remove_dir_all(&repo);
}
