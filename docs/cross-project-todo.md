# Todo graphs that span projects

Status: design. TODO row R137. Builds on `docs/answer-waits.md` (R136), which is landed.

A board's edges stay inside one session tree today: `row` names a row on the same board, `child`,
`job` and `monitor` are this daemon's facts, `room` and `owed` are the room pass's rows, and
`operator` is a person (`crates/tokencore/src/store.rs:986`). The ask of R136 is the one edge that
leaves the tree: A's row waits on B's answer, and B may be in another project on the same host
(the `host` room, `docs/room.md` "Rooms"). This doc decides what else may cross a project and how.

Scope is one host. Asks across hosts are R138 (`docs/cross-host-identity.md`), and nothing here
depends on it. Evidence from the live store, read-only: 236 top-level sessions over 71 workspace
roots, no `ask` or `owed` edge written yet. So nothing in use constrains the shape.

## Decisions

### No bare `row` edge across boards

`TodoNeed::Row { content }` stays local. A row on another project's board is not named by its words
from here. Reasons:

- A row's identity is its words (`unmet_needs`, `crates/tools/src/builtins/todo.rs:1661`). B
  re-words its rows freely, and a re-worded row is `NoSuchRow` on A's side with nobody told. On one
  board the check catches it in the same pass. Across boards B never learns it broke A.
- R136 ruling 1: B must know it is waited on. A plain read of B's board from the shared store
  (`Store::todos`, `crates/tokencore/src/store.rs:3030`) would make a wait B cannot see.
- A store read is host-bound. An edge that silently stops resolving after a migration (R138) is the
  silent edge the `needs` doc forbids (`store.rs:975`).

### A cross-project row wait is an ask whose answer is the row being done

Instead `wait_on` gains a second ask shape: a row of another session, held by the room like any ask.

```text
todo_write {"wait_on": [{"content": "wire the port",
             "row": {"session": "serenedb", "content": "publish the port"}}]}
```

The room holds it as an ask of kind `row`. B's room pass does not add a new row for it: it puts the
`Owed { ask }` edge on B's existing row, so from then on the row's identity on B's side is the ask
id and not its words. A restatement keeps the edge: `keep_answers` (`todo.rs:755`) keeps `Ask` and
`Answered` today and is extended to `Owed`. A write that drops the tagged row, re-wording included,
since a re-worded row is a new row, gets it put back by the next pass with the sentence
*`arango` waits on this row - finish it or `room decline` ask #4211*. That is R136's rule for an
owed row: only an explicit act settles. The ask settles from B's pass, never from A:

| B's row | ask state | A's row reads |
|---|---|---|
| `Completed` | `Answered { text: "done: <row words>" }` | `` `serenedb` finished "publish the port" `` |
| `Cancelled` (struck off) | `Declined { text: "struck off" }` | `` `serenedb` struck off "publish the port" `` |
| dropped from the board | open, the pass puts the row back | still waiting |
| B declines (`room decline`) | `Declined { text }` | as R136 |
| B's session deleted | `Down` (`Retire`, unchanged) | as R136 |
| B gone, any length | open, escalated after the grace | as R136 |

Reasons: one holder, one settle path (`TodoBoard::settle_ask`, `todo.rs:706`), one withdrawal rule
(`asks::withdrawals`, `crates/harnessd/src/room/asks.rs:91`), and the services contract unchanged.
Done-ness is B's explicit act (marking a row done is B saying it), so ruling 1 holds. B can still
`decline` with words.

Validation at the call, so every refusal reaches the model synchronously as R136 does: A's daemon
reads B's board from the shared store and requires exactly one row with those words, not
`Completed`. A row already done is refused with *already done, nothing to wait on*. Ambiguous and
missing rows are refused with the board's open rows listed. The room does not hold boards and does
not validate them. The store read is same-host, which is the only case R137 covers.

### Who may be waited on

The R136 table (`docs/answer-waits.md` "Edge cases") applies unchanged: top-level sessions only,
because they are services (`docs/answer-waits.md` "Top-level sessions are services"). Additions:

