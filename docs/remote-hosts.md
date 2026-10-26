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

One TCP connection per host: `ControlMaster=auto` and a `ControlPath` under letibot's own runtime
directory (`$XDG_RUNTIME_DIR/letibot`, or the box's temp directory when there is none), so a command
costs a round trip rather than a handshake. `BatchMode=yes` and `StrictHostKeyChecking=yes`: a host
that asks for a password or a host-key confirmation is **refused by name**, never prompted for — a
daemon has no one at a terminal, and a prompt it cannot answer is a call that hangs until its
deadline.

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
   job output, and write — temp + `mv -f`) may be retried once, on a fresh connection.
   **Non-idempotent ones (`run`, starting a job) are at-most-once**: never re-sent after a timeout
   or a dropped connection.
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
8. **Back-pressure.** A host has a bound on concurrent calls (the ControlMaster's sessions);
   calls beyond it queue with the same deadlines rather than opening unbounded connections.
   **Built (RH6).**

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
| RH9 | resource leases on shared hosts (ports outside the ephemeral range, subnets, compose projects under a host lock), exact teardown, and hosts.toml's `reserve`/`flags`/`never_touch` |
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
* **`git fetch` runs through the same ssh** (`-c core.sshCommand=…` built from the
  backend's own flags and ControlPath), so the fetch reuses the connection the session's
  commands used and the fake-ssh tests exercise the real transport shape rather than a
  second code path.
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
  `job_output` and `write` are asked **once more** after a timeout or a dropped connection; a
  write's *chunks* are never retried on their own (a chunk sent twice appends itself twice — the
  whole write starts again from the top instead, and only the last `mv -f` touches the target).
  `run` and a job's start are **never** retried: a call that dies in flight may have run, partly
  run, or still be running there, and the refusal says so in those words rather than reporting a
  failure that would suggest nothing happened.
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
* **Rule 8 — eight ssh invocations per host at once.** `MaxSessions` defaults to 10 per
  connection (a ControlMaster is one connection); the two this leaves are the master's own channel
  and one `-O check`. Ten exactly would have the *server* refuse the eleventh with
  `Session open refused`, which reads like a fault on the host rather than a queue we forgot to
  keep. A call beyond the bound **waits for a slot under its own deadline** and is refused with a
  sentence if it does not get one — a queue that outlives the deadline it was queued under is the
  hang coming back through the door marked back-pressure. `-O check`/`-O exit` do not take a slot:
  they are local IPC to a socket, not a session on the far side.
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
