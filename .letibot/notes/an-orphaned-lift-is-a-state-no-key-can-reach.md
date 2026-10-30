<!-- abstract: MEASURED 2026-10-30 (branch agent/a-click-that-cannot-be-undone): the thinking's openness had four carriers and five doors; `set_verbosity` cleared `payload_sel` and left the lift pair standing (an orphan no key could reach — the latch), `carry_live_open` re-applied the pane's half at every round's end (the oscillation), and the review found the rest: an open run's marker must stay drawn (it is the toggle), `run_open_at` answered the LIVE_RUN sentinel row-independently, and `ctrl-r` at a bottom rung could lift a calls-only run into a blank. -->

# An orphaned lift is a state no key can reach — and four more things the review of it found

MEASURED 2026-10-30, branch `agent/a-click-that-cannot-be-undone` (`crates/tui`), from the
operator's *"sometimes when i click on thinking counter it gets stuck … then all subsequent turns go
with think open. ctrl-r doesnt help, esc too"*, then *"sometimes it latches, sometimes oscillates"*.

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
is no run to lift and it answered *"nothing is folded to open"*.

## What the review found, and each is a rule worth keeping

1. **An open run's marker must stay drawn.** Both walks asked `unseen_run_at` only when the run was
   CLOSED (`let unseen = if open_run { None } else { … }`), so a click made the counts line vanish
   and the only thing naming the state was the hint bar. The marker is the run's HEADER and the
   toggle: **the hit record a click is tested against comes from the drawn frame**
   (`marker_rows_in` reads `hist_lines`), so a suppressed marker is a state no click can toggle.
   The forward walk pushes the marker then the row's lines in one group; the backward walk's join
   arm used to `continue` (dropping the body) and now pushes `rows[1..]`.
2. **`run_open_at` answered the `LIVE_RUN` sentinel with a value that had no `row` in it** —
   `live.work() > 0 && !live_tail_covered(..)` — so a panel on the pane's own marker made EVERY
   row read as *inside the open run*: the whole conversation at `Visibility::lifted()`, and
   `run_skips` applied to rows the click was never about (a `thinking` click hid every tool row in
   the session). A row is never the live tail: the walks get `false`, and `App::run_is_open` — which
   asks *is the panel open* — answers the sentinel itself.
3. **A key that opens the thinking must ask whether the run holds any.** `run_skips(Some(Thinking))`
   hides every `ToolResult` and every empty-text `Assistant` row, so lifting a calls-only run hid
   the calls it held and drew nothing: `[3 tool calls]` gone, the run blank. `run_holds_thinking`
   checks the rows (a reasoning row with no words is not thinking — the marker counts display
   lines) and the press says a sentence naming `/verbosity normal` instead.
4. **A state reset must not rest on a heuristic.** `TurnStarted` is one event per ROUND, and
   `same_turn`'s `began_ms: None` arm is *the previous pane reads Running* — so a wake turn
   (`Sessions::wake` stamps no clock) over a running turn kept the lift. The pane's half now goes on
   every `TurnStarted` unconditionally; a panel on a run of ROWS is the reader's and only a new
   prompt closes it.

## Two smaller facts worth not rediscovering

* **`Editor::key(Esc)` answers `Reaction::Changed`, not `Idle`** — the first press of the
  interrupt pair. An arm written in `Reaction::Idle` never fires for Esc, and it has to sit AFTER
  the composer or `esc esc` can no longer interrupt while the thinking is open.
* **`item_lines` returns `(RowClass, Vec<String>)`, not `Vec<String>`** — a `let body = if … {
  item_lines(..) } else { Vec::new() }` does not typecheck, and the class it returns is what the
  blank-line rule reads. And a test that locates a marker by its words needs `find_last` once two
  markers on one frame say `1 thinking line`.

## How each claim was proved

The field rename makes the new tests uncompilable on the base tree, so the proof is a revert of one
hunk at a time: drop `on_turn_event`'s reset → `a_click_does_not_follow_into_the_next_turn` red;
restore `if open_run { None }` in the walks → `an_open_runs_marker_is_drawn_over_the_rows_it_lifted`,
`a_second_click_on_the_same_clause_puts_it_back` and
`a_click_that_opens_a_run_leaves_no_light_on_what_it_opened` red; drop `run_holds_thinking` →
`ctrl_r_will_not_open_a_run_that_holds_no_thinking` red; key the reset on `!same_turn` alone →
`a_new_round_drops_the_panes_lift_without_a_clock` red.

## UPDATE 2026-10-30 (same day): what the *second* review found

* **`run_open_at`'s LIVE_RUN arm and `App::run_is_open` are one question each, and they must not be
  the same call.** The walks ask *is this ROW of the open run* (answer: no, a row is never the live
  tail — answering `live.work() > 0 && !live_tail_covered(..)` made every row read as inside the
  open run). `run_is_open` asks *is the panel open*, and routing it through the per-row function
  made it **false** for a click on the pane's own counter: Up/Down fell through to `page_payload`,
  `payload_window_open()` said yes so the page keys and the wheel paged a window nobody draws, and
  the hint bar named `page the result` for it. `run_is_open` answers the sentinel itself.
  `a_panel_on_the_live_counter_scrolls_the_conversation` is the test — and note the assertion: a
  scroll key holds a ROW and `scroll` is recomputed at the next draw, so **`following()` is the
  honest predicate**, not `scroll > 0`.
* **`markers::LIVE_RUN` and `runclick::LIVE_RUN` were two constants with one name and different
  values** (`"<live>"` and `"\u{0}live"`). `ctrl-t` stored the first, a click the second, and
  `run_open_at`/`run_is_open` recognised only the first — so a click-opened panel was not a run at
  all. One constant now, and `markers` re-exports `app::runclick`'s.
* **Esc on a ctrl-v window must not fold the thinking.** The arm ended in `set_thinking(false)`,
  which at a rung that draws the thinking goes through `set_verbosity` and **rewrites `head.toml`**
  — a persisted side effect of a key pressed for something else. The arm closes its panel; the
  fold's Esc is the general arm one press further on.
