<!-- abstract: MEASURED 2026-10-27: `PRAGMA incremental_vacuum` yields ONE ROW PER PAGE it frees, so `execute_batch`/`execute` (stepped once) gives back exactly one page — the rows must be drained; and a DELETE only frees a page when every row on it goes, so a shrink test's fixture must delete a contiguous id prefix. -->

# `incremental_vacuum` frees one page per STATEMENT unless its rows are drained — and a delete frees nothing unless whole pages go

MEASURED 2026-10-27, branch `metrics-cap` (wt-mc, tools on lab2x1), giving `metrics.db` a size cap
(`crates/metrics/src/store.rs`, `Writer::shrink`). Two facts, both of which look like "the vacuum is
broken" and are not.

## 1. One page per call, whatever the argument

In this build of SQLite (3.46.1, both the `sqlite3` CLI and the bundled `rusqlite`) a 955-page file
with 923 free pages:

| call | pages after |
|---|---|
| `execute_batch("PRAGMA incremental_vacuum(100000)")` | 954 |
| `execute_batch("PRAGMA incremental_vacuum")` (no argument) | 953 |
| `BEGIN; PRAGMA incremental_vacuum(100000); COMMIT;` | 948 |
| **`let mut rows = st.query([])?; while rows.next()?.is_some() {}`** | **26** |

`PRAGMA incremental_vacuum` emits one ROW per page it frees, and a statement that is stepped once
yields one page and stops. `execute_batch` and `execute` both step once. The same statement read to
its last row frees everything. So: **drain it**, and pass a limit only when you want a bounded wait.

The consequence for this tree: the day prune's `execute_batch("PRAGMA incremental_vacuum;")` had
never shrunk anything since it was written — a file could lose 200M rows and keep its bytes. (The
same mistake is invisible in the `sqlite3` CLI, which steps to completion: the CLI probe of the same
file went 505 → 256 pages in one statement, which is what sent me looking.)

## 2. A delete frees a page only when every row on it goes

The size cap deletes the oldest end of the file, so it frees whole pages. Measured on a first
fixture whose two days were **interleaved in id order** (`ts = start + (i % 2) * DAY`): deleting
2,950 of 6,001 rows left `freelist_count == 0` and `page_count` unchanged — every leaf page held a
mix of deleted and surviving rows. The cap then could not shrink at all, and the fixture, not the
code, was wrong: a real `metrics.db` is appended in time order, so its oldest day is a contiguous
id prefix. **Write a shrink test with the days in id order.**

## 3. `auto_vacuum` cannot be switched on afterwards

`PRAGMA auto_vacuum=INCREMENTAL` is a no-op once the file has tables, so a `metrics.db` created by a
build that did not set it (before the first `CREATE TABLE`) can never give a page back. What the
operator runs once, and what `Writer::shrinks()` reports:

```sh
sqlite3 ~/.local/share/letibot/metrics.db 'PRAGMA auto_vacuum=INCREMENTAL; VACUUM;'
```

Read back with `PRAGMA auto_vacuum` (2 = incremental) *after* `open` has asked for it: a fresh file
answers 2, an old one 0. In WAL mode the truncation lives in the write-ahead log until a checkpoint,
so `wal_checkpoint(PASSIVE)` after the vacuum is what makes the file on disk fall.

## The rake

`PRAGMA page_count` not falling says **nothing** about whether the deletes worked: check
`freelist_count` too (`live = page_count − freelist_count`), then ask whether a whole page was
actually emptied, then whether the vacuum's rows were drained.
