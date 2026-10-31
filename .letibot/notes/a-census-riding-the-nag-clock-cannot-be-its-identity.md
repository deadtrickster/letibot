<!-- abstract: MEASURED 2026-11-01: a scheduled notice whose text carries a growing number must be paced by its OWN deadline, not by comparing texts (every delivery is "new") and not by another subject's ladder; plus a re-arm re-arms from NOW, `next_nag_at()` includes the sweep, and `merge_rows(Some(repo))` measured 27.5 µs against 434.5 µs unscoped. -->

# A notice that carries a growing number gets its own deadline

MEASURED 2026-11-01, branch `agent/the-queue-counts-what-is-stuck` (twice: the first cut was
refused by the review and this is the correction).

## 1. `nagged` is a TEXT identity, and a growing number cannot be in it

`TodoNagClock::said` decides *repeat* vs *new notice* by comparing the notice's text with
`nagged`; `interval()` doubles per repeat and caps at `NAG_REPEAT_CAP` (30 min). A sentence that
names **the age of the oldest stuck row** changes on every delivery. Put it in `nagged` and every
delivery is "new", the ladder resets to the base window (60 s), and an alarm becomes a metronome at
exactly the rate R77 was written to stop.

The first cut worked around this with a `said_queue: bool` beside `nagged` and made the census ride
the plan-check's TURN. **Both halves were wrong**: the ride cost a model call per notice (about
twenty over a night for one stuck set — the brief says "costs no model call", bold), and the
paired-identity trick was only needed *because* the census had no schedule of its own.

What is right: **a separate deadline per subject.** `TodoNagClock` now holds `due` (the plan's)
and `census_due` (the queue's), each with its own anchor (`nagged_at`, `census_said_at`), its own
interval (`window`×2^n, `repeat`), and its own delivery door. The text is then free to say
whatever the store says, because nothing compares it.

## 2. The no-turn notice door already existed

`Harness::note_to_model` → `record_notice` (`harness.rs`, `pub(crate)`) appends one
`Speaker::Agent` transcript row and returns. Its other caller is `Sessions::plugin_tick`, which is
the shape to copy: walk the sessions, take the queued sentences, write them, no round.
`Sessions::note_due_censuses` is that, on the idle pass beside `deliver_due_nags`. A test proves
"no model call" with `SessionEvent::TurnStarted` — zero — rather than by timing.

## 3. A re-arm re-arms from NOW, so "three passes inside one interval" needs care

`rearm` sets `due = now + window` when the notice is new. So a *pass* that re-arms moves the
deadline forward; the interval is measured from the last TURN. Only after a delivery (the repeat
arm) is the deadline anchored to `nagged_at`. A test that wants "three passes inside one interval
→ one notice" must either not re-arm between the passes (`deliver_due_nags` does not, when nothing
is due) or measure from the delivery.

## 4. `next_nag_at()` is not a probe for "is the nag armed"

It is `min(nag clocks, census clocks, sweep_at)`, and `arm_sweep` runs at the start of every turn
with `SWEEP_AFTER` (2 s). So `next_nag_at().is_some()` after a nag turn is the SWEEP. Assert on
deliveries, not on `next_nag_at`.

## 5. What a scoped read costs, measured

`stuck::of` reads `merge_rows(Some(repo))` per repository, not `merge_rows(None)`. On a store of
420 rows over 21 repositories: **scoped 27.5 µs a call, unscoped 434.5 µs** (release, 200
iterations each, warmed). The unscoped figure is an under-estimate — the fixture's `evidence` is
short and a real row's is up to ~7.8 KB, all of it materialised by the unscoped form.

27 µs is fine on a per-turn door, but the read is a table scan: `merge_queue_state_idx` is
`(state, priority, created_ms)`, so `WHERE repository = ?` uses no index. An index on
`merge_queue(repository)` is the obvious next step if this ever shows up in a profile.

## 6. Two counting rules drift unless a test prints both

`desk::states_of` returns formatted `"3 failed"` strings, which is what `status` prints but not
what a census counts. `desk::counts_of` is now the unformatted half (`Vec<(&'static str, usize)>`
in `FLOW` order) and `states_of` formats it — so `mergequeue::stuck` counts through the SAME
function `status` prints through. The agreement test counts the lines `status` prints and compares
against the census's numbers; that is the only kind of test that catches a second rule.

## The rake

**A scheduled sentence must not be paced by its own wording, and a notice must not ride a turn.**
When a sentence has to repeat on a clock AND say something that grows, give it a deadline and a
rate of its own — the text is the payload, never the schedule.
