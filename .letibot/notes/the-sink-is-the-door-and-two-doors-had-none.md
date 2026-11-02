<!-- abstract: `JobWatchSink` is a parameter of the invocation, not a property of the runtime, so arming a watcher is a decision each door makes - and two operator doors passed a bare `NullToolSink` while their rows promised delivery. -->

# The sink is the DOOR, and two of the daemon's doors armed nothing

The turn path arms a watcher; the operator doors did not. A promoted `!` run - a Ctrl+O
mid-flight promotion - returns `Backgrounded` and its row says the completion will be delivered
on its own, and with a bare `NullToolSink` nothing armed a watcher: no settlement queued, no
bell rung, nothing said so. `sessions.rs`'s `CommandKind::Promote` documents the promotion,
which is true of the process move and false of the telling.

`Harness::unprompted_sink()` is `JobWatchSink::new(NullToolSink, self.job_watch.clone())`: null
because the transcript row is the durable record, watched because the handle needs a table.
`bangrun::spawn_run` carries it across its thread. A new door that invokes a tool outside a turn
takes this, not a bare `NullToolSink`.

Rakes: `Parent::call` in `message_between_turns.rs` and `session_channel.rs` is a FIXTURE that
invokes through `runtime_handle()` with a bare sink, so its children never settle and it passes
by polling `task_result` (`clear_context.rs` does it right). And a test proving "the bell rang"
must loop over `next_work_until`, because the registry queues the child's own `Work::Open` and a
`Woken` for the child is not the parent's ring.
