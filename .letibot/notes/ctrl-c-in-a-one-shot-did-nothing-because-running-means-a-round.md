<!-- abstract: MEASURED 2026-10-26: Registry::close interrupts only `status().running` sessions, which is the ROUND-level flag and false for the whole of a round's tool calls — so Ctrl-C in a one-shot killed the command and let the turn ask the model again; and a one-shot claimed merge-queue leases for a prompt that exits. -->

# Ctrl-C in a one-shot did nothing, because `running` means a ROUND

MEASURED 2026-10-26 on the Mac, branch `startup-truth` (package S1), from the operator's report:
a one-shot (`harnessd --prompt`, socket `oneshot-<pid>.sock`) had a turn in flight, they pressed
Ctrl-C repeatedly, the model went on running `ssh` commands, and the process took minutes.

## The signal arrives; the interrupt was never queued

`Daemon::catch_signals` → self-pipe → `Registry::close`. That path is fine, and the run enders DO
work: the transcript of a `bash {"command":"sleep 120"}` call came back

```
tool_result … "reason":"`j1` killed by the daemon stopping"
```

and then `assistant: "done"` — **the turn went on to its next round and asked the model again**.
The cause is one predicate: `close` interrupted a session only `if h.status().running`, and
`SessionStatus::running` is read off the view's `TurnState`, which the engine publishes **once per
ROUND**. It is `false` for the whole of a round's tool calls — the one window a person presses
Ctrl-C in. `Hub::turn_in_flight` is the same instant with the calls taken into account and says in
its own doc that every reader meaning *is a turn in flight* by `running` is wrong for that window.

The fix, both halves load-bearing: ask `turn_in_flight()`, and submit the interrupt **before** the
run enders (killing the command hands the round straight back to the engine, so an interrupt queued
after the kill misses the boundary it was meant for).

**Rake:** `cargo test -p letibot-harnessd --test oneshot_sigint` measures this end to end — a real
`harnessd --prompt` child, a loopback fake provider, `kill -INT`, and the store's transcript read
back afterwards. The round-level `running` flag is the trap; a test asserting on it passes for a
turn that is doing the most work.

## A one-shot must serve no queue, and it did

The same day, a one-shot held the merge-queue leases of **six** repositories under
`~/Projects/stroppy-io`; the operator's next daemon logged six *"gave up the lease of …, its
heartbeat was 90 s old and pid 28089 is not running"*. A one-shot runs steps and exits, so a lease
it claims outlives it by construction. `cli::run` now sets `cfg.serve_queues = false` for a run of
steps (the `--no-queue` of package Q1), and `mergequeue::spawn_for` says which of the two causes
took the queue away.

## Two more from the same startup output

- **The "serving nothing" line read the wrong rule.** `spawn_for` printed `repository_of_workspace`
  (SINGULAR: one checkout, or its own child) while the supervisor's pass reads
  `repositories_of_workspace` (plural: also a one-project folder of checkouts). So a daemon seated
  in `~/Projects/stroppy-io` said *"serving nothing — it may contain some"* and then examined all
  fifteen. `mergequeue::serving_line` is now the same call the pass makes.
- **`--continue s-<id>` ran the id as a PROMPT**, into the newest session — the operator's live
  Stroppy conversation, which then acted on the string. `scripts/letibot` reads an id after
  `--continue` as `--session`, says so on stderr, and refuses anything else with the usage line.
