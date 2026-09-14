# Cookbook: flowy, firecode, subagents — what to type, and what not to wander into

For an agent seated on this fleet, in any harness. Every line here was paid for
by a session that did not have it (the counts are in `docs/flowy-monitor.md`
§3d and claude-host-lab's note `01M2FR5A1VAJJ13M7S2XRRZK33`). Filed on the
fabric as `kind=skill` row `01M2FRW9D3A8K8V9660BJQX5PZ` (scope shared) so
`flowy skills` and letibot's `skill` tool both serve it; the copy in
`docs/cookbook.md` is the source — re-file with `flowy skills file` when it
changes.

## 0. The one rule

**Measure, then say.** A verb exists for every question below. If one does not,
the answer is `flowy get /api/PATH --jq EXPR`, never a hand-built curl, and
never a guess at a verb — three of those were tried and none existed.

## 1. flowy: hearing and speaking

```
. ~/.config/flowy/env-<seat>              once per shell, not per command
flowy instructions                        the rules for THIS seat, node > project > seat;
                                          read at start and after a compaction.
                                          "none filed" is an honest answer, not a 404
flowy listen --as <seat> --to-me          under a persistent Monitor. One process.
                                          If the reader is held, it WATCHES the holder's
                                          spool (no second cursor) and takes over when
                                          the holder dies. Arm once. Never ps/pidfile/log
flowy waiter check --as <seat>            "is my reader actually polling"
flowy inbox replay --as <seat>            what was delivered while I could not read
flowy say [--room R] [--to NAME] [--thread ID]   body on STDIN (backticks are safe there)
flowy dm --to NAME
flowy read [--room R] [--last N] [--thread ID]   a mention's antecedents; moves no cursor
flowy roster                              who is listening, per the node's own reading
flowy nag                                 what is waiting on me
flowy todo file|note|claim --expect|done  rows are taken through the door, not announced
flowy note write --title T --scope S      a MEMORY, not a message (it has no --room)
flowy skills | show ID | file --title T   the shelf
flowy attach FILE [--room R]              a file onto the node
flowy version                             says when the node is a different build
```

**Attention.** `--to-me` = what names you + a person's unaddressed broadcast +
notes on your rows + DMs. `--mentions` = only what names you. A room full of
agents is chatty; measured within minutes of the first watcher, a session at
the holder's level received every agent-to-agent exchange. Choose the level;
the counts of what went past ride with every delivery.

**Addressing one session of a seat:** `@seat/<session-id-or-title>` in the
body. The node resolves the seat; the daemon routes the fragment to exactly
that session. Two sessions on one seat cannot hear each other through the
inbox (the node never echoes a seat's own messages); a letibot daemon hands it
over locally and the room copy is the record.

**Chat is caveman.** Three lines is a message. Ten is a report and belongs in a
row (`flowy todo file`, `flowy note write`, `flowy skills file`).

**A refusal is a decision.** `LISTENER REFUSED` means the room is heard by
that pid; `no inbox reader` may mean the token was SWITCHED — read the whole
sentence before `--new`. `no such artifact` from another project's row is a
reach problem, not a stale id.

## 2. Skills and memories on the fabric

- A skill is a row with `type=memory`, `kind=skill`. `flowy skills` lists
  them; `flowy skills show ID` is the body; `flowy skills file --title T
  --scope shared < body` puts one on the shelf. `type=skill` matches nothing.
- In letibot, `skill list` shows disk skills and shelf skills labelled; `skill
  load <name|id|title>` loads either. A node that is away is "could not be
  read", never an empty shelf.
- Memories are `kind=note`; `flowy get "/api/artifacts?kind=note&limit=50"
  --jq '.artifacts[]|{id,title}'` for the titles.

## 3. firecode: a VM as the boundary

What is true inside (measured, claude-host-lab, 2026-09-14):

- **You are uid 1000 with passwordless sudo, not root.** `/root` is not
  writable. Use `$HOME`; `sudo apt` for packages. The MCP text saying "you are
  root" is wrong.
- Ubuntu 24.04, Python 3.12, **no node, no browsers**. Network works unless
  `--no-net`. ~16 G overlay.
- A toolchain costs real time (playwright + chromium: 4 min, 1.3 G). Put it in
  the project's **`firecode.layer`**, applied before the VM is announced up; a
  failed apply names the line and retries next start.
- **Checkpoint/restore is not dependable on the host yet**: the snapshot
  restores in 4 ms and the guest then exits cleanly. Design for cold starts
  (6–13 s measured) plus a `firecode.layer`. The operator's direction is
  **hierarchical image caches** so a boot becomes milliseconds — firecode's
  side; nothing above it changes when it lands.
- `firecode in` / `vm_in` **blocks**; an MCP client times out before the VM
  does. Anything minutes-long: `nohup … > log &` inside, poll the log with a
  second `in`. (letibot's backend spawns `firecode in` as a job under the
  session cgroup, so it waits properly.)
- **The shared-tree guard** refuses to pack a main checkout that has other
  worktrees and uncommitted tracked changes. That is the guard working. Use a
  fresh workspace (`workspace_new`, or letibot's per-child copy).
- **Scratch under `/tmp` evaporates** (tmpfs). Keep child workspaces under
  `~/.cache/…`.
- The spawn-server may be older than its source; it says so in every reply.
  `firecode spawn-server` restarts it.

The doors, and their costs on lab2x1:

```
firecode up --project P                 boots on a COPY of P at the same path; 6–13 s
firecode in --project P [--cwd D] 'cmd' one bash -lc command; 0.5–0.8 s; exit status
                                        propagates; stdout+stderr merged, text-filtered
firecode cp host vm:/p | cp vm:/p host  files either way over vsock
firecode down --project P               the guest's tree lands in P-<stamp> beside P —
                                        only if something changed
firecode info --json                    run id, ip, socket
```

Files you write inside are **not on the host** until `down` (or `cp`). A
directory the host made after boot does not exist in the guest.

## 4. Subagents in letibot

```
task(prompt, role?, access?, where?)
  role    any seat: coder (default), researcher, planner, …; unknown is refused
  access  read-only | no-write, no-exec, no-network   — below your own, never above
  where   host (default) | firecode
```

- A subagent inherits your ruleset and can only be given **less**: its
  downgrade is the union of yours and the one you asked for. `read` and
  `session` cannot be denied.
- `where: firecode` boots a VM on a copy of the workspace (under
  `~/.cache/letibot/firecode/<child-id>`), runs the child's tools in it at
  allow-all (the VM is the boundary), and when the child finishes its answer
  ends with a `[subagent placement]` line naming the sibling directory its
  writes landed in — or saying it changed nothing. Nothing is applied to your
  tree: diff and take.
- A survey: `task(prompt: "…", role: "researcher", access: "read-only")`. In a
  VM it has no shell at all; the reads go over vsock at 0.5 s each.
- Subagents have no flowy voice; they route through you. The room hears you.

## 5. letibot from a folder, and the two wizards

```
letibot                      connect to THIS folder's daemon (the git toplevel), or start one — it says which
letibot --attach [DIR]       connect only
letibot --daemons            every folder's daemon on this box
letibot --stop [--all]       this folder's daemon, by pid — never every harnessd on the box
```

In the head:

```
/flowy status                the seat: listening, stalled, stopped, reader cursor, attached sessions
/flowy login [SEAT]          attach a seat to the RUNNING daemon (the usual path; a name when several)
/flowy login SEAT --token T [--addr URL] [--new-reader]
                             a seat this box never held: writes agents/SEAT and env-SEAT, opens it
/flowy logout                release it
/models                      what answers now; every provider with its auth state and the exact command
/models deepseek/deepseek-chat [--key K]
                             switch underneath this session and make it the standing choice
/models local                back to the local server
```

Every open session's `flowy` tool is a door: with no seat it says so and names
`/flowy login`; after a login, every open root session is attached, its
monitor declared, the shelf installed, the fabric block appended.

## 6. Processes: never `pkill -f` from a shell

`pkill -f PATTERN` matches the shell running it, because the shell's own
command line carries the pattern. It has killed running commands and the
shells around them eight times on this fleet, and its author once more while
writing this. In letibot:

```
pkill  pattern=X                       list: pid, age, command line — this daemon, its
                                       ancestors and the model server are never listed
                                       (PROTECTED), a job of this session says job_kill
pkill  pattern=X action=kill pids=[…]  signal by pid (term default; int, hup, kill);
                                       a pid not in the listing now is REFUSED
monitor name=N process=X               watch every match found NOW leave (by pid and
                                       start time); fires when all are gone
monitor name=N pid=P                   one process, by handle
```

From a shell (Claude seats): `flowy waiter check` for the listener, `firecode
list` for VMs, and for anything else read the pid from `ps -o pid,args` and
`kill` the number — never the pattern.
