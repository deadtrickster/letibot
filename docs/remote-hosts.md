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
hostname = "lab2x1"            # what ssh connects to
user     = "dead"
key      = "~/.ssh/headless_server"
root     = "~/letibot-remote"  # where remote worktrees live (default)
```

ssh runs with `-i key -o IdentitiesOnly=yes -o BatchMode=yes` and a ControlMaster socket per host
(one TCP connection, so a command costs a round trip, not a handshake). BatchMode: a host that asks
for a password or a host-key confirmation is refused by name, never prompted for.

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
