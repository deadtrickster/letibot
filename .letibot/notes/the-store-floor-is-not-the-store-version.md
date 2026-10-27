<!-- abstract: MEASURED 2026-10-26 (branch `store-tolerance`): three places in this tree pinned "a version above SCHEMA_VERSION means refuse" and the queue lease was one of them — so tolerating a newer store is a change in five files, not one, and `Store` has no `Debug` so `expect_err` on `Store::open` does not compile. -->

# The reader floor is not the version, and three tests had pinned the version

MEASURED 2026-10-26, branch `store-tolerance`, building the operator's ruling that *an older binary
keeps working when the newer schema only ADDED things, and says so in red on the status line*.

## The mechanism, in one line

`crates/tokencore/src/store/floor.rs`: `READER_ARMS` declares, for **every** arm of `migrate_from`,
whether it `Breaks` a younger reader or is `Additive`; the floor of a store at version V is the
newest `Breaks` at or below V. Recorded in a one-row table `schema_reader_floor(only_row CHECK 1,
min_reader)`, created `IF NOT EXISTS` at open and **raised, never lowered** — deliberately not a
version bump, because the binaries that need tolerating are the ones that would refuse one.

At `SCHEMA_VERSION` 33 the floor is **24**: v23 emptied the pre-split queue (rows an older reader
acts on, with no marker that says so) and v24 archived `merge_queue`/`merge_review` under `_v23`
names and put tables with no column list in common in their place. Everything else is additive —
including v8's backfill, which writes existing rows and still breaks nobody, because the columns it
fills did not exist for a v7 reader to read.

## The three places that pinned the VERSION, and the surprise

Tolerating a newer store is not a change in `migrate` alone. Every door that re-reads the schema
had the old rule baked in, and two of them were *tests* that had codified it:

* `Store::acquire_queue_lease` and `heartbeat_queue_lease` — the queue's own "second wall" (the
  daemon re-reads the file's schema live, in the claim's transaction, so a daemon already running
  when a newer build migrates the file finds out). `stored != SCHEMA_VERSION` → `WrongSchema`: a
  floor-tolerant open would have kept the session and lost the queue. Both now ask
  `floor::readable_here(conn, stored)`.
* `queues/tests.rs::a_store_of_another_schema_grants_no_lease_and_stops_a_heartbeat` pinned exactly
  the additive case as a refusal. Renamed `a_store_the_queue_has_moved_past_...`, it now asserts
  BOTH: a version bump with the floor unchanged keeps the lease; raising the floor takes it away.
* `harnessd/src/restart.rs::the_stop_is_recorded_after_a_newer_build_migrated_the_store` raised the
  version by one and asserted a fresh open is REFUSED. A bare version bump is now the additive
  case, so the fixture raises `schema_reader_floor` too — which is what a real newer build that
  makes an older daemon refuse actually does.

## Two rakes

* **`Store` has no `Debug`**, so `Store::open(path).expect_err("…")` does not compile (`T: Debug`
  is required, not just `E`). Use `.err().expect("…")`.
* **`migrate_from`'s arms are not in numeric order.** 19, 20 and 21 sit in the order they landed
  (`if from < 21` then `20` then `19`), each with a comment saying why. A test that scans
  `store.rs` for `if from < N` and compares the list against the declaration must compare SORTED
  sets — `every_migration_arm_declares_whether_it_breaks_a_reader` does, and that test is what
  makes a migration written by somebody who never heard of the floor fail rather than default to
  additive.

## The gap that is deliberately left open

A store bumped by a build that predates this module carries no floor row. `Store::open` refuses it
when it is ahead — *nobody said whether the migration was additive* is not *it was* — and the
window closes at the next migration applied by a build that knows about floors, which writes the
row for every store it touches (`ensure_reader_floor` on the `v == SCHEMA_VERSION` arm, one SELECT
and no write on the ordinary open).
