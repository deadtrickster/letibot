# Subagents: downgrade, placement, and the firecode backend

What a `task` subagent is allowed, where it runs, and what it costs. `docs/leticode.md` has the
one-paragraph version; this is the reasoning and the measured numbers.

## 1. Inherit, downgrade, never upgrade

A subagent runs under its parent's permission ruleset (opencode's model,
`crates/tools/src/permission.rs`) with a `SubagentAdjudicator` that turns every `ask` into a refusal:
it has no operator to ask. `task` takes:

```
task(prompt, role?, access?, where?, verify?, repo?)
  role    any seat this build knows (coder default; researcher, planner, ...);
          an unknown one is refused by name, not seated as coder
  access  read-only | no-write, no-exec, no-network (comma-separated)
  where   host (default) | firecode | host:NAME | host:NAME:vm
  verify  with a VM only: a shell command firecode runs after the child, before its VM stops
  repo    with a VM only: which repository the VM holds, when the session root holds several
```

`access` is a `Downgrade` (`letibot_tools::schema::Downgrade`): a set of access classes denied below
the role. Three properties, each a type rather than a check:

1. It only removes. The child's downgrade is the union of the parent's own and the one asked for, so
   a session denied writes cannot spawn a child with writes by naming a wider role. Nesting inherits
   every denial.
2. `read` and `session` cannot be denied. A subagent that cannot read its files or keep its own todo
   list is not a subagent; `Downgrade::parse` refuses those by name.
3. Three readers of one fact, and they agree by construction: the tools of a denied class are not
   seated (`Registry::without_access`, after `resolve_role`), so the prompt claims no capability the
   gate would refuse; the backend is opened without them (no `Write` in a read-only view, no process
   host without `Exec`); and the ruleset carries a `deny` per denied tool, by class and by well-known
   name whether seated or not (`downgraded_ruleset`), which wins by opencode's last-rule so a tool
   registered outside the role later is refused rather than admitted for want of a rule. The disclosure
   names it: `downgrade: no write, no exec, no network`.

## 2. Placement: host or a VM

`where: host` is the parent's own boundary, whatever it is. `where: firecode` is a VM - firecracker by
default, libvirt/qemu when a device must be passed through - and the VM is the boundary: inside it the
mode is allow-all and no gate question is ever asked, because nothing it does reaches the host except
through doors the host holds. The downgrade composes on top; it is the caller's explicit ask.

**A VM's life is the turn's, so the turn does not end over a job still running in it.** A child that
backgrounds work and answers is HELD: the VM stays up, the job's settlement rings the child's bell, the
wake runs a turn that names the job's exit, and the VM comes down when nothing is left running and
nothing is left owed. A job killed anyway - the child stopped, the daemon shutting down, the turn
failed - is recorded and reported as `lost with its VM` and never as the exit its `firecode in` client
came back with, and the child's finish is not a `Done` over it: `task_result` answers
`TaskStatus::Killed` (an abstention) with the child's own words and the kill.

Placement never widens permissions, and a placement the box cannot honour is refused by name with
nothing spawned: `where: firecode` on a box with no `firecode` binary, or `host:NAME` for a machine the
operator never declared, is a failed call and not a silent fallback to the host. Running here and
calling it a VM is the boundary claim this design exists not to make.

`where: firecode` with `host:NAME` is one placement, a VM on that machine, spelled either
`--where firecode --host NAME` or `where: "host:NAME:vm"`. `FirecodeBackend` holds this box's paths, so
a guest elsewhere has no transport through them: the same client runs on the host over the session's ssh
link instead, where `firecode up` / `in` / `down` are ordinary remote commands. `docs/remote-hosts.md` §
*A VM on that machine* has the order; the placement is `Placement::RemoteVm`.

### The one child whose placement is not the caller's word: a reviewer

A reviewer has no caller - the review queue's own clock starts it - so its placement is a project
setting, `[queue] reviewers` in `leticode.toml` (and `--reviewers` on the daemon's command line, which
overrides it):

```toml
[queue]
reviewers = "host"   # the default - or "vm"
```

