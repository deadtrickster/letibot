# Remote hosts: sessions and children that run on another machine

A remote host is a **placement**, as a firecode VM is (`crates/tools/src/firecode.rs`,
`Placement::Firecode`): the session's harness, model connection, store rows and gate stay in the local
daemon, and its **tool calls** - `bash`, `read`, `write`, `edit`, `grep`, `glob`, jobs - run on the remote
machine through an `ExecBackend` that speaks ssh where firecode's speaks `firecode in`.

* **No letibot on the remote**: it needs `sshd`, `bash`, `git`, coreutils and the work's own toolchain.
* **No keys leave this box**: the remote sees commands and files, never a model call.
* **Everything above the backend works unchanged**: `task_result`, activity, `wait`, the todo board,
  compaction, resume.
* What the machine's own toolchain is, is the point: a GPU box's models, a Linux box's cgroups.

## Hosts

`~/.config/letibot/hosts.toml`, the operator's file - never a project's, because a repository must not be
able to name a machine to run on:

```toml
[host.lab2x1]
hostname    = "lab2x1"                         # what ssh connects to; defaults to the table name
user        = "dead"                           # defaults to ssh's own
key         = "~/.config/letibot/keys/lab2x1"  # the default when the line is absent
path        = ["~/.cargo/bin"]                 # in front of the remote PATH, expanded THERE
root        = "~/letibot-remote"               # where remote worktrees live (default)
mise        = "~/.local/bin/mise"              # the toolchain wrapper; `false` turns it off
reserve     = 4                                # cores held back for the owner
flags       = ["heavy", "tmpfs"]               # what the machine is for
never_touch = ["etcd", "pgfuzz"]               # names letibot must never signal, stop or remove

[local]                                        # THIS box, and not a placement
never_touch = ["etcd", "pgfuzz"]
```

Nine keys, and an unknown one is **refused by name** rather than ignored: a typo places a session on a
machine nobody named, or points ssh at a key nobody meant. `reserve` is a whole number of cores and
anything else is refused rather than defaulted - a `reserve = "two"` read as zero is a scheduler that
believes a busy machine is empty. `flags` and `never_touch` use the `path` grammar (quoted strings on one
line) and are refused when half a list parses.

`path` is the one list: directories in front of `PATH` for every command the session runs there - its
calls, its jobs, and the `git-upload-pack` its branch comes back through. A login shell does not always have
the toolchain's directory. The entries are expanded by the **remote** shell, once, so `~` is that machine's
home.

`[local]` is not a `[host.…]` block: nothing connects to it and it is not a placement. It holds the one
list a process record on this box is checked against; absent is no declaration, not a default.

### The operator's own ssh material is never read

A rule about letibot's own reads, enforced in the command line and in two refusals:

* **`-F /dev/null` on every invocation**: the operator's ssh client config is not read at all - not its
  `Host` blocks, not its `ProxyCommand`s, not its `IdentityFile`s.
* **letibot's own known-hosts file** (`~/.config/letibot/known_hosts`), with `UserKnownHostsFile` at it and
  `GlobalKnownHostsFile=/dev/null`: a key accepted for letibot is not one accepted for their `git push`.
* **`IdentityAgent=none`, `IdentitiesOnly=yes`, one `-i`**: the agent holds the operator's keys and ssh
  would offer every one before the file it was given.
* **A dedicated key per host**, `~/.config/letibot/keys/<host>` by default. A `key` resolving inside the
  operator's ssh directory is refused by name even when they typed it, and the refusal prints the
  `ssh-keygen`/`ssh-copy-id` pair that makes the dedicated one.
* **A host key nobody has accepted is refused by name**, before anything is arranged on the other machine,
  with the one command that adds it:

      mkdir -p ~/.config/letibot && ssh-keyscan -t ed25519 lab2x1 >> ~/.config/letibot/known_hosts

  letibot never scans a key itself: one it trusted on its own is one nobody verified. A **hashed** entry
  (`ssh-keyscan -H`) cannot be recognised and reads as absent.

Every tool follows it: `read`, `write`, `edit`, `grep`, `glob` and the file side of every builtin **refuse a
path that resolves into that directory** - after every symlink, and for a file that does not exist yet -
regardless of mode, `allow-all` included, before any adjudicator is consulted. A `bash` command is **best
effort**: the gate refuses one whose words name an absolute path into it, but a path built at runtime
(`p="$HOME/.s""sh"`) is not something a matcher can see.

### What a connection costs

One TCP connection per host **per daemon**: `ControlMaster=auto`, `ControlPersist=120`, and a `ControlPath`
under letibot's runtime directory named by the host **and the daemon's pid** - `ssh-<host>-<pid>-call.sock`,
with `ssh-<host>-<pid>-git.sock` beside it for the `git fetch` this process does not start and so cannot
count (R99). A command costs a round trip rather than a handshake, no two daemons queue for the same ten
sessions, and git cannot be the ninth session on the call master.

