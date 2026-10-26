<!-- abstract: MEASURED 2026-10-26: main's red gate was Darwin's default SOFT open-file limit of 256, not a logic bug — the fifty-subagent test's stub accept thread died of `TooManyOpenFiles` (code 24), and `restart_restore` failed 2-4 different tests per run for the same reason. `memwatch::thread_names` reads /proc/self/task, so the fifty-subagent test's thread assertion is `0 == 0` on the Mac. -->

# `main`'s red was Darwin's 256 open files, and the fifty-subagent test asserts nothing about threads here

MEASURED 2026-10-26 on the Mac, at `2b51cc71` and again at `b79cd87c`. The gate's step
3 red was **one cause in two targets**: `-p letibot-harnessd --test session_lifecycle`
and `-p letibot-harnessd --test restart_restore`.

## What actually died

The stub's own accept thread, in `session_lifecycle`:

```
thread '<unnamed>' panicked at crates/harnessd/tests/session_lifecycle.rs:112:69:
clone the socket: Os { code: 24, kind: TooManyOpenFiles, message: "Too many open files" }
```

The stub goes with it, so not one child is ever answered and the test reports its
*subject* as missing (`waited 30s for all fifty children to answer`, 0 of 50, `stub
turns: 0`). `ulimit -n` here is **256** soft (`getrlimit`: `rlim_cur: 256, rlim_max:
RLIM_INFINITY`); Linux's default is 1024, which is why CI and the Linux box have
never seen it.

`restart_restore` is the same wall with a different face: 2 to 4 of its eighteen
tests failed per run and **different ones each time**, at `.expect("the parent
opens")` (a daemon that could not open its store), at a `wait_until` timeout and at
`task_start did not start` (a child that could not be started).

**Where ~350 descriptors go:** each harness opens the store AND a corpus connection
(`Harness::open_with_registry`: *"Two connections is the store's declared shape"*),
and sqlite in WAL keeps db + `-wal` + `-shm` — ~6 per harness — and a test binary
runs its tests in parallel. Five children at once is fine; fifty is not.

Decisive experiments, both on this box:

| | at 256 (the shell's soft limit) | at 8192 |
|---|---|---|
| `session_lifecycle` | red: EMFILE, 0/50 answered | 7 passed, 1.9 s, `stub turns: 50` |
| `restart_restore` | red: 2–4 tests, different each run | 18 passed, twice in a row |

Live proof while `session_lifecycle` hung: `lsof -p <test pid>` showed **102
`sessions.db` descriptors** and fd numbers up to 254.

## The fix

`crates/harnessd/tests/support/room.rs` — `room_for(what, need)`: raise the soft
limit to `min(4096, hard)`, print what was got, and ask
`apparatus::present(...)` for `need`, so a box that cannot give it *fails with that
sentence* (under `LETIBOT_REQUIRE_APPARATUS=1`) instead of timing out thirty seconds
later over the stub's corpse; without the prefix it is a skip with a reason, which is
what the tree means by "not provisioned". Both files call it at the top of every
test: `session_lifecycle` asks for 512, `restart_restore` for 1024. A failed
`getrlimit` claims nothing and the test runs as it always did.

## The other half: on macOS this test's central assertion is vacuous

`memwatch::thread_names()` reads `/proc/self/task`; on Darwin the directory does not
exist, so it answers an empty map, `threads_named` is **0**, and

```rust
assert_eq!(threads_after, threads_before, "threads were left behind …")
```

is `0 == 0`. The registry-entry and `opened() == 1` assertions do still measure. A Mac
census wants `proc_pidinfo(PROC_PIDLISTTHREADS)`, which gives thread ids and no
names — a separate change, not this red's.

## Why the seat note did not cover it

`which-seat-a-test-must-fixture-on-a-mac.md` says this file's
`a_message_to_a_parked_child_reopens_it_from_the_store` was "still red on the Mac"
with the stub's turn count never moving. Here it passes, in the same run as the red.
That note's suspicion (the 300 ms park window) is not established; the seat change
did work for the reopen.

## The shape of the trap, generally

A workspace test binary on this box starts at 256 descriptors. Anything that opens
more per unit of work than a handful is measuring the box's limit. The daemon's own
runtime is fine with a few children, but a real daemon asked for fifty at once on the
operator's Mac would hit the same wall — that is a product question nobody has asked
yet, and it is why the gate step (a `sh -c` from the daemon's own 256-fd shell) is
worth raising once, for every binary, rather than per file.
