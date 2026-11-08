use super::*;

/// The step's own output - a row's output is the step's output and then the memory evidence
/// line, so the step's answer is the first line of it.
fn step_said(run: &GateRun) -> String {
    run.steps[0]
        .output
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The tail of a running step GROWS between two reads - the measurement behind *is it
/// stuck?*: a step that is writing is not stuck, and a pane can only say so if the daemon
/// reads the file the step is writing while it writes it.
#[test]
fn a_running_steps_tail_grows_between_two_reads() {
    let dir = std::env::temp_dir().join(format!("letibot-gate-tail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the fixture's directory");
    let wt = dir.join("landing");
    std::fs::create_dir_all(&wt).expect("the landing tree");
    let repo = dir.display().to_string();
    watch(&wt, &repo);
    // A trailing sleep, so the last line is observable: a poll that has to land between `three`
    // being written and the step exiting misses under a loaded suite.
    let steps = vec!["echo one; sleep 0.6; echo two; sleep 0.6; echo three; sleep 1".to_string()];
    std::thread::scope(|s| {
        let run = s.spawn(|| {
            let stop = AtomicBool::new(false);
            let alive = || true;
            let ctl = GateControl {
                deadline: Instant::now() + Duration::from_secs(60),
                budget: Duration::from_secs(60),
                stop: &stop,
                alive: &alive,
            };
            run_with_memory(&wt, &steps, &ctl, Ask::Default)
        });
        let wait_for = |word: &str| -> RunningGate {
            let until = Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(g) = running(&repo)
                    && g.tail.contains(word)
                {
                    return g;
                }
                assert!(Instant::now() < until, "the step never wrote {word:?}");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let first = wait_for("one");
        assert_eq!(
            (first.step, first.of),
            (1, 1),
            "one step, and it is the first"
        );
        assert_eq!(
            first.command,
            "echo one; sleep 0.6; echo two; sleep 0.6; echo three; sleep 1"
        );
        let second = wait_for("three");
        assert!(
            second.log_bytes > first.log_bytes,
            "the tail grew: {} then {} bytes",
            first.log_bytes,
            second.log_bytes
        );
        assert!(
            second.tail.contains("two") && !first.tail.contains("three"),
            "each read is the file as it was: {:?} then {:?}",
            first.tail,
            second.tail
        );
        assert_eq!(
            run.join().expect("the gate's thread").verdict,
            GateVerdict::Passed
        );
    });
    assert!(
        running(&repo).is_none(),
        "a step that has ended leaves nothing to read"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The tail is bounded, and the bound is the READ - a step that has written far more than
/// [`TAIL_BYTES`] costs one 4 KiB read and keeps its last [`TAIL_LINES`] lines. A pane that read
/// the whole file would be a `cargo test` log in a head's memory.
#[test]
fn the_tail_of_a_long_log_is_its_end_and_only_its_end() {
    let dir = std::env::temp_dir().join(format!("letibot-gate-tailcap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the directory");
    let log = dir.join("step.log");
    let mut text = String::new();
    for i in 0..5_000 {
        text.push_str(&format!("line {i} of a build\n"));
    }
    std::fs::write(&log, &text).expect("the log");
    let (tail, bytes) = tail_of(&log);
    assert_eq!(bytes, text.len() as u64, "the size is the file's own");
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(
        lines.len(),
        TAIL_LINES + 1,
        "the marker, then the last {TAIL_LINES} lines"
    );
    assert!(
        lines[0].starts_with("… [the step has written"),
        "{:?}",
        lines[0]
    );
    assert_eq!(lines[TAIL_LINES], "line 4999 of a build");
    assert!(
        !tail.contains("line 0 of a build"),
        "the beginning is not read at all"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every step's row ends with what its memory did. A test binary is not started in a
/// delegated scope of its own, so this takes the old-style path: the row says *uncapped* and why
/// rather than saying nothing (`tests/gate_memory.rs` covers the other half).
#[test]
fn a_steps_row_says_what_its_memory_did_and_uncapped_when_there_is_no_tree() {
    let wt = std::env::temp_dir().join(format!("letibot-gate-unit-{}", std::process::id()));
    std::fs::create_dir_all(&wt).unwrap();
    let stop = AtomicBool::new(false);
    let alive = || true;
    let ctl = GateControl {
        deadline: Instant::now() + Duration::from_secs(60),
        budget: Duration::from_secs(60),
        stop: &stop,
        alive: &alive,
    };
    let run = run_with_memory(&wt, &["echo hi".to_string()], &ctl, Ask::Default);
    assert_eq!(run.verdict, GateVerdict::Passed, "{run:#?}");
    let out = &run.steps[0].output;
    assert!(
        out.starts_with("hi\n"),
        "the step's own output first: {out}"
    );
    match letibot_tools::exec::delegate_memory() {
        Err(why) => assert!(
            out.contains("[gate memory: uncapped — the step ran in the daemon's own cgroup")
                && out.contains(why.as_str()),
            "{out}"
        ),
        Ok(_) => assert!(out.contains("[gate memory: peak"), "{out}"),
    }
    let _ = std::fs::remove_dir_all(&wt);
}

/// A step killed at the cap names the cap, the peak and the line that raises it; one that
/// left a process running says it was killed with the cgroup.
#[test]
fn the_evidence_line_tells_a_kill_at_the_cap_from_a_peak() {
    let used = |oom_kills| {
        StepMemory::Used(MemoryUse {
            cap: Some(64 << 20),
            peak: 64 << 20,
            oom_kills,
        })
    };
    let killed = used(1).evidence(0, None).unwrap();
    assert!(killed.contains("KILLED at the gate's cap — peak 64 MiB of a 64 MiB cap"));
    assert!(killed.contains("[memory] gate"));
    assert!(used(1).killed_at_cap());
    let fine = used(0).evidence(2, None).unwrap();
    assert!(fine.starts_with("[gate memory: peak 64 MiB of a 64 MiB cap — near its cap]"));
    assert!(fine.contains("2 processes the step left running"));
    // .F8: what the model reads keeps a near-cap or killed line and drops a quiet peak.
    assert_eq!(for_the_model(&fine), fine);
    let quiet = StepMemory::Used(MemoryUse {
        cap: Some(64 << 20),
        peak: 8 << 20,
        oom_kills: 0,
    })
    .evidence(0, None)
    .unwrap();
    assert!(quiet.starts_with("[gate memory: peak 8 MiB"), "{quiet}");
    assert_eq!(for_the_model(&format!("ok\n{quiet}")), "ok");
    assert_eq!(StepMemory::Unknown.evidence(0, None), None);
}

/// The count is in hand when the line is written, so the noun is spelled - `1 process`,
/// `3 processes`, never `process(es)`. Both a single one and several are checked, because a
/// placeholder hides behind the plural form.
#[test]
fn an_evidence_line_counts_in_words_not_a_placeholder() {
    let used = StepMemory::Used(MemoryUse {
        cap: Some(64 << 20),
        peak: 8 << 20,
        oom_kills: 0,
    });
    for (leftover, said) in [
        (1, "1 process the step left running"),
        (3, "3 processes the step left running"),
    ] {
        let line = used.evidence(leftover, None).expect("the evidence line");
        assert!(line.contains(said), "{line}");
        assert!(!line.contains("(s)") && !line.contains("(es)"), "{line}");
    }
    for (ran, passed, tests, said) in [
        (
            1,
            0,
            1,
            "1 target ran — 0 passed, 1 failed; 1 test passed, 1 failed",
        ),
        (
            3,
            2,
            3,
            "3 targets ran — 2 passed, 1 failed; 3 tests passed, 1 failed",
        ),
    ] {
        let counted = Failures {
            targets_ran: ran,
            targets_passed: passed,
            targets_failed: ran - passed,
            tests_passed: tests,
            tests_failed: 1,
            ..Default::default()
        }
        .words(EVIDENCE_BYTES);
        assert!(counted.contains(said), "{counted}");
        assert!(
            !counted.contains("(s)") && !counted.contains("(es)"),
            "{counted}"
        );
    }
}

/// The measured case: letibot's `main` at 79c32c1, on lab2x1.
const GIB: u64 = 1 << 30;
fn measured(oom_kills: u64) -> StepMemory {
    StepMemory::Used(MemoryUse {
        cap: Some((24.9 * GIB as f64) as u64),
        peak: (24.1 * GIB as f64) as u64,
        oom_kills,
    })
}
/// The end of that step's output: compile warnings, no test result, no `error:`.
const ENDED_IN_WARNINGS: &str = "warning: function `answer_frames` is never used\n\
        warning: `letibot-tui` (lib test) generated 17 warnings (4 duplicates)\n";

/// A cargo that died at 96.8% of its cap with its compiles still running is the gate's limit, not
/// red code - whether its leader ended by `SIGKILL` or by a code, and although the kernel counted
/// no OOM kill.
#[test]
fn the_measured_death_near_the_cap_is_killed_at_the_cap_not_red() {
    for exit in [StepExit::Signal(libc::SIGKILL), StepExit::Code(101)] {
        let cause = red_cause(ENDED_IN_WARNINGS, exit, &measured(0));
        let RedCause::KilledAtCap(how) = &cause else {
            panic!("{exit:?}: {cause:?}");
        };
        assert!(
            how.contains("near its memory cap")
                && how.contains("out of memory, not a verdict")
                && !how.contains('%')
                && !how.contains("  "),
            "{how}"
        );
    }
    let words = killed_at_cap_words(
        "LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace --no-fail-fast -- --nocapture",
        &match measured(0) {
            StepMemory::Used(u) => u.clause(),
            _ => unreachable!(),
        },
        "it died near the cap",
        128,
    );
    assert!(words.starts_with(GATE_KILLED), "{words}");
    assert!(
        words.contains("KILLED at the gate's memory cap (peak 24.1 GiB of a 24.9 GiB cap)")
            && words.contains("128 processes were still running")
            && words.contains("UNJUDGED, not red")
            // The cap is the operator's knob, and no config key is named to a model that
            // cannot raise it (R93.F9).
            && words.contains("the operator's call — tell them")
            && !words.contains("providers.toml"),
        "{words}"
    );
    let line = measured(0)
        .evidence(128, Some(StepExit::Signal(libc::SIGKILL)))
        .unwrap();
    assert_eq!(
        line,
        "[gate memory: peak 24.1 GiB of a 24.9 GiB cap — near its cap]\n[step: killed by \
             signal 9 (SIGKILL)]\n[128 processes the step left running were killed with its cgroup]"
    );
}

/// The kernel's own count is the proof, and outranks the output: a rustc it killed makes
/// cargo print `could not compile`, which is not the code failing.
#[test]
fn an_oom_kill_counted_in_the_cgroup_is_killed_at_the_cap_whatever_the_output_says() {
    let out = "error: could not compile `letibot-tui` (lib test)\n\nCaused by:\n  process \
                   didn't exit successfully: `rustc …` (signal: 9, SIGKILL: kill)\n";
    assert!(matches!(
        red_cause(out, StepExit::Code(101), &measured(3)),
        RedCause::KilledAtCap(how) if how.contains("killed 3 processes")
    ));
    // Near the cap with no count, the same output is a kill, not a verdict.
    assert!(matches!(
        red_cause(out, StepExit::Code(101), &measured(0)),
        RedCause::KilledAtCap(_)
    ));
}

/// Code that failed is red, near the cap or not; a death far from the cap with no failing code is
/// red only when the leader answered with a code - a signal is somebody else's hand, and judged
/// nothing.
#[test]
fn failing_code_is_red_and_a_signal_far_from_the_cap_is_unjudged() {
    let failed = "test result: FAILED. 1 passed; 1 failed\nerror: 1 target failed\n";
    assert_eq!(
        red_cause(failed, StepExit::Code(101), &measured(0)),
        RedCause::Code
    );
    let far = StepMemory::Used(MemoryUse {
        cap: Some(24 * GIB),
        peak: 6 * GIB,
        oom_kills: 0,
    });
    assert_eq!(
        red_cause("merge gate: FAIL\n", StepExit::Code(1), &far),
        RedCause::Code
    );
    assert_eq!(
        red_cause(ENDED_IN_WARNINGS, StepExit::Signal(libc::SIGKILL), &far),
        RedCause::KilledBySignal(libc::SIGKILL)
    );
    assert_eq!(
        red_cause(failed, StepExit::Signal(libc::SIGTERM), &far),
        RedCause::Code,
        "a failure it printed before the signal is still a failure"
    );
    // Uncapped (no delegated scope): only the signal can tell.
    let uncapped = StepMemory::Uncapped("old-style start".into());
    assert_eq!(
        red_cause(ENDED_IN_WARNINGS, StepExit::Code(101), &uncapped),
        RedCause::Code
    );
    assert_eq!(
        red_cause(ENDED_IN_WARNINGS, StepExit::Signal(9), &uncapped),
        RedCause::KilledBySignal(9)
    );
}

/// A kill somewhere in the output does not un-fail a test that failed: the workspace run the
/// gate makes with `--no-fail-fast` goes on after one target fails, so a child the box's own
/// OOM killer took must not turn a real red into a step the queue re-gates for ever.
#[test]
fn a_failing_test_beside_a_kill_is_still_red() {
    let out = "test result: FAILED. 3 passed; 1 failed\n\nerror: test failed, to rerun pass \
                   `-p letibot-tui --lib`\n\nCaused by:\n  process didn't exit successfully: \
                   `rustc --crate-name letibot_ui src/lib.rs` (signal: 9, SIGKILL: kill)\n";
    let far = StepMemory::Used(MemoryUse {
        cap: Some(24 * GIB),
        peak: 6 * GIB,
        oom_kills: 0,
    });
    assert_eq!(
        red_cause(out, StepExit::Code(101), &far),
        RedCause::Code,
        "the failing test is the verdict, and a kill beside it is the box's business"
    );
    // And with nothing failing on its own, the same kill is still an unjudged step.
    let killed = "Caused by:\n  process didn't exit successfully: `rustc` (signal: 9, SIGKILL: \
                      kill)\n";
    assert_eq!(
        red_cause(killed, StepExit::Code(101), &far),
        RedCause::KilledBySignal(9)
    );
}

/// A rustc the BOX's own OOM killer took is not the code's verdict: cargo answers with a code of
/// its own and prints the child's kill, and the cgroup's counters never see it because the kill
/// happened outside the gate's cap.
#[test]
fn a_child_the_box_killed_far_from_the_cap_is_unjudged_not_red() {
    let out = "error: could not compile `letibot-tui` (lib)\n\nCaused by:\n  process didn't \
                   exit successfully: `rustc --crate-name letibot_tui src/lib.rs` (signal: 9, \
                   SIGKILL: kill)\n\nwarning: build failed, waiting for other jobs to finish...\n";
    let far = StepMemory::Used(MemoryUse {
        cap: Some(24 * GIB),
        peak: 6 * GIB,
        oom_kills: 0,
    });
    assert_eq!(
        red_cause(out, StepExit::Code(101), &far),
        RedCause::KilledBySignal(libc::SIGKILL),
        "nothing was judged: the compiler was killed, not the code"
    );
    // And the row it writes says so, rather than *the step failed*.
    let judged = judge(
        "cargo test --offline --workspace",
        StepEnd::Red(out.into(), StepExit::Code(101)),
        far.clone(),
        0,
        Duration::from_secs(1800),
    );
    assert_eq!(judged.outcome, MergeGateOutcome::Killed);
    let Some(GateVerdict::Infra(why)) = judged.verdict else {
        panic!("a kill by the box must not be a verdict about the code");
    };
    assert!(
        why.starts_with(GATE_KILLED)
            && why.contains("killed by signal 9 (SIGKILL)")
            && why.contains("UNJUDGED, not red")
            && !why.contains("%"),
        "{why}"
    );
    // A step that judged the code and then died is still the code's red.
    let failed = "test result: FAILED. 1 passed; 1 failed\nerror: 1 target failed\n";
    assert_eq!(red_cause(failed, StepExit::Code(101), &far), RedCause::Code);
}

/// A step that refused to skip is the MACHINE's, not the branch's - the measured case from
/// `lab2x1`: `LETIBOT_REQUIRE_APPARATUS=1 cargo test --no-fail-fast -p
/// letibot-harnessd --lib`, sixteen refusals and one genuine failure.
#[test]
fn a_step_that_refused_to_skip_is_the_machines_and_not_the_branches() {
    let refusal = |test: &str, what: &str| {
        format!(
            "---- {test} stdout ----\n\nthread '{test}' (805) panicked at \
                 crates/tokencore/src/apparatus.rs:181:9:\n\
                 LETIBOT_REQUIRE_APPARATUS is set, and {what} is not on this machine.\n\n\
                 This run does not accept a skip. Either run it where the apparatus is, or unset \
                 LETIBOT_REQUIRE_APPARATUS to let it skip with a reason.\n\n"
        )
    };
    let what = "a process-lifetime tree (cgroup v2; process groups on macOS)";
    // A host gate's step is `Elsewhere`: no cgroup of this daemon's, so no cap to read - and
    // that is the memory a host gate hands `red_cause`.
    let elsewhere = || StepMemory::Elsewhere("lab2x1".into());
    let out = format!(
        "{}{}failures:\n    jobwatch::tests::a\n    supervise::tests::b\n\n\
             test result: FAILED. 782 passed; 17 failed; 1 ignored; 0 measured; 0 filtered \
             out; finished in 26.28s\n\n\
             error: test failed, to rerun pass `-p letibot-harnessd --lib`\n\
             error: 1 target failed:\n    `-p letibot-harnessd --lib`\n",
        refusal("jobwatch::tests::a", what),
        refusal("supervise::tests::b", what),
    );
    assert_eq!(
        red_cause(&out, StepExit::Code(101), &elsewhere()),
        RedCause::ApparatusAbsent(what.to_string())
    );
    // The verdict is the gate's own: unjudged, and the row is `not_run`.
    let judged = judge(
        "LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace",
        StepEnd::Red(out.clone(), StepExit::Code(101)),
        StepMemory::Elsewhere("lab2x1".into()),
        0,
        Duration::from_secs(1800),
    );
    assert_eq!(judged.outcome, MergeGateOutcome::NotRun);
    let Some(GateVerdict::Infra(why)) = judged.verdict else {
        panic!("a machine's absence must not be a verdict about the code");
    };
    assert!(
        why.starts_with(
            "the gate could not run here: a process-lifetime tree (cgroup v2; process groups \
                 on macOS) is not on `lab2x1`"
        ),
        "{why}"
    );
    assert!(why.contains("UNJUDGED, not red"), "{why}");
    assert!(!why.starts_with(GATE_KILLED), "nothing was killed: {why}");

    // One genuine failure among the refusals is still red - the count is what tells them apart
    // and a suite that also failed on its own must not be excused by the machine.
    let mixed = format!(
        "{}thread 'a_test' (99) panicked at src/x.rs:1:1:\nassertion failed: 1 == 2\n\n",
        out
    );
    assert_eq!(
        red_cause(&mixed, StepExit::Code(101), &elsewhere()),
        RedCause::Code
    );
    // And so is a compile error, whatever the refusals said.
    let broken = format!("{out}\nerror[E0308]: mismatched types\n");
    assert_eq!(
        red_cause(&broken, StepExit::Code(101), &elsewhere()),
        RedCause::Code
    );
    // A run that only MENTIONS the words (a test of the message itself) is not a refusal.
    assert_eq!(
        red_cause(
            "test result: ok. 1 passed\nassert!(!m.contains(\"is not on this machine\"));\n",
            StepExit::Code(101),
            &elsewhere()
        ),
        RedCause::Code
    );
}

/// Two apparatuses named by one step read as a list - a workspace run refuses on more than one, and
/// the row says both rather than the first.
#[test]
fn the_row_names_every_apparatus_the_step_demanded() {
    let out = "thread 'a' (1) panicked at crates/tokencore/src/apparatus.rs:181:9:\n\
                   LETIBOT_REQUIRE_APPARATUS is set, and a cgroup v2 subtree this user may write \
                   (the process-lifetime tree) is not on this machine.\n\n\
                   This run does not accept a skip. Either run it where the apparatus is, or \
                   unset LETIBOT_REQUIRE_APPARATUS to let it skip with a reason.\n\n\
                   thread 'b' (2) panicked at crates/tokencore/src/apparatus.rs:181:9:\n\
                   LETIBOT_REQUIRE_APPARATUS is set, and a vocabulary GGUF (/nope.gguf) is not on \
                   this machine.\n\n\
                   This run does not accept a skip.\n";
    let RedCause::ApparatusAbsent(what) = red_cause(
        out,
        StepExit::Code(101),
        &StepMemory::Elsewhere("lab2x1".into()),
    ) else {
        panic!("two refusals are still the machine's");
    };
    assert_eq!(
        what,
        "a cgroup v2 subtree this user may write (the process-lifetime tree)\n\
             a vocabulary GGUF (/nope.gguf)"
    );
    let why = apparatus_absent_words("cargo test --workspace", &what, Some("lab2x1"));
    assert!(
        why.starts_with("the gate could not run here: these are not on `lab2x1`: a cgroup v2"),
        "{why}"
    );
}

/// End to end on a real step: a leader killed by `SIGKILL` before it printed a failure is
/// `killed`, the verdict is the gate's own (`Infra`, unjudged), and the row records the signal -
/// the fact the 79c32c1 row did not keep.
#[test]
fn a_step_killed_by_a_signal_is_unjudged_and_its_row_says_the_signal() {
    let wt = std::env::temp_dir().join(format!("letibot-gate-sig-{}", std::process::id()));
    std::fs::create_dir_all(&wt).unwrap();
    let stop = AtomicBool::new(false);
    let alive = || true;
    let ctl = GateControl {
        deadline: Instant::now() + Duration::from_secs(60),
        budget: Duration::from_secs(60),
        stop: &stop,
        alive: &alive,
    };
    let steps = vec![
        "echo compiling; kill -9 $$".to_string(),
        "echo next".to_string(),
    ];
    let run = run_with_memory(&wt, &steps, &ctl, Ask::Uncapped);
    let GateVerdict::Infra(why) = &run.verdict else {
        panic!("{run:#?}");
    };
    assert!(why.starts_with(GATE_KILLED), "{why}");
    assert_eq!(run.steps[0].outcome, MergeGateOutcome::Killed, "{run:#?}");
    assert!(
        run.steps[0]
            .output
            .contains("[step: killed by signal 9 (SIGKILL)]"),
        "{}",
        run.steps[0].output
    );
    assert_eq!(run.steps[1].outcome, MergeGateOutcome::NotRun);
    // And a step that fails by its own code is still red.
    let run = run_with_memory(&wt, &["echo nope; exit 3".to_string()], &ctl, Ask::Uncapped);
    assert!(
        matches!(&run.verdict, GateVerdict::Failed(w) if w.contains("[step: exit code 3]")),
        "{run:#?}"
    );
    let _ = std::fs::remove_dir_all(&wt);
}

/// A tmpfs or a disk: the one distinction this file's scratch decision turns on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileSystem {
    Disk,
    Tmpfs,
}

/// What a directory is on, where the platform says so. `None` for a path that is not there and for
/// a platform with no answer, so a caller asks rather than assumes.
fn filesystem_of(path: &Path) -> Option<FileSystem> {
    if !path.exists() {
        return None;
    }
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path this test owns and a buffer it owns, and `statfs` writes
    // only into that buffer.
    if unsafe { libc::statfs(c.as_ptr(), &mut buf) } != 0 {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        Some(if buf.f_type == libc::TMPFS_MAGIC {
            FileSystem::Tmpfs
        } else {
            FileSystem::Disk
        })
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `f_fstypename` is a NUL-terminated array inside the buffer `statfs` filled.
        let name = unsafe { std::ffi::CStr::from_ptr(buf.f_fstypename.as_ptr()) };
        Some(if name.to_bytes() == b"tmpfs" {
            FileSystem::Tmpfs
        } else {
            FileSystem::Disk
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = &buf;
        None
    }
}

/// One step, and what it was told about its scratch: the production path, from the tree's directory
/// to the removal at the end of the run.
fn run_a_scratch_step(wt: &Path) {
    let stop = AtomicBool::new(false);
    let alive = || true;
    let ctl = GateControl {
        deadline: Instant::now() + Duration::from_secs(60),
        budget: Duration::from_secs(60),
        stop: &stop,
        alive: &alive,
    };
    let run = run_with_memory(
        wt,
        &[
            r#"printf '%s %s' "$TMPDIR" "$( [ -d "$TMPDIR" ] && echo dir || echo MISSING )""#
                .to_string(),
        ],
        &ctl,
        Ask::Default,
    );
    assert_eq!(run.verdict, GateVerdict::Passed, "{run:#?}");
    // What the step was GIVEN, not what this test predicts: the run's number is the gate's, so the
    // path the step printed is the path to judge.
    let said = step_said(&run);
    let (tmp, is_dir) = said.split_once(' ').unwrap_or((said.as_str(), ""));
    let printed = Path::new(tmp);
    assert_eq!(printed.parent(), Some(Path::new(GATE_TMP_ROOT)), "{said}");
    assert_eq!(
        is_dir, "dir",
        "the gate makes it before the step runs: {said}"
    );
    assert!(
        !printed.starts_with(wt),
        "NOT inside the landing tree: {said}"
    );
    assert!(
        printed.as_os_str().len() + 1 + 60 <= 107,
        "the run's own directory is short too: {said}"
    );
    assert!(
        !printed.exists(),
        "the gate removes it when the run ends: {}",
        printed.display()
    );
}

/// A step's scratch is a short directory on a real disk, outside the tree, and it is gone
/// when the run ends ([`GATE_TMPDIR`], [`GATE_TMP_ROOT`]): a `TMPDIR` inside the landing
/// tree is too long for the AF_UNIX sockets this workspace's own tests bind.
#[test]
fn a_steps_scratch_is_short_on_a_real_disk_and_gone_when_the_run_ends() {
    // A fixture name, so this test cannot collide with the live landing tree: a test that reuses the
    // real name passes or fails by accident of the box it runs on.
    let tree = Path::new("/landing/letibot-fixture-abcdef123456");

    // The decision, as a decision: no process needed, the root it was given, the tree's name in the
    // directory, and a number that makes two runs two directories.
    let root = Path::new(GATE_TMP_ROOT);
    let a = gate_tmp_dir_in(root, tree);
    let b = gate_tmp_dir_in(root, tree);
    assert_eq!(a.parent(), Some(root), "{}", a.display());
    assert_ne!(a, b, "one directory per run, not per step");
    assert!(
        a.to_string_lossy().contains("letibot-fixture"),
        "a leftover names the tree it belongs to: {}",
        a.display()
    );
    // Short: the longest socket name this workspace binds is 60 bytes, and an AF_UNIX path must be
    // under 108 with its NUL, so a root may spend 46 and no more. A fact about the constant's
    // SPELLING, which is why it holds on every box.
    assert!(
        a.as_os_str().len() + 1 + 60 <= 107,
        "{} bytes leaves no room for the 60-byte socket names this workspace binds: {}",
        a.as_os_str().len(),
        a.display()
    );
    // The scratch the production path makes, and what the step is told: a directory, outside the
    // tree, short, and gone when the run ends. Every one of those is a fact about the code.
    let wt = std::env::temp_dir().join(format!("letibot-gate-scratch-{}", std::process::id()));
    std::fs::create_dir_all(&wt).unwrap();
    run_a_scratch_step(&wt);

    // Which filesystem the gate's root is on, said rather than asserted: on a box whose `/var/tmp`
    // is a tmpfs this is a real cost, and it is the box's to fix. Asked after the run, because the
    // root is a directory the first run makes.
    if filesystem_of(root) == Some(FileSystem::Tmpfs) {
        eprintln!(
            "NOTE: {GATE_TMP_ROOT} is on a tmpfs here, so a step's scratch is shmem charged to \
                 its own cgroup, so GATE_TMP_ROOT should name a real disk on this box"
        );
    }

    // The disk half, on a root this test chooses. A candidate has to BE a directory that is already
    // there, or the scratch could not be made under it.
    let candidates = [
        PathBuf::from(GATE_TMP_ROOT),
        std::env::temp_dir(),
        PathBuf::from("/var/tmp"),
        PathBuf::from("/tmp"),
    ];
    let chosen = candidates
        .into_iter()
        .find(|p| p.is_dir() && filesystem_of(p) == Some(FileSystem::Disk));
    let Some(root) = chosen else {
        let _ = std::fs::remove_dir_all(&wt);
        let _ = letibot_tokencore::apparatus::present(
            "a directory on a filesystem that is not a tmpfs, to put a step's scratch on",
            false,
        );
        return;
    };
    let made = GateScratch::for_run_in(&root, tree).expect("a scratch under a root that exists");
    let scratch = made.path().to_path_buf();
    assert!(scratch.is_dir(), "{} was made", scratch.display());
    assert_eq!(
        scratch.parent(),
        Some(root.as_path()),
        "{}",
        scratch.display()
    );
    assert_eq!(
        filesystem_of(&scratch),
        Some(FileSystem::Disk),
        "{} is on the root this test chose, which is not a tmpfs",
        scratch.display()
    );
    assert!(
        scratch.as_os_str().len() + 1 + 60 <= 107,
        "a scratch on a real root is short too: {}",
        scratch.display()
    );
    drop(made);
    assert!(
        !scratch.exists(),
        "the gate removes it when the run ends: {}",
        scratch.display()
    );
    let _ = std::fs::remove_dir_all(&wt);
}

/// The build's parallelism comes from the cap, not from the core count - one job per
/// [`GATE_JOB_MEMORY`], at least one, never more than the cores: rustc jobs started at the core
/// count against a cap that pays for fewer is what the kernel kills.
#[test]
fn the_builds_jobs_come_from_the_cap_and_never_exceed_the_cores() {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    // Both clamps: one job per 2 GiB, never zero, never more than the cores.
    assert_eq!(build_jobs(Some(25 << 30)), Some(12.min(cores)));
    assert_eq!(build_jobs(Some(50 << 30)), Some(25.min(cores)));
    assert_eq!(build_jobs(Some(4 << 30)), Some(2.min(cores)));
    assert_eq!(
        build_jobs(Some(1 << 40)),
        Some(cores),
        "a cap no box has is the cores, not 512"
    );
    assert_eq!(build_jobs(Some(64 << 20)), Some(1), "never zero");
    assert_eq!(build_jobs(Some(1)), Some(1));
    assert_eq!(
        build_jobs(None),
        None,
        "uncapped: no number, cargo's default"
    );
}

/// And the step is actually told - the value reaches the step's environment, overriding
/// whatever the box had: one job under a 64 MiB cap, the cores under a cap no box has, and
/// under `uncapped` nothing at all (so a cargo step falls back to its own default).
#[test]
fn a_step_is_told_how_many_jobs_its_cap_pays_for() {
    let wt = std::env::temp_dir().join(format!("letibot-gate-jobs-{}", std::process::id()));
    std::fs::create_dir_all(&wt).unwrap();
    let stop = AtomicBool::new(false);
    let alive = || true;
    let ctl = GateControl {
        deadline: Instant::now() + Duration::from_secs(60),
        budget: Duration::from_secs(60),
        stop: &stop,
        alive: &alive,
    };
    let said = |ask: Ask| {
        let run = run_with_memory(
            &wt,
            &["printf 'jobs=<%s>' \"$CARGO_BUILD_JOBS\"".to_string()],
            &ctl,
            ask,
        );
        assert_eq!(run.verdict, GateVerdict::Passed, "{run:#?}");
        step_said(&run)
    };
    assert_eq!(said(Ask::Amount(Amount::Bytes(64 << 20))), "jobs=<1>");
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    assert_eq!(
        said(Ask::Amount(Amount::Bytes(1 << 40))),
        format!("jobs=<{cores}>")
    );
    assert_eq!(
        said(Ask::Uncapped),
        format!(
            "jobs=<{}>",
            std::env::var(GATE_BUILD_JOBS).unwrap_or_default()
        ),
        "uncapped is no number: the gate sets nothing, so the step sees whatever its caller \
             had — an empty answer when the box had nothing"
    );
    let _ = std::fs::remove_dir_all(&wt);
}

/// A failing workspace run, as `cargo test --workspace --no-fail-fast` writes one: the failing
/// target's block, then sixty other targets each reporting `running 0 tests`. The end of the stream
/// is the other targets and cargo's one-line summary, which is all the row kept before this.
fn a_failing_workspace_run() -> String {
    let mut out = String::new();
    for i in 0..300 {
        out.push_str(&format!("test scene::tests::case_{i:03} ... ok\n"));
    }
    out.push_str(
        "     Running unittests src/lib.rs (target/debug/deps/letibot_harnessd-1234)\n\
             \n\
             running 301 tests\n\
             test worker::a_test ... FAILED\n\
             \n\
             failures:\n\
             \n\
             ---- worker::a_test stdout ----\n\
             thread 'worker::a_test' panicked at crates/harnessd/src/worker.rs:12:5:\n\
             the worker never started\n\
             \n\
             failures:\n    worker::a_test\n\
             \n\
             test result: FAILED. 300 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; \
             finished in 14.59s\n\
             \n\
             error: test failed, to rerun pass `-p letibot-harnessd --lib`\n",
    );
    for j in 0..60 {
        out.push_str(&format!(
            "     Running unittests src/bin/tool_{j}.rs (target/debug/deps/tool_{j}-abcdef)\n\
                 \n\
                 running 0 tests\n\
                 \n\
                 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; \
                 finished in 0.00s\n\
                 \n"
        ));
    }
    out.push_str("error: 1 target failed:\n    `-p letibot-harnessd --lib`\n");
    out
}

/// The failing test's name and its panic survive, though they are hundreds of lines above
/// the end - and the counts say the run happened, so `running 0 tests` blocks cannot be
/// mistaken for a step that did nothing.
#[test]
fn a_failure_hundreds_of_lines_before_the_end_is_what_the_excerpt_keeps() {
    let text = a_failing_workspace_run();
    let at = text.find("worker::a_test").unwrap();
    assert!(
        text.len() - at > EVIDENCE_BYTES,
        "the failure must sit outside the last {EVIDENCE_BYTES} bytes, which is what the old \
             excerpt kept — it is {} bytes from the end",
        text.len() - at
    );
    assert!(
        !tail(&text, EVIDENCE_BYTES).contains("worker::a_test"),
        "the old excerpt could not name the test, and this test is the regression"
    );

    let ex = excerpt(&text, EVIDENCE_BYTES);
    for want in [
        // The target, by cargo's own name for it.
        "`-p letibot-harnessd --lib`",
        "error: 1 target failed:",
        // The first failing test and why.
        "first failing test `worker::a_test`",
        "panicked at crates/harnessd/src/worker.rs:12:5",
        "the worker never started",
        // The counts: 1 + 60 targets, 60 of them green, and 300 + 1 tests.
        "61 targets ran — 60 passed, 1 failed; 300 tests passed, 1 failed",
    ] {
        assert!(ex.contains(want), "{want:?} is not in the excerpt:\n{ex}");
    }
    // The tail is present too - it fits beside the failure - and says what it left out.
    assert!(
        ex.contains("bytes of the step's output were left out here; its end follows"),
        "the cut has to be admitted:\n{ex}"
    );
    assert!(
        ex.trim_end().ends_with(
            "[gate excerpt: what failed is at the TOP of this row: `worker::a_test` in \
                 `-p letibot-harnessd --lib`]"
        ),
        "the row ends by naming what failed, for a reader that keeps only the end:\n{ex}"
    );
    // The model is handed the last `TAIL_LINES` lines of exactly this string
    // (`notices::failing_step`), so the failure has to survive THAT cut too.
    let keep = super::super::notices::TAIL_LINES;
    let lines: Vec<&str> = ex.lines().collect();
    let from = lines.len().saturating_sub(keep);
    let for_the_model = lines[from..].join("\n");
    assert!(
        for_the_model.contains("worker::a_test"),
        "the notice's own cut still loses the test:\n{for_the_model}"
    );
    assert!(
        ex.len() <= EVIDENCE_BYTES,
        "the excerpt is {} bytes, over a {EVIDENCE_BYTES}-byte cap:\n{ex}",
        ex.len()
    );
}

/// When the failure and the end cannot both fit, the failure stays and the end is left out in as
/// many words - never a silent truncation back to the tail.
#[test]
fn a_failure_too_big_for_the_cap_keeps_the_failure_and_says_the_end_is_gone() {
    let text = a_failing_workspace_run();
    let ex = excerpt(&text, 600);
    assert!(ex.contains("first failing test `worker::a_test`"), "{ex}");
    assert!(
        ex.contains("the end of the step's output is left out"),
        "{ex}"
    );
    assert!(
        ex.contains("`worker::a_test` in `-p letibot-harnessd --lib`]"),
        "even with the end gone, the row says what failed:\n{ex}"
    );
    assert!(
        !ex.contains("bytes of the step's output were left out here"),
        "{ex}"
    );
}

/// A `fork` that ran out of descriptors is named wherever it was printed - one line of a step's
/// output, 9 KiB above the end here, behind a daemon holding 253 of 256 descriptors.
#[test]
fn a_machine_that_ran_out_of_descriptors_is_named_wherever_it_was_printed() {
    // Cargo could not even start the test binary: no `failures:` block, no failing test.
    let mut text = String::from(
        "     Running unittests src/lib.rs (target/debug/deps/letibot_harnessd-1234)\n\
             error: failed to run `target/debug/deps/letibot_harnessd-1234`: Too many open files \
             (os error 24)\n",
    );
    for j in 0..80 {
        text.push_str(&format!(
            "     Running unittests src/bin/tool_{j}.rs (target/debug/deps/tool_{j}-abcdef)\n\
                 \n\
                 running 0 tests\n\
                 \n\
                 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; \
                 finished in 0.00s\n\
                 \n"
        ));
    }
    text.push_str("error: 1 target failed:\n    `-p letibot-harnessd --lib`\n");
    assert!(
        text.len() - text.find("Too many open files").unwrap() > EVIDENCE_BYTES,
        "the sentence has to be outside the last {EVIDENCE_BYTES} bytes"
    );
    let ex = excerpt(&text, EVIDENCE_BYTES);
    assert!(
        ex.contains("the step's whole output also says: error: failed to run"),
        "{ex}"
    );
    assert!(ex.contains("Too many open files (os error 24)"), "{ex}");
    assert!(ex.contains("`-p letibot-harnessd --lib`"), "{ex}");

    // And the same words inside a failing test's panic are kept as the panic.
    let mut text = a_failing_workspace_run().replace(
        "the worker never started",
        "clone the socket: Os { code: 24, kind: TooManyOpenFiles, message: \"Too many open \
             files\" }",
    );
    text.push_str("(a tail)\n");
    let ex = excerpt(&text, EVIDENCE_BYTES);
    assert!(ex.contains("TooManyOpenFiles"), "{ex}");
}

/// End to end on a real step: the failure is hundreds of lines above the end and the row still
/// names the test, the target and the counts.
#[test]
fn a_failed_step_keeps_the_failure_that_was_hundreds_of_lines_above_its_end() {
    let wt = std::env::temp_dir().join(format!("letibot-gate-excerpt-{}", std::process::id()));
    std::fs::create_dir_all(&wt).unwrap();
    let stop = AtomicBool::new(false);
    let alive = || true;
    let ctl = GateControl {
        deadline: Instant::now() + Duration::from_secs(120),
        budget: Duration::from_secs(120),
        stop: &stop,
        alive: &alive,
    };
    let step = r#"
i=0
while [ $i -lt 300 ]; do echo "test scene::tests::case_$i ... ok"; i=$((i+1)); done
echo "     Running unittests src/lib.rs (target/debug/deps/letibot_harnessd-1234)"
echo
echo "running 301 tests"
echo "test worker::a_test ... FAILED"
echo
echo "failures:"
echo
echo "---- worker::a_test stdout ----"
echo "thread 'worker::a_test' panicked at crates/harnessd/src/worker.rs:12:5:"
echo "the worker never started"
echo
echo "failures:"
echo "    worker::a_test"
echo
echo "test result: FAILED. 300 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 14.59s"
echo
echo 'error: test failed, to rerun pass `-p letibot-harnessd --lib`'
j=0
while [ $j -lt 60 ]; do
  echo "     Running unittests src/bin/tool_$j.rs (target/debug/deps/tool_$j-abcdef)"
  echo
  echo "running 0 tests"
  echo
  echo "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
  echo
  j=$((j+1))
done
echo "error: 1 target failed:"
echo '    `-p letibot-harnessd --lib`'
exit 101
"#
    .to_string();
    let run = run_with_memory(&wt, &[step], &ctl, Ask::Uncapped);
    let GateVerdict::Failed(why) = &run.verdict else {
        panic!("{run:#?}");
    };
    let out = &run.steps[0].output;
    for want in [
        "first failing test `worker::a_test`",
        "`-p letibot-harnessd --lib`",
        "the worker never started",
        "61 targets ran — 60 passed, 1 failed; 300 tests passed, 1 failed",
    ] {
        assert!(out.contains(want), "{want:?} is not on the row:\n{out}");
        assert!(why.contains(want), "{want:?} is not in the verdict:\n{why}");
    }
    assert!(
        out.len() <= EVIDENCE_BYTES,
        "the row is {} bytes against a {EVIDENCE_BYTES}-byte cap:\n{out}",
        out.len()
    );
    let _ = std::fs::remove_dir_all(&wt);
}

/// The row's cap covers the note the gate writes after the excerpt - the red `main` of
/// 2026-10-30 in one fixture, and it is the test that would have caught it.
#[test]
fn a_row_whose_end_is_as_big_as_the_cap_allows_still_fits_beside_the_gate_note() {
    let why = format!(
        "this daemon shares its cgroup \
             `/sys/fs/cgroup/user.slice/user-1000.slice/session-{}.scope` with 4 other \
             processes — it was not started in a scope of its own (the `letibot` launcher does \
             that with `systemd-run --user --scope`), so tool calls run UNCAPPED",
        9_999_999
    );
    let mut text = a_failing_workspace_run();
    text.push_str(&"x".repeat(EVIDENCE_BYTES));
    text.push('\n');

    let log = std::env::temp_dir().join(format!("letibot-gate-cap-{}.log", std::process::id()));
    std::fs::write(&log, &text).unwrap();
    let row = judge(
        "cargo test --workspace --no-fail-fast",
        StepEnd::Red(read_excerpt(&log), StepExit::Code(101)),
        StepMemory::Uncapped(why),
        0,
        Duration::from_secs(60),
    )
    .output;
    let _ = std::fs::remove_file(&log);

    for want in [
        "first failing test `worker::a_test`",
        "panicked at crates/harnessd/src/worker.rs:12:5",
        "[gate memory: uncapped",
        "[step: exit code 101]",
    ] {
        assert!(row.contains(want), "{want:?} is not on the row:\n{row}");
    }
    assert!(
        row.len() <= EVIDENCE_BYTES,
        "the row is {} bytes against a {EVIDENCE_BYTES}-byte cap — the note was spent \
             outside it:\n{row}",
        row.len()
    );
}

/// A panic in a test that PASSED is not evidence about the one that failed - the reviewer's
/// case 2026-10-30: the row led with `crates/harnessd/src/httphead.rs:405:13: the turn blew
/// up`, which is a lib unit test that panics on purpose and passes.
#[test]
fn a_panic_in_a_test_that_passed_is_never_quoted_for_the_one_that_failed() {
    let mut text = String::from(
        "     Running unittests src/lib.rs (target/debug/deps/letibot_harnessd-1234)\n\
             \n\
             running 302 tests\n\
             \n\
             thread '<unnamed>' (472254176) panicked at \
             crates/harnessd/src/httphead.rs:405:13:\n\
             the turn blew up\n\
             note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\
             test httphead::tests::a_panicking_handler_returns_its_permit ... ok\n",
    );
    for i in 0..300 {
        text.push_str(&format!("test scene::tests::case_{i:03} ... ok\n"));
    }
    text.push_str(
        "\n\
             test result: ok. 302 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in 0.41s\n\
             \n\
             Running tests/httphead.rs (target/debug/deps/httphead-abcd)\n\
             \n\
             running 2 tests\n\
             \n\
             thread 'a_turn_that_blew_up_is_reported' (472254183) panicked at \
             crates/harnessd/tests/httphead.rs:412:9:\n\
             assertion `left == right` failed: the status\n\
               left: 500\n\
              right: 200\n\
             test a_request_is_answered ... ok\n\
             test a_turn_that_blew_up_is_reported ... FAILED\n\
             \n\
             failures:\n\
             \n\
             failures:\n    a_turn_that_blew_up_is_reported\n\
             \n\
             test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in 0.31s\n\
             \n\
             error: test failed, to rerun pass `-p letibot-harnessd --test httphead`\n\
             error: 1 target failed:\n    `-p letibot-harnessd --test httphead`\n",
    );
    assert!(
        text.len() > EVIDENCE_BYTES,
        "the excerpt only runs when the stream is over the cap — it is {} bytes",
        text.len()
    );

    let ex = excerpt(&text, EVIDENCE_BYTES);
    // The failing target's own test, by name, with its own panic.
    assert!(
        ex.contains("first failing test `a_turn_that_blew_up_is_reported`"),
        "{ex}"
    );
    assert!(
        ex.contains("panicked at crates/harnessd/tests/httphead.rs:412:9"),
        "{ex}"
    );
    assert!(ex.contains("left: 500"), "{ex}");
    // And nothing of the test that passed, whose panic is on the same stream.
    assert!(
        !ex.contains("the turn blew up") && !ex.contains("httphead.rs:405"),
        "a panic from a test that passed is not evidence about this failure:\n{ex}"
    );
    assert!(
        !ex.contains("a_panicking_handler_returns_its_permit"),
        "the row must not name a test that passed:\n{ex}"
    );
    // The headline names the target's own test and cargo's name for the target.
    assert!(
        ex.trim_end().ends_with(
            "[gate excerpt: what failed is at the TOP of this row: \
                 `a_turn_that_blew_up_is_reported` in `-p letibot-harnessd --test httphead`]"
        ),
        "{ex}"
    );
    assert!(
        ex.contains("2 targets ran — 1 passed, 1 failed; 303 tests passed, 1 failed"),
        "{ex}"
    );
    assert!(
        ex.len() <= EVIDENCE_BYTES,
        "the excerpt is {} bytes, over a {EVIDENCE_BYTES}-byte cap:\n{ex}",
        ex.len()
    );
}

/// The arithmetic, on a real cargo stream from this box.
#[test]
fn a_real_workspace_stream_is_cut_into_targets_and_only_the_failing_one_is_quoted() {
    let text = r#"   Compiling probe-a v0.1.0 (/w/a)
   Compiling probe-b v0.1.0 (/w/b)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.17s
     Running unittests src/lib.rs (target/debug/deps/probe_a-d72f9922acc0c4ac)

running 1 test

thread '<unnamed>' (472254176) panicked at a/src/lib.rs:7:39:
the turn blew up
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test tests::a_panicking_handler_returns_its_permit ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/httphead.rs (target/debug/deps/httphead-e1c8c3a5aea31140)

running 3 tests

thread 'a_turn_that_blew_up_is_reported' (472254183) panicked at a/tests/httphead.rs:5:40:
assertion `left == right` failed: the status
  left: 500
 right: 200
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test a_request_is_answered ... ok
test a_refusal_is_retried - should panic ... FAILED
test a_turn_that_blew_up_is_reported ... FAILED

failures:

---- a_refusal_is_retried stdout ----
note: test did not panic as expected at a/tests/httphead.rs:9:4

failures:
    a_refusal_is_retried
    a_turn_that_blew_up_is_reported

test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p probe-a --test httphead`
     Running unittests src/lib.rs (target/debug/deps/probe_b-5c38161ba40627db)

running 1 test
test fine ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests probe_a

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests probe_b

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: 1 target failed:
    `-p probe-a --test httphead`
"#;
    let f = failures(text);
    // Five targets ran (two libs, an integration test and two doc-test targets), and cargo named
    // the one that failed - no other segment produced a failing target.
    assert_eq!(f.targets_ran, 5, "{f:#?}");
    assert_eq!((f.targets_passed, f.targets_failed), (4, 1), "{f:#?}");
    assert_eq!(
        f.targets.len(),
        1,
        "one target failed, and it is one: {f:#?}"
    );
    let t = &f.targets[0];
    assert_eq!(t.target, "-p probe-a --test httphead");
    // The first test the target reported FAILED, and both of its failures counted once each.
    assert_eq!(
        t.test.as_deref(),
        Some("a_refusal_is_retried - should panic")
    );
    assert_eq!(t.failed, 2, "two tests failed in it: {f:#?}");
    // No panic pairs with that test, so the row quotes libtest's own line rather than the panic of
    // `a_turn_that_blew_up_is_reported` - which is a test that also failed, but is not the one the
    // row names.
    assert_eq!(
        t.panic,
        vec!["test a_refusal_is_retried - should panic ... FAILED"]
    );
    assert!(
        !t.panic.iter().any(|l| l.contains("the turn blew up")),
        "{f:#?}"
    );
    assert_eq!(
        f.pointer(),
        "[gate excerpt: what failed is at the TOP of this row: `a_refusal_is_retried - should \
             panic` in `-p probe-a --test httphead`]"
    );
}

/// And a panic in a passing test of the SAME target is not quoted either - the pairing rule's own
/// case, and the reason a segment is not enough: one target can print a panic from a test that
/// passed above the `test … FAILED` line of the test that did fail. The NAME tells them apart.
#[test]
fn a_panic_in_a_passing_test_of_the_same_target_is_not_quoted() {
    let mut text = String::from(
        "     Running unittests src/lib.rs (target/debug/deps/letibot_harnessd-1234)\n\
             \n\
             running 2 tests\n\
             \n\
             thread 'httphead::tests::a_panicking_handler_returns_its_permit' (472254176) \
             panicked at crates/harnessd/src/httphead.rs:405:13:\n\
             the turn blew up\n\
             test httphead::tests::a_panicking_handler_returns_its_permit ... ok\n\
             \n\
             thread 'a_turn_that_blew_up_is_reported' (472254183) panicked at \
             crates/harnessd/tests/httphead.rs:412:9:\n\
             assertion `left == right` failed: the status\n\
             test a_turn_that_blew_up_is_reported ... FAILED\n\
             \n\
             failures:\n\
             \n\
             failures:\n    a_turn_that_blew_up_is_reported\n\
             \n\
             test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in 0.31s\n\
             \n\
             error: test failed, to rerun pass `-p letibot-harnessd --lib`\n\
             error: 1 target failed:\n    `-p letibot-harnessd --lib`\n",
    );
    text.push_str(&"warning: unused variable: `x`\n".repeat(300));
    let ex = excerpt(&text, EVIDENCE_BYTES);
    assert!(
        ex.contains("first failing test `a_turn_that_blew_up_is_reported`"),
        "{ex}"
    );
    assert!(
        ex.contains("panicked at crates/harnessd/tests/httphead.rs:412:9"),
        "{ex}"
    );
    assert!(
        !ex.contains("the turn blew up") && !ex.contains("httphead.rs:405"),
        "the other test's panic is not this failure's reason:\n{ex}"
    );
}

/// A target whose failure has no `panicked at` still names its test - a `#[should_panic]` that did
/// not panic, and a binary killed by a signal before libtest could report anything. The first quotes
/// libtest's own `test … FAILED` line; the second has no test to name and says the target.
#[test]
fn a_failure_with_no_panic_still_names_the_test_that_failed() {
    let mut text = String::from(
        "     Running unittests src/lib.rs (target/debug/deps/letibot_harnessd-1234)\n\
             \n\
             running 2 tests\n\
             test scene::tests::case_000 ... ok\n\
             test retry::tests::a_refusal_is_retried - should panic ... FAILED\n\
             \n\
             failures:\n\
             \n\
             ---- retry::tests::a_refusal_is_retried stdout ----\n\
             note: test did not panic as expected\n\
             \n\
             failures:\n    retry::tests::a_refusal_is_retried - should panic\n\
             \n\
             test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in 0.05s\n\
             \n\
             error: test failed, to rerun pass `-p letibot-harnessd --lib`\n",
    );
    for i in 0..300 {
        text.push_str(&format!(
            "     Running unittests src/bin/tool_{i}.rs (target/debug/deps/tool_{i}-abcdef)\n\
                 \n\
                 running 0 tests\n\
                 \n\
                 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; \
                 finished in 0.00s\n\
                 \n"
        ));
    }
    text.push_str("error: 1 target failed:\n    `-p letibot-harnessd --lib`\n");
    assert!(text.len() > EVIDENCE_BYTES);

    let ex = excerpt(&text, EVIDENCE_BYTES);
    assert!(
        ex.contains("first failing test `retry::tests::a_refusal_is_retried - should panic`"),
        "the name is what a person needs, and libtest printed it:\n{ex}"
    );
    // The quoted evidence is libtest's own FAILED line, not a panic from somewhere else.
    assert!(
        ex.contains("test retry::tests::a_refusal_is_retried - should panic ... FAILED"),
        "{ex}"
    );
    assert!(
        !ex.contains("2 tests failed in it") && !ex.contains("1 tests failed in it"),
        "one test failed, and the block header is the same test under its other name:\n{ex}"
    );
    assert!(
        ex.trim_end().ends_with(
            "[gate excerpt: what failed is at the TOP of this row: \
                 `retry::tests::a_refusal_is_retried - should panic` in \
                 `-p letibot-harnessd --lib`]"
        ),
        "{ex}"
    );

    // And an abort - the binary died and libtest never wrote a word, so cargo's own line is the
    // whole of what is known. It still names the target, and claims no test.
    let aborted = format!(
        "{}     Running unittests src/lib.rs (target/debug/deps/letibot_harnessd-1234)\n\
             \n\
             running 12 tests\n\
             test scene::tests::case_000 ... ok\n\
             error: test failed, to rerun pass `-p letibot-harnessd --lib`\n\
             \n\
             Caused by:\n  process didn't exit successfully: \
             `target/debug/deps/letibot_harnessd-1234` (signal: 11, SIGSEGV)\n\
             error: 1 target failed:\n    `-p letibot-harnessd --lib`\n",
        "warning: unused variable: `x`\n".repeat(300)
    );
    assert!(aborted.len() > EVIDENCE_BYTES);
    let ex = excerpt(&aborted, EVIDENCE_BYTES);
    assert!(
        ex.contains("failing target `-p letibot-harnessd --lib` — it named no failing test"),
        "{ex}"
    );
    assert!(
        ex.trim_end().ends_with(
            "[gate excerpt: what failed is at the TOP of this row: `-p letibot-harnessd --lib`]"
        ),
        "{ex}"
    );
}

/// And the end of a stream that names no failure is inside the cap too - [`tail`]'s marker says what
/// it dropped, and the marker is part of what it was given, not added to it.
#[test]
fn the_end_of_a_stream_that_names_nothing_is_within_the_cap() {
    let text = "warning: function `f` is never used\n".repeat(EVIDENCE_BYTES / 8);
    let ex = excerpt(&text, EVIDENCE_BYTES);
    assert!(ex.starts_with("… ["), "the cut has to be admitted:\n{ex}");
    assert!(ex.contains("bytes dropped from the front]"), "{ex}");
    assert!(
        ex.len() <= EVIDENCE_BYTES,
        "the end is {} bytes against a {EVIDENCE_BYTES}-byte cap",
        ex.len()
    );
}
