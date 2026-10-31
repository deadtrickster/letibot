<!-- abstract: MEASURED 2026-11-01: the head's gate detail needs TWO snapshots to say "the tail moved", but a `queue status` answer is one shot — the step's log already carries the second half, because its mtime IS when the last byte landed; so `RunningGate::wrote_ms` makes "tail moving (36 KiB)" a one-read fact. -->

# A log's mtime is the second half of *is it moving*, so a one-shot status can say it

MEASURED 2026-11-01, branch `agent/the-gate-is-visible-everywhere`, building the operator's
*"nothing about gate — gate is invisible"* / *"fans just start spinning"*: the gate's step has to be
on the `queue status` target line, one line, no click.

## The asymmetry that looked like a blocker

`crates/tui/src/ui/panes/queue.rs`'s `running_gate_lines` says *the tail moved 12s ago* from
`GateTail { text, since_ms }` — **what this head last read for this entry#step** — and its own doc
says why: *"it has not moved is a fact about two answers and never about one."* A head draws every
five seconds, so it has two. A `queue status` tool call has one, and the same sentence would have
been a guess.

## What makes one read enough

The gate streams a step into a log (`gate::watched()` → `Running.log`), so **the file's mtime is the
instant the step's last byte landed**. One `metadata()` call answers *when did it last move*; the
length answers *how much*. So `RunningGate` grew one field — `wrote_ms: Option<u64>` — and the desk
words it in three bands, the pane's own thresholds:

| how long the log has stood still | the words |
|---|---|
| < 60 s (`TAIL_MOVING_MS`) | `tail moving (36 KiB)` |
| < 10 min (`TAIL_STILL_MS`, the pane's `GATE_QUIET_MS`) | `tail moved 5m00s ago (36 KiB)` |
| ≥ 10 min | `tail still for 11m40s — it may be stuck (36 KiB)` |
| the log cannot be read (`None`) | `12 B written` — the bytes, no verdict |

**`wrote_ms: Option` is the honest shape**: a missing log is not a stall, and the line says what it
has and withholds the verdict. Same rule the pane keeps for a snapshot older than
`GATE_SNAPSHOT_STALE_MS`.

## Where it is drawn, and the three shapes it is free in

`desk.rs` has ONE `targets()` behind both the bounded default and `repository:` (which is
`detail()` → `status()` → `targets()`), so the gate clause is in both for one edit; `every()`
(`all`) is the second site, and there it is the step and the clock only — no tail, because that line
is a screenful for the whole box.

**The clause belongs to the STATE, not to the gate's registry.** `running_now(h)` asks
`gate::running` only when `h.state == HealthState::Gating`, so a target judged green says exactly
what it always said even with a step entry still lying in the process-wide `WATCHED` list. The test
proves it by moving the health row to green under a live step.

## Rakes under the tests

- **`gate::watch_step` is `pub(super)`**, i.e. visible in `mergequeue` and its descendants — so
  `desk::tests` can fixture a running gate with `gate::watch(&wt, &repo)` + `gate::watch_step(…)`
  and clear it with `drop(gate::step_guard(&wt))`. `gate::running` finds the FIRST entry for a
  repository, so a fixture needs its own repo path (a `TempDir` child is).
- **`watch_step` sets `started_ms = now_ms()`**, so an end-to-end test sees `0 ms`/`50 ms` and must
  not assert the clock; the exact `6m12s` is pinned by a unit test on `gate_words` with a `now` the
  test chooses.
- **A temp path is 60+ characters on this Mac**, so "the line is short" is a claim about the CLAUSE,
  not the line. Assert on `line.split(": ").nth(1)`.
- `all` appends ` · unserved` when no live lease serves the repository, so a test that expects
  `… · 3 queued` at the END of the line fails on a fixture with no lease.

## The edge, one line over

`Standings::gate: Option<(usize, usize)>` fills from the kept targets' `running_gate`, and the merge
column — which has ALWAYS counted `queued | gating` together — is worded `{n} gating (step S of M)`
when it is `Some`, `{n} being merged` otherwise. Same count either way: the word says the machine is
working, and the step is in parentheses so a narrow terminal loses the step and keeps the standing
(`box_edge` truncates from the right).
