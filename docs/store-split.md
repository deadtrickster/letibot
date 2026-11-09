# Store split: one file per project, one shared file

Status: proposal, design settled, P0, P1, P2, P3 and P4 built (their own paragraphs in § Build plan
say what landed). Left: the live migration of the real store on this box, which is the operator's
to run. The corpus question is decided: decisions live
in project files and there is no shared copy (§ The corpus). The stalls the split is meant to
remove were measured on lab2x1 on 2026-11-07 (§ What the daemons logged), and the build is in
five packages with their done-tests at the end.

## Why

Every daemon on the box writes one `sessions.db`. On lab2x1 on 2026-11-06 it was 9.4 GB with a
346 MB WAL that never shrinks: a checkpoint cannot finish while any of ten daemons holds a read
snapshot, and nothing truncates it. Writes are `IMMEDIATE` transactions that retry on busy for up
to 60 s, so one slow writer in any daemon stalls every other daemon's writes. A caller that holds
its own lock around a store write turns that stall into a frozen session (the prompt journal,
fixed in 99feb951).

Measured split of the file (`dbstat`, MB):

| table | MB | owner |
|---|---|---|
| `transcript_item` (+ its indexes) | 7710 | one session |
| `adjudication` (+ indexes) | 886 | one session, read as one corpus |
| `stable_prefix` | 385 | content-addressed, shared by transcripts |
| everything else | < 30 | mixed |

Nearly all bytes and nearly all writes belong to one session, and every session belongs to the
daemon of one folder.

## The shape

- **Project file** `~/.local/share/letibot/projects/<key>.db`, where `<key>` is the folder hash
  the daemon socket already uses (`42ce9f1aae08`). Written only by that folder's daemon.
- **Shared file** `~/.local/share/letibot/shared.db`. Written by every daemon, small and short
  transactions only.

A daemon opens its project file and the shared file. A head or CLI that lists sessions reads the
shared file and opens a project file only to load a transcript.

Inside one daemon the project file has one writer thread. Callers hand it work and never hold
their own lock across a write, so `database is locked` between daemons cannot happen and inside a
daemon it becomes an ordinary queue.

## Table assignment

Project file - everything one folder's sessions own:

- `transcript`, `transcript_item`: the history and the token blobs.
- `stable_prefix`: written once per prefix. Duplicated per project rather than shared, so a fork
  or resume never reads across files. The cost is one copy per project of each prefix in use.
- `todo`, `citation`, `session_marks`, `session_mode`, `session_monitor`: per-session state.
- `job`, `job_context`, `process`: a session's background work.
- `queued_prompt`: the prompt journal.
- `subagent_run`: parent and child are always in the same daemon, so a spawn or a settlement
  stays one transaction.
- `adjudication`: written by the session that asked, and read back out of the project file it
  is in. A corpus-wide query opens every project file read-only; there is no copy in the shared
  file (§ The corpus).

Shared file - what is read across folders or arbitrated between daemons:

- `session`: the index. id, title, workspace, parent, role, project key. `--sessions`, the
  picker, `--continue NAME` and the fleet view read only this.
- `session_writer`: which daemon holds a session. Arbitrates across daemons by definition.
- `review_queue`, `merge_queue`, `queue_lease`, `queue_transition`, `queue_told`,
  `target_health`: keyed by repository, and a repository's queue is served by whichever daemon
  holds its lease, which is not always the folder's own daemon.
- `host_lease`: ports and hosts are box-wide.
- `daemon_life`: box-wide heartbeat.
- `project_mode`, `mode_import`: keyed by folder, read before a daemon has a session.
- `schema_version`, `schema_reader_floor`: one per file, in both files.
- `merge_queue_v23`, `merge_review_v23`: legacy, dropped by the split if empty.

Nothing else is in the shared file. In particular no `adjudication` row is: a decision is written
once, by the daemon that made it, into that daemon's project file (§ The corpus).

## The corpus

Decisions live only in project files. `adjudication` is in `projects/<key>.db` and nowhere else:
no slim copy in `shared.db`, no second write per decision, no repair pass that can disagree with
the row it copies. The write path is the one it is today — the gate's sink inserts the row into
the session's own file, which is also the file the session's transcript is in.

