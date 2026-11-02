<!-- abstract: `activity` and `delivery` each matched a todo row against a job and had drifted - the activity ignored the job's slug and took a `Child` need for any handle - so `delivery::plan_says` is now the one predicate both read. -->

# The activity and the delivery decision had two copies of "this row names this job"

`delivery::facts_of` matched a row by `handle == c.job || c.slug == Some(handle)` - the job's id or the
name the model gave it - while `activity::awaited` compared the handle and nothing else and accepted
`TodoNeed::Child { id }` for any handle. So for a plan naming a job by its slug, the settlement was
`Delivery::Immediate(Why::Needed)` and the activity read `quiet`: `wait` exited *the turn ended* about a
session the same job was about to wake.

The fix is one predicate, `delivery::plan_says(rows, handle, slug, kind)`
(`crates/harnessd/src/delivery.rs:178`), called by both sides; the watcher set carries the slug too -
`JobWatchers::watching_now` (`crates/harnessd/src/jobwatch.rs:714`) returns `(handle, kind, slug)`.

The rule: `waiting_on_jobs: true` and `activity: quiet` cannot both be right. When a `wait` disagrees
with what the model believes, diff the two predicates before touching either, and pin the fix on both
sides - `activity.rs`'s `the_delivery_reads_the_plan_and_the_activity_does_not` asserts `plan_says` and
`decide` together, because a fix on one side lets them drift again.
