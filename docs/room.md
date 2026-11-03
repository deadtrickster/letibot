# The room - one host's chat, and one queue per tag

A room is a place where the sessions on this host, the operator and outside tools talk. There is one
**host room** that every top-level session is in, and one **project room** per project. Beside the
chat sits a tagged queue for the things only one session may have at a time: RAM, a port, the staging
database, a reviewer. Chat is the primary thing and the queue is unchanged by it.

The first half of this page is how to use it. The contract section is what the `/rooms` view and the
MCP bridge build on. The rest is the long form of `crates/room`.

## Using the room

### Talking

A session's `room` tool has `say` and `read`. The room defaults to the session's project room,
`host` names the host room, `project` names the project room explicitly.

```text
room {"verb": "say", "room": "host", "text": "who has the disk?", "to": "s-b"}
room {"verb": "say", "text": "build is green", "todo": "s-a/write the doc"}
room {"verb": "read", "room": "host", "last": 20}
room {"verb": "read", "thread": 14}
```

A message that names a session (`to`, or `@session-id` in the text) wakes that session's daemon and
starts a turn with a one-line notice. So does the operator's unaddressed message. Everything else is
recorded and shown in the pane and in `read`, and wakes nobody: a room of agents is chatty, and a turn
per line would be a turn nobody wanted. A session is never woken by its own messages.

The operator is in every room. A head asks its daemon, which speaks to the coordinator as `operator`.

The harness also speaks, as the author `todo` or `queue` with kind `system`. A todo row filed, started,
done, postponed or cancelled on a session's board, and a review or merge queue row that moved, each
post one short line to that session's project room (or the host room when the session has no project)
with a `ref` to the row. Nothing else is copied: the todo board and the merge queue stay where they are.

### Two sessions, one queue

Two sessions of one repository (two worktrees, a package and its reviewer) are in one project room
because a room is the project's: its name is derived from the root of the session's chain, so nothing
is joined or configured.

One of them wants the scarce thing and posts a need. The room never parses the body.

```text
room {"verb": "post", "tag": "ram", "body": "8 GB, ~10 min"}
  -> posted #3 under `ram`, open
```

The other reads the queue and claims its head. A tag is a lock, so a claim is refused while somebody
else holds it, and the refusal names the item, the holder and how long it has been held.

```text
room {"verb": "queue", "tag": "ram"}
room {"verb": "claim", "tag": "ram"}
```

A claim is a lease with a beat, so a holder that dies does not keep the tag. While a turn runs the
daemon beats on the claim's behalf. A claim whose beat stops for ninety seconds is taken back, and the
item returns to the head of its tag with its abandonment count up.

While it works, the holder says what it is waiting for, which is the difference between *this is mine*
and *something outside moves this*:

```text
room {"verb": "waiting", "item": 3, "text": "the build to finish"}
room {"verb": "done", "item": 3}
room {"verb": "abandon", "item": 3, "why": "the box ran out of disk"}
```

A post, a claim, a finish, an abandonment, a raised question and a ruling each arrive as a sentence
and start a turn on the sessions they concern. A beat does not. Reads:

```text
room {"verb": "queue", "tag": "ram"}
room {"verb": "holding"}
room {"verb": "reminders"}
```

An answer to a post is a `post` under the same tag. The item and its tag are the thread.

### Rules of the queue

- A tag is a resource, not a category. Two jobs that may run at once need two tags (`merge-a`,
  `merge-b`). There is no priority number. Urgent work gets a tag of its own (`ram-urgent`).
- The head is first come, first served. `claim` takes the head of a tag, `claim_item` takes a named item
  only while it is still the head.
- An abandonment is not a withdrawal. It counts, it goes on the item, and the arbiter reads it. A
  voluntary release from the pane is the same verb with a gentler sentence.
- Three abandonments raise a question for the room's arbiter, which rules `retry` (keep the count),
  `drop` (take the item out for good) or `hand` (give the claim to a named session, now). A raised
  item freezes and its tag's queue moves past it.
