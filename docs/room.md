# The room - one host's agents, one queue per tag

The long form of `crates/room`. The crate's docs are the map; this is the reasoning, the wire, and what
is still owed.

## The brief

All agents on one host have to be able to talk to each other, with a tagged queue so a top-level session
can coordinate. A RAM shortage is the example: three sessions want the same scarce thing and one runs. A
claim whose holder dies is marked as such and the queue moves on, and a holder gets reminders (with a
`waits_on` they can set) so they do not occupy an item for ever.

The design is a socket, a dedicated coordinator daemon with a tie-breaker agent, and a pane. There is no
store and no migration: what the state machine owns is independent of transport.

## The state machine

```
open --claim--> claimed --done--> done
  ^                 |
  +--abandoned<-----+          (beat stopped, session gone, released on purpose)
```

`abandoned` is not terminal: the item returns to the head of its tag, claimable again, with its count up,
and the state names the last thing that happened so a re-queued item cannot be mistaken for one nobody
tried. `Item::claimable()` is the predicate, not `state == Open`. `done` ends the item; `dropped` is the
terminal a ruling can give a raised item, and it is not `done` because nobody did the work.

A tag is a lock: one live claim per tag, and while it lives every other claim under the tag is refused
`Busy`, naming the holder and the item. Two independent jobs that may run at once need two tags
(`merge-a`, `merge-b`): a tag is a resource, not a category.

The order is `(posted_ms, id)`, and there is no priority number: a number invites every session to claim
0, and then it carries no information and the room has a tie it cannot settle without the arbiter. Urgent
work gets its own tag (`ram-urgent`), a name a reader can see. An abandoned item returns to the head,
because it was posted before everything behind it.

A claim is a lease with a beat: `claimed_by`, `claim_ms`, `beat_ms`, the merge queue's own 90 s, eighteen
missed beats at a five-second beat. It is lost in three ways, and they are different facts:

| how | when the room knows | what it records |
|---|---|---|
| `Room::sweep` - the beat stopped | `now - beat_ms > lease_ms` | `Why::BeatStale { age_ms }` |
| `Room::note_gone` - the session is gone | at once, from the daemon | `Why::SessionGone { says }` |
| `Room::abandon` - the holder gives up | at once, from the holder | `Why::Released { says }` |

The first two are the difference between the daemon being alive and the session not working on the item
and the daemon being gone. A voluntary release is still counted: an item everybody gives up on is a
problem whether or not each gave up politely. At `ABANDON_THRESHOLD = 3` the item is raised: the count
stops moving, it is no longer claimable, its tag's queue moves past it, and a `Question` is asked while it
stays frozen.

## Where the tie-breaker decides

The arbiter is a model, so it is asked a question with a decision in it, never a dashboard.
`Question::text` is the question and `Question::facts` is the evidence it names.

1. An item that has abandoned three claimants (`About::ItemRaised`): the question names the item, each
   claimant it lost and why, and who is queued behind it, and asks for `Retry` (the count kept, so the
   next loss raises at once), `Hand { to }` (give the claim to a named session, now) or `Drop { why }`.
2. An abandonment under dispute (`About::ClaimDisputed`): a claimant comes back after its beat went stale
   and finds the item abandoned, perhaps claimed by somebody else. The room refuses its late beat by
   name; the dispute is how it says *I am still on it*, and the arbiter is asked which of the two has it.
3. Two items at one tag's head: the answer is `(posted_ms, id)`, deterministically. It is reachable through
   the dispute, and a ruling overrides the ordering rule rather than being a second one.

Two coordinators are not a question for the arbiter: two daemons owning a room is answered with a lease on
the coordinator, since the room is one process and one clock. A ruling is refused rather than half-applied:
a `Hand { to }` naming a session the room has never met, or a subagent, or a tag another item holds, leaves
the item frozen and the question open. The daemon vouches for a stranger (`Room::vouch`), and a session the
room has met as a member of a project's room counts too.

## The wire

`ROOM_PROTOCOL_VERSION` is the room's own number, not `letibot_sessionlog::PROTOCOL_VERSION`: the
coordinator is a different socket with a different lifetime, and pairing a head with a daemon must not be
entangled with pairing a session with a room.

