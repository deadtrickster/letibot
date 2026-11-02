//! A command that exits 125 is not a command that never ran.
//!
//! 125 is a legitimate exit code, so a state derived from the exit code alone reported a
//! command that chose 125 as a launcher failure, and the model read that word as fact:
//!
//! ```text
//! Some(EXIT_NOT_SCOPED) => JobState::NotScoped,     // host.rs
//! ```
//!
//! Both directions are asserted, because a one-directional test would pass on a head that
//! stopped classifying launcher failures at all.

use letibot_tools::exec::{
    Cgroup2, ExecError, HostProcesses, JobId, JobState, ProcessHost, Reaping, ScopeId, ScopeKind,
    ScopeTree, SpawnRequest,
};
use letibot_tools::testing::runner_harness;
use letibot_transcript::ToolOutcome;

/// The branch taken on a box with no delegated cgroup v2, asserted rather than skipped: the
/// refusal must name what was missing.
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
                // Announced through `apparatus`, so `LETIBOT_REQUIRE_APPARATUS=1` can refuse
                // the skip that an `eprintln!` and a `return` would hide.
                return letibot_tokencore::apparatus::absent(
                    "a cgroup v2 subtree this user may write",
                );
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

/// A background `bash -c "exit 125"` is a job that ran and chose 125 as its answer, and must be
/// readable as exactly that: the state word, `never_ran`, and the command's own output.
#[test]
fn a_command_that_exits_125_ran_and_did_not_fail_to_join_its_scope() {
    let mut h = runner!("exit_125");
    let host = h.processes.clone().unwrap();

    // stderr proves the command ran, and 125 is its own answer.
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

    // The state word: `exited 125`, not `not run (could not join its scope)`.
    assert_eq!(
        view.state,
        JobState::Exited { code: 125 },
        "a command's own exit code was read as a launcher failure: {}",
        view.state.word()
    );
    // The fact the heads draw their sentence from: nothing here is a job that never ran, so
    // the sentence must not be the never-ran one.
    assert!(
        !view.state.never_ran(),
        "`never_ran` is true for a command that ran and exited 125"
    );
    // And it wrote something: a genuine launcher failure produces `0` because the command
    // never ran, and `0` looks like an answer.
    let out = host
        .output(&JobId(id), 0, 4096)
        .expect("the finished job's output");
    assert!(
        out.text().contains("ran-anyway"),
        "the command's own stderr is missing: {:?}",
        out.text()
    );
}

/// The other direction, without which a fix could pass by classifying nothing at all. The wrapper writes
/// its own pid into the scope's `cgroup.procs` and `exec`s only if that worked, so a scope that cannot be
/// written is a launcher failure and the command is not run.
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
            memory: None,
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
    // A genuine launcher failure writes the marker, and the command's own line is absent.
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

/// A tree whose scopes cannot be joined: `open` hands back a directory with no `cgroup.procs`,
/// which is the launcher-failure half of what the substrate protects against and is
/// unreachable on a healthy box.
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

/// The real tree, kept honest: these tests depend on a cgroup's `cgroup.procs` being where the
/// wrapper writes, so the marker path and the real path are built the same way.
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

/// A command that PRINTS the marker is not a command that never ran: the marker is a string a command can
/// print. Only the daemon holds whether a process was ever in the cgroup, and the command below does
/// everything a launcher failure does except fail.
#[test]
fn a_command_that_prints_the_marker_ran_and_did_not_fail_to_join_its_scope() {
    let mut h = runner!("marker_echo");
    let host = h.processes.clone().unwrap();

    // The marker verbatim, as the wrapper writes it: this test is about that exact string.
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
