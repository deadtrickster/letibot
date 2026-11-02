//! **The idle clock, proved**: it reads one child's CPU and nobody else's, and it waits for the
//! work to be over instead of for a clock.
//!
//! This file exists because the instrument it tests decided a red on the merge gate and nobody
//! could tell what the number meant. Four facts, in one test and in this order: the `/proc` and
//! `getrusage` readings are one number; a neighbour's CPU does not move this process's reading;
//! the settle waits for a burn to end; and a process that never goes quiet is reported as one.
//! `no_spin.rs` is where the same number is measured on a quiet box and on a busy one.

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

/// **Wait until a pid's own reading has advanced by `at_least`** - so a claim about a burner is
/// measured on the burner. Three answers: `Some(true)` it burned, `Some(false)` the reading moved
/// too little, and `None` on a box with no `/proc/<pid>/stat` - macOS - judged at the call site.
fn has_burned(pid: u32, at_least: Duration, within: Duration) -> Option<bool> {
    let started = Instant::now();
    let first = cpu::of_pid(pid)?;
    while started.elapsed() < within {
        // A pid that goes away mid-wait answers `None`, and a burner that is not there burned
        // nothing: a vanished reading is `false`, not an absence of the facility.
        if cpu::of_pid(pid).unwrap_or(first).saturating_sub(first) >= at_least {
            return Some(true);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Some(false)
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
    // **The burner's own reading, where this box can take it.** `cpu::of_pid` answers `None` where
    // there is no `/proc` - macOS - and `unwrap_or_default()` used to turn that into a burner that
    // read 0 for ever, so a Darwin run failed with *the burner never got going*.
    match has_burned(
        burner.pid(),
        Duration::from_millis(300),
        Duration::from_secs(30),
    ) {
        Some(true) => {}
        Some(false) => {
            panic!("the burner never got going, so nothing below would have been proved either way")
        }
        None if !cfg!(target_os = "linux") => println!(
            "  no /proc/<pid>/stat on {}: the burner's own reading is not available, so that it \
             burned is unproved here — what follows is this process's own getrusage",
            std::env::consts::OS
        ),
        // And on a box that HAS `/proc`, a pid with no reading is a shell that is already gone,
        // which is the fixture failing and not the platform: it stays a red.
        None => panic!(
            "the burner's own reading was not available on a box that has /proc/<pid>/stat, so its \
             shell was gone before it burned anything"
        ),
    }
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
    // **The failure path, which is the half that has to work.** A settle that waited for a spinning
    // process would wait for ever, so the ceiling is what turns the spin into a red.
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
