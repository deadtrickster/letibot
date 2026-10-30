<!-- abstract: MEASURED 2026-10-30: `ui::panes::plugin::content` recorded a stop row for every ITEM (including the chrome lines), while the cursor counts only `Item::stop`s — so on the queue pane a click addressed a stop the cursor does not have, and Enter on the row the operator clicked opened nothing. -->

# A plugin pane's click index is its STOP count, and the drawing recorded one per item

MEASURED 2026-10-30, branch `agent/a-merge-row-you-cannot-open`, from the operator's *"i go to
/queue and cant click or enter that merge job"*.

## The mechanism, in one sentence

`content()` in `crates/tui/src/ui/panes/plugin.rs` called `out.push_stop(first)` for **every** `Walk::Item`
it drew, but `Item::stop()` answers `Some` only for a line or row with an `id` (and always for a
markdown item and a finding). So `PaneLines::stop_rows` — which `App::pane_stops` turns into the
click's `(screen row → index)` map — was longer than `stops(view, screen)`, which is what
`queue_sel`/`plugin_sel` index.

## The numbers, on a real pane

One review row and one `gating` landing, 120×30, the plugin drawing:

```text
stop_rows = [3, 6, 9]        # pane rows: the lease line, agent/one, agent/three
queue_plugin_stops = 2       # the cursor's list: two stops
```

A click on the landing (screen row 10) resolved to `k = 2` — `target_of(view, screen, 2)` is `None`,
so `queue_selected_id()` was `None` and Enter fell through to `ControlFlow::Continue(())`. Nothing.

## Why it was invisible on the pane this one replaced

The host pane (`ui/panes/queue.rs`) records its own stops one per row, so the two counts agreed
there; only a pane drawn by a LAYER with a `lines` block above its `tree` — the queue's chrome, a
dashboard's summary — was shifted, by one per drawn line that is not a stop.

## The fix, and the two halves a next pane needs

```rust
if it.stop().is_some() { out.push_stop(first) } else { out.push(first) }
```

and, for the queue, a click now OPENS the row (`keys/mouse.rs` → `App::open_act(Surface::Queue)`)
rather than only moving the mark.

## The rake

**Two records of "where a row is" must be built by one predicate.** A `*_stop_rows` vector and the
`stops()` the cursor walks are the same list or the pane is half-broken in a way no test sees: the
mark is right (the drawing counts stops properly), and only a CLICK is wrong — and a click on a row
whose index is past the end moves the cursor to a stop that is not there, so the key pressed next
does nothing either.
