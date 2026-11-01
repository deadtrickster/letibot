<!-- abstract: MEASURED 2026-11-01: `cargo clippy -- -D warnings` reported `Finished` in 0.02 s and exit 0 on a crate that had just failed the same command twice, because the args after `--` are not part of cargo's fingerprint and the unit was already built without them; `touch` brought the errors back. A warm target dir can make the gate's clippy step pass vacuously. -->

# A `cargo clippy -- -D warnings` run can say `Finished` without running the lints

MEASURED 2026-11-01, worktree `agent-the-gate-means-it-about-clippy`, on lab2x1,
`cargo clippy -p letibot-transcript --all-targets -- -D warnings` with a warm
`CARGO_TARGET_DIR`.

    run 1  -> exit 101, two errors (large_enum_variant, empty_line_after_outer_attr)
    run 2  -> exit 0, "Finished `dev` profile ... in 0.02s"     (nothing changed)
    run 3  -> exit 0, same                                    (nothing changed)
    $ touch crates/transcript/src/lib.rs
    run 4  -> exit 101, the same two errors

The likely mechanism (inference, not measured): cargo's fingerprint for a unit does
not include the arguments after `--`, so a unit already built in that target dir
WITHOUT `-D warnings` — by a plain `cargo clippy`, a `cargo check`, or a
`cargo clippy --fix` run — is considered fresh, and the invocation exits 0 having
linted nothing at all.

**Why it matters for this tree**: the merge gate's second step is exactly
`cargo clippy --all-targets -- -D warnings`. On a landing worktree with a cold target
dir it is a real check; on any tree whose target dir already holds a build of the same
sources made with different `--` args it can be a no-op that reports success.

**What to do**: verify a clippy fix in a target dir that has never seen the crate
built without the same flags — `CARGO_TARGET_DIR=/data/letibot-target-verify` — or
`touch` the sources first. `--fix` is the one run that is immune, because it passes
its own `--cap-lints` and rewrites the units it touches.

The rake, generally: **an exit status is a claim about a run, and a run that did no
work makes the same claim as a run that did.** `Finished` in 0.02 s is the tell.