What reads the corpus, as the tree has it today (paths in this repo):

- The `decisions` tool (`crates/harnessd/src/decision_source.rs`). A scoped read — `session=<id>`,
  or no `session` at all, which means this session — resolves through the shared `session` index
  to one project key and opens that one file. `session=any` is a sweep: every project file,
  read-only, one at a time.
- The start-up disclosure (`crates/harnessd/src/corpus.rs`): the sink counts the rows in its own
  file, so after the split the number a daemon prints is its project's. It opens one file and no
  sweep is involved.
- `/gate` (`crates/harnessd/src/slash.rs`), which takes the same `StoreAt` the rest of the daemon
  does: `counts` and `recent` are whole-corpus questions and sweep; `ok|grant|revoke` still writes
  the one file it is handed.
- `Store::corpus(only_labelled, limit)` is `/gate recent`'s query; beyond that it is reached only
  from `tokencore`'s own tests.
- `Store::open_reader(path)` is the sweep's door onto another folder's file: a read-only
  connection that migrates nothing and creates nothing. `Store::open` would migrate it, and
  migrating a file somebody else's daemon is writing is the one write a question may not make.

The sweep is the accepted cost: 29 read-only opens on lab2x1, ordered by project key so that a
listing cut short is still a stable prefix. It takes no write lock on any file, so it cannot
stall a project's daemon the way a shared writer does; a file that has gone missing or is half
migrated is named in the answer and the sweep goes on with the rest — the count of the files it
could not read rides the answer beside the count of the rows it found, because a total that
leaves a file out must not read as the whole corpus. That answer is also why the sweep is not on
the start-up path: a daemon must not wait on 29 files to say hello.

## Migration

One offline pass, run with every daemon stopped:

1. Map every session to its tree's root by `parent_session_id`, and the root's
   `workspace_root` to a project key. A child's own `workspace_root` is its worktree and must not
   be used: on lab2x1 it gives 570 distinct folders, the roots give 29.
2. For each key, create `projects/<key>.db` with the project schema and copy that key's rows of
   every project table, in one transaction per file.
3. Create `shared.db`, copy the shared tables, add the `project` column to `session`.
4. Verify: per table, row counts and a hash of the primary keys match between old and new, and
   every `adjudication` row is in exactly one project file — the old table's count equals the sum
   over the new files, and `shared.db` has no `adjudication` table at all. Every transcript's hash
   chain is re-verified the way a resume does.
5. Rename `sessions.db` to `sessions.db.pre-split`. It is kept until the operator deletes it.

A session whose workspace no longer exists still gets a project file under its recorded key.
`--migrate` moves a daemon to another host; its project file goes with it instead of rows being
copied out of the shared store.

## What the daemons logged

`busy::SLOW_CALL` writes one line to a daemon's stderr for a store call that took a second or
more (`crates/tokencore/src/store/busy.rs`). It landed in 571de459 at 09:26 on 2026-11-07, so
no log written before that could hold one.

### The line after P0

P0 took `busy_timeout` off the connection and put a busy handler of the store's own in its place
(`crates/tokencore/src/store/busy.rs`): it sleeps SQLite's own schedule (1, 2, 5, 10, ... 100 ms
and 100 ms after that), gives up at the same 30 s, and adds every sleep up. The line carries that
sum as `waited`, so what is left of `took` is the call's own work:

```text
store: a call at store/procs.rs:345 took 11405 ms (waited 11328 ms, 0 busy retries)
store: a call at store.rs:3150 failed after 87 ms (waited 0 ms, 2 busy retries, 3 at this site): database is locked
```

A call over `SLOW_CALL` is the first form. `waited` near `took` is a lock wait; `waited 0 ms` is
I/O. The retry count stays beside it and means something else - the attempts that spent their
whole 30 s - which is why a stall can read `0 busy retries` and still be a wait.

The second form is the two failures the store counts by call site, `database is locked` and
`database or disk is full`. It is logged however fast the call was, and it carries how many of
that kind have reached a caller at that `file:line` since the process started: the `locked` and
`or disk is full` columns of the tables below, per site, without the grep.

