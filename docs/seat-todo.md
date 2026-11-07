# A Claude Code seat's todo board

Status: design, then the build. TODO row R144.

A Claude Code seat orchestrating leticode sessions has no board: its waits live in Monitors and in
its own memory, and nothing nags it about a plan it has stopped moving. The bridge already holds the
seat (`cc@<project>`, `crates/bridge/src/rooms.rs:214`) and its packages as heads
(`crates/bridge/src/tools.rs:37`), so the board is the session's board, kept by the seat.

## The shape

One `Seat` per bridge process: the handle, the shared store, a `TodoBoard`
(`crates/tools/src/builtins/todo.rs:20`) and a `TodoNagClock`
(`crates/harnessd/src/sessions.rs:252`). Both are the session's own types, so a row, a wait, a
notice and a status word mean one thing on both sides of the bridge - the board is not a second
vocabulary for the same plan.

Three tools, registered with the others (`crates/bridge/src/tools.rs:1023`):

| tool | answer |
|---|---|
| `todo_write` | the session's tool, the same schema and the same refusals: `todos`, `update`, `operator`, `wait_on` (`crates/tools/src/builtins/todo.rs:1799`, `1965`, `2205`). No `target`: a seat does not write onto a package's board (`ChildTodos::upsert_child` answers why). |
| `todos` | the board as it stands - rows, their needs, the version - so a restarted bridge reads back what it wrote. |
| `todo_answer` | the operator's reply to a row's question, recorded by `TodoBoard::answer_operator` (`crates/tools/src/builtins/todo.rs:630`). |

## Where the board lives

The shared store, in a table of its own: `seat_todo(seat TEXT PRIMARY KEY, todos_json TEXT NOT NULL,
updated_ms INTEGER NOT NULL)`, schema version 41, an `Additive` arm (`crates/tokencore/src/store/floor.rs:46`).

Not the `todo` table (`crates/tokencore/src/store.rs:326`) for two reasons that are the same reason:
its `session_id` is a foreign key to `session(id)` and `foreign_keys` is on at every open
(`crates/tokencore/src/store.rs:1400`), so a seat's rows need a `session` row first - and a session
row with no transcript, no hub and no daemon would list to every reader of the registry as a session
that failed. A board with no session behind it is its own table.

The write is whole on every change (`put_todos`'s contract, store.rs:3040): no delta, no reorder by
id. A bridge reads it at the seat's first use, so a restart loses nothing - and a store from before
v41 gains the table at its next open, empty.

## Which waits a seat may use

- **`child`** - a package session this bridge holds: the id `leticode_start` or `leticode_attach`
  returned. Checked against the held heads by `ChildTodos::own_child`, so a wait naming a session
  this seat does not hold is refused by name rather than armed into silence. A sibling's id, a
  room's, a queue row's: not a child of the seat, and a wait on one would never end here.
- **`job`** - an open job of one of those packages, by handle (`j…`, from `leticode_status`'s
  `jobs_open`, `crates/bridge/src/head.rs:488`) or by the name it was started under. The seat's job
  table is the union of its packages' open jobs, so the tool's own resolution
  (`crates/tools/src/builtins/todo.rs:2302`) runs unchanged.
- **`operator`** - a question the seat asks a person. There is no card: a card is a head's frame and
  a seat has no head. The row's question is what the nag names, the seat asks in the conversation,
  and the answer arrives by `todo_answer` as the operator's own words, kept on the row as
  `TodoNeed::Answered`. A session's ask validates an option index against the card's options
  (`crates/harnessd/src/operator_ask.rs:55`); a seat's has none to validate, because there is
  nothing the person chose between.
- **`row` / `monitor` / `room`** - unchanged and free: the board is the same type, so an edge between
  its own rows resolves as it does in a session, and the check already honours a `Room` edge
  (`crates/tools/src/builtins/todo.rs:1014`). Nothing writes one for a seat yet: answer waits
  (`docs/answer-waits.md`, R136) are what will, and when they do this file does not change.

## A package is a child while its turn is in flight

The daemon's children are the task journal's `opening` and `running` entries
(`crates/harnessd/src/sessions.rs:145`): a child that has ended is not in the snapshot at all, which
is what makes its parent's `Child` need met. The seat's snapshot is the held heads whose turn is
`Turn::Running` (`crates/bridge/src/head.rs:396`) - a package that ends its turn leaves it, and the
`Child` need is met by the same rule, from the same function
(`waits_on_a_child_with_room_to_breathe`, `crates/tools/src/builtins/todo.rs:991`).

`age` is how long the current turn has run - the watcher's own clock, since a head does not record
when a turn started - and `quiet_for` is since that head last changed. So a package thirty minutes
into one turn is the *stuck* case and the notice steers to it, exactly as a subagent is; a package
inside its room to breathe keeps the row quiet and arms the deadline at which it grows up
(`when_a_young_child_grows_up`, `crates/tools/src/builtins/todo.rs:1125`).

## How a met wait is detected

By the clock, not by a poll of the store. The seat's watcher wakes about once a second, builds the
children-and-jobs snapshot from the heads, and compares a signature of it and of the board's version
with the last pass: a change re-arms the clock (`rearm`, `crates/harnessd/src/sessions.rs:348`), and
every pass asks `due_now` (`:521`). The board is memory, the store is write-behind, and nothing a
wait does is read out of a file.

A job that ends and a turn that ends are seen the same way - the head's `jobs_open` loses the handle,
the head's `turn` stops being `Running` - so a met wait needs no event of its own. The turn end also
reaches the client as `push.rs`'s notification (`crates/bridge/src/push.rs:49`), which is the seat's
wake; the row being askable is what the nag is for.

## How the nag reaches the seat

`notifications/claude/channel`, the frame a held session's events already use
(`crates/bridge/src/push.rs:84`), with `meta.source: "seat"` and `meta.event: "todo"`. The content is
`unfinished_plan`'s sentence (`crates/tools/src/builtins/todo.rs:1159`) - the same renderer the
daemon's idle check speaks with, so the seat and a session cannot describe two plans - under a line
saying whose board it is.

The schedule is the session's, unchanged: a 60 s window (`TODO_NAG_AFTER`,
`crates/harnessd/src/sessions.rs:52`), one notice per change of it (`nag_should_arm`, `:214`), the
repeat ladder capped at 30 minutes (`said`, `:501`). *Idle* for a seat is the same fact one layer
out: any tool call the bridge serves is the seat working, so the window is armed from the last call,
where a session's is armed from the end of its turn. The watcher starts beside `push_sessions` in
`mcp.rs` (`crates/bridge/src/mcp.rs:90`), because a client that has not initialized has no channel
to push to.

