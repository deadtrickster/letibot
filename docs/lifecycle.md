# Forceful resource lifecycle for letibot: research and design

Date: 2026-11-12. Input: `/build/oss-harnesses/LIFECYCLE-BRIEF.md`. Letibot paths are relative to
`/home/dead/Projects/letibot/letibot`. Nothing in that repository was edited, built or run.

Contents: (a) what other harnesses do, (b) what the web says, (c) the proposal reviewed,
(d) the design, (e) the package split.

## Summary of the position

1. Ownership is proved by where a resource lives and what it carries (a cgroup, a directory
   root, a ref namespace, a label, a run file), stamped at creation by letibot. The table of
   holdings is an index and a journal over those stamps. It is not the proof, and it is not
   something an agent is asked to keep.
2. An owner has three separate lifetimes: compute (ends at close, idle or TTL), work (ends at
   landed or archived, and is always archived first), caches (bounded by quota and by the
   owner's end). "Reap on landed or closed" as one trigger is too coarse.
3. The reaper is a box-level program run by a systemd user timer and by the daemons. It does
   not need the session, the agent or the session's daemon. On a remote host a local reaper
   acts on a lease that the home box renews, so a partition ends in a kill, not in a leak.
4. Letibot already has half of this: cgroup scopes with `cgroup.kill` and a counted result
   (`crates/tools/src/exec/scope.rs`), a `process` table with pid identity
   (`crates/tokencore/src/store/procs.rs`), and a VM reaper for subagent children
   (`crates/harnessd/src/vm_reap.rs`). The design extends these. It does not replace them.

## (a) What the other harnesses do

How this was checked: two read-only research agents opened every cited line. I re-read five
of them myself (grok-build `reason.rs` and `auto_gc.rs`, oh-my-pi `isolation-ownership.ts`,
opencode `pipe/unix.rs`, deepseek-harness `linux-scope.ts`) and they matched. The rest is
their reading, not mine.

Path prefixes:

- **OC** = `/build/oss-harnesses/opencode/packages` (commit 532d099)
- **PI** = `/build/oss-harnesses/pi-mono/packages` (commit 0f57b31)
- **OMP** = `/build/oss-harnesses/oh-my-pi/packages` (commit cc61891), **OMPC** = `OMP/coding-agent/src`
- **GB** = `/build/oss-harnesses/grok-build/crates/codegen`
- **DS** = `/build/oss-harnesses/deepseek-harness/packages`
- **GC** = `/build/oss-harnesses/grok-cli/src`

### a.1 Table

| | opencode | pi-mono | oh-my-pi | grok-build | deepseek-harness | grok-cli |
|---|---|---|---|---|---|---|
| Worktrees for agents | tool action only, removal needs `--force` on request | none | per task, with an owner marker (pid + start time) | per subagent, kept by default | experimental, removed on archive unless dirty | none |
| Unlanded work on removal | refused when dirty | - | workspace moved to `.retained-*`; failed branch kept | snapshot to `refs/grok/subagents/<id>`, then a keep-reason gate | non-force remove, branch always kept | - |
| Orphan worktree sweep | DB rows only | - | manual `omp worktree clear`, skips live owners | at startup: dead-pid pool sweep and age GC (7 d, at most every 6 h) | none | - |
| Sandbox / VM / docker | none | none | none in the agent | Landlock/Seatbelt process-wide; cgroup v2 per command | bwrap `--die-with-parent`, Landlock, Seatbelt | `shuru` microVM per command, untracked |
| Process containment | process group per command, zombie held so the pgid cannot be reused | `detached`, `kill(-pid, SIGKILL)` | setsid group; `nohup` survives | setsid + `killpg`; cgroup `cgroup.kill` on drop | `systemd-run --user --scope --collect` per command, process group fallback | direct child only |
| Kill ladder | TERM, KILL after 3 s | KILL at once | TERM, KILL after 1 s | TERM, KILL after 1 s | TERM, KILL after 3 s | TERM, KILL after 1 s (3 s for background) |
| On session end | jobs cancelled, children recursed | registered cleanups run | jobs cancelled by `ownerId`; children parked | `ProcessScope.kill_all` | owner's jobs cancelled and awaited | `cleanup()` sends TERM only |
| After a crash of the harness | daemon stops when its one owner connection closes | nothing | workers poll the parent every 250 ms and kill themselves | subagent records repaired; no process sweep | bwrap dies with parent; nothing else | nothing; background delegations outlive the parent |
| Default bash timeout | 120 s | none | 300 s | 120 s, backgrounded after 15 s, max 10 h | 120 s, max 600 s | 30 s |
| Idle / TTL | location services evicted after 60 min idle | none | broker exits 3 s after last client; LSP mux 15 min; children parked after 420 s | background task max 10 h | none | none |
| File retention | tool and shell output 7 d, swept hourly | none | `omp gc`, manual, preview unless `--apply`; cold archive after 30 d | off unless `cleanup_ttl_days` is set | spill files 30 d, swept at startup | none |
| Session archive / delete | delete cascades to children and rows | trash or unlink | delete removes transcript and artifacts | delete removes the directory, not worktrees | archive only | nothing deletes |
| Owner registry | Effect `Scope` + `Job` registry | `registerSessionResourceCleanup` | `postmortem` registry, 10 s deadline | `ProcessScope` per session + global, Drop guards | Cordis `ctx.effect` disposers | none |

### a.2 Notes per harness

**opencode.** The interesting part is the shell daemon. Each command is its own process
group, the kill is `killpg(SIGKILL)` plus `kill(pid)`, and the exited child is left a zombie
so its pgid cannot be reused while a kill might still be aimed at it
(`OC/process-daemon/src/service/pipe/unix.rs:1-14, 109, 187-195`). The daemon has exactly one
owner connection: when it closes with no handoff ticket the daemon stops, a ticket lives
120 s and startup must be claimed within 5 s (`OC/process-daemon/src/ownership.rs:5-6, 50-60,
122-131`). That is a lease by connection. Shutdown kills every process with a 5 s watchdog
before `exit(1)` (`OC/process-daemon/src/daemon/server.rs:264-280`). Tool stop is TERM then
KILL after 3 s (`OC/core/src/shell.ts:50, 371-387`). Retention is by age on a timer: tool
output 7 days, hourly (`OC/core/src/tool-output.ts:15, 153-158`), shell output the same
(`shell.ts:46-47, 207-233`). Session delete interrupts, cancels jobs, closes the transport
and recurses into children (`OC/core/src/session.ts:405-418`), and it does not touch a
worktree. Worktree removal is `git worktree remove`, `--force` only when asked
(`OC/core/src/git.ts:1428-1439`), and the model's tool is told to force only when the user
confirmed (`OC/core/src/tool/plugin/opencode.ts:41-43`). `refresh` drops DB rows whose
directory is gone and is not a git prune (`OC/core/src/worktree.ts:273-326`). Idle eviction
exists for location services: 60 min, swept each minute, held by an active session or job
(`OC/core/src/location-activity.ts:30, 79-117`). No containers, no cgroups.

**pi-mono.** Minimal. Bash is `detached` with its pid tracked
(`PI/coding-agent/src/core/tools/bash.ts:127, 136, 188`) and `killProcessTree` goes straight
to `kill(-pid, SIGKILL)` (`PI/coding-agent/src/utils/shell.ts:220-262`). Tracked pids are
killed on TERM, HUP, a dead terminal or an uncaught crash
(`PI/coding-agent/src/modes/interactive/interactive-mode.ts:4519, 4549, 4586`). Bash has no
default timeout (`bash.ts:41`). Spill files go to the OS tmpdir and are never cleaned
(`PI/coding-agent/src/utils/output-files.ts:1-30`). The registry is
`registerSessionResourceCleanup`, run from `AgentSession.dispose`
(`PI/ai/src/session-resources.ts:1-24`). Nothing survives a SIGKILL of pi itself being
noticed by anyone.

**oh-my-pi.** The closest to letibot's problem. Each isolation directory carries an owner
marker with the pid and a start-time token, so a recycled pid is not mistaken for a live
owner (`OMPC/task/isolation-ownership.ts:15-60`, `OMPC/task/worktree.ts:629-679`). Cleanup
stops the backend, then removes; a failed unmount keeps the directory with a sidecar
(`worktree.ts:686-711`). When capture or merge fails the workspace is moved to `.retained-*`
instead of being deleted (`OMPC/task/isolation-runner.ts:355-386, 489-536`). Branches are
`omp/task/<id>`, force-deleted when merged and kept when the merge failed
(`worktree.ts:972, 1039-1086`). Orphans are removed only by a manual command that skips
sandboxes whose owner is live (`OMPC/cli/worktree-cli.ts:11-16, 196-253`). `omp gc` is
manual and previews unless `--apply` (`/build/oss-harnesses/oh-my-pi/docs/session.md:601`).
Worker subprocesses poll their parent every 250 ms and SIGKILL themselves when it is gone
(`OMPC/subprocess/parent-watchdog-worker.ts:1-60`). The launch broker exits 3 s after its
last client (`OMPC/launch/broker.ts:1551-1567`). Idle children are parked to disk after
420 s (`OMPC/task/settings.ts:555-563`), and a top-level shutdown parks children rather than
killing them (`OMPC/session/agent-session.ts:6693-6708`). Subagent `maxRuntimeMs` defaults
to unlimited (`settings.ts:522-525`). `nohup` jobs survive cancel and session close
(`/build/oss-harnesses/oh-my-pi/crates/pi-shell/src/session.rs:37-46`). A `postmortem`
registry runs teardowns on exit with a 10 s deadline (`OMP/utils/src/postmortem.ts:64`).

**grok-build.** The most complete worktree story. A subagent worktree is kept by default
(`GB/xai-grok-shell/src/agent/subagent/handle_request.rs:2357-2374`; the reclaim feature is
`default_enabled: false`, `GB/xai-grok-config-types/src/registry.rs:285-289`). With the
feature on it snapshots to `refs/grok/subagents/<id>` first and removes only when a gate
answers `Now` (`handle_request.rs:2434-2530`). The gate's keep reasons are an enum: `Dirty`,
`HiddenFromStatus`, `IgnoredContent`, `EmbeddedRepo`, `Unpushed`, `OnlyCopy`,
`NotInSnapshot`, `WorktreeLocalState`, `NoRepo`, `CheckFailed`, `GateTimedOut`
(`GB/xai-fast-worktree/src/git/reason.rs:10-22`). Note `CheckFailed` and `GateTimedOut`:
not being able to tell is a reason to keep. Startup runs two sweeps off the main thread
(`GB/xai-grok-shell/src/agent/mvp_agent/acp_agent.rs:157-162`): pool worktrees whose `.pid`
owner is dead (`GB/xai-grok-shell/src/session/worktree_pool.rs:29-94`), and an age GC with
7 days default, at most once per 6 h, `Manual` worktrees never expiring
(`GB/xai-fast-worktree/src/auto_gc.rs:39-52`). The GC skips a worktree with a live creator
pid or any live process whose cwd is inside it (`GB/xai-fast-worktree/src/api/gc.rs:295-337`).
Processes: `setsid` and `killpg` (`GB/xai-tty-utils/src/lib.rs:171-176, 654-668`), a
`ProcessScope` per session whose `kill_all` runs whenever a session leaves memory including
idle unload (`GB/xai-tty-utils/src/process_scope.rs:173-250`,
`GB/xai-grok-shell/src/agent/mvp_agent/session_lifecycle.rs:199-208`), and a cgroup v2 group
per Linux command that writes `cgroup.kill` and rmdirs on drop
(`GB/xai-grok-tools/src/computer/local/cgroup.rs:252-265`). Background tasks have a hard
10 h bound (`GB/xai-grok-tools/src/computer/local/terminal.rs:41-46`). Session cleanup by
age is off unless configured (`GB/xai-grok-shell/src/agent/config.rs:684-687`). Deleting a
session does not remove its worktrees. No sweep of orphaned processes after a crash.

**deepseek-harness.** The one that uses systemd the way letibot does, one level finer: each
command runs in `systemd-run --user --scope --collect --unit=dsh-subprocess-*`
(`DS/subprocess/subprocess-local/src/linux-scope.ts:540-560, 686`), empty scopes are stopped
explicitly (`linux-scope.ts:385-409`), with a process group as the fallback
(`DS/subprocess/subprocess-local/src/spawn.ts:93-146`). bwrap gets `--unshare-pid
--die-with-parent` (`DS/sandbox/sandbox-local/src/profiles.ts:17`). Jobs are capped at 10
per owner, and when the owning agent is disposed its jobs are cancelled and awaited
(`DS/jobs/jobs-local/src/index.ts:34, 656-671`). The experimental worktree plugin runs a
non-force `git worktree remove` on archive, keeps anything dirty, untracked, locked or
detached, always keeps the branch, and never sweeps worktrees of sessions archived earlier
(`DS/experimental/worktree/README.md:31`). Tools that declare no timeout have none
(`DS/guard/timeout-policy/src/index.ts:60-71`). No startup sweep of orphaned scopes. A scope
per command is also a warning for letibot: it multiplies transient units, and the unit table
is finite (b.3).

**grok-cli.** The negative control. Background processes are `detached: false` with TERM
only at cleanup (`GC/tools/bash.ts:194-199, 343-364`), SIGTERM or an uncaught exception
exits with no cleanup (`GC/index.ts:43-57`), a subagent's own shell is never cleaned up
(`GC/agent/agent.ts:1228, 1389-1391`), and background delegations are spawned detached and
`unref()`'d with a pid that is recorded and never used (`GC/agent/delegations.ts:96-115`).
Each command's microVM is not tracked (`GC/tools/bash.ts:450-480`). Its tables have
`ON DELETE CASCADE` and nothing ever deletes (`GC/storage/migrations.ts:41-125`).

### a.3 What to take from them

1. **Nobody has a ledger. Everybody who cleans up uses a scope or a marker.** opencode's
   `Scope`, pi's cleanup registry, grok-build's `ProcessScope`, deepseek's `ctx.effect`:
   ownership is the structure the resource was created inside. oh-my-pi and grok-build add a
   marker on the directory (pid plus start time) for what must survive the process.
2. **All of them are in-process.** Every registry dies with the harness. Only grok-build
   sweeps anything at the next start, and only worktrees. None has a reaper that runs when
   the harness is not running. That is the gap letibot's complaint is about, and nobody
   fills it.
3. **Work is preserved by a snapshot ref plus a gate that keeps when unsure**
   (grok-build), or by moving the workspace aside (oh-my-pi). Nobody deletes a dirty tree.
4. **The best ones keep by default and reclaim behind a flag.** grok-build ships its
   reclaim off. That is the honest state of the art: it is why their users' disks fill too.
5. **Liveness by "is any process's cwd inside it"** (grok-build `gc.rs:295-337`) is a cheap
   extra guard before removing a directory. The design adopts it.
6. **No one handles docker, VMs or memory per owner.**

## (b) What the web says

Collected by a research agent from primary pages. Items it could not confirm on a primary
page are marked.

### b.1 Coding agents

- **Claude Code** creates `--worktree` checkouts under `.claude/worktrees/<name>/` on branch
  `worktree-<name>`. A clean unnamed session's worktree and branch are removed on exit; one
  with changes, untracked files or new commits prompts. Subagent worktrees
  (`isolation: worktree`) are removed when the subagent finishes without changes; otherwise
  a periodic sweep removes them once older than `cleanupPeriodDays`, and never while they
  hold changed or untracked files or unpushed commits. The sweep only touches worktrees
  carrying Claude's marker in git metadata. It holds a `git worktree lock` while an agent
  runs and releases locks whose owning process has exited. Non-interactive `-p` runs never
  clean up. https://code.claude.com/docs/en/worktrees
- `cleanupPeriodDays` defaults to 30 and covers transcripts, tool results, file history and
  orphaned worktrees. https://code.claude.com/docs/en/claude-directory
- **Codex app** keeps the most recent 15 managed worktrees, deletes on chat archive or over
  the limit, and saves a snapshot before deleting; pinned and in-progress chats are
  protected. https://learn.chatgpt.com/docs/environments/git-worktrees
- **Codex cloud** keeps cached container state up to 12 hours.
  https://learn.chatgpt.com/docs/environments/cloud-environment
- **Cursor** has `cursor.worktreeMaxCount` (default 25 per machine) and a cleanup interval,
  oldest first, and says worktrees made outside Cursor under the same root "are eligible for
  deletion". https://cursor.com/docs/configuration/worktrees That last clause is the
  pattern-match hazard, shipped.
- Cursor cloud agent retention and Devin's sleep-on-inactivity: unconfirmed.

The shape across all three: a marker proves ownership, a count or age bound triggers the
sweep, a snapshot is taken before deletion, and work that is not pushed is never removed.

### b.2 Kubernetes

- Ownership is a reference on the dependent (`ownerReferences`), and it is scoped:
  "Cross-namespace owner references are disallowed by design."
  https://kubernetes.io/docs/concepts/architecture/garbage-collection/
- Deletion cascades in the background by default. Foreground deletion marks the owner with
  `deletionTimestamp` and a `foregroundDeletion` finalizer and removes dependents first.
  Orphaning is an explicit policy. Same URL.
- A finalizer keeps an object in Terminating until its list is empty, which is how a stuck
  cleanup stays visible instead of vanishing.
  https://kubernetes.io/docs/concepts/overview/working-with-objects/finalizers/
- `terminationGracePeriodSeconds` defaults to 30 s: TERM, then KILL. A forced delete with
  zero grace can leave the workload running.
  https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle/
- `ttlSecondsAfterFinished` deletes a finished Job after a TTL and is sensitive to clock
  skew. https://kubernetes.io/docs/concepts/workloads/controllers/ttlafterfinished/
- Liveness is a Lease whose `renewTime` the holder updates.
  https://kubernetes.io/docs/concepts/architecture/leases/

Taken: the owner tree, children first; `stuck` as a state that blocks and is seen; TTL after
finished; a lease instead of a liveness probe for a remote host; scope of an owner reference
limited to one box.

### b.3 systemd and cgroup v2

- `KillMode=control-group` is the default and kills everything in the unit's cgroup; SIGKILL
  follows after `TimeoutStopSec=`; `KillMode=process` is "not recommended".
  https://www.freedesktop.org/software/systemd/man/latest/systemd.kill.html
- Without `--collect`, failed transient units are "kept in memory until the user explicitly
  resets their failure state". `CollectMode=inactive-or-failed` unloads them.
  https://www.freedesktop.org/software/systemd/man/latest/systemd-run.html
  https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html
- The manager's limit is `#define MANAGER_MAX_NAMES 131072`.
  https://github.com/systemd/systemd/blob/main/src/core/manager.h Hitting it returns
  `-E2BIG`, shown as "Argument list too long", which is the message R192 quotes.
  https://bugzilla.redhat.com/show_bug.cgi?id=2148170
- `MemoryHigh=` throttles; `MemoryMax=` is the hard limit, to be used as the "last line of
  defense". `Delegate=` hands the subtree to the unit.
  https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html
- `OOMPolicy=kill` sets `memory.oom.group=1`; the default is `continue` when `Delegate=` is
  on. `RuntimeMaxSec=` terminates a unit after a wall-clock bound.
  https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html
- systemd-oomd kills only leaf cgroups or those with `memory.oom.group=1`.
  https://www.freedesktop.org/software/systemd/man/latest/systemd-oomd.service.html
- `cgroup.kill` kills the whole subtree and is safe against concurrent forks;
  `cgroup.events` `populated` says when it is empty; `memory.events` counts `high`, `max`,
  `oom`, `oom_kill`, `oom_group_kill`. https://docs.kernel.org/admin-guide/cgroup-v2.html

Taken: letibot's scopes already use `--collect`, `Delegate=yes` and `cgroup.kill`. Missing
are a slice, `MemoryHigh`, `RuntimeMaxSec` and `memory.oom.group` on jobs. One scope per
daemon is the right grain: a scope per command (deepseek-harness) spends the unit table.

### b.4 Docker

- Labels are fixed for the life of an object.
  https://docs.docker.com/engine/manage-resources/labels/
- `docker system prune` takes `label` and `until` filters; `until` does not cover volumes,
  and volumes are pruned only with `--volumes`, anonymous ones only.
  https://docs.docker.com/reference/cli/docker/system/prune/
  `docker volume prune --all` adds named volumes, with the `label` filter.
  https://docs.docker.com/reference/cli/docker/volume/prune/
- `--rm` removes a container and its anonymous volumes, not named ones.
  https://docs.docker.com/reference/cli/docker/container/run/
- There is no default-label mechanism: dockerd's `--label` labels the daemon, and the CLI's
  `config.json` has no label property. https://docs.docker.com/reference/cli/dockerd/
- Testcontainers' Ryuk reaps by label (`org.testcontainers.sessionId`) when the owning
  process's connection has been closed for 10 s: containers, networks, volumes and images.
  https://github.com/testcontainers/moby-ryuk

Taken: a label is the only ownership docker offers, nothing puts it on by default, so the
harness must put it on at the API. Reap by label filter, with a connection or lease as the
liveness signal. Volumes need their own rule because `until` does not apply to them.

### b.5 Git

- `git worktree remove` refuses an unclean worktree without `--force`; `lock` blocks prune
  and removal; `prune` clears admin files of missing worktrees.
  https://git-scm.com/docs/git-worktree
- `git gc` keeps everything reachable from "anything else in the refs/* namespace". Loose
  unreachable objects go after two weeks (`gc.pruneExpire`), unreachable reflog entries
  after 30 days. https://git-scm.com/docs/git-gc
- A ref outside `refs/heads` is not pushed by the default push and not fetched by a default
  clone. https://git-scm.com/docs/git-push https://git-scm.com/docs/git-clone
- `git stash create` makes a commit without storing it in any ref, and documents no way to
  include untracked files without touching the tree. https://git-scm.com/docs/git-stash
- `git bundle create <file> --all` is an offline copy of refs and their objects.
  https://git-scm.com/docs/git-bundle

Taken: a custom ref namespace is a complete, cheap archive. A dangling commit is not (two
weeks). The dirty tree needs a scratch index rather than stash. A bundle is the archive for
a host that cannot be fetched from.

### b.6 MicroVMs and hosted sandboxes

- Firecracker's jailer: "It's up to the user to handle cleanup after running the jailer."
  https://github.com/firecracker-microvm/firecracker/blob/main/docs/jailer.md
- Modal sandboxes default to a 5 minute lifetime, `timeout` up to 24 h, plus `idle_timeout`.
  https://modal.com/docs/guide/sandboxes
- E2B sandboxes have a timeout that can be extended, 24 h at most.
  https://docs.e2b.dev/sandbox
- Fly machines stop or suspend when idle (`auto_stop_machines`).
  https://docs.fly.io/launch/autostop-autostart

Taken: every hosted sandbox has a hard TTL and an idle timeout, on by default, extended by
an explicit call. None waits for the workload to say it is done. This is the rule the
proposal lacks.

### b.7 Proving a process is yours

- `/proc/<pid>/stat` field 22 is the start time; with the pid it identifies a process
  across pid reuse. https://man7.org/linux/man-pages/man5/proc_pid_stat.5.html
- A pidfd refers to one task and becomes readable on its exit.
  https://man7.org/linux/man-pages/man2/pidfd_open.2.html
- Cgroup membership with `cgroup.kill` and `populated` is the race-free form (kernel doc
  above).

## (c) The main session's proposal, point by point

What letibot has today, read from the code, because the proposal has to be judged against it:

- **Processes.** Every tool process joins a cgroup before `exec`, in a tree
  `letibot.<pid>.<n>/session.<sid>/turn.<tid>/job.<n>` with an `explicit.<name>` sibling that
  outlives the session (`crates/tools/src/exec/scope.rs:1-13`). Ending a scope writes
  `cgroup.kill` and then checks absence (`scope.rs:685-688`, `:941`). Per-job `memory.max` is
  set (`scope.rs:661`), and `memory.events` `oom_kill` is read back (`scope.rs:344-348`).
- **Process records.** The `process` table holds session, handle, host, pid, pgid, an
  `identity` that makes a reused pid "not this process", a wall-clock `deadline_ms` and a
  six-state life including `lost` for a host that did not answer
  (`crates/tokencore/src/store/procs.rs:15-47`). The supervisor ticks every second
  (`crates/harnessd/src/supervise.rs:14`) and `teardown_session` ends every open process of a
  session on every host (`supervise.rs:402-436`).
- **Daemon scope.** The launcher starts each daemon with
  `systemd-run --user --scope --collect -p Delegate=yes -p OOMPolicy=continue`, unit
  `<stem>-<key>-<pid>` (`scripts/letibot:330-338`). No slice, no memory bound on the scope, no
  runtime bound.
- **Daemon idle exit.** Default 3600 s (`crates/harnessd/src/config.rs:1716`). It is refused
  while any session "has a head attached" (`crates/harnessd/src/sessions.rs:3039-3048`). A
  bridge's head counts, which is how a finished package holds its daemon for ever.
- **VMs.** `FirecodeBackend::down` runs `firecode down --project`, waits up to 600 s for the
  copy-out record, then removes the booted copy (`crates/tools/src/firecode.rs:839-903`). It
  runs from `Drop` (`firecode.rs:913-916`), so it runs only when the thread that holds the
  backend unwinds. A turn stuck in a tool call never drops it, which is R193.
- **VM reaper.** `vm_reap::pass` runs every 5 min inside each daemon
  (`crates/harnessd/src/vm_reap.rs:24`, `:428-461`). It maps a VM to an owner by its project
  directory under letibot's cache (`vm_reap.rs:187-191`) and judges it by the owner's
  `subagent_run` row. A VM whose owner has no `subagent_run` row is `Unrecorded` and is kept
  (`vm_reap.rs:80-88`). A top-level session started with `leticode_start vm: true` has a
  `session` row and no `subagent_run` row, so the reaper that exists is blind to exactly the
  case in R193. The kill ladder itself is right: `firecode down`, the run's `cgroup.kill`,
  SIGKILL by pid with the command line rechecked (`crates/tools/src/firecode/held.rs:300-345`).
- **Worktrees.** Letibot does not create a package's worktree. `leticode_start` takes an
  existing path (`crates/bridge/src/tools.rs:1067-1074`); the caller ran `git worktree add`.
  The merge queue has the careful half: `remove_worktree` refuses a dirty tree
  (`crates/harnessd/src/mergequeue/git.rs:1136-1165`), `commit_proof` proves a commit is on the
  target before a branch goes (`git.rs:159`), and `reap_worktrees` prunes registrations whose
  directory is gone (`git.rs:1256-1292`). None of it is applied to package worktrees.
- **Remote hosts.** A remote session's home is `<root>/.letibot-home`
  (`crates/tools/src/remote.rs:1292`). Its processes are ended by an environment mark and pid
  files, because "a host has no cgroup" (`remote.rs:1904-1909`). That sweep runs from the
  session's close, over ssh, from the home box.
- **Sessions.** `archived_ms` and `archived_reason` exist, with `leticode_archive` (TODO R184).
  Auto-archive of package and child sessions is listed there as left to do.
- **firecode.** `firecode gc [keep]` exists and refuses to sweep a live run
  (`/home/dead/Projects/firecode/bin/firecode:7755-7800`). Nothing in letibot calls it.
  `firecode down --discard` exists (`firecode:4068-4097`).
- **Docker.** Nothing. No label, no record, no socket policy.

### 1. A ledger of what each owner created: right idea, wrong direction of proof

Right: without a per-owner index there is no "holds:" line and no plan.

Wrong: "every package or session records each resource as it creates it". The things that
filled the disks were made by commands inside the agent's shell: `git worktree add`,
`git branch`, `cargo build` with a target dir, `docker run`, `mktemp`. No hook sees those. A
ledger that depends on the creator writing a row has the same failure as a signal a blocked
agent has to send. It is also the wrong side of a crash: create, then crash, then no row.

What to do instead:

- **Stamp, do not record.** Letibot decides where an owner's things go before the agent
  starts: one cgroup subtree, one owner root directory, one ref namespace, one docker label
  set, one firecode run stamp. Anything inside is the owner's because of where it is. The
  kernel enforces the first. Letibot's own launcher enforces the rest.
- **Write the row before the act**, for the things letibot itself creates (VM, daemon,
  worktree, remote home). `subagent_run` already does this: "the row is written BEFORE the
  child's session row exists" (`crates/tokencore/src/store/subagents.rs:13-16`). A row with no
  resource is harmless. A resource with no row is the leak.
- **Discover the rest at reap time** by walking the stamps. The table then learns what the
  agent made without the agent saying so.

Also: the word "ledger" is taken twice in this codebase, by the token ledger
(`crates/tokencore/src/ledger.rs`) and by the remote job directory (`remote.rs:1904`). The
table is called `holding` below.

### 2. Reaping on landed or closed: right about force, wrong about one trigger

Right: no cooperation, kill the cgroup, bring the VM down, stop the daemon.

Wrong or missing:

- **One trigger for three lifetimes.** At close the compute must go and the work must stay
  (a closed session is resumable by design, R184). At landed the work products can go. A
  target directory follows neither: deleting it at every close throws away the incremental
  build, and keeping it until landed is how 735G accumulated. See (d).
- **The trigger that the complaint is about is missing.** "Reports completion, its work gets
  incorporated, and then it just hangs" is neither closed nor, as far as letibot can tell,
  landed: the caller merged by hand and never called `leticode_stop`. The fix is an idle
  lease and a hard TTL on every non-interactive owner, the way every hosted sandbox does it
  (b.6). An owner that nobody renews is closed by the reaper.
- **"Stop the daemon once it holds no sessions"** is today's rule, and today's rule is what
  fails: the bridge's attached head is the hold (`sessions.rs:3046`). A closed owner must
  release its holds, and a package daemon must exit when its last package owner closes, not
  an hour later.
- **`firecode down --discard` is the wrong default.** Discard means nothing comes back. For a
  forced reap of an owner whose work is not proved landed, the order is `down` with copy-out,
  bounded, then archive the copy-out, and `--discard` only when the owner is `settled`.
- **"Delete the branch once merged"** needs a definition of merged that survives a squash or
  a rebase landing. The cheaper rule is in point 3.
- **Cascade.** A subagent of a package is not mentioned. Owners form a tree
  (`session.parent_session_id`), and a reap is foreground-cascading: children first.

### 3. Never lose work: right, and it should be unconditional

`refs/archive/<owner>` before removal is right. Three changes:

- **Archive always, not only when unmerged.** A ref costs nothing, and "is it merged" is the
  judgement that goes wrong. Every branch an owner made is copied to
  `refs/letibot/archive/<owner>/…` before it is deleted. Then deletion is never a decision
  about value.
- **A dirty tree is committed by the reaper, with a scratch index**, including untracked
  files that are not ignored. `git stash create` leaves untracked files out.
- **Copy-outs and remote placements are work too.** The `x-vm-*` branches are a hand-made
  version of this. The reaper fetches a copy-out's commits into the archive namespace and
  commits its dirty tree before the 12G of copies go.

### 4. Orphan sweep: right, except "known patterns that have no owner"

A periodic sweep with a grace period is right. Reaping by pattern is how another project's
workload gets touched. firecode's own `gc` carries the confession in a comment: counting by
recency "I did that to four of somebody else's VMs" (`firecode:7772-7779`). Rule: a pattern
produces a report row, never a reap. The operator turns a report row into an owned holding
with `letibot gc --adopt`, once, and from then on the normal path applies.

Missing: who runs the sweep when every daemon is dead, and what runs on a remote host that
the home box cannot reach.

### 5. Docker shim on PATH: too weak to be the proof

A PATH shim is bypassed by `/usr/bin/docker`, by compose plugins, by any SDK or `curl` on the
socket, and it cannot label a pulled image at all. If the label is the proof of ownership,
the label has to be put on at the API. Options, strongest first: docker inside the VM (the
guest's daemon dies with the guest, nothing to label); no docker socket in a host seat; a
per-owner API proxy socket that injects labels on create and build. The shim is at most a
convenience on top. Images are a shared cache and are never owner-reaped.

### 6. RAM: right, with the level wrong

`memory.max` per job exists. What is missing is accounting and a bound per owner and for
letibot as a whole. One `memory.max` on a session's whole subtree with group kill would turn
one runaway test into the loss of every job of that session, which is the lesson of R64 one
level down. Design: `memory.high` at the owner, `memory.max` plus `memory.oom.group` at the
job, one `letibot.slice` above all daemons so other projects' workloads have a floor. And a
VM's memory is the firecracker process's, in the run's own cgroup, outside the session
scope: the report has to add it. memwatch watches the daemon's own heap
(`crates/harnessd/src/memwatch.rs:1-3`), not owners.

### 7. Visible: right, add reversible

`letibot gc` as plan and `--now` as act is right. Missing: a journal of what was reaped and
where its archive is, a restore verb, a hold verb, and a `stuck` state the operator is told
about instead of a reaper that retries silently.

### What the proposal does not mention

- firecode run directories (3,809) and state layers (224G): record class, retired by
  firecode's own verb, per run.
- The session rows themselves: auto-archive of settled package sessions (R184's remainder).
- Tool-call bounds (R194): the cheapest forced cleanup is a call that cannot hang.
- The systemd unit table (R191): the units that filled it were not letibot's. The sweep
  reports table fill and resets only `letibot-*` failed units.
- A reaper that fails half way, two reapers at once, pid reuse, a store that cannot be
  opened, clock skew to a remote host.

## (d) The design

### d.1 Terms

- **Owner**: the unit that things belong to and that is reaped as a whole. Kinds: `session`
  (interactive, operator-held), `package` (started by `leticode_start`), `subagent` (a child
  session), `gate` (a merge queue run), `review`. Owner id is the session id where there is
  one (`s-…`), else `q-<run>`. Owners form a tree by parent.
- **Holding**: one resource of one owner on one host.
- **Class** of a holding: `compute` (costs RAM or a slot while it exists), `work` (may hold
  the only copy of something a person wants), `cache` (can be rebuilt), `record` (logs, run
  directories, session rows).
- **Stamp**: the mark on the resource itself that names its owner and this box.

### d.2 Data model

Two tables in the shared store file (the one that holds `session`, so a box-level reaper
opens one file), plus a journal.

```sql
CREATE TABLE owner (
  id            TEXT PRIMARY KEY,      -- s-… or q-…
  kind          TEXT NOT NULL,         -- session | package | subagent | gate | review
  parent_id     TEXT,                  -- owner tree; NULL at the top
  repo          TEXT,                  -- main checkout this owner works against
  state         TEXT NOT NULL,         -- live | closing | closed | settling | settled | swept | stuck
  held_until_ms INTEGER,               -- operator hold; NULL = none, 0 = for ever
  lease_ms      INTEGER NOT NULL,      -- last renewal (activity)
  idle_ttl_ms   INTEGER,               -- NULL = never closed for idleness
  hard_ttl_ms   INTEGER,               -- NULL = no hard bound
  closed_ms     INTEGER, closed_why TEXT,
  landed_sha    TEXT,                  -- set when the queue proves the work is on the target
  settled_ms    INTEGER,
  created_ms    INTEGER NOT NULL
);
CREATE TABLE holding (
  owner_id   TEXT NOT NULL,
  kind       TEXT NOT NULL,            -- see d.5
  host       TEXT,                     -- NULL = this box
  handle     TEXT NOT NULL,            -- path, ref, run id, unit, container id
  class      TEXT NOT NULL,            -- compute | work | cache | record
  state      TEXT NOT NULL,            -- intended | held | reaping | gone | kept | stuck | lost
  stamp      TEXT NOT NULL,            -- what on the resource proves it (d.4), as checked last
  bytes      INTEGER,                  -- last measured size, for the plan
  archive    TEXT,                     -- where its content was preserved, once it was
  why        TEXT,                     -- last verdict sentence
  created_ms INTEGER NOT NULL, checked_ms INTEGER, attempts INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (owner_id, kind, host, handle)
);
CREATE TABLE reap_event (              -- append-only journal
  at_ms INTEGER NOT NULL, owner_id TEXT NOT NULL, kind TEXT, host TEXT, handle TEXT,
  act TEXT NOT NULL,                   -- plan | signal | kill | down | archive | remove | keep | stuck | restore
  said TEXT NOT NULL,                  -- one sentence: what, why, and where the archive is
  by TEXT NOT NULL                     -- daemon instance, `gc timer`, or the operator's command
);
```

The `process` table stays the record for processes. `holding` does not duplicate it: the
compute holdings are the scope (one row per owner cgroup), the VM, the daemon and containers.
`subagent_run` and `daemon_life` are read as they are.

On every host, each owner also has a small file `owner.json` in its owner root (d.4), with
the owner id, the home box id, the kind, and the lease. It is what a host-local reaper and a
reaper with no store can read.

### d.3 Owner states and what triggers each reap

```
live ──close trigger──▶ closing ──compute gone──▶ closed ──settle trigger──▶ settling ──▶ settled ──▶ swept
  ▲                         │                        │                            │
  └──── reopened ───────────┴────────────────────────┘                          stuck (any step that
        (resume by id)                                                             failed twice; told)
```

- **live**: may hold anything.
- **closing**: compute is being reaped (d.5). Bounded: 10 s of SIGTERM, then `cgroup.kill`,
  plus the VM's copy-out bound.
- **closed**: no compute. Work, caches and records remain. The session's rows are untouched
  and it can be reopened. Reopening moves it back to `live` with a fresh lease, and a fresh
  VM if it had one.
- **settling / settled**: work has been archived and removed, caches removed, the session
  archived (R184) with reason `settled: landed <sha>` or `settled: archived unlanded`.
- **swept**: no holding left that is not `gone`. The owner row and the journal stay for ever.
- **stuck**: a reap step failed twice. Nothing further is removed for that holding. The
  operator sees it in every `letibot gc` and once in the room.

Close triggers. Any one is enough, and none is an agent's action:

| Trigger | Applies to | Source |
|---|---|---|
| `leticode_stop`, `leticode_archive`, `--archive`, head quit of its own session | all | daemon, on the frame |
| The queue landed the owner's branch | package, subagent | merge queue, where `commit_proof` answers |
| Parent closed | subagent, review | cascade, children first |
| Idle: no turn in flight, no job running, no input from a head, for `idle_ttl` | package 30 min, subagent 30 min (today's `STALL_MS`), gate 10 min after its run, session never | reaper |
| Hard TTL since creation or last reopen | package 24 h, gate 6 h, session never | reaper, and `RuntimeMaxSec=` on the unit as the backstop |
| Its daemon is gone: `stopped_ms` set, or beat stale and no `harnessd` at that pid | all but an interactive session with a live head elsewhere | box reaper, `vm_reap::daemon_of` reused |
| Lease not renewed (remote host) | holdings on that host | host-local reaper |

An attached bridge head does not renew a lease. What renews it: a turn starting or ending, a
tool call returning, a message from the operator or the caller (`leticode_send`), an explicit
`letibot hold`. A package that reported completion and is not spoken to closes 30 minutes
later, its VM comes down with its copy-out, and its daemon exits. Nothing is lost by that:
`leticode_send` to a closed session reopens it.

Settle triggers:

| Trigger | Grace before work is removed |
|---|---|
| Closed and landed (`landed_sha` set) | 1 h |
| Closed, not landed | 24 h, then archived and removed |
| `leticode_archive` or `letibot gc --settle OWNER` | none |
| Disk pressure on the holding's filesystem (under 10% free) | caches at once; work as above, never sooner |

A hold (`letibot hold OWNER [--for 2d]`) stops every trigger except the operator's own
commands. It is the only opt-out, it is visible in `holds`, and it expires.

### d.4 How ownership is proved

Reaping needs two keys that agree: the stamp on the resource and a row (an `owner` row at
least). The table of stamps:

| Kind | Stamp (on the resource) | Why an outsider cannot have it by accident |
|---|---|---|
| process | membership of the owner's cgroup, under a unit named `letibot-…` in `letibot.slice`; for a host with no cgroup, the `process` row's `(pid, identity)` | the kernel puts a fork in its parent's cgroup; identity defeats pid reuse |
| daemon | its unit name, its runtime record `$RUNDIR/<key>.json`, `(pid, start time)` | unit names are unique per manager |
| VM | `runs/<run>/project` is under letibot's cache and named for the owner (today's `owner_of`), plus a new `runs/<run>/owner` file written at `up`; firecracker's `--id <run>` rechecked before each signal | the run directory is firecode's own record |
| directory (worktree, target, tmp, home, copy-out) | it is inside the owner root, which carries `owner.json` with this box's id; path canonicalised, no symlink followed, one filesystem | only letibot makes owner roots |
| worktree | `<gitdir>/worktrees/<name>/letibot-owner` holding the owner id, and the path is in the owner root | the admin directory is outside the tree the agent works in |
| branch | `branch.<name>.letibot-owner` in the repository's config, set when letibot creates it; or the branch's creating reflog entry is inside the owner's life and the branch appears in that worktree's own `logs/HEAD` | a branch somebody else made never appears in this worktree's HEAD log |
| docker object | labels `letibot.owner`, `letibot.box`, put on by the API proxy | the proxy is the only path to the socket from a seat |
| remote resources | same directory rule under the host's `root`, and the host's own `owner.json` | same |

Rules:

1. **Stamp and row agree** on the owner: reapable when the owner's state says so.
2. **Row, no resource**: the holding is `gone`. No action.
3. **Stamp names an owner the store does not know** (a lost store, another box's owner): it
   is listed as `unrecorded`, never reaped by the timer. `letibot gc --unrecorded --now`
   reaps those older than 7 days, compute and cache only. Work goes to the archive first, as
   always.
4. **No stamp**: not letibot's. It can appear in the report under "looks like ours" when a
   configured pattern matches, with its size. The only way it is ever reaped is
   `letibot gc --adopt PATTERN`, which writes holdings under an owner `legacy-<date>` in
   state `closed`, shows the list, and from there the normal plan and `--now` apply.
5. **The never-touch list is checked first** (`Supervisor::may_touch`, already there for
   processes), for every kind.
6. **A reaper never reaps the owner it runs under**, nor that owner's ancestors.
7. **A directory with a live process whose cwd is inside it, from outside the owner's
   cgroup, is kept** for that pass and said (grok-build's guard, a.2). Somebody is looking.
8. **Not being able to tell is a reason to keep.** A check that errors or times out gives
   `kept`, with the error, never `reap`.

The owner root: `<gc.root>/<box>/<owner>/` with `wt/`, `target/`, `tmp/`, `home/`, `out/`.
`gc.root` is a setting per host, `/build/letibot` on lab2x1 so builds never land on the root
disk. The launcher exports `CARGO_TARGET_DIR`, `TMPDIR`, `HOME` (for a confined seat) and
`DOCKER_HOST` pointing inside it, and the seat's bwrap binds make the root writable and its
siblings not. An agent that overrides `CARGO_TARGET_DIR` to a path it cannot write gets an
error, not a leak.

#### Naming conventions: how far they go

A name that embeds the owner id does three things for free: discovery is a glob, the sweep
can attribute a resource with no store at all, and the operator can read `ls`. So every name
letibot chooses carries the owner's short id (`<sid8>`):

| Thing | Name |
|---|---|
| owner root | `<gc.root>/<box>/<sid>/` with `wt/`, `target/`, `tmp/`, `home/`, `out/` |
| branch | `pk/<sid8>/<name>` |
| archive | `refs/letibot/archive/<sid>/…` |
| fetched work (today `x-vm-*`, `remote/<host>/<id>`) | `refs/letibot/fetch/<sid>/…`, outside `refs/heads` |
| cgroup | `session.<sid>` (already) under a unit `letibot-<stem>-<key>-<pid>.scope` in `letibot.slice` |
| firecode run | `runs/<run>/name` = `lb-<sid8>`, project dir named `<sid>` (already) |
| docker | `COMPOSE_PROJECT_NAME=lb-<sid8>`, container name prefix `lb-<sid8>-` |
| remote home | `<root>/.letibot-home/<sid>/` |

Two of these remove work rather than organise it. Fetching collected commits into
`refs/letibot/fetch/` means they are never branches, so there is nothing in the branch list
to reap. And `COMPOSE_PROJECT_NAME` in the seat's environment makes compose itself label
every container, network and volume with `com.docker.compose.project=lb-<sid8>`, which
covers compose without the proxy (compose's labelling is from memory, not re-checked here).

Where a name is enough as proof: in a namespace only letibot writes. `refs/letibot/*`,
`letibot.slice`, `<gc.root>`, the `letibot.*` label keys. There, name plus a known owner row
is reapable.

Where a name is not enough: in a namespace shared with people and other tools.
`refs/heads/pk/…`, a container name, a directory outside the root. Anyone can make a thing
with that name, and Cursor's "worktrees made outside Cursor under the same root are eligible
for deletion" (b.1) is what reaping by name there looks like. In those places the name finds
the candidate and the second key decides: the branch config or HEAD-log proof, the label,
`owner.json`. A name with no second key is a "looks like ours" report row.

### d.5 Per resource kind

Order inside one owner: children's owners first, then compute, then work (archive, then
remove), then cache, then record. Every step ends by looking: a step is done when the thing
is absent, not when the command returned (`scope.rs` already counts "what was there before
the kill and what survived").

| Kind | Class | Created by | Reaped at | How | Preserved |
|---|---|---|---|---|---|
| cgroup scope `session.<sid>` and below | compute | `exec/scope.rs` | close | SIGTERM to the scope, 10 s, `cgroup.kill`, verify empty, `rmdir` | job logs stay |
| `explicit.<name>` scope | compute | the model, on purpose | owner settled, or hard TTL | same | same |
| remote processes | compute | `remote.rs` | close; lease expiry on the host | mark and pid files today; after G4 a `systemd-run --user --scope` on hosts that have a user manager, then `cgroup.kill` | logs |
| firecode VM | compute | `FirecodeBackend::up` | close | `firecode down` bounded, wait copy-out up to 600 s, then `held::reap`'s ladder. `--discard` only for a settled owner or a gate | the copy-out (below) |
| daemon | compute | launcher | last non-closed owner gone (package daemons: at once; interactive: today's idle exit) | `Stop` frame, SIGTERM, 5 s, SIGKILL by `(pid, start)`; unit has `RuntimeMaxSec` | store rows |
| docker container, network | compute | agent, through the proxy | close | `docker rm -f` / `network rm` by `label=letibot.owner=<id>` | none; logs to the owner root first |
| docker volume | work | agent | settle | removed by label only after the owner is settled; a named volume is listed in the plan with its size | not archived; 24 h grace even when landed |
| docker image | cache | pull or build | never per owner | built images carry the label and go at settle; pulled images are left to a box-level opt-in policy (d.7) | - |
| worktree | work | `leticode_start` (new) or adopted | settle | archive (d.8), then `git worktree remove --force`, `prune` | `refs/letibot/archive/<owner>/…` |
| branch (owner namespace or proved) | work | letibot or agent | settle | archive ref, then `git branch -D` | archive ref |
| fetch branches (`remote/<host>/<id>`, `x-vm-*`) | work | collect | settle | moved to the archive namespace | archive ref |
| VM copy-out (`~/.cache/letibot/firecode/<sid>-<ts>`) | work | firecode | settle | fetch its commits and commit its dirty tree into the archive namespace of the source repo, then remove | archive ref |
| booted copy (`cache/<sid>`) | cache | `FirecodeBackend::up` | VM down with a recorded copy-out (`done` or `none`) | remove, unless it holds a HEAD no source ref contains or content the source lacks; kept when the copy-out failed | it is the only copy then: becomes `work`. The guest's commits and dirty tree go to `cache/<sid>.rescue.bundle` before `down`, kept unless the delivered tree has the guest's HEAD |
| target dir in the owner root | cache | cargo | close | remove | none |
| shared target (`target-landing*`, queue targets) | cache | queue | quota | owner is the queue; removed whole when over quota or unused for 3 days, under the build lock `rrun-runner` already takes | none |
| tmp, confined home | cache | tools | close | remove | none |
| remote owner root (`.letibot-home/<sid>` and build dirs) | cache + work | `remote.rs` | close for cache; settle for the tree | after the branch is home or bundled (d.8) | bundle on that host, or fetched ref |
| firecode run dir | record | firecode | owner settled, VM down, copy-out not pending | `firecode gc --run <id>` (a new, small firecode verb); until it exists, move to `runs-archive/<yyyy-mm>/` | console log, result, verdict kept in the archive |
| firecode state layers of a per-owner project | cache | firecode | owner settled | firecode's own removal for that project slug; if firecode has no verb for one slug, this is a firecode row to file | none |
| session rows | record | store | settle | `set_archived` with the reason (R184) | never deleted |
| systemd units | record | launcher | - | `--collect` already; `reset-failed` for `letibot-*` only | - |

Two rules worth stating once:

- **Compute is reaped at close without a grace period**, except the VM's copy-out wait. A
  closed owner being resumable is what makes that safe.
- **Nothing of class `work` is removed unless its `archive` column was written in the same
  pass and the archive was read back.** A failed archive is `stuck`, not a delete.

### d.6 The orphan sweep

One program, `letibot gc`, run three ways: by a daemon when one of its owners changes state
(the fast path), by a systemd user timer `letibot-gc.timer` every 10 minutes on every box
(the path that needs no daemon), and by the operator. One at a time per box
(`flock` on `$RUNDIR/gc.lock`), and each holding is claimed by a compare-and-set on its
`state` and `attempts`, so two reapers cannot both act.

One pass:

1. **Owners.** For each `live` owner: is its daemon alive (`daemon_life` beat under 90 s, or
   a `harnessd` still at that pid, the rule in `vm_reap::daemon_of`)? Has its idle or hard
   TTL passed? Is its parent closed? Any yes moves it to `closing`.
2. **Discover.** For each owner not `swept`, walk its stamps: the cgroup subtree, the owner
   root, the worktree's HEAD log for branches, `docker ps -a --filter label=…`, firecode's
   runs with its `owner` file. Insert holdings that are missing. Measure bytes.
3. **Unowned.** Walk the same places for stamps that name no known owner (rule 3), and the
   configured patterns (rule 4). Report only.
4. **Plan.** For each holding, a verdict sentence: reap now, reap at a time, or keep and
   why. This is what `letibot gc` prints.
5. **Act** (`--now`, and always for the timer and the daemons, limited to rules 1 and 2 of
   d.4). In the order of d.5, journal each act.

Grace periods, in one place:

| What | Grace |
|---|---|
| compute after close | none (10 s SIGTERM) |
| VM after its owner finished, when the owner's own `down` may be delivering | 10 min (today's `FINISHED_GRACE_MS`) |
| compute of an owner whose daemon is gone | 90 s of stale beat, then none |
| work of a landed owner | 1 h |
| work of an unlanded closed owner | 24 h |
| docker volumes | 24 h after settle trigger |
| unrecorded stamped resources | 7 days, and only on the operator's command |
| run dirs, logs | at settle; archived, not deleted |
| archive refs, session rows, journal | never expire on their own |

### d.7 Docker, RAM and cgroups

**cgroups and systemd.**

- All daemons go under one slice: `systemd-run --user --scope --slice=letibot.slice …`. The
  slice gets `MemoryHigh=` (default 60% of RAM, a setting) so letibot as a whole is throttled
  before it starves other projects' workloads. No `MemoryMax=` on the slice by default: a
  hard cap there kills an arbitrary daemon.
- Each daemon scope keeps `Delegate=yes`, `OOMPolicy=continue`, `--collect`. A package daemon
  adds `-p RuntimeMaxSec=<hard TTL + 1 h>` and `-p TimeoutStopSec=30`. That bound is enforced
  by the user manager, so it holds with every letibot process wedged.
- Inside the delegated tree: `memory.high` on `session.<sid>` (the owner's share), and on
  each `job.<n>` the `memory.max` that exists today plus `memory.oom.group=1`, so a job that
  hits its cap dies whole instead of leaving half a test run.
- Every gate step and job gets `RLIMIT_CORE=0` (R191). The sweep prints the user manager's
  unit count against its limit, and resets failed units named `letibot-*` only.
- When no scope can be made (R192) the owner is created with `compute: unbounded` in its
  `holds` line, and its hard TTL falls back to kill by process group and `(pid, identity)`.

**RAM visibility.** `letibot holds` sums, per owner: `memory.current` and `memory.peak` of
its session cgroup, the same two for its VM's run cgroup (firecode records
`runs/<run>/cgroup`), and its containers' cgroups when the proxy placed them. `letibot holds
--top` sorts owners by that sum and prints the box's `MemAvailable`. An OOM kill read from
`memory.events` becomes one line in the owner's session, naming the job.

A VM that runs out of memory inside the guest is a sizing problem (R200), not a reaping
problem. The owner's holds line shows the guest size so the caller can see it.

**Docker.** Three placements, three rules:

1. **Session in a VM**: docker, if the image has it, runs in the guest. It goes with the
   guest. Nothing to label. This is the recommended way to give a package docker.
2. **Host seat, default**: no docker socket is bound into the seat. `docker` fails with a
   sentence that names the two ways to get it.
3. **Host seat, docker granted**: `DOCKER_HOST=unix://<owner root>/docker.sock`, served by a
   small proxy in the daemon. It forwards the Engine API and rewrites create calls:
   containers, networks and volumes get `letibot.owner=<id>` and `letibot.box=<box>` labels;
   builds get the same through the `labels` parameter; container create gets a
   `HostConfig.CgroupParent` under the owner's scope when the engine's cgroup driver allows
   it, and a memory limit when none was given. List calls are filtered to the owner's label,
   so one owner cannot `rm` another's or another project's container. Bind mounts outside
   the owner root and `--privileged` are refused.

Reaping is then `docker ps -aq --filter label=letibot.owner=<id>` and `rm -f`, and the same
for networks and volumes. Never `docker system prune` without the label filter.

**Other projects' docker debris** (the 124G of images, the dangling volumes): letibot cannot
prove any of it and does not touch it. `letibot gc --docker-report` prints sizes grouped by
compose project label and age, and the exact `docker … prune --filter` commands the operator
could run. An opt-in section in config (`[gc.docker] prune_images_unused_for = "30d"`,
`prune_label = ["com.docker.compose.project=bench-*"]`) makes the timer run those, box by
box. Off by default.

### d.8 How unlanded work is preserved

Namespace: `refs/letibot/archive/<owner>/`. It is outside `refs/heads`, so it is in no branch
list, it is not pushed or fetched by default refspecs, and it keeps its commits reachable so
`git gc` never prunes them.

For a worktree, before removal:

1. For each branch proved the owner's (d.4):
   `git update-ref refs/letibot/archive/<owner>/heads/<branch> <sha>`.
2. The worktree's HEAD, if detached:
   `refs/letibot/archive/<owner>/head`.
3. If the tree is dirty: with a scratch `GIT_INDEX_FILE`, `git add -A` (which honours
   `.gitignore`, so `target/` is not swept in), `git write-tree`, `git commit-tree -p HEAD`
   with a message naming the owner and the time, `update-ref
   refs/letibot/archive/<owner>/wip`. The worktree's real index is not touched.
4. Guard: if the untracked, not ignored content is over 512 MB, do not commit it. Move the
   tree to `<gc.root>/<box>/.trash/<owner>-<ts>` instead (a rename on one filesystem), keep
   it 7 days, and say so in the plan. A build output dropped into the tree by mistake must
   not be written into the object store.
5. Read back each ref and compare. Only then `git worktree remove --force`, `git branch -D`.

For a VM copy-out or a remote placement: `git fetch <copy> '+refs/heads/*:refs/letibot/archive/<owner>/vm/heads/*'`
and its HEAD, then step 3 run inside the copy and fetched the same way. For a remote host
that cannot be reached from home at settle time, the host-local reaper writes
`git bundle create <root>/.letibot-archive/<owner>.bundle --all` inside the placement before
removing the tree, and the bundle is a `work` holding the home box fetches later.

The session's conversation is never touched: rows stay, `archived_ms` hides them, opening
brings them back (R184).

Restore: `letibot gc --restore OWNER` recreates branches from the archive refs and, with
`--worktree`, a worktree at the archived head with the `wip` commit checked out as
uncommitted changes (`git checkout <wip> -- .` over `HEAD`). The journal line of every
removal ends with the restore command.

Archive refs are removed only by `letibot gc --drop-archive OWNER|--older 90d`, which plans
first and never runs from the timer.

### d.9 What the operator sees

- **`letibot holds [OWNER]`** and one line in `--status`, in the session's status and in
  `leticode_status`:
  `holds: vm fccode3 (6.1G rss), 14 procs (2.3G), wt pk/r193 (+2 branches), target 38G, 1 container · idle 12m, closes in 18m`.
- **`letibot gc`**: the plan. Grouped by owner, one line per holding: what, size, verdict,
  when. Totals per class and per disk. A section "unrecorded" and a section "looks like
  ours, not provably" with the adopt command. A section "stuck".
- **`letibot gc --now [OWNER]`**: act. `--settle OWNER` skips the grace. `--adopt`,
  `--restore`, `--drop-archive`, `--unrecorded`, `--docker-report` as above.
- **`letibot hold OWNER [--for D]` / `letibot release OWNER`**.
- **The journal**: `letibot gc --log [OWNER]` prints `reap_event`. Each automatic pass that
  did something posts one line to the box's room: `gc: closed 3 idle packages, reaped 2 vms,
  archived 4 worktrees (refs/letibot/archive/…), freed 212G; 1 stuck - letibot gc`.
- **Before an idle close**: the owner's session gets one notice row 5 minutes ahead
  ("closing in 5 min for idleness; `letibot hold` keeps it"), and the bridge delivers it as
  a channel event. It informs. It asks nothing, and no answer is needed for the close.
- **Failures are loud once**: a `stuck` holding is a row in the room and stays at the top of
  every plan until the operator settles or adopts it.

### d.10 Failure modes

| Failure | What happens |
|---|---|
| Reaper dies half way | Every act is journaled and each holding's state is advanced only after absence is verified. The next pass reads `reaping` rows and repeats the step. All steps are idempotent: kill of an empty cgroup, `down` of a VM that is down, `update-ref` to the same sha, remove of a missing path. |
| Archive step fails (repo locked, disk full) | Holding goes `stuck` after two attempts. Its removal never runs. Compute and caches of the same owner still go. |
| Daemon crashed or was SIGKILLed | Its owners' compute is found by the timer pass: stale beat, no `harnessd` at the pid, so `closing`. Cgroups are killed by path. The VM by `held::reap`. `Drop` is not relied on anywhere. |
| Daemon wedged, still beating | The owner's idle and hard TTL still fire in the timer pass, because they are read from the store and the cgroup, not asked of the daemon. `RuntimeMaxSec` is the last bound. |
| Store cannot be opened | Fail closed for work and cache: nothing is removed. Compute bounds still hold through `RuntimeMaxSec` and, on the reaper's side, through `owner.json`: an owner root whose lease is past its hard TTL has its cgroup killed. Said in the room. |
| Two reapers | `flock` per box, compare-and-set per holding. |
| Pid reuse | Never signal a bare pid. Cgroup first; else `(pid, identity)` as the supervisor does; firecracker's `--id` rechecked (`held.rs:246-256`). |
| Remote host unreachable | Holdings there become `lost` (the word `process` already uses) and are retried. Meanwhile the host's own timer runs the same binary over `<root>/*/owner.json`: lease older than `idle_ttl + 1 h` kills that owner's processes and removes its caches; work is bundled, never deleted. Leases are compared as "seconds since last renewal by this host's clock", so skew between boxes does not matter. |
| Remote host has no letibot reaper installed | `letibot gc` on the home box says so per host ("no reaper on lubuntu2: holdings there are reaped only while it is reachable"), and host setup installs the timer. |
| The systemd user manager refuses scopes (R191) | Owners are created `unbounded` and say so. The sweep reports unit-table fill. It does not restart the manager. |
| The reaper would reap itself | Skipped by rule 6. A package cannot run `gc --now` on its ancestors. |
| An agent moves a tree out of its owner root | It cannot write outside it from a confined seat. In an allow-all host seat it can, and then the thing has no stamp and shows up under "looks like ours". This is the residue the design accepts, and the reason VM placement is the default for packages. |
| A reopened owner while its work grace runs | Reopen sets `live`, clears `closed_ms`, and the settle timers start again. A worktree already archived and removed is restored by `--restore` on reopen, automatically. |
| firecode changes its layout | The VM stamp is firecode's own files. The `owner` file and `gc --run` are firecode verbs with tests on both sides. |

### d.11 What this does not do

- It does not clean other projects' docker objects, VMs, branches or build directories.
- It does not delete a conversation, an archive ref or a journal row, ever, on its own.
- It does not replace the per-kind code that exists. `scope.rs`, `supervise.rs`,
  `held::reap`, `mergequeue/git.rs` are the hands. The new part is the owner, the stamps,
  the triggers and the one program that runs them.

## (e) Package split

Six packages, each one agent. G1 first; G2, G3, G4 and G5 can run in parallel after it; G6
after G1 and G2.

### G1 - Owners, holdings and the read-only plan

Scope: the `owner`, `holding` and `reap_event` tables (additive schema step) in the shared
store file; owner rows written at session open for every kind, before anything is created;
the owner root and `owner.json`; stamps for what letibot creates today (cgroup scope, VM
`owner` file through a firecode environment variable, daemon unit, booted copy, copy-out,
remote home); discovery walk; `letibot holds` and `letibot gc` as a plan that changes
nothing; the `holds:` line in `--status` and `leticode_status`.

Done when:
- a test opens a package session with a stub VM and two jobs and finds one owner row, and
  holdings for the scope, the VM and the daemon, each with a stamp that the discovery walk
  re-derives from the filesystem alone;
- a holding row deleted by hand is re-inserted by the next discovery pass;
- a directory with no `owner.json` beside an owner root is listed under "looks like ours"
  and has no verdict that reaps;
- `letibot gc` on a store with a closed owner prints a plan and the store and the disk are
  byte-identical afterwards;
- on lab2x1 the plan accounts for the VM copy-outs and booted copies under
  `~/.cache/letibot/firecode` by owner, with sizes.

Depends on: nothing.

### G2 - Compute reap: close, idle, TTL, and a daemon that lets go

Scope: the owner state machine through `closed`; close triggers (stop, archive, landed,
parent closed, idle TTL, hard TTL, daemon gone); lease renewal points; an attached bridge
head no longer holds a closed owner or a daemon; a package daemon exits when its last owner
closes; `vm_reap` judges a top-level session's VM by its `owner` row (closing R193); close
brings the VM down from the reaper, not from `Drop`; the 5 minute notice row; the
`letibot-gc.timer` unit and `letibot gc --now` for compute; `RuntimeMaxSec` and
`letibot.slice` in `scope_run`; `hold` and `release`. R194's tool-call bound is a
prerequisite row, not part of this package, but the hard TTL covers its absence.

Done when:
- a test closes a firecode-placed top-level session whose turn is stuck in a tool call, and
  within the bound finds no VM for it, no process in its cgroup, and a second session able
  to open on the same daemon (R193's own test);
- a package session left idle with a bridge head attached is `closed` after its idle TTL
  (a short one in the test), its notice row was written first, its daemon has exited, and
  `leticode_send` to it reopens it with its transcript intact;
- a daemon SIGKILLed with a job and a VM running leaves neither after one `letibot gc --now`
  run by a process that never talks to a daemon;
- `letibot hold` on that owner makes the same pass keep everything and say why;
- `systemd-run` properties in the launcher include the slice and the runtime bound, checked
  by a test that reads the unit back (skipped where there is no user manager, and saying so).

Depends on: G1.

### G3 - Work: owned worktrees and branches, archive, settle, restore

Scope: `leticode_start` gains `package: <name>` and creates the worktree in the owner root
and the branch `pk/<short id>/<name>` itself, row first; adoption of an existing worktree
path writes the stamp; branch proof from the worktree's HEAD log; the archive procedure of
d.8 including the scratch-index commit and the 512 MB guard; settle triggers and graces;
copy-out and `remote/<host>/<id>` / `x-vm-*` fetch branches moved into the archive
namespace; auto-archive of the settled session (R184's remainder); `--restore`,
`--settle`, `--drop-archive`; reuse of `mergequeue/git.rs`'s `commit_proof`,
`is_worktree_of` and `remove_worktree`.

Done when:
- a test settles an owner whose worktree has one unmerged commit, one modified tracked file
  and one untracked file: the worktree and branch are gone, `refs/letibot/archive/<owner>/`
  holds the commit and a `wip` commit containing both files, and `git gc --prune=now` then
  `--restore --worktree` brings back a tree with the same `git status`;
- the same with an ignored 1 GB file in the tree: the file is not in the object store;
- with a 600 MB untracked, not ignored file: the tree is in the trash directory, not
  committed, and the plan says so;
- a branch created in another worktree during the owner's life is not touched;
- an archive step made to fail (a read-only `.git`) leaves the worktree in place, the
  holding `stuck`, and one room line;
- a VM copy-out with a commit the source repo does not have ends with that commit reachable
  from the archive namespace and the copy-out directory gone;
- on lab2x1, `letibot gc --adopt 'wt-pk-*'` followed by the plan lists the existing package
  worktrees with a verdict each, and nothing is removed without `--now`.

Depends on: G1.

### G4 - Disks: caches, firecode records, remote hosts

Scope: `gc.root` per host and the launcher's exports (`CARGO_TARGET_DIR`, `TMPDIR`); cache
removal at close; the quota rule for shared targets under the existing build lock; disk
pressure trigger; firecode run retirement per owner (`firecode gc --run`, with the fallback
move) and per-owner state layers; the host-local reaper: `owner.json` on remote hosts, lease
renewal on the supervisor's per-host tick, the timer installed by host setup, lease-expiry
kill, cache removal and the bundle rule; `lost` holdings and their retry.

Done when:
- a package that builds leaves no directory under its owner root's `target/` and `tmp/`
  one pass after close, and its worktree (a `work` holding) is still there;
- a shared target over its quota with no lock held is removed, and one with the lock held is
  kept and said;
- a remote owner whose lease is not renewed (the home side stopped in the test) has its
  processes ended and its caches removed by the host's own pass, and a bundle exists holding
  its unfetched commit; the home box later fetches the bundle into the archive namespace and
  the bundle goes;
- a pass with the remote unreachable marks its holdings `lost`, removes nothing local that
  belongs to it, and succeeds on the next pass;
- after an owner with a VM settles, its run directory is no longer probed by `firecode in`
  (measured: lookup time with 500 retired runs is within 10% of the time with none).

Depends on: G1. Touches firecode (a small verb), so it carries a firecode row.

### G5 - Memory: bounds and who holds what

Scope: `memory.high` at the owner's cgroup and `memory.oom.group` at the job; the slice's
`MemoryHigh` setting; per-owner sums including the VM's run cgroup; `letibot holds --top`;
the OOM line in the session; `RLIMIT_CORE=0` on gate steps and jobs (R191); the unit-table
line and `reset-failed` for `letibot-*`.

Done when:
- a job that allocates past its cap is killed whole (no surviving child), its session gets
  one line naming the job and the cap, and the daemon and the session's other job survive;
- `letibot holds --top` on a box with two owners, one with a stub VM process in a run
  cgroup, attributes that process's memory to its owner;
- a gate step that runs `sh -c 'ulimit -f 1; dd if=/dev/zero of=x bs=1k count=8'` leaves no
  new entry in `coredumpctl list` (R191's own test);
- the plan prints the user manager's unit count, and a failed `letibot-*` unit is reset
  while a failed unit of another name is left.

Depends on: G1.

### G6 - Docker: no socket by default, a labelling proxy when granted

Scope: the seat's docker policy (VM: inside the guest; host: no socket unless granted); the
per-owner proxy socket with label injection on container, network, volume create and build,
list filtering, refusal of privileged and of binds outside the owner root, cgroup parent
where the engine allows; reaping by label at close (containers, networks) and settle
(volumes, built images); `--docker-report` and the opt-in prune policy for unowned objects.

Done when:
- in a seat with docker granted, `docker run -d`, `docker compose up -d` and a raw
  `curl --unix-socket "$DOCKER_HOST"` create each produce objects carrying
  `letibot.owner=<id>`; closing the owner removes the containers and networks and leaves a
  container created outside the proxy in the same test untouched;
- `docker ps` in the seat does not list that outside container, and `docker rm` of it is
  refused;
- a named volume survives close, is in the plan with its size, and goes at settle;
- in a seat without the grant, `docker ps` fails with the sentence that names the grant and
  the VM route;
- `--docker-report` prints unowned images and volumes by compose project and age and removes
  nothing; with the opt-in policy set in a test config, only objects matching the configured
  label are pruned.

Depends on: G1, G2.
