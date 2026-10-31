<!-- abstract: MEASURED 2026-10-31: `JobWatchSink` is per CALL SITE, so arming a watcher is a property of the door — the turn path arms it, and two operator doors did not; a promoted `!` run is the reachable instance and the row still promised delivery. -->

# The sink is the DOOR, and two of the daemon's doors armed nothing

**MEASURED 2026-10-31**, branch `agent/the-operator-door-arms-a-watcher`, chasing the brief's
"an operator-run `task` arms nothing" — which was half right, and the half it got wrong is the
useful half.

## The mechanism, in one sentence

`JobWatchSink` is what calls `JobWatchers::watch`, and **a `ToolEventSink` is a parameter of the
invocation, not a property of the runtime** — so "is this handle watched?" is decided by the
call site that invokes the tool, and every new door that invokes one is a new chance to mint a
handle in nobody's table.

| door | sink before this | can it return `Backgrounded`? |
|---|---|---|
| `Harness::run_rounds` (`self.tool_sink`) | armed | yes — the whole chain |
| `Harness::run_operator_call` (the console) | `NullToolSink` | yes (`task`, and anything `HEAD_RUN_TOOLS` grows to hold) |
| `Harness::run_operator_shell` (`!`, synchronous) | `NullToolSink` | **yes — a Ctrl+O mid-flight promotion** |
| `bangrun::run_and_hand_back` (`!` between turns) | `NullToolSink` | **yes, same** |
| `watch_opened_pr` / `plugin_arm` / `restart` | `NullToolSink` | no — `monitor` and friends never background |

## The reachable one, and why it is the good evidence

`bash`'s promotion arm (a foreground call the operator moved to the background) returns
`Invocation::backgrounded(...)` and its row says, in words, *its completion will be delivered to
you on its own when it ends*. With a bare null sink nothing ever armed a watcher for that
handle: no settlement queued, no bell rung, no delivery — **the row made a promise the daemon
could not keep**, and nothing said so. `sessions.rs`'s `CommandKind::Promote` arm documents the
promotion as working, which is true of the process move and false of the telling.

## The fix, and the shape to keep

`Harness::unprompted_sink()` = `JobWatchSink::new(NullToolSink, self.job_watch.clone())`. Null
because the transcript row is the durable record and a second copy as an event is one fact in
two places; watched because the handle needs a table. `bangrun::spawn_run` now takes the
watcher across its thread. **A new door that invokes a tool outside a turn must be given this,
not a bare `NullToolSink`.**

## Rakes

* **`Parent::call` in `message_between_turns.rs` / `session_channel.rs` is a FIXTURE, not a
  door.** It invokes through `runtime_handle()` with a `NullToolSink` to stand in for the
  engine's call — so those two files spawn children that never settle, and pass by polling
  `task_result`. `clear_context.rs`'s `Parent::call` is the same fixture done right
  (`JobWatchSink::new(NullToolSink, h.job_watchers())`). There is no *production* caller of
  `runtime_handle()` outside `bangrun`.
* **A test that proves "the bell rang" must not use `next_work` once and believe it.** The
  registry queues the child's own `Work::Open`, and a `Woken` for the child's session is not
  the parent's ring: loop over `next_work_until(Some(now))`, pass over everything that is not
  `Woken(me)`, and let `Idle` be the only thing that sleeps.
* **`git stash push -- <paths>` after committing the fix stashes nothing** and prints "No stash
  entries found" — the before/after proof has to revert a commit
  (`git checkout <fix>^ -- <paths>`, run, `git checkout <fix> -- <paths>`).
* A `promote_midflight` run now leaves a `.letibot/delivery-log.jsonl` in the workspace (the
  promotion's settlement is delivered, rule `immediate:dependency_failed` — a promoted call is
  `Awaited`, and `signalled 15` is `failed`). That file is test litter; do not commit it.
