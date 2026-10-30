# Remote hosts: sessions and children that run on another machine

Written 2026-10-26. The operator: *"I want you and letibot itself to be able to run childs on remote
machines"*, and *"access is going to be simple for time being - hostname, user, passwordless ssh
key"*.

## The shape: the model stays here, the tools run there

A remote host is a **placement**, exactly as a firecode VM is (`crates/tools/src/firecode.rs`,
`Placement::Firecode`): the session's harness, its model connection, its store rows and its gate stay
in the local daemon, and its **tool calls** — `bash`, `read`, `write`, `edit`, `grep`, `glob`, jobs —
execute on the remote machine through an `ExecBackend` that speaks ssh where firecode's speaks
`firecode in`. Consequences, each one a reason for the choice:

* **No letibot on the remote.** It needs `sshd`, `bash`, `git`, coreutils (`base64`, `mv`, `mkdir`)
  and whatever the work needs (cargo, go, …). Nothing is installed or built there by us.
* **No keys leave this box.** Provider keys, the session store and the permission corpus stay local;
  the remote sees commands and files, never a model call.
* **Every existing piece works unchanged**: `task_result`, activity (D1), the bridge's `wait`, the
  todo board, compaction, resume — they are all above the backend.
* What the remote machine's own toolchain is, is the point: lab2x1's GPU and models, a Linux box's
  bwrap and cgroups, a bench box's databases.

## Hosts

`~/.config/letibot/hosts.toml`, the operator's file (never a project's — a repository must not be able
to name a machine to run on):

```toml
[host.lab2x1]
hostname = "lab2x1"                          # what ssh connects to
user     = "dead"
key      = "~/.config/letibot/keys/lab2x1"  # the default when the line is absent
path     = ["~/.cargo/bin"]                 # in front of the remote PATH, expanded THERE
root     = "~/letibot-remote"               # where remote worktrees live (default)
mise     = "~/.local/bin/mise"              # the toolchain wrapper; `false` turns it off
```

