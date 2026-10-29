<!-- abstract: MEASURED 2026-10-29 (harnessd 0.5.1, pid 53775): the 8½-hour merge-queue wedge was NOT a two-thread lock-order inversion — `JobWatchers::stop` took its own settlement queue twice (one non-reentrant mutex, one thread) and every publisher queued behind it; and the fix is the "leaf lock" rule for anything `Hub::publish` reaches. -->

# The queue was the victim, not half of the cycle — and "leaf lock" is the rule

MEASURED 2026-10-29, branch `agent/fix-jobwatchers-deadlock` (worktree
`agent-fix-jobwatchers-deadlock`, `main` at `706f4147`), from `sample 53775` plus the code.

## What the stack actually was

```
RepoQueue::pass → reap_gate → apply_gate → finish_health → judged → raise
  → publish_for_repository → Hub::publish → refresh_activity → activity::facts_of
    → JobWatchers::pending → Mutex::lock → __psynch_mutexwait     (795/795 samples)
```

**At `raise` the queue's thread holds no queue mutex.** `reap_gate` takes the gate out of
the slot *before* `apply_gate` (`slot.take()`), `judged`'s store writes are function-scoped,
and `first_time`/`rung` are released before `raise`. So the queue-side edge of the "cycle" is
the **worker thread itself**: `pass` never returns, so `beat()` is never called again — which
is exactly the damage (lease heartbeat 511 minutes stale) and why `target_health` sat `gating`
with **no gate process** (the slot was already empty when the verdict was applied).

The other side was one thread taking one mutex twice:

```rust
pub fn stop(&self) {
    let mut own = self.completions.lock()...;   // (1)
    let mut all = self.take_held();             // -> say_held() -> hub.publish(HeldResults)
                                                //    -> refresh_activity -> facts_of
                                                //    -> w.pending() -> completions.lock()  (2) DEADLOCK
```

`std::sync::Mutex` is not reentrant, so the closing session wedged itself, and every
publisher on the box — the merge queue's worker among them — queued behind it. `say_held`
only publishes when the held counts CHANGED, so the wedge needs a non-empty `held` set at
some point in the session's life; that is the normal case (`held` exists so a RUNNING turn is
not interrupted).

## The invariant, and the rule that satisfies it

> Nothing reachable from `Hub::publish` may block on a lock a non-queue thread can hold.

The publish path is: hub inner lock (released before the refresh) → `turn_shape` /
`work_pending` / `open_ask_ids` → monitors → `TodoBoard::snapshot` → the watcher set. Every
one of those except the watcher set was already a **leaf**: held only across a clone or a
store of an `Arc`, never across a call that can block. A leaf lock cannot be in a cycle.

So the fix is not `try_lock` and not a channel: `JobWatchers` now keeps the three facts the
activity reads where a publisher can take them without waiting —

* `pending` and `held` are **atomics**, written *inside* the lock that owns the queue/vector
  (`Settlements::any`, `held_now`), so a reader that loads `false` cannot have missed one;
* the watchers are one **snapshot** (`Arc<Vec<Watched>>`) behind a leaf mutex, rebuilt by
  whoever moves `mine`/`wanted` (order: `mine` → `wanted` → `snapshot`; the reader takes only
  `snapshot`), with each handle's `WantedNow` folded in so `awaited` needs no lookup.

Exactness beat `try_lock` here: the facts are cheap to keep exactly true, so there is no
"skipped refresh" to explain to a reader.

## Two rakes

* **A test for a deadlock must not `join` the wedged thread.** My first cut asserted after
  `stopper.join()`; on the pre-fix tree that turned a 5-second failure into a 600-second hang
  of the whole test binary. Assert on the bounded `recv_timeout` result first, join after.
* **The settlement's own `refresh_activity` runs while the handle is still in `mine`** (the
  watcher removes it after the push), so the line read `delivery, child s-kid-done` for a
  child that had already gone. Both watcher tails now rebuild + refresh after the removal.

## Proof, before and after

Reverted `activity.rs` + `jobwatch.rs` to `706f4147` (keeping only the `hold_settlements_for_test`
seam) and ran the three new tests:

| test | before | after |
|---|---|---|
| `a_publish_returns_while_the_settlement_queue_is_held` | `Timeout` (5 s) | ok |
| `a_session_closing_itself_does_not_wedge_a_publisher` | `Timeout` (5 s) | ok |
| `the_line_is_right_while_a_watcher_lives_and_after_it_has_gone` | `[Delivery, Child("s-kid-done")]` | `[Delivery]` |
