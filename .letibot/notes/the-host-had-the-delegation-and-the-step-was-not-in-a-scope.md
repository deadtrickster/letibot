<!-- abstract: MEASURED 2026-10-30: lab2x1 needed NO delegation — a gate step's cgroup is the ssh session's scope (`session-79934.scope`, `writable=no`) while `systemd-run --user --scope -p Delegate=yes` answers 0 there, so the 21-target apparatus red was "the step was not in a scope of its own", not a missing grant; and `gate_on_host` is the unconfined channel to a host while a subagent's tools there run inside bwrap. -->

# The host had the delegation; the step was not in a scope of its own

MEASURED 2026-10-30 on `lab2x1`, branch `agent/an-apparatus-the-host-has-not-got`, from the merge
gate's own failure at `main` `7becd144` — 21 targets red on one sentence, *"LETIBOT_REQUIRE_APPARATUS
is set, and a cgroup v2 subtree this user may write (the process-lifetime tree) is not on this
machine"*.

## What the machine actually looks like, from the gate's own ssh session

```
cgroup=0::/user.slice/user-1000.slice/session-79934.scope
controllers=cpu memory pids
writable=no                                # -> Cgroup2::probe's mkdir fails
XDG_RUNTIME_DIR=/run/user/1000
/usr/bin/systemd-run
systemd-run --user --scope -p Delegate=yes --quiet -- true   -> rc=0
```

So there is **nothing for the operator to grant**: a transient delegated scope works. What was
missing is that a gate step runs in the ssh SESSION's scope, and a session scope is not writable by
the user. Wrapping the step in `systemd-run --user --scope -p Delegate=yes` fixes it, and the
apparatus comes back with the demand intact:

| step, on the host, prefix on | before | wrapped |
|---|---|---|
| `-p letibot-tools --test confine` | 2 passed, 23 failed (19 boundary, 4 cgroup) | **25 passed, 0 failed** |
| `-p letibot-harnessd --lib` | 782 passed, 17 failed (16 apparatus + 1 dependent) | **806 passed, 0 failed** |

And the boundary comes back too: inside a fresh session scope, nested `bwrap` stops failing with
`nesting depth … exceeded (ENOSPC)` — the reason `a-confined-session-cannot-nest-bwrap` sees no
boundary at all is the session it runs in, not the machine.

## `gate_on_host` is the unconfined channel; a subagent on a host is not

A `task { where: "host:lab2x1" }` child's `bash` runs **inside a bwrap boundary** — no `/sys`
mounted, `/proc/self/cgroup` = `0::/`, `$HOME` a per-session dir, `unshare -U` ENOSPC — so every
"what does this machine have" question asked through one is answered about the boundary. Measured
twice before I noticed. The channel that IS the machine is the tree's own example:

```sh
cargo run --offline -p letibot-harnessd --example gate_on_host -- lab2x1 <tree> -- "<step>" "<step>"
```

It runs its steps over plain ssh, in exactly the environment the gate's steps get, prints each row's
output and the verdict, and touches no store and no lease. That is how every number above was taken.

## Two rakes under the fix

* **`Cgroup2::probe` needs only a WRITABLE parent cgroup, not "alone".** It tries
  `delegate_memory()` first (alone + `memory` in `cgroup.controllers` + writable) and falls back to
  `probe_under(own_cgroup())`, which is a bare `mkdir`. So a probe that mirrors `delegate_memory`
  over-reports absence; the honest probe is *make a directory in my own cgroup and remove it*.
* **The step counter in `gate_host` is shared with the bundle's temp name** (`send_bundle` calls
  `self.next()`), so a COLD gate's first step is `step-1.sh` and a warm gate's is `step-0.sh`. A
  test that reads the launcher by number must index the sorted files, not assume 0.

## And the classifier's shape

A `LETIBOT_REQUIRE_APPARATUS` refusal is a panic whose message is on the line AFTER `panicked at`,
and the gate's step runs with `-- --nocapture`, so nothing keeps them adjacent. The rule that works
is a count: `matches("panicked at").count() <= matches("is not on this machine").count()`, plus no
`error[`/`error: could not compile`/`Diff in `. The measured `-p letibot-harnessd --lib` run is the
case that proves the count matters — 17 panics, 16 refusals, one genuine failure, and that one must
stay RED.
