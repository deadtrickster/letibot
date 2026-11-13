use super::*;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("letibot-firecode-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn file(p: &Path, bytes: usize) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, vec![b'x'; bytes]).unwrap();
}

/// `down_all` brings down every live backend in the process, other tests' included - so
/// the test of it and a test that asserts a VM stays up must not run at the same time. The
/// attach test failed about one run in five on the parent's VM being "downed" under it.
static DOWN_ALL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A stub firecode: `<root>/bin/firecode`, a shell script whose `case "$1"` arms are
/// `arms`, with `<root>/runs/` beside it the way the real checkout lays them out (the
/// backend finds a run's record there through the binary's own path). Returns the binary.
fn stub_firecode(root: &Path, arms: &str) -> PathBuf {
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("runs")).unwrap();
    let bin = root.join("bin").join("firecode");
    crate::testing::write_stub(
        &bin,
        &format!(
            "#!/bin/sh\nROOT='{}'\ncase \"$1\" in\n{arms}\nesac\n",
            root.display()
        ),
    );
    bin
}

/// The arms every stub needs: `up` boots, `info` names run `r1`, `layer inherit` works.
const HAPPY: &str = "up) exit 0 ;;\n\
                         info) echo '{\"run\":\"r1\"}' ;;\n\
                         layer) [ \"$2\" = inherit ] && exit 0; echo 'usage: firecode layer [add <image>|ls|rm <image>|inherit <source-dir>]' >&2; exit 1 ;;";

fn spec_with(src: &Path, cache: &Path, bin: &Path) -> FirecodeSpec {
    let mut spec = FirecodeSpec::new(src, "child");
    spec.cache = cache.to_path_buf();
    spec.exec = false;
    spec.bin = Some(bin.to_path_buf());
    spec
}

/// The CLI a VM placement is checked against is one the caller names, so no test has to set
/// `$FIRECODE_BIN` under every other reader of it in this binary: the refusal names the path
/// it was given, and a bare name is looked for on `PATH`.
#[test]
fn the_firecode_a_caller_names_is_the_one_that_is_looked_for() {
    let root = tmp("available");
    let missing = root.join("no-such-firecode");
    let e = firecode_available_at(missing.clone()).expect_err("nothing is at that path");
    assert!(e.contains(&missing.display().to_string()), "{e}");
    // A program is answered with the very path it was named by.
    let stub = stub_firecode(&root.join("fc"), HAPPY);
    assert_eq!(firecode_available_at(stub.clone()), Ok(stub));
    // A bare name has no path component, so it is a question about `PATH`.
    let bare = format!("letibot-no-such-firecode-{}", std::process::id());
    let e = firecode_available_at(PathBuf::from(&bare)).expect_err("no such program here");
    assert!(e.contains("PATH"), "{e}");
}

/// A child in its parent's VM never brings it down - the operator: *"children live in
/// the _same_ vm"*. An attached backend runs in the parent's project and reports that it
/// does not own the VM; dropping it calls no `down`, and the parent's own drop still does.
#[test]
fn a_child_attached_to_its_parents_vm_leaves_it_up() {
    let _alone = DOWN_ALL.lock().unwrap_or_else(|e| e.into_inner());
    let root = tmp("attach");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    let arms = format!("{HAPPY}\ndown) touch \"$ROOT/downed\" ;;");
    let bin = stub_firecode(&root.join("fc"), &arms);
    let downed = root.join("fc").join("downed");
    let parent = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin))
        .expect("the parent's VM is placed");
    assert!(parent.owns_vm());
    let child = FirecodeBackend::attach(
        Some(bin.clone()),
        parent.project().to_path_buf(),
        true,
        false,
    )
    .expect("the child attaches to the parent's VM");
    assert!(!child.owns_vm(), "the VM is the parent's");
    assert_eq!(child.project(), parent.project(), "the same tree");
    drop(child);
    assert!(
        !downed.exists(),
        "the child's end took the parent's VM down"
    );
    drop(parent);
    assert!(downed.exists(), "the parent's own end still brings it down");
    let _ = std::fs::remove_dir_all(&root);
}

/// A placement that fails leaves no copy behind - FIRECODE-NOTES: *"A failed spawn leaks
/// its copy. The very first attempt left 286 MB in ~/.cache/letibot/firecode/; the error
/// path remove_dir_alls on a refused status but not on a spawn error."*
#[test]
fn a_firecode_that_cannot_be_run_leaves_no_copy_behind() {
    let root = tmp("unrunnable");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    // A directory where the program should be: running it is a spawn error.
    let bin = root.join("not-a-program");
    std::fs::create_dir_all(&bin).unwrap();
    let cache = root.join("cache");
    let r = FirecodeBackend::up(&spec_with(&src, &cache, &bin));
    assert!(r.is_err(), "a firecode that cannot run placed a VM");
    assert!(
        !cache.join("child").exists(),
        "the copy outlived the failed placement: {:?}",
        r.err()
    );
}

/// A VM's jobs can be watched: the owned process host is the same one `processes` lends.
/// Without it the trait default is `None`, so no job watcher was built for a firecode session
/// and a finished background job was never announced.
#[test]
fn a_vm_that_runs_processes_hands_its_host_to_the_job_watcher() {
    // An exec VM builds its process host on a process-lifetime tree, and CI's runner has none,
    // so this test refuses under `LETIBOT_REQUIRE_APPARATUS=1` like the other host-spawning
    // tests.
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        crate::host_tree().is_ok(),
    ) else {
        return;
    };
    let root = tmp("procarc");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    let bin = stub_firecode(&root.join("fc"), HAPPY);
    let mut spec = spec_with(&src, &root.join("cache"), &bin);
    spec.exec = true;
    let b = FirecodeBackend::up(&spec).expect("the VM is placed");
    assert!(b.processes().is_some(), "an exec VM lends no process host");
    let owned = b
        .processes_arc()
        .expect("an exec VM hands no owned process host to the job watcher");
    let lent = b.processes().unwrap() as *const dyn crate::exec::ProcessHost as *const ();
    assert_eq!(
        Arc::as_ptr(&owned) as *const (),
        lent,
        "the owned host is not the one the tools run on"
    );

    let mut spec = spec_with(&src, &root.join("cache2"), &bin);
    spec.name = "child2".into();
    let b = FirecodeBackend::up(&spec).expect("the VM is placed");
    assert!(
        b.processes_arc().is_none(),
        "a no-exec VM hands out a process host"
    );
}

