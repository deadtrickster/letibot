<!-- abstract: The reader floor is a per-arm declaration in `READER_ARMS`, recorded in `schema_reader_floor` and raised rather than bumped, because the binaries that need tolerating are the ones that would refuse a version bump. -->

# The reader floor is not the version, and three tests had pinned the version

`crates/tokencore/src/store/floor.rs` declares, for every arm of `migrate_from`, whether it
`Breaks` a younger reader or is `Additive`; the floor of a store at version V is the newest
`Breaks` at or below V, recorded in a one-row `schema_reader_floor` created if absent and
raised, never lowered.

Tolerating a newer store is not a change in `migrate` alone: every door that re-reads the schema
asks `floor::readable_here`, and the queue's lease claim was one of them -
`stored != SCHEMA_VERSION` would have kept the session and lost the queue.

Rakes: `Store` has no `Debug`, so `Store::open(p).expect_err(..)` does not compile - use
`.err().expect(..)`. And `migrate_from`'s arms are not in numeric order (19, 20 and 21 sit in
the order they landed), so a test comparing the declaration against the source must compare
sorted sets.

A store bumped by a build that predates the floor carries no row; `open` refuses it when it is
ahead, and `ensure_reader_floor` writes the row at the next migration.