One connection per top-level session. `Hello { protocol, speaker, room }` carries the session, the parent
it speaks through, and the room's name; a subagent is refused at the door. Per session rather than per
daemon, because a push has an address and because the close is a fact: a session whose daemon died drops
its socket and the coordinator calls `note_gone` at once instead of waiting out the lease. The socket is
the liveness of the daemon; the beat is the liveness of the work. `Welcome` carries `Server::identity()`,
so a session that dialled the wrong coordinator is told rather than obeyed.

| frame | answer | idempotent because |
|---|---|---|
| `post`, `claim`, `claim_item` | `Posted`, `Claimed` | the reply is remembered per `(session, id)`; a repeat by the holder is the same claim |
| `beat`, `waiting`, `acknowledge` | `Beaten`, `Waited`, `Acknowledged` | a stamp or a set, not an append |
| `done`, `abandon` | `Finished`, `Abandoned` | a second is refused by name once the replay window forgets |
| `dispute`, `rule` | `Question`, `Ruled` | one open question per item; a question is answered once (`AlreadyRuled`) |
| `members` | `Members` | a set: a repeat is the same tree |
| `queue`, `holding`, `reminders`, `questions` | `Queue`, `Holding`, `Reminders`, `Questions` | reads: no id, nothing to act on twice |
| `room` | `Room {identity, arbiter, connected, items, questions, agents, members}` | a read: the PANE's, and the only one the server completes |

Every mutating frame carries a client-minted `id`. The replay window is 256 replies per room: a client
away for longer must re-read, not replay, since every reply carries the item as it stands, and a reply to
a repeat carries no events because the bell rang the first time. A request id is not a clock: the window
keys on `(session, id)`, so two acts in one millisecond are one act, and `room::link::request_id` is the
counter that makes an id unique. A version mismatch is `Bye { why }` with both numbers and which side is
older.

### A claim's words: what it is DOING, and what it is WAITING on

`Claim::said` is *seen #1; working on it*, written by `acknowledge` when the claim is taken: the holder is
saying the item is its own work, so a queue read can tell a claim from a session that took the row and
died. `Claim::waits_on` is what the holder waits for, written by `waiting`: something outside moves the
item, which is why this one makes a reminder quiet. A fact that means *this is mine to do* must never be
written into the field that means *this is not mine to move*, or the reminder that says *finish it or
release it* never fires.

## The bell, and the client that cannot hear it

A bell is something that can inject a turn. A client that only acts when a person prompts it cannot be
woken; the room is a mailbox to it, not a bell. What is worth ringing about is `Posted`, `Claimed`,
`Done`, `Abandoned`, `Raised`, `Asked`, `Ruled`, and not a beat.

### The bell, per client

Verified per client, never assumed. Four kinds of client could plausibly be woken by a room event, and two
of them cannot be.

| client | a room client? | can an event start a turn? | the mechanism, or the absence |
|---|---|---|---|
| **a leticode session** (`crates/harnessd`) | yes, one `SessionRoom` per top-level session | **yes** | the reader turns the `Event` into a sentence (`room::wake::sentence`), queues it and rings `Bell::ring_wake`; `Harness::wake` runs a turn with it as a `Speaker::Agent` row. `crates/harnessd/tests/room_bell_turn.rs` proves both halves |
| **the head** (`letibot-tui`) | **no** | no: the daemon wakes it | no `letibot-room` dependency and no socket; its pane comes from the daemon's feed (`crates/harnessd/src/plugins/room_feed.rs`). A room note draws a notice row, never a card |
| **the bridge** (`letibot-bridge`, `Head::wait`) | **no** | no: `Head::wait` answers on an ask, a failure, a turn that ended, or the deadline | no `letibot-room` dependency; a room event reaches it only through a turn, so it can never be the `Ask` that exits 10 |
| **a firecode VM agent** | **no** | no: a subagent is refused at the door (`Refused::Subagent`) | `Harness::open` gives a session with a parent no link at all, and a VM agent's door is `firecode in`, an exec channel with no message verb |

A head and a bridge head are clients of the daemon, so they cannot see a room event the daemon chose not
to turn into a turn; what they keep is the pane, the transcript row and the turn.

### Two daemons, one host: the end-to-end check

`crates/harnessd/tests/room_two_sessions.rs` is the live one. Two `Daemon`s - two registries, stores,
sockets and workspaces, a canned model behind each - against the shipped `Coordinator`, and the whole
chain in one run: A posts a need with the tool, B claims the head and answers it with a `post` under
the same tag, and A is woken by that answer with nobody typing into it and reads it back with `room
queue`. Two sessions only share a queue inside ONE project (the same repository, slice 6 below), which
is why the fixture's workspaces are plain directories - they both land in the box's own room.

