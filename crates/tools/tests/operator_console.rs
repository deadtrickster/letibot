//! The operator's own `!` line on the top-level seat: the boundary spares it, so it runs on the
//! operator's real home and rc, and it gets a bash that rc can be written for.

use std::time::Duration;

use letibot_tools::backend::{ExecBackend, HostBackend};
use letibot_tools::exec::seat::{self, Level};
use letibot_tools::exec::{ScopeKind, SpawnRequest};
use letibot_tools::mode::Mode;

/// A bash 4+ this box can run, which is what the rule needs to have something to choose.
fn has_bash_four() -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|d| {
        std::process::Command::new(d.join("bash"))
            .args(["-c", "[ \"${BASH_VERSINFO[0]}\" -ge 4 ]"])
            .status()
            .is_ok_and(|s| s.success())
    })
}

#[test]
fn the_operators_line_on_the_wide_seat_reads_a_bash_four_rc_without_complaint() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a bash 4 or newer on PATH (a stock Mac has only /bin/bash 3.2)",
        has_bash_four(),
    ) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("letibot-opconsole-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, project) = (root.join("home"), root.join("project"));
    std::fs::write(
        home.join(".bashrc"),
        "declare -A seen=([a]=1)\necho \"rc-read ${seen[a]}\"\n",
    )
    .unwrap();
    let view = seat::seat_for_level(
        Level::Wide,
        &Mode::WRITES_ALLOWED,
        "this machine",
        &[] as &[&str],
        &home,
        None,
    );
    let backend = HostBackend::confined_over(&project, &project, Vec::new(), view)
        .unwrap_or_else(|e| panic!("the wide seat did not build on this box: {e}"));
    let host = backend.processes_arc().expect("a process host");
    let id = host
        .spawn(&SpawnRequest {
            command: "echo ran".into(),
            slug: None,
            cwd: ".".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![("HOME".into(), home.display().to_string())],
            // The operator's own run: the shell that reads their rc.
            tty: true,
            memory: None,
            env_trailer: false,
        })
        .expect("spawn");
    host.wait_job(&id, Duration::from_secs(30)).expect("wait");
    let out =
        String::from_utf8_lossy(&host.output(&id, 0, 8192).expect("output").bytes).into_owned();
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        !out.contains("invalid option"),
        "a bash too old for the rc read it: {out}"
    );
    assert!(out.contains("rc-read 1"), "the rc was read: {out}");
    assert!(out.contains("ran"), "{out}");
}
