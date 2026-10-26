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

## UPDATE 2026-10-27 — the list IS cargo's, and it names FAILING targets

MEASURED on this box (cargo 1.99.0, a two-member scratch workspace whose two test targets assert
`false`, `cargo test --offline --workspace --no-fail-fast`), exit 101:

```
error: test failed, to rerun pass `-p b --test two`
   Doc-tests a
…
test result: ok. 0 passed; 0 failed; …
error: 2 targets failed:
    `-p a --test one`
    `-p b --test two`
```

So the tail in every notice is cargo's **failing-target summary** — `N targets failed:` followed by
one backticked `` `-p X --test Y` `` per target that failed — and the count line is the line
immediately above it, which the tail's line budget cuts off. It is in
`src/tools/cargo/src/diagnostics/passes.rs` (the strings in `~/.rustup/toolchains/1.99.0-*/bin/cargo`
carry `1 target failed:` / ` targets failed:` / `, to rerun pass `` next to it).

Therefore step 3 on `main` is **not** "its build and tests passed": ~28 test *targets* failed, and
`setup_failure` took its second branch only because `shows_code_failing` saw none of its markers in
the step's output — no `test result: FAILED`, no `failures:`, no `panicked at`. A target that fails
an assertion prints all three (the measurement above), so these targets printed **nothing**: they
died at exec, before libtest ran. That is the shape this repository already measured once
(`the-gate-could-not-find-llama-cpp.md`: 70 targets aborting with `dyld: Library not loaded:
@rpath/libllama.0.dylib`), and the tip that can produce it is one touching a build script's rpath.

What to read, in `gate_steps_json`: `error: N targets failed:` for the count, and for each target
`error: test failed, to rerun pass …` + its `Caused by: …` line — `(signal: 6, SIGABRT)` and
`Library not loaded` mean the rpath/load class (a build, not the target's history), while a
`Caused by: exited with code 101` means the binary ran and something else is going on.
