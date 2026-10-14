//! The firecode backend against a real VM. `FIRECODE_LIVE=1`, or it skips loudly.

use letibot_tools::backend::{Command, ExecBackend};
use letibot_tools::exec::{ScopeKind, SpawnRequest, Waited};
use letibot_tools::firecode::{FirecodeBackend, FirecodeSpec};

#[test]
fn a_vm_backend_reads_writes_lists_runs_and_lands_its_work() {
    if std::env::var("FIRECODE_LIVE").ok().as_deref() != Some("1") {
        eprintln!("SKIPPED: FIRECODE_LIVE is not 1 — the check did not run and this is not a pass");
        return;
    }
    let src = std::env::temp_dir().join(format!("letibot-fc-src-{}", std::process::id()));
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.txt"), "hello\n").unwrap();
    std::fs::write(src.join("sub/b.bin"), [0u8, 255, 10, 13, 128, 7]).unwrap();
    std::fs::write(src.join("target-not-excluded.txt"), "x").unwrap();
    std::fs::create_dir_all(src.join("target")).unwrap();
    std::fs::write(src.join("target/big"), "should not be copied").unwrap();

    let cache = std::env::temp_dir().join(format!("letibot-fc-cache-{}", std::process::id()));
    // NOT /tmp for a real session (it evaporates); fine for a test that ends.
    let mut spec = FirecodeSpec::new(&src, "child-1");
    spec.cache = cache.clone();
    let t0 = std::time::Instant::now();
    let b = FirecodeBackend::up(&spec).expect("up");
    eprintln!("up in {:.1}s: {}", t0.elapsed().as_secs_f32(), b.describe());
    let copy = b.project().to_path_buf();
    assert!(copy.join("a.txt").exists());
    assert!(
        !copy.join("target").exists(),
        "target is excluded from the copy"
    );

    // read: text and binary, relative and absolute.
    assert_eq!(b.read("a.txt").unwrap(), b"hello\n");
    assert_eq!(b.read("sub/b.bin").unwrap(), [0u8, 255, 10, 13, 128, 7]);
    assert_eq!(
        b.read(&format!("{}/a.txt", copy.display())).unwrap(),
        b"hello\n"
    );
    assert!(matches!(
        b.read("nope.txt"),
        Err(letibot_tools::backend::BackendError::NotFound(_))
    ));
    assert!(matches!(
        b.read("sub"),
        Err(letibot_tools::backend::BackendError::IsADirectory(_))
    ));

    // write: small inline, large through cp, into a new directory; then read back.
    b.write("new/dir/small.txt", b"written in the guest\n")
        .unwrap();
    assert_eq!(
        b.read("new/dir/small.txt").unwrap(),
        b"written in the guest\n"
    );
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    b.write("new/big.bin", &big).unwrap();
    assert_eq!(b.read("new/big.bin").unwrap(), big);
    // Not on the host: the guest's file.
    assert!(!copy.join("new/dir/small.txt").exists());

    // list and stat.
    let names: Vec<String> = b.list("new").unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, vec!["big.bin", "dir"]);
    let st = b.stat("new/dir").unwrap();
    assert!(st.is_dir);
    assert!(b.stat("absent").is_none());
    assert!(matches!(
        b.list("a.txt"),
        Err(letibot_tools::backend::BackendError::NotADirectory(_))
    ));

    // run: argv, cwd, env, exit status.
    let out = b
        .run(&Command {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "pwd; echo $GREETING; cat small.txt; exit 4".into(),
            ],
            cwd: "new/dir".into(),
            env: vec![("GREETING".into(), "hi there".into())],
        })
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("{}/new/dir", copy.display())),
        "{text}"
    );
    assert!(
        text.contains("hi there") && text.contains("written in the guest"),
        "{text}"
    );
    assert_eq!(out.exit, 4);

    // a job through the process host: the same VM, the session's cgroup.
    let host = b.processes().expect("exec");
    let id = host
        .spawn(&SpawnRequest {
            command: "echo job-ran; id -un; exit 0".into(),
            // Nobody named this run: the substrate's own test.
            slug: None,
            cwd: String::new(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: false,
            env: vec![],
            // No terminal: these are the substrate's own tests, not an operator's run.
            tty: false,
            memory: None,
        })
        .unwrap();
    let w = host
        .wait_job(&id, std::time::Duration::from_secs(60))
        .unwrap();
    assert!(matches!(w, Waited::Happened { .. }), "{w:?}");
    let o = host.output(&id, 0, usize::MAX).unwrap();
    let jt = String::from_utf8_lossy(&o.bytes);
    assert!(jt.contains("job-ran"), "{jt}");

    // down: the guest's tree lands beside the copy, with the writes.
    b.down();
    let landed = b.landed().expect("the sibling directory appeared");
    eprintln!("landed in {}", landed.display());
    assert_eq!(
        std::fs::read(landed.join("new/dir/small.txt")).unwrap(),
        b"written in the guest\n"
    );
    assert_eq!(std::fs::read(landed.join("new/big.bin")).unwrap(), big);
    drop(b);
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&src);
}

