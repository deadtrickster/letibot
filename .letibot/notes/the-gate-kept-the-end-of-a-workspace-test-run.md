<!-- abstract: The gate's excerpt was the end of a `cargo test --workspace --no-fail-fast` stream, which after the failing target is every other target's `running 0 tests` - so it segments at `error: test failed, to rerun pass`. -->

# The gate kept the end of a workspace test run, and the failing test is at the top of it

With `--no-fail-fast`, cargo runs every remaining target after one fails, so the last 4 KiB of the
stream is other targets' `running 0 tests` blocks and one summary line. A 4 KiB tail therefore said
`error: 1 target failed: -p letibot-harnessd --lib` and nothing else - not the test's name, not its
panic.

The boundary that attributes a failure to a target is `error: test failed, to rerun pass <clause>`,
written once per failing target and AFTER that target's output. `gate::failing_target`
(`crates/harnessd/src/mergequeue/gate.rs:1549`) segments on it, and the first `test … FAILED` or
`panicked at` in a segment is that target's first failure.

`gate::excerpt` now leads with the failure and ends with a tail: the failure is clipped to
`FAILURE_BYTES` (2400) and a tail of `MIN_TAIL` (320) survives. It also scans the whole stream for `Too many open files` and
`os error 24`, because a cargo that could not exec a test binary prints one such line and no
`failures:` block.

The row's second reader keeps only its END - `notices::failing_step` hands the model the last
`TAIL_LINES` (30) lines - so the excerpt also ends with a sentence naming the test.
