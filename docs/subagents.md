# Subagents: downgrade, placement, and the firecode backend

What a `task` subagent is allowed, where it runs, and what it costs — as built
on 2026-09-14. `docs/leticode.md` has the one-paragraph version; this is the
reasoning and the measured numbers.

## 1. Inherit, downgrade, never upgrade

A subagent runs under its parent's permission ruleset (opencode's model,
`crates/tools/src/permission.rs`) with a `SubagentAdjudicator` that turns every
`ask` into a refusal — it has no operator to ask. `task` adds two arguments:

```
task(prompt, role?, access?, where?)
  role    any seat this build knows (coder default; researcher, planner, …);
          an unknown one is refused by name, not seated as coder
  access  read-only | no-write, no-exec, no-network (comma-separated)
  where   host (default) | firecode
```

`access` is a **Downgrade** (`letibot_tools::schema::Downgrade`): a set of
access classes denied *below* the role. Three properties, each a type rather
than a check:

1. **It only removes.** The child's downgrade is the **union** of the parent's
   own and the one asked for, so a session denied writes cannot spawn a child
   with writes by naming a wider role. Nesting inherits every denial.
2. **`read` and `session` cannot be denied.** A subagent that cannot read its
   files or keep its own todo list is not a subagent; `Downgrade::parse`
   refuses those by name.
3. **Three readers of one fact, and they agree by construction:**
   - the tools of a denied class are not seated (`Registry::without_access`,
     applied after `resolve_role`), so the prompt does not claim a capability
     the gate would refuse;
   - the backend is opened without them — a read-only view without `Write`,
     no process host without `Exec`, in both the confined and the unconfined
     (leticode) paths;
   - the ruleset carries a `deny` per denied tool — the seated ones by class
     and the well-known names whether seated or not (`downgraded_ruleset`) —
     which wins by opencode's last-rule, so a tool registered outside the role
     later is refused rather than admitted for want of a rule.

   The disclosure names it: `downgrade: no write, no exec, no network`.

A survey: `task(prompt: "map the crates and their seams", role: "researcher",
access: "read-only")`.

## 2. Placement: host or a VM