- A dispute is how a claimant says *I am still on this* after its beat went stale and the item was
  given to somebody else. The arbiter decides which of the two has it.
- A subagent has no room, chat or queue. Its words go through the session that spawned it. Two
  projects never share a queue, and a question says which room asked it.

### Watching it

`/pane room/room` draws one line per tag, one row per item (claimant, age of its beat, abandonment
count), the open questions, the arbiter and who is connected, the project's tree, and the room's
identity sentence: *started T ago and holding N item(s)*, which also says that the room keeps no store.
The daemon's feed carries the newest chat the session heard in `chat`, for a pane that draws it.

A row offers only the verbs that would do something there: `claim` on the head, `release` and `abandon`
on a claim this session holds, `rule` on a frozen item whose question is open. The pane also carries the
room's restart, which asks the supervisor that starts coordinators, because a control that killed its
own feed could not say what happened.

A holder also sees its work on its own todo board: one row per item it holds, loud when the claim is
ready to act on (nothing outside moves it, or its beat stopped) and quiet when it declared a wait and
its beat is fresh.

### Starting the coordinator

The coordinator is a whole `harnessd --room`, started before any session opens: `letibot --room` makes
sure one is serving and says what it is, `letibot --room --stop` stops it, and its log is
`~/logs/room-<host key>.log`. `scripts/letibot` runs it on every start. A daemon the MCP bridge starts
(`leticode_start`) asks the same launcher first (`letibot --room`, from `$LETIBOT_ROOM_SUPERVISOR` or
`PATH`), so its sessions are in the rooms too. A failed start is said on stderr and the daemon starts
anyway: a session with no room works, and its `room` tool refuses by name.

## The contract

This is what a client of the coordinator may rely on. Chat lives in `crates/room/src/chat.rs` and the
server in `crates/room/src/server.rs`. Frames are one line of JSON, `Request` and `Response` in
`crates/room/src/wire.rs`, tagged by `type`.

### Rooms

| id | who is in it |
|---|---|
| `host` | every top-level session on the host, the operator, every external |
| `project/<key>` | the top-level sessions of that project, the operator, every external |
| `dm` | not a room: the pseudo id a principal reads its direct messages from |

`<key>` is the project key: twelve hex characters of the SHA-256 of the project's canonical root (the
same key `s-gatekeeper-<key>` and `scripts/letibot` use). A session in no project (the daemon could not
place it) is in the host room only. A project room exists once a session of that project has connected.
Subagents are in no chat room. The roster of a room is the tree's top-level sessions for it plus
everything connected.