Two reads are timed too - `Store::load_transcript` and the session list behind
`Store::list_sessions`, `Store::session` and `Store::children_of` - so a slow read is a line at
all. A reader never waits for a writer in WAL, so their `waited` should read 0; what they show is
the disk the split cannot separate.

The tests are in `crates/tokencore/src/store/busy/tests.rs`:
`a_lock_wait_is_on_the_line_as_waited_and_the_body_is_not` (a lock held 300 ms under a write
that also spends a second of its own: `waited` is the block, `took` is both),
`an_uncontended_slow_call_reports_no_wait`, `a_call_under_the_slow_threshold_writes_nothing`,
`a_slow_store_call_is_one_line_naming_its_caller` and
`a_refused_write_is_counted_by_kind_and_call_site`.

Where the lines go: the launcher and the bridge send a daemon's stderr to
`~/logs/harnessd-<folder key>.log` (`scripts/letibot`, `crates/bridge/src/daemon.rs:199`), and
the room coordinator to `~/logs/room-<host key>.log`. The per-session logs under the data
directory (`<data>/logs/<session>/`, `Config::daemon_logs`) hold events, not stderr, and that
directory was not readable from the seat that took this measurement (a masked, read-only mount),
so the store's own size and WAL could not be re-measured either: the 9.4 GB and the 346 MB above
are 2026-11-06's.

The window: 16 daemons share one `sessions.db` — 15 of them started between 12:37:46 and
12:40:16 on 2026-11-07 (12 letibot worktrees, `serenedb`, `arangodb`, `etcd`) and the box's room
coordinator from 02:05 — and their logs were read at 13:43. That is about 65 minutes of one file.

Seven lines, in five daemons, every one of them with **0 busy retries**:

| took ms | site, as the log names it | method | what the call is | daemon | project |
|---:|---|---|---|---|---|
| 18226 | `store.rs:2462` | `append_items_with_context` | `write_tx`, a chunk of transcript rows | `e1272afc059d` | `~/Projects/serenedb` |
| 11405 | `store/procs.rs:345` | `check_process` | `UPDATE` one `process` row, at start-up | `ce04a5e7078a` | `wt-room-tui-p0` |
| 6716 | `store/procs.rs:345` | `check_process` | the same call, four lines later in the same start | `ce04a5e7078a` | `wt-room-tui-p0` |
| 2853 | `store/procs.rs:345` | `check_process` | `UPDATE` one `process` row | `114b334e3d2d` | `wt-r144-seat-todo` |
| 1853 | `store.rs:3193` | `record_adjudication` | `INSERT` one decision | `248ae6698c0c` | `~/Projects/arangodb` |
| 1527 | `store/procs.rs:345` | `check_process` | `UPDATE` one `process` row | `e1272afc059d` | `~/Projects/serenedb` |
| 1348 | `store/subagents.rs:510` | `life_beat` | `UPDATE` one `daemon_life` row | `9bae3e9cbc8d` | `wt-r130-remote-secrets` |

Every one is a write, because `exec` and `write_tx` are the only two store calls that went
through `retry_busy` (`crates/tokencore/src/store.rs:3449`, `:3456`): this instrument could not
see a read at all when these lines were written. P0 (§ The line after P0) times two reads; these
logs predate it. Four of the seven are single-row `UPDATE`s by primary key — `process` is keyed
`(session_id, handle)` — so the time is not a table scan.

Failures that reached a caller are the other half of the picture, and the same logs carry them.
`database is locked` is a write that lost the lock; `database or disk is full` is the filesystem
refusing a write:

| log | daemon | `locked` | `or disk is full` | last written |
|---|---|---:|---:|---|
| `harnessd-defc553f46c3` | etcd | 39 | 42 | 13:43 today |
| `harnessd-248ae6698c0c` | arangodb | 9 | 10 | 13:43 today |
| `harnessd-e1272afc059d` | serenedb | 0 | 3 | 13:43 today |
| `harnessd-fcf91a545af4` | one folder's daemon | 127 | 33 | 2026-11-01 |
| `harnessd-42ce9f1aae08` | one folder's daemon | 87 | 60 | 2026-11-05 |
| `harnessd-c74d1fd17483` | one folder's daemon | 33 | 30 | 2026-11-05 |
| `harnessd-5a0a8f3c88f8` | one folder's daemon | 0 | 9 | 2026-10-27 |
| seven other per-folder logs | per-folder daemons | 33 | 0 | 2026-11-06 |
| `harnessd.log` | the old shared log, before R92.3 | 26 | 6 | mixed, Sep 6 – Nov 7 |

