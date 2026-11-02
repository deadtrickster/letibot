<!-- abstract: MEASURED 2026-11-01: `cargo clippy -- -D warnings` can print `Finished` in 0.02 s and exit 0 without linting, because the args after `--` are not in cargo's fingerprint; and with `-D` the run also stops at the FIRST failing crate, so a cold run's error list is one crate deep. -->

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

## UPDATE 2026-11-02 (branch `agent/the-doc-class-cold`): the same `-D` also makes a census one crate deep

The trap above is about a run that lints nothing. This is its other face: with
`-D warnings`, the first crate that fails does not produce an rmeta, so every crate
downstream of it is never checked at all. A cold `--all-targets -- -D warnings` over
this tree at `52ebb6b7` names **one** error (`tokencore/src/store.rs:1119`) and exits
101, while the tree holds 23.

The whole-tree count needs two changes to the command, and neither weakens the lint:

    sh scripts/clippy-gate.sh --all-targets --keep-going -- -A <the gate's four>

`--keep-going` keeps cargo going past the failing crate, and dropping `-D warnings`
is what lets the crates that DO fail still emit their rmeta, so their dependants get
linted. Every warning is then printed and nothing aborts. Measured before/after with
that exact line in a fresh `CARGO_TARGET_DIR` each time: **23 findings over 11 files
-> 0**, and the gate's own `-D warnings` line in a third fresh directory exits 0.

## And the cause of that class is the em-dash sweep

`2811f711` replaced `—` with `-` everywhere, including at the start of a doc line,
where a rhetorical dash becomes a Markdown bullet. Everything after it at the same
indent is then `doc_lazy_continuation`. **A sweep that rewrites a punctuation mark
must not leave it in a position where the new character is syntax**: after such a
sweep, `git grep -n '^ *//[/!] - '` is the census, and the fix is to fold the clause
up so no line begins with the hyphen, not to indent the paragraph under it (that
keeps a bullet the sentence does not have) and not to insert a blank `///` (that
cuts one sentence into two).
