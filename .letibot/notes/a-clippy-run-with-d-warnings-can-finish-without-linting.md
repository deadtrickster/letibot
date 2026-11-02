<!-- abstract: `cargo clippy -- -D warnings` can print `Finished` in 0.02 s and exit 0 having linted nothing, because the arguments after `--` are not part of cargo's fingerprint, so a warm target dir can make the gate's clippy step pass vacuously. -->

# A `cargo clippy -- -D warnings` run can say `Finished` without running the lints

Measured with a warm `CARGO_TARGET_DIR`: `cargo clippy -p letibot-transcript --all-targets -- -D
warnings` exited 101 with two errors, then exited 0 twice with `Finished` in 0.02 s and nothing
changed, then exited 101 with the same two errors after `touch crates/transcript/src/lib.rs`.

The mechanism is inferred: cargo's fingerprint for a unit does not include the
arguments after `--`, so a unit already built in that target dir without `-D warnings` - by a plain
`cargo clippy`, a `cargo check`, a `cargo clippy --fix` - is considered fresh and the invocation exits
0 having linted nothing.

It matters because the merge gate's second step is exactly that command: cold, it is a real check;
on a target dir that already holds a build of the same sources made with different `--` args it can be
a no-op that reports success.

Verify a clippy fix in a target dir that has never seen the crate built without the same flags
(`CARGO_TARGET_DIR=/data/letibot-target-verify`), or `touch` the sources first.

The rake: an exit status is a claim about a run, and a run that did no work makes the same claim as a
run that did. `Finished` in 0.02 s is the tell.
