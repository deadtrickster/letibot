<!-- abstract: MEASURED/DECIDED 2026-10-29 building `crates/room` (branch `agent/a-queue-per-tag-the-store`): the operator pivoted from a `sessions.db` table to a socket + dedicated autostarted harnessd + tie-breaker, so no schema moved; and the four decisions the next slice must not undo — a tag is a LOCK (not a list of independently claimable items), `abandoned` is not terminal, the socket close is a liveness fact separate from the beat, and there is no priority number. -->

# A tag is a lock, and three more things the room's next slice must not undo

**MEASURED/DECIDED 2026-10-29**, branch `agent/a-queue-per-tag-the-store` (commit `e345577a`), building
`crates/room`. The long form is `docs/room.md` in the tree; this note is the four traps, because they
are all silent when undone.

## The pivot, so nobody looks for the migration

The first brief was a table in `sessions.db` plus a pass. The operator replaced it the same night —
*"id still do it as a socket tho, we have a dedicated harnessd instance that autostarts and has a tie
breaker agent … and then we gain a wasm plugin for that server"*. **No schema version moved, no
`tokencore` store code changed**, and the branch's diff is pure additions. If a later session finds a
`room` table in `sessions.db`, it is not this work.

## 1. A tag is a LOCK, not a list of claimable items

Three sessions each post `{tag: "ram", need: "8 GB"}`. If a claim takes *any* open item of the tag,
all three claim a different item and all three run — the RAM example is exactly the case where only
one may. So: one live claim per tag, everybody else refused `Busy` naming the holder. Two independent
jobs that may run at once need two tags (`merge-a`, `merge-b`); a tag is a resource, not a category.

The ordering that follows is `(posted_ms, id)` — first come, first served — and it makes *"an
abandoned item returns to the head"* fall out for free, because it was posted first. **No priority
number**: a number invites every session to claim 0, and then the room has a tie it cannot settle
without the arbiter. Urgent work gets its own tag.

## 2. `abandoned` is not terminal

The item goes back in the queue — claimable again, at the head, count up — and the state names the
last thing that happened so a re-queued item is not mistaken for an untouched one. `Item::claimable()`
is the predicate; `state == Open` is **not**, and a pane or a claim path that filters on `Open` will
silently lose every item that ever lost a claimant.

## 3. The socket close and the beat are two different liveness facts

The beat says *the session is working on it*; the connection says *its daemon is alive*. A session
whose turn ended is alive with a stopped beat (parked), and its claim must go — that is the
operator's *"so they don't occupy it for ever"*. A session whose daemon died is gone now, and waiting
out the ninety-second lease for it is a minute and a half of a blocked tag. So the wire is **one
connection per top-level session**, and the coordinator calls `note_gone` on the close.

## 4. Three abandonments raise, and a raise must not block the tag

`ABANDON_THRESHOLD = 3`: one loss is an accident (a restart, a kill, a sleeping box), the second says
the work is fragile, the third says cycling costs more than asking. The raised item freezes and **its
tag's queue moves past it** — an item that keeps killing its claimants must not hold the resource for
ever. The arbiter answers `Retry` (count kept, so the next loss raises at once), `Hand{to}` or
`Drop{why}`, and a ruling that cannot be applied is refused rather than half-applied, leaving the
item frozen.

What is *not* a question for the arbiter: two coordinators. One process, one clock, one writer — that
is a lease (the merge queue's shape), not a model. Asking a model to settle what a lock settles makes
the arbiter a bottleneck with an opinion.
