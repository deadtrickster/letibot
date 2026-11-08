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
  (`crates/tools/src/builtins/todo.rs:2302`) runs unchanged. It is not what the check SUGGESTS: a
  package's job is named as that package's (`K1 is running j83509`) and what a row whose package is
  busy is offered is that package's TURN, which is a `child` wait (R156).
- **`external`** - something outside letibot, in the seat's own words: a review on a forge, a
  deploy, a person's other tool. `wait_on`'s `"external": "…"` or a `needs` entry of that kind.
  Nothing here can see it end, so the check is silent on that row until the row moves - the seat
  clears it with `"detach": true` - and the board draws `waits on <what>` (R156).
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

## A seat's own jobs, and a process it did not start

Two waits a seat has that a session does not need, because nothing else here can watch them
(R156, extended):

- **`seat_job_start {argv, cwd?, name?}`** runs a command as the SEAT's job - no shell, output to a
  file under the bridge's rundir, the row in the store's `seat_job` table under the seat's handle.
  It answers a `j…` handle, and `todo_write {'wait_on': [{'content': …, 'job': 'j9'}]}` takes it:
  the seat's open jobs are part of its job table, so the two agree. `seat_jobs` lists them with the
  state each ended in; `seat_job_kill` sends SIGKILL to that exact process. A job's end settles the
  row AND pushes a `job_ended` channel frame (`meta.source: seat`), so the seat is told the way it
  is told about a package's job.
- **`wait_on {'pid': N}`** waits on a process this host is already running - a gate somebody
  started by hand. The edge records the pid AND `/proc/<pid>/stat`'s start time, read when the row
  is written: after the number is reused, the pair no longer matches, which is the whole of what
  makes the wait exact. The seat's watcher reads the process table every pass and stops being
  quiet on the row when the process goes.

Neither kind is a session's: a daemon has its own process host, and a session that wrote one would
be asking a runtime that cannot see it.

## The once-per-change key is the PLAN
A seat is nagged once per change of its plan - its rows, their statuses and their waits
(`the_plan_as_a_key`, `crates/tools/src/builtins/todo/nag.rs`) - and not once per change of the
sentence it is spoken in. The sentence names the packages' running jobs, so a held package starting
or ending one rewrote it: twenty of those in fifteen minutes were twenty checks of one unchanged
row. `TodoNagClock::keyed_by_plan` is the switch, `said_plan` records a delivery by the plan,
and the repeat ladder paces the rest. A session keeps its sentence - its own jobs are its own work.

## A package is a child while its turn is in flight

The daemon's children are the task journal's `opening` and `running` entries
(`crates/harnessd/src/sessions.rs:145`): a child that has ended is not in the snapshot at all, which
is what makes its parent's `Child` need met. The seat's snapshot is the held heads whose turn is
`Turn::Running` (`crates/bridge/src/head.rs:396`) - a package that ends its turn leaves it, and the
`Child` need is met by the same rule, from the same function
(`waits_on_a_child_with_room_to_breathe`, `crates/tools/src/builtins/todo.rs:991`).

`age` is how long the package has been a child, and `quiet_for` is since it last visibly worked:
its head changed, or it had a job of its own running. A package stays a child between turns while
it has an open job or an ask. The room to breathe (`CHILD_ROOM_TO_BREATHE`, thirty minutes) is
measured from `quiet_for`, not `age`, so an hour-long package that is still moving keeps the row
quiet, and one quiet for thirty minutes is the *stuck* case the notice steers to, exactly as a
subagent is (`when_a_young_child_grows_up` arms that deadline).

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

## How a head sees it

The head's todo pane draws the seats' boards beside the session's. The daemon answers
`ClientFrame::ListTodos` with `ServerFrame::Todos`, and that frame carries `seats` - one
`SeatBoard` per handle in `seat_todo`, read by `Store::seat_boards` and converted by
`Harness::todo_entry`, the same conversion a session's rows take. The field is `serde(default)`,
so a daemon from before it says nothing and an older head drops it (the version note at
`PROTOCOL_VERSION` 67). A seat's rows are DRAWN and are not stops: the cursor walks the session's
rows and the file's, and Enter on a seat's row is nothing, because the seat writes its own board
and the head has no door to it.

## Not built here

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
