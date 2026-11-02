<!-- abstract: A warm `leticode --session` open does restore, but the head sits on an empty snapshot while the worker opens the harness - and a `Filling` is in no snapshot, so nothing said a restore was coming. -->

# A warm session is handed over empty, and the wait was invisible

`leticode --session <id>` against a running daemon does restore - `crates/harnessd/tests/warm_open.rs`
drives a real store, a real `HeadClient` and `Sessions::open`, and every stored row reaches the head.
The order a head sees is `ResumeSession` -> `Sessions{created}` -> `Switch` -> a `Hello` whose snapshot
is EMPTY (the hub is registered before `Harness::open` runs on the worker) -> the rows as
`TranscriptAppended`/`TranscriptContent` when the worker gets to it. For the length of that wait the
head holds `stored_items: 166` from the store and `items: []` from the snapshot.

`sessionlog::RESTORING_ROWS` is one constant with two speakers: the socket publishes
`Filling{what, "rows", 0, brief.items}` in the `ResumeSession` arm, and `Harness::open` ends it with
`republish_after`. `App::restoring` (`crates/tui/src/app/mod.rs`) draws an empty transcript whose
brief says `stored_items > 0` as a `system (restore)` row naming the count. Only `items.is_empty()`
counts - `items.len() < stored_items` is the ordinary state of a COMPACTED session.

`Filling` is in no snapshot and `Hub::attach` replays no backlog, so a fill published before a head
attaches is never seen: `Inner::filling_now` is re-said on attach and on resync, and is kept only
while `done < total`, because re-announcing a finished restore says an operation that is over is still
running.
