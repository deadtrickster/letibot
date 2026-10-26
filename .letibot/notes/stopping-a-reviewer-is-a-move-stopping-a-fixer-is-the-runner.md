<!-- abstract: MEASURED 2026-10-26 (package QD): serve.rs cannot stop a reviewer — the withdrawal IS the stop (review::serve kills a child whose row is no longer reviewing under it); a fixer can only be stopped by the runner that started it, so its stop lives in review::serve behind collect()==Running; and session_live's default false is right for the landing cleanup and wrong for "the child died". -->

# Stopping a reviewer is a MOVE; stopping a fixer is the runner that started it — MEASURED 2026-10-26, branch `queue-superseded`

Package QD: a target review row is about ONE tip, and the queue had to stop the children of the rows
it withdraws. The two children are stopped by two different mechanisms, and neither is on the queue's
own thread (`RepoQueue` holds a `Store` and a `QueueDoor`, and no `TaskRunner`).

## A reviewer is stopped by the ROW MOVING — do not look for a kill in serve.rs

`RepoQueue::review_upkeep`'s deadline path does not kill anything: it moves the row
`reviewing → waiting` with `.clear("reviewer")`. `mergequeue::review::serve`'s collect loop then finds
a desk entry whose row is no longer `reviewing` under it (`ours == false`) and calls
`p.runner.kill(&child)`. So **withdrawing a row is stopping its reviewer**, and that is why
`supersede_older_tip` (serve.rs) needs no runner: same mechanism, one line of prose in its doc comment.
The host is also the only thing that CAN kill it — `HarnessTaskRunner::kill` looks the handle up in
*its own* `slots` and refuses with *"no subagent `{handle}` in this session"* otherwise.

## A fixer can only be stopped by the runner that started it — so its stop lives in review.rs

The red tip's fixer is a `task_start` child of the gatekeeper session, started by
`review::Pool::runner.start_worktree`. Its handle is on the *target* review row's `fixes` column (set
by `open_fix_branch`), not in any desk. So `review::serve` carries the stop, and it is idempotent by
asking the runner first:

```rust
if matches!(p.runner.collect(fixer, ZERO), TaskStatus::Running { .. }) { let _ = p.runner.kill(fixer); }
```

`Running` is the whole test, so a fixer that has finished (the ordinary case — its branch row exists
only after it finished) is never asked about again. The trigger is `tip_now(repository) != r.head_sha`
read off git, so nothing has to be recorded on the row to make the stop happen once.

## `session_live`'s default `false` means two different things, and only one of them is safe

`QueueDoor::session_live` is asked by the landing cleanup (*is this tree in use?* — `false` ⇒ remove
the tree, R92.24) and by the new re-seat rule (*has the reviewer child died?* — `false` ⇒ move the row
back to `waiting`). One default cannot serve both. MEASURED by trying it: `harness::queue_e2e`'s
`Quiet` door answering `true` for everything made
`a_finished_child_leaves_a_review_row_and_the_queues_land_it` fail on
*"the landed branch's worktree is retired"* — the fixture's child session is still in the registry, so
an honest door keeps the tree, and that test asserts the tree goes.

So the trait grew `fn sees_sessions(&self) -> bool` (default `false`): a door with no registry never
reads as *the child is gone*. `RegistryDoor` and the `Door` in `serve/tests.rs` answer `true`; `Quiet`
does not, and `queue_e2e.rs` is unchanged.

## The rows themselves

- A target review row whose `head_sha` is not the target's tip is withdrawn — **every** move of the
  tip, not only R76 item 4's rewrite. That is wider than the old `supersede_if_rewritten`, which kept
  a review whose tip had moved FORWARD ("the red may still be in it") and so left four open at once.
- A fix branch (`kind == Branch`, `fixes == Some(red)`) is superseded when `red` is not the tip; its
  open merge row goes with it; its worktree and branch go only through `git::retire_if_empty`
  (`0 == commits_over(tip, head)` then `retire_branch`, which refuses on uncommitted work), and a tree
  whose session is still live is never touched.
- `open_fix_branch` (review.rs) refuses when the tip has moved past `r.head_sha`, recording the
  refusal as a `rejected → rejected` transition rather than raising: the new tip is gated, and that is
  the only thing anybody has to act on.
