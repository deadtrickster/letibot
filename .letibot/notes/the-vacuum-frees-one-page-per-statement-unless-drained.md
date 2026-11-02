<!-- abstract: `PRAGMA incremental_vacuum` emits one row per page it frees, so a statement stepped once gives back one page - the rows must be drained; and a DELETE frees a page only when every row on it goes. -->

# `incremental_vacuum` frees one page per STATEMENT unless its rows are drained — and a delete frees nothing unless whole pages go

Drain it: `let mut rows = st.query([])?; while rows.next()?.is_some() {}`. `execute` and
`execute_batch` step once, so `execute_batch("PRAGMA incremental_vacuum")` freed 1 page of 923
free ones, and the day prune's vacuum had never shrunk anything since it was written. The
`sqlite3` CLI steps to completion, which is why the same statement looks fine there.

A delete frees a page only when every row on it goes: a fixture whose two days were interleaved
in id order freed 2,950 of 6,001 rows and left `freelist_count == 0`. Write the shrink test with
the days in id order, the way a real `metrics.db` is appended.

`PRAGMA auto_vacuum=INCREMENTAL` is a no-op once the file has tables, so an old `metrics.db` can
never give a page back: `PRAGMA auto_vacuum=INCREMENTAL; VACUUM;` once by hand. In WAL mode
`wal_checkpoint(PASSIVE)` after the vacuum is what makes the file on disk fall.

`page_count` not falling says nothing about whether the deletes worked: check `freelist_count`,
then whether a whole page was emptied, then whether the vacuum's rows were drained.
