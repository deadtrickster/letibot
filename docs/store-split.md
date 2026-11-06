# Store split: one file per project, one shared file

Status: proposal. Nothing here is built yet.

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
- `adjudication`: written by the session that asked. Read as one corpus through the shared
  file's view (below).

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

## The corpus

The adjudicator reads decisions across sessions. Two options:

1. Decisions are written to the project file and the shared file keeps a slim copy (request id,
   tool, verdict, verdict_by, mode, decided_ms) for cross-project queries. One extra small write
   per decision.
2. Decisions live only in project files and a corpus read attaches every project file read-only.
   No duplicate, but a corpus query opens every project file (29 on lab2x1).

Proposal: option 1. Decisions are rare compared to transcript writes, and the corpus query stays
one file.

## Migration

One offline pass, run with every daemon stopped:

1. Map every session to its tree's root by `parent_session_id`, and the root's
   `workspace_root` to a project key. A child's own `workspace_root` is its worktree and must not
   be used: on lab2x1 it gives 570 distinct folders, the roots give 29.
2. For each key, create `projects/<key>.db` with the project schema and copy that key's rows of
   every project table, in one transaction per file.
3. Create `shared.db`, copy the shared tables, add the `project` column to `session`.
4. Verify: per table, row counts and a hash of the primary keys match between old and new. Every
   transcript's hash chain is re-verified the way a resume does.
5. Rename `sessions.db` to `sessions.db.pre-split`. It is kept until the operator deletes it.

A session whose workspace no longer exists still gets a project file under its recorded key.
`--migrate` moves a daemon to another host; its project file goes with it instead of rows being
copied out of the shared store.

## Open questions

- `stable_prefix` duplication: measure the per-project total before deciding it is cheaper than
  a shared table.
- Children placed on another host: confirm they never write the parent's project file from the
  remote side.
- Order of work: measure long writes on the current store first, so the split is shown to remove
  the stalls that are actually seen.
