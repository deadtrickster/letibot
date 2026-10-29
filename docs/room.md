# The room — one host's agents, one queue per tag

The long form of `crates/room`. The crate's docs are the map; this is the reasoning, the wire, and
the list of what the next slice still owes.

## The brief, and the two decisions that shaped it

The operator, 2026-10-29:

> *"we have to be able to let all agents running in letibot communicate on a single host for
> starters. look how firecode chat works. we need builtins. top level sessions only. Example — there
> is RAM shortage — agents should be able to talk thru it and coordinate. so this chat should come
> with a tagged priority queue — queue per tag essentially. top level session can coordinate via it.
> once agent that claimed the queue item dies (or goes offline) that item is marked as such and
> queue moves on. Just like with todo top level session get reminds (with ability to mark waits_on)
> so they dont occupy it forever."*

**First decision — a table in `sessions.db` plus a pass.** Every daemon on this box shares that
store, and the merge queue proves the shape survives restarts.

**Second decision — a socket, a dedicated daemon, a tie-breaker, a pane.** The operator reversed the
first one the same night:

> *"id still do it as a socket tho, we have a dedicated harnessd instance that autostarts and has a
> tie breaker agent (model configurable with deepseek as default) and then we gain a wasm plugin for
> that server — raw session of the tie breaker, queues, etc, with the manual controls"*

So there is **no store and no migration**. What the state machine owns is independent of transport
and is what this crate is: the item lifecycle, the claim as a lease with a beat, the abandonment
with a reason and a count, top-level-sessions-only, the ordering per tag, and the reminder question.
The wire is proposed here; the daemon that owns the socket is the next slice.

## The state machine

```
open ──claim──▶ claimed ──done──▶ done
  ▲                │
  └──abandoned◀────┘          (beat stopped · session gone · released on purpose)
```

* **`abandoned` is not terminal.** The item returns to the head of its tag, claimable again, with
  its count up; the state names the last thing that happened so a reader cannot mistake a re-queued
  item for one nobody ever tried. `Item::claimable()` is the predicate; `state == Open` is not.
* **`done`** ends the item. **`dropped`** is the terminal a ruling can give a raised item — it is
  not `done`, because nobody did the work.

### A tag is a lock

One live claim per tag. A session claims *the head of a tag*; while that claim lives every other
claim under the tag is refused `Busy`, naming the holder and the item. That is the RAM example's
whole shape: three sessions want the same scarce thing, one runs.

The consequence to know: two independent jobs that may run at once need two tags (`merge-a`,
`merge-b`), not one. A tag is a resource, not a category.

### The order is `(posted_ms, id)` — and there is no priority number

First come, first served; the room's own id breaks a tie within one millisecond. **A numeric
priority was rejected on purpose**: a number invites every session to claim 0, and then the number
carries no information and the room has a tie it cannot settle without the arbiter. Urgent work gets
its own tag (`ram-urgent`) — a name a reader can see, in place of a number nobody can rank.

The property that makes this more than a sort: **an abandoned item returns to the head**, because it
was posted before everything behind it. The queue moves on from a claimant, not from the request.

### A claim is a lease with a beat

`claimed_by`, `claim_ms`, `beat_ms` — the `queue_lease`/`host_lease` discipline. The default lease
is the merge queue's own **90 s** (`mergequeue::lease::LEASE_TTL_MS`), eighteen missed beats at a
five-second beat.

A claim is lost in three ways, and they are different facts:

| how | when the room knows | what it records |
|---|---|---|
| `Room::sweep` — the beat stopped | `now - beat_ms > lease_ms` | `Why::BeatStale { age_ms }` |
| `Room::note_gone` — the session is gone | at once, from the daemon | `Why::SessionGone { says }` |
| `Room::abandon` — the holder gives up | at once, from the holder | `Why::Released { says }` |

The first two are the difference between *the daemon is alive and the session is not working on it*
(a parked turn, a model that stopped calling `beat`) and *the daemon is gone* (exited, restarted, its
socket closed). A voluntary release is still counted: an item everybody gives up on is a problem
whether or not each gave up politely, and a rule with an exception is a rule somebody has to explain.

**The abandonment is loud.** It is an `Event::Abandoned` carrying the reason, the count and whether
the item is back in the queue, and the item keeps every abandonment in its history.

### Three abandonments raise the item

`ABANDON_THRESHOLD = 3`. The first abandonment is usually an accident — a daemon restarted, a
session killed, a box that slept — and one is far too few to give up on a request. The second says
the work is fragile or the claimant picked badly. At the third, cycling costs more than asking:
three different claimants have been killed by this item, and the next one would be chosen by who is
fastest rather than by who can do it.