A client lists the rooms it may see with `rooms`. A session gets `host` and its own project room. The
operator and an external get `host` and every project room that exists. Each row carries `members`,
`last_seq` and `unread` (messages after this principal's inbox cursor that are not its own).

### Identity, seats and the operator

A connection says its **seat** in `hello` (`seat`, default `session`):

| seat | name | rooms | notes |
|---|---|---|---|
| `session` | the session id | host and own project | any name except `operator`; never a subagent |
| `operator` | exactly `operator` | all | the person, through a head |
| `external` | a handle: `a-z`, `0-9`, `-`, at most 32, not starting `s-`, not `operator` (for example `claude-code`) | all | an outside tool |

A message's `author` and `kind` (`session`, `operator`, `external`, `system`) come from the connection,
never from the request, so a client cannot post under another name. A wrong name for the seat is
refused at `hello` (`reserved_name`). Only a `session` seat may send `notify` (a daemon reporting a
fact), only as source `todo` or `queue`, and only into the host room or its own project room.

**How the operator is authenticated on one host.** The coordinator's socket is the user's own (mode
0600), so any process of that user can open it. What stops a model from speaking as the operator is
that no model-facing door reaches the operator seat: the `room` tool speaks only over the session's own
connection, whose author is the session. A head reaches the seat through its daemon with
`ClientFrame::RoomChat`, and the daemon refuses that frame on any connection that came out of a tool
call, using the socket peer check (`crates/sessionlog/src/peer.rs`, `door`). The daemon holds one
operator connection to the coordinator (`crates/harnessd/src/room/operator.rs`), opened on the first
request, and forwards only chat requests (`rooms`, `say`, `dm`, `read`, `roster`, `inbox`, `ack`).
Inside a command boundary the coordinator's socket and its lease and marker files are hidden: absent
or replaced by `/dev/null` under bwrap, and denied for connect and file access under seatbelt, so a tool
cannot open the socket at all (`docs/boundary-and-adjudication.md`). Limit: an unconfined session has no
boundary, so a process of the user there that opens the coordinator socket itself and claims
`seat: operator` is not stopped. The peer check on the daemon is its only guard.

### Messages

```json
{"seq": 14, "ts": 1793735794000, "room": "host", "author": "s-a", "kind": "session",
 "to": "s-b", "thread": 9, "refs": [{"kind": "todo", "id": "s-a/write the doc"}],
 "text": "who has the disk?"}
```

- `seq` is one counter across the whole log (all rooms and DMs), starting at 1. It is the cursor.
- `to` names one principal. `thread` is the `seq` of the message answered, in the same room.
- `refs` point at rows other parts of letibot own: `todo` (`<session>/<title>`) or `queue` (the queue
  row id). At most 8 per message, each id at most 200 bytes. The row is not copied.
- `text` is trimmed, non-empty and at most 4000 bytes. Longer is refused (`chat_too_long`), never cut.
- A name nobody on the host answers to (`to`, or the target of a `dm`) is refused (`unknown_addressee`).
  The operator is always known, an external once it has connected.

A DM is a message with room `dm` and a `to`. It is readable by its author and `to` only.

### History

Each room keeps its newest **500** messages in memory (DMs together keep 500). The oldest is dropped
first. Nothing is written to disk: a coordinator restart empties every room and every cursor. A read
says `trimmed: true` when messages after the asked-for `seq` were dropped.

### Requests

| request | answer | notes |
|---|---|---|
| `hello {protocol, speaker, room, seat}` | `welcome` | `room` is `<host key>/<project key>` for a session |
| `rooms` | `rooms {rows}` | the rooms this connection may see |
| `say {id, room, text, to?, thread?, refs?}` | `said {id, message}` | `room` empty is the project room, or `host` with no project |
| `dm {id, to, text, thread?}` | `said` | |
| `notify {id, room, source, text, refs?}` | `said` | daemons only, kind `system` |
| `read {room, since?, last?, thread?}` | `history {room, rows, last_seq, trimmed}` | moves no cursor. `last` defaults to 200, the most one read returns. `thread` returns the root and replies |
| `roster {room}` | `roster {room, rows}` | `name`, `kind`, `connected`, `project`, `label` |
| `inbox {attention?, limit?}` | `inbox {rows, skipped, cursor, head}` | what was missed since the cursor. Moves nothing |
| `ack {seq}` | `acked {cursor}` | moves the cursor forward only, never past the head |

A repeated `say`, `dm` or `notify` with the same `(author, id)` returns the first message and appends
nothing, inside a window of 256 requests. Refusals are `refused {id, why}`; a room a connection may not
see is `unknown_chat_room`.

### Subscribing

A connected client is subscribed to every room it may see, and to its DMs. The coordinator pushes
`chat {message, names_you}` for each new message to every such connection except the author's.
`names_you` is true when the message is addressed to the reader or mentions it. The client's `on_chat`
hook receives it on the reader thread.

**Attention** is how much of that a reader acts on:

| attention | passes |
|---|---|
| `to_me` (default) | what names you, the operator's unaddressed messages, and DMs to you |
| `mentions` | only what names you |
| `all` | everything you may see, except your own |

A session's daemon applies `to_me` to decide whether to start a turn. The coordinator pushes all of it.

**Inbox replay.** Each principal has a cursor on the coordinator, set to the head of the log at its
first `hello` (a reader hears what happens next) and kept while the coordinator runs. After a gap, `inbox`
returns what the filter lets through after the cursor and counts the rest as `skipped`. The reader acks
once it has handled the rows. Delivery is at least once. A head, which has no push, polls `read` with
`since` or `inbox`.

### Through the daemon

`ClientFrame::RoomChat {client_request_id, op}` carries a room `Request` as JSON in `op` and is answered
by `ServerFrame::RoomChat {client_request_id, reply}`, where `reply` is the room `Response` as JSON or
`{"error": "..."}` when the coordinator is unreachable or `op` is not a chat request. It needs the
head door, so a tool call is refused.

### Versions

`ROOM_PROTOCOL_VERSION` is 6, the room's own number. `PROTOCOL_VERSION` (head to daemon) is 61.

### What is kept from flowy, and what is not

The local chat follows flowy's shape for talking (the verbs agents use: `say`, `dm`, `read`, `inbox`,
`roster`). It is one coordinator on this host.

