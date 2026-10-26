<!-- abstract: The gate appends `[gate memory: …]`/`[step: …]` to a step's stored output (gate.rs:263-291) and setup::names_a_missing_thing then matches "no such file" INSIDE that annotation — so on a Mac every step-3 test failure without a code-failing marker reads "the gate's own setup failed". -->

# The gate read its own annotation as the step's last words

Found 2026-10-26/27 from the queue's notices on `main`: the same step 3
(`LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace --no-fail-fast -- --nocapture`) with the same
tail was called **RED** at tips `e51847ed`, `f35d19c`, `54e5a37`, `0158b95`, and **UNJUDGED** at
`300159d0` — "the gate's own setup failed at step 3 of 4 … its build and tests passed, and then:
`[gate memory: uncapped — the step ran in the daemon's own cgroup: … /proc/self/cgroup could not be
read: No such file or directory (os error 2)]`".

## The sentence quotes the runner, not the step

`gate.rs:263-291` (`StepMemory::evidence`) **appends** its own lines to the step's stored output —
`[gate memory: …]`, `[step: …]`, `[N process(es) …]`. `setup.rs`'s `names_a_missing_thing`
(`LAST_LINES = 3`) then scans the last three non-empty lines for `no such file`, `not found`, `no
binary`, … — and the `Uncapped` line ends with the OS error `No such file or directory (os error
2)`. So a Mac (no `/proc/self/cgroup` ⇒ every step is `Uncapped`) always has a "missing thing" in
those last three lines, and `setup_failure`'s second branch always fires for a step whose output
shows a `test result: ok` and no `error[`/`test result: FAILED`/`panicked at`/`failures:`.

The wording is the giveaway: `"its build and tests passed, and then: {last}"` is that branch
verbatim, so an operator can tell *which* classifier spoke without the code.

## What that costs

* **The same failure flips RED ↔ UNJUDGED by annotation, not by history.** A `cargo test
  --workspace` step over ~200 targets always contains `test result: ok` lines from the targets that
  passed, so `shows_a_pass` is true for *any* failure of that step.
* **UNJUDGED does not mean the code is fine.** The step still exited 101. What is missing is a
  *cause* in a shape `shows_code_failing` knows — and the shape actually printed (a bare list of
  ``\`-p X --test Y\`` `` lines, no `error:` line among them) is not one of them.

## Where a fix would go, if this is the shape to fix

Both in `crates/harnessd/src/mergequeue/`: strip the runner's own `[...]` annotation lines before
`names_a_missing_thing` searches (a step is judged by the step's words), and teach
`shows_code_failing` the shapes a workspace `cargo test` failure really takes here. Neither is a
package's change; and until the *step's* own 101 is explained, fixing the classifier only moves the
word, not the verdict.
