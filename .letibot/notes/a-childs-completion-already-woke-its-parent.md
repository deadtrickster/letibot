<!-- abstract: MEASURED 2026-10-31: R57's last hop WORKS on this tree — a real turn's `task` call is armed by `JobWatchSink`, the child's settle rings the parent, and `Harness::wake` composes the `[task]` notice; the hole that remained was the store row not being re-delivered at the next open. -->

# A child's completion already woke its parent — the hole was the row nobody re-read

MEASURED 2026-10-31, branch `agent/a-childs-completion-wakes-its-parent` (worktree of the same
name), on lab2x1. R57's TODO entry says *"nothing is armed when a `task` child starts"* and *"the
last hop is STILL UNTESTED"*. The first half is **stale**; the second was true and is now tested.

## The chain, measured end to end

`crates/harnessd/tests/child_completion_wakes.rs` drives it through production doors only: a real
`Harness` over a real store and registry, a real turn (`Harness::submit`) whose model is a stub
that emits a `task` tool call, a real child on its own thread, then
`Registry::next_work` → `Work::Woken(parent)` **with nobody calling `task_result`**, then
`Harness::wake` → one `User { speaker: Agent }` row saying *a subagent you started has finished*,
naming the child and carrying the first line of its answer. Green in 0.84 s on the first run.

The arming is a property of the **call site**, not of the runtime: `JobWatchSink` is what calls
`JobWatchers::watch`, and a `ToolEventSink` is a parameter of the invocation. Two places arm it
and both are armed on this head — the round's own sink
(`self.runtime.invoke(&turn_id, call, &mut self.tool_sink)`, `Harness::run_rounds`), and the
operator's doors, which invoke through `self.unprompted_sink()` (`Harness::run_operator_call`,
`Harness::run_operator_shell`) with `crate::bangrun`'s thread carrying the watcher across itself.
That is `the-operator-door-arms-a-watcher` (`33db35cf`, `c128fb0e`), landed on `main` before this
branch was rebased onto it, and its test is `crates/harnessd/tests/operator_task_wakes.rs`. The
bare `NullToolSink`s that remain are test fixtures (`Parent::call` in
`message_between_turns.rs`/`session_channel.rs`), not doors.

## What was actually missing: the row was never re-read

`JobWatchers` is built per open, so a settlement for a session that is not open dies twice: the
queue belongs to a harness nobody drains, and the ring names a session `wake_route` answers `Gone`
for. The durable half survives — `subagent_run` (written by the child's own thread, `delivered = 0`)
and `job` (written by `JobRecorder::record`, same column) — but **nothing read it at the next open**:
`restore_after_restart` filters `r.instance != me`, so a row THIS daemon wrote was skipped, and the
store's own doc (`crates/tokencore/src/store/subagents.rs`, `instance` paragraph) says it should be
delivered at the next open.

`Harness::deliver_settlements_left_undelivered` (harness.rs, called at the top of
`restore_after_restart`) queues those rows as ordinary `JobCompletion`s, so the sentence is
`completion_notices`' and not a restart's. Disjoint from the `instance != me` half by construction.

## Rakes

* **A stub that answers its connections one at a time deadlocks the test it was written for.** The
  first cut held the child's request inside the accept loop, so the parent's next round could not be
  answered and `submit` failed with `WouldBlock` three minutes in. One thread per connection.
* **Gate the child's request by what it was ASKED, not by index.** `task`'s result echoes only
  `first_line(prompt)`, so a marker on the subtask's second line is in the child's prompt and in no
  prompt of the parent's — a stable discriminator where request order is a race.
* **Two `Registry`s are two daemons.** `Registry::instance()` is minted per registry, so a test that
  reopens a session must reuse the registry or the `instance` filter measures the wrong branch.
* A session is claimed for writing at open (`writer::claim`), so a test that reopens one must DROP
  the first harness first.

## The numbers, and what each test would have failed on

* both tests: **2 passed in 1.31 s** (`--test-threads=1`, lab2x1, `LETIBOT_REQUIRE_APPARATUS=1`).
  Test 1 alone was green in 0.84 s on the base tree, before any change — which is the measurement
  that says the chain was never broken.
* **test 1 with `JobWatchSink::emit`'s `w.watch(handle.clone())` commented out**: FAILED in 30.71 s
  at *"the child answered and its parent was never woken — nothing was armed for it"*. That is the
  2026-10-03 state reproduced on purpose, and it is what makes the test a measurement of the arming.
* **test 2 with `harness.rs` reverted to the parent commit**: FAILED in 0.85 s at
  *"the settlement this daemon dropped is a turn at the next open"* — `wake()` answered `None`,
  i.e. the settlement never reached the model.

## Where the operator door is armed — corrected, and it was already done

An earlier cut of this note said `Harness::run_operator_call` invoked with a bare
`letibot_tools::events::NullToolSink`, so a `task` the OPERATOR starts (a `!` line, a head's `/`
verb) armed no watcher and produced no wake. **That was false on the head this branch is rebased
onto.** Both operator doors invoke through `self.unprompted_sink()` — a `JobWatchSink` over
`NullToolSink` — and `crate::bangrun`'s thread carries the watcher across itself, so an
operator-run `task` and a `!` line promoted mid-flight each queue a settlement and ring the
session's bell. The measurement is `the-sink-is-the-door-and-two-doors-had-none.md` in this same
notes directory; the test is `crates/harnessd/tests/operator_task_wakes.rs`
(`a_task_the_operator_runs_rings_the_session_that_ran_it`). Nothing is left to arm here, and the
next reader should not go looking.
