<!-- abstract: MEASURED 2026-10-26: a head the hub registered but never greeted stays in `attached_heads` for ever - `seat_in` attaches and THEN writes the `Hello` with a `?`, so the error builds no `Seat` to drop; the fix is `Drop for Seat` plus a hand detach on that one failing write. -->

# A head the daemon registered but could not greet lived for ever

`seat_in` (`crates/sessionlog/src/server.rs`) does `hub.attach(...)` first - the head is in
`Inner::heads`, `HeadAttached` has fired and `attached_heads` counts it - and only then writes the
`Hello` and the backlog. That write is a `?`, so a socket already gone returns before any `Seat`
exists and nothing ever detaches it: the count stays up for ever. Measured as an e2e expecting 2
heads seeing 3, stably, for 60 s (67.8 s per failing run against 8.0 s per passing one).

Both halves of the fix are load-bearing: `impl Drop for Seat` detaches on every path, the ~45
`write(...)?` calls in the read loop included, and `seat_in`'s greeting failure detaches by hand
because that is the one path with no `Seat` to drop. `Hub::detach` is idempotent, so a `Switch`
that already left is safe.

What a leaked head costs: `Answers::ask` and `ask_question` read `Hub::deciding_heads()` before
posting a card, so a head left in it makes every later ask on that session wait out its deadline
and come back "nobody answered". And every daemon opens a gatekeeper session at startup with
nobody attached (`letibot_sessionlog::gatekeeper_session`, per workspace, not the old
`s-gatekeeper` constant), so that session is always attachable and always in play.

The rule: a registration whose greeting can fail needs the registration's undo on the failure
path, and `?` inside a long match arm is where that undo goes missing. The test that catches it is
a COUNT that must come back down (`attached_heads`), not one that must go up.