The 48 locked in today's hour are all queue work — `merge queue: finding/serving the queues`,
`review queue: the reviewers could not be served` — plus job bookkeeping (`its process record was
not recorded`), and they are logged by exactly the three daemons that serve queues. The 12
worktree daemons, started in the same minute, logged none.

## Does the split separate them?

**Writer against writer: yes, and that is the largest number here.** 328 locked writes in the
per-folder logs (2026-11-01 to 2026-11-07, 48 of them in the hour measured) is the coupling the
split removes by construction: after it no two daemons write a file that holds a session's
transcript, so one folder's writer cannot make another's return `database is locked`. What the
split does *not* remove is the queue path — `merge_queue`, `review_queue`, their leases — which
stays in `shared.db` and stays fought over by every daemon. It gets better (a small file, short
transactions instead of long ones) but not different. If that contention is to go away it needs
one daemon per repository, which the queue lease already half builds, not this split.

**Writer against reader: the logs cannot answer it, and the mechanism says the counter is not
the place.** In WAL a reader never waits for a writer; a reader holds a snapshot, and a snapshot
is what stops a checkpoint from moving the WAL's start. That is the reader→writer coupling, and
the 346 MB WAL that never shrinks is its evidence. The split cuts it per file — a project's
readers pin only that project's WAL, so each file's checkpoint can finish — and that is the
strongest argument for the split. It is also almost entirely outside this instrument: nothing
here times a read but P0's two, and neither can wait for a writer.

**And the seven slow calls are not shown to be lock waits at all.** Every one reports 0 retries,
but that is not a clean bill: the connection set a 30 s busy timeout
(`crates/tokencore/src/store.rs`), so SQLite absorbed a lock wait of up to 30 s inside one
attempt and the ladder counted nothing. All seven are under 30 s. "0 retries" means *no attempt
exhausted its 30 s*, not *nothing waited*. P0 now times the wait and names it on the line, so the
next window of logs answers this; these seven are from before it, and stay unanswered — the table
must not be quoted about locks.

What the split does not separate, and must not be sold as separating: **the disk**. 160 `or disk
is full` in these logs, 55 of them in today's hour, and on 2026-11-07 a test gate linking on the
root disk blocked every daemon's reads (TODO R129). One spindle serves every file, so a
saturated disk stalls a project's file and the shared file alike — which is why builds went to
`/build`, and it stays true after the split.

## Build plan

Five packages, in this order, each with paths no other package touches and each with the test
that says it is done. Every test is a new file, so no two packages edit one path. Each ends with
`sh scripts/check-fmt.sh main`. On this box every build uses
`CARGO_TARGET_DIR=/build/letibot-r129-target`: the root disk holds the live store.

**P0 — time the wait, not just the retries.** Built. `crates/tokencore/src/store/busy.rs`,
`crates/tokencore/src/store/busy/tests.rs`, and the one line in `store.rs` that installs the
handler; the format it writes is in § The line after P0. What the doc asked for and what landed:
a `busy_handler` that accumulates the milliseconds spent waiting and reports them on the same
line (`waited N ms, R busy retries`); the two hottest reads (`load_transcript` and the session
list) routed through the ladder so a read can be seen at all; and `database is locked` and
`database or disk is full` counted by call site on the same line. Done-tests:
`a_lock_wait_is_on_the_line_as_waited_and_the_body_is_not` - another connection holds the lock
300 ms while this one writes, and the writer's line reports `waited` ≥ 250 ms, well under
`took`, with 0 retries and the caller's `file:line`; `an_uncontended_slow_call_reports_no_wait`;
`a_call_under_the_slow_threshold_writes_nothing`; and the failure count, which has no test in
the brief and got one anyway in `a_refused_write_is_counted_by_kind_and_call_site`.

