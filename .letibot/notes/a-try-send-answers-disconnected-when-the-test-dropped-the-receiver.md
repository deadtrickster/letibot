<!-- abstract: A push into a `sync_channel` answers `Disconnected` rather than `Full` if the test dropped the Receiver, `sync_channel(0)` makes `try_send` a race, the room's handshake must be written before the writer thread starts, and `armed` is the only witness that a disconnected session existed. -->

# `try_send` answers `Disconnected`, not `Full`, when the Receiver was dropped — and three neighbours

`crates/room/src/server.rs` pushes to a connection through a bounded `SyncSender<Response>` drained by
one writer thread per connection (`Conn::send` -> `give_up` on either error). The traps, all found
while testing it:

1. **A bound of 0 is a rendezvous channel, and `try_send` on it is a race**: it succeeds only if the
   receiver happens to be inside `recv` at that instant. Clamp the bound at one frame.
2. **A unit test must HOLD the `Receiver`.** Drop it and the queue answers `Disconnected` before it
   can ever answer `Full`, so the overflow path is never reached and the test is green for the wrong
   reason. `let (outbox, _frames) = sync_channel(2);` — the underscore binding keeps it alive; a bare
   `_` would drop it.
3. **The handshake is written straight to the socket, before `spawn_writer`.** Start the writer first
   and the `Welcome` can interleave with the first queued frame inside one line. The writer thread is
   started after, so one writer owns the socket from then on.
4. **`is_known` and `armed` answer different questions.** After a session disconnects there is no
   connection, and on a box whose daemon sent no tree there is no member row either, so `is_known` is
   false: a message ADDRESSED to that name is refused (right) but a room message that merely `@`-names
   it must still be kept for it (also right). The `armed` set — one entry per principal the room has a
   chat cursor for — is the only witness that the session ever existed, and it is what the pending
   inbox tests a mentioned name against. Without it the first version of the away-message fix kept
   nothing at all, and its test hung rather than failed.