So the third abandonment **raises** it: marked abandoned, the count stops moving, no longer
claimable, and its tag's queue **moves past it** to the next item — which is the point, since an
item that keeps killing its claimants must not block the tag for ever. A `Question` is asked; the
item is frozen until it is answered.

## Where the tie-breaker decides

The arbiter is a model, so it is asked **a question with a decision in it**, never a dashboard.
`Question::text` is that question and `Question::facts` is the evidence it names, so the model can
check the words against the numbers.

1. **An item that has abandoned three claimants** — `About::ItemRaised`. The question names the
   item, its body, each claimant it lost and why, and who is queued behind it (the cost of keeping
   it), and asks for one of:
   * `Retry` — offer it again, the count kept, so the next loss raises at once;
   * `Hand { to }` — give the claim to a named session, now;
   * `Drop { why }` — out of the queue for good, with the reason on the item.
2. **An abandonment under dispute** — `About::ClaimDisputed`. A claimant comes back after its beat
   went stale (its daemon was paused, the box slept, its model was thinking) and finds the item
   abandoned and perhaps claimed by somebody else. The room refuses its late beat **by name**; the
   dispute is how it says *"I am still on it"* instead of fighting for the claim, and the arbiter is
   asked which of the two has it.
3. **Two items at one tag's head** — the room's answer is `(posted_ms, id)`, deterministically, so
   the arbiter is never *needed* for it. It is reachable all the same: a session that believes its
   request should have gone first asks through the dispute, and the question carries both items'
   facts. The ordering rule stays the room's; a ruling is how a person overrides it, not a second
   ordering rule.

**What is not a question for the arbiter: two coordinators.** The room is one process and one clock,
so "two claimants that each believe they are alive" cannot arise *inside* it — the room's state is
the truth. It arises only if two daemons both own a room, and that is answered with a lease on the
coordinator itself (the merge queue's own shape), not with a model. Asking a model to settle a
question a lock can settle is how an arbiter becomes a single point of slowness.

**A ruling is refused rather than half-applied.** A `Hand { to }` naming a session the room has
never met, or a subagent, or a tag that another item holds, leaves the item frozen and the question
open. The daemon vouches for a session the room has not met (`Room::vouch`) — the tie-breaker's own,
or one the operator names in the pane — and that is the only way a stranger becomes a target.

## The wire

`ROOM_PROTOCOL_VERSION = 1` — the room's own number, not `letibot_sessionlog::PROTOCOL_VERSION`. The
coordinator is a different socket with a different lifetime, and pairing a head with a daemon must
not be entangled with pairing a session with a room.

**One connection per top-level session.** `Hello { protocol, speaker }` carries the session and the
parent it speaks through (read by the daemon from `session.parent_session_id`); a subagent is
refused at the door. Per session rather than per daemon, for two reasons worth the sockets:

* **a push has an address** — an `Event` goes to the connections that own the sessions it is about;
* **the close is a fact** — a session whose daemon died drops its socket, and the coordinator calls
  `note_gone` at once instead of waiting out the lease. The socket is the liveness of the *daemon*;
  the beat is the liveness of the *work*.

| frame | answer | idempotent because |
|---|---|---|
| `post {id, tag, body}` | `Posted {id, item}` | the reply is remembered per `(session, id)`; a repeat is the same item |
| `claim {id, tag}` | `Claimed {id, item}` | a repeat by the holder is the same claim; `claim_ms` does not move |
| `claim_item {id, item}` | `Claimed {id, item}` | same, and the item must still be the head |
| `beat {id, item}` | `Beaten {id, item}` | stamping the same claim again is harmless |
| `waiting {id, item, text}` | `Waited {id, item}` | a set, not an append |
| `done {id, item}` | `Finished {id, item}` | a second is refused by name once the replay window forgets |
| `abandon {id, item, why}` | `Abandoned {id, item}` | same |
| `dispute {id, item, says}` | `Question {id, question}` | one open question per item; asking twice is the same question |
| `rule {id, question, decision}` | `Ruled {id, question}` | a question is answered once (`AlreadyRuled`) |
| `queue {tag}` | `Queue {tag, rows}` | a read: no id, nothing to act on twice |
| `holding {session?}` | `Holding {session, rows}` | a read |
| `reminders` | `Reminders {rows}` | a read |
| `questions` | `Questions {rows}` | a read |

The replay window is 256 replies per room. **A client away for longer than that must re-read**, not
replay: every reply carries the item as it stands, so a re-read is cheap and cannot be wrong, and a
replay window is not a durable log. **A reply to a repeat carries no events** — the bell rang the
first time.

A protocol mismatch is `Bye { why }` with both numbers in the sentence: the clean refusal the
daemon's own `Hello` makes.

## The bell, and the client that cannot hear it

firecode's fourth rule, paid for there: **a bell is something that can inject a turn.** A client that
only acts when a person prompts it cannot be woken; the room is a mailbox to it, not a bell.

The room produces the events; delivering them is the daemon's job and the next slice's. What this
slice settles is what is worth ringing about: `Posted`, `Claimed`, `Done`, `Abandoned`, `Raised`,
`Asked`, `Ruled` — and **not a beat**, because a bell that rings five times a minute is a bell nobody
listens to.

The other three of firecode's rules land on the daemon's side too, and they are worth stating here
so the next slice does not rediscover them: start listening before you start working; one read is
not membership; **acknowledge before you act** — a one-line "seen, doing X" before the work, because
silence reads as absence.

## The precedent: the review queue's own session

`crates/harnessd/src/sessions/gatekeeper.rs` is the shape "a dedicated, autostarted session with an
identity" already has in this tree, and the fit is close:

* **An id derived from the workspace**, not a constant — `s-gatekeeper-<key>`. Two daemons on one
  box shared `s-gatekeeper` once and appended to one transcript at the same time; the derived id is
  what stops that. The coordinator's own id must be derived the same way (a room is per box, so
  `s-room-<host key>`), for the same reason.
