<!-- abstract: MEASURED 2026-10-29 (queue-pane-declutter): nothing on the wire says which daemon the head is attached to — no Hello/Wiring field, no hostname in crates/tui — so "is this claim ours?" can only be answered by comparing the row's `host/pid` with the lease holder of that repository. -->

# The head has no daemon identity, so "this daemon" is the repository's lease holder

MEASURED 2026-10-29, branch `agent/queue-pane-declutter` (worktree of the same name), while making
the queue pane say `by <owner>` only when the claim is not the local daemon's.

## What is on the wire, and what is not

- `QueueReview.owner` / `QueueMerge.owner` and `QueueLeaseView.owner` are the SAME shape:
  `harnessd::mergequeue::lease::this_daemon()` mints `format!("{host}/{pid}/{started_ms}")` once
  per process, so a row's claim and the lease's claim are comparable as `host/pid`.
- **Nothing on `Hello`, `SessionWiring` or `QueueSnapshot` names the daemon the head is attached
  to.** `QueueSnapshot.here` is the *repository* the asking session is in (R92 item 5), not the
  daemon. `grep -rn hostname crates/tui` answers nothing.

So the only answerable question is *"is this row claimed by whoever holds this repository's
lease?"* — which is what `Said::serves` asks (`crates/tui/src/ui/panes/queue.rs`). If a real
"is this us?" is ever wanted, the daemon has to put its own `host/pid` in the snapshot (an added,
defaulted field: no protocol bump).

## The other two things the declutter cost me

- **A row's width budget includes the 2-cell mark column.** `queue_lines` pushes the head with
  `format!("{} ", pane::mark(picked))` (2 cells) before the row's own spans, so a row built to
  `w` is 2 columns over. `row_line` subtracts it, and the reason is trimmed to what the fixed
  fields LEAVE — put the reason last without that arithmetic and `trim_to` eats the reason first,
  which is the opposite of the point.
- **`pane_stops` maps only the head rows**, so a click on the picked row's detail lines selects
  nothing (same as before, when the second line was also not a stop). A detail line that should
  be clickable needs its own `Target`, not a stop.
