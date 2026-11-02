<!-- abstract: A command taken off the hub queue is not a finished delivery: the turn it makes has not started yet, so refreshing the activity at the pop reports a false quiet and a `wait` answers with the previous turn's message. -->

# A command taken off the hub queue is not a delivery finished, and refreshing there is a false *done*

`Delivery` means the model is owed a turn it has not taken: `Hub::work_pending()`
(`crates/sessionlog/src/hub.rs`) plus the watcher set's queued and held settlements. `Hub::submit`
publishes it when a prompt is queued, and the worker taking the command does not start the turn -
`run_prompt` appends the row, checks the wall, maybe compacts, and only then does the engine emit
`TurnStarted`.

So a refresh at the pop is a lie: it says Quiet in the gap, and a `wait` that arms on any busy value
answers `0 - the turn ended` with the previous turn's message, milliseconds after the send.

Two rules follow. Do not refresh the activity at the pop. And `wait` arms on `Turn`/`Calls` only,
never on any busy value. The escape hatch for a verb that runs no turn is that every such command
ends by saying why on the log - `Warning` is in `SessionEvent::touches_activity`
(`crates/sessionlog/src/event.rs`), which is what keeps a stale `Delivery` from wedging a session.
