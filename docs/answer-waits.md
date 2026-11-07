# Answer waits - a row that waits on another session's answer

Status: built, all eight steps. The room holds the ask at `ROOM_PROTOCOL_VERSION` 10
(`crates/room/src/chat.rs`, `server.rs`, `wire.rs`), the edges are step 2's and 3's, the verbs and the
link are `crates/harnessd/src/room/link.rs`, the passes are `crates/harnessd/src/room/asks.rs`, and
the end-to-end test is `crates/harnessd/tests/room_asks_e2e.rs`. `docs/room.md` and TODO.md's R136 row
are the operator's half. What is NOT built: a control in a head's ask list for the operator's own
withdrawal - the operator's seat can send `withdraw` over the wire and no pane offers it yet.

The plan below is the one that was followed, step by step, so the numbered sections are history as
much as design.

A row on session A's board waits on an answer from session B, asked in a room. B gets a row on its
own board saying A asked and A waits, and is nagged until it answers or declines. If B's daemon goes
away for 5 minutes, the operator is asked to restart B. The ask stays open. It ends only by B's
answer or decline, by A or the operator withdrawing it, or by B being shut down for good (its
session deleted), which is the one `DOWN`.

The assumption is that agent graphs are stable structures on one host, with something like a
service agreement between their members: a stopped agent is a fault to repair, not an answer.
Timeouts only escalate to the operator, they never settle. Cross-host identities are not stable yet
(see the TODO row on migrations), so asks are same-host only.

<!-- Later: todo graphs that span projects, an ask edge being the first cross-project edge. TODO R137. -->

The operator's rulings, 2026-11-07:

1. Only the explicit `answer` verb settles. B must know it was asked and that A waits, so the ask
   becomes a todo row on B's board.
2. B may decline.
3. Claude Code seats can be asked, through a bridge `room_answer` tool.
4. and 5. No settling timeout. After 5 minutes gone, an operator ask: *B stopped, please restart
   it*. The same for an asker gone for long. A waits-on-B edge goes away only by explicit cleanup
   (A or the operator) or B's explicit shutdown.

## Decisions

### The room holds the ask

The room tracks every open ask, the way the Erlang runtime and not the two processes holds a
monitor. It is the one process that sees every session's connection, sees B's answer when A is not
listening, and outlives either daemon's restart.

An ask is server-level state beside the chat, not a `Room` (project) item, because the host room is
box-wide and the connection list is the server's. It is persisted as two new `chat::Record` lines,
`Ask` and `AskState`, in the chat store, so a coordinator restart keeps it.

The ask's id is the `seq` of the chat message that carries the question. One number is then the ask,
the thread its answer goes in, and what `read {"thread": N}` shows. The question text is also kept
on the ask, so a trimmed ring loses nothing.

```text
AskView { seq, room, asker, target, question, asked_ms,
          gone_since_ms: Option<u64>, escalated: bool,
          state: Open | Answered { text, ms } | Declined { text, ms } | Down { ms }
               | Withdrawn { by, ms } }
```

### Wire

`ROOM_PROTOCOL_VERSION` moves to 9. Six frames:

- `Ask { id, room, to, question }` - posts the question as a `Say` to B (kind `session`, `to: B`, a
  ref `{kind: "ask"}`) and records the ask. Answered with `Asked { id, ask: AskView }`.
- `Reply { id, ask, text }` - B's answer. Posted as a thread reply to `seq`, addressed to A, and the
  ask becomes `Answered`. Only the target may send it.
- `Decline { id, ask, text }` - B will not answer. Posted the same way, the ask becomes `Declined`.
  Only the target may send it.
- `Withdraw { id, ask }` - the asker or the operator takes it back. A system thread note tells B.
- `Retire { id, session }` - B's daemon says B is shut down for good (session deleted). Every open
  ask of B settles `Down`. Sent only by the daemon that owns B.
- `Asks` (a read) - `{ by_me: Vec<AskView>, of_me: Vec<AskView> }` for the calling session.

### Spelling and storage on A's row

A new `TodoNeed` variant, in all three copies (`tokencore::store`, `sessionlog::event`, the wire):

```rust
/// A question to another session, carried by the room as ask `ask`.
Ask { ask: u64, of: String, room: String, question: String },
```

`wait_on` spells it as:

```text
todo_write {"wait_on": [{"content": "<the row>",
             "ask": {"session": "serenedb", "room": "host", "question": "which port?"}}]}
```

`room` is optional and defaults the way `say` does, to the asker's project room. The ask is made
synchronously through the session's room link before the edge is written, so every refusal reaches
the model in the same call. The todo tool gains an optional `RoomLink`, the way it holds a child
resolver.

`needs` in `todos` cannot spell `ask` (refused by name, as `answered` is): asking is a side effect,
and only `wait_on` may make it. A restated row keeps its `Ask` edges, as `keep_answers` keeps
answers, or a restatement would drop the wait while B is still nagged. `detach` takes them off.

