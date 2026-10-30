<!-- abstract: MEASURED 2026-10-30 (branch agent/a-click-that-cannot-be-undone): the thinking's openness had four carriers and five doors; `set_verbosity` cleared `payload_sel` and left the lift pair standing (an orphan no key could reach — the latch), and `carry_live_open` re-applied the pane's half at every round's end (the oscillation); also `thinking=folded` at `conversation` is `normal`, so you cannot close the thinking by writing the fold at a bottom rung. -->

# An orphaned lift is a state no key can reach — and the fold at a bottom rung opens rather than closes

MEASURED 2026-10-30, branch `agent/a-click-that-cannot-be-undone` (`crates/tui`), from the
operator's *"sometimes when i click on thinking counter it gets stuck … then all subsequent turns go
with think open. ctrl-r doesnt help, esc too"* and then *"sometimes it latches, sometimes
oscillates"*.

## The shape: N carriers, and the doors that empty one of them

"The thinking is open" was stored in four places — `App::reasoning` (the fold),
`visibility.thinking` (the ladder's switch), and a PAIR `run_show` (a run of rows) +
`live_open` (the live pane). Two doors nobody pressed broke it:

* **`set_verbosity` emptied `payload_sel` and neither of the pair.** The panel is the lift's only
  exit (`esc folds the run back` is drawn off `payload_sel`, and `key_payload_window`'s Esc arm
  requires it), so `ctrl-r` — the key a reader reaches for — turned a click's state into an
  **orphan**: still drawn (the pane's `lifted` was read unconditionally), unreachable by any key.
  That is the latch.
* **`carry_live_open` re-applied `live_open` at every round's end**, so the next round's pane was
  forced open too. Seen from outside: open while a round's thinking streams, gone the moment it
  lands as a row, once per round. That is the oscillation. One defect, two symptoms — the
  operator's own read (*"there is a state switch inside"*) was right.

The fix is one field (`App::lift`) read against `payload_sel` (`App::lifted`), two writers
(`set_lift`/`drop_lift`, which take the panel with the switch), one reader of the answer
(`thinking_open`) and one writer (`set_thinking`), with `flip_thinking` as the verb every door calls.

## The trap that cost a design pass: `thinking=folded` at a bottom rung is `normal`

At `conversation` and `read-edits` the reasoning rows are HIDDEN, so the fold draws nothing — and
writing it there moves the RUNG, not the thinking: `Visibility::rung()` is *the highest switch that
is showing*, so `{thinking: folded}` over `conversation` is `normal` and the whole working appears.
So "close the thinking" cannot be spelled as the fold at a hiding rung (an `esc` would *raise* the
rung), and "open the thinking" there is only the run lift. The branch that picks a spelling must be
`visibility.hides_the_working()`, **not** `!shows(Thinking)` — `terse` hides the thinking yet draws
its rows, and a `!shows(Thinking)` test sent `ctrl-r` at `terse` down the lift branch, where there
is no run to lift and it answered *"nothing is folded to open"*. A key that works at `normal` and
dies one rung below it is the defect being fixed, one level up.

## Two smaller facts worth not rediscovering

* **`Editor::key(Esc)` answers `Reaction::Changed`, not `Idle`** — the first press of the
  interrupt pair. An arm written in `Reaction::Idle` never fires for Esc. And the arm has to sit
  AFTER the composer has seen the key, or `esc esc` can no longer interrupt while the thinking is
  open (the same defect `esc goes up` in a subagent records).
* **A run whose marker counts live calls needs the pane's half of the lift as well.** The panel on
  such a run is empty of the kind that was clicked (the calls are not rows yet) — the operator's
  *"clicked on the tools counter — empty panel"*. So `lifted()` (the pane) is true when
  `payload_sel` is `LIVE_RUN` **or** the named run is the one the in-flight work will join
  (`run_carries_live_now`), and the expensive question is only asked while a lift is set.

## How the latch was proved, not argued

The field rename makes the new tests uncompilable on the old tree, so the proof is a revert of one
hunk: drop `on_turn_event`'s `if !same_turn { self.drop_lift() }` and
`a_click_does_not_follow_into_the_next_turn` goes red (13 markerclick tests, 12 green). The
oscillation test (`a_round_boundary_does_not_touch_the_thinking`) passes either way when no click has
been made, which is the honest limit of it: it asserts the *state* is untouched by a boundary, not
that the lines are identical — a round's content differs, so the counts differ.
