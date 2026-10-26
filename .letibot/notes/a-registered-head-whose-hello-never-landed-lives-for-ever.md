<!-- abstract: MEASURED 2026-10-26: `serve_conn`'s Seat is now detached on Drop, and `seat_in` detaches when the Hello/backlog write fails — because a head the hub registered but never told it was here stayed in `attached_heads` for ever (3 where 2 were expected, stably, for 60 s). -->

# A head the daemon registered but could not greet lived for ever

MEASURED 2026-10-26, package D1, `crates/sessionlog/src/server.rs` and
`crates/bridge/tests/end_to_end.rs` line ~1044 (*"the operator's head and the watcher"*).

## The symptom, and it is stable rather than slow

The e2e's `assert_eq!(hub.attached_heads(), 2, "the operator's head and the watcher")` failed
about **one run in three** with `left: 3`, and polling for 60 s did not bring it down: the third
head is registered FOR EVER. Every run that failed spent 67.8 s (the poll), every run that passed
spent 8.0 s.

## Why, and both halves are one fact about the order of two calls

`seat_in` does `hub.attach(...)` first — which puts the head in `Inner::heads`, fires
`HeadAttached`, and counts in `attached_heads` — and only THEN writes the `Hello` (and a
resumed/attached head's backlog). The write is a `?`. A head whose socket has already gone makes
that write fail, `seat_in` returns `Err`, and **no `Seat` was ever built**, so the only code that
would have detached the head (the read loop's tail, and `leave`) never ran.

Two things made this reachable rather than theoretical:

* `crates/bridge`'s `wait` attaches a watching head, reads the snapshot, and drops the socket. It
  used to take ten seconds to decide (the `SETTLE` window), so the daemon had long finished the
  greeting; `PROTOCOL_VERSION` 52 deleted that window and a `wait` can now be over in
  milliseconds — faster than a `Hello` carrying a snapshot of a whole session is written.
* and the same class of leak sits in the read loop: ~45 `write(...)?` calls inside it, each of
  which returns from `serve_conn` through the error arm, past the `seat.hub.detach(&seat.head_id)`
  at the bottom.

## The fix

`impl Drop for Seat { fn drop(&mut self) { self.hub.detach(&self.head_id); } }` — every path,
including the `?`s, and safe beside the explicit detaches because `Hub::detach` is idempotent (a
`Switch` drops the old `Seat` after `leave` already detached it). Plus, in `seat_in`, the
`Hello`/backlog write's error is answered by `hub.detach(&head_id)` before returning, because that
is the one path with no `Seat` to drop.

After the fix: **8/8 e2e runs green at 8.0 s**. Before it: 2/6 leaking, all 67.8 s.

## What to reach for next time

**A registration whose greeting can fail needs the registration's undo on the failure path, and
`?` in a long match arm is where that undo goes missing.** The test that catches it is a *count*
that must come back down — `attached_heads` — not one that must go up.
