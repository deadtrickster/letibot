<!-- abstract: A conversation click record is window-relative and `main_area` shifts it by the header row, so a record that forgets the shift makes every click land one row off - and a run's marker can count live calls that have no rows. -->

# The empty tools panel, and the rows a frame shifts

A conversation click record is WINDOW-relative: `file_rows_in` / `notice_rows_in` / `card_rows_in` /
`picture_rows_in` return `line - start`, and `main_area`'s `Drawn::Conversation` arm does `*row += y`
for each of them, `y` being the header row. A new record that forgets that line looks right and makes
every click land one row off - a click test on the operator's own words opened a picture because row 1
was both "the words" on the glass and "first thumbnail row" in the record. Adding a click record to the
conversation means adding the shift, and clearing the record in `record_main_hits`
(`crates/tui/src/app/hits.rs:129`), which clears `file_rows`/`notice_rows`/`card_rows` before drawing.

The empty panel: a round's result rows are appended only after every call in it has run, so while they
run the marker's numbers are the live pane's and the run under it may hold no rows at all. A click that
set `payload_sel` to the run head then hid the very rows the run has. The fix is `LIVE_RUN`
(`crates/tui/src/app/runclick.rs:9`) - a click on the live marker names the live pane.