`host` is the default, and the reason is measured rather than aesthetic. A VM-placed reviewer paid a
guest exec per file read: `grep` did one `backend.read` per candidate, which over the firecode backend
is one `firecode in` each, so a repository-scale search was hundreds of round trips and minutes of a
held queue slot. And the subagents that AUTHOR these branches already run on the host, doing `cargo
test` in the operator's own tree with the operator's credentials, so placing only the reader in a VM
buys isolation its author never had.

Either way the reviewer gets a tree of its own at the head it judges: `start_reviewer` clones the
entry's repository at the row's `head_sha` (`<tmp>/letibot-review-clones/review-<row id>`), a VM is
booted on it, and a host reviewer works in it, so `read`, `grep`, `glob` and `bash` see the branch under
review and never the tree the operator has open.

What `host` gives up: a reviewer here is a `gatekeeper`-seated child with `bash`, and its calls are
cards that go to the review queue's head. That is a boundary on the operator's attention, not a
sandbox: it decides who is asked, not what a command may do once allowed. A project that wants the
boundary itself says `reviewers = "vm"` and pays the guest exec per read.

### On a Mac: what the VM holds

A VM-placed child in a git worktree - every `task_start` child - is booted on that worktree itself
(`firecode::mounts`), shared read-write with the main checkout read-only and the repository's `.git`
read-write, so its commits are on its branch as they happen and the backend never removes the project
(`mounted`). Anything else is copied, and on Linux everything is. A session placed with `--vm` boots its
VM on its workspace folder itself (`firecode up --mount-rw`, `Config::vm_mounts_workspace`), and a VM
already up with that folder mounted (firecode's `info --json` says `"mount": "rw"`) is joined rather
than doubled, so a joiner neither boots nor brings it down. `--vm-copy` restores the copy for the
session too.

### Holding a placed child to a check

`verify` with a VM is a shell command firecode runs in the child's project after the child finishes and
before its VM stops (`firecode up --verify`). Its recorded exit status - not anything the child says -
is put in the parent's answer beside the delivered path, as `ACCEPTANCE CHECK PASSED`, `FAILED` or
`GAVE NO VERDICT`. Refused without a VM, where nothing would run it. Setup on a Mac:
[firecode-on-macos.md](firecode-on-macos.md).

## 3. The firecode backend - what firecode actually offers

Measured on lab2x1 against a scratch project:

| door | measured |
|---|---|
| `firecode up --project P` | boots and waits: **12.4 s**. The guest gets a **copy** of `P` at the same path (`firecode-src`); the host tree is untouched |
| `firecode in --project P [--cwd D] 'cmd'` | **0.55 s** per command over vsock; one argument is a shell command (`bash -lc`), several are quoted words; the **exit status propagates** (3 -> 3, missing command -> 127); **stdout and stderr arrive merged**, through the terminal filter |
| `firecode cp host vm:/path`, `cp vm:/path host` | both ways, tar over vsock |
| `firecode down --project P` | waits for the copy-out and prints `work delivered: <path>` (a **sibling** `P-<stamp>`); `runs/<run>/copy_out` records the outcome. With `up --verify CMD` it runs `CMD` first and keeps the verdict in `runs/<run>/verify-status` |
| a `--cwd` the host made after boot | does not exist in the guest: no shared filesystem, the copy is as of boot |
| `firecode info --json` | the run id, VM ip, socket |

Two facts the backend is built on: a job is the `firecode in` client process under the session's
cgroup, so killing it kills the client and the guest command may run on until `down`; and `root_path`
is `None`, because a host path would make `path_is_inside` answer confidently about a filesystem it is
not looking at.

## 4. What is left

- A per-run `--cwd` created after boot needs `mkdir -p` in the guest first; the backend does that for
  `write`. Snapshots and checkpoints (`firecode checkpoint`, `vm_reset`) are not wired: a subagent gets
  a fresh boot, and the 12 s could become 60 ms with a warm checkpoint per project.
- The subagent's flowy voice: none, by design - it routes through its parent. Inside a VM, firecode's
  own `firecode-chat` exists and is not used here.
