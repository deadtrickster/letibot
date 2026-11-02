<!-- abstract: A pane's hover text is read from `pointer_at` and `plugin_legend_at`, not from `hovered`, because `clickable_at` answers only for hit targets and a row a click cannot act on never becomes one - and the example dashboard's legend keys are denominators and role words, so it feeds that seam nothing. -->

# The legend on hover: why it reads `pointer_at`, and what it cannot explain yet

`App::relight` → `clickable_at` (`crates/tui/src/app/pointer.rs:208`) answers *what a click here would
act on*, and for a pane it is `stop_block`, which walks the stop rows a frame recorded. A line with no
`id` is not a stop, so a title, a findings summary or the first line of a panel with no ids sits in no
block and gets `hovered = None` - and that is exactly the row a reader points at. So the hover text is
read from `pointer_at` and `plugin_legend_at` (`crates/tui/src/app/mod.rs:187`), never from `hovered`.
Adding a `Hit` per legend row instead would give each of them a hand pointer, i.e. *clicking this does
something*, and a legend row is documentation.

The seam is unanswered by the one real dashboard: `legend()` in
`plugins/examples/plan-dashboard/src/lib.rs:551` keys on `done/total`, on role words and on each
panel's denominator, and the hover matches a line's label or its first cell exactly. Either the example
documents its labels, or the hover also answers for a denominator on the panel's title row.
