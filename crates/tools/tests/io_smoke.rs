//! A job that writes 64 MiB shows about 64 MiB written, measured in a real delegated scope.
//! Off the normal path: it writes to /build, so it runs only under `LETIBOT_IO_SMOKE=1`. With io
//! delegated the total is `io.stat`; without it, the sampled `/proc` fallback.

use std::process::Command;
use std::time::Duration;

use letibot_tools::exec::diskio::IoSource;
use letibot_tools::exec::{ProcessHost, ScopeKind, SpawnRequest};
use letibot_tools::testing::runner_harness;

const GATE: &str = "LETIBOT_IO_SMOKE";
const INNER: &str = "LETIBOT_IO_SMOKE_INNER";
const THIS: &str = "a_job_that_writes_64_mib_shows_about_64_mib_written";

#[test]
fn a_job_that_writes_64_mib_shows_about_64_mib_written() {
    if std::env::var_os(GATE).is_none() {
        return;
    }
    if std::env::var_os(INNER).is_some() {
        return inner();
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
            "-p",
            "IOAccounting=yes",
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
    println!("{text}");
    assert!(
        out.status.success() && text.contains("INNER MEASURED"),
        "the inner run failed:\n{text}"
    );
}

fn inner() {
    let _ = letibot_tools::exec::delegate_memory();
    let h = runner_harness().expect("a cgroup tree");
    let host = h.processes.clone().expect("a process host");
    let file = format!("/build/letibot-io-smoke-{}", std::process::id());
    // The sleep keeps the job alive past a guard tick, which the sampled path needs.
    let cmd = format!(
        "dd if=/dev/zero of={file} bs=1M count=64 oflag=direct status=none; rm -f {file}; sleep 12"
    );
    let id = host
        .spawn(&SpawnRequest {
            command: cmd,
            slug: None,
            cwd: ".".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("spawn");
    host.wait_job(&id, Duration::from_secs(60)).expect("wait");
    let _ = std::fs::remove_file(&file);
    let job = host.job_handle(&id).expect("the job");
    let used = job.io_used().expect("an io reading");
    let mib = used.stat.wbytes as f64 / (1u64 << 20) as f64;
    println!(
        "INNER MEASURED: {} ({:.1} MiB, {:?}; io {})",
        used.clause(None),
        mib,
        used.source,
        match letibot_tools::exec::diskio::io_delegation() {
            Ok(()) => "delegated".to_string(),
            Err(why) => format!("not delegated: {why}"),
        }
    );
    assert!(
        (60.0..=72.0).contains(&mib),
        "64 MiB was written, the job says {mib:.1} MiB ({:?})",
        used.source
    );
    let _ = IoSource::Cgroup;
}
