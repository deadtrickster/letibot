<!-- abstract: MEASURED 2026-10-26 (RH7): activity::awaited and delivery::facts_of each matched a todo row against a job, and the copies had drifted two ways — `awaited` ignored the job's SLUG (so a plan naming `release-build` was delivered at once while `wait` read `quiet`) and accepted `TodoNeed::Child` for any handle. -->

# The activity and the delivery decision had two copies of "this row names this job"

MEASURED 2026-10-26, branch `supervisor` (RH7), while chasing the operator's false wake
(`jobs_open: ["j13281"]`, `waiting_on_jobs: false`, `activity: quiet`).

## The two copies

`crates/harnessd/src/delivery.rs`'s `facts_of` matched a row by `handle == c.job || c.slug ==
Some(handle)` — the job's id **or the name the model gave it** (`bash(slug: "release-build")`, and
`TodoNeed::Job { handle: "release-build" }`). `crates/harnessd/src/activity.rs`'s `awaited` compared
the handle and nothing else, and accepted `TodoNeed::Child { id }` for **any** handle.

So, for a session whose plan names a job by its slug:

* the settlement is `Delivery::Immediate(Why::Needed)` — handed over at once;
* the activity reads `quiet`, so `wait` (which asks the activity) exits *the turn ended*.

That is a false *done* about a session that is about to be woken by exactly that job — the shape the
bridge's `wait` must never produce.

## What the fix is

One predicate: `delivery::plan_says(rows, handle, slug, kind) -> (when_row, needed)`, called by both.
The watcher set carries the slug now — `JobWatchers::mine` is `handle → (kind, Option<slug>)`, read
in `watch()` from the live job view at the one moment it exists (`host.job(&JobId)?.slug`), and for a
child from `TaskRunner::slug_of`. `watching_now()` returns `(handle, kind, slug)`.

## The rake

**`waiting_on_jobs: true` and `activity: quiet` cannot both be right**, and the daemon has two
readings of one rule — so when a `wait` disagrees with what the model believes, diff the two
predicates before touching either. The test that pins it is
`activity::tests::a_job_named_by_its_slug_counts_and_the_delivery_agrees`, and it asserts BOTH sides
(`awaited` is true **and** `decide` is `Immediate(Needed)`), because a fix on one side alone lets
them drift again.

## What was not established

The operator's `j13281` case was not opened (their store is off-limits to a session). It may have
been named by its slug; it may have been named by nothing at all, which would be the *delivery* rule
misbehaving rather than the activity — a plan-less job whose settlement arrives while the session is
idle is delivered at once (`Rule::Idle`), which the activity's `awaited` would never predict. That
second reading is not fixed and is not disproved.
