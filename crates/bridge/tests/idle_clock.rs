//! **The idle clock, proved**: it reads one child's CPU and nobody else's, and it waits for the
//! work to be over instead of for a clock.
//!
//! This file exists because the instrument it tests decided a red on the merge gate and nobody
//! could tell what the number meant. Four facts, in one test and in this order, because each
//! depends on the process being quiet at the start of the next:
//!
//! 1. **The two readings are one number.** `/proc/<pid>/stat`'s `utime + stime` and
//!    `getrusage(RUSAGE_SELF)`'s `ru_utime + ru_stime` are the same two fields of the same
//!    process, and `no_spin.rs` takes the syscall because it needs no `/proc` walk. A test that
//!    says *this is the child's CPU* has to be able to show the two agree — MEASURED on lab2x1
//!    (2026-11-01, USER_HZ 100): they agree to the tick, and the assertion allows two ticks
//!    because a tick can land between the two calls.
//! 2. **A neighbour's CPU is not this process's CPU.** A sampled percentage — `ps`, `top`, a
//!    delta of the whole machine — answers *the box is busy* for every process on it, which is
//!    what the gate's three reds had in common. So a real burner is started BESIDE this process,
//!    and this process's own reading must not move: measured with a child at a full core,
//!    **4–18 µs of a one-second span**. The burner's own reading is taken too, so the test cannot
//!    pass by the burner having failed to start.
//! 3. **The settle waits for a burn to end.** [`cpu::wait_until_idle`] is the half that was
//!    missing on 2026-11-01: a fixed `sleep(500 ms)` after a daemon's start opened the measured
//!    window on the daemon's opening, so the number was a function of how fast the box got
//!    through 260 ms of fixed work (1.3 ms quiet, 178 ms loaded). Here a burn runs for a second
//!    inside this process, and the settle must not return until it is over — which a fixed pause
//!    cannot do.
//! 4. **And a process that never goes quiet is reported as one.** The failure path is the half
//!    that has to work: a daemon that spins never settles, so the ceiling is what turns it into a
//!    red, and the red has to name the defect. Proved against a one-second ceiling rather than
//!    [`cpu::CEILING`]'s two minutes.
//!
//! **The load this file does not need to be run under**, and why: the instrument's whole claim is
//! that the number is the same on a quiet box and on a busy one. `no_spin.rs`'s report is where
//! that is measured (the same six windows, 0.6–4.0 ms quiet and under four hundred busy
//! processes); what is proved here is the mechanism that makes it so.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/cpu.rs"]
mod cpu;

/// A child that burns a whole core until it is killed, and takes its process away with it.
struct Burner(Child);