/// **The acceptance check is run by firecode, and its verdict is the answer's.** A placed child's
/// "done" used to be its word; the check is the harness's, run in the VM after the child, and
/// what `close` says is what firecode recorded. Failed first (the file is not there), then passed.
#[test]
fn a_vm_backends_acceptance_check_decides_what_close_says() {
    if std::env::var("FIRECODE_LIVE").ok().as_deref() != Some("1") {
        eprintln!("SKIPPED: FIRECODE_LIVE is not 1 — the check did not run and this is not a pass");
        return;
    }
    let src = std::env::temp_dir().join(format!("letibot-fc-gate-src-{}", std::process::id()));
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.txt"), "hello\n").unwrap();
    let cache = std::env::temp_dir().join(format!("letibot-fc-gate-cache-{}", std::process::id()));

    for (name, write, says) in [
        ("gate-fails", false, "ACCEPTANCE CHECK FAILED"),
        ("gate-passes", true, "ACCEPTANCE CHECK PASSED"),
    ] {
        let mut spec = FirecodeSpec::new(&src, name);
        spec.cache = cache.clone();
        spec.verify = Some("test -f done.txt".into());
        let b = FirecodeBackend::up(&spec).expect("up");
        if write {
            b.write("done.txt", b"ok\n").unwrap();
        }
        let answer = b.close().expect("an answer");
        eprintln!("{name}: {answer}");
        assert!(answer.contains(says), "{name}: {answer}");
        let landed = b.landed();
        if write {
            let landed = landed.expect("work was delivered");
            assert!(answer.contains(&landed.display().to_string()), "{answer}");
            assert!(
                landed.join(".firecode-verify.log").exists() || answer.contains("log:"),
                "the check's log is named"
            );
        }
        drop(b);
    }
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_dir_all(&src);
}

/// **A worktree is mounted, not copied** (a Mac): the backend boots on the worktree itself, a write
/// lands in it while the VM runs, nothing is copied under the cache, and after `close` the worktree
/// is intact and the answer says the work is there.
#[cfg(target_os = "macos")]
#[test]
fn a_vm_backend_on_a_worktree_works_in_it_directly() {
    if std::env::var("FIRECODE_LIVE").ok().as_deref() != Some("1") {
        eprintln!("SKIPPED: FIRECODE_LIVE is not 1 — the check did not run and this is not a pass");
        return;
    }
    let base = std::env::temp_dir().join(format!("letibot-fc-wt-{}", std::process::id()));
    let repo = base.join("repo");
    let wt = base.join("repo-feat");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["-C", repo.to_str().unwrap(), "init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "base\n").unwrap();
    git(&["-C", repo.to_str().unwrap(), "add", "a.txt"]);
    git(&[
        "-C",
        repo.to_str().unwrap(),
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "base",
    ]);
    git(&[
        "-C",
        repo.to_str().unwrap(),
        "worktree",
        "add",
        "-q",
        "-b",
        "feat",
        wt.to_str().unwrap(),
    ]);
    assert!(letibot_tools::firecode::mounts(&wt));

    let cache = base.join("cache");
    let mut spec = FirecodeSpec::new(&wt, "wt-child");
    spec.cache = cache.clone();
    let b = FirecodeBackend::up(&spec).expect("up");
    eprintln!("{}", b.describe());
    assert_eq!(b.project(), wt.as_path());
    assert!(
        !cache.join("wt-child").exists(),
        "nothing was copied under the cache"
    );
    b.write("from-vm.txt", b"written in the guest\n").unwrap();
    assert_eq!(
        std::fs::read(wt.join("from-vm.txt")).unwrap(),
        b"written in the guest\n",
        "the write is in the worktree on the host while the VM runs"
    );
    let answer = b.close().expect("an answer");
    eprintln!("{answer}");
    assert!(answer.contains("worked in the worktree"), "{answer}");
    assert!(
        wt.join("a.txt").exists() && wt.join("from-vm.txt").exists(),
        "the worktree is intact"
    );
    drop(b);
    let _ = std::fs::remove_dir_all(&base);
}

/// **`--vm`'s folder, mounted and shared** (a Mac): a session's VM boots on the folder itself,
/// read-write; a second session joins that VM instead of booting another; a write through either
/// lands in the folder on the host as it is made; the joiner's end leaves the VM up, and the
/// starter's end brings it down with the folder intact.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_vm_is_mounted_shared_and_joined() {
    if std::env::var("FIRECODE_LIVE").ok().as_deref() != Some("1") {
        eprintln!("SKIPPED: FIRECODE_LIVE is not 1 — the check did not run and this is not a pass");
        return;
    }
    let base = std::env::temp_dir().join(format!("letibot-fc-org-{}", std::process::id()));
    let folder = base.join("org");
    std::fs::create_dir_all(folder.join("repo")).unwrap();
    std::fs::write(folder.join("repo/a.txt"), "base\n").unwrap();
    let cache = base.join("cache");

    let mut first = FirecodeSpec::new(&folder, "starter");
    first.cache = cache.clone();
    first.mount_rw = true;
    let starter = FirecodeBackend::up(&first).expect("up");
    assert!(starter.owns_vm());

    let mut second = FirecodeSpec::new(&folder, "joiner");
    second.cache = cache.clone();
    second.mount_rw = true;
    let joiner = FirecodeBackend::up(&second).expect("join");
    eprintln!("{}", joiner.describe());
    assert!(
        !joiner.owns_vm(),
        "the second session joined the VM rather than booting one"
    );
    assert_eq!(joiner.run_id(), starter.run_id(), "the same VM");

    joiner.write("repo/from-joiner.txt", b"joined\n").unwrap();
    assert_eq!(
        std::fs::read(folder.join("repo/from-joiner.txt")).unwrap(),
        b"joined\n"
    );
    assert!(!cache.exists(), "nothing was copied");

    let said = joiner.close().unwrap();
    eprintln!("{said}");
    drop(joiner);
    assert_eq!(
        starter
            .read("repo/a.txt")
            .expect("the joiner's end left the VM up"),
        b"base\n"
    );
    let said = starter.close().unwrap();
    eprintln!("{said}");
    assert!(
        folder.join("repo/from-joiner.txt").exists(),
        "the folder is intact"
    );
    drop(starter);
    let _ = std::fs::remove_dir_all(&base);
}
