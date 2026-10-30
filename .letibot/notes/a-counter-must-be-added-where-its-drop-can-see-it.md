<!-- abstract: MEASURED 2026-10-30: `Store::live()`'s add sat after `migrate()?`, so a refused store's unconditional `Drop` took the counter to `usize::MAX` and poisoned the census for the whole process — and the one test that measures by it asserted a bound that `MAX` satisfies trivially. -->

# An add after the first `?` with a `Drop` that always subtracts wraps to `MAX` — MEASURED 2026-10-30, branch `agent/a-counter-that-underflows`

`crates/tokencore/src/store.rs`'s `from_connection` built the `Store`, ran `store.migrate()?`, and
only then did `LIVE.fetch_add(1)`. `Drop` subtracts on every path. So a store whose migration was
refused (a floor above the build, a version with no floor) was dropped uncounted: `usize::MAX`, and
every later `Store::live()` in that process is nonsense.

## The shape that is right, and it is one line's position

The add goes **where the struct exists**, before the first `?` that can return with one, because
`Drop`'s sub is unconditional. Everything earlier in the constructor returns before a `Store`
exists, so nothing can be dropped uncounted. A `Live` token field created only on success is the
other honest shape and it is worse: the same invariant stated twice (a field, an `Option` check in
`Drop`) where the promise is only *a constructed `Store` is counted*, and it hides the connection a
migration holds — which is what a census about connections is for.

## Why the test is a binary of its own

`crates/tokencore/tests/store_census.rs` — one test, its own process, so the numbers are exact.
`--lib` runs a crate's tests in parallel threads and any sibling opening a store between the two
reads makes the assertion boundable at best. The same reasoning is `store_handles.rs`'s `SERIAL`
mutex, taken to its end: a one-test binary has no siblings.

## The sweep — the class has one member

`LIVE.fetch_sub` is the only counter `tokencore`/`harnessd` gives back in a `Drop`. `memwatch`'s
heap `LIVE` is balanced by the allocator's own non-null guard and has no `?` in it.
`SpawnAnnounced`/`ChildSessionGuard` in `harness.rs` are announce-then-withdraw guards, not
counters, and their announce is adjacent to the guard with nothing fallible between.