impl Burner {
    fn start() -> Burner {
        let child = Command::new("sh")
            .arg("-c")
            .arg("while :; do :; done")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("a shell to burn a core");
        Burner(child)
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Burner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// **Wait until a pid's own reading has advanced by `at_least`** — so a claim about a burner is
/// measured on the burner, and a burner that never ran cannot make a test pass.
fn has_burned(pid: u32, at_least: Duration, within: Duration) -> bool {
    let started = Instant::now();
    let first = cpu::of_pid(pid).unwrap_or_default();
    while started.elapsed() < within {
        if cpu::of_pid(pid).unwrap_or_default().saturating_sub(first) >= at_least {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn the_idle_clock_reads_one_child_and_waits_for_the_work_to_be_over() {
    // The instrument's own state: quiet, so the first measurement means something.
    cpu::wait_until_idle("the test process, before anything starts");

    // --- 1. the two readings are one number --------------------------------
    if let Some(from_proc) = cpu::procs_reading() {
        let from_rusage = cpu::this_process();
        let apart = from_proc.abs_diff(from_rusage);
        println!("  getrusage {from_rusage:?} · /proc/self/stat {from_proc:?} · apart {apart:?}");
        assert!(
            apart <= Duration::from_millis(20),
            "the two readings of one process's CPU disagree by {apart:?}: getrusage says \
             {from_rusage:?} and /proc/self/stat says {from_proc:?}. One of them is not \
             utime + stime of this process, and every window this tree measures rests on which."
        );
    } else {
        // No `/proc` (macOS). The cross-check is not available here, and saying so is the
        // difference between a skipped proof and a proof that passed.
        println!("  no /proc/self/stat on this box: the two readings are one number unproved here");
    }

    // --- 2. a neighbour's CPU is not this process's CPU --------------------
    let burner = Burner::start();
    assert!(
        has_burned(
            burner.pid(),
            Duration::from_millis(300),
            Duration::from_secs(30)
        ),
        "the burner never got going, so nothing below would have been proved either way"
    );
    let mine = cpu::over(cpu::SPAN);
    println!(
        "  beside a child at a full core: this process spent {mine:?} over a {:?} span",
        cpu::SPAN
    );
    assert!(
        mine < cpu::settle_budget(),
        "a child burning a core beside this process moved this process's own reading to {mine:?} \
         against a {:?} line over {:?}. That is the defect the instrument exists to avoid: a \
         sampled percentage — `ps`, `top`, a delta of the whole machine — answers for the box, and \
         the box was busy.",
        cpu::settle_budget(),
        cpu::SPAN
    );
    drop(burner);

    // --- 3. the settle waits for a burn to end -----------------------------
    cpu::wait_until_idle("the test process, after the neighbour has gone");
    let until = Instant::now() + Duration::from_millis(1_000);
    let burning = std::thread::spawn(move || {
        while Instant::now() < until {
            std::hint::spin_loop();
        }
    });
    // Let the burn begin, so the settle below cannot be one that ran before it.
    std::thread::sleep(Duration::from_millis(50));
    let settled = cpu::wait_until_idle("a burn of a second, inside this process");
    let _ = burning.join();
    println!("  the settle returned {settled:?} into a one-second burn");
    assert!(
        settled >= Duration::from_millis(750),
        "the settle returned after {settled:?} of a burn that runs for a full second: it is a \
         pause rather than a condition, and a pause is what made the measured window open on the \
         daemon's opening instead of on its idleness (178 ms of CPU against a 300 ms budget on \
         the gate's box, 1.3 ms on a quiet one — the same daemon)."
    );
    let after = cpu::over(cpu::SPAN);
    assert!(
        after < cpu::settle_budget(),
        "and the window after the settle is loud too ({after:?} over {:?}): the settle returned \
         while the burn was still going",
        cpu::SPAN
    );
    // A write to stdout, so a run under `--nocapture` shows the numbers even when it is green.
    let _ = writeln!(
        std::io::stdout(),
        "  the idle clock reads one child and waits for the work"
    );

    // --- 4. a process that never goes quiet is reported as one ------------
    // **The failure path, which is the half that has to work.** The daemon this instrument was
    // written for spent a whole core per stopped session; a settle that waited for that process to
    // go quiet would wait for ever, so the ceiling is what turns the spin into a red. Proved here
    // with a ceiling of two seconds against a burn of three, so the proof costs a couple of seconds
    // rather than [`cpu::CEILING`]'s two minutes.
    let until = Instant::now() + Duration::from_secs(3);
    let burning = std::thread::spawn(move || {
        while Instant::now() < until {
            std::hint::spin_loop();
        }
    });
    std::thread::sleep(Duration::from_millis(50));
    // The panic is the instrument working, so its message is captured and printed rather than left
    // to the default hook, which would print it as though this test had failed.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught = std::panic::catch_unwind(|| {
        cpu::wait_until_idle_within("a burn that never ends", Duration::from_secs(2))
    });
    std::panic::set_hook(hook);
    let _ = burning.join();
    let payload = caught.expect_err("a process at a full core must not settle");
    let said = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    println!("  the ceiling said: {said}");
    assert!(
        said.contains("has not been quiet") && said.contains("a thread looping"),
        "the ceiling fired with words that do not name the defect, so a red it produced would send \
         the next reader hunting something else: {said}"
    );
}
