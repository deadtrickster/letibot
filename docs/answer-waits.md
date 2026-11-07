# Answer waits - a row that waits on another session's answer

Status: design, not built.

A row on session A's board waits on an answer from session B, asked in a room. B is nagged until it
answers. If B's daemon goes away and does not come back within a grace, A is told B is gone and the
row is released, the way an Erlang monitor delivers `DOWN`. The operator's words: *"that <session
name> in <room name> gets the nag. If that waited-on session stops (harnessd exits), then it is the
usual thing - a notification that the session is gone and no answer will arrive. Can be done with a
timeout of say 2 minutes, so restarts are possible."*

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
          state: Open | Answered { text, ms, late } | Down { gone_since_ms, ms } | Withdrawn { ms } }
```

### Wire

`ROOM_PROTOCOL_VERSION` moves to 9. Four frames:

- `Ask { id, room, to, question }` - posts the question as a `Say` to B (kind `session`, `to: B`, a
  ref `{kind: "ask"}`) and records the ask. Answered with `Asked { id, ask: AskView }`.
- `Reply { id, ask, text }` - B's answer. Posted as a thread reply to `seq`, addressed to A, and the
  ask becomes `Answered`. Only the target may send it.
- `Withdraw { id, ask }` - the asker takes it back. A system thread note tells B.
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
`serenedb` is gone - its room connection closed at 14:02 and did not come back within 2 minutes; no answer will arrive
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
answer `s-a`'s question #4211 - which port?    (yours, in progress)
```

The row carries a new met edge `TodoNeed::Owed { ask: u64 }`, the pass's key for it, the way
`Room { item }` keys a reminder row. `set_room_rows` retires by either key. The row is ready, so the
idle check nags it on the existing `TodoNagClock`: armed on idle, said once per change. No second
clock and no room-side reminder. Reason: one nag mechanism, already rate limited.

B answers with a new `room` verb, `answer {"ask": 4211, "text": "5433"}`, which sends `Reply`. A plain
thread reply does not settle the ask. Reason: *looking at it* must not unblock a plan. It still
reaches A as ordinary chat. The nag row's text names the verb. Marking the row done without answering
does nothing: the next pass puts it back while the ask is open.

### A learns the answer from its pass

A's room pass reads `Asks.by_me` every 5 s and applies every settled ask to every row carrying its id,
then flushes the board. That pass is the only applier: it is idempotent, and it covers A restarting,
the room restarting, and B answering while A's daemon is not connected. The chat push is a wake, not
the source of truth.

The same pass withdraws: an open ask in `by_me` that no open row of A's carries (the row was done,
cancelled, detached or re-worded) gets a `Withdraw`. That is the counterpart of the operator flow's
*the model took its question back*.

### DOWN

Gone means B holds no room connection. The connection is the daemon's liveness: B's daemon holds one
connection per top-level session, it closes when the daemon exits, and it closes when B's session is
closed (its `Client` is dropped). Not `session_writer` (its `heartbeat_ms` is the claim time, not a beat) and not `daemon_life`
(per daemon, and a live daemon may have closed B).

The server keeps `gone_since: BTreeMap<session, ms>`, set when a session's last connection closes and
cleared when it connects. The existing 5 s clock settles an open ask `Down` when its target has been
gone for `ASK_GRACE_MS = 120_000`. A restart inside the grace is invisible. On a coordinator start
every target not yet connected starts its grace at the start time, since the room did not see when it
left.

A late answer: B returns after `Down` and sends `Reply`. It is accepted and posted to A. The ask
becomes `Answered { late: true }`, and A's pass replaces the gone edge's reply with the answer if the
row still carries it. Erlang delivers messages after `DOWN` too. B returning without answering
changes nothing: the ask is settled and B's nag row is retired.

The asker going away does not withdraw the ask, because its row is persisted and the answer is still
wanted. But `Asks.of_me` leaves out asks whose asker has been gone past the grace, so B is not nagged
for a session that is not there. The row comes back when A reconnects.

The room itself being unreachable settles nothing. Room down is not B down. A's row stays blocked and
the check says the room is unreachable.

## Edge cases

