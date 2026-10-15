use super::*;

fn tmp(name: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "letibot-held-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A process in a fake `/proc`: its `cmdline` (NUL-separated) and a `stat` in `state`.
fn fake_proc(proc_root: &Path, pid: u32, args: &[&str], state: &str) {
    let d = proc_root.join(pid.to_string());
    std::fs::create_dir_all(&d).unwrap();
    let mut cmd = args.join("\0");
    cmd.push('\0');
    std::fs::write(d.join("cmdline"), cmd).unwrap();
    std::fs::write(
        d.join("stat"),
        format!("{pid} (fire) cracker) {state} 1 1 1"),
    )
    .unwrap();
}

/// A run directory the way `firecode up` leaves one, with its jail's `vm-config.json`.
fn fake_run(runs: &Path, run: &str, project: &str, tap: &str) {
    let d = runs.join(run);
    let jail = runs.join(format!("{run}-jail"));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::create_dir_all(&jail).unwrap();
    std::fs::write(d.join("project"), format!("{project}\n")).unwrap();
    std::fs::write(d.join("jail"), jail.display().to_string()).unwrap();
    std::fs::write(
        jail.join("vm-config.json"),
        format!(
            r#"{{"boot-source":{{}},"network-interfaces":[{{"iface_id":"eth0","host_dev_name":"{tap}"}}]}}"#
        ),
    )
    .unwrap();
}

/// **A slot's holder is read off firecode's own records, and only a VM that is alive holds
/// one.** The four tap holders of 2026-10-15 in miniature: a live firecracker for each run
/// (named by `--id`), a run directory whose VM is gone (no process — no slot), a zombie (exited,
/// not yet reaped by init — no slot), and a process that is not a firecracker at all but carries
/// the words (a `grep --id …` — not a VM).
#[test]
fn a_slot_is_held_by_a_live_firecracker_for_a_recorded_run() {
    let root = tmp("live");
    let (runs, proc_root) = (root.join("runs"), root.join("proc"));
    let run = "firecode-20261014-191759-812644";
    fake_run(
        &runs,
        run,
        "/home/dead/.cache/letibot/firecode/s-1-sub-2-sub-3",
        "fccode1",
    );
    fake_run(&runs, "firecode-20261013-010101-1", "/p/gone", "fccode2");
    fake_run(&runs, "firecode-20261013-010101-2", "/p/zombie", "fccode3");
    fake_proc(
        &proc_root,
        861706,
        &[
            "/firecracker",
            "--id",
            run,
            "--api-sock",
            "/firecracker.socket",
        ],
        "S",
    );
    fake_proc(
        &proc_root,
        900,
        &["/firecracker", "--id", "firecode-20261013-010101-2"],
        "Z",
    );
    fake_proc(&proc_root, 901, &["grep", "--id", run], "S");

    let vms = live_vms(&runs, &proc_root);
    assert_eq!(vms.len(), 1, "{vms:?}");
    let vm = &vms[0];
    assert_eq!(vm.run, run);
    assert_eq!(vm.pids, vec![861706]);
    assert_eq!(vm.tap.as_deref(), Some("fccode1"));
    assert_eq!(
        vm.project,
        PathBuf::from("/home/dead/.cache/letibot/firecode/s-1-sub-2-sub-3")
    );
    assert_eq!(
        vm.line(),
        "fccode1: firecracker pid 861706, run firecode-20261014-191759-812644 started \
         2026-10-14 19:17:59"
    );
}

#[test]
fn a_runs_start_is_read_off_its_id() {
    assert_eq!(
        started_of("firecode-20261012-003911-3769277").as_deref(),
        Some("2026-10-12 00:39:11")
    );
    assert_eq!(started_of("r1"), None);
    assert_eq!(started_of("firecode-2026101-003911-1"), None);
}

/// **A stand-in firecracker**: a script NAMED `firecracker`, run with `--id RUN`, that ignores
/// SIGTERM the way a jailed VM (pid 1 of its namespace) does. Returns the child and its pid.
fn stubborn_firecracker(dir: &Path, run: &str) -> std::process::Child {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("firecracker");
    std::fs::write(
        &script,
        "#!/bin/sh\ntrap '' TERM\nwhile :; do sleep 0.2; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let child = std::process::Command::new(&script)
        .args(["--id", run])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    // Until the shell has exec'd, /proc shows the spawning process's cmdline.
    let began = Instant::now();
    while firecrackers(Path::new("/proc"))
        .iter()
        .all(|(_, p)| *p != child.id())
        && began.elapsed() < Duration::from_secs(5)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

fn stub_firecode(dir: &Path, down: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("firecode");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\ncase \"$1\" in\ndown) echo \"$2\" >> '{}/downs'; {down} ;;\nesac\n",
            dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn quick() -> Patience {
    Patience {
        down: Duration::from_secs(10),
        after_down: Duration::from_millis(300),
        after_kill: Duration::from_secs(5),
    }
}

/// **A VM that ignores `down` is killed, and the reap says so only when `/proc` agrees** — the
/// operator's measurement of 2026-10-15: SIGTERM ended the nine unjailed orphans and none of the
/// four jailed tap holders. The stand-in ignores SIGTERM; `down` (the stub) does nothing; the
/// run's cgroup is a directory with a `cgroup.kill` that kills nothing (a plain file). What
/// takes it is SIGKILL to its own pid, and the answer is `Killed`, read off `/proc`.
#[test]
fn a_vm_that_ignores_down_is_killed_and_seen_gone() {
    let dir = tmp("reap");
    let run = "firecode-20261014-192007-899231";
    let mut fc = stubborn_firecracker(&dir, run);
    let cg = dir.join("cgroup");
    std::fs::create_dir_all(&cg).unwrap();
    std::fs::write(cg.join("cgroup.kill"), "").unwrap();
    let bin = stub_firecode(&dir, "exit 0");
    let vm = HeldVm {
        run: run.into(),
        project: dir.join("copy"),
        name: None,
        tap: Some("fccode2".into()),
        pids: vec![fc.id()],
        started: started_of(run),
        cgroup: Some(cg.clone()),
    };
    let said = reap(&bin, &vm, Path::new("/proc"), quick());
    let _ = fc.wait();
    assert_eq!(
        said,
        Reaped::Killed {
            how: "SIGKILL to its pid".into()
        },
        "{}",
        said.said()
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("downs")).unwrap().trim(),
        run,
        "firecode's own `down` is asked first, by the run's id"
    );
    assert_eq!(
        std::fs::read_to_string(cg.join("cgroup.kill")).unwrap(),
        "1",
        "the run's cgroup.kill is the first SIGKILL"
    );
}

/// **A VM `down` brings down is `Down`, and nothing is killed.**
#[test]
fn a_vm_that_down_brings_down_is_not_killed() {
    let dir = tmp("down");
    let run = "firecode-20261014-191759-812644";
    let mut fc = stubborn_firecracker(&dir, run);
    let bin = stub_firecode(&dir, &format!("kill -KILL {}", fc.id()));
    let vm = HeldVm {
        run: run.into(),
        project: dir.join("copy"),
        name: None,
        tap: None,
        pids: vec![fc.id()],
        started: None,
        cgroup: None,
    };
    let said = reap(&bin, &vm, Path::new("/proc"), quick());
    let _ = fc.wait();
    assert_eq!(said, Reaped::Down, "{}", said.said());
}

/// **A pid that is no longer this run's firecracker is never signalled** — a pid reused by
/// something else between the listing and the reap is not the VM.
#[test]
fn a_pid_that_is_not_the_runs_firecracker_is_left_alone() {
    let dir = tmp("reuse");
    let mut other = stubborn_firecracker(&dir, "firecode-20260101-000000-1");
    let bin = stub_firecode(&dir, "exit 0");
    let vm = HeldVm {
        run: "firecode-20261014-191759-812644".into(),
        project: dir.join("copy"),
        name: None,
        tap: None,
        pids: vec![other.id()],
        started: None,
        cgroup: None,
    };
    let said = reap(&bin, &vm, Path::new("/proc"), quick());
    assert_eq!(said, Reaped::Down, "nothing of this run is left: {said:?}");
    assert!(
        alive(Path::new("/proc"), other.id()),
        "the other run's process was signalled"
    );
    let _ = other.kill();
    let _ = other.wait();
}
