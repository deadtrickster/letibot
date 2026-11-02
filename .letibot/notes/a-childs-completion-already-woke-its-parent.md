<!-- abstract: A `task` child's completion already wakes its parent - arming is the call site's `JobWatchSink` and both operator doors are armed - and the hole was a settlement for a session that was not open, now re-delivered at the next open. -->

# A child's completion already woke its parent — the hole was the row nobody re-read

The chain works: `crates/harnessd/tests/child_completion_wakes.rs` drives a real turn whose stub model
emits a `task` call, a real child, then `Work::Woken(parent)` with nobody calling `task_result`, and one
`User { speaker: Agent }` row naming the child and the first line of its answer.

Arming is a property of the call site, not of the runtime: `JobWatchSink` is what calls
`JobWatchers::watch`, and the round's own sink plus both operator doors (`run_operator_call`,
`run_operator_shell`, through `self.unprompted_sink()`) are armed - see
`the-sink-is-the-door-and-two-doors-had-none.md`. The bare `NullToolSink`s left in the tree are fixtures.

The hole was the row nobody re-read. `JobWatchers` is built per open, so a settlement for a session that
is not open dies twice: its queue belongs to a harness nobody drains, and its ring names a session
`wake_route` answers `Gone` for. `subagent_run` and `job` survive with `delivered = 0`, but
`restore_after_restart` filtered `r.instance != me`;
`Harness::deliver_settlements_left_undelivered`, at the top of it, now queues those rows as ordinary
`JobCompletion`s.