| Case | Rule |
|---|---|
| B is A | Refused: *you cannot wait on your own answer*. |
| Unknown handle | Refused with the known top-level handles. `resolve_name` passes an unknown name through for `say`, so `Ask` also requires a tree member. |
| Ambiguous handle | Refused with the candidates `resolve_name` returns. |
| B is the operator | Refused, pointing to the `operator` wait. |
| B is an external (`cc@...`) | Refused for now: no board to nag and no `answer` verb on the bridge. |
| B or A is a subagent | Refused: a subagent has no room connection. A child waits on its parent's answer through its parent. |
| B cannot see the room | Refused, naming `host` or an `Invite`. |
| B not connected at ask time | Accepted. The grace starts at the ask, and the reply says B is not connected now. |
| Same asker, target and question, open | The room returns the existing ask. Several rows carry one id and one settle releases all of them. |
| Another asker, same question | A separate ask and a separate row on B's board. |
| A restarts | The edge is on the persisted board, and the first pass after reconnect applies what settled meanwhile. |
| Room restarts | Asks come back from the chat store. Grace clocks restart at the room's start. |
| Room ran with no store, or pruned the ask | The pass finds an id the room does not know and settles it: *the room no longer knows this question*. |
| Settled asks | Kept 24 h after settling, then dropped at the chat store's rewrite. |

## Relation to existing waits

`Operator` is A's side: asked, blocked, settled into `Answered`, one line to the model. `Room { item }`
is B's side: room state turned into a pass-owned row by `fill_room_board`. `Child` is the liveness
precedent. Nothing is duplicated, `Ask` and `Owed` are new keys on existing paths.

## Implementation plan

Each step lands red tests first.

1. **Room state.** `crates/room/src/chat.rs` (records, `AskView`, store restore and rewrite),
   `server.rs` (`gone_since`, clock sweep, dedup), `wire.rs` (four frames, version 9). Tests in
   `crates/room`: an ask settles `Answered` on `Reply` and refuses a `Reply` from anyone else. A
   target closed and back within the grace stays open. A target gone past the grace settles `Down`
   (clock injected, no sleeping). Asks survive a server restart over a store. A duplicate returns the
   same id. Self, unknown, ambiguous, operator, external and subagent targets are refused.
2. **TodoNeed.** `tokencore/src/store.rs`, `sessionlog/src/event.rs`, the conversions in
   `harnessd/src/sessions.rs` and `harness/turns.rs`, `Answered.ask`. Tests: round trip through
   `needs_json`, and an old row without `ask` reads.
3. **Todo tool.** `crates/tools/src/builtins/todo.rs`: `wait_on` `ask`, the `RoomLink` seat,
   `needs` refusal, `keep_answers` keeping `Ask`, `detach`, `unmet_needs`/`NeedFate::PeerAsked`,
   `render`, `blocked_line`, `all_on_the_operator`, `settle_ask`. Tests in a new `todo/tests/ask.rs`
   modelled on `operator.rs`, with a fake link.
4. **Room link and verb.** `harnessd/src/room/link.rs` (`request_of`, `answer_sentence` for
   `answer` and `Asks`), `crates/tools/src/builtins/room.rs` (the verb and its schema text). Test:
   the verb becomes a `Reply` frame.
5. **Passes.** `harnessd/src/harness/room.rs` and `room/remind.rs`: `Owed` rows from `of_me`,
   settles and withdrawals from `by_me`, the note and bell. Tests: pure functions over `AskView`
   lists, in the style of `remind::rows`.
6. **Heads.** `crates/tui/src/ui/panes/todos.rs` and `app/todos.rs`: draw `Ask` and `Owed`.
7. **End to end.** Two sessions in two daemons against one coordinator in a scratch dir. A asks, B
   answers, A's row is released. Then B's daemon is killed, and A's row is released with the gone
   reply after the grace. The grace comes from config so the test runs in seconds.
8. **Docs.** `docs/room.md` (the verbs) and the `todo_write` schema text.

## Open questions for the operator

1. Should a plain thread reply from B also settle, or only the explicit `answer` verb? The draft says
   only the verb.
2. May B decline (*I will not answer this*), settling like an abstention? The draft has no decline.
   B can answer with words that say so.
3. Externals (`cc@...`): refused for now. Do you want a bridge `room_answer` tool so Claude Code
   seats can be asked?
4. Is 2 minutes right for every target, or should `gatekeeper@<project>` and other system agents,
   which are reseated rather than restarted, get their own grace?
5. Should an asker gone for a long time (days) withdraw its asks, or are they kept open for ever
   while its row is?
