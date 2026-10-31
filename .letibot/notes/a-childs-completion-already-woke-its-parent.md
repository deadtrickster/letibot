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

The arming is exactly one line in the tree: `grep -n self.tool_sink crates/harnessd/src/harness.rs`
finds `self.runtime.invoke(&turn_id, call, &mut self.tool_sink)` and nothing else. A `task` call
that does **not** go through a turn is not armed at all — `Parent::call` in
`message_between_turns.rs`/`session_channel.rs` uses `NullToolSink`, and `Harness::run_operator_call`
uses a `NullToolSink` too. So an operator-run `task` (the `!` door) would arm nothing. That is a
second, narrower hole than the one the entry names.

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

## The door that is still not armed

`Harness::run_operator_call` invokes the call with `letibot_tools::events::NullToolSink`
(harness.rs ~6833), so a `task` the OPERATOR starts (a `!` line, a head's `/` verb) arms no
watcher and produces no wake at all. Left open on purpose: it is the operator door, it is a
different slice, and the brief's measurement was the model's path. The fix is one decorator at
that door and the same `JobWatchers` — but it changes what an operator-run `task` does to the
model's turn, which is a ruling rather than a repair.