**P1 — two files, one schema each, one writer each.** Built. `crates/tokencore/src/store/split.rs`
(new), `crates/tokencore/src/store/split_tests.rs` (new), and `crates/tokencore/src/store.rs` (the
open and DDL region: `pub mod split`, the `pragmas` a file's second connection shares, and the test
module). What the doc asked for and what landed: the table assignment as data — `place_of` answers a
table's file, and a table the schema creates with no assignment fails the done-test by name, so the
two lists above are the only place the assignment is written; `StoreSet::open(dir, key)` creates and
opens `shared.db` and `projects/<key>.db` from the tree's own DDL filtered by that assignment, so a
table's name decides its file and neither file can drift from `migrate`'s schema; a `Store` is one
file and a `Writer` is its one writer thread, which owns the file's write connection and whose first
job is that file's creation, a caller handing it work and waiting holding nothing. `exec` and
`write_tx` are unchanged, so no call site moves and the daemon still opens `sessions.db`; P3 is what
points the daemon at the set and the queue at the writer. Three things the doc left open, decided
here: the four tables added after § Table assignment was written (`claude_review`, `queue_mute`,
`seat_todo`, `seat_job`) are in the shared file by the assignment's own rule, and
`merge_queue_v23`/`merge_review_v23` are routed to it although a fresh store creates neither; a
project file's keys to `session` are dropped, because the index is the shared file's and a
`REFERENCES` to a table this file does not hold refuses the first append with `foreign_keys` on; and
a file this build cannot know refuses to open even where its reader floor would let a newer build
read it, because this daemon is the one that writes it. Done-tests, all on temp directories:
`every_table_the_schema_creates_is_assigned`, `every_table_is_in_exactly_one_file`,
`a_newer_project_file_is_refused_by_name`, `opening_the_set_twice_is_not_a_second_connection`,
`a_project_file_takes_its_own_rows_and_keeps_its_own_keys`,
`every_statement_the_schema_is_built_from_names_its_table` and `a_key_that_is_not_a_name_is_refused`.