`BatchMode=yes` and `StrictHostKeyChecking=yes`: a host that wants a password or a host-key confirmation is
**refused by name**, never prompted for, because a daemon has no one at a terminal. `ConnectTimeout=20`,
`ServerAliveInterval=5`, `ServerAliveCountMax=3`: a connection that is up is not a host that answers.

## Where it is asked for

| who | how |
|---|---|
| the operator | `leticode --host lab2x1` - a session whose tools run on lab2x1 (like `--vm`) |
| the operator, in a VM | `leticode --host lab2x1 --vm` - a guest booted by lab2x1's own `firecode` |
| a model | `task { where: "host:lab2x1", … }` - a child placed there (like `where: "firecode"`) |
| a model, in a VM | `task { where: "host:lab2x1:vm", … }` - a child in a guest on lab2x1 |
| Claude Code | `leticode_start { host: "lab2x1", vm: true, … }` - the bridge passes both flags |

`host:NAME` is also spelled `remote:NAME`, and `--where host:NAME` is the same act as `--host NAME` - one
rule, the name checked against `hosts.toml` in both places. A child of a remote session runs on the same
host unless it asks otherwise; a child of a session in a guest there gets a guest of its own.

## The tree: git, both ways

The remote works on a **checkout**, not a copy of the local folder:

1. On placement, the local HEAD (and its branch) goes over as a `git bundle` into `root/<repo>.git` - a bare
   mirror, created on first use - and is checked out as a worktree `root/<repo>/<short id>` on branch
   `remote/<short id>`. In a guest it is a **clone**: a copy of a linked worktree carries no repository.
2. The session works and commits there.
3. When it ends - or when asked - its branch comes back with `git fetch` as `remote/<host>/<short id>`. That
   branch is the result: the parent reviews it and merges what it wants.

