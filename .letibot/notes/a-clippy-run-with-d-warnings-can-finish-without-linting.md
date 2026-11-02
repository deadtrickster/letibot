<!-- abstract: `cargo clippy -- -D warnings` can exit 0 having linted nothing, because args after `--` are not in cargo's fingerprint; and with `-D` a cold run stops at the first failing crate, so its error list is one crate deep. -->

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

The same `-D warnings` also makes a census one crate deep: the first crate that fails emits no
rmeta, so nothing downstream of it is linted. For a whole-tree count use
`sh scripts/clippy-gate.sh --all-targets --keep-going -- -A <the gate's four>`, which prints every
warning and aborts nothing.

A sweep that rewrites punctuation must not leave the new character where it is syntax: a doc line
that starts with ` - ` becomes a Markdown bullet and trips `doc_lazy_continuation`. After such a
sweep, `git grep -n '^ *//[/!] - '` is the census; fold the clause up so no line starts with the
hyphen.