Six keys, and an unknown one is **refused by name** rather than ignored: a typo here places a
session on a machine nobody named, or points ssh at a key nobody meant. `hostname` and `user` are
the two that must be said; `root` defaults to `~/letibot-remote`, `key` to
`~/.config/letibot/keys/<host>`, and `mise` to *find it once and cache the answer* ([§ Toolchains
through mise](#toolchains-through-mise)).

`path` is the one list: directories that go **in front of `PATH`** for every command the session
runs on that machine — its foreground calls, its background jobs, and the `git-upload-pack` its
branch comes back through. A login shell on a host does not always have the toolchain's directory
on `PATH` (measured on lab2x1, 2026-10-26: `cargo` lives in `~/.cargo/bin` there and no profile adds
it), and a remote session that cannot find `cargo` is a session that can do nothing. The entries
are expanded by the **remote** shell, once, so `~` is that machine's home and not this one's.

The file has a second kind of section, and it is the **machine the daemon itself runs on**:

```toml
[local]
never_touch = ["etcd", "pgfuzz"]     # what letibot must never signal, stop or remove HERE
```

`[local]` is not a `[host.…]` block — nothing connects to it, and it is not a placement — and it is
checked where a `process` record names no host (§ *As built (FB)*). `[host.NAME]` also gains
`reserve`, `flags` and `never_touch` in the remote-host slices; all of them are lists of quoted
strings on one line, refused by name when they do not parse.

### The operator's own ssh material is never read

The operator, 2026-10-26: *"letibot is forbidden to touch and even read `~/.ssh`"*. That is a rule
about letibot's own reads, and it is enforced in the command line and in two refusals rather than
asked for in a comment:

* **`-F /dev/null` on every invocation.** The operator's ssh client config is not read at all — not
  its `Host` blocks, not its `ProxyCommand`s, not its `IdentityFile`s.
* **letibot's own known-hosts file** (`~/.config/letibot/known_hosts`), with
  `UserKnownHostsFile` pointing at it and `GlobalKnownHostsFile=/dev/null`. A host key accepted for
  letibot is not a host key accepted for the operator's own `git push`.
* **`IdentityAgent=none`, `IdentitiesOnly=yes` and one `-i`.** The agent holds the operator's own
  keys and ssh would offer every one of them, in ssh's own order, before the file it was given.
* **A dedicated identity file per host**, `~/.config/letibot/keys/<host>` — the default when
  `hosts.toml` says nothing. A `key` that resolves inside the operator's ssh directory is **refused
  by name**, even when the operator typed that path themselves, and the refusal prints the
  `ssh-keygen`/`ssh-copy-id` pair that makes the dedicated one.
* **A host key nobody has accepted is refused by name**, before anything is arranged on the other
  machine, with the one command that adds it:

      mkdir -p ~/.config/letibot && ssh-keyscan -t ed25519 lab2x1 >> ~/.config/letibot/known_hosts

  Read the key it prints before you keep it. letibot does not run `ssh-keyscan` itself: a key it
  scanned and trusted on its own is a key nobody verified, and `StrictHostKeyChecking=yes` against a
  file letibot filled silently would be strict about nothing. A **hashed** entry (`ssh-keyscan -H`)
  cannot be recognised by letibot and reads as absent; the way out is the same scan without `-H`.

And the same rule for every tool: `read`, `write`, `edit`, `grep`, `glob` and the file side of every
builtin **refuse a path that resolves into that directory** — after every symlink, and for a file
that does not exist yet — regardless of mode, `allow-all` included, before any adjudicator is
consulted. A link in the workspace named `keys` and pointing there is that directory. A `bash`
command is **best effort**: the gate refuses a command whose words name an absolute path into it, but
a path built at runtime (`p="$HOME/.s""sh"`) is not something a matcher can see, and no design here
pretends otherwise. The file tools are the hard line because they hand over a path rather than a
sentence.

### What a connection costs

One TCP connection per host **per daemon**: `ControlMaster=auto` and a `ControlPath` under
letibot's own runtime directory (`$XDG_RUNTIME_DIR/letibot`, or the box's temp directory when there
is none) named by the host **and the daemon's pid**, so a command costs a round trip rather than a
handshake and no two daemons queue for the same ten sessions (R99, § *As built (R99)*).
`BatchMode=yes` and `StrictHostKeyChecking=yes`: a host that asks for a password or a host-key
confirmation is **refused by name**, never prompted for — a daemon has no one at a terminal, and a
prompt it cannot answer is a call that hangs until its deadline.

## Where it is asked for

| who | how |
|---|---|
| the operator | `leticode --host lab2x1` — a session whose tools run on lab2x1 (like `--vm`) |
| a model | `task { where: "host:lab2x1", … }` — a child placed there (like `where: "firecode"`) |
| Claude Code | `leticode_start { host: "lab2x1", … }` — the bridge passes `--host` |

A child of a remote session runs on the same host unless it asks for another placement (the rule a
VM's children already follow: *"children live in the same vm"*).

## The tree: git, both ways

The remote works on a **checkout**, not a copy of the local folder:

1. On placement, the local repository's HEAD (and the branch it is on) is sent with `git bundle` over
   ssh into `root/<repo>.git` (a bare mirror, created on first use) and checked out as a worktree
   `root/<repo>/<session-short-id>` on a branch `remote/<session-short-id>`.
2. The session works and commits there.
3. When it ends — or when asked — its branch comes back with `git fetch` over ssh into the local
   repository as `remote/<host>/<session-short-id>`. That branch is the result: the parent reviews it
   and merges what it wants, as with any worktree package.

Uncommitted local changes are **not** carried: the placement says so, with the list, and the operator
or parent commits first if they want them there. A folder that is not a git repository is refused for
remote placement in this slice.

## Jobs and processes

A command is `ssh host 'cd <worktree> && bash -lc …'`. A background job runs remotely under `setsid`
with its output in a file under the worktree's `.letibot-remote/jobs/`, its pid recorded, so it
survives the ssh connection and this Mac sleeping; `job_output` reads the file, `job_kill` kills the
remote process group. A run that the Mac's daemon loses track of (a restart) is found again from those
files.

## Permissions

The remote host is not a boundary the way a VM is: it is the operator's machine with the operator's
files. So a remote session gets the **same gate as a host session** — the modes, the always-ask list,
the corpus — with the remote host named in every card. A per-host `trust = "allow-all"` in hosts.toml is
a later decision, not this slice's.

## A remote is an RPC, and the client follows the practices

Every tool call on a placed session is a remote procedure call: ssh is the transport, the
ControlMaster is the connection pool, the remote shell is the server. The operator, 2026-10-26:
*"the tooling shouldnt hang, that is important"* — *"like it is classic RPC architecture right"* —
*"so we should follow practicies"*. These are the rules every remote code path is held to:

1. **A deadline on every call.** Nothing waits unbounded: connect (`ConnectTimeout`), each
   operation (its own wall-clock bound, enforced by killing the local ssh process — never by
   trusting the remote), a command (`run` keeps the tool's own deadline), a probe (a total bound
   across hosts). A passed deadline returns a sentence naming the host and the step.
2. **Failure detection.** `ServerAliveInterval`/`ServerAliveCountMax`, so a host that stops
   answering mid-call is noticed in seconds; a stale ControlMaster is checked or replaced, never
   waited on.
3. **Retries follow idempotency.** Idempotent operations (read, stat, list, workdir, a probe,
   job output, and write — one session, temp + size and hash checked + `mv -f`) may be retried
   once, on a fresh connection. **Non-idempotent ones (`run`, starting a job) are at-most-once**:
   never re-sent after a timeout or a dropped connection — with the one exception of a session the
   multiplexer *refused*, which never reached the shell and is retried for any operation.
4. **An unknown outcome is said as unknown.** A `run` whose connection dropped is reported as
   *"it may have run, partly run, or still be running on HOST — check before running it again"*,
   never as a failure that implies it did not happen.
5. **Fail fast on a host that is down.** After repeated connection failures a host is marked
   unavailable for a short window (a circuit breaker): calls to it fail at once with the reason
   and the time it was last reached, instead of each waiting out a timeout. **Built (RH6).**
6. **Cancellation reaches the far side.** An interrupt kills the local ssh *and* the remote
   process group (the job's recorded pgid), so a stopped turn leaves nothing running there.
   **Built (RH6).**
7. **Observability.** Every remote call is recorded with host, step, duration and outcome, so a
   slow or flaky host is visible on the head and in `hosts {probe: true}`, not guessed at.
   **Built (RH6).**
8. **Back-pressure.** A host has a bound on concurrent calls (this daemon's own ControlMaster and
   its sessions), and calls beyond it queue with the same deadlines rather than opening unbounded
   connections. **Built (RH6)**, and made a real bound per daemon by **R99**.

How each of them is built, and the numbers chosen where the contract left them open, is
[§ As built (RH6)](#as-built-rh6-four-practices-one-refusal-and-a-toolchain).

## Toolchains through mise

The operator, 2026-10-26: *"regarding cargo and stuff - we do it via mise generally"*. Measured
that day on all four hosts: `mise` lives at `~/.local/bin/mise` and is **not on a non-login
shell's `PATH`**, the globally installed tools differ per machine, and a project pins its own with
`mise.toml` or `.tool-versions`. So a remote command that runs without mise sees the wrong
`cargo`, or none.

Every remote command therefore runs as

    <mise> exec -C <the checkout> -- bash -lc '<the command>'

when the host has mise — a foreground command, a step of the backend's own work, a background
job's launcher, a probe, and git's `git-upload-pack`, which is the one that has to be spliced in
by hand because sshd runs it without a login shell. **`-C` has to be a directory that exists**:
mise refuses one that does not (`mise ERROR Directory specified with --cd does not exist`, exit 1),
so the wrapper is pointed at the host's `root` — which the placement has already made — until the
checkout exists, and at the checkout afterwards. A `~` in it is expanded **there**, the same way a
`path` entry is; quoting it would hand mise a literal `~` to resolve against the wrong directory.
`path` stays as the escape hatch, and `mise = false` turns the wrapper off for a host whose
toolchain is on `PATH`.

`mise exec` **may install a pinned tool on its first use** — that is network traffic on the remote
machine, as the command asked for, and it is the one thing in this file that reaches the network.

## The supervisor, and work across hosts

The operator, 2026-10-26: *"since we introduced remote hosts we can do map reduce now — usually agents
hand roll orchestration that span multiple hosts, say 'run this server tests here and that server
there', either on both separately or on both together"* — and *"so the remote pid supervisor becomes
harnessd concept"*. The Stroppy session that day hand-rolled exactly that: `ssh … nohup … &` to a
bench box, `ps -p` polls, log tails, and a run the session lost track of when its one-shot ended.

**The supervisor is harnessd's, and it is cross-cutting: ANY process letibot starts is
supervised**, no exceptions — a foreground `bash` call and a background job, an operator `!` run,
the merge gate's steps, a reviewer's or a subagent's commands, an LSP server, a firecode VM, a
fanout branch, a scenario role; local or remote. Each is a **record in the store**: id, owning
session, host, process group, role, command, deadline, readiness check, state, and the RPC record of
its last check. The store is the truth, so a restarted daemon finds every remote process again. The
daemon's own loop supervises them under the RPC rules above: bounded liveness checks per host,
deadlines enforced, readiness probed, the per-host circuit breaker. **Teardown follows ownership**:
a session's close, an interrupt, or a failed role kills the recorded process groups on every host.
One record feeds every reader: the activity (`waiting on: job j12 on lab2x1`), the jobs pane,
`hosts {probe}`, and the bridge.

**Two tools are clients of it:**

* **`fanout` — the same work on many hosts, separately (map), with the answer as the reduce.**
  `{hosts: [..] | {host: command}, command, deadline}`: one supervised job per host in parallel;
  the result is one row per host — exit status, duration, output tail, or why it never ran (an
  unreachable host is a row, not a failure of the whole). A background job like any other.
* **`scenario` — work on several hosts together.** Roles, each `{host, command, ready, after}`:
  `ready` is a readiness check (a port that accepts, a log line, a command that exits 0) with its
  own deadline; `after` orders the starts; a role's command may name another's facts
  (`${server.host}`, `${server.port}`). Started in order, each waited ready; the run's own command
  (the client, the test) runs when all are ready; on its end, on any role failing, or on an
  interrupt, **everything is torn down on every host** and the result is per role. docker-compose
  across machines, without containers.

Both stay under the session's gate, every card naming the hosts it reaches.

## What an agent hand-rolled without builtins (the Stroppy session, measured 2026-10-26)

The operator: *"look at the stroppy conformity session, look at the orchestration patterns it came up
with. we need to give it builtins"*. A read-only survey of that Claude Code session (1,038 Bash calls in
the main transcript, ~20,500 across ~140 subagents; counts approximate) found it had built a job
system of its own in `stroppy-conformity-local/bin/` — `remote.sh` (laptop side, 290 lines),
`remote-job.sh` (host side, 339), `fleet.sh` (sharding, 641), rewritten several times — and taught its
agents to use it. Those scripts are the spec. What it needed, ranked by the hand-written code each
would remove:

1. **Named remote jobs** (`remote.sh -x` ×1,197, `wait` ×230): launch detached (`systemd-run --user`,
   else `setsid nohup`), state in a per-job dir (`cmd, pid, log, exit, DONE, STOP, started`), liveness
   by `/proc/<pid>/cmdline` so a reused pid is not the job, log read from an offset, stop = group kill
   plus the job's resources, states `none | starting | running | done N | stopped N | dead | lost`,
   and an unreachable host retried with backoff (`ServerAliveInterval`, exit 75 = unreachable, the job
   may still run). → **RH7's supervisor records are exactly this**, local and remote.
2. **A wait/watch primitive** (~400 `until`/`sleep` loops, 95 Monitor calls re-armed on timeout,
   `letibot-mcp wait` in the background): wake the agent on a job's end, a pid, a file or log regex
   (local or remote), or a worker's attention, with a deadline and no re-arming. → RH7.
3. **Workers that outlive a turn**: worktree from origin/main, held across turns, wake on done / ask /
   failure, liveness by last activity, broadcast to N, a diff against the merge base; and a worker must
   not end its turn while its own jobs run (it did: *"You ended your turn again while jobs were in
   flight"*). → the bridge and D1 inside letibot: `task_start` gains these.
4. **Tree sync and cross-build to a host** (rsync ×119, scp ×107): incremental sync to a per-job dir
   including untracked files, a local cross-compile (`GOOS=linux`), and a per-host environment
   preamble (`mise activate bash --shims` ×56). → RH6's mise wrapper plus an rsync mode beside the git
   placement.
5. **A matrix / fleet runner** (`fleet.sh` ×235, `for h in …` ×137): hosts × configs × repetitions,
   placement by live capacity (cores − load − reserve, free memory) and longest-first by measured cost,
   interleaved arms (each with its own database), per-cell outcome, **a check that every planned cell
   ran** (its own audit found none), merged results, resume from a plan file. → RH8's `fanout` grows
   into this.
6. **Resource leases and teardown on shared hosts** (`docker ps` ×430): per-job compose project,
   a block of ports outside the ephemeral range (checked against `ss -ltn`), a subnet, all under a host
   lock; teardown of exactly what the job created; and a per-host **never-touch list** of the owner's
   own workloads (pgfuzz on lubuntu1, flowy-dogfood-pg on lubuntu3, the etcd fuzz on lab2x1).
   Failures it hit: ephemeral-port collisions, loopback exhaustion, "address already in use" ×18,
   "too many open files" ×34, a container left 19 h. → new: RH9.
7. **Host registry with capacity** (cores, reserve, flags such as `heavy`/`tmpfs`, the never-touch
   list) plus live load. → `hosts.toml` gains `reserve`, `flags`, `never_touch`; `hosts {probe}` reads
   live capacity (HT).
8. **A service fixture**: start, wait ready (`pg_isready`, a port, a log line), run clients, always tear
   down — mostly on one host, inside one job. → RH8's `scenario`.

## Packages

| id | builds |
|---|---|
| RH1 | hosts.toml; `SshBackend: ExecBackend` (run, read, write, list, stat, processes with remote jobs); `Placement::Remote(host)`; the git placement both ways; `task where: "host:NAME"`; `--host NAME` for a root session; the progress and seam lines; tests with a fake `ssh` (a script that runs the command locally against a temp "remote root"), never a real host |
| RH2 | **done, but for the smoke test**: the bridge's `leticode_start { host }`, the host-name check, the placement record, and the refusal to reuse a daemon placed elsewhere (§ *As built (RH2)*). A live smoke test on lab2x1 is the orchestrator's, and nothing here contacted a host |
| RH3 | **done, but for per-host trust and a remote's GPU/model as the provider**: a remote command and every remote job run inside `bwrap` **on the host**, in the same view a local confined session gets; the probe is one round trip per host, cached; a host whose probe fails falls back to today's shape and says why; and `backend_confined` is read off the backend instead of being hardcoded `true` (§ *As built (RH3)*) |
| RH6 | **done**: the RPC practices 5–8 (circuit breaker, far-side cancellation, call records, back-pressure) after HT's 1–4; auth refusals made definite; `migrate`'s steps bounded; toolchains through `mise`; the `notes` tool's body exempt from the never-write list |
| RH7 | harnessd's process supervisor: store records for every started process, the supervising loop, ownership teardown, restart recovery; `bash background` on it |
| RH8 | `fanout` (growing into the matrix runner: capacity placement, interleaved reps, every-cell check, resume) and `scenario` (service fixtures) on the supervisor |
| RH9 | **done, but for a subnet lease**: resource leases on shared hosts (a block of ports outside the ephemeral range, taken under the store's own lock), exact teardown of what a lease created, and `hosts.toml`'s `reserve`/`flags`/`never_touch` (§ *As built (RH9)*) |
| RH4 | later: the whole daemon on the remote, the head attaching over an ssh-forwarded socket — a session that keeps thinking with the laptop's lid closed. The operator: *"move load from laptops — I start doing something, it starts eating resources and just stays on the hot laptop"*. RH1 moves the load (builds, tests, benchmarks) but not the model loop. |
| RH5 | **done**: one ssh policy for both callers (RH1's `SshBackend` and RH4's `--migrate`), the operator's own ssh material never read, `hosts.toml` read strictly with a per-host `path` list, and `~/.ssh` a hard refusal for every tool |
| HT | **done**: the `hosts` tool — which machines are declared, and which is least loaded, read at the call rather than frozen in a prompt; the remote checkout's own job directory kept out of `git status`; and, for every ssh operation that is not a command, a wall clock that kills the local ssh rather than waiting |

## As built (RH1)

Everything above is what was built, with these decisions taken where the contract left a
choice — each one written down because a slice that quietly narrows its own contract is the
defect this tree keeps paying for:

* **The seam is a branch, and the branch is checked.** `task`'s `seam:` line for a remote
  child says what comes back (`remote/<host>/<short id>`) and when (when it ends) — the
  opposite of the VM's line, which had to stop promising a branch.
* **`git fetch` runs through the same ssh policy** (`-c core.sshCommand=…` built from the
  backend's own flags) but on a **ControlPath of its own**: git starts that ssh itself, so the call
  takes no place in the eight-slot gate, and a socket of its own is what keeps a fetch from being
  the ninth session on the master the calls are bounded on (R99).
* **`~` in `root` is expanded by the remote shell, once, and the answer must be
  absolute.** Quoting it (`cd '~/x'`) silently makes a directory literally named `~`, and
  the result is a worktree neither this box nor `git fetch` can name; a non-absolute answer
  is refused rather than used.
* **A remote job's exit is its exit file.** `<worktree>/.letibot-remote/jobs/<id>.{sh,pid,out,exit}`;
  `wait_job` polls that file every 120 ms, which is the one cost of this shape (one ssh
  round trip per check) and is bounded by the run's own deadline.
* **The deadline is armed on the remote**, not on this Mac: a detached `sleep … kill` over
  the same connection. A local thread would die with this daemon while the process it guards
  does not — a job outlives this machine sleeping, so its deadline has to as well.
* **A group kill only when the pid leads one.** `kill -TERM -<pid>` on a pid that is not a
  process-group leader signals *this shell's* group; `ps -o pgid=` is asked first, and a host
  without `setsid` gets a single-process kill and a note on the record saying so.
* **`task_start` from a remote session is refused by name.** It makes git worktrees with
  the local `git` against a local path; a tree made here is one the child could not reach.
  The way to have a remote child is `task` (or `task_start` run from a host session), whose
  `open` arranges the checkout where the tree is.
* **No monitors on the remote.** `monitor` refuses by name (the process host says it has no
  monitor mechanism) rather than watching a condition nothing would reap. Watchers are the
  next slice's business, with the rest of the host's.
* **A command's stderr arrives merged into its stdout**, because ssh carries one channel —
  the same trade `firecode in` makes, said in `SshBackend::describe` rather than left to be
  discovered.
* **There is no `remote_sync` verb**: the branch comes back when the session ends
  (`ExecBackend::close`). The contract allows either.
* **`host:NAME` is also spelled `remote:NAME`**, and `--where host:NAME` is the same act as
  `--host NAME` — one rule, with the name checked against `hosts.toml` in both places.

## As built (RH2): `leticode_start { host }`, and a daemon that says where it is

Written 2026-10-26. The third caller in the table above, Claude Code, and the one fact a head was
missing: **a placement is a fact of the DAEMON, and the protocol carries it nowhere.** A head
attaching is told the session's model, endpoint and dialect — all per session, all on the wire —
and nothing about the backend, which is one per process anyway.

* **`leticode_start { host }` passes `--host NAME`** to the daemon it starts, and checks the name
  first with the tree's one `hosts.toml` reader and the one refusal (`task { where: "host:NAME" }`
  gives that sentence, and the bridge gives the same). The bridge runs no ssh and builds no
  backend: `harnessd` owns the placement.
* **The daemon records its placement beside its socket** — `<key>.placement`, written by
  `letibot_sessionlog::placement` after the folder's bind, so only the daemon that actually holds
  the folder writes it. The record names the socket's `(dev, ino)` as well, which is what makes a
  record left behind by a daemon that is gone read as *nothing known* rather than as the new
  daemon's answer (the same pair `unlink_if_ours` compares, for the same reason: a filename is not
  an identity).
* **`leticode_start` joins an already-running daemon only where the call asked to be.** A
  mismatch — another host, or this box, or a daemon that recorded nothing — is refused, naming the
  running daemon's placement and `letibot --stop`: stopping somebody else's daemon is the
  operator's act. The failure this closes is the one that would be silent: a package asking for
  `host: lab2x1` that joined the local daemon would run its whole brief here, and every answer
  would look working.
* **`placement` is in every answer that can be asked** (`leticode_start`, `_status`, `_list`,
  `_attach`), and `tools_run` on `leticode_start` is the same fact as a sentence — with the half
  that did **not** move, because a remote session's test result is read here.

## As built (RH5): one ssh policy, and the operator's ssh material is never read

RH1 and RH4 each spelled their own `hosts.toml` reader and their own ssh argv, and by the time
RH5 was asked for they had already drifted: **neither passed `-F`**, so both read the operator's
ssh client config, and neither checked the host key it was about to trust. Both are now one
implementation, in `crates/tools/src/remote.rs` — the policy section above — and `crates/letibot`'s
migration calls it (`Endpoint::ssh` is a `Ssh`, not a vector of flags). The dependency direction
is the tree's own: `letibot` already depends on `harnessd`, which depends on `tools`, so nothing
was inverted to share it.

* **One `hosts.toml` reader, strict.** Unknown keys refused by name, a key stated twice refused,
  and `path` refused unless it is one line of quoted strings. RH4's reader was already strict;
  RH1's tolerated anything, and it is RH1's that a session actually uses.
* **The two refusals happen at connect time**, in `Ssh::connect`, before anything is arranged on
  the other machine: the identity file (missing, relative, or resolving inside the operator's ssh
  directory) and the host key (not in letibot's own known-hosts file). A placement that failed
  half way would have a mirror on the far side and no way to say which step refused.
* **The never-write list is about paths, not prose.** It used to be applied to every string
  argument, so a `write` whose *body* quoted a path under the operator's ssh directory was
  refused as if it were writing there, and a `grep` for the pattern was refused as if it were
  reading the directory. Measured while building this: three such refusals opened the circuit
  breaker on `write` for the rest of one session. A **path argument** is still scanned, and so is
  `bash`'s `command`; a replacement's text, a file's body and a search pattern are not paths.
* **And the list is now asked of the path, not the spelling.** `ssh_resolution_hit` resolves each
  path argument — every symlink, and a leaf that does not exist yet — and refuses it if it lands
  in the operator's ssh directory. The tree's own words for why: a rule that matched only a
  spelling is a rule the first symlink routes around.

## As built (HT): the `hosts` tool, and ssh that cannot hang

The operator: *"the tooling shouldnt freeze host list in the prompt"*, and then *"the tooling
shouldnt hang, that is important"*. Both are about the same thing — a model that can see where
it may place work, and never waits on a machine that has stopped answering.

* **`hosts` reads the file at the call.** No arguments is the declared hosts with what a
  connection would need (the dedicated key, the accepted host key), and **no ssh call at all** —
  so it answers with every machine switched off. `probe: true` is one bounded ssh call per host,
  **in parallel**, reporting cores, load average, memory, free disk under the host's `root`, and
  whether `git`/`cargo` are on its `PATH` with the host's `path` list in front — the facts that
  decide *where*. The list is never in the prompt: the file is the operator's and changes without
  a restart, and the prompt is a cached prefix, so a list there would be stale *and* expensive.
* **A probe never writes anything on the far side.** It is one script of `uname`, `nproc`,
  `/proc/loadavg`, `/proc/meminfo`, `df -Pk` and `command -v`, and nothing else.
* **`hosts {probe: true}` returns inside ten seconds whatever the hosts do.** Each host's probe is
  bounded, and the collecting loop has a total bound of its own, so a host whose ssh ignores its
  own bound is reported as timed out rather than waited on.
* **`ServerAliveInterval=5` and `ServerAliveCountMax=3` are in the one policy**, beside
  `ConnectTimeout`: a TCP connection that is up is not a host that is answering, and without them
  a link that dies leaves a socket that accepts writes and returns nothing for ever.
* **Every ssh call that is not a command carries a wall clock** ([`STEP_BOUND`], 20 s; a status
  poll is shorter). The **local ssh is killed** at the deadline — nothing is signalled on the far
  side — and the refusal names the host and the step. A model's command (`run`) keeps the tool's
  own deadline and nothing else, because a build's duration is the tool's business.
* **A stale `ControlMaster` socket is checked and dropped.** `ssh -O check` with a few seconds on
  the question, then the socket (ours, in letibot's runtime directory) is removed so the next
  call dials afresh instead of waiting on a master that is gone.
* **The retry follows idempotency.** `read`, `stat`, `list`, `list_tree`, `workdir`, a probe,
  `job_output` and `write` are asked **once more** after a timeout or a dropped connection. A
  write is one session with its bytes on stdin and its own size and SHA-256 checked before an `mv`
  (R99), so the retry is the whole write and never a part of it. `run` and a job's start are
  **never** retried: a call that dies in flight may have run, partly run, or still be running
  there, and the refusal says so in those words rather than reporting a failure that would suggest
  nothing happened. **One exception, and it is a refusal, not a loss**: a session the multiplexer
  refused never reached the remote shell, so it is retried whatever the operation — including
  `run`, which then runs exactly once (§ *As built (R99)*).
* **The session's own job directory is not a change in the tree.** `.letibot-remote/` is written
  into the checkout's own `info/exclude` when the checkout is made — `git rev-parse --git-path`
  answering where that file is, rather than this side guessing a path that differs per layout —
  and the uncommitted list on close drops it as well, for a checkout made by an older build.

## As built (RH6): four practices, one refusal, and a toolchain

The operator, 2026-10-26: *"the tooling shouldnt hang, that is important"* — *"like it is classic
RPC architecture right"* — *"so we should follow practicies"*. Rules 1–4 were HT's; this is the
rest, plus three things the same day made obvious: an auth refusal is not a lost connection, a
migration's steps were unbounded, and a remote command that runs without the project's toolchain
is a command that builds with the wrong compiler.

Everything here lives in one new module, `crates/tools/src/rpc.rs`, and **there is one
implementation of each** — the backend (`Ssh`), the `hosts` tool's probe and `letibot --migrate`
all call it. The state is `static`, keyed by the host's name, because the contract's rule 5 says
so: three private breakers would mean a host that is down costs each caller its own three
timeouts, which is the cost the breaker exists to remove.

* **Rule 5 — three failures in a row, inside a minute, and the host is not dialled for thirty
  seconds.** Three, because one failing step is already two attempts (the retry), so waiting for
  two *steps* would be four timeouts before anything decided; sixty seconds, because "in a row"
  without a window opens the breaker on three unrelated bad minutes in a day, and one hung turn
  produces its three attempts well inside it; thirty seconds of cooldown, because that is the
  shortest window in which *did it come back* is a real question (`ServerAliveInterval` gives up
  on a dead link in about 15 s, `ConnectTimeout` on an unreachable one in 20). The refusal says
  the count, when it opened, **when the host was last reached**, and when the next probe is. After
  the cooldown **exactly one call is let through** as a probe — a floodgate at the moment the
  client knows least would be the wrong shape — and a probe that answers closes it.
* **A refusal from the far side never counts as a connection failure.** `Permission denied` is a
  machine that is up; counting it would open the breaker on a key nobody installed.
* **Rule 6 — the remote process group is recorded and killed.** A command of the model's
  (`ExecBackend::run`, the one non-idempotent door) is wrapped as
  `setsid bash -c 'echo $$ > <file>; ( <cmd> ); …'`: `setsid` makes the shell a session leader, so
  its pid **is** its process group, and the file is removed when the command ends normally, so a
  stale id can never be signalled later. A call that stops answering kills the local ssh *and*
  then makes one more bounded call (`CANCEL_BOUND`, 10 s) that reads the group, asks
  `ps -o pgid=` whether that pid really leads one — a `kill -TERM -<pid>` on a pid that does not
  signals the *caller's* group — and sends `TERM`, waiting at most a second before `KILL`. A
  background job already had this ([§ Jobs and processes](#jobs-and-processes)); this is the same
  rule for the one command that does not go through a job.
* **`run`'s bound is the connection's**, and the earlier text above said something else. There is
  no "tool's own deadline" on `Command`: `run` carries [`Ssh::bound`], which is `STEP_BOUND` (20 s)
  in production. Said plainly here rather than left as a claim the code does not keep — and it is
  what an abandoned `run` is cancelled against.
* **Rule 7 — every call is recorded, in a ring of 256 per process.** Host, step, duration and one
  of five outcomes: `ok` (it answered, whatever it said), `refused` (the far side said no),
  `timed_out`, `dropped`, `unknown` (the local process could not be started). The **step is a
  name, never a payload** — *reading a path*, *this command ran*, *a probe of lab2x1* — because a
  record is read by a head and printed to a log. `hosts {probe: true}` shows the last four calls
  and the three slowest per host, including for a host that could not be reached; and
  `$LETIBOT_RPC_DEBUG=1` prints one line per call to stderr, which is the daemon's log. **The one
  exception is a heartbeat**: a job's status poll is one call every 120 ms, so 256 of them are
  half a minute's worth and would evict every call a reader could act on — a poll is recorded
  only when it fails, which is the only time it says anything a reader needs.
* **Rule 8 — eight ssh invocations per host at once, and the connection is this daemon's own.**
  `MaxSessions` defaults to 10 per connection (a ControlMaster is one connection); the two this
  leaves are the master's own channel and one `-O check`. Ten exactly would have the *server*
  refuse the eleventh with `Session open refused`, which reads like a fault on the host rather than
  a queue we forgot to keep. **The bound is a promise only because the socket is per daemon**
  (R99): shared by every daemon on the box, eight per process was eight times the number of
  daemons against one limit of ten. A call beyond the bound **waits for a slot under its own
  deadline** and is refused with a sentence if it does not get one — a queue that outlives the
  deadline it was queued under is the hang coming back through the door marked back-pressure.
  `-O check`/`-O exit` do not take a slot: they are local IPC to a socket, not a session on the far
  side.
* **An auth refusal is definite, said in words, and never retried.** Measured live that day: three
  hosts whose key had not been installed answered `Permission denied (publickey)`, and the client
  called it *lost the connection, it may not have happened* — and asked **twice**. OpenSSH prints
  those words and exits 255 with nothing on stdout, which is exactly the shape a dropped connection
  has, so the difference is read from stderr (`Permission denied (…)`,
  `Too many authentication failures`, `Host key verification failed`,
  `No … host key is known for …`, `remote host identification has changed`). It becomes its own
  outcome, it is never retried, it does **not** open the breaker, and the sentence is the one the
  contract asks for: *letibot's key is not authorised on HOST (or HOST's host key changed)* — with
  `ssh-copy-id` for the first and `ssh-keyscan` for the second, because the two fixes are two
  different files.
* **`letibot --migrate`'s steps are bounded, and its build is watched.** Every step of a move is
  `STEP_BOUND`; the two streamed transfers (a bundle out, a bundle back) are `GIT_BOUND` (5 min)
  and the bound is applied *before* the encoders are joined, because `wait()` on an encoder blocks
  on the ssh's stdin and is how a hang gets in front of the rule that forbids one. The far side's
  `cargo build` gets `BUILD_BOUND` (30 min, `--build-timeout SECS`) and **progress**: the last
  thing cargo said is printed every ten seconds, so *still working* is distinguishable from
  *wedged*. The retry classes are the backend's (`Retried::Once` for the questions — `git
  rev-parse`, `git status`, `cat <stamp>` — and `Never` for everything that changes something),
  and the breaker and the call records are the same `static`s, because it is one process.
* **Toolchains through mise.** `hosts.toml` gains `mise`: a program path (`~` expanded on the
  remote, once) or `false`. Left out, it is **probed once per host per process** — `$HOME/.local/bin/mise`
  first, then `command -v mise` — and the answer is cached **including "no mise"**, because a host
  without it must pay one round trip for that fact and not one per command. A probe that did not
  *answer* is not cached: a timeout is not a fact about what a machine has installed. The wrapper
  is `<mise> exec -C <a directory that exists> -- bash -lc '<the host's PATH; the command>'`, and
  `-C` is the checkout rather than `$HOME` because a project pins its versions in its own tree —
  with two rules the first cut got wrong, both measured on a host with a real mise. **`-C` must
  exist**, and the checkout does not until `git worktree add` has made it, so the wrapper points at
  the host's `root` until then (a placement died at `making the mirror` on every one of the
  operator's machines until this was fixed). And **a `~` in it is expanded on the remote**
  (`one_entry`, as `path`'s entries are): a quoted `~` is a path mise resolves against the wrong
  directory. git's side is spliced by hand (`--upload-pack '<mise> exec -C … -- git-upload-pack'`)
  because sshd runs it with no login shell at all; a background job inherits the environment its
  launcher was started with, so it is wrapped by the same act. `hosts {probe: true}` reports the
  mise version and `mise ls --current` in the host's root **instead of** *cargo on PATH* — on a
  host with mise that question has no meaning — and falls back to `PATH` on one without.
* **The `notes` tool's body is prose.** The never-write list was narrowed once (RH5) to skip a
  `write`'s body, a replacement's text and a search pattern; `notes` was not in that list, so a
  note *about* `.git` or `.ssh` — which is the note that records the rake — could not be written.
  `text`, `abstract` and `query` are content arguments now. A note's **`name` is not**: it becomes
  a file under the notes directory, so it is still scanned, and both halves have a test.
* **A fixture says which toolchain it has.** `mise = false` on every fake host in the test suites,
  because the default probes *the machine running the test*: on a developer's Mac that finds the
  real `/opt/homebrew/bin/mise` behind the fake ssh and puts it in front of every assertion. The
  mise tests are the ones that name a path or plant `$HOME/.local/bin/mise`.

## As built (RH3): a command on a host runs inside a boundary

Written 2026-10-26. The measurement that asked for it: a session placed with `--host lab2x1` at
`writes-allowed` asked the operator **over a hundred times** across one package — *"the remote
side has no confinement, so every shell command is a decision"* — and an `allow_session` grant
barely helps, because it is keyed on a pipeline's last program plus the exact intent set. A
**local** Linux session's commands run inside `bwrap` (`crates/tools/src/exec/confine.rs`, Layer
1) and the gate answers with a boundary behind them. This is the same thing, one machine over.

* **One builder, two renderings.** `confine.rs`'s argv builder now produces `Piece`s — an ordinary
  argument, or a fragment the **far** shell must expand — and each caller joins them for the medium
  it has: `Bwrap::wrap` for a process this box starts, `Bwrap::remote_pieces` for a shell script on
  another machine. The view, the egress decision, the two seals and the `--unshare-*` set are
  spelled **once**. A second spelling of the binds is how a policy drifts, and the first cut of the
  remote wrapper was that second spelling.
* **Why the keep-list has to be the far shell's.** `--clearenv` puts back `KEEP_ENV`, and its values
  are facts of the machine the command runs on: a Mac's `PATH` inside a Linux sandbox is a list of
  directories that do not exist there. So the remote rendering emits `--setenv PATH "$PATH"` and
  the role is played by the remote's own login shell, evaluated just before the helper runs.
* **The probe is one round trip per host, and it is cached — including a negative answer.**
  `Bwrap::probe` cannot be used: the helper is there, the namespaces are that kernel's, and the
  denominator a readback is compared against is *that box's* `/proc/self/ns/*`, not this one's. So
  `confine::probe_script` prints the helper's version, the **outer** inode map, the toolchain paths
  that exist under that `$HOME`, and then the readback from inside the same argv a command will be
  wrapped in; `read_probe` + `measured_from_probe` read it through the same `state_of` and the same
  three refusals a local probe uses. Cached per host (`remote::CONFINE`) because the answer is a
  fact about a kernel, not about a session; **a probe that did not answer is not cached**, because
  remembering a timeout as a fact is how a session spends its life unconfined — while *`bwrap` is
  not installed here* and *it ran and entered nothing* are facts, and are kept.
* **The view, and the one thing deliberately not in it.** The checkout read-write at its own path;
  the system roots read-only; the **toolchain read-only** at `~/.cargo`, `~/.rustup`, `~/.local/bin`,
  `~/.local/{share,state}/mise`, `~/bin` and a `~/Projects/llama.cpp/build*` when one is there;
  `~/.gitconfig` read-only, because without it every `git commit` inside the boundary fails with
  *Author identity unknown* and the branch **is** the deliverable; and a `$HOME` that is letibot's
  own directory (`<root>/.letibot-home/<short>`), never the operator's. The operator's ssh
  directory is not on that list and cannot be: it is not a toolchain, RH5's refusal is about
  letibot never reading it, and a view whose `$HOME` is not theirs does not contain it in the first
  place. **A read-only cargo home cannot fetch a new dependency** — that is the stated cost, and the
  answer to it is a wider view, not a writable operator cache.
* **The toolchain variables travel.** Measured on `lab2x1`: with `$HOME` a fresh directory, `cargo`
  is on `PATH` but `rustup` looks for its toolchain under `$HOME/.rustup` and answers *rustup could
  not choose a version of cargo to run*. So `CARGO_HOME`/`RUSTUP_HOME` (and mise's two data/state
  variables) are set to the grants, and the operator's toolchain works from a home that carries
  nothing.
* **No `--new-session` on a remote spawn, and that is load-bearing.** A group kill reaches the
  processes that are in the group; `--new-session` moves the sandbox out of it, so `job_kill` would
  end the launcher and leave the work running. The flag is now `exec::Session` — `Detached` for a
  local job, `Kept` for a pane (its pty is its own) and for a remote command (its process group is
  what an interrupt kills). Nothing is given up: a remote command's stdio is pipes and files, so
  there is no terminal for `TIOCSTI` to reach.
* **Jobs go through the same wrapper.** The launcher's `( … )` subshell now runs the helper's argv
  with `/bin/sh -c '<command>'` after it, and a job's environment pairs travel as `--setenv` (they
  would be discarded by `--clearenv` otherwise). The launcher is `setsid`'d, so the sandbox is in
  the job's process group and `kill -TERM -<pid>` still reaches everything.
* **The network is the session's rule, not a default.** `Egress::Denied` unless the point seats
  network calls, in which case the boundary shares the host's namespace with the sentence that
  decided it (`SshSpec::egress`). A session carrying `web_fetch` must not have its `curl` fail at
  the socket after the gate said yes, and a session carrying none must not reach out because a
  sandbox defaulted the other way.
* **The gate: `backend_confined` is now read, not asserted.** The remote arm of `Harness::open`
  hardcoded it to `true` — so a placement that confined nothing passed the mode's
  `Prereq::Confinement` and could sit at `allow-all`, whose whole promise is that a structural
  boundary is doing the deciding. It is now `in_vm || backend.boundary().is_some()`
  (`ExecBackend::boundary`), which fixes the local confined seat the same way. A host whose probe
  failed is unconfined: every call a decision, the banner and the placement note say why, and the
  **card** says why — `AdjudicatedGate::with_boundary` replaces the fixed sentence *"the host
  filesystem is not sandboxed; §11.4's boundary arrives with firecode"* with the measured view, or
  with the probe's refusal, or with that old sentence when no question was asked at all.
* **A confined session's file tools see what a command sees, and nothing else.** `read`, `write`,
  `edit`, `list` and `glob` refuse a path the view does not contain (`BackendError::Outside`), and
  admit one it does — the checkout, the system roots, the toolchain grants. **This is the half RH5's
  refusal cannot reach on a placed session**: that rule resolves a path on *this* box, so a path on
  the far machine resolves to nothing here and would pass it; a view has no such trouble. Before
  this slice `root_path` answered `None` for every remote session — honest when there was no
  boundary, wrong once there is one.
* **What is still not confined, stated rather than left to be found.** The placement's own SSH
  traffic (a bundle out, a branch back, the probe) runs as letibot's ssh invocation outside the
  boundary, because it is this side's transport and not a session's command. A `--grant-ro` is a
  path on **this** machine and is not carried into a remote view; the toolchain list is the remote's
  own facts. And the operator's ssh directory is unreachable from a command because `$HOME` is not
  theirs — which is stronger than a rule against reading it.
* **Tests never contact a host.** `crates/tools/tests/remote_confine.rs` is RH1's pattern with a
  `bwrap` shim behind the fake ssh: it answers `--version`, **fabricates the readback** in the
  probe's own vocabulary, and otherwise runs the command after `--` with the
  `--clearenv`/`--setenv` environment it was handed. So the assertions are about the argv that
  crosses the connection — the binds, the `$HOME`, the absent `--new-session`, the toolchain at
  read-only — and they hold on a Mac with no `bwrap` at all. The kernel itself is measured where it
  always was: `tests/confine.rs` runs the real helper for a local spawn, and once, by hand, on
  `lab2x1` for this slice.

## As built (RH7): the record, the probes, and the one rule the two activities shared

Written 2026-10-26. **What this slice is:** the record every supervised process gets, the liveness
check per host, deadline enforcement, teardown by ownership, restart recovery — and the fix for the
false wake the brief opens with. **It is wired**: `JobRecorder` writes a job's `process` row from the
live view and its ending from the settlement, `crate::supervise::spawn` is started beside
`vm_reap::spawn` (its first tick is the recovery), `Harness::open` adopts this session's open
records, and `Sessions::close`/`close_backends` are the two teardowns.

* **A table of its own, and not a column on `job`** (schema 34, `store::procs`). `job` answers *what
  became of a handle a session read a moment ago*; it is written twice per job by two callers who
  each know half, and it has no row at all for a process a restarted daemon **adopts** rather than
  starts. The supervisor's questions are about a process in the world: where it runs, what makes a
  reused pid not it, which group a teardown signals, when it must be gone, and how it was last
  looked at. Those are `process`'s columns.
* **Six states, and the last three are not interchangeable** — `starting | running | done |
  stopped | dead | lost`. `dead` is *gone with no exit recorded* and is deliberately not `done 0`:
  nothing said the command succeeded. `lost` is *the host did not answer*, and the job may still be
  running — which is why `lost` is **open**, retried on the next tick, and why a host that goes
  quiet for a minute does not bury eight builds.
* **The probes are the box's own, and one of them was already written.** `identity_of` is
  `letibot_tools::exec::procs::read`'s `start` field spelled `start:<n>` — `/proc` on Linux, `libproc`
  on macOS, with the zombie rule a hand-rolled read gets wrong — and the remote arm is the ONE script
  `SshProbe` builds over the tree's one ssh policy (`Ssh::sh_once`, now public for it). A pid is
  never compared alone, and a job a daemon did not start is settled [`JobState::Dead`] — *gone, and
  no exit was recorded* — never `exited 0`.
* **Adoption is how the tools read the record, and no tool changed.** `ProcessHost::adopt` turns a
  `process` row back into a `Job` (its cgroup name `<owner>/job-<handle>` is derivable, so re-opening
  it is the same directory), and `Harness::open` adopts this session's open records and arms the
  watcher. `jobs`, `job_output`, `job_kill` and `job_wait` therefore work unchanged after a restart,
  and a backend that cannot identify a pid refuses by name instead of pretending.
* **The supervisor is one thread per process, and a `static` for the teardown's sake.**
  `supervise::spawn` opens its own connection and ticks once a second, logging only when something
  happened; the tick is `Supervisor::tick` and the first one IS the recovery. `OnceLock<Arc<Supervisor>>`
  is what `Sessions::close` and `close_backends` reach — `rpc`'s breaker precedent, and one supervisor
  per process by construction.
* **One check per host per tick, and never one per job.** `Probe::check` is handed every open record
  of one host and answers for all of them: locally one pass over `/proc` (no fork at all on Linux),
  remotely ONE script over ONE connection that reports every job directory on that host. The test
  that pins it counts probe calls, not assertions about jobs.
* **A deadline is a wall clock, not a duration**, because the thread that armed it may not be the
  one that enforces it — a restart is exactly this slice's case. The kill is `TERM`, a second of
  grace, then `KILL`, and it is the **process group only when the record says the pid leads one**
  (`pgid == pid`): `kill -TERM -<pid>` on a pid that is not a leader signals the caller's own group,
  which is a way to end the daemon instead of the job.
* **A job that vanished before its deadline is `dead`, never `stopped by its deadline`** — killing a
  process that is not there is not what happened, and the record says which one it was.
* **A kill that could not be delivered is not a kill.** The record stays `lost` and the next tick
  tries again, so a teardown never reports work ended that it never reached.
* **Teardown follows ownership, and the list of what does NOT tear down is the load-bearing half:** a
  **session closing** kills every open record it owns on every host, and the daemon's stop is every
  session closing. A **turn ending** is not a teardown (a job started to outlive its turn keeps
  running across turns — that is what `background: true` means), and neither is an **interrupt**,
  which ends a round and leaves the job the session's.
* **Restart recovery is one tick over the store.** Nothing in the supervisor remembers a `Child`, a
  thread or a connection, so `recover()` is `tick()`: the records it leaves open are the ones a
  session has to be re-attached to, and that half is the harness's, because re-arming a watcher is.
* **A session moved to another machine does not take its records with it.**
  `store::transfer`'s `TABLES` list — the whole of the rule for what a migration carries — does not
  name `process`, and deliberately: a record is a pid on ONE host, and a copy on the far side would
  hold rows that name processes it cannot see, cannot check and must not signal. What is left behind
  is what is true there — the processes are still running here.

### The false wake, and the two spellings of one rule

The bridge's `wait` woke on a session whose model had said it would be woken by its job: the
activity said `quiet`, `jobs_open` said `["j13281"]`, and the daemon was about to hand that job's
settlement over. The cause is that **the delivery decision and the activity each had their own copy
of the same predicate**, and the copies had drifted in two ways:

1. **The slug.** `delivery::facts_of` matched a todo row against the job's handle **or the name the
   model gave it** (`bash(slug: "release-build")`, and a row that says `needs: job release-build`);
   `activity::awaited` compared the handle and nothing else. A session waiting on a NAMED job was
   therefore delivered at once (`Why::Needed`) and read `quiet` at the same time — `wait` reads the
   activity and reported *the turn ended* about a session that was about to take a turn.
2. **The kind.** `awaited` accepted `TodoNeed::Child { id }` for **any** handle, so a row blocked on
   a child id that happened to be a job's read as *busy* about a result no delivery would hand over.

Both copies are gone: `delivery::plan_says` is the one reading of the board, and `activity::awaited`
calls it. The watcher set carries the job's **slug** now (`JobWatchers::mine` is
`handle → (kind, slug)`, read from the live view at the one moment it exists), because the activity
cannot match a name it was never given. The regression test asserts both sides — the activity says
busy **and** `decide` says `Immediate(Needed)` — because a fix on either side alone would let them
drift again.

**What could not be reproduced, said plainly:** the operator's own case is not in this worktree and
its store was not opened. `j13281` may have been named by its slug, or by nothing at all — the
second would mean the model expected a wake the plan never asked for, which is a different defect
(the delivery rule's, not the activity's) and is not fixed here.

## As built (RH8): `fanout`, `scenario`, and the process book they share

Written 2026-10-26. The two tools the contract asked for, and the one new seam under them. Everything
here is code that runs on the box the tools run on; a test never contacts another machine.

```text
crates/tools/src/fleet.rs                the engine: hosts, legs, cells, readiness, the book seam
crates/tools/src/builtins/fanout.rs      `fanout`  — one command per host, one row per host
crates/tools/src/builtins/scenario.rs    `scenario` — roles, a run, and a teardown that happens
crates/harnessd/src/supervise/book.rs    `StoreBook` — the daemon's half of the process book
```

### One branch is a job, and a job is a record

Neither tool invents a way to run something. A leg — a fanout branch, a scenario role, a scenario's
run — is `spawn`ed through that host's own [`ProcessHost`]: `RemoteProcesses` for a name in
`hosts.toml`, the session's own backend for `local`. So the launcher, the `setsid` process group, the
pid file, the exit file and the group kill are RH1's code, and a fanout branch dies the way a
`bash` background job dies.

On top of that each start is **written down**, through a seam this slice adds:

* `letibot_tools::fleet::ProcessBook` — `started(&ProcessNote)` and `ended(handle, state, exit)`.
  `NoBook` writes nothing (a run with no store says so); the daemon registers
  `supervise::StoreBook` bound to its own session and store, so every branch and every role is a row
  in the `process` table (schema 34) with its host, its pid, its identity, its deadline and the path
  of its output file.
* **Why it had to be written**: `JobRecorder` writes that row for a job of *this* host, and a branch
  of a fanout is a job of a host the session is merely reaching. Nothing else would have written it,
  and the two holes that leaves are the ones RH7 exists to close — a daemon that restarted mid-fanout
  would find no process to adopt, and a session's close would kill nothing on the machines it
  reached.
* **What the record gives, measured through RH7's supervisor**: one liveness check per host per tick
  (`SshProbe` reads the exit file first, then `ps -p … -o command=` against the `cmd:` marker), the
  record's own wall-clock deadline (a branch whose model turn is long gone is still killed), and
  `teardown_session` on a session's close or the daemon's stop. The record's identity is `cmd:<the
  command>` on a host and `start:<n>` here — the two spellings `supervise` already reads.

### `fanout`

`{hosts: [names] | {name: command}, command?, deadline_s?, cwd?}`. One leg per host; the list form
takes the shared command, the object form carries a per-host one. `local` is the session's own box
and needs no ssh at all.

* **The row, and the three fates.** `exited N · 1.2s · tail: …`, or `stopped · the deadline passed`,
  or `never ran — <why>`. The three never collapse: an exit status is the command's own answer, a
  kill is something this tool did, and *never ran* is a host that could not be used. A non-zero exit
  is a **row's** business and not the call's, so four hosts out of five still report.
* **The count is the table's shape.** The answer is `crate::fleet::cells(plan, done)` — the plan,
  each leg with a row, and a leg with no result is a row saying it never ran. The Stroppy session's
  own fleet runner skipped cells and never noticed; here the omission is unspellable.
* **An unknown host is a row with the names the file has** (`there is no host `nope` in … — it
  knows: fake1`), and an unreachable one carries ssh's own sentence (`Could not resolve hostname …`).
  The breaker's refusal, when it is open, is a row too.
* **`deadline_s`** (default 600, ceiling 3600) is the call's wall. It is enforced twice and the
  duplicate is deliberate: this loop kills a leg that outlived it and writes `stopped`, and the
  **record** carries the same instant so a daemon that outlives the call still ends it. A branch that
  exits at 0.4 s of a 600 s deadline costs nothing.
* **Starts are sequential, the work is not.** Each launch waits for its own pid (that is RH1's
  `launch`), so N hosts cost N launch round trips; the commands then run concurrently. Making the
  waits concurrent is a change to `RemoteProcesses`, not to this tool.
* **One card, naming everything.** `Access::Exec`, and the call's arguments are the hosts and the
  commands — which is what the adjudicator's brief carries. Nothing here is a second gate.
* **Every branch is addressable in its own host's table** (`jN` on that machine) and the row names
  the file its output went to, `<root>/fanout-<call>/.letibot-remote/jobs/<n>.out`.

### `scenario`

`{roles: {name: {host, command, ready?, after?, deadline_s?, ready_deadline_s?, cwd?}}, run?,
deadline_s?}`.

* **Three readiness checks, asked on the role's own host** — `{port: N}` (a TCP connect there),
  `{log: "<regex>"}` over the tail of that role's own output, `{check: "<cmd>"}` as a short job of its
  own whose exit 0 is the answer. Each has its own deadline (default 30 s) and every wait is capped
  by the whole scenario's. A role **that has already died is known not to be becoming ready**, and
  the row says that instead of waiting out the deadline.
* **`after` is a dependency, not a sequence**: a role starts when the roles it names are READY.
  The order is Kahn's, one role at a time, and ties follow the order the caller wrote them in —
  `serde_json` keeps a call's key order, and a `BTreeMap`'s is alphabetical, which is a plan nobody
  wrote. A cycle, an `after` naming no role, and a role called `run` are refused **before anything
  starts**.
* **`${role.host}` and `${role.port}`** are substituted into a role's command and into the run's.
  `host` is the `hosts.toml` name (which is also what ssh connects to unless the file says
  otherwise); `port` exists only for a role that declares `ready: {port: N}` — a `log` or `check`
  readiness check names no port, and asking for one is refused rather than spliced as empty, because
  `host:` in a client fails thirty seconds later with no breadcrumb.
* **The teardown is not optional**, and it is one place: after the run, after a role that never came
  up, after a role that could not start, and on any early refusal. Every started role is stopped on
  its own host, in reverse start order, and its row says what it was before: a role that had already
  died says `exited N`; one that was still up says `stopped · the scenario ended` (or `the scenario
  failed: …`). **An interrupt is the supervisor's**: the roles are `process` records owned by the
  session, so a session's close or the daemon's stop ends them through `teardown_session` — the same
  list, from the store, rather than a `Drop` this build would have to trust.
* **A row per role and one for the run**: `web on fake1 · stopped · 0.5s · the scenario ended · ready
  after 0.0s · tail: …`. *Ready after* is how long the readiness check took to answer — the number a
  person asks for when a fixture is slow.

### Seams, and the two things they cost

* **`ProcessHost::job_log`** — a default method, `None` by every implementation but
  `RemoteProcesses`, which answers the job directory's `<n>.out`. It is not `JobView::output_file`,
  and the difference is the reader: that field is a path *this* box can open, and this one is a path
  the *host* would open (the supervisor derives the exit file beside it). One fact, two readers, two
  fields.
* **`remote::expand_root`** — a host's `root` as the far shell must spell it, `~` expanded THERE
  once. The rake RH1 hit and RH4 hit again, in one function so a third caller cannot hit it a third
  time: `mkdir -p '~/letibot-remote'` makes a directory called `~`.

### What this slice does not do, said plainly

* **`fanout` blocks for its answer and has no background form of its own** — `background: true` is
  FB's, below. Until then a long fanout holds its turn; `deadline_s` is what bounds it.
* **Consequently `job_output` and `job_kill` do not reach a branch.** Its handle is in that host's
  table, and the row names the output file. **FB gives both a door** — the parent handle — and the
  branch handles stay where they are.
* **The matrix half of RH8 is not built**: capacity placement from `hosts {probe}`, interleaved
  repetitions, resume from a plan file, and the merged result set. `fanout` is the shape those grow
  from and its cell accounting is the audit they need.
* **No per-host trust.** Every card is the session's own, and a fanout's card names the hosts because
  its arguments do. A `trust = "allow-all"` per host is a later decision (the contract says so too).

### Rakes, measured here

* **A `case` glob over `$*` is not a host test.** The fake ssh that refuses one host matched `*gone*`
  anywhere in its argv — and the first fanout test named its work directory `fanout-gone`, so a
  healthy host was reported unreachable for a reason that lived in the test. It matches
  `*gone.invalid*` now, which is the destination.
* **A job that exits within the launch's pid poll has pid 0.** `RemoteProcesses::launch` polls for
  the pid and stops early when the exit file is already there, so a five-millisecond command reports
  `pid: 0`. Harmless — the exit file is the settlement and nothing signals pid 0 — but a test that
  wants to look at the process has to give it a moment.
* **`mise exec -C` must point at a directory that exists**, so a host's work directory
  (`<root>/fanout-<call>`) is made with a plain step *before* the connection is told to use it as its
  `-C`. RH6 measured the refusal; this is the order that avoids it.

## As built (RH9): leases, exact teardown, and a host's never-touch list

Written 2026-10-26. **What this slice is:** a block of a shared host's ports that one session holds
and nobody else can be given, the prefix its containers are named under, a teardown that removes
exactly those, and the operator's own workloads made untouchable. It is the answer to the list a
Stroppy session wrote down after colliding with itself — *ephemeral-port collisions, loopback
exhaustion, "address already in use" ×18, "too many open files" ×34, a container left 19 h* — and
to its second half, that the hosts it collided on also run the owner's own work.

```text
crates/tokencore/src/store/leases.rs       `host_lease` (schema 35), the row, and `pick`
crates/tools/src/lease.rs                  the engine: the census, the block, the teardown
crates/harnessd/src/leases.rs              the daemon's half: StoreLeases, release_session, the reaper
```

### `hosts.toml` gains three keys, and it is still strict

```toml
[host.lab2x1]
hostname   = "lab2x1"
user       = "dead"
reserve    = 4                       # cores held back for the owner
flags      = ["heavy", "tmpfs"]      # what the machine is for
never_touch = ["etcd", "pgfuzz"]     # names letibot must never signal, stop or remove

[local]                              # FB: the same list, for the box the daemon runs on
never_touch = ["etcd", "pgfuzz"]
```

* **`reserve` is a whole number of cores and anything else is refused**, not defaulted. A
  `reserve = "two"` read as zero would be a scheduler that believes a busy machine is empty, which
  is the collision this package exists to remove. `flags` and `never_touch` are lists of quoted
  strings on one line, the `path` grammar — refused by name, and refused when half a list parses.
* **`hosts {probe}` shows all three** and the live capacity they are for: `cores − load − reserve`
  and the memory available. The subtraction is reported, never enforced, because a scheduler this
  build does not have would be the wrong place to start. A host that did not answer its load is
  `unknown` and not `0.0` — a machine that is full and one nobody asked are different facts — and
  an oversubscribed box reports a **negative**, because the reason is the useful half.
* **`never_touch` is a case-insensitive SUBSTRING**, and that is the safe direction rather than the
  lazy one. The operator declared the workload (`pgfuzz`, `etcd`, `flowy-dogfood-pg`) and not its
  spelling; a rule that missed `etcd-fuzz-0` because the line said `etcd` would be the one failure
  this list exists to prevent, and it would be silent — the thing is stopped and nobody is told. A
  rule that is too broad refuses a teardown the session could have had, and **says which pattern
  refused it**, which is one word in the file.

### A lease is a row, and the lock is the store's

* **Schema 35, `host_lease`**, declared `(35, Arm::Additive)` in `READER_ARMS` — a new table, so an
  older binary keeps serving a store a newer one has migrated. `id | host | session_id | prefix |
  port_first | port_last | ephemeral | taken_ms | released_ms | released_by`. **A released lease is
  not deleted**: *this session held 61000–61015 on lab2x1 for four minutes* is what a person reads
  when asking why a port was busy, and it is the only record a restart can tell *held by a session
  that is gone* from *never held* by.
* **`BEGIN IMMEDIATE` is the per-host lock, and it is the only one that spans processes.** Two
  daemons on this box share one `sessions.db` (`store/floor.rs` measured ~11 of them), so a mutex in
  a tool would serialise half the callers. The census is taken *before* the transaction, because a
  probe over ssh inside the write lock would hold every other session's lease for up to a round trip.
* **The block starts above the host's own ephemeral range**, read there: `/proc/sys/net/ipv4/
  ip_local_port_range`, or the `sysctl` pair on a Mac. A port inside that range is one the kernel
  hands to somebody else's outgoing connection whenever it feels like it, which is precisely the
  collision. A host that will not say its range is a REFUSAL and not a guess.
* **A port somebody is already listening on is skipped, whole.** One read-only round trip runs
  `ss -ltnH` (or `netstat -ltn` on a BSD) and the ports are parsed on this side, so the far side's
  locale is not this side's problem. A run of ports that touches a busy one is stepped over rather
  than trimmed: a block is contiguous, because a service that takes four wants `first + 3` to be its
  own.
* **The prefix carries the lease's number** — `letibot-<host>-<session>-<id>` — so two blocks on one
  host from one session cannot share one. The id is the store's, so it is assigned before the name is
  final and the row is written once.
* **What a lease is not, said plainly:** a kernel cannot be asked to keep a port free. The block
  guarantees that no other session of letibot's took it and that nothing was listening there when it
  was taken. A program on that machine that binds one without asking is not stopped by anything here.

### The tools take from it, and never pick

* **`scenario`: `ready: {"port": "lease"}`** — a port from the session's block on the role's own
  host, assigned **before anything is started**, one allocation per host and not one per role. Every
  `${role.port}` in the call then resolves to a number the block owns, so the client is pointed at it
  by the same substitution that has always pointed it at an explicit port. An explicit
  `ready: {"port": 5432}` still works and is the caller's own business.
* **`fanout`: `lease: true | N`** — a block per branch, on that branch's own host, one port or N.
  The block travels to the command as `LETIBOT_LEASE_PORT_FIRST`, `…_PORT_LAST`, `…_PORTS` and
  `…_PREFIX`, because a fanout's branches are all planned before any block exists and a command that
  had to spell its ports would have to be written after. A branch that could not get a block is a
  ROW saying so (`LETIBOT_LEASE_REFUSED`), not a failure of the call: the other machines still work.
* **The substitution check moved out of `plan` and into `answer`**, and that is load-bearing: the
  facts a command resolves against now include a port that does not exist until the lease is taken.
  It still happens before a single process starts, which is the promise that matters — a refusal,
  not a teardown.

### Exact teardown: the prefix says what is mine, and the list wins

`Blocks::release` is one act, called from four places — a scenario's end, a fanout's end, a
session's close, and the reaper — so there is one answer to *what does ending this do*.

1. **What is there** — one `docker ps -a --format '{{.Names}}'` and one `docker compose ls`. A host
   with no `docker` is a sentence (`no docker`), not a failure: a machine that runs no containers is
   an ordinary machine.
2. **What is mine** — container names that **start with** the lease's prefix, and the compose project
   that **is** the prefix. `starts_with` and not `contains`, because two sessions' prefixes share a
   head (`letibot-fake1-s-ab-1` and `letibot-fake1-s-ab-9`), and a `contains` here would remove a
   stranger's work.
3. **What is not** — every name about to be removed is checked against the host's `never_touch` list
   first, and a name that matches **is left alone and the refusal names the pattern**. The prefix says
   *what is mine*; the operator's list says *what is not*; the list wins.
4. **What is gone** — `docker rm -f` for the containers, `docker compose -p <prefix> down` for the
   project, every argument single-quoted (a name is arbitrary text).
5. **And only then the row goes back.** A release that happened first would leave a lease nobody
   holds whose containers are still up, and the next session's block could be the one that collides
   with them.

**The supervisor asks the same question before every signal it sends.** `Supervisor::may_touch` is
the door the deadline kill and `teardown_session` both go through: a record whose command matches a
pattern on its host's list is not signalled, its record stays **`lost`** — nothing was ended, so
nothing may say `stopped` — its `check_how` is `refused`, and the sentence naming the pattern goes
to the log. **A record with no host** — a process on the box this daemon runs on — is checked against
this box's own list, a `[local]` table in the same file; FB builds that and § *As built (FB)* says
what it is.

### Two moments a lease goes back, and the one that is not one

* **A session closes** — `Sessions::close` and `close_backends`, after the processes are ended,
  because a role's containers are what its processes started.
* **The owner is found gone** — the supervisor's tick, which is the half a hard-killed daemon leaves
  behind: the row says the block is held by a session nobody serves.
* **A restart is not an owner being gone**, and the difference is `leases::GRACE_MS` (60 s). A daemon
  that has just come up holds no session for the first second of its life, so a reaper that asked *is
  this session in the registry* would release every block on the box at each restart — the opposite
  of what a lease is for. A session must be **seen** gone for the grace, and the first tick that finds
  it gone only stamps it.
* **The reaper's decision and its act are separate** (`leases::due` and `leases::reap`): the grace is
  arithmetic a test can move a clock through, and the act is an ssh round trip. The store's and the
  clock's locks are **down before the act**, because a release opens its own connection and may tear a
  prefix down on a host.
* **`RegistryOwner` is the daemon's answer to *is anyone still working in this session*** —
  `Registry::holds`, which sees a session on a court's own thread and one in `Sessions::open` alike.

### Tests, and what they cost

`crates/tools/tests/leases.rs` is RH8's fake-ssh rig plus **a fake `docker`** in the rig's own `bin`,
first on the far side's `PATH` — the teardown's whole job is *what did it remove*, and the answer has
to come from the program that would have removed it. **No test contacts another machine and none can
reach a real docker.**

* two sessions on one host get disjoint blocks, both above the box's own `/proc` range;
* a port a test is **really listening on** is skipped, with the range stated so the block is
  deterministic;
* a release frees the block (the next caller gets the same ports) and removes only its own prefix —
  another session's container and the operator's own are untouched, and so is a *longer* prefix that
  merely starts with ours;
* a container matching `never_touch` survives, and the refusal names the pattern and the host;
* a host with nothing of ours left is a sentence, and the block still goes back;
* **a new engine over the same store still sees the block held** — a restart, as a second connection;
* **a scenario's `${role.port}` is a number out of the block**, and the block goes back with it.

`crates/harnessd/src/supervise/tests.rs` adds the other half: a `teardown_session` reaches `kill` for
the session's own build and not for the record whose command matches `etcd`, the record stays `lost`
with `check_how = refused`, and `may_touch`'s sentence names the pattern, the host and the handle.
`crates/harnessd/src/leases.rs` tests the grace with a clock it moves and a release it records — no
ssh, no store, no docker. `crates/tokencore/src/store/leases.rs` tests `pick` as arithmetic and the
allocation as two sessions on one store. `crates/tools/src/remote.rs` tests the three keys, their
refusals, the substring, and `cores − load − reserve`.

### What this slice does not do, said plainly

* **No subnet lease.** The contract's item 6 also names one; a docker network's address pool is not
  something this build reads or reserves, and inventing a `subnet` column nothing checks would be the
  shape of a guarantee without the substance.
* **Interleaved arms, capacity placement and resume from a plan file** are still RH8's *matrix half*
  and not this slice's. `hosts {probe}` now reports the spare cores they would place by; nothing
  places by them yet.
* **No teardown for anything but docker.** A session that starts its services by other means has its
  processes ended by the supervisor and its names are its own business.
* **A lease is not swept for size.** A session that takes a second block on a host it already holds
  one on gets a second block; nothing merges them.

## As built (FB): `fanout` settles as a background job, and `[local]` never_touch

Written 2026-10-26. Two things RH8 and RH9 each said they could not do, done where they said the
work was — and one of them turned out to need less than the note claimed.

```text
crates/tools/src/builtins/fanout_bg.rs   `background: true` — the handle, the tracker, the journal
crates/tools/src/builtins/fanout.rs      the `background` argument, and the one `table` both forms use
crates/tools/src/exec/host.rs            `Aggregate` — a job of jobs, and the four questions it answers
crates/tools/src/remote.rs               the `[local]` table, and one copy of the never-touch rule
crates/harnessd/src/supervise.rs         `may_touch` asks the local list too
crates/harnessd/src/jobwatch.rs          a settlement carries what a job of jobs says for itself
```

### One handle in this session's table, and not a composite process host

RH8's note said giving a fanout a settlement *"means the session's process host becomes a composite
of the hosts it can reach … a restructure of `HostProcesses`/`RemoteProcesses` and `JobRecorder`, not
a tool"*. **The restructure was not needed, and the smaller shape is better.**

A settlement needs exactly four things from `ProcessHost` — `wait_job`, `job`, `output`,
`kill_job_as` — plus `jobs()` so a handle resolves. So `ProcessHost` gains one seam,
`hold(&JobId, Arc<dyn Aggregate>)`, and `HostProcesses` keeps a second table beside its jobs:

* **`Aggregate`** (`crates/tools/src/exec/host.rs`) is *a job of jobs*: `state`, `elapsed`,
  `produced`, `output`, `wait`, `kill`, `report`. Every read path in `HostProcesses` asks the
  aggregate table first, so `jobs`, `job_list`, `job_output`, `job_kill`, `job_wait`, `jobwatch` and
  the activity's `waiting on: job …` are **the code that was already there**.
* **A composite would have had to answer a harder question**: what `SpawnRequest.host` means, which
  table `JobView.host` names, and which host's recorder writes the row. This answers none of them,
  because the fanout's branches are still jobs of their own hosts — each with its own `process`
  record (RH8), its own deadline and its own group kill. The parent is a *handle*, not a host.
* **The row a job of jobs answers with says two things plainly**: `pid` is 0 (there is no process,
  and the same 0 a remote job reports when it exits inside the launch's pid poll), and its scope has
  no cgroup path (there is no group of ours holding those branches). `kill_job_as` routes to
  `Aggregate::kill` before it ever looks at a cgroup.
* **`JobRecorder` skips the `process` record for one** — a pid nothing on this box can be would be
  refused by the supervisor's identity check on every tick. The `job` row is still written, which is
  the durable half a restart reads.

### `fanout {background: true}`

`{hosts, command, deadline_s, background: true}` returns ONE handle at once. What it does with it:

* **The branches start in the call, the waiting does not.** A leg that could not start is a row
  immediately (the blocking form's rule, one call early), and the starts happen before the handle is
  held so a call that started nothing hands back no handle.
* **The session must be able to hold it** — `hold` refuses, and the call then stops every branch it
  just started and answers with that refusal rather than with a handle nothing can reach. A session
  with no process host is refused *before* the first branch, because branches running with nobody
  able to reach them is worse than a refusal.
* **A thread of the fanout's own watches the branches** (`FanoutJob::track_loop`), one pass over
  them, a breath between passes, bounded by the plan's own deadline. It is the only writer of a
  row, so a kill and a deadline and a command that exits all produce exactly one ending each.
* **A job of jobs is a name as well as a handle**: `job_output`, `job_kill` and `job_wait` take
  `fanout` (or `fanout-2`) exactly as they take a `bash` slug, through the host's own slug record.

**What the model is handed.** Three things, and the third is the one this slice adds:

1. the `Backgrounded` result — the handle, the name, and *its table reaches you on its own*;
2. `job_output` mid-run reads a **journal**: `<host> · started` for every branch in flight, and the
   row it becomes once it ends. Append-only, because an offset into a re-rendered table would mean a
   different thing a second later;
3. the settlement, and **the settlement carries the table**. `ProcessHost::settlement_note` is the
   one hook a job has for saying more than its state, and an aggregate is the one kind whose result
   IS its output — so `completion_notice` prints it under the job's row, cut at 24 lines, and the
   whole of it is one `job_output` away. **A `bash` settlement is byte for byte what it was**: the
   hook answers `None` for everything that is not a job of jobs.

**How it ends.** Every branch exited 0 is `Exited { 0 }`; anything else is `Exited { 1 }` — the
parent's own status is the table's reduction, and the table says which cells. A `job_kill` on the
handle stops every branch **through its own host and its own process group** (RH1), the parent
settles as `Killed { by }`, and each branch's `process` record ends `stopped`.

### What FB does not do, said plainly

* **A daemon restart loses the parent's settlement.** The table is in memory and the watcher is a
  thread of the old process, so nothing wakes the model with the per-host table after a restart.
  `job_output` on the old handle answers *no job called that* — a refusal, not a hang, and it claims
  nothing about work still in flight. **The branches survive** (they are `process` records, adopted
  by RH7's supervisor), so the work is still killed at its deadline or by a session's close. A
  re-armed parent would need the table in the store; that is a schema change and a supervisor pass,
  and it is not this slice.
* **Nothing links a branch row to its fanout parent, and nothing needs to.** The parent has no
  process, so it has no record for a branch to point at; teardown is per record and already covers
  every branch (RH8's book), so a `parent` column would be a link nothing reads. Schema stays 35.
* **One fanout cannot be nested in another.** A branch is a command, and a `fanout` is a tool call —
  the same line the blocking form draws.

### `never_touch` for this box: the `[local]` table

RH9's *"One hole, said plainly"* said a record with no host had no list to be checked against, and
that the fix was *"a list for this box"*. It is a section of its own, because this box is not a
placement — a `[host.…]` block names a machine ssh reaches:

```toml
[local]
never_touch = ["etcd", "pgfuzz"]
```

* **One key, refused by name** like the host blocks': a `[local].never_tuch` typo would leave the
  operator's workload unprotected and silent, which is the one failure this list exists to prevent.
* **One copy of the rule** — `remote::never_touch_hit(list, what)` — because three lists read it now
  (a host's, this box's, and the lease teardown's), and two copies of one rule is how they start to
  disagree. The match is still a case-insensitive substring, for the reason RH9 gives.
* **Both doors ask it.** `Supervisor::may_touch` is the one function the deadline kill and
  `teardown_session` go through, and a record with no host is now checked against this list before
  one is checked against a host's. The refusal is the same sentence shape — the handle, the pattern,
  *will not signal, stop or remove it* — the record stays **`lost`** with `check_how` `refused`
  (teardown) or `expired` (the deadline), and the sentence names the pattern.
* **An absent `[local]` table is no declaration**, not a default: a box nobody has told us to keep
  anything on.

### Tests, and what they cost

`crates/tools/tests/fanout.rs` (the RH8 fake-ssh rig, **no network, no cgroup**) drives a background
fanout against `HostProcesses::with_tree(…, NoScopes)`: the handle is a job of this session's table
with `pid` 0 and the name `fanout`, the call returns while two `sleep 20` branches run, the journal
names the row that ended and the branch still `started`, `job_kill` leaves both pids gone and both
records `stopped` with the parent `Killed`, and the settlement is the same table — including a row
for a host whose ssh does not resolve.

`crates/harnessd/src/jobwatch/tests.rs` proves the settlement with **no process at all**: a fake
`Aggregate` held by a `NoScopes` host is watched, `JobSettled` reaches the hub, the completion is
queued, and its `detail` is the table. `crates/harnessd/src/harness/tests/jobs.rs` proves the table
reaches the notice text, indented under the job's row, and that an ordinary job's notice is
unchanged. `crates/harnessd/src/supervise/tests.rs` proves both local never-touch doors.


## As built (R99): one master per daemon, a refused session that is transport, and a write that is one call

Written 2026-10-27, from the operator's report: with five or six packages placed on lab2x1, two
placements failed writing the 19 MB bundle (`incoming.bundle.letibot-<pid>` stopped at 2.8 MB) and
an `edit` through the placement **left a file at 0 bytes twice**, once with
`mm_send_fd: sendmsg(1): Message too long` and once reporting `found no match … Nothing was
written`. Retried one at a time, both placements succeeded — the failure was load, and the load was
the multiplexer.

```text
crates/tools/src/remote.rs   control_path (pid + role), git_control_path, MuxRefusal,
                             mux_refusal, write_abs/write_once (one streamed call)
crates/tools/src/rpc.rs      run_bounded_fed — the same runner, with bytes on stdin
crates/tools/tests/remote_sessions.rs   the fake ssh with a ledger, and seven forcing tests
```

* **One `ControlMaster` per daemon per host, and a second one for git.** The socket was named by the
  host alone, so every daemon on the box multiplexed over **one** OpenSSH connection with
  `MaxSessions` (10) slots on it, while each daemon's own back-pressure allows eight — i.e. eight
  times the number of packages against a limit of ten. It is now `ssh-<host>-<pid>-call.sock`, with
  `ssh-<host>-<pid>-git.sock` beside it for the `git fetch` that this process does not start and so
  cannot count (`core.sshCommand`), which is why the two cannot meet. **Measured, and this is why the
  cost is acceptable:** a full handshake, key exchange and public-key attempt against lab2x1's own
  sshd is **50–70 ms** (five runs, loopback, 32 cores), and `ControlPersist=120` makes that once per
  daemon per two minutes of idleness — six daemons pay six dials where they paid one, and stop paying
  it in refusals. The session count per call was the other measurement: a 400 KiB write was **9
  sessions** before (one per 48 KiB) and is **1** after, and the 19 MB bundle goes from ~400 to one.
* **A refused session is transport, not the command's exit.** Four sentences are read out of ssh's
  stderr and told apart from a lost connection: `mux_client_request_session: session request failed`,
  `mm_send_fd: sendmsg(N): Message too long`, `channel N: open failed: administratively prohibited`
  (the server's own `MaxSessions` refusal) and `Control socket connect …: Connection refused`. All
  four mean **no session reached the remote shell**, so nothing ran: the socket is dropped, the call
  is retried on a fresh connection **whatever the caller's retry policy says** — including `run`,
  which is otherwise at-most-once — and the sentence a model reads is *this is a refused session, not
  your command's exit status*. A refused session also counts as *the host is up* for the circuit
  breaker: a full session table is a busy path, not a dead machine, and counting it would have opened
  the breaker on a healthy host the moment six packages wrote at once.
* **A write is ONE ssh call, and the far side checks it before the `mv`.** The bytes go on the
  session's **stdin** (base64, so a short stream cannot look complete), and one script does
  `mkdir` → `base64 -d > <temp>` → `wc -c` → `sha256sum` (or `shasum`, or `openssl dgst`) → `mv -f`.
  The temp name belongs to that attempt alone (`<path>.letibot-<pid>-<counter>`) and a trap removes
  it on any exit, so a stream cut by a deadline or a lost connection leaves the **target
  byte-identical**. The bound scales with the payload — `STEP_BOUND` plus a second per 512 KiB — so
  the 19 MB bundle gets ~58 s where 400 chunked calls each had 20.
* **What was wrong with the old write, measured.** `crates/tools/src/remote.rs` sent one call per
  48 KiB, **discarded every chunk's exit status**, and ran `mv -f` unconditionally; the temp name was
  one per path per *process*, so an attempt that outlived its deadline could move a file a retry was
  still writing. Forced through the fake ssh with a far side whose `base64` cannot write, the old
  code returned **no error at all** and left the target at **0 bytes** — which is FB's report
  exactly, and why the `edit` that followed said *found no match*.

### Tests, and what they cost

`crates/tools/tests/remote_sessions.rs` — a fake `ssh` with a ledger of the sessions it opened
(files under the fake's own directory: a live file per session, an append-only event log, and the
arithmetic done in Rust), and **no network at all**:

* **`a_far_side_that_cannot_decode_never_touches_the_target`** — the zero-byte file as a forcing
  test: the write fails, the target is byte-identical, and no temp file is left behind.
* **`a_write_is_one_session_whatever_its_size`** — 400 KiB in **one** `base64 -d` session.
* **`a_refused_session_is_retried_on_a_fresh_connection`** — the first write session is refused with
  the mux sentence, the retry lands, two sessions carried it.
* **`a_run_whose_session_was_refused_runs_exactly_once`** — the at-most-once door, refused once and
  retried: the command's effect is in the file **once** and the tool result is the command's own exit
  0, not ssh's 255.
* **`a_cut_stream_leaves_the_target_byte_identical`** — the far side receives 64 characters of a
  300 KiB stream, twice; the write fails naming the short stream, the target is untouched, and a
  third write lands whole.
* **`twelve_threads_through_one_host_all_get_an_answer`** — twelve threads writing, reading and
  running at once against a fake that refuses the ninth concurrent session the way a full master
  does: **zero refusals**, and the peak it saw was eight.
* **`one_daemon_has_one_master_and_git_has_another`** — the socket names the daemon, git's differs,
  and the path fits `SUN_LEN`.

A rake worth keeping, measured while writing them: **a `mkdir` lock in a fake is not a lock.** The
first version of the ledger took one around a read-modify-write counter; 40 concurrent callers left
it at **32**, because a releaser's `rmdir` can land after the next holder's `mkdir` and delete *its*
lock. Files that are created and removed need no lock at all.

### What this does not do, said plainly

* **`MaxSessions` is not raised, and letibot does not read it.** The bound the client keeps is eight,
  which is safe for the default of ten; an `sshd` configured *below* that would still refuse, and the
  refusal is now a retried transport failure rather than a truncated file.
* **A per-daemon master means a daemon's own gate is the only limit** — which is what makes it a
  promise — but a `git fetch` and a `letibot --migrate` in another process each dial their own
  connection, so a box with many short-lived processes sees more handshakes than before. At 50–70 ms
  each and with `ControlPersist=120`, that is the price this slice chose over a cross-process pool.
* **The old socket names are not cleaned up.** A daemon built before this change left
  `ssh-<host>.sock` behind; it expires on its own `ControlPersist` and nothing reads it again.

## The merge gate on a declared host (2026-10-29)

The operator: *"i told you about other hosts so you actually use that capability for running gates
and subagents fanouts"*. The fanout half was already there (`fanout`, `task { where: "host:NAME" }`);
this is the gate half, and it exists because a night of landings ran `cargo test --workspace` on the
box that was also serving the daemon, a dozen worktrees and every VM.

### The setting

```toml
[queue]
gate_host = "lab2x1"      # a [host.…] from ~/.config/letibot/hosts.toml; empty is this machine
```

or, on the daemon's command line, `--gate-host lab2x1` — which wins over the file, and
`--gate-host local` puts it back here for one invocation. **Empty is the default and is today's
behaviour byte for byte**: `LocalGate` with nothing set is `run_with_memory`, in this machine's own
cgroup, under `[memory] gate`.

The value is read once at daemon start and handed to the queue through
`mergequeue::gate::set_gate_host`, because the queue builds **one** runner for the whole daemon
(`QueueEnv::production`'s `Arc::new(LocalGate)`) — the same seam, and the same shape, as
`[memory] gate`. `LocalGate`'s `run` is therefore the *dispatch*: nothing set is the local run, a
name is `HostGate` (`mergequeue::gate_host`). The name a row carries is the host's, so the queue's
green line reads `` `main` is green at 0f2d9550 (lab2x1 gate) ``.

A daemon seated above several repositories reads the workspace's own `leticode.toml`, exactly as it
does for `[queue] reviewers`.

### What crosses the wire, and what does not

**The merge stays here.** The host runs `sh -c <step>` in a copy of the landing worktree; the target
branch moves by a compare-and-swap on this box's `refs/heads/<target>`, and nothing in the host path
runs a `git` that writes anywhere but its own copy.

Two transports, and the second is not optional:

* **`rsync -a --delete`, over the host's declared key and root**, into
  `<host root>/gate/<repo>-<hex>/tree`. A landing worktree is not only its commit: `[queue]
  gate_carry` copies gitignored inputs into it (`images/`, `vendor/` — a firecode VM's kernel and
  rootfs), and those are in no commit, so only a filesystem copy carries them. `--delete` so a file
  the branch removed is gone there too; the repository's own directory and `target` are excluded
  (the first is a *file* naming a path on this box; the second is the local build cache).
* **A `git` bundle**, because `rsync` cannot carry a repository: a landing worktree's metadata is a
  file pointing at `<repo>/.git/worktrees/<name>`, which means nothing on another machine — and the
  gate's first step here is `sh scripts/check-fmt.sh main`, which runs `git diff main HEAD` and
  `git status --porcelain`. Without objects and a ref named `main` that step fails on every branch
  and the whole gate is red for a reason that has nothing to do with the code. So the tip and the
  target branch go over as a bundle (`git fetch` into the host's bare `src.git`, then a worktree at
  the tip), and the rsync'd tree lands on top: the index and `HEAD` are the commit, the working tree
  is the landing tree.

**The bundle is incremental and its own refs are the record.** The host's `refs/gate` and
`refs/heads` are asked for their tips and passed as `--not`, so the first gate pays for the history
and every one after it pays for the branch. What to fetch is read back off the bundle
(`git bundle list-heads`) rather than assumed: a thin bundle whose prerequisite already covers a
branch **drops that ref**, and asking for it is a `git fetch` that dies on *couldn't find remote
ref* (measured on the third gate of one repository).

### The warm cache

`CARGO_TARGET_DIR` is `<gate dir>/target` — outside the rsync'd tree and outside the worktree, so
neither `--delete` nor `git clean -fd` can take it. A cold gate on a host is worse than a local one
(the whole dependency tree compiled from nothing), which is the whole reason the directory is
per-repository and permanent.

Measured on lab2x1 (32 cores, 73 GiB free, its own 1.4 GB registry cache, ~15 ms round trips over
the daemon's `ControlMaster`), gating this repository at `f7397ad5`:

| what | cost |
|---|---|
| **the first gate of a repository on a host, nothing there** | **2.1 s** before the first step: 33 MB of tree over `rsync`, a 19 MB bundle, the bare store and the worktree (the step itself was 146 ms) |
| **every gate after it, same tip** | **0.8 s** — no bundle is built (the host already has the commit) and `rsync` moves no bytes |
| `cargo build --offline --bins -p letibot-harnessd`, cold `target/` | 32.2 s |
| the same step, warm | 1.3 s |
| `sh scripts/check-fmt.sh main` | 0.15 s |
| `cargo test --offline -p letibot-tokencore --lib` (139 tests) | 7.7 s warm, 11.8 s cold |

Two things to read off that. **The transport is not the cost** — a second and a half for a whole
repository the first time, and nothing after it — so what a cold host gate really costs is
COMPILATION, which is what the warm cache is for (a workspace `cargo test` cold on that box is
minutes; the numbers above are the shape of it, taken on one crate and on the daemon's own build).
And **the poll is a second**: a step that finishes in 146 ms reports as ~0.8 s in all, because the
gate learns it finished at the next poll — the same order as the local gate's 100 ms, one machine
further away.

**The gate says where the time went, on its first row.** Every gate writes one line into the first
step's log, so a row that says *thirty-seven seconds* is not read as a slow machine:

```text
[gate on lab2x1: the repository 1.28 s, the checkout 0.15 s, the tree 0.14 s — 1.42 s of transport
 out of the gate's 30 min; build cache COLD — this gate made it, so the first step below pays for
 the whole dependency tree; 209.7 GiB free on that filesystem]
```

Measured on lab2x1 on 2026-10-30 with this line in place, one step
(`cargo build --offline --bins -p letibot-harnessd`), the gate's directory removed between the two
runs:

| | transport | the step | in all |
|---|---|---|---|
| **the first gate, nothing on the host** | 1.42 s (1.28 bundle, 0.15 checkout, 0.14 tree) | **35.7 s** (cold `target/`) | **37.5 s** |
| **the second gate of the same tree** | 0.21 s (nothing to send) | 1.2 s (warm `target/`) | **2.0 s** |

So a first gate on a host is **transport plus a cold build**, and the transport is four percent of
it. `COLD` in that line is asked of the host *before* the gate's own `mkdir` touches `target/`, so
it says whether THIS gate makes the cache rather than guessing from a size.

### Two clocks, and a host that cannot be reached

A gate on a host is under **two** bounds and they are not the same number, which cost the queue a
halt on 2026-10-30:

* **The step's** — `[queue]`'s `gate_deadline`, thirty minutes unless set. It is the only thing that
  may end a step, and a first gate on a host needs all of it.
* **One poll's** — `POLL_BOUND`, thirty seconds. It asks *can this machine fork a shell and answer*,
  and on a box whose own build holds every core the answer can take many seconds.

The first real gate on `lab2x1` ran `LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace` on 25 of
its 32 cores; one poll took longer than the **ten** seconds a poll was then allowed, and a step four
minutes in was killed and reported as *the gate could not run* — which wrote the target `blocked`
and stopped both queues behind a healthy host.

So **a poll that is not answered is asked again, not a verdict.** The question is idempotent (its
answer is `tail -c +<offset>`, so nothing is lost by asking twice), a miss says *the machine was
busy*, and only the step's own deadline — or a poll that reaches no machine at all, which is the
breaker's judgement — may end a step. A verdict that does time out names the budget it ran out of
(`the gate's own deadline passed (30 min)`), and a step whose last minutes were spent asking a
question nobody answered says so, with the bound the last poll was given.

**A gate that could not run is not a verdict about the code.** `GateVerdict::Infra` — a host that
did not answer, a key that is not installed, a tree that would not arrive — writes the target back
to **`gating`** with the sentence that stopped it, and the tip is gated again by itself once a
minute has passed (`UNJUDGED_RETRY_MS`). It is *not* `blocked`: `blocked` is for a tip nobody can
judge at all (no declared `Merge gate`, a landing tree that cannot be made) and it stops both
queues. A branch whose own landing gate could not run goes back in line the same way and with the
same window, so a host that is down costs the queue a minute per attempt rather than a `/queue
restart`. The window is there because the worker's pass loop goes again at once when it worked:
without it, a broken `gate_host` would be one ssh attempt per second, for ever. The one `Infra` that
stays `blocked` is a step killed at the gate's memory cap — that step RAN, so re-running it with the
same cap kills it the same way.

Every call is bounded by `rpc::run_bounded` and goes through the same breaker (`rpc::before`) the
rest of the fleet path uses, so **a host that is down is a verdict in the first second**, not a
hang: `GateVerdict::Infra` with a sentence — *"… did not reach lab2x1: ssh exited 255, which is a
connection or authentication failure and never a remote command's own status — Connection refused.
Nothing was judged — the tip is UNJUDGED, not red"* — and the next pass gates it again when the
host is back.

Each step is started **detached, under `setsid`, with a pid file**, and polled once a second: one
round trip answers both *how is it* and *what has it written since*, so the log arrives at the same
local path the local gate writes (`$TMPDIR/letibot-gate/…`) and the pane and the failure excerpt do
not change. The status file is read **before** the pid, for
`a-finishing-job-and-a-killed-job-answer-the-same-poll`'s reason. At the deadline, on the daemon's
stop, or when the lease goes, the far side's whole process group gets `SIGTERM`, five seconds, then
`SIGKILL` — the local gate's own discipline, one machine over.

Two things are worth knowing about the exit status:

* **A shell reports *my child was killed by signal n* as `128 + n`**, and a `137` read as a code
  would make a step killed by `SIGKILL` a RED branch instead of an unjudged one. `exit_of` decodes
  129–192 as signals, so a host step and a local step are read the same way.
* **The row says its memory was not measured** — `[gate memory: not measured — the step ran on
  lab2x1, where this daemon has no cgroup of its own]`. The model's copy of the evidence drops that
  line like any other quiet one; the operator's row keeps it, because *where* a step ran is a fact
  about the run.

The one window a step can outlive its gate is a daemon killed with `SIGKILL` between two polls —
the same window the local gate has. A far-side `timeout` is deliberately NOT used: its exit status
would be one more number to tell apart from the step's own.

### Trying it by hand

```sh
cargo run --offline -p letibot-harnessd --example gate_on_host -- \
    lab2x1 ~/.local/share/letibot/landing/letibot-0123abcd
```

runs that tree's own `## Merge gate` on that host and prints one line per step with its outcome and
milliseconds; a trailing list of commands replaces the steps, which is how the numbers above were
taken. It writes nothing to the store, claims no lease and runs no queue.

### What this does not do

* **The host's `[memory] gate` is not applied.** There is no delegated cgroup on that machine, so a
  step there is bounded by the host's own memory and by nothing of ours. A step killed there is
  still `killed` and unjudged, not red.
* **`gate_carry` is re-sent on every run** — `rsync` is incremental, so an unchanged file costs a
  stat and not a byte, but a project that carries a gigabyte of images pays for the check each time.
* **A gate host is a daemon-wide setting**, not a per-repository one, for the reason above: the
  queue's runner is built once. A daemon serving several repositories where one wants a fleet box
  and another wants the local one needs two daemons.