/// No `layer inherit` is a missing convenience, not a missing VM - FIRECODE-NOTES item 4:
/// *"degrade where only a convenience is missing ("no layer inheritance; the guest builds on
/// base-image toolchains") and refuse only what is genuinely required."*
#[test]
fn a_firecode_without_layer_inherit_still_places_the_vm_and_says_so() {
    let root = tmp("noinherit");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    let bin = stub_firecode(
        &root.join("fc"),
        "up) exit 0 ;;\n\
             info) echo '{\"run\":\"r1\"}' ;;\n\
             layer) echo 'usage: firecode layer [add <image>|ls|rm <image>] [--project DIR]' >&2; exit 1 ;;",
    );
    let b = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin))
        .expect("the VM is placed without inherited layers");
    assert!(
        b.describe().contains("no layer inheritance"),
        "the missing layers are not said: {}",
        b.describe()
    );
}

/// `down` waits while firecode says the copy-out is still running, however long that is: a
/// fixed wait reported a child's work missing while it was still being delivered, and the
/// directory appeared two minutes later. The stub's copy-out takes 12 s; the old wait gave up at 10.
#[test]
fn down_waits_while_the_copy_out_is_pending() {
    let root = tmp("pending");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    let fc = root.join("fc");
    let delivered = root.join("delivered");
    let arms = format!(
        "{HAPPY}\n\
             down) mkdir -p \"$ROOT/runs/r1\"; echo pending > \"$ROOT/runs/r1/copy_out\"; \
             (sleep 12; mkdir -p '{d}'; echo 'done {d}' > \"$ROOT/runs/r1/copy_out.t\"; \
             mv \"$ROOT/runs/r1/copy_out.t\" \"$ROOT/runs/r1/copy_out\") >/dev/null 2>&1 & exit 0 ;;",
        d = delivered.display()
    );
    let bin = stub_firecode(&fc, &arms);
    let b = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin)).unwrap();
    b.down();
    assert_eq!(
        b.landed().as_deref(),
        Some(delivered.as_path()),
        "down gave up on a copy-out firecode said was still running"
    );
}

/// The record names where the work went, so the backend does not have to guess from
/// the directory names beside the copy.
#[test]
fn down_takes_the_delivered_path_from_the_run_record() {
    let root = tmp("record");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    let fc = root.join("fc");
    let delivered = root.join("elsewhere").join("work");
    let arms = format!(
        "{HAPPY}\n\
             down) mkdir -p \"$ROOT/runs/r1\" '{d}'; echo 'done {d}' > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;",
        d = delivered.display()
    );
    let bin = stub_firecode(&fc, &arms);
    let b = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin)).unwrap();
    b.down();
    assert_eq!(b.landed().as_deref(), Some(delivered.as_path()));
    assert!(
        !root.join("cache").join("child").exists(),
        "the copy is removed once the work landed"
    );
}

/// A worktree is mounted, a submodule and a plain tree are copied - both a worktree and a
/// submodule have a `.git` file, and only a worktree's names `worktrees/`.
#[cfg(target_os = "macos")]
#[test]
fn a_worktree_is_mounted_and_a_submodule_is_not() {
    let root = tmp("mounts");
    let wt = root.join("repo-x");
    let sub = root.join("sub");
    let plain = root.join("plain");
    for d in [&wt, &sub, &plain] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(wt.join(".git"), "gitdir: /p/repo/.git/worktrees/repo-x\n").unwrap();
    std::fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();
    assert!(mounts(&wt));
    assert!(!mounts(&sub));
    assert!(!mounts(&plain));
    assert_eq!(
        project_for(&wt, "s1"),
        wt,
        "a mounted VM's project is the worktree"
    );
    assert_ne!(
        project_for(&plain, "s1"),
        plain,
        "a copied one's is the copy"
    );
}

/// A mounted placement copies nothing and never deletes the worktree - the project IS the
/// operator's worktree, so the copy-and-clean-up path must not reach it, on success or after
/// `down`, and the answer says the work is in the worktree rather than in a copy.
#[cfg(target_os = "macos")]
#[test]
fn a_mounted_worktree_is_neither_copied_nor_removed() {
    let root = tmp("mounted");
    let wt = root.join("repo-x");
    file(&wt.join("work.txt"), 10);
    std::fs::write(wt.join(".git"), "gitdir: /p/repo/.git/worktrees/repo-x\n").unwrap();
    let fc = root.join("fc");
    let arms = format!(
        "{HAPPY}\n\
             down) mkdir -p \"$ROOT/runs/r1\"; echo 'done {w}' > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;",
        w = wt.display()
    );
    let bin = stub_firecode(&fc, &arms);
    let b = FirecodeBackend::up(&spec_with(&wt, &root.join("cache"), &bin)).unwrap();
    assert_eq!(
        b.project(),
        wt.as_path(),
        "the VM's project is the worktree itself"
    );
    assert!(
        !root.join("cache").join("child").exists(),
        "nothing was copied"
    );
    let answer = b.close().unwrap();
    assert!(wt.join("work.txt").exists(), "the worktree survives down");
    assert!(answer.contains("worked in the worktree"), "{answer}");
    assert!(answer.contains("Nothing was copied"), "{answer}");
}

