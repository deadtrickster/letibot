<!-- abstract: `Registry::close` interrupted only sessions whose round-level `running` flag was set, which is false for the whole of a round's tool calls - so Ctrl-C in a one-shot killed the command and let the turn ask the model again. -->

# Ctrl-C in a one-shot did nothing, because `running` means a ROUND

`Daemon::catch_signals` -> self-pipe -> `Registry::close` is fine, and the run enders do work: a
`bash {"command":"sleep 120"}` call came back `killed by the daemon stopping`, and the turn then went
on to its next round and asked the model again. The cause is one predicate - `close` interrupted a
session only `if h.status().running`, and that is the view's `TurnState`, published once per ROUND, so
it is false for the whole of a round's tool calls: exactly the window a person presses Ctrl-C in.

`Hub::turn_in_flight` is the same instant with the calls taken into account, and its own doc says
every reader that means "is a turn in flight" and reads `running` is wrong for that window. Both
halves of the fix are load-bearing: ask `turn_in_flight()`, and submit the interrupt BEFORE the run
enders, because killing the command hands the round straight back to the engine.

`crates/harnessd/tests/oneshot_sigint.rs` measures it end to end: a real `harnessd --prompt` child, a
loopback fake provider, `kill -INT`, and the store's transcript read back afterwards.

A one-shot must also serve no queue: one held the merge-queue leases of six repositories until the
next daemon took them over. `cli::run` sets `cfg.serve_queues = false` for a run of steps.
