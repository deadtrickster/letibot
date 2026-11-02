<!-- abstract: The ctrl-v payload window's body extent is nowhere in the API, so the head reads the seams off the drawn rows; and the body is sized from the terminal's HEIGHT, so a height-only resize must invalidate the row cache. -->

# The payload window's body is read off its seams, and it is sized by the HEIGHT

`ToolRow::layout` with a window open draws `shown_rows = body_lines.min(window_rows).max(4)`: a
header, an `↑ N more lines above` seam when `page > 0`, the body, then a closing seam.
`window_rows = screen_rows - WINDOW_CHROME` (10), so "97 lines, 61 elided" means `end = 36` - a
37-row window, a 47-row terminal at page 0.

Nothing in `RowLayout` says which drawn rows are payload. `App::payload_rows_in`
(`crates/tui/src/app/scroll.rs:569`) walks the header, the `↑` seam, the body, and stops at the
first row that starts with `…` and ends with `esc closes` - a payload line that merely prints
`…` is still payload. Those records must be ABSOLUTE screen rows (`top + r` in
`record_main_hits`): `file_rows` is window-relative, and a diff card's `stop_block` cannot paper
over the mismatch the way it does there.

The body is sized from the terminal's height, so `body_window` invalidating on a width change
alone left a body drawn for the old screen and arrows clamping against a page the frame no
longer had. `compose_screen` now invalidates on `screen_rows` changing too. Any other `*_rows`
record rebuilt from `hist_lines` inherits the trap.
