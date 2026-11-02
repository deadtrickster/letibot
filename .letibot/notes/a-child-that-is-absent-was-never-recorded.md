<!-- abstract: A child missing from `task_result` was never recorded - its `subagent_run` write lost a lock held over a minute - so the session row is the only witness, and both store reads take the "due" window from `first_subagent_run_ms`. -->

# A child absent from `task_result` was never recorded, not filtered

The daemon's log says it: `not recorded (sqlite: database is locked); it runs, and a daemon restart
will not bring it back`. `put_subagent_run` goes through `busy::retry_sql` with `BUSY_RETRY_TOTAL` 60 s
plus a 30 s busy timeout per attempt, so the write lost a lock held for over a minute by another writer
on the same store. Waiting longer is not the fix; the record is gone.

"No record" only means something inside a window: nothing recorded children before schema 26, so a plain
`not exists (subagent_run)` query finds hundreds of healthy children.
`Store::children_without_runs` and `Store::child_without_run` both call the private `unrecorded_of`,
which takes the window from `first_subagent_run_ms` itself, so a list and a keyed read cannot disagree;
 A gatekeeper session's children are never recorded
at all (`record_start` refuses), so do not ask.

The class travels with the row: `UnrecordedChild::why: NoRecord::{Due, BeforeTheStore}`, worded in one
place, `subagent_record::no_record_cause`.

Rakes: a child's `title` is its BRIEF, not a name, and `u64 as i64` on a window wraps NEGATIVE, so use
`i64::try_from(..).unwrap_or(i64::MAX)`.