Uncommitted local changes are **not** carried: the placement says so, with the list. A folder that is not a
git repository is refused. `.letibot-remote/` (the session's job directory) goes into the checkout's
`info/exclude`, so it never shows as a change.

The mirror is made by one placement and the bundle belongs to one (R101): the mirror step loops until
`<mirror>/objects` exists rather than testing the directory, and the bundle is
`incoming-<pid>-<counter>-<short>.bundle` - a shared name hands a placement that loses the race a checkout of
somebody else's commit, silently.

## Jobs and processes

A command is `ssh host 'cd <worktree> && bash -lc …'`. A background job runs remotely under `setsid`, with a
launcher, a pid file, an exit file and its output in `<worktree>/.letibot-remote/jobs/<id>.out`, so it
survives the connection and this Mac sleeping; `job_output` reads the file, `job_kill` kills the group, and a
run the daemon loses track of is found again from those files. `wait_job` polls the exit file every 120 ms,
the one cost of this shape.

* **The deadline is armed on the remote**, as a detached `sleep … kill` over the same connection: a local
  thread would die with this daemon while the process it guards does not.
* **A group kill only when the pid leads one**: `kill -TERM -<pid>` on a pid that is not a process-group
  leader signals *this shell's* group, so `ps -o pgid=` is asked first.
* **A command the model runs is wrapped** as `setsid bash -c 'echo $$ > <file>; ( <cmd> ); …'`, and the file
  is removed when it ends normally, so a stale id can never be signalled later.

`monitor` refuses by name on a remote host: there is no monitor mechanism there. `task_start` from a remote
session is refused too - it makes worktrees with the local `git` against a local path; the way to a remote
child is `task`. A command's stderr arrives merged into its stdout, because ssh carries one channel.

## Permissions

The remote host is not a boundary the way a VM is: it is the operator's machine with the operator's files. A
remote session gets the **same gate as a host session** - the modes, the always-ask list, the corpus - with
the remote host named in every card. A per-host `trust = "allow-all"` is not built.

A command does run inside `bwrap` **there**, when the host has it (RH3), with the view a local confined
session gets - the checkout read-write at its own path, the system roots and the toolchain read-only, and a
`$HOME` that is letibot's own (`<root>/.letibot-home/<short>`). `confine.rs` renders the same `Piece`s for
either medium (`Bwrap::wrap`, `Bwrap::remote_pieces`), so the binds are spelled once; the keep-list is the
**far** shell's, and `CARGO_HOME`/`RUSTUP_HOME` are set to the grants or a confined `cargo` finds no
toolchain. There is no `--new-session` on a remote spawn, because it would move the sandbox out of the group
`job_kill` signals. The probe is one round trip per host, cached - including a negative answer, but never a
probe that failed to answer. `docs/boundary-and-adjudication.md` has the view's contents.

`backend_confined` is **read, not asserted** (`in_vm || backend.boundary().is_some()`), so a placement that
confines nothing cannot sit at `allow-all`. A confined session's file tools see what a command sees
(`BackendError::Outside`) - the half the ssh-directory refusal cannot reach on a placed session, since that
rule resolves a path on *this* box. Still not confined: the placement's own ssh traffic, and a `--grant-ro`,
which is a path on this machine.

## A VM on that machine: `--host NAME --vm`

The two placements combine into one thing: a **firecode guest booted by that host's own `firecode`**,
reached over the session's existing ssh link, with the guest as the boundary. `leticode --host lab2x1 --vm`,
`leticode_start { host: "lab2x1", vm: true }` and `where: "host:lab2x1:vm"` are the three spellings; either
flag alone is what it always was.

1. **The host is asked what it has, once per host per process**: `command -v firecode` in a login shell and
   `/dev/kvm` readable and writable by this connection's user. A host with neither is refused by name before
   anything is arranged there; a probe that did not answer is not cached.
2. The checkout, mirror and bundle are arranged as a host placement's are, with one difference forced by the
   guest: a **clone**. The guest holds a copy of the directory, and a worktree's `.git` names a directory in
   the mirror that the copy does not carry, so nothing could commit there.
3. **`firecode up --project <checkout>`** on the host; the guest holds a copy at the same path, so a path
   means the same thing on both sides.
4. **Every call is `firecode in --project <checkout> --cwd <checkout> '<script>'`**: a command, a read, a
   write, a job's launcher, a status poll, the group kill. Nothing of the session's reaches the host's own
   shell, which is the point of the guest being the boundary.
5. **On close, `firecode down --project <checkout>`**, and the guest's work is taken onto the host checkout:
   `down` leaves the tree in a sibling `<checkout>-<stamp>` when something changed, that copy's branch is
   fetched into the checkout and fast-forwarded, and it comes home as `remote/<host>/<short id>`.

The guest is the boundary, so the session is at `allow-all` inside it (`Config`'s `in_vm`), the way a `--vm`
session here is: builds and tests run without a card, and there is no `bwrap` probe because there is nothing
for `bwrap` to bound. The model, the store and the gate stay here.

The copy-out is asynchronous, so the wait is a 30 s quiet window (`VM_COPY_OUT_QUIET`): a copy still on its
way appears as `<checkout>-<stamp>`, and the close's sentence gives the `git fetch` that takes it.

## A remote is an RPC

ssh is the transport, the ControlMaster is the connection pool, the remote shell is the server.

1. **A deadline on every call**, enforced by killing the local ssh and never by trusting the remote:
   `STEP_BOUND` 20 s for every call that is not a command (a model's `run` carries it too), `CANCEL_BOUND`
   10 s for the extra call that kills a group, `POLL_BOUND` 5 s for a job's status poll, `GIT_BOUND` 5 min
   for a streamed transfer, `MASTER_BOUND` 3 s for an `-O check`.
2. **Failure detection** by `ServerAliveInterval`/`ServerAliveCountMax`; a stale ControlMaster is checked and
   replaced (`ssh -O check`, then the socket is removed so the next call dials afresh).
3. **Retries follow idempotency.** `read`, `stat`, `list`, `list_tree`, `workdir`, a probe, `job_output` and
   `write` are asked **once more** on a fresh connection after a timeout or a drop. `run` and a job's start
   are **at-most-once**, because a call that dies in flight may have run, partly run, or still be running
   there. The one exception is a session the multiplexer **refused**, which never reached the shell: retried
   whatever the operation, `run` included, and then it runs exactly once.
4. **An unknown outcome is said as unknown**: *"it may have run, partly run, or still be running on HOST -
   check before running it again"*, never as a failure that implies it did not happen.
5. **Fail fast on a host that is down.** Three connection failures inside a minute open a breaker for thirty
   seconds (`rpc::BREAKER_THRESHOLD`, `BREAKER_WINDOW`, `BREAKER_COOLDOWN`) - three, because one failing step
   is already two attempts. The refusal says the count, when it opened, when the host was last reached and
   when the next probe is; after the cooldown **exactly one call** is let through as a probe, and one that
   answers closes it. A refusal from the far side never counts: `Permission denied` is a machine that is up.
6. **Cancellation reaches the far side**: an interrupt kills the local ssh *and* the remote process group. A
   call that stops answering makes one more bounded call (`CANCEL_BOUND`) that reads the group, asks `ps -o
   pgid=` whether that pid really leads one, and sends `TERM`, a second before `KILL`.
7. **Observability**: every call is recorded in a ring of 256 per process - host, step, duration, and one of
   `ok`, `refused`, `timed_out`, `dropped`, `unknown` - with the **step as a name and never a payload**,
   because a record is read by a head and printed to a log. `hosts {probe: true}` shows the last four calls
   and the three slowest per host; `$LETIBOT_RPC_DEBUG=1` prints one line per call to stderr. A job's status
   poll is recorded **only when it fails**.
8. **Back-pressure**: `MAX_SESSIONS_PER_HOST` is 8 ssh invocations at once per host per process, against
   OpenSSH's `MaxSessions` of 10 - the two left over are the master's own channel and one `-O check`. Ten
   exactly would have the *server* refuse the eleventh with `Session open refused`, which reads like a fault
   on the host. A call beyond the bound **waits for a slot under its own deadline**; `-O check`/`-O exit` take
   no slot.

`crates/tools/src/rpc.rs` is the one implementation of 5-8, its state `static` and keyed by host name,
because three private breakers would mean a host that is down costs each caller its own three timeouts. The
backend, the `hosts` tool's probe and `letibot --migrate` all call it.

**A refused mux session is transport, not the command's exit.** Four sentences are read out of ssh's stderr
and told apart from a lost connection: `mux_client_request_session: session request failed`,
`mm_send_fd: sendmsg(N): Message too long`, `channel N: open failed: administratively prohibited` and
`Control socket connect …: Connection refused`. All four mean nothing reached the remote shell, so the socket
is dropped and the call retried; the sentence a model reads is *this is a refused session, not your command's
exit status*. A refused session also counts as *the host is up* for the breaker.

**A write is one ssh call, and the far side checks it before the `mv`.** The bytes go on stdin (base64, so a
short stream cannot look complete) and one script does `mkdir` → `base64 -d > <temp>` → `wc -c` → `sha256sum`
(or `shasum`, or `openssl dgst`) → `mv -f`. The temp name belongs to that attempt alone
(`<path>.letibot-<pid>-<counter>`) and a trap removes it on any exit, so a stream cut by a deadline leaves
the target **byte-identical**. The bound scales with the payload: `STEP_BOUND` plus a second per 512 KiB.

## Toolchains through mise

`mise` lives at `~/.local/bin/mise` and is **not on a non-login shell's `PATH`**, the globally installed
tools differ per machine, and a project pins its own with `mise.toml` or `.tool-versions`. So every remote
command runs as

    <mise> exec -C <the checkout> -- bash -lc '<the command>'

when the host has mise - a foreground command, a step of the backend's own work, a background job's launcher,
a probe, and git's `git-upload-pack`, spliced in by hand (`--upload-pack '<mise> exec -C … -- git-upload-pack'`)
because sshd runs it with no login shell at all.

* **`-C` must be a directory that exists**: mise refuses one that does not (`mise ERROR Directory specified
  with --cd does not exist`, exit 1), so the wrapper points at the host's `root` - which the placement has
  already made - until the checkout exists. It is the checkout rather than `$HOME` because a project pins its
  versions in its own tree.
* **A `~` in it is expanded there**, as a `path` entry is; a quoted `~` is a path mise resolves against the
  wrong directory.
* Left out of `hosts.toml`, mise is **probed once per host per process** and the answer is cached **including
  "no mise"**, because a host without it must pay one round trip for that fact and not one per command. A
  probe that did not *answer* is not cached. `mise = false` turns the wrapper off; `path` is the escape
  hatch.
* `mise exec` **may install a pinned tool on its first use** - network traffic on the remote machine, and the
  one thing here that reaches the network.

`hosts {probe: true}` reports the mise version and `mise ls --current` in the host's root **instead of** *cargo
on PATH* - on a host with mise that question has no meaning - and falls back to `PATH` without it.

## The supervisor, and work across hosts

**Any process letibot starts is supervised**: a foreground `bash` call and a background job, an operator `!`
run, the merge gate's steps, a reviewer's or a subagent's commands, an LSP server, a firecode VM, a fanout
branch, a scenario role; local or remote. Each is a **record in the store** (`process`, schema 34): id,
owning session, host, process group, role, command, deadline, readiness check, state, and the RPC record of
its last check. The store is the truth, so `recover()` is `tick()`: `Harness::open` adopts this session's open
records and re-arms the watcher, and `jobs`, `job_output`, `job_kill` and `job_wait` work unchanged after a
restart.

* **Six states, and the last three are not interchangeable**: `starting | running | done | stopped | dead |
  lost`. `dead` is *gone with no exit recorded* and is deliberately not `done 0`; `lost` is *the host did not
  answer* and the job may still be running, which is why it is retried.
* **One check per host per tick, never one per job**: locally one pass over `/proc`, remotely one script over
  one connection reporting every job directory on that host.
* **A deadline is a wall clock, not a duration**, because the thread that armed it may not be the one that
  enforces it. `TERM`, a second, `KILL`, and the **group only when the record says the pid leads one**; a kill
  that could not be delivered leaves the record `lost`, so a teardown never reports work ended that it never
  reached.
* **Teardown follows ownership**: a session closing kills every open record it owns on every host, and the
  daemon's stop is every session closing. A **turn ending** is not a teardown - that is what `background:
  true` means - and neither is an interrupt.
* **A moved session does not take its records**: `store::transfer` does not carry `process`, because a record
  is a pid on ONE host. **A pid is never compared alone**: the identity is `start:<n>` here and `cmd:<the
  command>` on a host.

### `fanout` and `scenario`

Both are clients of the supervisor: a leg is spawned through that host's own process host, so the launcher,
the `setsid` group, the pid file, the exit file and the group kill are the same code. Each start is also
written to `process` through `fleet::ProcessBook`, because a branch is a job of a host the session is merely
reaching.

**`fanout`** - the same work on many hosts, separately (map), with the answer as the reduce:
`{hosts: [names] | {name: command}, command?, deadline_s?, cwd?, lease?, background?}`. One leg per host; the
list form takes the shared command, the object form a per-host one; `local` needs no ssh.

* Three fates that never collapse: `exited N · 1.2s · tail: …`, `stopped · the deadline passed`, `never ran
  — <why>`. A non-zero exit is a **row's** business, not the call's, so four hosts out of five still report;
  a leg with no result is a row saying it never ran, so an omitted cell is unspellable.
* `deadline_s` (default 600, ceiling 3600) is enforced twice on purpose: this loop kills a leg that outlived
  it, and the **record** carries the same instant so a daemon that outlives the call still ends it.
* **`background: true` returns ONE handle at once**: the branches start in the call, the waiting does not, and
  a thread of the fanout's own is the only writer of a row. `job_output` mid-run reads an append-only journal,
  and the **settlement carries the table**, cut at 24 lines. A restart loses the parent's settlement (the
  table is in memory) but not the branches.

**`scenario`** - work on several hosts together: `{roles: {name: {host, command, ready?, after?,
deadline_s?, ready_deadline_s?, cwd?}}, run?, deadline_s?}`.

* Three readiness checks, on the role's own host: `{port: N}` (a TCP connect there), `{log: "<regex>"}` over
  that role's own output, `{check: "<cmd>"}` as a short job whose exit 0 is the answer. Each has its own
  deadline (default 30 s), and a role that has already died is known not to be becoming ready.
* `after` is a dependency, not a sequence: a role starts when the roles it names are READY. Kahn's order, one
  role at a time, ties in the caller's order. A cycle, an `after` naming no role, and a role called `run` are
  refused **before anything starts**. `${role.host}` and `${role.port}` are substituted into a role's command
  and the run's; `port` exists only for a role declaring `ready: {port: N}`, and asking for one a `log` or
  `check` role does not have is refused rather than spliced as empty.
* The teardown is not optional and is one place: after the run, after a role that never came up, after a role
  that could not start, and on any early refusal. Every started role is stopped on its own host in reverse
  start order. An interrupt is the supervisor's: the roles are `process` records owned by the session.

## Leases on a shared host (RH9)

A shared host also runs the owner's work, and two sessions that both want port 5432 collide. A **lease** is a
block of a host's ports one session holds and nobody else can be given, the prefix its containers are named
under, and a teardown that removes exactly those.

```text
crates/tokencore/src/store/leases.rs   `host_lease` (schema 35), the row, and `pick`
crates/tools/src/lease.rs              the engine: the census, the block, the teardown
crates/harnessd/src/leases.rs          the daemon's half: StoreLeases, release_session, the reaper
```

* **A lease is a row**, `(35, Arm::Additive)` in `READER_ARMS`, so an older binary keeps serving a store a
  newer one migrated. **A released lease is not deleted**: *this session held 61000–61015 on lab2x1 for four
  minutes* is what a person reads when asking why a port was busy, and it is the only record that tells *held
  by a session that is gone* from *never held*.
* **`BEGIN IMMEDIATE` is the per-host lock, and the only one that spans processes** - two daemons share one
  `sessions.db`, so a mutex in a tool would serialise half the callers. The census is taken *before* the
  transaction, because a probe over ssh inside the write lock would hold every other lease.
* **The block starts above the host's own ephemeral range**, read there
  (`/proc/sys/net/ipv4/ip_local_port_range`, or the `sysctl` pair on a Mac): a port inside it is one the
  kernel hands to somebody else's outgoing connection. A host that will not say its range is a refusal.
* **A port somebody is listening on is skipped, whole**: one read-only round trip runs `ss -ltnH` (or
  `netstat -ltn`). A run that touches a busy port is stepped over rather than trimmed, because a block is
  contiguous and a service that takes four wants `first + 3`.
* **The prefix carries the lease's number** - `letibot-<host>-<session>-<id>`, or `letibot-<session>-<id>`
  here - so two blocks on one host from one session cannot share one.
* **What a lease is not**: a kernel cannot be asked to keep a port free. The block guarantees that no other
  session of letibot's took it and that nothing was listening when it was taken.

The tools take from it, and never pick: **`scenario`'s `ready: {"port": "lease"}`** is a port from the
session's block on the role's own host, assigned before anything is started, one allocation per host, so
every `${role.port}` resolves to a number the block owns (an explicit `{port: 5432}` still works); and
**`fanout`'s `lease: true | N`** is a block per branch on that branch's host, travelling to the command as
`LETIBOT_LEASE_PORT_FIRST`, `…_PORT_LAST`, `…_PORTS` and `…_PREFIX`, because a fanout's branches are all
planned before any block exists - a branch that could not get one is a row saying so
(`LETIBOT_LEASE_REFUSED`), not a failure of the call.

**Exact teardown.** `Blocks::release` is one act, called from four places - a scenario's end, a fanout's end,
a session's close, and the reaper:

1. **What is there** - one `docker ps -a --format '{{.Names}}'` and one `docker compose ls`. A host with no
   `docker` is a sentence, not a failure.
2. **What is mine** - names that **start with** the prefix, and the compose project that **is** it.
   `starts_with` and not `contains`, because two sessions' prefixes share a head.
3. **What is not** - every name about to be removed is checked against the host's `never_touch` first, and a
   name that matches **is left alone and the refusal names the pattern**. The prefix says *what is mine*; the
   operator's list says *what is not*; the list wins.
4. **What is gone** - `docker rm -f` for the containers, `docker compose -p <prefix> down` for the project,
   every argument single-quoted.
5. **And only then the row goes back**: a release first would leave a lease nobody holds whose containers are
   still up.

**`never_touch` is a case-insensitive SUBSTRING**, and that is the safe direction rather than the lazy one:
the operator declared the workload (`pgfuzz`, `etcd`) and not its spelling, and a rule that missed
`etcd-fuzz-0` because the line said `etcd` would fail silently - the thing is stopped and nobody is told. A
rule that is too broad refuses a teardown the session could have had, and says which pattern refused it.
`remote::never_touch_hit` is the one copy of the rule, read by three lists: a host's, this box's, and the
lease teardown's.

**The supervisor asks it before every signal.** `Supervisor::may_touch` is the door the deadline kill and
`teardown_session` both go through: a matching record is not signalled, stays **`lost`** (nothing was ended,
so nothing may say `stopped`) with `check_how` `refused`. A record with no host is checked against this box's
`[local]` list.

**Two moments a lease goes back, and one that is not one**: a session closing, after its processes are ended;
and the owner being **seen** gone, which is the supervisor's tick and the half a hard-killed daemon leaves
behind. **A restart is not an owner being gone**, and the difference is `leases::GRACE_MS` (60 s): a daemon
that has just come up holds no session for its first second, so a reaper that asked *is this session in the
registry* would release every block on the box at each restart.

## The merge gate on a declared host

```toml
[queue]
gate_host = "lab2x1"      # a [host.…] from ~/.config/letibot/hosts.toml; empty is this machine
```

or `--gate-host lab2x1` on the daemon's command line, which wins over the file, and `--gate-host local` puts
it back here for one invocation. **Empty is the default and is the local gate byte for byte**: `LocalGate`
with nothing set is `run_with_memory`, in this machine's own cgroup, under `[memory] gate`. The value is read
once at daemon start and handed to the queue through `mergequeue::gate::set_gate_host`, because the queue
builds **one** runner for the whole daemon - so `LocalGate::run` is the dispatch: nothing set is the local
run, a name is `HostGate`. The row's green line reads `` `main` is green at 0f2d9550 (lab2x1 gate) ``. A
daemon seated above several repositories reads the workspace's own `leticode.toml`, as it does for
`[queue] reviewers`.

**The merge stays here.** The host runs `sh -c <step>` in a copy of the landing worktree; the target branch
moves by a compare-and-swap on this box's `refs/heads/<target>`, and nothing in the host path runs a `git`
that writes anywhere but its own copy.

Two transports, and the second is not optional:

* **`rsync -a --delete`**, over the host's declared key and root, into `<host root>/gate/<repo>-<hex>/tree`. A
  landing worktree is not only its commit: `[queue] gate_carry` copies gitignored inputs into it (`images/`,
  `vendor/` - a firecode VM's kernel and rootfs), and those are in no commit. `--delete` so a file the branch
  removed is gone there too; the repository's own directory and `target` are excluded.
* **A `git` bundle**, because rsync cannot carry a repository: a landing worktree's metadata is a file
  pointing at `<repo>/.git/worktrees/<name>`, which means nothing on another machine - and the gate's first
  step is `sh scripts/check-fmt.sh main`, which runs `git diff main HEAD`. So the tip and the target branch go
  over as a bundle and the rsync'd tree lands on top. It is incremental and its own refs (`refs/gate`,
  `refs/heads`) are the record, and what to fetch is read back with `git bundle list-heads` rather than
  assumed: a thin bundle whose prerequisite already covers a branch **drops that ref**.

**`CARGO_TARGET_DIR` is `<gate dir>/target`** - outside the rsync'd tree and outside the worktree, so neither
`--delete` nor `git clean -fd` can take it. A cold gate on a host is worse than a local one, which is why the
directory is per-repository and permanent. **The gate says where the time went, on its first row**:

```text
[gate on lab2x1: the repository 1.28 s, the checkout 0.15 s, the tree 0.14 s — 1.42 s of transport
 out of the gate's 30 min; build cache COLD — this gate made it, so the first step below pays for
 the whole dependency tree; 209.7 GiB free on that filesystem]
```

So what a cold host gate costs is COMPILATION; `COLD` is asked of the host *before* the gate's own `mkdir`
touches `target/`, so it says whether THIS gate makes the cache, and the transport is a couple of seconds.

**Two clocks, and they are not the same number.** The step's is `[queue]`'s `gate_deadline`, thirty minutes
unless set, and it is the only thing that may end a step. One poll's is `POLL_BOUND`, thirty seconds: it asks
*can this machine fork a shell and answer*, and on a box whose own build holds every core the answer can take
many seconds. So **a poll that is not answered is asked again, not a verdict**: the question is idempotent
(`tail -c +<offset>`), a miss says *the machine was busy*, and only the step's own deadline - or a poll that
reaches no machine at all - may end a step.

**A gate that could not run is not a verdict about the code.** `GateVerdict::Infra` - a host that did not
answer, a key that is not installed, a tree that would not arrive - writes the target back to **`gating`**
with the sentence that stopped it, and the tip is gated again after a minute (`UNJUDGED_RETRY_MS`). It is
*not* `blocked`: `blocked` is for a tip nobody can judge at all (no declared `Merge gate`, a landing tree that
cannot be made) and it stops both queues. A branch whose own landing gate could not run goes back in line the
same way, so a host that is down costs the queue a minute per attempt rather than a `/queue restart`. The one
`Infra` that stays `blocked` is a step killed at the gate's memory cap, because that step RAN.

Each step is started **detached, under `setsid`, with a pid file**, and polled once a second: one round trip
answers both *how is it* and *what has it written since*, so the log arrives at the same local path the local
gate writes (`$TMPDIR/letibot-gate/…`). At the deadline, on the daemon's stop, or when the lease goes, the far
side's whole process group gets `SIGTERM`, five seconds, then `SIGKILL`; a far-side `timeout` is deliberately
NOT used, because its exit status would be one more number to tell apart from the step's own.

**A shell reports *my child was killed by signal n* as `128 + n`**, and a `137` read as a code would make a
step killed by `SIGKILL` a RED branch instead of an unjudged one, so `exit_of` decodes 129-192 as signals.
And **the row says its memory was not measured** - `[gate memory: not measured — the step ran on lab2x1,
where this daemon has no cgroup of its own]` - which the model's copy drops like any other quiet line.

### The demand a step makes of the machine

`LETIBOT_REQUIRE_APPARATUS=1` is **the project's** demand and not the gate's: this repository's `AGENTS.md`
writes it on step 3 so a test that skips cannot read as a pass. The gate's job is to run the step as the
project wrote it - but *which machine* runs it is the gate's, and a demand a machine cannot meet has to be
**met, dropped or said**, never turned into a verdict about a branch.

A gate step runs in the ssh session's cgroup, which the user may not write (`writable=no`), so
`Cgroup2::probe`'s `mkdir` fails - while `systemd-run --user --scope -p Delegate=yes` answers 0 there. The
machine was not missing a delegation: **the step was not in a scope of its own.**

One call before the steps, and only when a step carries the prefix: an EXPERIMENT, not an inspection - it
makes a directory in its own cgroup and removes it, then tries the same inside the delegated scope.

| the host answers | what the gate does |
|---|---|
| `cgroup=here` (and `bwrap=yes`) | nothing - the steps run byte for byte as the project wrote them |
| `cgroup=own-scope` | starts each step that carries the demand inside a transient delegated scope, and **keeps the demand** |
| anything missing | takes `LETIBOT_REQUIRE_APPARATUS=1` off **that one step** and says so in the row |

The wrap is `( exec systemd-run --user --scope -p Delegate=yes --quiet -- bash -c '<step>' )` inside the
launcher's subshell: `systemd-run --scope` execs the command in place and answers with its exit status, so the
pid file, the group kill and `$?` mean what they meant before. A probe that cannot answer changes **nothing**,
because reading a failed probe as *absent* would take a demand off a machine that has everything. The row says
which happened, in the gate's own words, so no classifier reads it (`setup::step_words` drops it):

```text
[gate on `lab2x1`: `LETIBOT_REQUIRE_APPARATUS=1` was TAKEN OFF 1 of 5 step(s) — this machine
 cannot meet it (a cgroup v2 subtree this user may write (the process-lifetime tree)), so the
 tests that need it SKIPPED here and THIS IS NOT A PASS for them; …]
```

**The net under it: a refusal is not a verdict.** Whatever the probe said, a step that failed and whose
**every panic is a `LETIBOT_REQUIRE_APPARATUS` refusal** is `RedCause::ApparatusAbsent`: the row is `not_run`
and the verdict is `GateVerdict::Infra`, so the tip goes back to `gating` and is gated again in a minute, never
`red`. The rule is a count and not a pairing, because the refusal is on the line AFTER `panicked at` and
`-- --nocapture` does not promise to keep them adjacent: one genuine failure among sixteen refusals leaves a
panic with no refusal behind it, and the step stays red.

**On a machine where that call fails**, give it systemd's own delegation, then log out and in so the user
manager restarts:

```sh
sudo install -d /etc/systemd/system/user@.service.d
printf '[Service]\nDelegate=yes\n' | sudo tee /etc/systemd/system/user@.service.d/delegate.conf
sudo systemctl daemon-reload   # then log out and in, so the user manager is restarted
```

`cgroup.subtree_control` by hand is not an option: it cannot be written on a cgroup that holds processes of
its own, which a login session's scope does.

**Trying it by hand**, with no store, no lease and no queue:

```sh
cargo run --offline -p letibot-harnessd --example gate_on_host -- \
    lab2x1 ~/.local/share/letibot/landing/letibot-0123abcd
```

It runs that tree's own `## Merge gate` on that host and prints one line per step with its outcome and
milliseconds; a trailing list of commands replaces the steps.

**What this does not do.** The host's `[memory] gate` is not applied: a step there is bounded by the host's own
memory and by nothing of ours, because the cgroup it would write is the host's (a step that demands the
apparatus *is* started in a delegated scope, but that scope exists for the apparatus and no cap is put on it).
`gate_carry` is re-sent on every run - rsync is incremental, so an unchanged file costs a stat, but a project
carrying a gigabyte of images pays the check each time. And **a gate host is a daemon-wide setting**: a daemon
serving repositories that want different machines needs two daemons.

## Migrating a session: `letibot --migrate` (RH4)

A host placement moves a session's **tools** and leaves the daemon here; `--migrate` moves the daemon, and
`local` is an endpoint like any other:

```text
letibot --migrate SESSION --to lab2x1     laptop  -> lab2x1
letibot --migrate SESSION --to local      lab2x1  -> laptop
letibot --migrate SESSION --to lubuntu1   lab2x1  -> lubuntu1   (relayed through here)
```

Four things move: **the session** (the store's rows through `store::transfer`, carried and read in with the
same hash-chain check a resume makes), **the tree** (a `git bundle` of the source HEAD, cloned on the far side
onto `remote/<short id>`; nothing uncommitted is carried), **the daemon** (`harnessd --session ID` on the
destination, built there from a bundle of this tree; `--build-timeout SECS` for a slow machine), and **the
socket** (`ssh -N -L <local>:<remote>`, recorded beside the launcher's own daemon records so a head finds it
where it finds any daemon).

Exactly one copy is live, and the marker says which: a move marks the source `session.lives_on` rather than
deleting it, so `--sessions` says *lives on lab2x1 since …*. The local store is the index of record, which is
what makes `lab2x1 -> lubuntu1` possible at all. Step one is quiesce: attach to the session's daemon and wait
until its activity is quiet. A session stopped on a **card** is refused rather than waited out, because the
only thing that would clear it is a person. Then the source is shut - a **host's** daemon serves exactly this
session and is stopped, a **local** one serves the operator's other conversations and only this session is
closed - and host to host is relayed through this box so both ends get the same verification.

## What is not built

* **No per-host trust.** Every card is the session's own, and a fanout's card names the hosts because its
  arguments do.
* **No matrix runner.** `fanout` is the shape capacity placement, interleaved repetitions, resume from a plan
  file and a merged result set would grow from. `hosts {probe}` reports the spare cores (`cores − load −
  reserve`) they would place by; nothing places by them yet.
* **No subnet lease.** A docker network's address pool is not something this build reads or reserves.
* **No teardown for anything but docker**, and **no `remote_sync` verb**: the branch comes back when the
  session ends (`ExecBackend::close`).
* **`MaxSessions` is not raised, and letibot does not read it.** The client bound is 8, safe for the default of
  10; an `sshd` below that still refuses, and the refusal is a retried transport failure. A per-daemon master
  means each short-lived process dials its own connection: at 50-70 ms each and with `ControlPersist=120`, that
  is the price this shape chose over a cross-process pool.
