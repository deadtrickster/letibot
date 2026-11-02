<!-- abstract: The switch's wait was pushed after the whole history, so entering a subagent drew a full screen of the session being left; and the test was green because an empty snapshot cannot tell replace from append under. -->

# The switch's wait was appended UNDER the conversation it was leaving

`switch_to` sets `fetching` and `attaching` on both arms, so the state was right and the DRAW
was wrong: `body_window` pushed the cat after the history walk, so the pane was every row of the
session being left with a spinner under it. `switch_wait` replaces the body, and
`App::leaving()` is the predicate both readers share (`switching_to` for the status row,
`body_window` for the pane).

A test of what a state replaces needs something on the screen to replace: the old test attached
to an empty snapshot, where the two behaviours draw the same screen.

`main_area` truncates the body's TAIL while `body_window` keeps the last `room` lines of
history, so anything appended below is the first thing lost. `cfg.width` is the FRAME's width -
the body is drawn two columns in, so pass `cfg.width - 2` or the last words are clipped instead
of folded. And `rano::agent::screens::attaching` pads `room / 2` blanks, so it is `room/2 + 7`
rows, not `room`.

The header still names the session being LEFT, deliberately: the head is seated there until the
`Hello`.
