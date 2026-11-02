<!-- abstract: `no_spin`'s instrument was already the child's own CPU; the window was the defect, because it opened 500 ms after start and a daemon's opening is a fixed ~260 ms of work. The settle's line must be a tenth of the window's. -->

# A rate over a window that a clock opened is the box's speed, not the child's

`crates/bridge/tests/no_spin.rs` samples `getrusage(RUSAGE_SELF)`'s utime + stime, which is the child's own CPU. The instrument was right.

The 3 s window opened 500 ms after `World::start()`, and a daemon's opening is a fixed amount of work: about 260 ms of CPU on a quiet box and the same 260 ms under load, spread over 0.6 s and 4.7 s. So the window asked how fast the box got through it - 1.288 ms quiet, 177.852 ms under 400 busy processes, against a 300 ms budget.

The settle's line has to be a tenth of the window's. `settle_budget` (`crates/bridge/tests/support/cpu.rs`) is `SPAN / 100`, 10 ms of CPU in a second, so the most a stretched opening can leak into a 3 s window is 30 ms. Two weaker lines were measured failing: 10% of a core over 250 ms slices let a 4 ms lull end the settle and left 144 ms in the window, and 2% over two quiet slices let a bursty tail through and left 68 ms.

`crates/bridge/tests/idle_clock.rs` tests the instrument: the two readings agree, a child burning a core beside this process does not move the reading, the settle waits out a burn, and the ceiling fires on a process that never goes quiet.
