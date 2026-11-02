<!-- abstract: The four decisions behind the room's tag queue that are silent when undone: a tag is a lock, `abandoned` is not terminal, the socket close and the beat are two liveness facts, and a raise must not block the tag. -->

# A tag is a lock, and three more things the room's next slice must not undo

`crates/room` is the tag queue and `docs/room.md` is the long form. Four decisions are load-bearing,
and each is silent when undone.

A tag is a LOCK, not a list of claimable items. Three sessions posting `{tag: "ram", need: "8 GB"}`
must not each take a different item: one live claim per tag, everybody else refused `Busy` naming the
holder. Order is `(posted_ms, id)`, which makes "an abandoned item returns to the head" fall out, and
there is no priority number - a number invites every session to claim 0.

`abandoned` is not terminal: the item is claimable again at the head with its count up.
`Item::claimable()` (`crates/room/src/item.rs`) is the predicate, and a claim path or a pane that
filters on `state == Open` silently loses every item that ever lost a claimant.

The socket close and the beat are two liveness facts: the beat says the session is working on it, the
connection says its daemon is alive. A parked session's claim must go, and a dead daemon's goes at
once (`Room::note_gone`) rather than after the ninety-second lease.

Three abandonments raise (`ABANDON_THRESHOLD = 3`), and a raise must not block the tag: the item
freezes and its tag's queue moves past it. Two coordinators are never a question for the arbiter - one
process, one clock, one writer is a lease.
