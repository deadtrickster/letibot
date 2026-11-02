<!-- abstract: A request id minted from the clock alone collides on a unix socket, and the room's replay window then answers the second act with the first act's reply - no state change, no error. -->

# A request id minted from a clock is not unique, and an idempotency window makes the second act vanish

The room keys its replay window on `(session, id)` (`REPLAY_WINDOW`, `Room::replayed` / `remember`), which is what makes a reconnecting client's repeat answerable instead of acted on twice. Both of the daemon's doors minted their id from the clock alone.

Two round trips on a unix socket are tens of microseconds, so two verbs on one row really can share a millisecond. The second frame is then answered from the window: Ok, with the first act's reply, and no state change and no error. The symptom was a state machine that did not move - an item at two abandonments where the threshold is three - and nothing named the transport.

`link::request_id(what)` (`crates/harnessd/src/room/link.rs`) is the clock and a process-wide `AtomicU64`, and both doors use it.

Also found here: `PluginHost::validated` ran `View::validate()` and not `validate_for(declared)`, so a pane the DAEMON draws had its verbs checked by nobody. It takes the answering layer's `actions` now.

Rake: an idempotency key derived from a clock has a collision window as wide as the fastest possible pair of operations. When an act seems to have done nothing, ask whether it was ever performed.
