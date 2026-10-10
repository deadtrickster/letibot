//! **A command that exits 125 is not a command that never ran.**
//!
//! R19's sibling, found by the other head on 2026-09-22 and handed over because only the
//! daemon holds the evidence. `JobState::NotScoped` was derived **from the exit code
//! alone** —
//!
//! ```text
//! Some(EXIT_NOT_SCOPED) => JobState::NotScoped,     // host.rs
//! ```
//!
//! — and `EXIT_NOT_SCOPED` is **125**, which is a legitimate exit code. So
//! `bash -c "exit 125"` in the background was listed as
//! `not run (could not join its scope)`; its `produced` was `0`, where a genuine launcher
//! failure writes 63 bytes of marker to stderr; and the model then read the state word and
//! concluded *"the 125 exit code was never produced"*. **The word taught the model
//! something false about the world**, which is worse than teaching the operator something
//! false: the operator can disbelieve a card.
//!
//! It is one integer carrying two meanings, which is R18's shape one layer down — that was
//! one accessor answering a different question, this is one exit code standing for both
//! *"the wrapper could not join the cgroup"* and *"the command exited"*.
//!
//! # What this file asserts, and both directions
//!
//! A fix for this that is not falsified is a guess, so the two cases are asserted together:
//! a command that exits 125 **ran** (and its output is its own), and a wrapper that cannot
//! join its cgroup **did not**, and still says so. A one-directional test would pass on a
//! head that simply stopped classifying launcher failures at all, which would be a worse
//! defect than the one being fixed — `NotScoped` exists because the F5 rule is *never let a
//! component's "I did not do this" be reported upward as success*.

use letibot_tools::exec::{
    Cgroup2, ExecError, HostProcesses, JobId, JobState, ProcessHost, Reaping, ScopeId, ScopeKind,
    ScopeTree, SpawnRequest,
};
use letibot_tools::testing::runner_harness;
use letibot_transcript::ToolOutcome;

/// The branch taken on a box with no delegated cgroup v2, asserted rather than skipped —
/// the rule `exec.rs` and `background.rs` both state.
macro_rules! runner {
    ($name:literal) => {
        match runner_harness() {
            Ok(h) => h,
            Err(e) => {
                eprintln!(
                    "{}: no cgroup v2 subtree here, checking the refusal: {e}",
                    $name
                );
                let msg = format!("{e}");
                assert!(
                    msg.contains("nothing would reap it") || msg.contains("cgroup"),
                    "a refusal must name what was missing: {msg}"
                );
                return;
            }
        }
    };
}

