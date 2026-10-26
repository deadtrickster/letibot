//! **A job that outgrows its memory cap dies alone, and says so** — `TODO.md` R64/R65,
//! measured in a real delegated scope rather than asserted about one.
//!
//! The cap exists only when the process that owns the tree was started in a systemd scope of
//! its own with `Delegate=yes` and has stepped into a leaf (`delegate_memory`). A test binary
//! is not started that way, so this test RE-RUNS ITSELF inside one: the outer half asks
//! `systemd-run --user --scope -p Delegate=yes` for the scope and runs this same test in it,
//! and the inner half — the one that sees `LETIBOT_MEMCAP_INNER` — does the measuring.
//!
//! A box with no user systemd skips, and says so; with `LETIBOT_REQUIRE_APPARATUS=1` (the
//! merge gate's setting) a skip is a failure, because a skipped test is a passed one as far
//! as a step's status is concerned.
//!
//! **A platform with no systemd is not that box.** macOS has no `systemd-run` and no cgroups,
//! so a cap in a delegated scope is a thing it can never have; the same absence there is
//! announced through `apparatus::applies` and is not a failure a provisioning could clear.

use std::process::Command;
use std::time::Duration;

use letibot_tools::exec::{JobState, ProcessHost, ScopeKind, SpawnRequest};
use letibot_tools::testing::runner_harness;

const INNER: &str = "LETIBOT_MEMCAP_INNER";
const THIS: &str = "a_job_that_outgrows_its_cap_dies_alone_and_one_under_its_cap_reports_its_peak";

/// `awk` building a 200 MB string: everywhere, and allocates what it says.
const EAT_200MB: &str = "awk 'BEGIN { s = sprintf(\"%200000000s\", \"\"); print length(s) }'";

#[test]
fn a_job_that_outgrows_its_cap_dies_alone_and_one_under_its_cap_reports_its_peak() {
    if std::env::var_os(INNER).is_some() {
        return inner();
    }
    let scope_ok = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--expand-environment=no",
            "true",
        ])
        .status()
        .is_ok_and(|s| s.success());
    if !scope_ok {
        // **Two absences, and only one of them is this box's.** `systemd-run --user` and the
        // delegated scope it asks for are Linux's: a Linux box with no user manager is one
        // that has not been set up, and `LETIBOT_REQUIRE_APPARATUS=1` refuses that skip on
        // purpose. macOS can never have either — the absence there is the platform's, so it
        // is announced through `applies` and is not refusable.
        let Some(_) = letibot_tokencore::apparatus::applies(
            "a delegated systemd scope to cap a job in",
            cfg!(target_os = "linux"),
        ) else {
            return;
        };
        return letibot_tokencore::apparatus::absent(
            "a systemd user manager, whose delegated scope is what a memory cap needs",
        );
    }
    let exe = std::env::current_exe().expect("the test binary");
    let out = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--expand-environment=no",
            "-p",
            "Delegate=yes",
        ])
        .arg(&exe)
        .args(["--exact", THIS, "--nocapture", "--test-threads=1"])
        .env(INNER, "1")
        .output()
        .expect("systemd-run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success() && text.contains("INNER MEASURED"),
        "the inner run, in its own delegated scope, failed:\n{text}"
    );
}

fn inner() {
    let base = letibot_tools::exec::delegate_memory()
        .as_ref()
        .expect("started in a delegated scope of its own, so the daemon's leaf must take");
    let h = runner_harness().expect("a cgroup tree under the delegated scope");
    let host = h.processes.clone().expect("a process host");
    assert!(
        host.scopes().is_empty() || base.exists(),
        "the tree must hang off the delegated scope"
    );

    let run = |memory: u64| {
        let id = host
            .spawn(&SpawnRequest {
                command: EAT_200MB.into(),
                slug: None,
                cwd: ".".into(),
                scope: ScopeKind::Turn,
                scope_name: None,
                background: false,
                env: vec![],
                tty: false,
                memory: Some(memory),
            })
            .expect("spawn");
        host.wait_job(&id, Duration::from_secs(60)).expect("wait");
        let job = host.job_handle(&id).expect("the job");
        (
            job.state(),
            job.memory_used()
                .expect("the kernel's counters, read at the settle"),
        )
    };

    // Under a 64 MiB cap the 200 MB string cannot be built: the kernel kills it, and only it.
    let (state, used) = run(64 << 20);
    assert!(
        used.killed_at_cap(),
        "64 MiB must not hold 200 MB: {state:?}, {used:?}"
    );
    assert_eq!(used.cap, Some(64 << 20));
    assert!(
        used.peak <= 64 << 20,
        "the peak cannot pass the cap: {used:?}"
    );
    assert!(
        !matches!(state, JobState::Exited { code: 0 }),
        "a killed job must not read as a success: {state:?}"
    );

    // Under 512 MiB it fits, and the peak says roughly what it cost.
    let (state, used) = run(512 << 20);
    assert_eq!(state, JobState::Exited { code: 0 }, "{used:?}");
    assert!(!used.killed_at_cap(), "{used:?}");
    assert!(
        used.peak > 150 << 20,
        "the peak must show the 200 MB it built: {used:?}"
    );

    // And the test process — the daemon's stand-in — is still here to say so.
    println!(
        "INNER MEASURED: killed at 64 MiB; under 512 MiB: {}",
        used.clause()
    );
}