| flowy | here |
|---|---|
| rooms | kept: `host` and `project/<key>` |
| one append-only log with a cursor | kept as `seq` over a bounded ring per room, in memory |
| `say --to`, `dm`, `--thread` | kept as `to`, `dm`, `thread` |
| `@name` mentions | kept: any `@name` in the text |
| attention (`--to-me`, `--mentions`) | kept as `attention` |
| inbox with a server-side cursor, replay | kept as `inbox` and `ack`; the push replaces the blocking waiter |
| never echo your own messages | kept |
| roster | kept, from the connections and the tree |
| `@seat/session` addressing | dropped: a seat here is one session, and the name is the session id |
| tokens, projects, roles, permissions | replaced by seats (above) |
| Postgres, event DAG, hybrid clocks, signatures, federation | left out: one host, one process, memory only |
| waiter kinds and presence (`tracked`, `forked`) | left out: `connected` is the socket |
| todos, notes, memory, findings, reports, attachments | left out: the todo board and the merge queue stay as they are, and a room line carries a `ref` to a row |

## The state machine

```
open --claim--> claimed --done--> done
  ^                 |
  +--abandoned<-----+          (beat stopped, session gone, released on purpose)
```

`abandoned` is not terminal: the item returns to the head of its tag, claimable again, with its count
up, and the state names the last thing that happened. `Item::claimable()` is the predicate. `done` ends
the item. `dropped` is the terminal a ruling can give a raised item.

A tag is a lock: one live claim per tag, and while it lives every other claim under the tag is refused
`Busy`, naming the holder and the item. The order is `(posted_ms, id)`. An abandoned item returns to the
head because it was posted before everything behind it.

A claim is a lease with a beat: `claimed_by`, `claim_ms`, `beat_ms`, 90 s, eighteen missed beats at a
five-second beat. It is lost in three ways, and they are different facts:

| how | when the room knows | what it records |
|---|---|---|
| `Room::sweep`, the beat stopped | `now - beat_ms > lease_ms` | `Why::BeatStale { age_ms }` |
| `Room::note_gone`, the session is gone | at once, from the daemon | `Why::SessionGone { says }` |
| `Room::abandon`, the holder gives up | at once, from the holder | `Why::Released { says }` |

A voluntary release is still counted. At `ABANDON_THRESHOLD = 3` the item is raised: the count stops
moving, it is no longer claimable, its tag's queue moves past it, and a `Question` is asked.

### Where the tie-breaker decides

The arbiter is a model, so it is asked a question with a decision in it. `Question::text` is the
question and `Question::facts` the evidence.

1. An item that has abandoned three claimants (`About::ItemRaised`): the question names the item, each
   claimant it lost and why, and who is queued behind it, and asks for `Retry`, `Hand { to }` or
   `Drop { why }`.
2. An abandonment under dispute (`About::ClaimDisputed`): the arbiter is asked which of two claimants
   has it. The room refuses the late beat by name.
3. Two items at one tag's head: `(posted_ms, id)` decides. A ruling overrides the ordering rule.

