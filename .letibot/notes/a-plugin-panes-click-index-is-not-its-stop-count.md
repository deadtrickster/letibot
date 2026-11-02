<!-- abstract: A plugin pane's click index is its STOP count, and `content()` recorded one per item, so on the queue pane a click addressed a stop the cursor does not have. -->

# A plugin pane's click index is its STOP count, and the drawing recorded one per item

`content()` (`crates/tui/src/ui/panes/plugin.rs`) called `out.push_stop(first)` for every `Walk::Item` it drew, but `Item::stop()` answers `Some` only for a line or row with an `id`. So `PaneLines::stop_rows`, which `App::pane_stops` turns into the click's `(screen row → index)` map, was longer than `stops(view, screen)`, which is what the cursor indexes.

One review row and one gating landing at 120×30: `stop_rows = [3, 6, 9]` against two cursor stops. A click on the landing resolved past the end, so `target_of` was `None`, the selection was `None`, and Enter fell through to `Continue`.

The host pane recorded its own stops one per row, so the two counts agreed there; only a pane drawn by a LAYER with lines above its tree was shifted, by one per line that is not a stop. The fix is `if it.stop().is_some() { out.push_stop(first) } else { out.push(first) }`, and for the queue a click now opens the row rather than only moving the mark.

Rake: two records of where a row is must be built by one predicate. The mark stays right and only a click is wrong, so nothing notices - and a click past the end moves the cursor to a stop that is not there, so the next key does nothing either.
