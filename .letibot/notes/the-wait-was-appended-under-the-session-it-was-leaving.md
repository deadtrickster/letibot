<!-- abstract: MEASURED 2026-11-01: `body_window` pushed the attach cat AFTER the whole history, so entering a subagent drew a full screen of the session being left; the test that was green over it attached to an EMPTY snapshot, where "replace" and "append under" draw the same screen. Plus `main_area` truncates the body's TAIL and `cfg.width` is not the body's width. -->

# The switch's wait was appended UNDER the conversation it was leaving

MEASURED 2026-11-01, branch `agent/the-pane-shows-where-you-are-going` (`crates/tui`), from the
operator on the build that already carried `a080c7ec`/`33a48e93`: *"on this build I 'Entered'
sub agents and still see you — main session"*.

## The state was set on every path; the DRAW was the defect

`switch_to` (picker row, subagents pane, `esc` up to the parent, a queue row, a plugin act) has two
arms — `Action::Switch` for a live session, `Action::ResumeSession` for one the daemon does not hold
— and BOTH call `wait_for_session` → `fetching = Some(id)` + `begin_attach()`. So `attaching` was
true, the status row said `switching to <label>…`, and the cat was drawn.

What `body_window` did was `segs.push(cat)` **after** the history walk and the live pane. So the
pane was every row of the session being left, with a spinner under it — measured at 100×24 with two
rows in the parent: `PARENT-ANSWER` on row 1 and the cat in the middle. On this box a switch is
seconds, so that is seconds of the wrong conversation presented as the answer. The fix is a
`switch_wait` that REPLACES the body (plus `App::leaving()`, the predicate both readers share:
`switching_to` for the status row, `body_window` for the pane).

## Why the test was green over it: an empty fixture cannot tell "replace" from "append under"

`entering_a_subagent_says_that_a_switch_is_in_flight` attaches with `Hub::new("s").snapshot()` — no
rows. With no rows, the two behaviours draw the SAME screen, and the test asserts the cat is there,
which is true either way. A test of *what a state replaces* needs something on the screen to
replace: the new test puts a `PARENT-QUESTION`/`PARENT-ANSWER` pair there first and asserts they are
gone. (Rake: when a test of a new state passes first time, ask what the fixture had in it.)

## Three layout facts under it

* **`main_area` truncates the body's TAIL** (`out.truncate(room)`), while `body_window`'s own
  `take_window` keeps the last `room` lines of the history. Anything APPENDED below the history is
  therefore the first thing lost: a 19-row body in an 18-row pane dropped the second wrapped line of
  the sentence, silently. `switch_wait` drains its own leading blanks instead.
* **`cfg.width` is the FRAME's width, not the body's** — 96 at a 100-column terminal, and the body is
  drawn two columns in (the gutter). `system_row(…, cfg.width)` clips the last words rather than
  folding them; pass `cfg.width - 2`.
* `rano::agent::screens::attaching(elapsed, room, w)` pads itself with `room / 2` blanks above the
  cat, so it is `room/2 + 7` rows, not `room` — a centring that stops holding the moment anything is
  drawn after it.

## What was deliberately NOT changed

The header still names the session being LEFT (`session_label(&self.session_id)`), because that is
where the head is still seated until the `Hello` — the status row and the `system (switch)` row are
what say where it is going. A reviewer who wants the header to move too should know it was a choice.
