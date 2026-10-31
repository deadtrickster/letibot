<!-- abstract: MEASURED 2026-11-01: a sentence that rides the todo-nag clock and carries a growing AGE cannot be part of the clock's identity (`nagged`) — it would be a new notice at every delivery and the ladder would never leave the base window; the pair (plan text, said_queue) is the fix. Plus: a re-arm re-arms from NOW, and `next_nag_at()` includes the sweep. -->

# A census that rides the nag clock cannot be in the clock's identity

MEASURED 2026-11-01, branch `agent/the-queue-counts-what-is-stuck`, adding the stuck-queue
census to `Sessions::TodoNagClock` as a second subject beside the plan.

## 1. `nagged` is the ladder's identity, and a growing number cannot be in it

`TodoNagClock::said` decides *repeat* vs *new notice* by comparing the notice's text with
`nagged`; `interval()` doubles per repeat and caps at `NAG_REPEAT_CAP` (30 min). The census's
sentence names **the age of the oldest stuck row** — which changes on every delivery. Put that
text in the identity and every delivery is "new", the ladder resets to the base window (60 s),
and the alarm becomes a metronome at the exact rate R77 was written to stop.

So the clock holds two things: `nagged: Option<String>` (the plan's sentence, unchanged) and
`said_queue: bool` (whether the census rode with it). `said` compares the PAIR. A census that has
come or gone is new; the same census with a bigger age is the same notice.

`Harness::nag_said_as` is therefore still the plan half only, and `None` is the right answer for
a project with nothing on its board and eight rows stuck in its queue.

## 2. A re-arm re-arms from NOW, so "three passes inside one interval" needs care

`rearm` sets `due = now + window` when the notice is new. So a *pass* that re-arms moves the
deadline forward — the interval is measured from the last TURN, not from the first arming. Only
after a delivery (the repeat arm) is the deadline anchored to `nagged_at`. A test that wants
"three passes inside one interval → one notice" must either not re-arm between the passes
(`deliver_due_nags` does not, when nothing is due) or measure the interval from the delivery.

## 3. `next_nag_at()` is not a probe for "is the nag armed"

It is `min(nag clocks, sweep_at)`, and `arm_sweep` runs at the start of every turn with
`SWEEP_AFTER` (2 s). So `next_nag_at().is_some()` after a nag turn is the SWEEP, not the plan
check. `sweep_due_at()` is the one that asks about the sweep alone; there is no public per-session
nag deadline at all — assert on deliveries, not on `next_nag_at`.

## 4. The counting rule had to move one level down

`desk::states_of` returns formatted `"3 failed"` strings, which is what `status` prints but not
what a census counts. `desk::counts_of` is now the unformatted half (`Vec<(&'static str, usize)>`
in `FLOW` order) and `states_of` formats it — so `mergequeue::stuck` counts through the SAME
function `status` prints through, and `stuck/tests.rs` proves it by counting the lines `status`
prints and comparing.

## The rake

**A notice whose text is its own identity cannot carry a number that moves.** When a sentence has
to be repeated on a schedule AND say something that grows, the growing part belongs in a separate
bit of state, not in the string the ladder compares.