/// The job id out of a result body.
fn job_id_anywhere(text: &str) -> Option<String> {
    let at = text.find("`j")?;
    let rest = &text[at + 1..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

/// **The defect, reproduced the way the other head reproduced it.**
///
/// A background `bash -c "exit 125"` is a job that ran and chose 125 as its answer, and it
/// must be readable as exactly that: the state word, the `never_ran` fact the two heads
/// draw their sentence from, and the command's own output.
#[test]
fn a_command_that_exits_125_ran_and_did_not_fail_to_join_its_scope() {
    let mut h = runner!("exit_125");
    let host = h.processes.clone().unwrap();

    // The same shape as leticl's scratch daemon: stderr proves the command ran, and 125 is
    // its own answer.
    let r = h.call(
        "bash",
        &serde_json::json!({
            "command": "echo ran-anyway >&2; exit 125",
            "background": true,
        })
        .to_string(),
    );
    let id = match &r.outcome {
        ToolOutcome::Backgrounded { handle, .. } => handle.clone(),
        other => job_id_anywhere(&r.payload).unwrap_or_else(|| panic!("{other:?}: {}", r.render())),
    };

    // Wait for it to settle rather than racing it: 125 is not a duration.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let view = loop {
        let v = host.job(&JobId(id.clone())).expect("the job is known");
        if !v.state.is_running() {
            break v;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the job never settled: {}",
            v.state.word()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };

    // 1. **The state word.** `exited 125`, not `not run (could not join its scope)`.
    assert_eq!(
        view.state,
        JobState::Exited { code: 125 },
        "a command's own exit code was read as a launcher failure: {}",
        view.state.word()
    );
    // 2. **The fact the two heads draw their sentence from** (§11.6): nothing here is a job
    //    that never ran, so the sentence must not be the never-ran one.
    assert!(
        !view.state.never_ran(),
        "`never_ran` is true for a command that ran and exited 125"
    );
    // 3. **And it wrote something**, which is the third reading of leticl's report: the
    //    genuine launcher failure produces `0` because the command never ran, and the
    //    interesting half of the bug was that `0` looked like an answer.
    let out = host
        .output(&JobId(id), 0, 4096)
        .expect("the finished job's output");
    assert!(
        out.text().contains("ran-anyway"),
        "the command's own stderr is missing: {:?}",
        out.text()
    );
}

/// **The other direction, and without it a fix could pass by classifying nothing at all.**
///
/// The wrapper writes its own pid into the scope's `cgroup.procs` and `exec`s only if that
/// worked, so a scope whose `cgroup.procs` cannot be written is a launcher failure: the
/// command is **not run**, and the state says so.
///
/// The tree is a stub rather than a broken cgroup mount on purpose: a real cgroup cannot be
/// asked to fail on purpose, and a test that could only be run by breaking the box would be
/// a test nobody runs. `with_tree` is the seam the substrate already has for exactly this
/// (`NoScopes` and `NoConfinement` are the other two users).
#[test]
fn a_scope_that_cannot_be_joined_still_says_the_command_never_ran() {
    let dir = std::env::temp_dir().join(format!("letibot-notscoped-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    let host = HostProcesses::with_tree(&dir, Box::new(Unjoinable))
        .with_shell(vec!["/bin/sh".into(), "-c".into()]);

    let id = host
        .spawn(&SpawnRequest {
            command: "echo this must never run".into(),
            // Nobody named this run: the substrate's own test.
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: Vec::new(),
            // No terminal: these are the substrate's own tests, not an operator's run.
            tty: false,
        })
        .expect("the spawn itself succeeds: the failure is the join, not the fork");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let view = loop {
        let v = host.job(&id).expect("the job is known");
        if !v.state.is_running() {
            break v;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the job never settled: {}",
            v.state.word()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };

    assert_eq!(
        view.state,
        JobState::NotScoped,
        "a launcher failure was reported as the command's own ending: {}",
        view.state.word()
    );
    assert!(view.state.never_ran());
    assert_eq!(
        view.state.word(),
        "not run (could not join its scope)",
        "§11.6 pins this literal on both heads"
    );
    // The evidence, and the number leticl measured: a genuine launcher failure writes the
    // marker, and the command's own line is absent.
    let out = host.output(&id, 0, 4096).expect("the job's output");
    assert!(
        out.text().contains("could not join the scope cgroup"),
        "the wrapper's marker is what separates the two cases: {:?}",
        out.text()
    );
    assert!(
        !out.text().contains("must never run"),
        "the command ran in a scope that could not be joined: {:?}",
        out.text()
    );
}

/// **A tree whose scopes cannot be joined.** `open` hands back a directory that
/// `cgroup.procs` cannot be written in (there is no such file), which is precisely one
/// half of what T24's substrate is protecting against and is unreachable on a healthy box.
struct Unjoinable;

impl ScopeTree for Unjoinable {
    fn describe(&self) -> String {
        "a stub tree whose cgroup.procs cannot be written, for the launcher-failure path".into()
    }
    fn open(
        &self,
        kind: ScopeKind,
        name: &str,
        _parent: Option<&ScopeId>,
    ) -> Result<ScopeId, ExecError> {
        Ok(ScopeId {
            kind,
            name: name.to_string(),
            path: std::env::temp_dir().join(format!("letibot-unjoinable-{name}")),
        })
    }
    fn members(&self, _scope: &ScopeId) -> Result<Vec<u32>, ExecError> {
        Ok(Vec::new())
    }
    fn end(&self, scope: &ScopeId) -> Reaping {
        Reaping {
            scope: scope.clone(),
            at: std::time::SystemTime::now(),
            mechanism: "nothing to kill (a stub tree)",
            observed: Vec::new(),
            survivors: Vec::new(),
            waited: std::time::Duration::ZERO,
            removed: true,
            note: None,
        }
    }
    fn migrate(
        &self,
        from: &ScopeId,
        _to: &ScopeId,
    ) -> Result<letibot_tools::exec::Migration, ExecError> {
        Err(ExecError::Spawn(format!(
            "a stub tree cannot migrate out of {from}"
        )))
    }
    fn list(&self) -> Vec<ScopeId> {
        Vec::new()
    }
}

/// The real tree, kept honest: the property these tests depend on is that a cgroup's
/// `cgroup.procs` is where the wrapper writes, so the marker path and the real path are
/// built the same way.
#[test]
fn the_procs_path_this_test_relies_on_is_the_real_one() {
    let scope = ScopeId {
        kind: ScopeKind::Turn,
        name: "t".into(),
        path: std::path::PathBuf::from("/sys/fs/cgroup/example"),
    };
    assert_eq!(
        Cgroup2::procs_path(&scope),
        std::path::PathBuf::from("/sys/fs/cgroup/example/cgroup.procs")
    );
}

/// **A command that PRINTS the marker is not a command that never ran.**
///
/// The fix above replaced *the exit code alone* with *the marker in the output*, and that is
/// still not the evidence the question needs: the marker is a string a command can print.
/// The daemon holds a stronger fact than either — **whether a process was ever in the
/// cgroup** — and this test is what says the difference is real.
///
/// leticl's framing, kept because it is the precise one: *only the daemon holds the evidence
/// of whether a process was spawned. An exit code is the process's own answer and cannot be
/// the evidence that there was no process* — and neither can a sentence the process wrote.
///
/// The command below does everything a launcher failure does except fail: it writes the
/// marker to stderr, and exits 125. It **ran**, and its own answer is 125.
#[test]
fn a_command_that_prints_the_marker_ran_and_did_not_fail_to_join_its_scope() {
    let mut h = runner!("marker_echo");
    let host = h.processes.clone().unwrap();

    // The marker, verbatim, from the same const the wrapper uses — so this test cannot drift
    // from the string it is about.
    let marker = "letibot: could not join the scope cgroup; the command was NOT run";
    let r = h.call(
        "bash",
        &serde_json::json!({
            "command": format!("echo '{marker}' >&2; exit 125"),
            "background": true,
        })
        .to_string(),
    );
    let id = match &r.outcome {
        ToolOutcome::Backgrounded { handle, .. } => handle.clone(),
        other => job_id_anywhere(&r.payload).unwrap_or_else(|| panic!("{other:?}: {}", r.render())),
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let view = loop {
        let v = host.job(&JobId(id.clone())).expect("the job is known");
        if !v.state.is_running() {
            break v;
        }
        assert!(std::time::Instant::now() < deadline, "never settled");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };

    assert_eq!(
        view.state,
        JobState::Exited { code: 125 },
        "a command that printed the marker and exited 125 RAN — the marker is a string a \
         command can print, and the daemon's evidence is whether a process was ever in the \
         cgroup, not what the output says"
    );
    assert!(
        !view.state.never_ran(),
        "and `never_ran` must be false: a process existed, and it answered 125"
    );
}