Settled, the edge is replaced in place by `TodoNeed::Answered { question, reply }`, with one new
field `ask: Option<u64>` (serde default, so old rows read). Reason: a met edge that keeps its words
already exists, with its render, restatement and check paths. The reply says which outcome it was:

```text
`serenedb` answered: 5433
`serenedb` declined: not my area, ask arango
`serenedb` was shut down for good (its session was deleted) - no answer will arrive
the ask was withdrawn by the operator
```

### Board and check on A's side

- Render: ``the session `serenedb` - asked "which port?" (ask #4211 in host)``.
- `unmet_needs`: `Ask` is unmet with a new `NeedFate::PeerAsked`, treated as `OperatorAsked` is: a
  root in `all_on_the_operator`, listed as blocked, the rest of the plan keeps going, and the
  sentence names the detach door. Reason: a peer and a person are both outside the plan's hands.
- When a settle lands, A gets a note and a bell, as `operator_ask::answer_line` does:
  `[answer] on "<row>": you asked serenedb "which port?" - 5433.` or `[down] on "<row>": ...`.

### B sees it and is nagged by its own board

The question is addressed to B, so it wakes B once (`wake::chat_sentence`, `names_you`). Then the nag is a row on B's board, put there by the existing room pass
(`Harness::fill_room_board`, every 5 s) from `Asks.of_me`:

```text
`arango` asked you #4211 and its row "wire the port" waits on your answer: which port?
answer with `room answer {"ask": 4211, ...}` or decline with `room decline`    (yours, in progress)
```

The row names the asker by its handle, the asker's waiting row, and both verbs, so B knows a
peer is blocked on it, not just that a message arrived.

The row carries a new met edge `TodoNeed::Owed { ask: u64 }`, the pass's key for it, the way
`Room { item }` keys a reminder row. `set_room_rows` retires by either key. The row is ready, so the
idle check nags it on the existing `TodoNagClock`: armed on idle, said once per change. No second
clock and no room-side reminder. Reason: one nag mechanism, already rate limited.

B answers with a new `room` verb, `answer {"ask": 4211, "text": "5433"}`, which sends `Reply`, or
declines with `decline {"ask": 4211, "text": "..."}`, which sends `Decline`. A plain thread reply does
not settle the ask. Reason: *looking at it* must not unblock a plan. It still reaches A as ordinary
chat. Marking the row done without answering does nothing: the next pass puts it back while the ask
is open.

A Claude Code seat (`cc@...`) can be asked. It has no board, so the bridge gets `room_answer` and
`room_decline` tools and `room_asks` lists what it owes. Its nag is the channel push of the question
plus the operator escalation below.

### A learns the answer from its pass

A's room pass reads `Asks.by_me` every 5 s and applies every settled ask to every row carrying its id,
then flushes the board. That pass is the only applier: it is idempotent, and it covers A restarting,
the room restarting, and B answering while A's daemon is not connected. The chat push is a wake, not
the source of truth.

The same pass withdraws: an open ask in `by_me` that no open row of A's carries (the row was done,
cancelled, detached or re-worded) gets a `Withdraw`. That is the counterpart of the operator flow's
*the model took its question back*.

### Top-level sessions are services

This design rests on one contract: a top-level session is a service. It has a stable name on its
host, it is expected to be running, and a stopped one is a fault the operator repairs. Subagents
are not services: they are owned by their parent and cannot be asked.

So liveness never settles an ask. It only escalates.

Gone means the session holds no room connection. The connection is the daemon's liveness: B's
daemon holds one connection per top-level session, it closes when the daemon exits, and it closes
when B's session is closed (its `Client` is dropped). Not `session_writer` (its `heartbeat_ms` is
the claim time, not a beat) and not `daemon_life` (per daemon, and a live daemon may have closed B).

The server keeps `gone_since: BTreeMap<session, ms>`, set when a session's last connection closes
and cleared when it connects. The existing 5 s clock marks an open ask `escalated` when its target
has been gone for `ASK_GRACE_MS = 300_000`, and raises an operator ask: *`serenedb` stopped and
`arango` waits on its answer to "which port?" - please restart it.* A restart inside the grace is
invisible. When B reconnects the escalation is cleared and its card withdrawn. On a coordinator
start every target not yet connected starts its grace at the start time.

The asker gone is the same: after the grace the operator is asked to restart A, and B's row stays,
marked *the asker is not running*. B can still answer, the answer waits in the room for A's pass.

An ask ends in exactly these ways:

- B answers (`Reply`) or declines (`Decline`).
- A withdraws it: its row was done, cancelled, detached or re-worded, and A's pass sends `Withdraw`.
- The operator withdraws it, from the escalation card or the head's ask list.
- B is shut down for good: its session is deleted, and its daemon sends `Retire` before closing.
  This is the only `DOWN`. A deleted asker withdraws its asks the same way.

