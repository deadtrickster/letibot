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

## Packages

| id | builds |
|---|---|
| RH1 | hosts.toml; `SshBackend: ExecBackend` (run, read, write, list, stat, processes with remote jobs); `Placement::Remote(host)`; the git placement both ways; `task where: "host:NAME"`; `--host NAME` for a root session; the progress and seam lines; tests with a fake `ssh` (a script that runs the command locally against a temp "remote root"), never a real host |
| RH2 | the bridge's `leticode_start { host }`; a live smoke test on lab2x1 run by the orchestrator |
| RH3 | later: per-host trust, bwrap on Linux remotes, a remote's GPU/model as the session's provider |
| RH4 | later: the whole daemon on the remote, the head attaching over an ssh-forwarded socket — a session that keeps thinking with the laptop's lid closed. The operator: *"move load from laptops — I start doing something, it starts eating resources and just stays on the hot laptop"*. RH1 moves the load (builds, tests, benchmarks) but not the model loop. |
| RH5 | **done**: one ssh policy for both callers (RH1's `SshBackend` and RH4's `--migrate`), the operator's own ssh material never read, `hosts.toml` read strictly with a per-host `path` list, and `~/.ssh` a hard refusal for every tool |

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