Nothing else in the tree runs two daemons against one room, so a break in the hop from a frame to
another session's turn fails there rather than on somebody's screen. A person doing the same by hand:
`letibot --room`, then a session in each of two worktrees of one repository, a prompt in each, and
`/pane room/room` to watch the queues.

## The coordinator (slice 2, as built)

The room is a whole `harnessd --room` (`crates/harnessd/src/room/coordinator.rs`), serving
`letibot_room::Server`: an accept loop, one read loop per connection, `Hello` at the door, and the room
behind one `Mutex`. Its clock thread runs `sweep(now)` every five seconds and the lease's beat.

That lease is a file, `<data dir>/room-<host key>.lease`, with `host/pid/<ns>` for an owner, a ten-second
beat and the merge queue's ninety-second TTL; a refusal names the live holder. A file and not a row,
because the room keeps no store. The names all derive from the host (socket, lease,
`s-room-<host key>`), so two boxes never collide.

### Which seats carry `room`

The tool is seated on `m1_orchestrator` (the default top-level seat, and the coordinator's own) and on
`leticode`, and nowhere else: a subagent's words go through the session that spawned it, and the room
refuses a subagent at its door. A session on any other seat does not see the tool at all, because
`Registry::resolve_role` keeps only the tools a role NAMES. This shipped wrong once, with the name in a
roles table no seat resolves, and every raised item froze for ever, silently;
`crates/harnessd/tests/room_coordinator.rs` now rules on a raised item through the tool.

### The launcher starts it, and the lease decides which one

`scripts/letibot`'s `room_ensure` starts a coordinator before any session opens, because the tool is
seated at `Harness::open` from the socket that exists then, and the tool cannot do it: a daemon that
forked another daemon would be a second `harnessd` under a lease it does not hold. Two runs at the same
instant start two coordinators and exactly one wins, `letibot_room::lease` refusing the other by name;
what the launcher owns is the key, `room-<host key>.sock` derived in bash from `uname -n` and pinned to
`lease::host_key_of` by a test, since a key that drifted would look for a socket no coordinator binds.

It is started detached with `idle_exit_after = None`, and it writes the flat record a folder's daemon
writes with `role: room`, so `--ls` and `--stop --all` see it. `letibot --room` makes sure one is serving
and says what it is; `--room --stop` stops it. A start that fails is said, not swallowed: the
coordinator's last words are read out of its log. No model flags are passed, so the arbiter is
`providers.toml`'s default or deepseek and a provider session needs no GGUF.

## The pane (slice 3)

`plugins/builtin/room` draws the queues per tag from the `room` feed
(`crates/harnessd/src/plugins/room_feed.rs`), built from the session's own connection
(`room::link::SessionRoom` reads `Request::Room` once a second on a thread of its own). Its `identity` is
`Server::identity()` verbatim: *started T ago and holding N item(s)*, and the sentence saying the room
keeps no store.

The four manual controls are rows. `claim` is `claim_item`, on behalf of the session whose pane it is, and
the item must still be the head. `release` and `abandon` are the same verb with two different sentences
left on the item's history, because a voluntary release is still an abandonment and still counted. `rule`
answers a question, and the decision rides in the row's entry (`q3:retry`, `q3:drop`, `q3:hand:<session>`).

A key is only offered where it would do something: a row held by somebody else carries no act, a row this
session holds leads with `release` and offers `x` for `abandon`, and a frozen item offers nothing because
its question is the `a` panel. The manifest gates both doors to a verb:
`actions = ["claim", "release", "abandon", "rule"]`, and `View::validate_for` refuses a row whose `enter`
OR whose letter names a verb that is not in it, which the daemon runs before a pane is drawn.

### The restart (slice 4), and what it says about itself

| what | where it is |
|---|---|
| the act asks the SUPERVISOR, the launcher, with the two verbs a person would type (`--room --stop`, then `--room`), never the coordinator on itself | `crates/harnessd/src/room/supervise.rs` (`RestartFrame`, `RoomSupervisor`, `Launcher`); `SessionRoom::restart` |
| what survives is stated: the `s` panel says a restart begins with no items at all | `plugins/builtin/room`'s `server()`; the feed's `restart` block |
| a marker says when and why: `<data dir>/room-<host key>.restart`, holding `at_ms`, `started_ms`, the cause, the session and the writer's own clause | `crates/room/src/restart.rs` (`Marker`, `Cause`) |

The coordinator cannot do it: the pane is fed by the instance it would kill, so a coordinator that stopped
itself mid-act could not report the outcome. Its own daemon is therefore never given a supervisor
(`coordinator::configure` clears `Config::restart_room`), and its pane draws the reason instead of a key
that would refuse.

`RestartFrame { at_ms, by, was_running, why }` is the act, written as one JSON line on the launcher's
stdin; `was_running`, read off the lease before the act, tells a restart from a start. The causes are
`asked`, `daemon` (nothing was running) and `crashed` (an instance change nobody recorded), and the
instance is the lease's `started_ms`, a number two readers can compare where the identity sentence is
prose whose age moves every second. A refusal is `room_restart_refused` with the launcher's own last line
in the sentence and nothing written to the marker, and the success is `room_restarted`.

## The reminder, as built

`crates/harnessd/src/room/remind.rs` joins `reminders` to the session's own todo board on the daemon's
idle tick, because a board may only be written by the thread that drives the harness: `Sessions`'
per-session idle arm (`OwnWork::TimedOut`, where `deliver_due_nags` runs) calls
`Harness::fill_room_board` every `ROOM_PASS_EVERY` (5 s, the room's own sweep interval).

One row per item the session holds, in the session's own half:

```text
content  you hold `ram` #3, 8 GB, ~10 min     <- tag, number, body, and NOTHING that moves
by       model                                <- the nag draws it as `yours`
status   in_progress
needs    [Room { item: 3, working: bool }]    <- the room's edge, always present
```

The words carry no clock, and that is the whole of *one row per item, updated not appended*: a row's
identity on this board is its exact `content`, so `… for 4m` would be a new row every pass. Everything
that moves lives in the edge's `working`. `true` means the holder told the room what it waits for and its
claim's beat is fresh: the row is BLOCKED and `waits_on_the_room` filters it out of the idle check. `false`
means no declared wait, or a beat that stopped: the row is READY and the check names it, *you hold `ram`
#3, 8 GB, ~10 min, yours (in progress) … mark it done with `todo_write`'s `update`*. Two loud facts, and
they are different: nothing outside is going to move the item, or the room is about to take it.

The edge is on every room row, loud or quiet, and that is what makes retirement survive a restart:
`Harness::open` rebuilds the board from the store, and `TodoBoard::set_room_rows` retires a row whose room
edge names an item this session no longer holds. It is the only door on the model's half that can remove a
row: a room row is the ROOM seen from a session, not a plan. Top-level only, twice: `Harness::open` gives
a session with a `parent_session_id` no link at all, and `fill_room_board` refuses one by name.

## Slice 5, as built: one agent per tag, and what a turn costs

A tag can be talked to about itself: one subsession per tag, owned by the host, idle without a model call
and taking a turn only when it must rule. A claim, a healthy beat and a finished item are comparisons, not
judgements; only a dispute earns a turn.

| what | where it is |
|---|---|
| the name is derived, never invented: `s-room-<host key>-<tag>`, and `--session host:ram` resolves to it | `crates/room/src/lease.rs` (`tag_session`, `tag_of_session`); `harnessd::room::session_named`; `cli`'s `--session` |
| one agent per tag, for every tag with a live item and never a second one for one tag | `crates/harnessd/src/room/agents.rs` (`ensure_tag_agents`) |
| a bell per tag: the server addresses an event to that tag's agent, and the connection keeps only its own tag | `crates/room/src/server.rs` (`route`), `crates/harnessd/src/room/link.rs` |
| re-seating, counted, drawn in the feed's per-tag `agent` block | `letibot_room::AgentLedger`; `Response::Room { agents }`; `plugins/builtin/room`'s `agent_cell` |

The suffix is an escape, not a sanitisation: `a-z`, `0-9` and `-` are kept, `_` is written `__`, and every
other character is `_` plus two hex digits per byte of its UTF-8. So `8 gb` and `8-gb` stay two tags with
two agents, `RAM` and `ram` are one (the room's own `normalise_tag` is the one function both read), and
`tag_of_session` inverts it exactly.

A re-seat is: *dead* means the registry no longer holds it, so the upkeep seats it again through
`Registry::create_under` with the box's owner as the parent, which is what makes it a subsession the picker
draws under `host`. A first seat is zero and every return is one more, and `seat` is separate from
`reseat` so a caller that has not checked liveness cannot inflate the number.

Three structural facts keep the deterministic path from opening a turn: `ensure_tag_agents` takes no bell;
a tag's agent hears its own tag and only `Raised` and `Asked` ring it, while a post, a claim, an
abandonment and a `done` arrive as notes; and a session seated and never woken runs no turn, because
`Harness::wake` with an empty notice list answers `Ok(None)`.

A tag's agent is not a claimer. Still open: nothing closes it, so a box with hundreds of finished tags
keeps hundreds of idle sessions; it does not rule its own disputes yet; the re-seat count is drawn and not
raised; and its notes are unbounded until something rings it.

## Slice 6, as built: a room is a project's, and its membership is the tree

The tree is `<project>/<top-sessions>/<subagents>*|system/`, and it already existed in
`crates/tui/src/app/session.rs`'s `App::placements()`. The gap this closed was that two unrelated sessions
had no way to find each other: a message door takes a session id, and nothing in the tree will name one.

| what | where it is |
|---|---|
| a project's key is the tree's own: the twelve hex characters `scripts/letibot` cuts and `s-gatekeeper-<key>` is spelled from | `crates/room/src/tree.rs` (`project_key`), pinned to `workspace_key` by `crates/harnessd/tests/room_tree.rs` |
| a room's name derives both ways (`<host key>/<project key>`), and `room_of_name` checks the round trip AND the key's shape | `tree.rs` (`room_name`, `room_of_name`) |
| one room per project on the one socket, and a session names no room: its daemon derives the project from the root of its chain | `crates/room/src/server.rs` (`Inner::rooms`, `project_of_hello`), `crates/harnessd/src/harness.rs` (the `room_link` arm) |
| the membership is the tree: the project's sessions, their subagents at any depth, and `system`'s groups | `crates/harnessd/src/room/roster.rs`; `Request::Members` -> `Room::set_members` |
| two projects never leak: routing is filtered by room, and a question says which room asked it | `Server::route`, `Question::project`, `Request::Rule`'s `project` |

`project_key` is `workspace_key`'s rule written a second time, because the room crate holds no store and no
`tokencore`, and the two spellings are pinned to one table by a test, exactly as `lease::host_key_of` is
pinned to the launcher's bash.

The default project is a project: `DEFAULT_PROJECT` is the empty key, its room is the bare host key, and it
is where a one-shot, a fixture, the arbiter and any session the daemon could not place live. A membership
is a list of sessions with the group the tree draws each in, its depth under the project, its parent, and
whether the daemon holds it live or only on disk; it is not the connections, since a subagent never opens
one, and the daemon's tree replaces what the room held rather than merging into it.

Still open: the pane does not draw the tree, since `RoomSnapshot.members` carries it; `letibot --sessions`
still prints the five groups, because this slice makes the ROOM agree with the tree, not the CLI agree
with the room; a tag's agent is still the box's, since `tag_session` has no project in its name; and no
project gets an agent of its own.

## What the state machine assumes

- One clock, given as `now` in milliseconds by the caller. A clock that goes backwards is survived, not
  corrected: staleness is `now.saturating_sub(beat_ms)`, so a beat from the future reads as fresh.
- Liveness is two facts, both given: the beat (it is working) and the caller's word (it is gone). The room
  never asks the OS, never reads a store, never guesses.
- One writer: every mutating method is `&mut self`, so the process that owns the room is the only writer
  by construction.
- No durability. A restart empties the room, and the pane states it; a journal is a later answer if the
  operator wants one.

## Still owed

1. ~~Restart the server~~: built, and the pane's fifth control asks the launcher, not the coordinator.
2. ~~A reminder row on the board~~: built (see *The reminder, as built*).
3. The room's first start costs a terminal line: a box with no key for the arbiter's provider says so on
   every run that finds no coordinator, because a launcher may not remember a refusal on the operator's
   behalf. A once-per-box note (a stamp file beside the lease) would be the fix, and it needs its own
   decision about what clears it.
4. A daemon the bridge started has no room: `leticode_start` spawns `harnessd` itself rather than going
   through `scripts/letibot`, so a box whose daemons all came from the bridge has no coordinator until a
   launcher run, the same shape as R102's missing record and the same fix.

Also open, in the slices above: a tag's agent is never closed, does not rule its own disputes, and its
notes are unbounded (slice 5); the pane draws no membership tree, and a tag's agent is still the box's
rather than a project's (slice 6).
