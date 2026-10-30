<!-- abstract: MEASURED 2026-10-30 on lab2x1: a test that races the scheduler passes alone and fails 48 of 48 when sixteen copies of its own binary run at once — so "it passes when I run it" proves nothing; and a hand-run test binary is not the suite's environment (no `~/.cargo/bin` on PATH fails `intent::env_tests::a_path_that_finds_the_same_file_changes_nothing`). -->

# A racy test passes alone; sixteen copies of its own binary make it fail every time

MEASURED 2026-10-30 on lab2x1, chasing the landing gate's red step (`--workspace --no-fail-fast`
under `systemd-run --user --scope -p Delegate=yes`).

## The recipe

```sh
BIN=~/…/target/debug/deps/mode_consent-<hash>
for r in 1 2 3; do
  seq 1 16 | xargs -P 16 -I{} sh -c "$BIN > /tmp/m-$r-{}.log 2>&1 || echo FAIL-$r-{}"
done
```

Sixteen *processes* of the same test binary, three rounds. Each is its own daemon/socket/temp
dir, so they do not interfere — they compete for the scheduler, which is the whole point.

| test | alone | 16 in parallel, before | after the fix |
|---|---|---|---|
| `bridge/tests/mode_consent.rs::a_mode_that_needs_nobody_applies_and_allow_all_waits_for_a_person` | 25/25 pass | **48/48 FAIL** | 48/48 pass |
| `mergequeue::serve::tests::a_gate_past_its_deadline_is_killed_and_timed_out_is_its_own_outcome` | pass | 36/36 pass (not reproduced) | pass |

So **"it passes when I run it" is not evidence** for a test that races the scheduler: run it once,
then run sixteen of it at once. And a test that does *not* reproduce under this is not thereby
proved innocent — the gate's own run failed `a_gate_past_its_deadline…` while 800 tests shared one
binary, which no amount of copies of that one test reproduces.

## The two shapes this caught

* **`mode_consent`** spawned the "person" thread and called `set_mode` on the next line: whether the
  person's `Attach` landed before the ask was the scheduler's to decide, and the daemon's refusal
  (*"no head that may decide is attached … besides the one that asked"*) reads like a policy answer.
  The fix waits for the fact — a channel send after `HeadClient::attach` returns, i.e. after the
  daemon greeted the head, which it does *after* attaching the seat.
* **`a_gate_past_its_deadline…`** gave the gate a 1 s deadline and then read the pidfile the step
  writes. The clock starts in `start_gate` and covers the gate's own setup (scratch dir, the cgroup
  probe the run's own `[gate memory: …]` line comes from) before the step's first line. 1 s measured
  the machine. 10 s is still 6× shorter than the step's `sleep 60`.

## Rake — a hand-run test binary is not the suite's environment

`cargo test` puts `~/.cargo/bin` on `PATH` (the harness exports it); a binary started from a plain
ssh shell does not, and `intent::env_tests::a_path_that_finds_the_same_file_changes_nothing` then
fails with *"the program `cargo` is not in it"* — four of my runs "failed" for that reason alone.
Export the same `PATH` the suite has before believing a hand-run binary.

## Two more the same runs caught, named and not fixed

* **`crates/tools/src/exec/term.rs:1051`** —
  `exec::term::tests::closing_kills_the_program_and_says_the_operator_closed_it` fails **4 of 4**
  full-workspace runs on lab2x1 and passes 5/5 when its own binary runs alone, 2/2 under cargo
  alone, 1/1 under a delegated scope, and 36/36 as twelve parallel copies of itself. So the ending
  it waits 20 s for is lost only while the suite runs, and a longer clock is not the fix: the wait
  is already a condvar on a fact.
* **`crates/tools/tests/remote_hosts.rs:1168`** — `a_stale_control_master_is_checked_and_dropped`
  writes `control_path("fake", std::process::id())`: a path keyed by HOST and PID, which every
  parallel test in that binary that talks to host `fake` shares (and `ControlPersist=120` keeps a
  master there). It failed one run in four, with the fake ssh's ledger showing no `-O check` at all.
