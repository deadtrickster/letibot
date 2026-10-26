<!-- abstract: MEASURED 2026-10-26: the bridge's "a session that has never run a turn" flake was NOT one session's activity leaking into another — `run_turn` started the prompt clock before the door's own act, so a wake with nothing to say (the daemon's startup ring) published `waiting on: turn` then `quiet`, which is exactly the pair `wait` arms on and answers 0 from. -->

# An empty wake published a turn that never ran

MEASURED 2026-10-26, branch `activity-leak` (package F1), `crates/bridge/tests/end_to_end.rs`
failing about one run in four at *"a session that has never run a turn times out rather than
reporting one ended"*.

## What it was not

Not a leak between sessions. The orchestrator's guess (a shared `prompt_in_flight`, a daemon-wide
fact, an `ActivityChanged` on the wrong hub) is wrong on every count: `Harness::open` builds its own
`prompt_in_flight: Arc<AtomicBool>`, its own `Monitors`, its own `JobWatchers` and its own board, and
`Hub` is per session. Proven by printing `Arc::as_ptr(&h.prompt_in_flight)` at install and at
`begin_turn_clock`: the two sessions' addresses differ, and the idle session's own closure was called
with its own hub and its own flag.

## What it was

`turn_tail::run_turn` called `h.begin_turn_clock(now)` **before** the door's own act — i.e. before it
was known whether the door had anything to say. `Harness::wake`'s gathering then answered `Ok(None)`
(nothing to say) and `run_turn` called `end_turn_clock`. Between those two calls the hub published
`waiting on: turn`, and at the end it published `quiet`:

```
PROBE run_turn door=wake session=mcp-daemon-session
PROBE begin_turn_clock hub=mcp-daemon-session      <- Busy{[Turn]} published here
PROBE own (mcp-daemon-session): activity=Busy { waiting_on: [Turn] }
PROBE end_turn_clock hub=mcp-daemon-session        <- Quiet published here
```

`Head::wait` arms on `Turn`/`Calls` and answers `TurnEnded` on the next quiet, so it exited 0 with an
empty message for a session that had never turned. The wake itself is ordinary: the daemon's own
startup ring (`Sessions::put_a_restored_session_to_work` → `bell.ring_wake`) is drained by the
worker's first pass, and on a session with nothing restored it says nothing.

## The fix, and the shape worth keeping

- The clock starts where a turn **speaks**: `Harness::submit_items_then` (before the rows it appends,
  so the append's own publish already carries `turn`) and `Harness::run_rounds` (the operator's `!`
  line, whose rows are already in). A door that says nothing never reaches either.
- `Harness::begin_turn_clock` is **idempotent within one prompt** (a wall-continuation re-enters
  `submit_items_then`), and it refuses to start a clock unless the tail has **taken** a turn
  (`Harness::turn_taken`, set by `run_turn`, cleared by `end_turn_clock`). That second gate is not
  decoration: `Harness::system_update` → `submit_item` → `run_rounds` runs **outside** any turn (a
  compaction's fabric block, `/compact`, the `m1` script) and has no `end_turn_clock` behind it — a
  clock started there would leave the session reading `waiting on: turn` for ever.

## Two measurement traps

- **libtest eats the daemon's stderr on a green run.** The daemon and the harness live in the test
  process, so `eprintln!` probes from `harnessd` only appear with `--nocapture` — and the failing run
  shows them under the panic banner, which reads as "only failures print". Three probe rounds were
  spent reading silence.
- `git stash push <two src files>` is the cheap way to prove a new test fails on the parent commit
  while keeping the test in the tree: it failed with `left: ["waiting on: turn", "quiet"]`,
  `right: []`.