**P2 — the offline migration.** Built. `crates/tokencore/src/migrate_split.rs` (new), one `pub mod`
line in `crates/tokencore/src/lib.rs`, `crates/harnessd/src/bin/letibot-migrate-split.rs` (new),
`crates/harnessd/tests/store_split_migrate.rs` (new). What the doc asked for and what landed: every
session is walked up `parent_session_id` to its tree's root, and the ROOT's recorded
`workspace_root` is hashed into the project key (`migrate_split::project_key`: the first twelve hex
of its SHA-256, which is the key the folder's daemon socket and log already wear), so a child's
worktree names no file; each key's file is created from P1's `schema_sql(StoreFile::Project)` and
its rows copied in one transaction, row by row in primary-key order so the append-only trigger under
`transcript_item` accepts them, with `defer_foreign_keys` so a fork may name a transcript written
after it; `shared.db` is written the same way and its `session` table gains the `project` column the
index needs to say which file a session's rows are in; `stable_prefix` is copied into every project
file whose transcripts name it, the one row in two files and the assignment's own rule. Step 4 is a
refusal and not a print: per file, each table's row count and a hash of its primary keys are the old
store's for those rows; across files, every project table's sum is the old count and every
`adjudication` row is in exactly one file; `shared.db` holds no `adjudication` table at all; and
every transcript's chain is replayed through `TokenLedger::restore`, the read a resume does. A failed
check names what did not add up, removes the files the pass wrote and leaves `sessions.db` under its
own name, which is why step 5 renames only after step 4 passes. The command refuses while a daemon
holds the store (the `session_writer` lease, live while the pid it names runs on this host) and
refuses a second run: a data directory that already holds `shared.db` or a project file is a split
store, and copying a fresh `sessions.db` over files a daemon has written since is the one thing this
pass must never do. Two things the doc left open, decided here: the v24 archive's
`merge_queue_v23`/`merge_review_v23` go to `shared.db` — created from the old file's own
`sqlite_master` row, because no DDL here creates them — when they hold rows and are dropped when
empty, as § Table assignment says; and a parent edge naming a session the store does not hold stops
the walk, so that child is its own root, which is the answer a resume gives it. Done-tests, all on
temp directories: `every_row_lands_in_exactly_one_file_and_the_keys_hash` — three root folders, two
of them with rows and one whose session wrote nothing, a child in a worktree, and one prefix both
projects name, so the file names, the key hashes, the empty file, the duplicated prefix, the
`session.project` column and the absence of `adjudication` from `shared.db` are all asserted;
`a_second_run_refuses`, with the old file restored as well as renamed away;
`a_broken_verification_leaves_sessions_db_in_place_under_its_own_name`, whose fixture breaks one
row's `h_k` under the append-only trigger; and
`a_live_writer_claim_is_a_refusal_and_a_stale_one_is_not`, which is the command's own refusal, since
a claim on another host is one this box cannot ask about pids for and so is not taken for stale.

**P3 — the daemon opens two paths.** Built. `crates/harnessd/src/config.rs` (`StoreAt` and
`Config::store_at`), `crates/harnessd/src/cli.rs`, `crates/harnessd/src/daemon.rs` (untouched:
the process loop holds no store), `crates/bridge/src/daemon.rs`, `scripts/letibot`,
`crates/harnessd/tests/store_split_paths.rs` (new), and - because a call has to be routed
somewhere - the routing itself in `crates/tokencore/src/store.rs` and
`crates/tokencore/src/store/split.rs`. What the doc asked for and what landed: `--store` is a DATA
DIRECTORY, and the key is the folder's own - `Config::store_key`, pinned at `for_this_box` from
`migrate_split::project_key`, overridden by `--project KEY`, and never derived from a stored
session's workspace, which a resume may change after the store is open. The bridge's default is
`~/.local/share/letibot` and the launcher's `$LETIBOT_STORE` is that directory; `letibot migrate`,
which takes one file, is handed `$STORE/projects/<key>.db`. `Config::store_at` is the one door:
`StoreAt::Set { dir, key }` opens `shared.db` and `projects/<key>.db` through P1's `StoreSet` -
`open` for a daemon, which creates them and refuses a data directory still holding `sessions.db`
and no `shared.db` with the `letibot-migrate-split` line, and `open_existing` for a question,
which creates nothing. One rule keeps every existing test's meaning: a path ending in `.db` is ONE
file, the pre-split shape, which is a fixture's and never a deployment's - and a `sessions.db`
whose directory already holds `shared.db` is refused by name, because a daemon that went on
writing the old file would be writing a store nobody reads. Routing is `Store::at(table)`: a
single-file store answers its one connection for every table, so `place_of` decides nothing
there, and a set's handle holds both connections, so no caller names a file. `exec` reads the
table out of its own SQL (57 call sites unchanged); `write_tx_at(table, ..)` names the file a
transaction is in, and the two bodies that wrote both files - the append's context row and a
session delete - are two commits now, because no transaction spans them. The listing runs
`sessions_index_sql` on a set: `transcript`/`transcript_item` are the project file's and the index
may not reach into another project's file, so its four history columns are `NULL` (P4's sweep is
where a listing gets them back), which is also why a listing answers with the project file renamed
away. Three cross-file JOINs lost theirs: `approved_shapes` and `shapeless_human_admits` read the
sessions first and then the corpus, and `unrecorded_of` reads the recorded child ids and then the
children the index names. Consumers whose tables are all in the shared file are handed that FILE -
`StoreAt::path(<a shared table>)` for the queues and the modes, and a lease's release or a
writer's claim, which read the handle's own `Store::path()`, which a set answers with
`shared.db`. P1's `split::Writer` threads are still not the write path: `exec` and `write_tx_at`
keep the process's one connection per file, and pointing every write at the queue is left for a
later package. Done-tests, all on temp directories:
`a_daemon_writes_the_index_and_the_history_to_two_files`
(a daemon opened on a data directory writes the `session` row to `shared.db` and the `transcript`
row to `projects/<key>.db`, `shared.db` has no `transcript_item` and no `adjudication` table at all,
and `harnessd --store DIR --list-sessions` still lists the session with the project file renamed
away),
`a_directory_that_still_holds_sessions_db_is_refused_with_the_migrations_name` (a directory holding
only `sessions.db` is refused, names the file and `letibot-migrate-split`, and writes no
`shared.db`) and `the_legacy_file_inside_a_split_directory_is_refused_by_name` (the other half of
that rule: once the directory IS a store, a `--store` naming the old file inside it is refused and
points at the DIRECTORY).

