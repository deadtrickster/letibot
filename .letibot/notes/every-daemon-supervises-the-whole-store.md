<!-- abstract: The supervisor's tick reads every open process in the shared store with no owner filter, while the gatekeeper board IS filtered by the lease owner - so a worktree daemon opens a session and draws an empty one. -->

# The supervisor supervises the whole store, once per daemon — and three more things one-daemon.md found

`Supervisor::tick` (`crates/harnessd/src/supervise.rs:216`) reads `Store::open_processes()`, which is
every record in `state IN ('starting','running','lost')` with no session or instance predicate
(`crates/tokencore/src/store/procs.rs:305`). One supervisor thread per daemon means N daemons on one box
make N ssh round trips a second over the same rows, and the tick prints only when something happened.

The gatekeeper board is the opposite: `fill_queue_board` (`crates/harnessd/src/harness.rs:3205`) builds
it from `gatekeeper::waiting(store, &me)` with `me` the lease owner, so a daemon without the lease opens
a session and draws nothing. The session is per daemon; the queue is per repository.

`crates/bridge/src/tools.rs:581` tells a model a worktree of the same repository is the same daemon. It
is not: `workspace_of` resolves `git rev-parse --show-toplevel`, which inside a worktree is the worktree.
