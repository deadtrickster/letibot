<!-- abstract: MEASURED 2026-10-26 building protocol 52's activity: a command TAKEN off the hub queue is not work finished, and refreshing the activity at the pop made `wait` exit 0 with the previous turn's message — the window between the pop and TurnStarted must stay `delivery`, and the bridge's wait must arm on turn/calls only. -->

# A command taken off the hub queue is not a delivery finished, and refreshing there is a false *done*

MEASURED 2026-10-26, package D1 (protocol 52, `SessionEvent::ActivityChanged`), on
`crates/bridge/tests/end_to_end.rs` — the first run after wiring the bridge to the new value.

## The shape

The daemon's activity has a `Delivery` entry: *the model is owed a turn it has not taken*. It is
`Hub::work_pending()` (a queued command or an own-wake) plus the watcher set's queued and held
completions plus a monitor firing. `Hub::submit` publishes `Delivery` the moment a prompt is
queued. Then the worker takes the command — and the turn it makes has NOT started: `run_prompt`
appends the row, checks the wall, maybe compacts, and only then does the engine emit
`TurnStarted`.

First cut: `try_command` / `take_own_work_until` refreshed the activity after the pop, to clear
`Delivery` for a verb that runs no turn (`/mode`, a refusal). That made the pop **Quiet**, and the
bridge's `wait` — which armed on any busy value — answered `0 — the turn ended` with the PREVIOUS
turn's message, milliseconds after the `send`. The e2e failed at
`assert!(out.contains(THIRD_ANSWER))` with `out` = the *second* turn's answer.

## What fixed it, and both halves are load-bearing

1. **No refresh at the pop.** `work_pending` stays true through the gap, so the value is
   `Delivery` until the turn starts (`TurnStarted` publishes) or a verb that runs no turn answers
   (`Warning` is in `SessionEvent::touches_activity` — every such command ends by saying why on the
   log, and that is the escape hatch that keeps a stale `Delivery` from wedging a session).
2. **`wait` arms on `Turn`/`Calls` only**, not on any busy value. `Delivery` says a turn is OWED,
   which is not the same as one having run; what it is *for* is the quiet-from-the-start case,
   where the activity is not quiet and the wait keeps waiting instead of taking the grace path.

## The measurement to keep

The e2e went from **18.18s to 8.02s** (14 consecutive green runs) once `SETTLE` (ten seconds) was
deleted — the same assertions, the same scripted turns. One run in ~15 failed and never reproduced
in 14 tries, including under a concurrent compile; it never printed the panic, so it is unfiled.

## The flake this note could not explain, found later

That "one run in ~15 failed and never reproduced" is the daemon's leaked head, and the note
`a-registered-head-whose-hello-never-landed-lives-for-ever` has it: a `wait` that returns in
milliseconds drops its socket before the daemon has written the `Hello`, and the head stayed
registered for ever. `attached_heads` said 3 where the test wanted 2, stably, for 60 s. Fixing
that turned 2-in-6 failures into 8 green runs at 8.0 s.
