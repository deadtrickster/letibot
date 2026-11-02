<!-- abstract: A spawned child's handle is returned before its session is adopted, so every liveness check that reads `Registry::get` answers "gone" for a child that is still opening; the announcement is the third state. -->

# A child's handle exists before its session, and every liveness door said "gone"

`HarnessTaskRunner::start` mints the subagent id and returns it; the child's own thread calls
`Registry::adopt` only after `Harness::open` finishes, which for a VM-placed reviewer is a clone and a
boot. `Registry::get` answers `None` for that whole window, so `QueueDoor::session_live` - asked by
`RepoQueue::review_upkeep`'s `Reviewing` arm - said gone about a child that had just started. Three
passes of that reached `MAX_REVIEW_ATTEMPTS` and the row went `blocked`.

A rule that reads liveness off `registry.get` reads "not yet" as "no longer", for every child. The
missing third state is the announcement - `Registry::announce_spawn` / `spawn_settled` / `opening`,
spent by `adopt` and withdrawn by a `Drop` guard on the child thread - so `session_live` is
`get().is_some() || opening()`. `resumable()` and `mint_sub_id` want the same answer.

To stand a test in that state: the child's first act before its session exists is
`served_ctx(&parent.endpoint)`, a `GET /props`, so a loopback endpoint that holds that answer holds the
child inside its open with no sleep and no race (`mergequeue::serve::tests`' `HeldProps`).