**P4 — the corpus sweep.** Built. `crates/harnessd/src/corpus.rs`,
`crates/harnessd/src/decision_source.rs`, `crates/harnessd/src/slash.rs`,
`crates/harnessd/tests/store_split_corpus.rs` (new), one line of `crates/harnessd/src/sessions.rs`
(`/gate` is handed the `StoreAt` rather than a path), `crates/tools/src/builtins/decisions.rs`
(`DecisionHits` and `DecisionCounts` carry the files a sweep could not read, and the answer names
them), and `Store::open_reader` in `crates/tokencore/src/store.rs`. What the doc asked for and what
landed: a scoped read resolves through the shared index — the `project` column the migration wrote
when a row has one, and otherwise the folder hash of the tree's ROOT workspace, walked up
`parent_session_id`, because a child's own workspace is its worktree and names no file (P2's step
1, done here from the index alone) — and opens that one file; `session=any`, `counts` and
`/gate counts|recent` sweep every `projects/*.db` in key order through `Store::open_reader`, which
is a read-only connection that migrates nothing and creates nothing, since `Store::open` would
have migrated another daemon's file; a file that will not open or will not answer is named in the
answer beside the count and the listing, and the sweep goes on with the rest; and the start-up
count is untouched, because the sink reads the handle it already holds. Three things the doc left
open, decided here: a count that fails is an ERROR and not a zero, so a file that is not a
database is named rather than counted as empty; a sweep's page is one page of the corpus, so each
file's newest `offset+limit` rows are merged and sorted before the page is taken, and `last` caps
the summed match set; and `request=<id>` stops the sweep at the file that holds the row, an id
being unique, rather than opening the other 28. What the review of P3 left broken and this fixes:
`find` read `adjudication` off the shared connection, which on a set is a file that has no such
table. Done-tests, all on temp directories: `a_corpus_sweep_reads_every_project_file_and_a_scoped_read_opens_one`
— three project files, `session=any` returns all three, `session=s-b` returns only the other
project's row, no `session` at all returns this daemon's own, `counts` follows the same two rules,
the start-up count stays at one, a `request=` is found in the third file, and with `c` overwritten
as junk the sweep names it and still returns the other two (and `/gate counts` says "2 decisions
recorded" with the unreadable file named, `/gate recent` lists neither);
`a_sweep_takes_no_write_lock_and_renames_nothing` — the other project's bytes are unchanged and
its journal holds no frames after a sweep; and `a_sweep_of_twenty_nine_project_files_is_measured`,
which is where the open question below got its number.

Left of P4: nothing. Left of R129: the live migration of the real store on this box —
`letibot-migrate-split ~/.local/share/letibot/sessions.db ~/.local/share/letibot`, with every
daemon stopped — which is the operator's to run, and the daemons go back on the split store after
it.

## Open questions

- `stable_prefix` duplication: measure the per-project total before deciding it is cheaper than
  a shared table.
- Children placed on another host: confirm they never write the parent's project file from the
  remote side.
- What a corpus sweep costs on 29 project files — the open, the count and the newest-200 list —
  measured after P3, since a sweep that takes a second would push `decisions` into the slow-call
  log and read as a store problem. **Answered by P4: 9-10 ms.** `a_sweep_of_twenty_nine_project_files_is_measured`
  builds 29 project files (8 decisions each) in a temp data directory and times
  `corpus::sweep_counts` plus `corpus::sweep_recent(200, 200)` — 29 read-only opens, 29 counts and
  a merged newest-200 — at 9-10 ms across reruns of a debug test build on a warm page cache. That
  is two orders of magnitude under `busy::SLOW_CALL`, so a `decisions session=any` is not the line
  that gets read as a store problem; the test fails if the sweep ever passes a second. It is a
  fixture's number, not lab2x1's: a cold cache on a 29-file store the size of that box's would be
  the disk's cost, which § What the daemons logged already says the split does not separate.
- Whether P0's instrument then shows the seven calls above to be waits or I/O. If they are I/O,
  the split's benefit on this box is the WAL per file and nothing more, and the disk is the work.
