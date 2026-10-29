<!-- abstract: MEASURED 2026-10-29 (branch agent/standings-stuck-and-landed): `QueueSnapshot` carries EVERY row (review_rows(None)/merge_rows(None), nothing is deleted), so terminal rows inflate any live counter; and the test fixture's `at_ms` is 62 000 ms after the epoch, so nothing in it can be older than a 24 h window — a window test must move the snapshot's own clock. -->

# The queue snapshot is the whole history, and its clock is 62 seconds after the epoch

MEASURED 2026-10-29, branch `agent/standings-stuck-and-landed` (the edge's `stuck`/`landed`
columns), reading `crates/harnessd/src/mergequeue.rs` and `crates/tui/src/app/tests/mod.rs`.

## The snapshot a head holds is every row ever, not the open ones

`mergequeue::snapshot` (`crates/harnessd/src/mergequeue.rs:188`) fills `reviews` from
`store.review_rows(None)` and `merges` from `store.merge_rows(None)` — **no state filter**, and
nothing in this tree deletes a queue row. So `QueueSnapshot.reviews`/`.merges` contain the
terminal rows too: a `rejected` review whose complaints went back to the branch's session, a
`withdrawn` or `moved` landing, an `approved` review that is the merge queue's business now.

**Any counter over that snapshot is a reading, and it has to say which states it is reading.**
The `parked` column was counting `rejected` into a live count — a row nothing will retry and
nobody has to touch — so it sat there for ever. The fix is a state whitelist per column
(`waiting|reviewing`; `queued|gating`; the open-and-stopped set; `landed` inside a window) and
every other state counted nowhere.

## The pane's fixture clock is 62 000 ms after the epoch

`app::tests::queues()` builds the snapshot with `at_ms: 62_000` and its rows with
`state_ms: 2_000`, i.e. *a minute ago*. That is fine for the ages the pane draws, and it makes a
WINDOW test impossible to write the obvious way: `RECENT_MS` is 86 400 000, so
`snap.at_ms - (RECENT_MS + 1_000)` **panics on overflow** (`attempt to subtract with overflow`,
measured), and the `saturating_sub` fallback puts the stamp at 0, which `now.saturating_sub(0)`
then reads as *inside* the window (0 is "now" in a `u64` clock that has not started).

The shape that works — and the one to copy for any test about `RECENT_MS`:

```rust
let mut snap = queues(Vec::new(), vec![queue_merge_row("m-old", "landed")]);
snap.at_ms = 10 * crate::ui::panes::queue::RECENT_MS;   // a clock where "older than a day" exists
snap.merges[0].state_ms = snap.at_ms - (crate::ui::panes::queue::RECENT_MS + 1_000);
```

`App::queue_now()` is `snapshot.at_ms + (now_ms - queues_got_ms)`, and `scoped()` carries
`at_ms` through, so `queues` and `queues_all` are the same clock — `standings_where` takes
`now` as a parameter for that reason (`App::queue_now()` at both call sites).

## A landed row's own stamp

`MergeState::Landed` is written by `gating_to(..)` only (`serve.rs:1719`, `:2073`), each time
with `Move::new(..).set("landed_sha", ..)` — so `state_ms` is when the row landed and
`landed_sha` is the tree. A landed row with no `landed_sha` is a row the daemon never wrote
(the test fixture's default), which is why counting by `state` and not by `landed_sha.is_some()`
keeps the fixture honest and the count right.
