<!-- abstract: MEASURED 2026-11-01: the child missing from task_result was not filtered — its subagent_run row was never written (sqlite: database is locked, printed and carried on), and the session row is the only witness left; with the numbers (517 of 573 child sessions predate the first record) and the rakes. -->

# A child absent from `task_result` was never recorded, not filtered

**MEASURED 2026-11-01**, branch `agent/the-lost-child-is-still-listed`, from the operator's
report that `s-1789462738453908838-sub-1793488007308855461` (`the-notices-stay-collapsed`) was
missing from a 54-child list after a 2 s restart.

## The answer, in one line from the daemon's own log

`~/logs/harnessd-42ce9f1aae08.log:5054`:

```
  s-1789462738453908838-sub-1793488007308855461 · not recorded (sqlite: database is locked); it runs, and a daemon restart will not bring it back
```

**None of the four candidate filters dropped it.** `grep -ac` finds 1456 mentions of the id
because its *session* rows are there; the `subagent_run` row is not, and never was. Its
session's `created_at` is 387 s after the id was minted, and it wrote 1456 rows and four
commits with no record at all. The next line in the log is the same loss again — its `job`
row: *"its start was not recorded: sqlite: database is locked"*.

`put_subagent_run` goes through `Store::exec` → `busy::retry_sql` with `BUSY_RETRY_TOTAL` =
60 s plus a 30 s busy timeout per attempt, so the write lost a lock **held for over a
minute** by another writer on the same 9 GB store (the log's neighbours show the merge
queue failing the same way). Waiting longer is not the fix; the record was simply gone.

## The window that makes "no record" a meaningful class

`select … from session where parent_session_id=… and not exists (subagent_run)` finds **517**
children of that one session — because nothing recorded children before schema 26. The
honest class is *children created at or after the store's first `subagent_run`*
(`MIN(started_ms)` over the whole table): for that session that is **exactly one**, and it is
the child that was lost. `Store::first_subagent_run_ms` + `children_without_runs(parent, since)`.

**A gatekeeper session must not be asked at all** — `record_start` refuses to record its
children (reviewers are the queue's), so "no record" is the ordinary state there: 388 + 59 + 11
children on this box would have been news to a model.

## The second witness, and where the state word comes from

The child's own session row is a *different* write (made by the child when it opens), and
`StoredSession::last_item` already answers "how did its last turn end": an `Assistant` with no
calls after it is a finished turn, anything else is a turn still going when the rows stopped.
That rule is the whole of `subagent_record::a_child_with_no_record`: `Done` with that text, or
`Failed` with a sentence naming its last row, its workspace and its session.

## Rakes

* **A child session's `title` is its BRIEF**, not a name (the whole task prompt, truncated).
  Using it as a slug makes `label()` print a paragraph — a lost child has no name and answers
  to its handle.
* **`u64 as i64` on a window wraps NEGATIVE.** `since_ms = u64::MAX` as `i64` is -1, which
  makes the window *everything* — the direction that hides the bug behind a wall of rows. Use
  `i64::try_from(..).unwrap_or(i64::MAX)`.
* **`Registry::new()` already returns `Arc<Registry>`** (again): `Arc::new(Registry::new())`
  is a type error.
* `TranscriptItem::ToolResult`'s fields are `edit`, `origin`, `media` — `who` is gone.
* `session` has **no index on `parent_session_id`**, so the query is a scan of the table
  (1690 rows here, sub-millisecond). Fine now; a store with 100k sessions would want one.
