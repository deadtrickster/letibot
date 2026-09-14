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

## 3. The firecode backend — what firecode actually offers

Measured on lab2x1, 2026-09-14, against `~/Projects/firecode` (commit
`4e06cf9`) with a scratch project:

| door | measured |
|---|---|
| `firecode up --project P` | boots and waits: **12.4 s**. The guest gets a **copy** of `P` mounted at the same path (`firecode-src`); the host tree is untouched |
| `firecode in --project P [--cwd D] 'cmd'` | **0.55 s** per command over vsock; one argument is a shell command (`bash -lc`), several are quoted words; the command's **exit status propagates** (3 → 3, missing command → 127); **stdout and stderr arrive merged**, through the terminal filter (text only) |
| `firecode cp host vm:/path`, `cp vm:/path host` | both ways, tar over vsock; extraction uses Python's `data` filter |
| `firecode down --project P` | returns in 0.07 s; the VM's copy of the project lands in a **sibling** `P-<stamp>` a few seconds later; `runs/<run>/` holds `console.log`, `result`, `project`, `cgroup`, `vsock` |
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
