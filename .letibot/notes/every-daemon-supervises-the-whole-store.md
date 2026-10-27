<!-- abstract: MEASURED 2026-10-26 reading the code for docs/one-daemon.md: `supervise::spawn` ticks `Store::open_processes()` — every open record in the SHARED store, no owner filter — once a second from every daemon, so a host with one job gets N ssh round trips; the gatekeeper board is filtered by the lease owner so nine of ten worktree daemons open a queue session with an empty board; and the bridge's `leticode_attach` description claims a worktree of the same repository is the same daemon, which no key makes true. -->

# The supervisor supervises the whole store, once per daemon — and three more things one-daemon.md found

MEASURED 2026-10-26, branch `one-daemon-design`, read (not run) for `docs/one-daemon.md`.

## `supervise::tick` has no owner filter, and there is one supervisor per daemon

`crates/harnessd/src/cli.rs:1806` spawns one supervisor thread per daemon; `Supervisor::tick`
(`crates/harnessd/src/supervise.rs:283`) reads `Store::open_processes()`, which is every record in
`state IN ('starting','running','lost')` with **no session or instance predicate**
(`crates/tokencore/src/store/procs.rs:365-369`), groups by host, probes each host once, and writes
what it found. The `Owner` in the struct (`supervise.rs:187-190`) is only the lease reaper (RH9),
not a filter on what is checked.

So with N daemons on one box, a host with one running job gets N ssh round trips a second and N
writers of the same rows. The line each daemon prints is silent unless something happened
(`supervise.rs:1021-1025`), so the duplication is invisible in the log until there is news.

## The gatekeeper board is filtered by the LEASE OWNER

`Harness::fill_queue_board` (`crates/harnessd/src/harness.rs:10419-10438`) builds the board from
`gatekeeper::waiting(store, &me)` where `me` is `mergequeue::lease::this_daemon().owner`. So the ten
worktree daemons of one repository each open `s-gatekeeper-<worktree key>` — a session, a composed
prompt, a vocabulary — and nine of them draw an empty board because they do not hold the lease. The
session exists per daemon; the queue exists per repository.

## `leticode_attach`'s description is false about worktrees

`crates/bridge/src/tools.rs:651` says *"the daemon is found by the workspace's hash, so a worktree
of the same repository is the same daemon"*. `daemon::workspace_of` resolves `git rev-parse
--show-toplevel`, which inside a worktree is the WORKTREE, so `~/Projects/letibot/letibot` and
`~/Projects/letibot/wt-od` hash to different keys and are different daemons
(`crates/sessionlog/src/lib.rs:190-197`). The sentence is true only for a subdirectory of the same
worktree. A package that resolved a sibling worktree intended the same daemon and got another one —
which is *why* `--no-queue` exists (`mergequeue.rs:517-525`).

## The mode store refreshes at every session open BECAUSE daemons share the rows

`ModeStore` keeps `by_root` in memory, and `refresh()` is documented as *"called at every session
open and after every write, so no copy outlives the moment it is read"*
(`crates/harnessd/src/modes.rs:100-102`) — the fix for the era when `modes.tsv` was one file each
daemon rewrote whole. The same shape of debt: `Parts` holds one `ModeStore` per process
(`harness.rs:120-125`).

Which is the general lesson for the one-daemon question: every place the tree already had to undo
"one daemon per folder" is visible as a *refresh* or a *key derived from the workspace*. They are
the checklist for the change, not the obstacle to it.
