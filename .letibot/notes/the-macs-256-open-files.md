<!-- abstract: A workspace test binary on this Mac starts at Darwin's soft limit of 256 open files, so a test that opens six per harness and fifty at once dies of EMFILE in its own stub - and a thread-count assertion here is 0 == 0. -->

# `main`'s red was Darwin's 256 open files, and the fifty-subagent test asserts nothing about threads here

The gate's step 3 was red in two targets for one reason: `session_lifecycle`'s stub accept thread died
of `Os { code: 24, kind: TooManyOpenFiles }`, the stub went with it, and the test reported its subject
as missing (`stub turns: 0`). Each harness opens the store twice and sqlite in WAL keeps db + `-wal` + `-shm`, so fifty in parallel is
~350 descriptors against a limit of 256 (Linux's default is 1024, which is why CI never saw it).

`crates/harnessd/tests/support/room.rs:25`'s `room_for(what, need)` is the fix: raise the soft limit to
`min(4096, hard)`, print what was got, and ask `apparatus::present` for `need`, so a box that cannot
give it fails with that sentence under `LETIBOT_REQUIRE_APPARATUS=1` instead of timing out over the
stub's corpse. And the thread assertion measures nothing here: `memwatch::thread_names()`
(`crates/harnessd/src/memwatch.rs:246`) reads `/proc/self/task`, which Darwin has not, so
`assert_eq!(threads_after, threads_before)` is `0 == 0`.

Anything that opens more than a handful of descriptors per unit of work in a test binary here is
measuring the box's limit, not its own defect.
