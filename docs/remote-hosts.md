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
```

Five keys, and an unknown one is **refused by name** rather than ignored: a typo here places a
session on a machine nobody named, or points ssh at a key nobody meant. `hostname` and `user` are
the two that must be said; `root` defaults to `~/letibot-remote` and `key` to
`~/.config/letibot/keys/<host>`.

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
   and the time it was last reached, instead of each waiting out a timeout.
6. **Cancellation reaches the far side.** An interrupt kills the local ssh *and* the remote
   process group (the job's recorded pgid), so a stopped turn leaves nothing running there.
7. **Observability.** Every remote call is recorded with host, step, duration and outcome, so a
   slow or flaky host is visible on the head and in `hosts {probe: true}`, not guessed at.
8. **Back-pressure.** A host has a bound on concurrent calls (the ControlMaster's sessions);
   calls beyond it queue with the same deadlines rather than opening unbounded connections.

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

## Packages

| id | builds |
|---|---|
| RH1 | hosts.toml; `SshBackend: ExecBackend` (run, read, write, list, stat, processes with remote jobs); `Placement::Remote(host)`; the git placement both ways; `task where: "host:NAME"`; `--host NAME` for a root session; the progress and seam lines; tests with a fake `ssh` (a script that runs the command locally against a temp "remote root"), never a real host |
| RH2 | the bridge's `leticode_start { host }`; a live smoke test on lab2x1 run by the orchestrator |
| RH3 | later: per-host trust, bwrap on Linux remotes, a remote's GPU/model as the session's provider |
| RH6 | the RPC practices 5–8 (circuit breaker, far-side cancellation, call records, back-pressure) after HT's 1–4 |
| RH7 | harnessd's process supervisor: store records for every started process, the supervising loop, ownership teardown, restart recovery; `bash background` on it |
| RH8 | `fanout` and `scenario` on the supervisor |
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