`where: host` is the parent's own boundary, whatever it is. `where: firecode`
is a VM — firecracker by default, libvirt/qemu when a device must be passed
through — and **the VM is the boundary**: inside it the mode is allow-all, no
gate question is ever asked, because nothing it does reaches the host except
through doors the host holds (§5 of the design brief: "no decision is made
because the action is inside"). The downgrade composes on top; it is the
caller's explicit ask and costs nothing to honour.

Placement never widens permissions. A subagent in a VM is still the parent's
ruleset minus the downgrade, inside a boundary.

Until the backend below existed, `where: firecode` was refused by name and the
subagent was **not** run on the host in its place — running here and calling
it a VM is the boundary claim this design exists not to make.

### The one child whose placement is not the caller's word: a reviewer

A `task` call says where its child runs. A **reviewer** has no caller — the review queue's own clock
starts it — so its placement is a project setting, `[queue] reviewers` in `leticode.toml` (and
`--reviewers` on the daemon's command line, which overrides it):

```toml
[queue]
reviewers = "host"   # the default — or "vm"
```

**`host` is the default, and the reason is measured rather than aesthetic.** A VM-placed reviewer
paid a guest exec per file read: `grep` did one `backend.read` per candidate, which over the
firecode backend is one `firecode in` each, so a repository-scale search was hundreds of round
trips and minutes of a held queue slot; one exec lost its request between `socat`'s `OK` and the
guest's first read, and both ends waited for ever. A review therefore took 30–60 minutes and held
one of two slots. And the symmetry is hard to argue with: the subagents that AUTHOR these branches
already run on the host — `task_start`'s children do `cargo test` in the operator's own tree with
the operator's credentials — so placing only the reader in a VM buys isolation its author never had.

**Either way the reviewer gets a tree of its own, at the head it judges**: `start_reviewer` makes
one detached clone of the entry's repository at the row's `head_sha`
(`<tmp>/letibot-review-clones/review-<row id>`), a VM is booted on it, and a host reviewer *works
in* it — its session workspace and the `cwd` its prompt names are that checkout, so `read`, `grep`,
`glob` and `bash` all see the branch under review and never the tree the operator has open.

**What `host` gives up, in one paragraph.** A reviewer on this machine is a `gatekeeper`-seated
child with `bash`, and nothing but the seat and the permission cards stands between it and the box:
it can run anything the operator can, read the operator's credentials and the whole filesystem, and
a branch under review may carry a test that does something destructive — run it and the damage is
done on the operator's own machine. The seat is read-only and its `bash` calls are permission
cards that go to the review queue's head (a card nobody answers times out and the review fails),
but that is a boundary on the operator's ATTENTION, not a sandbox: it decides who is asked, not
what a command may do once allowed. A project that wants the boundary itself says
`reviewers = "vm"` and pays the guest exec per read.

### A VM **on** another machine is not built, and the pair is refused by name

`--where firecode` and `--host NAME` are two placements, and this build makes one at a time
(R100, filed 2026-10-28 from the operator's *"how to run those remote letibots in
firecode"*). What the pair **should** mean is unambiguous — a VM on `NAME`: `firecode up`
driven over the session's own ssh link (a login shell, so `~/bin` is on the remote `PATH`),
the guest on `NAME`'s own `/dev/kvm`, the copy made from the remote checkout
`SshBackend::place` already arranges, and the commits coming back as
`remote/NAME/<short id>` the way every remote placement's do. `lab2x1` has all of that: the
CLI, the source in `~/Projects/firecode`, and `/dev/kvm` with an ACL for the operator.

What is missing is on this side, and it is not a wiring detail.
`letibot_tools::firecode::FirecodeBackend` runs a `firecode` binary **on this box** and holds
**this box's** paths: the CLI (`FirecodeSpec::bin`), the copy under
`~/.cache/letibot/firecode`, the scratch file a promoted job's output goes to, and the `.git`
file it reads to decide a worktree is mounted rather than copied. A guest on another machine
has no transport through any of those, so the combination needs a remote runner for the
firecode backend — not a flag.

Until it exists the pair is refused, by name, once the whole command line has been read:

```
$ leticode --host lab2x1 --where firecode
harnessd: `--host lab2x1` and `--vm` are two placements and this build can make one at a time. …
```

The refusal is order-independent, and that is the whole of the fix: the two flags used to
write one field, so `--host lab2x1 --where firecode` booted a VM **here** on a copy of this
box's workspace and `--where firecode --host lab2x1` ran on `lab2x1` with no VM at all — the
operator's other ask dropped without a word, and which one survived decided by argument
order. Running somewhere nobody asked for and saying nothing is the failure this refuses.

### `--vm`: the session's folder, mounted read-write

On a Mac a session placed with `--vm` (`--where firecode`) boots its VM on its workspace folder
itself, mounted read-write (`firecode up --mount-rw`, `Config::vm_mounts_workspace`): every
repository in it, nothing else of the host. Everything is allowed inside, as for any VM
placement. Its children attach to that VM by the folder's path, so the whole tree works in one
machine on one folder. A VM already up with the folder mounted (firecode's `info --json` says
`"mount": "rw"`) is joined rather than doubled: a joiner neither boots nor brings it down. A
child explicitly given a VM of its own, and a reviewer booted on its entry's repository, are
copied (or mounted as a worktree) as before; `--vm-copy` restores the copy for the session too.

### A child in a worktree is mounted, not copied

On a Mac a VM-placed child whose workspace is a git worktree - every `task_start` child - is
booted on that worktree itself (`firecode::mounts`): firecode shares it into the guest
read-write, with the main checkout read-only and the repository's `.git` read-write. Nothing is
copied under `~/.cache/letibot/firecode`, its edits and commits are in the worktree and on its
branch as they happen, and the backend never removes the project (`mounted`), since it is the
operator's own. Its children attach to the VM by the worktree's path (`firecode::project_for`).
Anything that is not a worktree is still copied, and on Linux everything is.

### Holding a placed child to a check

`task` / `task_start` take `verify` with `where: firecode`: a shell command firecode runs in the
child's project after the child has finished and before its VM stops (`firecode up --verify`). Its
recorded exit status - not anything the child says - is put in the parent's answer beside the
delivered path, as `ACCEPTANCE CHECK PASSED`, `FAILED` or `GAVE NO VERDICT`. Refused without
`where: firecode`, where nothing would run it. Setup and day-to-day use on a Mac:
[firecode-on-macos.md](firecode-on-macos.md).

## 3. The firecode backend — what firecode actually offers

Measured on lab2x1, 2026-09-14, against `~/Projects/firecode` (commit
`4e06cf9`) with a scratch project:

| door | measured |
|---|---|
| `firecode up --project P` | boots and waits: **12.4 s**. The guest gets a **copy** of `P` mounted at the same path (`firecode-src`); the host tree is untouched |
| `firecode in --project P [--cwd D] 'cmd'` | **0.55 s** per command over vsock; one argument is a shell command (`bash -lc`), several are quoted words; the command's **exit status propagates** (3 → 3, missing command → 127); **stdout and stderr arrive merged**, through the terminal filter (text only) |
| `firecode cp host vm:/path`, `cp vm:/path host` | both ways, tar over vsock; extraction uses Python's `data` filter |
| `firecode down --project P` | waits for the copy-out and prints `work delivered: <path>` (a **sibling** `P-<stamp>`); `runs/<run>/copy_out` records it as `pending`, then `done <path>`, `none` or `failed …`. With a gate (`up --verify CMD`) it first runs `CMD` in the project and prints the verdict, also kept as `runs/<run>/verify-status`. `runs/<run>/` holds `console.log`, `result`, `project`, `cgroup`, `vsock` |
| a `--cwd` the host made after boot | does not exist in the guest: no shared filesystem, the copy is as of boot |
| `firecode info --json` | the run id, VM ip, socket |

Consequences the backend is built on:

- **Files are the guest's.** `read`/`write`/`list`/`stat` go over vsock. A
  read is `base64 -w0 < path` decoded on the host (binary-safe through the
  text filter); a write is `cp` to a temp name in the guest then `mv -f` —
  atomic where the trait requires it. Every file call is one 0.5 s round trip.
- **Processes are host-side clients.** A job is the `firecode in` client
  process, spawned by the existing `HostProcesses` under the session's cgroup
  with a `FirecodeConfinement` whose `wrap` is `firecode in --project P --cwd
  D` and an empty shell, so the job's command string reaches the guest as one
  `bash -lc` command. Jobs, scopes, monitors, promotions and the reap log come
  from the host implementation unchanged. What differs: killing a job kills
  the client; the guest command may run on until `down`, and `describe` says
  so.
- **The VM's lifetime is the backend's.** `FirecodeBackend::up` boots it;
  dropping the backend (the subagent harness ending) runs `down`. The sibling
  directory is where the subagent's writes are, and its result to the parent
  names that path — firecode's own contract ("nothing is applied for you;
  diff it and take what you want").
- **No host root.** `root_path` is `None`, as the trait's own doc says a
  firecode guest must be: a host path would make `path_is_inside` answer
  confidently about a filesystem it is not looking at.

## 4. What is left

- A per-run `--cwd` created after boot needs `mkdir -p` in the guest first;
  the backend does that for `write`.
- Snapshots/checkpoints (`firecode checkpoint`, `vm_reset`) are not wired: a
  subagent gets a fresh boot. The 12 s could become 60 ms with a warm
  checkpoint per project; that is the next measurement.
- The subagent's flowy voice: none, by design — it routes through its parent.
  Inside a VM, firecode's own `firecode-chat` exists and is not used here.
