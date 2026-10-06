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
            cwd: String::new(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: false,
            env: vec![],
            // No terminal: these are the substrate's own tests, not an operator's run.
            tty: false,
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
