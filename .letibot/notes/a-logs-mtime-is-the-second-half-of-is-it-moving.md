<!-- abstract: A gate step streams into a log, so the log's mtime is when its last byte landed - which makes "is the tail moving" a one-read fact for a one-shot `queue status`. -->

# A log's mtime is the second half of *is it moving*, so a one-shot status can say it

The head draws every five seconds, so it can say *the tail moved 12s ago* from two answers. A `queue status` call has one, and the same sentence from one answer would be a guess.

The gate streams a step into a log (`gate::watched()` to `Running.log`), so the file's mtime is when the last byte landed and its length is how much. `RunningGate.wrote_ms: Option<u64>` carries it, and `mergequeue::desk::gate_words` words it in the pane's bands: under `TAIL_MOVING_MS` (60 s) is `tail moving (36 KiB)`, under `TAIL_STILL_MS` (10 min) is `tail moved 5m00s ago (36 KiB)`, past it is `tail still for 11m40s - it may be stuck`.

`Option` is the honest shape: an unreadable log is not a stall, so the line gives the bytes and withholds the verdict. The clause belongs to the state, not the gate's registry - `running_now(h)` asks `gate::running` only when `h.state == HealthState::Gating`, so a green target says what it always said.

A fixture cannot assert the clock: `gate::watch_step` sets `started_ms = now_ms()`, and a temp path is long, so assert on the clause after `": "` rather than the line.
