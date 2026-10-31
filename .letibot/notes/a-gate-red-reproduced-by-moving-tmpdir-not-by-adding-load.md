<!-- abstract: MEASURED 2026-10-31: the harnessd lib pair that reds the merge gate is not shared state — it is two wall clocks, and it reproduces 5/6 by pointing TMPDIR at /var/tmp (disk) instead of /tmp (tmpfs), which makes the binary take 135 s instead of 15 s; 48 CPU hogs did NOT reproduce it. -->

# A gate red reproduced by moving `TMPDIR`, not by adding load — and it was two clocks, not shared state

MEASURED 2026-10-31, branch `agent/the-tests-share-what-they-set` (`main` at `ef025a37`), chasing
`-p letibot-harnessd --lib` failing 2 of 950 in the merge gate's whole-workspace run while passing
alone, at `--test-threads=4` and serialised (`--test-threads=1`: 950 passed, 79.85 s).

## The reproduction, and the two things that did NOT reproduce it

| condition | the binary | the pair |
|---|---|---|
| alone, `TMPDIR=/tmp` (this box's tmpfs) | 11–15 s | passes |
| 48 CPU hogs at load 32, same `TMPDIR` | 0.72 s for the three tests | **passes** |
| 6 copies at once, `TMPDIR=/tmp` | 15 s each | 0 of 12 copies failed the pair (3 failed `room::link`) |
| **6 copies at once, `TMPDIR=/var/tmp/letibot-gate/x`** | **115–136 s each** | **5 of 6 failed, `948 passed; 2 failed` — the gate's own numbers** |
| the whole workspace, `TMPDIR=/tmp` | lib: 15.7 s | passes (it did reproduce the launcher red) |

So: **a slow box, and the slowness that matters is disk, not CPU.** `/tmp` here is a 93 GiB tmpfs
and `/var/tmp` is a 98 %-full ext4; the gate's own step sets `TMPDIR=/var/tmp/letibot-gate/<tree>-<n>`
(`gate::GateScratch`), which is exactly the slow arm. Six copies were enough — no 222 binaries needed.

## What actually failed (the messages)

```
a_repository_with_no_merge_gate_is_not_a_queue_target ... FAILED
  panicked at serve/tests.rs:3368: the one that asked was gated
a_running_gate_does_not_stop_claims_or_deadlines ... FAILED
  panicked at serve/tests.rs:3593: no pass waited for the gate
```

Both are false statements about the product. The log lines beside the first one show the supervisor
doing its job: `merge queue: serving …/one` and `queues: 1 repository … declares no Merge gate — two
is not queued`.

## Mechanism 1 — a "floor" that stopped the service between two facts

`leases_seen_over`'s stop predicate was `done(s) || (over && seen_any) || ceiling`. `done` is the
health row the caller asserts; `over && seen_any` is a 3 500 ms floor. A lease is claimed in
`supervise`, the health row is written by the WORKER's first pass (which makes a landing worktree
first) — so on a slow box the floor fired in between and the caller asserted *"the one that asked
was gated"* about a supervisor that had done exactly right.

## Mechanism 2 — and why the previous fix was inert

`serve_until` had `Instant::now() + 20 s` and returned silently; `leases_seen_over`'s ceiling was
120 s, **inside a loop that could never run that long**. A fix that raised the inner number to 120 s
and made it assert could not fire — MEASURED: that ceiling was already in the tree and the red
survived it. One clock, owned by the caller, and a give-up that names the fact is the shape.

`a_running_gate_does_not_stop_claims_or_deadlines` failed on `started.elapsed() < 10 s` — a stopwatch
over four passes whose subject (*a pass WAITED for the gate*) would hang, not be slow.

## Two more of the same family, found by the same run

- `mergequeue::review::capacity_tests::a_reviewer_with_no_free_vm_slot_waits_uncounted_and_is_reviewed_when_a_slot_frees`
  asserted `deadline_ms - gate::now_ms()` in `50_000..=60_000` — i.e. *how long the fixture had been
  running*: measured 48 323 ms on the slow box while the row's own `deadline_ms - state_ms` still said
  60 000. The row's two stamps are the fact.
- `room::link::tests::an_event_about_a_session_rings_its_bell_and_a_reader_is_not_rung` — 3 of 6 on
  the slow box. `room::wake` is process-wide and keyed by SESSION ID (right for a daemon, which has
  one session per id), and five tests in `room/link.rs` connect a room as `s-a`, so their sentences
  landed in this test's queue. Fixed by giving the test ids of its own (`s-a-wake`).

## The rake

**A wall clock in a fixture is a bet on the box, and the box it is bet on is the gate's** — which
runs every test binary at once with `TMPDIR` on a real disk. Two numbers are legitimate: a hang
detector (far above any plausible slow box) and a *fact read off the product's own stamps*
(`deadline_ms - state_ms`, `created_at < started_ms`). Anything between them — 10 s, 20 s, a 3 500 ms
"floor" — turns a slow box into a lie about the subject, and the message it leaves names the product,
so the next session hunts a product bug that is not there. And to reproduce one: change `TMPDIR`, do
not add CPU load.
