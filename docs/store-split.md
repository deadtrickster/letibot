# Store split: one file per project, one shared file

Status: proposal, design settled, nothing built. The corpus question is decided: decisions live
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
- `/gate` (`crates/harnessd/src/slash.rs`), which opens a connection of its own on the store
  path: `counts` and `recent` follow the same two rules — one file for a scoped question, a
  sweep for the whole corpus.
- `Store::corpus(only_labelled, limit)` is `/gate recent`'s query; beyond that it is reached only
  from `tokencore`'s own tests.

The sweep is the accepted cost: 29 read-only opens on lab2x1, ordered by project key so that a
listing cut short is still a stable prefix. It takes no write lock on any file, so it cannot
stall a project's daemon the way a shared writer does; a file that has gone missing or is half
migrated is named in the answer and the sweep goes on with the rest. That answer is also why the
sweep is not on the start-up path: a daemon must not wait on 29 files to say hello.

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

**P1 — two files, one schema each, one writer each.** `crates/tokencore/src/store/split.rs`
(new), `crates/tokencore/src/store/split_tests.rs` (new), `crates/tokencore/src/store.rs` (the
open and DDL region only). `split.rs` holds this doc's table assignment as data: a table's name
decides its file. `StoreSet::open(dir, key)` opens `shared.db` and `projects/<key>.db`, and a
`Store` is one file driven by its own writer thread — callers hand it work and hold no lock
across a write (`exec` and `write_tx` keep their signatures, so no call site changes). Done-test:
every table in the two lists above is found in exactly one file and in neither the other; a
project file with a newer `schema_version` refuses to open by name; opening the set twice in one
process is a no-op, not a second connection.

**P2 — the offline migration** (before P3 runs on a box, after P1's schema exists).
`crates/tokencore/src/migrate_split.rs` (new), `crates/tokencore/src/lib.rs` (one `pub mod`
line), `crates/harnessd/src/bin/letibot-migrate-split.rs` (new),
`crates/harnessd/tests/store_split_migrate.rs` (new). The five steps of § Migration, the project
key from the session's root and never its worktree, one transaction per file, and the
verification as a refusal rather than a print. Done-test: a fixture store with two projects, a
child in a worktree and one empty project — every row lands in exactly one file, the key hashes
match, `shared.db` has no `adjudication`, a second run refuses, and a deliberately broken
verification leaves `sessions.db` in place under its own name.

**P3 — the daemon opens two paths.** `crates/harnessd/src/config.rs`,
`crates/harnessd/src/cli.rs`, `crates/harnessd/src/daemon.rs`,
`crates/bridge/src/daemon.rs`, `scripts/letibot`,
`crates/harnessd/tests/store_split_paths.rs` (new). `--store` becomes a data directory plus a
project key (the bridge already resolves the key from the folder); `sessions.db` is accepted as a
path only by P2's migration. Done-test: a daemon on a temp data directory writes a session to
`shared.db` and a transcript row to `projects/<key>.db`; `--sessions` still answers with that
project file renamed away, and lists the session.

**P4 — the corpus sweep.** `crates/harnessd/src/corpus.rs`,
`crates/harnessd/src/decision_source.rs`, `crates/harnessd/src/slash.rs`,
`crates/harnessd/tests/store_split_corpus.rs` (new). A scoped read resolves through the shared
index to one project file; `session=any`, `counts` and `/gate` sweep every project file
read-only in key order, name a file they could not read, and keep going; the start-up count stays
the daemon's own file. Done-test: three project files — `decisions session=any` returns rows from
all three, a session-scoped read returns only its own, and with one file unreadable the sweep
names it and still returns the other two.

## Open questions

- `stable_prefix` duplication: measure the per-project total before deciding it is cheaper than
  a shared table.
- Children placed on another host: confirm they never write the parent's project file from the
  remote side.
- What a corpus sweep costs on 29 project files — the open, the count and the newest-200 list —
  measured after P3, since a sweep that takes a second would push `decisions` into the slow-call
  log and read as a store problem.
- Whether P0's instrument then shows the seven calls above to be waits or I/O. If they are I/O,
  the split's benefit on this box is the WAL per file and nothing more, and the disk is the work.
