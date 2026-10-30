//! **A gate step that outgrows the gate's memory cap is killed alone, and its row says so** —
//! `TODO.md` R76 item 6, measured in a real delegated scope rather than asserted about one.
//!
//! The gate's steps used to run in the daemon's own cgroup, so a heavy `cargo test` was charged
//! to the daemon's `memory.high`. Now each step joins a cgroup of its own beside the daemon's
//! leaf. That exists only when the process was started in a systemd scope of its own with
//! `Delegate=yes` and has stepped into its leaf (`delegate_memory`), and a test binary is not
//! started that way — so this test RE-RUNS ITSELF inside one, the way
//! `crates/tools/tests/memory_cap.rs` does: the outer half asks `systemd-run --user --scope -p
//! Delegate=yes` for the scope, and the inner half (`LETIBOT_GATEMEM_INNER`) measures.
//!
//! A box with no user systemd skips and says so; under `LETIBOT_REQUIRE_APPARATUS=1` (the merge
//! gate's setting) a skip is a failure.
//!
//! **A platform with no systemd is not that box.** macOS has no `systemd-run` and no cgroups, so
//! a cap in a delegated scope is a thing it can never have; the same absence there is announced
//! through `apparatus::applies` and is not a failure a provisioning could clear.

use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use letibot_harnessd::mergequeue::gate::{GateControl, GateVerdict, run_with_memory};
use letibot_tokencore::store::MergeGateOutcome;
use letibot_tools::exec::memory::{Amount, Ask};

const INNER: &str = "LETIBOT_GATEMEM_INNER";
const THIS: &str = "a_gate_step_past_its_cap_is_killed_alone_and_its_row_says_so";

/// `awk` building a 200 MB string: everywhere, and allocates what it says.
const EAT_200MB: &str = "awk 'BEGIN { s = sprintf(\"%200000000s\", \"\"); print length(s) }'";

#[test]
fn a_gate_step_past_its_cap_is_killed_alone_and_its_row_says_so() {
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
        // delegated scope it asks for are Linux's: a Linux box with no user manager is one that
        // has not been set up, and `LETIBOT_REQUIRE_APPARATUS=1` refuses that skip on purpose.
        // macOS can never have either — the absence there is the platform's, so it is announced
        // through `applies` and is not refusable.
        let Some(_) = letibot_tokencore::apparatus::applies(
            "a delegated systemd scope to cap a gate step in",
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
        .expect("started in a delegated scope of its own, so the daemon's leaf must take")
        .clone();
    let wt = std::env::temp_dir().join(format!("letibot-gate-memory-{}", std::process::id()));
    std::fs::create_dir_all(&wt).unwrap();
    let stop = AtomicBool::new(false);
    let alive = || true;
    let ctl = GateControl {
        deadline: Instant::now() + Duration::from_secs(120),
        budget: Duration::from_secs(120),
        stop: &stop,
        alive: &alive,
    };
    let steps = vec![EAT_200MB.to_string(), "echo never reached".to_string()];

    // Under a 64 MiB cap the 200 MB string cannot be built: the kernel kills the step, and
    // the row and the verdict say it was the GATE's cap, not red code.
    let run = run_with_memory(&wt, &steps, &ctl, Ask::Amount(Amount::Bytes(64 << 20)));
    let GateVerdict::Infra(why) = &run.verdict else {
        panic!("a step killed at the gate's cap is the gate's limit: {run:#?}");
    };
    assert!(
        why.contains("KILLED at the gate's memory cap") && why.contains("of a 64 MiB cap"),
        "{why}"
    );
    assert!(
        why.contains("[memory] gate"),
        "names the line that raises it: {why}"
    );
    assert_eq!(run.steps[0].outcome, MergeGateOutcome::Killed, "{run:#?}");
    assert!(
        run.steps[0]
            .output
            .contains("[gate memory: KILLED at the gate's cap — peak"),
        "the row's evidence: {}",
        run.steps[0].output
    );
    assert_eq!(run.steps[1].outcome, MergeGateOutcome::NotRun);

    // Under 2 GiB it fits (gawk's `sprintf` holds more than one copy: MEASURED on lab2x1, it is
    // killed at 512 MiB and runs at 1 GiB); the row carries the step's own peak, and the
    // allocation was charged to the step's cgroup — not to the daemon's leaf, whose
    // `memory.high` it would have counted against.
    let run = run_with_memory(&wt, &steps[..1], &ctl, Ask::Amount(Amount::Bytes(2 << 30)));
    assert_eq!(run.verdict, GateVerdict::Passed, "{run:#?}");
    let out = &run.steps[0].output;
    assert!(
        out.contains("[gate memory: peak") && out.contains("of a 2 GiB cap"),
        "{out}"
    );
    let leaf_peak: u64 = std::fs::read_to_string(
        base.join(letibot_tools::exec::DAEMON_LEAF)
            .join("memory.peak"),
    )
    .unwrap()
    .trim()
    .parse()
    .unwrap();
    assert!(
        leaf_peak < 150 << 20,
        "the daemon's leaf must not have been charged for the step's 200 MB: {leaf_peak}"
    );

    // No gate cgroup is left behind.
    let left: Vec<_> = std::fs::read_dir(&base)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("gate."))
        .map(|e| e.path())
        .collect();
    assert!(left.is_empty(), "gate cgroups left behind: {left:?}");
    let _ = std::fs::remove_dir_all(&wt);

    // And the test process — the daemon's stand-in — is still here to say so.
    println!("INNER MEASURED: killed at 64 MiB alone; under 2 GiB: {out}");
}