/// `--vm`'s folder is mounted read-write and never copied or removed - booted with
/// `--mount-rw` on the folder itself, nothing under the cache, the folder intact after close,
/// and the answer says the work is already there.
#[test]
fn a_folder_placement_mounts_it_and_copies_nothing() {
    let root = tmp("mountrw");
    let folder = root.join("org");
    file(&folder.join("repo/a.txt"), 10);
    let fc = root.join("fc");
    let argv = root.join("up-argv");
    let arms = format!(
        "up) printf '%s\\n' \"$@\" > '{a}'; exit 0 ;;\n\
             info) [ \"$2\" = --json ] && exit 1; echo '{{\"run\":\"r1\"}}' ;;\n\
             down) mkdir -p \"$ROOT/runs/r1\"; echo 'done {f}' > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;",
        a = argv.display(),
        f = folder.display()
    );
    let bin = stub_firecode(&fc, &arms);
    let mut spec = spec_with(&folder, &root.join("cache"), &bin);
    spec.mount_rw = true;
    let b = FirecodeBackend::up(&spec).unwrap();
    assert!(
        std::fs::read_to_string(&argv)
            .unwrap()
            .lines()
            .any(|l| l == "--mount-rw")
    );
    assert_eq!(b.project(), folder.as_path());
    assert!(
        !root.join("cache").join("child").exists(),
        "nothing was copied"
    );
    assert!(
        b.describe().contains("mounted read-write"),
        "{}",
        b.describe()
    );
    let answer = b.close().unwrap();
    assert!(
        folder.join("repo/a.txt").exists(),
        "the folder survives down"
    );
    assert!(answer.contains("already there"), "{answer}");
}

/// A VM already up with the folder mounted is joined, not doubled - and a joiner neither
/// boots one nor brings the joined one down. One holding only a COPY of the folder is not
/// joined: its tree is not the folder.
#[test]
fn a_running_vm_with_the_folder_mounted_is_joined_and_one_with_a_copy_is_not() {
    for (mount, joins) in [("rw", true), ("copy", false)] {
        let root = tmp(&format!("join-{mount}"));
        let folder = root.join("org");
        file(&folder.join("a.txt"), 10);
        let fc = root.join("fc");
        let booted = root.join("booted");
        let downed = root.join("downed");
        let arms = format!(
            "up) touch '{b}'; exit 0 ;;\n\
                 info) printf '{{\"run\":\"r9\",\"project\":\"%s\",\"mount\":\"{mount}\"}}' '{f}' ;;\n\
                 layer) exit 0 ;;\n\
                 down) touch '{d}'; exit 0 ;;",
            b = booted.display(),
            d = downed.display(),
            f = folder.display()
        );
        let bin = stub_firecode(&fc, &arms);
        let mut spec = spec_with(&folder, &root.join("cache"), &bin);
        spec.mount_rw = true;
        let b = FirecodeBackend::up(&spec).unwrap();
        assert_eq!(
            !booted.exists(),
            joins,
            "mount {mount}: booted={}",
            booted.exists()
        );
        assert_eq!(!b.owns_vm(), joins, "a joined VM is not ours");
        if joins {
            let answer = b.close().unwrap();
            assert!(answer.contains("joined"), "{answer}");
            drop(b);
            assert!(!downed.exists(), "a joiner never brings the VM down");
        }
    }
}

/// The parent's acceptance check reaches `firecode up`, as `--verify <command>`, so it is
/// firecode - not the child - that runs it when the VM comes down.
#[test]
fn the_acceptance_check_is_handed_to_firecode_up() {
    let root = tmp("verify-up");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    let fc = root.join("fc");
    let argv = root.join("up-argv");
    let arms = format!(
        "up) printf '%s\\n' \"$@\" > '{a}'; exit 0 ;;\n\
             info) echo '{{\"run\":\"r1\"}}' ;;\n\
             layer) exit 0 ;;",
        a = argv.display()
    );
    let bin = stub_firecode(&fc, &arms);
    let mut spec = spec_with(&src, &root.join("cache"), &bin);
    spec.verify = Some("make e2e TEST=x".into());
    let _b = FirecodeBackend::up(&spec).unwrap();
    let said = std::fs::read_to_string(&argv).unwrap();
    let args: Vec<&str> = said.lines().collect();
    let at = args
        .iter()
        .position(|a| *a == "--verify")
        .expect("--verify reaches up");
    assert_eq!(
        args.get(at + 1),
        Some(&"make e2e TEST=x"),
        "and carries the command whole"
    );
}

/// A failed check is the answer's headline, beside where the work is - read from
/// firecode's record of the run, not from anything the child said.
#[test]
fn the_answer_carries_the_checks_verdict_and_where_the_work_is() {
    for (status, says) in [
        ("1", "ACCEPTANCE CHECK FAILED: `make e2e` exited 1"),
        ("0", "ACCEPTANCE CHECK PASSED: `make e2e` exited 0"),
        ("missing", "ACCEPTANCE CHECK GAVE NO VERDICT"),
    ] {
        let root = tmp(&format!("verdict-{status}"));
        let src = root.join("src");
        file(&src.join("a.txt"), 10);
        let fc = root.join("fc");
        let delivered = root.join("work");
        let arms = format!(
            "{HAPPY}\n\
                 down) mkdir -p \"$ROOT/runs/r1\" '{d}'; echo '{status}' > \"$ROOT/runs/r1/verify-status\"; \
                 echo 'done {d}' > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;",
            d = delivered.display()
        );
        let bin = stub_firecode(&fc, &arms);
        let mut spec = spec_with(&src, &root.join("cache"), &bin);
        spec.verify = Some("make e2e".into());
        let b = FirecodeBackend::up(&spec).unwrap();
        let answer = b.close().unwrap();
        assert!(answer.contains(says), "for status {status}: {answer}");
        assert!(
            answer.contains(&delivered.display().to_string()),
            "the delivered path is in the answer: {answer}"
        );
    }
}