* **Ensured on every pass**, not started once: `ensure_gatekeeper` opens it if it is not open, and
  says a standing failure once per reason instead of every pass. A room whose tie-breaker could not
  open must be visible in words, not as a queue that looks quiet.
* **A root with no `parent_session_id`**, a title a person reads, a seat that carries the tools it
  needs and no gated ones, and a prompt composed at open from the crate's own constants.
* **Its board is a view of state that lives elsewhere** — `TodoBy::Queue`, replaced from the store
  on every pass. The room's board is the same idea with the room as the state.

What differs, and it is the reason this is a *daemon* and not a session:

* the gatekeeper is a session inside a daemon whose real job is the operator's project; the
  coordinator is a whole `harnessd` whose only job is the room — its own socket, its own data
  directory, no project, and **autostart is the launcher's** (`scripts/letibot` already starts or
  attaches a daemon per folder; a room is one folder with a `room` mode).
* the gatekeeper refuses to open without a store (`--store` daemons have no queue). The coordinator
  needs no store at all — that is the whole point of the socket — so its "apparatus" is the socket
  and the model, and a box without the model must say so rather than run a room nobody arbitrates.
* the gatekeeper's model is the daemon's own configuration; the coordinator's is **configurable with
  deepseek as the default**, which is a `providers.toml` entry and a `--model` on that daemon, the
  same two knobs every other role uses.

## What the daemon still owes

### Slice 2, as built

The runtime is in, and it is one mode plus one crate's second half:

| what the list asked for | where it is |
|---|---|
| the socket and the daemon | `harnessd --room` (`crates/harnessd/src/room/coordinator.rs`), serving `letibot_room::Server` — accept loop, one read loop per connection, `Hello` at the door, the room behind **one** `Mutex` |
| the coordinator's own lease | `letibot_room::lease` — a **file**, `<data dir>/room-<host key>.lease`, with `host/pid/<ns>` for an owner, a ten-second beat, the merge queue's ninety-second TTL, a stale takeover that says whether the beat stopped or the pid is gone, and a refusal that names the live holder |
| the clock and the pass | the server's own clock thread: `sweep(now)` every five seconds, the lease's beat on the same thread (one process, one clock), and `note_gone` on every connection close |
| the tie-breaker session | the coordinator's own session, `s-room-<host key>`, opened with **no store**, seated `orchestrator` — which names `room` (see *Which seats carry `room`*) — prompted with `open_questions`, a sentence with the decision in it plus the typed facts, and answering through `rule` |
| the session-side tool | `room` (`crates/tools/src/builtins/room.rs`): post, claim, claim_item, beat, waiting, done, abandon, dispute, rule, queue, holding, reminders, questions |
| the beat on the session's behalf | `Harness::begin_turn_clock` starts it, `Harness::end_turn_clock` stops it, and `letibot_room::client::Client::start_beating` re-reads what the session holds on every tick |
| the wake | a `Response::Event` on the connection's reader thread becomes a sentence in `crate::room::wake`, rings the worker's bell, and `Harness::wake` composes it as a `room` notice |

