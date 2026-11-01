<!-- abstract: MEASURED 2026-11-01: the history walk and the click walk are two passes, so a click target that is one ROW of a multi-line notice row must carry the ids out of the same function that drew the lines and be refused unless its length equals the span's drawn height — and the draw's width is NOT always `self.cfg`'s (`row_cfg` narrows it while a payload window is open), which the height gate absorbs in the safe direction. -->

# A per-LINE click target needs the drawing's own list, and a height gate is what makes it safe

**MEASURED 2026-11-01**, branch `agent/click-the-card-read-the-subagent`, making a finished
subagent's line in a `[task]` notice a door to that child's session (`App::task_rows` /
`Target::TaskAgent`).

## The two passes

A row of the conversation is drawn by `item_lines` inside the history walk
(`ui/transcript/window.rs`), which records only `Span { row, at, lines }` — where an item's lines
begin and how many there are. The click map is built afterwards, in a second pass over the same
spans (`App::file_rows_in`, `notice_rows_in`, `card_rows_in`, `picture_rows_in`, and now
`task_rows_in`), because the walk holds `self` apart while it runs.

So "which child is the line at window row N about" cannot be asked of the walk. It is re-derived,
and the only safe way to re-derive it is to call **the same function that drew it**:
`session_row` in `ui/transcript/user.rs` now returns `(lines, ids)` and both `session_row_lines`
and `session_row_children` are thin halves of it. Two functions computing "which line is which"
would drift the day either changed.

## The gate that makes the re-derivation safe

`sp.lines` is the authority on how many lines the row was drawn in. If the id list's length is not
`sp.lines`, **clear it** — no switch at all — rather than indexing into it. A list one short shifts
every line after the gap onto a neighbour's child, which is a click that opens somebody else's
session: silent, plausible, and wrong. The safe failure is the popup the row had before.

## And the width is not always `self.cfg`'s

`window.rs` builds `row_cfg` per row: `RenderConfig { width: cfg.width - reserve, .. }` when a
payload window is open (`reserve` is its room). The click pass reads `self.cfg`. So while a payload
window is open the two passes can measure different widths — and `trim_to` only cuts, never wraps,
so a narrower draw can only produce **more** lines, which the height gate refuses. The mismatch is
one-directional and lands on the safe side; do not "fix" it by trusting the counts.

## The rake

**A `*_rows` record that is per LINE of a multi-line row must be built from the drawing, and
checked against the drawing's height.** A record that is per ROW of the item (as `notice_rows` is —
every line of a notice row opens the same popup) does not need either, which is why this problem
appears only when two lines of one row mean two different things.
