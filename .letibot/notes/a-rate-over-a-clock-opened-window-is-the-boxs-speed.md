<!-- abstract: MEASURED 2026-11-01: `no_spin`'s instrument was already the child's (`getrusage` = utime+stime), and its 3 s window opened 500 ms after the daemon's start — so it measured how much of a FIXED ~260 ms of start-up CPU the box had got through: 1.3 ms quiet, 178 ms under load. The settle's line must be a TENTH of the window's (1% of a core over a whole second, not 2% over 250 ms slices) or the tail of the opening leaks into the window. -->

# A rate over a window that a clock opened is the box's speed, not the child's

MEASURED 2026-11-01, branch `agent/measure-the-child-not-the-box`, from the merge gate's own report
(`main` at `a9d81c48`, 227 targets ran, 3 failed: `no_spin`, `oneshot_sigint`, `term_way_out`).

## The instrument was right and the window was wrong

`crates/bridge/tests/no_spin.rs` sampled `getrusage(RUSAGE_SELF)`'s `ru_utime + ru_stime` — which
**is** the child's own CPU, the same two numbers `/proc/<pid>/stat` carries, and no percentage and no
whole-machine delta. So the brief's suspect was not the defect, and neither is the fix "read
`/proc` instead".

The defect was that the 3 s window opened **500 ms after `World::start()`**, and a daemon's opening
is a *fixed* amount of work. Sampling this process every 250 ms:

| | quiet | under 400 busy processes |
|---|---|---|
| the opening's total CPU | **~260 ms** | **~265 ms** — the same work |
| wall clock it is spread over | **0.6 s** | **4.7 s** |
| what the first 3 s window read | **1.288 ms** | **177.852 ms** |

Same daemon, same assertion, a 138× difference, against a 300 ms budget. The window was asking *how
fast did the box get through 260 ms of work*, and the gate's box answered "not fast enough".

## The settle's line is arithmetic, and three cuts were needed to get it

Waiting for the process to go quiet is the right condition — it is read off the child, so a quiet box
and a busy one settle at the same *point* and differ only in how long it takes (printed beside the
number: `quiet after 3.76 s`). But the line decides whether the window that follows is clean, and
each mistake was measured:

1. **10% of a core over 250 ms slices** (the window's own line): a stretched opening sits *just
   under* the settle and just over the window. A **4 ms lull** in a 250 ms slice ended the settle and
   the window then read **144 ms against a 300 ms budget**.
2. **2%, two quiet slices in a row**: a bursty tail puts a few 250 ms slices under 5 ms each, so the
   condition was satisfied while the work went on — the settle ended after 5.5 s and left **68 ms**
   in the window (box at load 87).
3. **1% of a core over a whole second** — the shipped shape. The settle measures a *total*, which no
   phase of a burst can fool, and the most it can contribute to a 3 s window is 30 ms, a tenth of the
   budget. Idle is 0.19 ms in a 250 ms slice (0.02–0.05% of a core), twenty times under the line.

After (3): quiet `2.38 2.20 2.21 2.02 2.08 2.31 ms`, under 400 busy processes
`0.94 0.93 0.79 0.57 0.76 0.66 ms` — the instrument reads the same, which is the whole claim. The
first settle is 2.0 s quiet and 3.7–5.5 s loaded: that is the box's number, and printing it beside
the daemon's is what keeps the two apart.

## A test for the instrument, because the instrument decided a red nobody could read

`crates/bridge/tests/idle_clock.rs`, one test with four parts in order (one test, because parts 3 and
4 burn a core in-process and would poison the others):

1. the two readings agree (`getrusage` 0.6–1.0 ms vs `/proc/self/stat`, apart by under two ticks —
   `/proc` is quantised to 10 ms);
2. **a child burning a full core beside this process does not move this process's reading** (4–18 µs
   of a one-second span), and the burner's own reading is taken first so the test cannot pass by the
   burner not starting;
3. the settle does not return until a one-second burn inside the process is over (2.0 s);
4. **and the ceiling fires on a process that never goes quiet** — with a 2 s ceiling against a 3 s
   burn, `caught = catch_unwind(...)` and the message asserted to name the defect (`the last one cost
   978.322ms of CPU (the line is 10ms)`). The failure path is the half that has to work.

## Three rakes that cost a build each

**A wait for a frame the head QUEUES has to pump the loop.** `App` pushes `Action::TermStatus` on the
`Hello` and `Link::tick` writes it, so waiting on the daemon's channel alone waits for a frame nobody
has sent: all seven `term_way_out` tests timed out at the 60 s ceiling on the first cut of `attach`.

**`cargo test -p A --test X` is not the same build as `-p A -p B --test X --test Y`.** Feature
unification changes the metadata hash, so two `no_spin-<hash>` binaries exist in one target dir and
running the stale one reports the old numbers. Run the suite through `cargo`, or take the path cargo
just printed.

**`--nocapture` plus a `| tail -60` on a long verification script keeps only the tail** — which was
the shell's job-control noise from killing the load, not the results. Write the log to a file.

## The gate's own shape is `TMPDIR` on a real disk

`no_spin` reproduced on CPU load alone. `term_way_out` did **not** (20 copies × 400 spinners, all
green) and did under `TMPDIR=/var/tmp/letibot-gate-probe` + 300 spinners — 1 copy in 20 failed, two
tests, `the pane opened: []` and `the Enter is the program's …: []`, both because a 200 ms drain gave
the in-process daemon thread no chance to read a frame the head had already written. That is the axis
`a-gate-red-reproduced-by-moving-tmpdir-not-by-adding-load` found, and the lesson generalises: **a
200 ms drain of an in-process channel is a bet on the scheduler, and the gate's scheduler is not the
one in your shell.**
