<!-- abstract: `run_turn` started the prompt clock before the door's own act, so a wake with nothing to say published `waiting on: turn` then `quiet` - the exact pair `wait` arms on - and the clock now starts where a turn speaks. -->

# An empty wake published a turn that never ran

`turn_tail::run_turn` started the prompt clock before the door's own act, so a wake with nothing to
say - the daemon's startup ring, drained on a session with nothing restored - published
`waiting on: turn` and then `quiet`. `Head::wait` arms on `Turn`/`Calls` and answers `TurnEnded` on
the next quiet, so it exited 0 with an empty message for a session that had never turned.

The clock starts where a turn SPEAKS: `Harness::submit_items_then` (before the rows it appends, so
the append's own publish already carries `turn`) and `Harness::run_rounds` (the operator's `!` line,
whose rows are already in). A door that says nothing reaches neither.

`begin_turn_clock` is idempotent within one prompt, because a wall-continuation re-enters
`submit_items_then`, and it refuses to start a clock unless the tail has TAKEN a turn
(`Harness::turn_taken`, set by `run_turn`, cleared by `end_turn_clock`). That gate is not
decoration: `system_update` -> `submit_item` -> `run_rounds` runs outside any turn with no
`end_turn_clock` behind it, and a clock started there leaves the session reading `waiting on: turn`
for ever.

A measurement trap: libtest eats the daemon's stderr on a green run, so `eprintln!` probes from
harnessd appear only with `--nocapture`.
