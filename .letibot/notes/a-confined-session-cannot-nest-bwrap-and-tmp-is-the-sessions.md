<!-- abstract: MEASURED 2026-10-27 (FX, lab2x1, tools inside bwrap): no user namespace can be created from inside a confined session's boundary — `unshare -U true` is ENOSPC and nested `bwrap` fails the same way — so every test that spawns a real bwrap or a real process host skips there and only the merge gate measures it. Plus the /tmp decisions FX landed: a per-session directory bound at /tmp, a subdirectory of the daemon scratch for a reason. -->

# A confined session cannot nest `bwrap`, and `/tmp` is now the session's

MEASURED 2026-10-27 on lab2x1, branch `remote/61585000` (the migrated `wt-fx`), with this
session's tools running inside a bwrap boundary.

## No user namespace can be made from inside a boundary

```sh
$ unshare -U true
unshare: unshare failed: No space left on device
$ bwrap --unshare-user --unshare-pid --dev /dev --proc /proc /bin/echo hi
bwrap: Creating new namespace failed: nesting depth or /proc/sys/user/max_*_namespaces exceeded (ENOSPC)
$ cat /proc/sys/user/max_user_namespaces
2147483647          # so the *counter* half of bwrap's sentence is a red herring
```

This is the boundary doing its job — `--disable-userns --assert-userns-disabled` is what stops a
sandboxed child from `unshare`ing back out (`confine.rs`'s `SealKind::NestedNamespaces`).

**The consequence is a testing one, not a corner case.** From such a session, no test that spawns
a real `bwrap` can pass, and neither can one that opens a real `HostProcesses` (no delegated
cgroup v2 either). They all take their `no_boundary` / `apparatus::present` branch, which is
*loud and green*, so a run inside the boundary proves nothing about them. Anything in this shape
must be run on the merge gate; saying "the boundary tests are green here" about a session seated
like this is saying nothing. Same reason `crates/tools/tests/confine.rs`'s suite and the bridge's
`end_to_end`/`no_spin` report skips here.

## `/tmp` inside a boundary is the SESSION's, not the command's

Before FX it was `--tmpfs /tmp`: a fresh filesystem per command, so `echo x > /tmp/p` in one call
and `cat /tmp/p` in the next answered `No such file or directory` with nothing naming the
boundary. `TmpView::Session(dir)` (`crates/tools/src/exec/confine.rs`) binds `dir` at `/tmp`
instead, and both renderings share that one builder (`pieces_of`).

Where the directory is, and who removes it:

| where | path | made | removed |
|---|---|---|---|
| local bwrap | `<daemon scratch>/tmp/<session id>` | `Bwrap::probe` | `HostBackend::close` |
| remote bwrap | `<root>/.letibot-home/<short>/tmp` | `SshBackend::place`'s `mkdir`, beside the home | never — the home is not removed either |

The daemon scratch is `<XDG_RUNTIME_DIR>/letibot/scratch-<pid>` (`harness::scratch_dir`), one per
DAEMON. **The session's `/tmp` is a subdirectory of it and must be**: bound at `/tmp`, the scratch
itself would put `job-<id>.out` and a promoted run's output inside the sandbox, where a plain
`rm -rf /tmp/*` deletes them. `/var/tmp` and `/run` stay fresh tmpfs per command — nothing in an
agent's idiom writes `/var/tmp`, and its FHS meaning (*survives a reboot*) is one no sandbox
directory may claim.

**And a second probe per confined session open** is the cost: the argv is built once at probe time
so that what was measured is what runs, so giving a session a `/tmp` after construction is a new
`ViewSpec` and a new `Bwrap::probe` — the same rule `HostBackend::grant_into_view_impl` follows.

**The half that does not follow**: the model's own `read` tool cannot open the file. `/tmp/p`
inside the view is `<scratch>/tmp/<session>/p` on the host, and `read` takes host paths — so a
command's `/tmp` scratch is for the command, not for reading back into the transcript. That is
what the scratch dir and `job_output` are for.