/// A failed copy-out is said as one, and points at the copy that is now the only place
/// the work is, never "nothing appeared within 10 s".
#[test]
fn a_failed_copy_out_is_reported_as_a_failure_not_as_silence() {
    let root = tmp("copy-failed");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    let fc = root.join("fc");
    let arms = format!(
        "{HAPPY}\n\
             down) mkdir -p \"$ROOT/runs/r1\"; echo 'failed: no space left' > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;"
    );
    let bin = stub_firecode(&fc, &arms);
    let b = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin)).unwrap();
    let answer = b.close().unwrap();
    assert!(answer.contains("failed: no space left"), "{answer}");
    assert!(answer.contains("only place the work is"), "{answer}");
    assert!(!answer.contains("within 10 s"), "{answer}");
    assert!(
        root.join("cache").join("child").exists(),
        "the copy is kept"
    );
}

/// A daemon that stops brings its children's VMs down: their backends run on threads
/// `process::exit` does not unwind, so their `Drop` never ran and the VMs outlived the daemon.
/// `down_all` is the explicit half the daemon calls on its way out.
#[test]
fn down_all_brings_down_a_vm_whose_backend_is_still_alive() {
    let _alone = DOWN_ALL.lock().unwrap_or_else(|e| e.into_inner());
    let root = tmp("downall");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    let marker = root.join("down-was-called");
    let arms = format!(
        "{HAPPY}\n\
             down) touch '{m}'; mkdir -p \"$ROOT/runs/r1\"; echo none > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;",
        m = marker.display()
    );
    let bin = stub_firecode(&root.join("fc"), &arms);
    let b = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin)).unwrap();
    down_all();
    assert!(
        marker.exists(),
        "the VM of a live backend was not brought down"
    );
    drop(b);
}

/// What the installed firecode can do, asked once - for the daemon's start-up banner.
#[test]
fn the_probe_says_whether_layer_inherit_is_there() {
    let root = tmp("probe");
    let with = stub_firecode(&root.join("with"), HAPPY);
    let without = stub_firecode(
        &root.join("without"),
        "layer) echo 'usage: firecode layer [add <image>|ls|rm <image>] [--project DIR]' >&2; exit 1 ;;",
    );
    assert!(probe(&with).inherit, "{:?}", probe(&with));
    assert!(!probe(&without).inherit, "{:?}", probe(&without));
}

/// The workspace is the folder ABOVE the checkout, so
/// the build output and the agent worktrees are one level down. Every one of
/// them must be left behind, at that depth and deeper.
#[test]
fn excludes_apply_at_any_depth_not_only_at_the_root() {
    let src = tmp("src");
    let dst = tmp("dst").join("copy");
    file(&src.join("repo/crates/a/src/lib.rs"), 10);
    file(&src.join("repo/target/release/big"), 5000);
    file(&src.join("repo/.claude/worktrees/agent-1/target/x"), 5000);
    file(&src.join("repo/.claude/worktrees/agent-1/src/y.rs"), 10);
    file(&src.join("repo/web/node_modules/pkg/index.js"), 5000);
    file(&src.join("target/root-level"), 5000);

    let exclude = FirecodeSpec::new(&src, "t").exclude;
    copy_tree(&src, &dst, &exclude, u64::MAX).unwrap();

    assert!(dst.join("repo/crates/a/src/lib.rs").exists());
    assert!(
        !dst.join("repo/target").exists(),
        "nested target was copied"
    );
    assert!(
        !dst.join("repo/.claude/worktrees").exists(),
        "nested worktrees were copied"
    );
    assert!(
        !dst.join("repo/web/node_modules").exists(),
        "nested node_modules was copied"
    );
    assert!(!dst.join("target").exists(), "root target was copied");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(dst.parent().unwrap());
}

/// A copy over the ceiling is refused BEFORE anything is written, with the
/// size and the ceiling named.
#[test]
fn a_copy_over_the_ceiling_is_refused_with_its_size_named_and_nothing_written() {
    let src = tmp("big");
    let dst = tmp("bigdst").join("copy");
    file(&src.join("repo/data/blob"), 200_000);
    let exclude = FirecodeSpec::new(&src, "t").exclude;

    let err = copy_tree(&src, &dst, &exclude, 100_000)
        .unwrap_err()
        .to_string();
    assert!(err.contains("refusing to copy"), "{err}");
    assert!(
        err.contains("195 MiB") || err.contains("200000 bytes"),
        "{err}"
    );
    assert!(err.contains("LETIBOT_FIRECODE_COPY_MAX"), "{err}");
    assert!(!dst.exists(), "the refusal wrote the copy anyway");

    // The same tree under the ceiling copies.
    copy_tree(&src, &dst, &exclude, 1_000_000).unwrap();
    assert!(dst.join("repo/data/blob").exists());
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(dst.parent().unwrap());
}

/// The measurement counts what the copy would carry, not what the excludes
/// leave behind - otherwise the ceiling would refuse the very copies the
/// excludes made small.
#[test]
fn the_measurement_honours_the_excludes() {
    let src = tmp("meas");
    let dst = tmp("measdst").join("copy");
    file(&src.join("repo/src/main.rs"), 100);
    file(&src.join("repo/target/huge"), 900_000);
    let exclude = FirecodeSpec::new(&src, "t").exclude;
    let size = measure_copy(&src, &dst, &exclude).unwrap();
    assert!(size < 10_000, "excluded build output was measured: {size}");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(dst.parent().unwrap());
}

#[test]
fn base64_round_trips_binary_and_the_paddings() {
    for input in [
        &b""[..],
        b"f",
        b"fo",
        b"foo",
        b"foob",
        b"fooba",
        b"foobar",
        &[0u8, 255, 10, 13, 128][..],
    ] {
        let e = encode_base64(input);
        assert_eq!(decode_base64(e.as_bytes()).unwrap(), input, "{e}");
    }
    assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
    assert_eq!(encode_base64(b"fo"), "Zm8=");
}