Two coordinators are answered with a lease on the coordinator, not a question. A ruling is refused
rather than half-applied: a `Hand { to }` naming a session the room has never met, a subagent, or a tag
another item holds leaves the item frozen and the question open.

## The queue's wire

`Hello { protocol, speaker, room, seat }` carries the session, the parent it speaks through, the room's
name and the seat. One connection per top-level session, because a push has an address and a close is a
fact: a session whose daemon died drops its socket and the coordinator calls `note_gone` at once. The
socket is the liveness of the daemon, the beat is the liveness of the work. `Welcome` carries
`Server::identity()`. A version mismatch is `Bye { why }` with both numbers and which side is older.

| frame | answer | idempotent because |
|---|---|---|
| `post`, `claim`, `claim_item` | `Posted`, `Claimed` | the reply is remembered per `(session, id)` |
| `beat`, `waiting`, `acknowledge` | `Beaten`, `Waited`, `Acknowledged` | a stamp or a set |
| `done`, `abandon` | `Finished`, `Abandoned` | a second is refused by name |
| `dispute`, `rule` | `Question`, `Ruled` | one open question per item, answered once |
| `members` | `Members` | a set: a repeat is the same tree |
| `queue`, `holding`, `reminders`, `questions` | `Queue`, `Holding`, `Reminders`, `Questions` | reads |
| `room` | `Room {identity, arbiter, connected, items, questions, agents, members}` | a read: the pane's |

Every mutating frame carries a client-minted `id`. The replay window is 256 replies per room. A request
id is not a clock: the window keys on `(session, id)`, and `room::link::request_id` makes ids unique.

`Claim::said` is *seen #1; working on it*, written by `acknowledge` when the claim is taken.
`Claim::waits_on` is what the holder waits for, written by `waiting`. A fact that means *this is mine to
do* must never be written into the field that means *this is not mine to move*, or the reminder that
says *finish it or release it* never fires.

## The bell, per client

A bell is something that can inject a turn. Four kinds of client could plausibly be woken by a room
event, and two of them cannot be.

| client | a room client? | can an event start a turn? | the mechanism |
|---|---|---|---|
| a leticode session (`crates/harnessd`) | yes, one `SessionRoom` per top-level session | yes | the reader turns an `Event` or a chat message that names the session into a sentence (`room::wake`), queues it and rings `Bell::ring_wake`; `Harness::wake` runs a turn with it |
| the head (`letibot-tui`) | no | no, the daemon wakes it | no socket. Its pane comes from the daemon's feed, its chat from `RoomChat` |
| the bridge (`letibot-bridge`) | no | no | a room event reaches it only through a turn. It starts the coordinator for the daemons it starts |
| a firecode VM agent | no | no | a subagent is refused at the door |

A tag's agent and the arbiter work the queue and are never woken by chat.

### Tests that cover the chain

- `crates/room/tests/chat.rs`: two projects and the host room on one coordinator. Each project's
  sessions see the host room and their own project room and never the other's. A host message reaches
  both. The operator sees and posts in all three. A session is refused as the operator.
- `crates/harnessd/tests/room_chat.rs`: the same through `SessionRoom` and the `room` tool, the
  operator through its seat, and the todo and queue lines.
- `crates/harnessd/tests/room_two_sessions.rs`: two daemons and the queue chain, a post answered by a
  post under the same tag, the asker woken.

## The coordinator

`harnessd --room` (`crates/harnessd/src/room/coordinator.rs`) serves `letibot_room::Server`: an accept
loop, one read loop per connection, `Hello` at the door, the rooms behind locks. One room per project on
the one socket, keyed by project key, and `DEFAULT_PROJECT` (the empty key) holds the sessions the
daemon could not place, the arbiter and fixtures. Its clock thread runs `sweep(now)` every five seconds
and beats the lease.

The lease is a file, `<data dir>/room-<host key>.lease`, with `host/pid/<ns>` for an owner, a ten-second
beat and a ninety-second TTL. The names all derive from the host (socket, lease, `s-room-<host key>`).

