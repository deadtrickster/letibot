# The doubled marker, caught — 2026-09-26

Three frames from a 300-second tmux sample of the live head, taken once a second by
`scripts/headwatch.sh`. **113 of those 298 frames carry the doubling**, and these three show it, the
mechanism, and the target.

## What the three frames are

| file | what it shows |
|---|---|
| `1790450203.txt` | the doubling: two markers on one narration line |
| `1790450206.txt` | the same, 4 seconds later — the thinking count has grown, the call count has not |
| `1790450215.txt` | **the target**: one marker carrying both counts |

The line, quoted from each:

```
203  …the bug is visible. [1 tool call] · ctrl-t opens it [5 thinking lines] · ctrl-t opens it
206  …the bug is visible. [1 tool call] · ctrl-t opens it [46 thinking lines] · ctrl-t opens it
215  …the bug is visible. [1 tool call, 91 thinking lines] · /verbosity
```

## What the counts prove

**It is two markers, not one appended twice.** `1 tool call` is static across every frame of the
doubling while the thinking count climbs 5 → 46. An append would grow the whole marker; this grows
one of two.

So two code paths each glue a marker to the same line, and neither knows about the other:

- **the in-walk join** puts the run's marker — `[1 tool call] · ctrl-t opens it` — on the narration
  line, at the run's first row;
- **the end-of-walk join** (`live_joins`) then puts the LIVE work's marker —
  `[N thinking lines] · ctrl-t opens it` — on the same line, because that line is still
  `RowClass::Speech` by its test.

## The target, and the rule that gets there

215 is the correct rendering: one marker, both counts. Getting there means folding the live counts
into the run's marker, which `hidden_run_marker` already does — gated on `end == items.len()`. In
this sample the operator's own row is after the run, so `end < items.len()`, no fold, and both joins
fire.

**The naive widening of that gate is wrong, and a test says so.** Preferring *the newest run with
rows* breaks `the_calls_count_goes_pending_and_nothing_else_in_the_marker_does`: in that fixture the
newest run with rows is the PREVIOUS turn's, and folding the live work into it is the *"all tool call
counters are yellow now"* defect returning.

**So the rule is**: the fold belongs to the run **the live work belongs to** — the newest run *of the
current turn* — not to the newest run that happens to have rows. leticl calls it `live-here` and
computes it as the row live work rides on, which is a fact about the turn rather than about the
transcript's tail.

## Reproductions in the tree

- `a_marker_is_glued_to_a_line_once_however_many_frames_draw_it` — a guard. It passes, which is why
  it did not find this; the state it builds is not the state that does it.
- `the_work_in_flight_is_counted_by_one_marker_not_two` — `#[ignore]`d. It reproduces the same
  defect as two markers on **different** lines, which is the same two-join race in another
  configuration.

## Why an instrument, and not reading

The doubling lasts about ten seconds and is a property of the screen, not of the log: every event in
those frames is correct and in order. Only a sample of what the head DRAWS shows it, which is what
`scripts/headwatch.sh` is for.
