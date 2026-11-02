<!-- abstract: MEASURED 2026-11-01: an absence assertion that collects up to a barrier frame can never fail (the defect's frame is the next one), proved by mutating `TermClose` back into `App::pane_keys` — 4 passed/3 failed with the old assertions and the two absence tests among the passes; plus `cpu::of_pid`'s None-means-macOS contract and why `apparatus::present` is the wrong door for it. -->

# A barrier frame is not a window, and an absence assertion that stops at one has no teeth

MEASURED 2026-11-01, branch `agent/the-absence-still-has-teeth` (`main` at `480d9e6f`), from a
review of `agent/measure-the-child-not-the-box` after it landed.

## The shape

`crates/tui/tests/term_way_out.rs` proves *a way-out byte ends nothing* by collecting the frames
the head sent and asserting no `TermClose` is among them. The collection was `frames_until(…,
|fs| fs.iter().any(|f| matches!(f, ClientFrame::TermOpen { .. })))` — and `frames_until` returns
**the moment its predicate holds**.

`Link::tick` builds ONE `actions` vector in tick order: `App::key` pushes the Enter's
`Action::TermOpen`, then `App::pane_keys` appends the way-out's actions to the same vector, and the
writes happen in that order in the same tick. So the frame a defect would emit is always the NEXT
one, and a collection that stopped at the barrier never saw it. A unix socket being read in order
makes the barrier exact only for what was written **in front of** it.

## The mutation proof, which is the only thing that settles it

Mutate the defect back into `App::pane_keys`'s detach arm:

```rust
let pane = self.term.as_ref().and_then(|p| p.pane).unwrap_or(1);
self.detach();
out.push(Action::TermClose { pane });   // the way-out ends the program, as it used to
```

* with the OLD assertions: `term_way_out` **4 passed, 3 failed** — and the two tests whose subject
  is an absence were among the PASSES (the three reds were the ones already using
  `arrived_meanwhile`). That is the measurement of *coverage lost*.
* with the fix (`frames_until_and_after` = `frames_until` + `arrived_meanwhile`): both red, and the
  failure prints the shape — `[TermOpen { line: "!term nano", … }, TermClose { pane: 1 }]` and
  `[TermResize { … }, TermInput { pane: 1, bytes: [13] }, TermClose { pane: 1 }]`.
* mutation reverted: 7 passed in 4.05 s, the same wall clock as before — the two new 2 s windows
  run beside the two `the_deliberate_close…` already waited.

## The rule

**A positive frame makes an absence exact only for what was written before it.** If the claim is
*and nothing else happened*, the instrument is a window AFTER the barrier (`arrived_meanwhile`,
2 s) — never the collection the barrier stopped at.

Rake under it: a claim whose instrument is `clear()` (`try_recv`, no wait) has the same hole from
the other side — `a_way_out_byte_in_a_read_the_composer_consumed_is_not_a_way_out_of_anything`
asserts `!clear(&frames)` has no `TermOpen`, which can only see a frame the daemon thread has
already put on the channel. It is saved by a later net (the `submitted` collection is strictly
later and would hold it), not by its own instrument.

## And the sibling: `Option` from a probe is not a failure

`crates/bridge/tests/support/cpu.rs`'s `of_pid` answers `None` where there is no `/proc` (macOS)
*"so a caller can say the reading is not available on this box instead of failing on it"*.
`idle_clock` part 2 took the reading with `unwrap_or_default()` and then asserted the burner had
advanced — so on Darwin it waited the full 30 s and failed with *the burner never got going*: a red
that reads as a defect in the instrument. Part 1 of the same file had the `else`.

The fix is `Option<bool>` out of the helper and the split at the call site, where the platform is
known (`None if !cfg!(target_os = "linux")` → say it; `None` → red, a pid with no reading on a box
that has `/proc` is a shell already gone). `apparatus::present` is the WRONG door: it panics under
`LETIBOT_REQUIRE_APPARATUS=1`, which is the gate's own test step, and `/proc` is a facility macOS
can never be given — `applies`'s doc says that absence is not missing apparatus.

## The same measurement had three ranges in three places

`no_spin`'s six windows were quoted as **0.57–1.49 ms** in `cpu.rs`'s `settle_budget` doc,
**0.6–4.0 ms** in `idle_clock`'s module doc, and **2.0–2.4 ms quiet / 0.76–2.1 ms loaded** in the
commit that measured them — plus the "twenty times below the line" that only the first range
supports. Re-measured twice on lab2x1 at load 28: **1.87–2.51** and **1.55–3.72 ms**. So the honest
sentence is 0.76–3.72 ms across the runs on record, each with its box beside it, and "two and a
half times under the line" at the loudest.

A range is a claim like any other, and three copies of one drift apart in a way nobody notices
because each copy is plausible on its own. The unit matters too: the settle measures `SPAN` (1 s,
one `over()` call), never a run of 250 ms slices — `cpu::SLICE` said otherwise and existed only as
the wrong unit interpolated into a panic message.