The tool is seated on `m1_orchestrator` and on `leticode`, and nowhere else. A session on another seat
does not see it, because `Registry::resolve_role` keeps only the tools a role names.

`scripts/letibot`'s `room_ensure` starts a coordinator before any session opens, because the tool is
seated at `Harness::open` from the socket that exists then. A daemon that forked another daemon would be
a second `harnessd` under a lease it does not hold. Two runs at the same instant start two coordinators
and exactly one wins. It is started detached with `idle_exit_after = None` and writes the record a
folder's daemon writes with `role: room`, so `--ls` and `--stop --all` see it. No model flags are
passed, so the arbiter is `providers.toml`'s default.

### Projects

A project's key is the tree's own, `crates/room/src/tree.rs` (`project_key`), pinned to `workspace_key`
by `crates/harnessd/tests/room_tree.rs`. A room's name is `<host key>/<project key>`, and
`room_of_name` checks the round trip and the key's shape. A session names no room: its daemon derives the
project from the root of its chain (`project_of_hello`). The queue membership is the tree
(`crates/harnessd/src/room/roster.rs`, `Request::Members`): the project's sessions, their subagents at
any depth, and `system`'s groups, replaced on every report rather than merged. Routing is filtered by
room, and a question says which room asked it.

### The pane, the restart and the reminder

`plugins/builtin/room` draws the queues from the `room` feed
(`crates/harnessd/src/plugins/room_feed.rs`), built from the session's own connection
(`SessionRoom` reads `Request::Room` once a second on its own thread). The controls are rows: `claim` is
`claim_item`, `release` and `abandon` are one verb with two sentences, `rule` answers a question with the
decision in the row's entry. The manifest gates both doors to a verb, and `View::validate_for` refuses a
row naming a verb outside it.

The restart asks the supervisor (`crates/harnessd/src/room/supervise.rs`), the launcher, with the two
verbs a person would type, never the coordinator on itself. `RestartFrame` is one JSON line on the
launcher's stdin. A marker, `<data dir>/room-<host key>.restart`, says when and why
(`asked`, `daemon`, `crashed`). The coordinator's own daemon is never given a supervisor.

`crates/harnessd/src/room/remind.rs` joins `reminders` to the session's todo board on the daemon's idle
tick: one row per item held, whose words carry no clock, with a `Room { item, working }` edge. `working`
true means a declared wait and a fresh beat, and the row is blocked. False means ready, and the idle
check names it. Top-level only.

### One agent per tag

A tag can be talked to about itself: one subsession per tag, `s-room-<host key>-<tag>`
(`tag_session`, `tag_of_session`), owned by the host, idle without a model call and taking a turn only
when it must rule. The suffix is an escape, not a sanitisation, so `8 gb` and `8-gb` stay two tags.
`ensure_tag_agents` seats one for every live tag and `AgentLedger` counts re-seats. An agent hears its
own tag and only `Raised` and `Asked` ring it. A tag's agent is the box's, since the name carries no
project.

## What the state machine assumes

- One clock, given as `now` in milliseconds by the caller. A clock that goes backwards is survived.
- Liveness is two facts, both given: the beat (it is working) and the caller's word (it is gone). The
  room never asks the OS.
- One writer: every mutating method is `&mut self`.
- No durability. A restart empties the queue, the chat rings and the cursors, and the pane says so.

## Still owed

- The head has no `/rooms` view yet. It can already list, read and post through `RoomChat`.
- The MCP bridge has no external seat yet. The seat and its `claude-code` handle exist in the
  coordinator.
- A head gets chat by polling `read` or `inbox`. There is no push to a head.
- The first start of the room costs a terminal line when the arbiter's provider has no key. A
  once-per-box note would fix it.
- A tag's agent is never closed, does not rule its own disputes, and its notes are unbounded. The pane
  draws no membership tree, and `letibot --sessions` still prints the five groups.
