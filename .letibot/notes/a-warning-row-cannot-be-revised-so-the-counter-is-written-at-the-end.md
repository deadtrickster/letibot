<!-- abstract: A head files a warning under (code, ts, detail-hash), so a daemon re-publishing the same code with a bigger count adds a row rather than revising one - the honest daemon-side shape is one row written when the outage ends. -->

# A warning row cannot be revised, so the counter is written when the outage ends

`App::note` files a warning under `note_key` = `w/"<code>"|"<ts>"|<fnv1a(detail)>`
(`crates/tui/src/app/notes.rs`) and `holds()` refuses only an identical triple, so a daemon that
re-publishes the same code with a bigger count is ADDING a row. There is no daemon-side revise:
`SessionEvent` has no replace variant, transcript rows are append-only by trigger, and the head's
only in-place update is `Note::Pane`'s `closed` flag.

So a counter that climbs while the outage lasts needs the head. What a daemon can do is write the row
once, where the count is already final: `harnessd::harness::Outage` is opened by the first failed
attempt, changed by every later one and published never, and `settle_outage` takes it after the retry
loop and writes exactly one row - `model_endpoint_answered` if a later attempt answered,
`model_endpoint_retry` if the ladder gave up, and nothing at all for one attempt and no answer.

Every exit from the retry loop must settle the record, or a later outage inherits its clock. The
loop's one early `?` (the 401/403 arm) used to return with the record still in the field, so the next
turn's first failure continued it and a 45-minute-old `began_ms` landed in a fresh row.
