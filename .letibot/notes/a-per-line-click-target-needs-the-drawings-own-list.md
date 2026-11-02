<!-- abstract: A click target that is one LINE of a multi-line row must be rebuilt by the function that drew it and refused unless its length equals the span's drawn height. -->

# A per-LINE click target needs the drawing's own list, and a height gate is what makes it safe

The conversation is drawn by `item_lines` inside the history walk (`crates/tui/src/ui/transcript/window.rs`), which records only `Span { row, at, lines }`. The click map is a second pass over the same spans (`App::file_rows_in`, `notice_rows_in`, `card_rows_in`, `task_rows_in`), because the walk holds `self` apart while it runs.

So "which child is the line at window row N about" cannot be asked of the walk, and the only safe way to re-derive it is to call the function that drew it: `session_row` (`crates/tui/src/ui/transcript/user.rs`) returns `(lines, ids)`, and `session_row_lines` and `session_row_children` are thin halves of it.

The gate: if the id list's length is not `sp.lines`, clear it and offer no switch. A list one short shifts every line after the gap onto a neighbour's child, which is a click that opens somebody else's session - silent, plausible and wrong.

`window.rs` builds a per-row `row_cfg` narrower by a payload window's reserve while the click pass reads `self.cfg`. `trim_to` only cuts, never wraps, so the narrower draw can only produce more lines, which the gate refuses. That mismatch lands on the safe side; do not fix it by trusting the counts.