- A row on a subagent's board: refused, naming the parent. A subagent is owned and can end; its
  parent's row is the service's promise.
- A row on a Claude Code seat's board (`seat_todo`, `docs/seat-todo.md`): refused for now. A seat has
  no room pass to tag the row and settle it. Question asks of seats stay as R136.
- Same project: allowed. The shape is about another session's board, not another project, so the
  rule is one rule. Within one tree, `row` and `child` stay the spelling.

### The idle check

A's side: an ask of either kind is `NeedFate::PeerAsked` (`todo.rs:1615`), a root in
`all_on_the_operator` (`todo.rs:1518`). Unchanged: a peer is outside the plan's hands, and the
settle arrives through A's pass. `blocked_line` (`todo.rs:1740`) says the row kind:
`` `wire the port` waits on `serenedb`'s row "publish the port" (ask #4211) ``.

B's side: the tagged row is B's own work and is checked as any row. One change: `the_row_to_name`
(`todo.rs:1563`) ranks a ready row carrying `Owed` after the operator's rows and before the rest, and
the nag says *`arango` waits on this*. Reason: a peer blocked on a row is the cheapest unblock on the
board, and the nag is the only place B learns the order matters.

Cycles. Two row waits can deadlock: A's `r1` waits on B's `x`, and B's `x` waits on A's `r1`. Both
rows are `PeerAsked` roots, so both checks go silent and nobody is told. The room sees every ask, so
the `Ask` frame carries the asker's waiting row (`for_row`), and the room refuses an ask that closes a
cycle over (session, row) pairs it holds, naming the path. A cycle through a question ask (target row
unknown) is session-level only and may be false, so it is a note on `AskMade.note`, not a refusal.
Cycles made later by re-wording are not detected. See open question 3.

### How the head draws it

The todo pane draws one session's board (`crates/tui/src/ui/panes/todos.rs`). Two additions, no
graph layout:

