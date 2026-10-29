<!-- abstract: MEASURED 2026-10-29: the gate's 4 KiB excerpt was the END of a `cargo test --workspace --no-fail-fast` stream, which after the failing target is every other target printing `running 0 tests` — so two reds said only `error: 1 target failed: -p letibot-harnessd --lib`; the failing test's name is at the TOP of its own target block, and `error: test failed, to rerun pass X` is the boundary that says whose failures those are. -->

# The gate kept the end of a workspace test run, and the failing test is at the top of it

MEASURED 2026-10-29 (branch `agent/a-gate-excerpt-keeps-the-failure`, `main` at `8e30d902`).

## The shape of the stream, which is the whole defect

`LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace --no-fail-fast -- --nocapture` prints, per
target: `     Running unittests src/lib.rs (…)`, `running N tests`, the test lines, the failing
test's `failures:` block (name + `panicked at` + its message), `test result: FAILED. …`, then
`error: test failed, to rerun pass \`-p letibot-harnessd --lib\``. **`--no-fail-fast` then runs
every remaining target**, so the last 4 KiB of the stream is other targets' `running 0 tests`
blocks and cargo's one-line summary `error: 1 target failed: \`-p letibot-harnessd --lib\``.
`gate::tail` kept exactly that, so the row could not name the test — twice tonight, on `main` and
on another branch, and the same command by hand passed 782/0 both times.

## The boundary that attributes a failure to a target

`error: test failed, to rerun pass <clause>` is written ONCE PER FAILING TARGET and AFTER that
target's output. So segmenting the stream at those lines gives each target its own slice, and the
first `test … FAILED` / `---- X stdout ----` / `panicked at` in that slice is that target's first
failure. That is `gate::failing_target` — no need to map a test name to a package any other way.
A runner that is not cargo (`bin/firecode test`) has no boundary: the whole output is one segment.

## What the excerpt is now (`gate::excerpt`, cap `EVIDENCE_BYTES` = 4096)

    [gate excerpt: 61 target(s) ran — 60 passed, 1 failed; 300 test(s) passed, 1 failed. …]
    error: 1 target failed:
    `-p letibot-harnessd --lib`
    [gate excerpt: failing target `-p letibot-harnessd --lib` — first failing test `worker::a_test`]
        thread 'worker::a_test' panicked at crates/harnessd/src/worker.rs:12:5:
        the worker never started
    … [18851 byte(s) of the step's output were left out here; its end follows]
    <the tail, ~1.5 KiB>

Failure section is clipped to `FAILURE_BYTES` (2400) so a tail always survives; if the room left
is under `MIN_TAIL` (320) the end is dropped and the section says so in as many words. A stream
that fits in the cap is returned untouched, so a green step's row is byte-identical to before.

## The question it was built to answer: would an EMFILE `fork` have been recognisable?

**No, not in the old excerpt** — the panic line sits inside the failing target's block, hundreds
of lines before the end. That is why `excerpt` also scans the WHOLE stream for `Too many open
files` / `os error 24` / `Cannot allocate memory` / `No space left on device` / `Resource
temporarily unavailable` and prints up to 3 such lines, whether or not any test was named: cargo
that could not even exec a test binary says `error: failed to run \`…\`: Too many open files (os
error 24)` and no `failures:` block, and that line alone is now on the row.

## Rake

`[gate excerpt: …]` lines pass `gate::for_the_model` (it drops only `[gate memory: `) and
`setup::step_words`, so `setup::shows_code_failing` still sees cargo's `error: N target(s)
failed:` verbatim — the setup-vs-red classifier did not move.