## Reuse, not fork

Nothing above is a second implementation of the session's rules, and that is the point of where the
code moved:

- `TodoNagClock`, `NagHold`, `QueueNews`, `the_check_may_ask_about`, `the_plan_as_checked` and
  `nag_should_arm` move from `crates/harnessd/src/sessions.rs` to
  `letibot_tools::builtins::todo` (a `nag` module there); harnessd re-exports them under their old
  names, so its tests and its idle-nag suite (`crates/harnessd/src/sessions/idle_nag.rs`) read the
  same paths and assert the same behaviour. `TODO_NAG_AFTER`, `OPERATOR_ADD_NAG_AFTER`,
  `NAG_REPEAT_CAP` and the census repeat move with them; `mergequeue::stuck::STUCK_REPEAT_MS` is
  derived from the one constant rather than owned beside it.
- `TodoWriteTool::write` becomes `pub`, and the job table it takes becomes the trait `JobTable`
  (`crates/tools/src/builtins/jobs.rs`), implemented once for a process host's `JobView` list and
  once in the bridge over its packages' open jobs. The daemon passes what it always passed.
- `TodoBoard` and `OperatorAsk` are already public; the seat uses them.

## Not built here

- **The head's todo pane does not draw a seat's board.** The pane reads a session's board by session
  id, and a seat is not a session (that is the store's own argument above). Listing a seat beside the
  sessions is a pane change, and `crates/tui` is outside this brief.
- **A seat's question has no card.** It shows on the board (`sessions_waiting_on_operator` reads
  session keys, so not even the picker's mark) and in the nag, and it is answered in the
  conversation.
- **`/nag pause` has no seat spelling.** The clock's `hold` stays `Running`; a seat that wants
  silence marks its rows.

## The tests this design is judged by

`crates/bridge/tests/seat_todo.rs`:

- `a_seats_rows_are_written_by_the_session_tool_and_read_back_across_a_restart` - write with
  `todo_write`, drop the bridge, open another on the same store, `todos` lists the rows with their
  needs.
- `a_wait_on_a_packages_turn_is_met_when_that_turn_ends` - a row waiting on a held package's id is
  quiet while the package's turn is in flight and askable when the turn ends.
- `an_in_progress_row_with_no_wait_is_nagged_once_and_not_again_without_a_change` - one channel push
  after the window, nothing more until the board moves.
- `a_packages_open_job_is_a_wait_on_that_package_and_ends_with_it`.
- `a_row_waiting_on_the_operator_is_quiet_until_the_answer_lands`.

`crates/tokencore`: `a_seat_board_survives_a_store_that_gains_its_table` (a v40 store migrates to 41
and keeps a seat's rows). `crates/tools`: the moved nag tests, unchanged in what they assert.