1. On the board. `need_words` (`todos.rs:405`) draws a row ask as
   `` `serenedb`'s row "publish the port" (ask #4211, open) `` and an owed row's edge as
   `` `arango` waits on this (ask #4211) ``. Enter on either edge switches the pane to the peer's
   board, the way the picker switches sessions. Reason: following an edge is the one thing a person
   does with it.
2. The edges overlay, `/edges`. One line per open ask on the host, grouped by the asker's project,
   blocked longest first:
   `letibot/arango "wire the port" -> serenedb/serenedb "publish the port" - open 12m`. Escalated
   asks first and marked. Source: a new operator-seat read of every ask (below). Reason: a terminal
   graph of N boards does not fit a pane, and the question a person asks is *who is stuck on whom*,
   which a sorted edge list answers. A drawn DAG is not built.

### Store and wire changes, all additive

- `TodoNeed::Ask` gains `row: Option<String>` (serde default) in all three copies:
  `crates/tokencore/src/store.rs:1027`, `crates/sessionlog/src/event.rs:230` and the wire. `None` is
  the R136 question ask, so every edge written so far reads. `todos_json` is JSON, so no SQL schema
  version moves.
- `AskView` (`crates/room/src/chat.rs:464`) and the `Ask` chat record gain
  `kind: AskKind { Question, Row { content } }` (serde default `Question`) and `for_row: Option<String>`.
  A chat store written by protocol 10 reads.
- `Request::Ask` (`crates/room/src/wire.rs:233`) gains `row` and `for_row`, both serde default.
- `Request::AllAsks`, operator seat only: every ask on the host, open and settled inside the keep
  window. Answered with the existing `Response::Asks` shape, `of_me` empty.
- `ROOM_PROTOCOL_VERSION` 10 to 11 (`wire.rs:16`). An old coordinator would read a row ask as a
  question ask and nag B with a question that has no answer, so the skew must be refused, not
  ignored (`protocol_refusal`, `wire.rs:22`).
- `AskLink` (`todo.rs:2050`) gains `fn wait_row(&self, of, room, content, for_row) -> Result<AskMade,
  String>`, defaulted to a refusal so `NoAsks` and test links compile unchanged. A second seam,
  `PeerBoards { fn open_row(session, content) -> Result<(), String> }`, is the store read, so the tool
  crate does not open a store.

## Package plan

Paths are disjoint. P1 and P2 run in parallel, P3 after P2, P4 after all three, P5 after P1 and P2.

| pkg | paths | done-tests |
|---|---|---|
| P1 room | `crates/room/src/{chat,server,wire,room}.rs`, `crates/room/tests/ask_row.rs` | `a_row_ask_is_held_with_its_kind_and_survives_a_store_reload`, `a_row_ask_that_closes_a_cycle_is_refused_naming_the_path`, `a_question_ask_cycle_is_a_note_not_a_refusal`, `all_asks_is_the_operators_read_only`, `protocol_10_is_refused_with_the_upgrade_sentence` |
| P2 types | `crates/tokencore/src/store.rs` (`TodoNeed`), `crates/sessionlog/src/event.rs`, the two conversions in `crates/harnessd/src/sessions.rs` and `crates/harnessd/src/harness/turns.rs` | `an_ask_edge_without_row_reads_back_as_a_question_ask`, `a_row_ask_edge_round_trips_through_needs_json` |
| P3 tool | `crates/tools/src/builtins/todo.rs`, `crates/tools/src/builtins/todo/tests/ask_row.rs` | `wait_on_row_of_a_peer_asks_the_room_and_writes_the_edge`, `a_peer_row_that_is_missing_or_ambiguous_is_refused_with_its_open_rows`, `a_peer_row_already_done_is_refused`, `a_row_ask_is_a_peer_asked_root`, `an_owed_ready_row_is_named_before_the_others`, `the_blocked_line_names_the_peers_row` |
| P4 daemon | `crates/harnessd/src/room/{link,asks}.rs`, `crates/harnessd/src/harness/room.rs`, `crates/harnessd/tests/room_row_wait_e2e.rs` | `asks::a_row_ask_tags_the_existing_row_and_adds_none`, `asks::a_completed_tagged_row_replies_done`, `asks::a_struck_off_row_declines`, `asks::a_dropped_tagged_row_is_put_back_while_the_ask_is_open`, e2e `a_row_in_one_project_waits_on_a_row_in_another_and_is_released_when_it_is_done` |
| P5 head | `crates/tui/src/ui/panes/todos.rs`, `crates/tui/src/app/todos.rs`, new `crates/tui/src/ui/panes/edges.rs`, `crates/tui/src/app/edges.rs` | `a_row_ask_edge_draws_the_peers_row`, `enter_on_an_ask_edge_switches_to_the_peers_board`, `the_edges_overlay_lists_escalated_asks_first` |
| P6 docs | `docs/room.md`, `docs/answer-waits.md` (pointer only) | none, reviewed against P1-P5 |

The `todo_write` schema text is in `todo.rs`, so P3 owns it. P4's e2e is two daemons in two
project folders against one coordinator in a scratch dir, grace from `LETIBOT_ROOM_ASK_GRACE_MS`, as
`crates/harnessd/tests/room_asks_e2e.rs` does.

## Open questions for the operator

1. Does a row dropped from B's board settle the ask as `Declined`, or does B's pass put it back
   while the ask is open? Default: put back. It is R136's rule for owed rows, and a re-word would
   otherwise release A by accident. `room decline` is the explicit way out.
2. May a row wait target a Claude Code seat's board? Default: no, until the bridge has a pass that
   tags and settles. Question asks of seats stay.
3. Cycles made by re-wording after the ask: the room's 5 s sweep could re-check (session, row)
   cycles from the rows each pass reports. Default: not built. Creation-time refusal catches the
   common case, and re-wording a row someone waits on is already a visible act on B's board.
4. Should a cross-project row wait need the operator's consent, as a card? Default: no. The ask is
   visible on both boards and in `/edges`, and the operator can withdraw it (R136).
