<!-- abstract: MEASURED 2026-10-26 (UI1): the tool-call counter's panel opened EMPTY because a run's marker can count live calls that have no rows, and a click lifted the run whose reasoning the clause hides; and a new `*_rows` click record in the conversation must be shifted by the header row in `main_area` or every click lands one row off. -->

# The empty tools panel, and the rows a frame shifts

MEASURED 2026-10-26, branch `inline-images` (package UI1), `crates/tui`: the operator's
*"model started toolcalling (yellow counter), clicked on the tools counter — empty panel"*, and
the picture half of the same session.

## A run's marker can count calls that have no rows, and lifting the run showed none of them

A round's result rows are appended only after **every** call in it has run
(`harnessd::harness`), so while the calls run the transcript holds nothing about them and the
marker's numbers are the live pane's (`LiveWork`). The marker sits on the last run of hidden
rows — commonly a run of *reasoning* — and `run_carries_live` folds the live counts into it.
A click on the `N tool calls` clause then did `payload_sel = run head; run_show = Some(Tools)`,
and `run_skips(Some(Tools), reasoning)` hides exactly the rows that run has. Nothing on the
screen changed except that the marker line itself vanished with the run: an empty panel, and
(separately) no way back, because the lift lived only in `live_open`, which `run_is_open`, the
page keys and the hint bar never read.

Two fixes, both in `open_marker` (`app/runclick.rs`): the lift is carried to the live pane when
the run is the one the work belongs to (`run_carries_live_now`), and a click on the live pane's
marker names the view in `payload_sel` as `LIVE_RUN`. Esc clears `live_open` with the rest.

## A conversation `*_rows` record is WINDOW-relative, and `main_area` shifts it

`file_rows_in` / `notice_rows_in` / `card_rows_in` return `line - start` — a row of the window —
and `main_area`'s `Drawn::Conversation` arm does `*row += y` for each of them, where `y` is the
header row. A new record (mine: `picture_rows`) that forgets that line looks exactly right and
makes every click land one row off: my click test on the operator's own words opened the picture
because row 1 was both "the words" on the glass and "first thumbnail row" in the record. If you
add a click record to the conversation, add the shift (and the `retain(.. != 0)` /
`!= last` lines beside the pinned-prompt and holding banners).

Also: `record_main_hits` clears `file_rows`/`notice_rows`/`card_rows` before drawing, so a new
record has to be cleared there too, or a frame with no conversation on it answers a click with
the last frame's rows.

## placeholders are kitty's; iTerm2's picture is the cells it is drawn in

`cfg.images` (the Unicode-placeholder form) is now gated on the PROTOCOL, not on
`Features::images`: iTerm2's `OSC 1337;File=` draws at the cursor *into* cells, so this head's
diffing painter erases it on the first repaint of those rows and would have to send megabytes a
frame. Where the terminal offers only that, the chip in the operator's words stays the picture's
whole representation. rano has the escape (`term::picture::Picture::iterm_escape`) and tests it
for a host that draws once.
