<!-- abstract: MEASURED 2026-10-30 against this box's cargo: a `panicked at` line is `thread 'name' (472254183) panicked at …` — the thread id follows the name, so a parse that expects the closing quote last finds no thread; and with `-- --nocapture` a failing test has NO `---- <name> stdout ----` block, so the block is not the door to a panic. -->

# A panic line carries the thread's id, and `--nocapture` leaves no `---- stdout ----` block

MEASURED 2026-10-30 on the operator's Mac, chasing a reviewer's proof that the gate's excerpt
named a test that PASSED (branch `agent/the-excerpt-quoted-a-green-test`). The fixture was a
two-crate scratch workspace whose `a` has a lib unit test that panics on purpose and passes, an
integration target with a `#[should_panic]` that does not panic, and one real assertion failure:

```sh
cargo test --offline --workspace --no-fail-fast -- --nocapture
```

## 1. The thread's id is between the name and `panicked at`

```text
thread '<unnamed>' (472254176) panicked at a/src/lib.rs:7:39:
thread 'a_turn_that_blew_up_is_reported' (472254183) panicked at a/tests/httphead.rs:5:40:
```

So `s[..at].trim().strip_prefix("thread '")?.strip_suffix('\'')` answers **None** on every real
panic, and anything that pairs a panic to a test by its thread name silently pairs nothing — the
fallback then quotes the `FAILED` line and the reason is lost. The parse that works is
`rest.split_once('\'')`: the name ends at the closing quote, not at the end of the head.

**And the thread name is not always the test's.** A test that spawns a thread which panics prints
`thread '<unnamed>' (…) panicked at …` — so a panic attributed by thread name is the *test's own
thread* only, which is exactly the case a `catch_unwind` in a passing test produces.

## 2. `--nocapture` means the failing test has NO block

The gate's steps run `cargo test … -- --nocapture`. With nothing captured, libtest prints the
panic in the RUN and writes **no** `---- <name> stdout ----` block for it:

```text
thread 'a_turn_that_blew_up_is_reported' (472254183) panicked at a/tests/httphead.rs:5:40:
assertion `left == right` failed: the status
  left: 500
 right: 200
test a_request_is_answered ... ok
test a_refusal_is_retried - should panic ... FAILED
test a_turn_that_blew_up_is_reported ... FAILED
```

`---- a_refusal_is_retried stdout ----` *is* printed — libtest writes that note itself, so a
`#[should_panic]` that did not panic keeps its block and its reason while the real failure has
neither. A reader that looks for a panic only inside a block therefore loses the panic of every
test that actually panicked, on the one flag this gate always passes.

## 3. The two other shapes, both already known and both confirmed

`error: test failed, to rerun pass `-p probe-a --test httphead`` is printed immediately after the
failing target's block with **no blank line** before the next `     Running …` line; and
`   Doc-tests probe_a` (3 spaces, one crate name, no parentheses) is a target with its own
`running N tests` and `test result:` lines — so a stream with two crates has two doc-test
segments that ran nothing.

## The rake

**A shape you have not printed is a shape you are guessing at.** The unit fixtures here already
had `thread 'worker::a_test' panicked at` and a `---- stdout ----` block for the failing test —
both true of a *captured* run and both false of the one the gate actually makes. Two minutes of
`cargo test` in a scratch workspace replaced two wrong assumptions, and the fix needed both.
