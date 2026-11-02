<!-- abstract: `Room::answer` drops the events a verb produced, a client whose reader thread holds a strong `Arc` keeps its own socket open, and the arbiter is routed every event but must be woken by three. -->

# The room runs — and the four things that were silent until a test caught them

`Room::answer_traced -> Answer { response, events }`, with `answer` as a one-line wrapper. A
server built on `answer` answers every frame and rings nobody, and no test of the frames can see
it. When a state machine hands back a reply, ask what it did with the rest of the outcome.

A dropped client must close its socket. The reader thread holds a `Weak`, and `Drop for Shared`
calls `shutdown(Both)` on the writer - `shutdown` acts on the SOCKET, so the reader's blocked
`read` returns and the peer sees EOF. The order is the fix: `read_frame` first, `weak.upgrade()`
after. A `Weak` upgraded before a blocking read is still an owner, and the test then fails about
half the time at its own deadline.

The arbiter is routed every event but woken by three: `SessionRoom::connect` recognises it and
drops `Posted`/`Claimed`/`Done` before the push. Routing is who may see it; the wake is what is
worth a turn.

The lease file is `owner\tpid\tbeat_ms\tstarted_ms`, parsed with `trim_end` and not `trim`
(`crates/room/src/lease.rs:255`): a leading tab is the first field being empty, which is no
lease at all.
