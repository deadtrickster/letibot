<!-- abstract: A reviewer is stopped by moving its row - the serve loop kills any child whose row is no longer reviewing under it - and a fixer by the runner that started it, guarded on `collect() == Running`. -->

# Stopping a reviewer is a MOVE; stopping a fixer is the runner that started it — MEASURED 2026-10-26, branch `queue-superseded`

Do not look for a kill in the queue's upkeep. `RepoQueue::review_upkeep`'s deadline path moves the row
`reviewing → waiting` with `.clear("reviewer")`, and `mergequeue::review::serve`'s collect loop then
finds a desk entry whose row is no longer `reviewing` under it and calls `p.runner.kill(&child)`. So
withdrawing a row IS stopping its reviewer, and `supersede_older_tip` (`mergequeue/serve.rs:1384`)
needs no runner for the same reason. Only the host can kill it in any case:
`HarnessTaskRunner::kill` looks the handle up in its own `slots` and refuses otherwise.

A fixer's handle is on the review row's `fixes` column, not in a desk, so its stop lives in
`review::serve`, idempotent by asking the runner first:

```rust
if matches!(p.runner.collect(fixer, ZERO), TaskStatus::Running { .. }) { let _ = p.runner.kill(fixer); }
```

`QueueDoor::session_live` is asked two different questions - is this tree in use (landing cleanup), has
the reviewer child died (re-seat) - and one default cannot serve both. Hence `fn sees_sessions(&self)`
(default `false`, `mergequeue.rs:426`): a door with no registry must never read as "the child is gone".
