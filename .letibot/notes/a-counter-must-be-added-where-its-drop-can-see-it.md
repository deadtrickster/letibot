<!-- abstract: A counter whose `Drop` always subtracts must be incremented where the struct exists, before the first `?` that can return with one - otherwise a refused store drives the count to `usize::MAX` and poisons the census. -->

# An add after the first `?` with a `Drop` that always subtracts wraps to `MAX`

`crates/tokencore/src/store.rs`'s `from_connection` built the `Store`, ran `store.migrate()?`, and only
then did `LIVE.fetch_add(1)`, while `Drop` subtracts on every path. A store whose migration was refused
- a floor above this build, a version with no floor - was dropped uncounted, and every later
`Store::live()` in that process was nonsense.

The add goes where the struct exists, before the first `?` that can return with one. Everything earlier
in the constructor returns before a `Store` exists, so nothing can be dropped uncounted. A `Live` token
field created only on success states the same invariant twice (a field and an `Option` check in `Drop`)
where the promise is only that a constructed `Store` is counted.

The test is a binary of its own, `crates/tokencore/tests/store_census.rs`: `--lib` runs a crate's tests
in parallel threads, and any sibling opening a store between the two reads makes the assertion
boundable at best. A one-test binary has no siblings.

`LIVE.fetch_sub` is the only counter `tokencore`/`harnessd` gives back in a `Drop`; `memwatch`'s heap
counter is balanced by the allocator's own non-null guard and has no `?` in it.
