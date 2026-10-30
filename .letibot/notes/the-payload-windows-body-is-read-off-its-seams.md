<!-- abstract: MEASURED 2026-10-30: the ctrl-v payload window's body extent is nowhere in the API — the head reads rano's seams off the drawn rows to map a screen row to a payload line; the body is sized from the terminal HEIGHT, so a height-only resize left a stale body and a stale clamp; and "97 lines, 61 elided" decodes to a 47-row terminal at page 0. -->

# The payload window's body is read off its seams, and it is sized by the HEIGHT

MEASURED 2026-10-30, branch `agent/a-viewport-that-cannot-scroll`, from the operator's
*"it has 97 lines, 61 line elided and no way for me to scroll that 'viewport' like it doesn
respond to the mouse"*.

## The report's numbers decode to the window's own arithmetic

`ToolRow::layout` (`rano::agent::tool_row`) with a window open draws `shown_rows =
body_lines.min(window_rows).max(4)` rows: one header, one `↑ N more lines above` seam when
`page > 0`, the body, then one seam (`… +N lines · ↓ pages down · esc closes` or
`… end of output · esc closes`). `window_rows = screen_rows − WINDOW_CHROME (10)`. So
**97 total and 61 elided means `end = 36` — a 37-row window, i.e. a 47-row terminal, at page
0.** That is how to reproduce a report like this one: `a.screen(100, 47)`.

## The body's extent is nowhere in the API

`RowLayout` gives `lines` and `max_page` and nothing that says *which of these rows are
payload*. A head that wants `(screen row → payload line)` — for a click — has to read the
drawing: the header is the row's first line, the `↑` seam is the next when the page is past
the head, the body follows, and the first row that starts with `…` **and ends with
`esc closes`** ends it (a payload line that merely prints a `…` is still a payload line).
`App::payload_rows_in` is that walk, and it is the sibling of `file_rows_in` /
`picture_rows_in` over `spans`.

**And these records must be ABSOLUTE screen rows.** Every body row is adjacent to the next,
so `stop_block`'s "extend the block to the next stop" cannot paper over a relative/absolute
mismatch the way it does for a diff's card. `file_rows` is window-relative and `key_click`
compares stops against an absolute `y` — do not copy that; `payload_rows` is recorded as
`top + r` in `record_main_hits`.

## A height-only resize leaves the window stale, and nothing said so

`body_window` invalidates the transcript's row cache on a **width** change only. The window's
body is sized from the terminal's **height**, so a resize in height alone kept the row drawn
at the old screen's size: measured, a 36-line body on a 24-row frame, with `payload_max`
still 62 where the new frame's last full page is 85 — the arrows then clamped against a page
the frame no longer had. `compose_screen` now invalidates on `screen_rows` changing too.

## What the window honours (so the next report can be placed)

`ctrl-v` opens `App::newest_payload_row()` — the newest result with more than two lines — at
page 0 (`App::open_payload_window`, one writer for both doors). It honours ↑↓ (10), PgUp/PgDn
(`screen_rows/2 + 1`), the wheel (3, through `wheel_at` → `page_payload`, positional over the
conversation or not), Home/End, Esc, and now a click on a body line (that line becomes the
window's first). The header and both seams carry no click stop, so a click on *"61 elided"*
acts on nothing. A drag over the window is the pointer's own cell selection and copies — it
was never a per-row handler, which is why it already worked.

## The rake

**A window whose body is a function of the screen's height is a row the height must
invalidate.** Any other `*_rows` record that is rebuilt from `hist_lines` inherits the same
trap. And a brief that quotes "N lines, M elided" is quoting `total` and `total − end`:
subtract to get the window's height before believing anything about the terminal.