The names are all derived from the host — the socket, the lease and the session id
(`s-room-<host key>`) — so two boxes never collide and `letibot --session host` always resolves.
The lease is a **file and not a row**: the room keeps no store, so a coordinator that opened
`sessions.db` to hold its own lock would be the first half of putting the queue in it too.

### Which seats carry `room`, and why the coordinator's own has to

**Two seats, and they are the two a person's own session runs on:**

| seat | role | carries `room` | why |
|---|---|---|---|
| `orchestrator` (the default; `letibot`, `letibot --read-only`) | `roles::m1_orchestrator()` | **yes** | the default top-level seat, and **the coordinator's own** (`s-room-<host key>`) — the arbiter rules through this tool |
| `leticode` (`leticode`, `letibot --role leticode`) | `roles::leticode()` | **yes** | the working top-level seat |
| `planner`, `researcher` | their roles | no | a plan mode and a seat with no retrieval backend; neither holds work a tag would be locked for |
| `coder`, `runner` | `m2_coder`, `m2_runner` | no | a subagent's seat — its words go through the session that spawned it |
| `gatekeeper` | `roles::gatekeeper()` | no | the review queue's session and its reviewers: the reviewers are children, and the room refuses a subagent at its door |
| `notes_keeper` | `roles::notes_keeper()` | no | a hidden child, and read-only |

**A session on any other seat does not see the tool at all.** `Registry::resolve_role` keeps only
the tools a role NAMES, so a seat that does not name `room` has it dropped from the session's
registry even though `Harness::open` registered it — and a model that calls it anyway is answered
by the runtime's unknown-tool miss: *"no tool called `room` is available here"* plus the list of
the tools it does have. That is deliberate: a tool in the prompt that always refuses is prefix
bytes and a wrong verb taught to a model that cannot use either.

**This is the defect the first cut shipped, and the reason it is written down here.** The name was
put into §8.4's `roles::orchestrator()` — a table **no seat resolves** — while the live seat is
`roles::m1_orchestrator()`. The coordinator's session therefore had a `room`-less registry, and the
tie-breaker was woken, handed *"Answer with the `room` tool: verb `rule`"*, and had no such tool:
every raised item froze for ever, silently, which is the one outcome the no-model refusal above
exists to prevent. `crates/harnessd/tests/room_coordinator.rs` is the test that now says otherwise
— it boots a coordinator on a socket of its own, opens the arbiter's session through
`Harness::open`, asserts the seat resolves a registry holding `room`, and then rules on a raised
item **through the tool**, which is the call the prompt tells the model to make.

**The wake, exactly.** A session is woken by a `Response::Event` about it — posted, claimed, done,
abandoned, raised, asked, ruled — and by nothing else. A beat is not an event. The connection is
per session and stays open for the session's life, so the *close* is the daemon's liveness and the
beat is the work's: a session whose turn ended keeps its socket and loses its claim to the beat.

### Still owed

1. **The wasm pane.** The queues per tag, the tie-breaker's raw session, and the manual controls —
   claim on a session's behalf, release, mark abandoned, rule on a dispute — plus **restart the
   server**, asked of the daemon that supervises the coordinator and not performed by the
   coordinator on itself. Until it exists, `Server::identity()` is the sentence a pane must print:
   *started T ago, and a restart begins with no items at all* — because the room keeps no store, and
   "restarted clean" and "restarted empty" are the same thing here unless it is said.
2. **The room's own identity in the protocol.** `Hello` should carry the room's name, so a session
   that connected to the wrong coordinator is told rather than obeyed. `Welcome` could carry
   `Server::identity()` in one field; the protocol number did not move for slice 2 and this is the
   change that would.
3. **A reminder row on the board.** `reminders` answers the question; nothing turns it into a
   `todo_write` row yet, so a holder is reminded only if the model asks.
4. **The launcher's autostart.** `--room` exists; `scripts/letibot` does not yet start a coordinator
   when a session's `room` tool finds no socket. Today the tool refuses by name, which is honest and
   is not the same as the room being there.

## What the state machine assumes

* **One clock**, given as `now` in milliseconds by the caller. A clock that goes backwards is
  survived, not corrected: a beat stamps `now`, and staleness is `now.saturating_sub(beat_ms)`, so a
  beat from the future reads as fresh rather than as instantly stale.
* **Liveness is two facts, both given**: the beat (it is working) and the caller's word (it is
  gone). The room never asks the OS, never reads a store, never guesses.
* **One writer.** Every mutating method is `&mut self`; there is no interior mutability, so the
  process that owns the room is the only writer by construction.
* **No durability.** A restart empties the room. That is a decision the pane must state (see above),
  and the alternative — a journal — is a later answer if the operator wants one.
