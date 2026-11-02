<!-- abstract: `QueueSnapshot` carries every queue row ever, so any live counter must name the states it reads; and the pane fixture's clock is 62 s after the epoch, so a window test must move the snapshot's own clock. -->

# The queue snapshot is the whole history, and its clock is 62 seconds after the epoch

`mergequeue::snapshot` (`crates/harnessd/src/mergequeue.rs:166`, `:198`) fills reviews from
`review_rows(None)` and merges from `merge_rows(None)` - no state filter, and nothing in this
tree deletes a queue row. So a counter over the snapshot is a reading and must name the states
it reads: a `rejected` review is a row nothing will retry, and counting it as parked leaves it
there for ever.

The pane fixture is built with `at_ms: 62_000`, so nothing in it can be older than a day.
`snap.at_ms - (RECENT_MS + 1_000)` panics on overflow and `saturating_sub` puts the stamp at 0,
which reads as inside the window. Move the snapshot's clock instead:

```rust
snap.at_ms = 10 * RECENT_MS;
snap.merges[0].state_ms = snap.at_ms - (RECENT_MS + 1_000);
```

`App::queue_now()` is `snapshot.at_ms + (now_ms - queues_got_ms)` and `scoped()` carries `at_ms`
through, so both listings share one clock. A landed row's `state_ms` is when it landed and
`landed_sha` is the tree; a landed row with no `landed_sha` is one the daemon never wrote, so
count by `state`.