/// Is this pid gone? Polled, because a killed process answers `kill -0` for as long as it takes
/// its new reaper to collect it. Ten seconds is far more than a reaper needs and still fails
/// loudly rather than passing on a ghost.
fn wait_gone(pid: i32, within: Duration) -> bool {
    let began = std::time::Instant::now();
    loop {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !alive {
            return true;
        }
        if began.elapsed() >= within {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A guest call that loses its reply fails in seconds and leaves nothing running: the stub
/// answers `true` and otherwise hangs, so the call returns `Err` at the deadline naming the
/// timeout, and both the client and the `sleep` it started are gone.
#[test]
fn a_guest_call_that_loses_its_reply_fails_in_seconds_and_leaves_nothing_running() {
    let root = tmp("lost-reply");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    let pids = root.join("in.pids");
    let arms = format!(
        "{HAPPY}\n\
             in) for a do last=$a; done\n\
                 case \"$last\" in true) exit 0 ;; esac\n\
                 sleep 30 & echo \"$! $$\" > '{p}'; wait ;;\n\
             down) mkdir -p \"$ROOT/runs/r1\"; echo none > \"$ROOT/runs/r1/copy_out\" ;;",
        p = pids.display()
    );
    let bin = stub_firecode(&root.join("fc"), &arms);
    let b = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin))
        .expect("the VM is placed")
        .with_guest_timeout(Duration::from_secs(1));

    let began = std::time::Instant::now();
    let e = b
        .read("a.txt")
        .expect_err("a guest that never answers answered");
    let took = began.elapsed();
    let said = format!("{e}");

    assert!(said.contains("did not answer within 1 s"), "{said}");
    assert!(
        took >= Duration::from_millis(900),
        "the call came back in {took:?}, before its 1 s deadline: it failed for some other \
             reason — {said}"
    );
    assert!(
        took < Duration::from_secs(15),
        "the call waited {took:?} on a 1 s deadline: {said}"
    );
    assert!(
        said.contains("Retrying is safe for a read"),
        "the sentence does not say what a reviewer may do next: {said}"
    );

    // Nothing survives it. The client is the direct child; the `sleep` is its own child and
    // shares its process group, which is what the group kill takes. A client-only kill leaves
    // the ghost the reviewer's next call queues behind.
    let recorded = std::fs::read_to_string(&pids).expect("the stub never ran");
    let seen: Vec<i32> = recorded
        .split_whitespace()
        .map(|p| p.parse().expect("a pid"))
        .collect();
    assert_eq!(
        seen.len(),
        2,
        "the stub recorded {recorded:?}, not the sleep and the client"
    );
    for pid in seen {
        assert!(
            wait_gone(pid, Duration::from_secs(10)),
            "pid {pid} outlived the deadline: {said}"
        );
    }
    drop(b);
    let _ = std::fs::remove_dir_all(&root);
}

/// The batched read is the files, whatever is in them: the real command run by a real `/bin/sh`
/// over real files. The marker byte, a NUL, a CR and a byte that is not UTF-8 all sit inside a
/// file and none can be mistaken for a boundary.
#[test]
fn a_page_of_files_survives_the_round_trip_whatever_is_in_them() {
    let root = tmp("batch-read");
    let plain = root.join("plain.txt");
    std::fs::write(&plain, "one\ntwo\n").unwrap();
    let empty = root.join("empty.txt");
    std::fs::write(&empty, b"").unwrap();
    let nasty = root.join("nasty.bin");
    let nasty_bytes: &[u8] = &[0, b'\n', 0x1e, 0xff, 0x1e, b'\r', 0x1e];
    std::fs::write(&nasty, nasty_bytes).unwrap();
    let dir = root.join("a-directory");
    std::fs::create_dir_all(&dir).unwrap();
    let gone = root.join("gone.txt");

    let owned: Vec<String> = [&plain, &empty, &nasty, &dir, &gone]
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let asked: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    let out = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(batched_read_command(&asked))
        .output()
        .expect("a shell to run the guest command");
    assert!(
        out.status.success(),
        "the guest command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let got = parse_batched_reads(&out.stdout, &asked).expect("one slot per path");
    assert_eq!(got.len(), 5);
    assert_eq!(
        got[0].as_ref().unwrap(),
        b"one\ntwo\n",
        "the first file answers in the first slot"
    );
    assert_eq!(
        got[1].as_ref().unwrap(),
        b"",
        "an empty file is a file of zero bytes, not a missing one"
    );
    assert_eq!(
        got[2].as_ref().unwrap(),
        nasty_bytes,
        "a file holding the marker, a NUL, a CR and a non-UTF-8 byte"
    );
    assert!(
        matches!(got[3].as_ref(), Err(BackendError::IsADirectory(_))),
        "a directory answers as `read` answers it: {:?}",
        got[3]
    );
    assert!(
        matches!(got[4].as_ref(), Err(BackendError::NotFound(_))),
        "a path that is not there answers as `read` answers it: {:?}",
        got[4]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A stream that does not account for every path is the CALL's failure, which is the caller's
/// signal to fall back, while a single short slot is that file's own error and leaves its
/// neighbours intact.
#[test]
fn a_lost_slot_fails_the_call_and_a_short_slot_fails_only_its_file() {
    // The guest printed one file for two paths (it was killed mid-page, say).
    assert!(
        parse_batched_reads(b"\x1e3\nYWJj\n", &["a", "b"]).is_err(),
        "a page that came back short must fail the call, not answer for two files"
    );
    // A banner before the first marker is the run-up and is dropped; a slot that
    // claims nine bytes and holds three is that file's error, and the next slot
    // still answers.
    let stream: &[u8] = b"a banner from bash -l\n\x1e9\nYWJj\n\x1e3\nZGVm\n";
    let got = parse_batched_reads(stream, &["short", "fine"]).expect("two slots");
    assert!(
        matches!(got[0].as_ref(), Err(BackendError::Io(_))),
        "a truncated body was handed to the matcher as the file: {:?}",
        got[0]
    );
    assert_eq!(got[1].as_ref().unwrap(), b"def");
}

#[test]
fn the_confinement_wraps_a_job_as_firecode_in_with_the_cwd() {
    let c = FirecodeConfinement::new(PathBuf::from("/usr/bin/firecode"), PathBuf::from("/w/copy"));
    let plan = ConfinePlan {
        cwd: Path::new("/w/copy/sub"),
        env: &[],
    };
    assert_eq!(
        c.wrap(&plan).unwrap(),
        vec![
            "/usr/bin/firecode",
            "in",
            "--project",
            "/w/copy",
            "--cwd",
            "/w/copy/sub"
        ]
    );
    assert!(c.describe().contains("NOT confined"));
    assert!(c.boundary().is_none());
    assert!(c.memory_is_elsewhere(), "the guest's RAM bounds a VM job");
}

#[test]
fn quoting_leaves_plain_words_and_wraps_the_rest() {
    assert_eq!(shell_quote("cargo"), "cargo");
    assert_eq!(shell_quote("a b"), "'a b'");
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
    assert_eq!(shell_quote(""), "''");
}

/// The perimeter's pairs are spelled in front of the guest's command, since `firecode in` has no
/// env flag, and a value a file or a command gave is refused: the command string is an argv this
/// box's `ps` reads, and a VM has no second channel.
#[test]
fn a_guest_command_is_spelled_with_the_perimeters_pairs_and_refuses_a_secret() {
    let env = vec![("A_PAIR".to_string(), "a value".to_string())];
    let own = vec![("OWN".to_string(), "its own".to_string())];
    assert_eq!(
        guest_env_prefix(&env, &own),
        "A_PAIR='a value' OWN='its own' "
    );
    assert!(secret_names_in_a_guest(&[], &env).is_empty());
    // A name the call calls a secret, and a value whose PROVENANCE makes it one.
    assert_eq!(
        secret_names_in_a_guest(
            &[("PLAINLY_NAMED".to_string(), "v".to_string())],
            &[("GH_TOKEN".to_string(), "v".to_string())]
        ),
        vec!["GH_TOKEN".to_string(), "PLAINLY_NAMED".to_string()]
    );
}

/// One round trip for the whole tree, end to end, which is why [`ExecBackend::list_tree`]
/// exists: the stub answers the one `find` with the guest's listing, and what comes back is
/// what the per-directory loop would have produced. The `in` call log is the count.
#[test]
fn the_tree_is_one_in_call_and_parses_like_the_loop() {
    let root = tmp("tree");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    let cache = root.join("cache");
    let copy = cache.join("child");
    let calls = root.join("in-calls");
    let arms = format!(
        "{HAPPY}\n\
             in) echo \"$@\" >> '{calls}'; printf 'd\\t0\\t{c}/a b\\nf\\t7\\t{c}/a b/inner.txt\\n\
             f\\t12\\t{c}/plain.txt\\nd\\t0\\t{c}/target\\nf\\t999\\t{c}/target/junk\\n' ;;",
        calls = calls.display(),
        c = copy.display()
    );
    let bin = stub_firecode(&root.join("fc"), &arms);
    let b = FirecodeBackend::up(&spec_with(&src, &cache, &bin)).unwrap();

    let in_calls = || {
        std::fs::read_to_string(&calls)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let before = in_calls();
    let (got, truncated) = crate::backend::walk(
        &b,
        &copy.display().to_string(),
        100,
        &crate::backend::default_skip,
    );
    let log = std::fs::read_to_string(&calls).unwrap_or_default();

    let at = |rel: &str, name: &str, is_dir: bool, bytes: u64| DirEntry {
        path: format!("{}/{rel}", copy.display()),
        name: name.into(),
        is_dir,
        bytes,
        modified: None,
    };
    assert_eq!(
        got,
        vec![
            at("a b", "a b", true, 0),
            at("plain.txt", "plain.txt", false, 12),
            at("a b/inner.txt", "inner.txt", false, 7),
        ],
        "the tree came back in find's order, or with the skipped target's contents"
    );
    assert!(!truncated);
    // The tree is one round trip: exactly one `in` ran a `find`. `up` probes the
    // door with an `in ... true` of its own, so the count is of tree commands and
    // not of every call the backend made.
    let tree: Vec<&str> = log.lines().filter(|l| l.contains("find ")).collect();
    assert_eq!(
        tree.len(),
        1,
        "the tree took {} round trips, not one (in calls {before} -> {}): {log}",
        tree.len(),
        in_calls()
    );
    // And the one call really is the whole tree: no depth limit, and the path
    // form the parser needs.
    let tree_call = tree.first().copied().unwrap_or_default();
    assert!(
        tree_call.contains("-mindepth 1")
            && tree_call.contains("%p")
            && !tree_call.contains("-maxdepth"),
        "the one command is not the whole tree: {tree_call}"
    );
}

/// A page of files really is one round trip through the backend, not merely one command: the
/// stub runs the guest command over the copy `up` made, and the `in` call log is the count.
/// The empty file and the missing path are in the page on purpose.
#[test]
fn a_page_is_one_in_call_and_its_slots_are_the_files() {
    let root = tmp("batch-in");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for (name, body) in [
        ("a.txt", "one\ntwo\n"),
        ("b.txt", ""),
        ("c.txt", "needle_here\n"),
    ] {
        std::fs::write(src.join(name), body).unwrap();
    }
    let cache = root.join("cache");
    let calls = root.join("in-calls");
    // The stub is the guest: it logs the call, then RUNS the command it was
    // handed, so the bytes parsed below are the bytes a real VM would print.
    let arms = format!(
        "{HAPPY}\nin) echo \"$@\" >> '{calls}'; sh -c \"$6\" ;;",
        calls = calls.display()
    );
    let bin = stub_firecode(&root.join("fc"), &arms);
    let b = FirecodeBackend::up(&spec_with(&src, &cache, &bin)).unwrap();

    let in_calls = || {
        std::fs::read_to_string(&calls)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let before = in_calls();
    let got = b
        .read_many(&["a.txt", "b.txt", "c.txt", "gone.txt"])
        .expect("a page");
    let log = std::fs::read_to_string(&calls).unwrap_or_default();
    // `up` keeps talking to the VM for its own reasons (its door probe, its run
    // record), so the claim is about the commands that read FILES: one that carries
    // the page, and none of the old one-file shape.
    let paged: Vec<&str> = log.lines().filter(|l| l.contains("for f in")).collect();
    let per_file: Vec<&str> = log.lines().filter(|l| l.contains("base64 -w0 <")).collect();
    assert_eq!(
        (paged.len(), per_file.len()),
        (1, 0),
        "four files took {} page call(s) and {} per-file call(s) (in calls {before} -> {}):\n{log}",
        paged.len(),
        per_file.len(),
        in_calls()
    );
    assert_eq!(got[0].as_ref().unwrap(), b"one\ntwo\n");
    assert_eq!(got[1].as_ref().unwrap(), b"", "the empty file, not a miss");
    assert_eq!(got[2].as_ref().unwrap(), b"needle_here\n");
    assert!(
        matches!(got[3].as_ref(), Err(BackendError::NotFound(_))),
        "a path that is not there answered with bytes: {:?}",
        got[3]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A file that is there and cannot be read is not a file that is not there. The batch answered
/// both with `-`, which `notes` reads as *this note is not there*, so a corpus with one
/// unreadable note was reported as a whole one.
#[test]
fn a_file_that_is_there_and_cannot_be_read_is_not_a_missing_one() {
    use std::os::unix::fs::PermissionsExt;

    let root = tmp("batch-locked");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("open.md"), "read me\n").unwrap();

    let cache = root.join("cache");
    let calls = root.join("in-calls");
    let arms = format!(
        "{HAPPY}\nin) echo \"$@\" >> '{calls}'; sh -c \"$6\" ;;",
        calls = calls.display()
    );
    let bin = stub_firecode(&root.join("fc"), &arms);
    let b = FirecodeBackend::up(&spec_with(&src, &cache, &bin)).unwrap();

    // The file is put in the GUEST's tree, which is the copy `up` made: a file the copy could
    // not read would fail the copy rather than reach the page.
    let locked = cache.join("child").join("locked.md");
    std::fs::write(&locked, "cannot be read\n").unwrap();
    let mut perms = std::fs::metadata(&locked).unwrap().permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&locked, perms).unwrap();
    // The fixture, proved before the subject is asked: a box that reads a 0000 file anyway
    // (this test as root) cannot build the case, and says so rather than passing.
    assert!(
        std::fs::read(&locked).is_err(),
        "this box reads a 0000 file, so the unreadable case cannot be built here"
    );

    let got = b
        .read_many(&["open.md", "locked.md", "gone.md"])
        .expect("a page");
    assert_eq!(got[0].as_ref().unwrap(), b"read me\n");
    assert!(
        matches!(&got[1], Err(BackendError::Io(said)) if said.contains("locked.md")),
        "a file that is there and unreadable answered as one that is not there: {:?}",
        got[1]
    );
    assert!(
        matches!(&got[2], Err(BackendError::NotFound(_))),
        "a path that is not there is still a miss: {:?}",
        got[2]
    );

    let mut perms = std::fs::metadata(&locked).unwrap().permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(&locked, perms).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// The `in` arm a test needs when the point is a command the guest never answers on its own: the
/// client blocks until the VM is gone, exactly as the real one does when its guest stops, and
/// `down` leaves the marker that tells it.
const BLOCKS_UNTIL_DOWN: &str = "in) shift
    cmd=
    while [ $# -gt 0 ]; do
      case \"$1\" in
        --project|--cwd) shift 2 ;;
        *) cmd=\"$*\"; break ;;
      esac
    done
    sh -c \"$cmd\" ;;
down) touch \"$ROOT/up-down\"; mkdir -p \"$ROOT/runs/r1\"; printf 'none\\n' > \"$ROOT/runs/r1/copy_out\" ;;";

/// A package VM boots 32 GiB of layer, not the image's own 16: a debug build of this workspace's
/// test binaries fills that, and `ld` dies of a bus error when it does.
#[test]
fn a_boot_names_the_layer_size_a_debug_build_needs() {
    let root = tmp("layer-env");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    let arms = "up) printf '%s\\n' \"${FIRECODE_LAYER_SIZE-}\" > \"$ROOT/up.layer\"; exit 0 ;;\n\
                info) echo '{\"run\":\"r1\"}' ;;\n\
                layer) [ \"$2\" = inherit ] && exit 0; \
                       echo 'usage: firecode layer [add <image>|ls|rm <image>|inherit <source-dir>]' >&2; \
                       exit 1 ;;";
    let bin = stub_firecode(&root.join("fc"), arms);
    let mut spec = spec_with(&src, &root.join("cache"), &bin);
    // Mounted, not copied: `copy_tree` wants `rsync`, which a guest without one does not have, and
    // the copy is not what this test is about.
    spec.mount_rw = true;
    let vm = FirecodeBackend::up(&spec).expect("the VM is placed");
    let named = std::fs::read_to_string(root.join("fc").join("up.layer")).unwrap_or_default();
    assert_eq!(
        named.trim(),
        LAYER_SIZE_BYTES.to_string(),
        "`firecode up` was given no layer size: {named:?}"
    );
    assert_eq!(LAYER_SIZE_BYTES, 32 * 1024 * 1024 * 1024, "32 GiB");
    drop(vm);
    let _ = std::fs::remove_dir_all(&root);
}

/// The door a session's close uses when its worker is inside a tool call: the VM is found by the
/// session in the process's own record, brought down, and the client blocked in the guest comes
/// back - which is what frees the worker to serve the sessions queued behind it.
#[test]
fn a_close_brings_down_the_vm_its_session_holds_from_any_thread() {
    let _alone = DOWN_ALL.lock().unwrap_or_else(|e| e.into_inner());
    let root = tmp("close-down");
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    let arms = format!("{HAPPY}\n{BLOCKS_UNTIL_DOWN}");
    let bin = stub_firecode(&root.join("fc"), &arms);
    let downed = root.join("fc").join("up-down");
    let mut spec = spec_with(&src, &root.join("cache"), &bin);
    spec.name = "s-close".to_string();
    // Mounted, not copied: `copy_tree` wants `rsync`, and the copy is not what this is about.
    spec.mount_rw = true;
    let vm = FirecodeBackend::up(&spec).expect("the VM is placed");
    assert!(held_for("s-close"), "the boot was not recorded");
    assert!(!held_for("s-other"), "a session it never booted for");

    std::thread::scope(|s| {
        let blocked = s.spawn(|| {
            // A command that ends only when the VM does, which is what a real `firecode in` in a
            // killed guest looks like from here.
            vm.guest_within(
                vm.project(),
                &format!("while [ ! -e '{}' ]; do sleep 0.05; done", downed.display()),
                std::time::Duration::from_secs(120),
            )
        });
        // The client is in the guest: nothing here answers it but the VM going away.
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!downed.exists(), "the VM was already gone");
        assert!(
            down_session("s-close"),
            "nothing here held a VM for the session a close names"
        );
        assert!(downed.exists(), "the VM was not brought down");
        let (_, code) = blocked
            .join()
            .expect("the client thread")
            .expect("the client came back when the VM went");
        assert_eq!(code, 0, "the blocked command answered {code}");
    });
    assert!(!held_for("s-close"), "the record outlived the VM");
    assert!(
        !down_session("s-close"),
        "a second close claimed a VM that was gone"
    );
    let _ = std::fs::remove_dir_all(&root);
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(o.status.success(), "git {args:?}: {o:?}");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A source repository with one commit, and the stub's `down` running `then` in the copy
/// before it records `none`: what a guest that worked and a copy-out that saw nothing look like.
fn none_after(name: &str, then: &str, guest: bool) -> (PathBuf, PathBuf, FirecodeBackend) {
    let root = tmp(name);
    let src = root.join("src");
    file(&src.join("a.txt"), 10);
    git_in(&src, &["init", "-q"]);
    git_in(&src, &["add", "-A"]);
    git_in(&src, &["commit", "-qm", "base"]);
    let guest_arm = if guest {
        "in) cd \"$5\" && sh -c \"$6\" ;;"
    } else {
        ""
    };
    let arms = format!(
        "{HAPPY}\n{guest_arm}\n\
         down) (cd \"$3\" && {then}) >/dev/null 2>&1; mkdir -p \"$ROOT/runs/r1\"; \
         echo none > \"$ROOT/runs/r1/copy_out\"; exit 0 ;;"
    );
    let bin = stub_firecode(&root.join("fc"), &arms);
    let b = FirecodeBackend::up(&spec_with(&src, &root.join("cache"), &bin)).unwrap();
    (src, root.join("cache").join("child"), b)
}

const COMMIT: &str = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false \
                      commit -q --allow-empty -m work";

/// The booted copy is removed only when it holds nothing the source lacks: a commit no ref of
/// the source contains keeps it, whatever the copy-out said.
#[test]
fn a_copy_holding_a_commit_the_source_lacks_is_kept() {
    let (_src, copy, b) = none_after("keep-commit", COMMIT, false);
    let answer = b.close().unwrap();
    assert!(
        copy.exists(),
        "a copy with an unreachable commit was removed"
    );
    assert!(answer.contains("was kept, not removed"), "{answer}");
    assert!(answer.contains("in no ref of"), "{answer}");
}

#[test]
fn a_copy_with_changes_the_source_lacks_is_kept() {
    let (_src, copy, b) = none_after("keep-dirty", "echo new > b.txt", false);
    let answer = b.close().unwrap();
    assert!(copy.exists(), "a dirty copy was removed");
    assert!(answer.contains("`b.txt`"), "{answer}");
}

#[test]
fn a_copy_with_nothing_of_its_own_is_removed() {
    let (_src, copy, b) = none_after("keep-clean", "true", false);
    let answer = b.close().unwrap();
    assert!(!copy.exists(), "an untouched copy was kept: {answer}");
    assert!(!answer.contains("was kept"), "{answer}");
}

/// The guest's commits are carried out before `down`, so a copy-out that reports `none` for a
/// guest that committed cannot lose them: the jail held the only copy of the drive it wrote.
#[test]
fn the_guests_commits_are_carried_out_when_the_copy_out_says_none() {
    let (src, copy, b) = none_after("rescue", "true", true);
    git_in(
        &copy,
        &["commit", "-q", "--allow-empty", "-m", "guest work"],
    );
    let head = git_in(&copy, &["rev-parse", "HEAD"]);
    let answer = b.close().unwrap();
    assert!(answer.contains("which is wrong"), "{answer}");
    let bundle = copy.with_file_name("child.rescue.bundle");
    assert!(answer.contains(&bundle.display().to_string()), "{answer}");
    git_in(
        &src,
        &[
            "fetch",
            "-q",
            &bundle.display().to_string(),
            "refs/letibot/rescue:refs/heads/rescued",
        ],
    );
    assert_eq!(git_in(&src, &["rev-parse", "rescued"]), head);
}