A daemon exit, `--stop`, a crash or a reboot is not a shutdown for good. Those are restarts the
operator owes.

The room itself being unreachable settles nothing. Room down is not B down. A's row stays blocked
and the check says the room is unreachable.

## Edge cases

| Case | Rule |
|---|---|
| B is A | Refused: *you cannot wait on your own answer*. |
| Unknown handle | Refused with the known top-level handles. `resolve_name` passes an unknown name through for `say`, so `Ask` also requires a tree member. |
| Ambiguous handle | Refused with the candidates `resolve_name` returns. |
| B is the operator | Refused, pointing to the `operator` wait. |
| B is an external (`cc@...`) | Accepted. The bridge's `room_answer`, `room_decline` and `room_asks` serve it. No board row, the channel push is its nag. |
| B or A is a subagent | Refused: a subagent has no room connection. A child waits on its parent's answer through its parent. |
| B cannot see the room | Refused, naming `host` or an `Invite`. |
| B not connected at ask time | Accepted. The grace starts at the ask, and the reply says B is not connected now. |
| B on another host | Refused: identities are not stable across hosts yet (TODO R138). |
| Same asker, target and question, open | The room returns the existing ask. Several rows carry one id and one settle releases all of them. |
| Another asker, same question | A separate ask and a separate row on B's board. |
| A restarts | The edge is on the persisted board, and the first pass after reconnect applies what settled meanwhile. |
| Room restarts | Asks come back from the chat store. Grace clocks restart at the room's start. |
| Room ran with no store, or pruned the ask | The pass finds an id the room does not know and raises an operator ask rather than settling. |
| Settled asks | Kept 24 h after settling, then dropped at the chat store's rewrite. Open asks are never dropped. |

## Relation to existing waits

`Operator` is A's side: asked, blocked, settled into `Answered`, one line to the model. `Room { item }`
is B's side: room state turned into a pass-owned row by `fill_room_board`. `Child` is the liveness
precedent. Nothing is duplicated, `Ask` and `Owed` are new keys on existing paths.

## Implementation plan

Each step lands red tests first.

1. **Room state.** `crates/room/src/chat.rs` (records, `AskView`, store restore and rewrite),
   `server.rs` (`gone_since`, clock sweep, escalation, dedup), `wire.rs` (six frames, version 9). Tests in
   `crates/room`: an ask settles `Answered` on `Reply` and refuses a `Reply` from anyone else. A
   target closed and back within the grace stays open. A target gone past the grace stays open and
   is `escalated`, and the escalation clears on reconnect (clock injected, no sleeping). `Decline`
   settles. `Retire` settles every open ask of that session `Down`, and only its own daemon may send
   it. Asks survive a server restart over a store. A duplicate returns the same id. Self, unknown,
   ambiguous, operator, subagent and other-host targets are refused.
2. **TodoNeed.** `tokencore/src/store.rs`, `sessionlog/src/event.rs`, the conversions in
   `harnessd/src/sessions.rs` and `harness/turns.rs`, `Answered.ask`. Tests: round trip through
   `needs_json`, and an old row without `ask` reads.
3. **Todo tool.** `crates/tools/src/builtins/todo.rs`: `wait_on` `ask`, the `RoomLink` seat,
   `needs` refusal, `keep_answers` keeping `Ask`, `detach`, `unmet_needs`/`NeedFate::PeerAsked`,
   `render`, `blocked_line`, `all_on_the_operator`, `settle_ask`. Tests in a new `todo/tests/ask.rs`
   modelled on `operator.rs`, with a fake link.
4. **Room link and verb.** `harnessd/src/room/link.rs` (`request_of`, `answer_sentence` for
   `answer`, `decline` and `Asks`), `Retire` sent when a session is deleted,
   `crates/tools/src/builtins/room.rs` (the verbs and their schema text), and the bridge's
   `room_answer`, `room_decline`, `room_asks` in `crates/bridge`. Tests: the verbs become `Reply`
   and `Decline` frames, and deleting a session sends `Retire`.
5. **Passes.** `harnessd/src/harness/room.rs` and `room/remind.rs`: `Owed` rows from `of_me`,
   settles and withdrawals from `by_me`, the note and bell, and the operator ask raised and
   withdrawn on `escalated`. Tests: pure functions over `AskView`
   lists, in the style of `remind::rows`.
6. **Heads.** `crates/tui/src/ui/panes/todos.rs` and `app/todos.rs`: draw `Ask` and `Owed`.
7. **End to end.** Two sessions in two daemons against one coordinator in a scratch dir. A asks, B
   answers, A's row is released. Then B's daemon is killed: after the grace the operator ask is
   raised and A's row stays blocked. B restarts and answers, the card goes and A's row is released.
   Then B's session is deleted and a second ask settles `Down`. The grace comes from config so the
   test runs in seconds.
8. **Docs.** `docs/room.md` (the verbs) and the `todo_write` schema text.
