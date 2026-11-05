<!-- abstract: `HostProcesses::new` needs a delegated cgroup v2 subtree and bwrap cannot nest in this session at all, so spawn and boundary tests either skip (and read as passed) or, on the wide-seat path, FAIL: `harnessd/tests/clear_context.rs` is red from inside a seat on any tree, so a whole-workspace gate needs `--no-fail-fast` and a per-failure judgement. -->

# A host test that needs a cgroup skips here, and the plain tree is how to measure anyway

MEASURED 2026-11-04 in a confined session on lubuntu1 (`wt-z-fix-tools`).

`letibot_tools::host_tree()` is `Cgroup2::probe()`, and inside a boundary there is no writable
cgroup v2 subtree. So `apparatus::present("a process-lifetime tree ...", host_tree().is_ok())` is
`None`, and every `exec::host` test that spawns through `HostProcesses::new` prints `SKIPPED ...
THIS IS NOT A PASS` and returns - which libtest counts as **ok**. A run that says `1444 passed`
can have measured nothing in that file. Read the `--nocapture` output before believing one.

`HostProcesses::with_tree(root, PlainTree::over(root.join("tree")))` (the stub in `exec::host`'s
test module) spawns for real without a cgroup: the join script writes `$$` into a plain file, and
`members` reads it back. Two things it cannot do: `end` signals nothing (a fixture's process is
left to die on its own), and `kill_job_as` therefore cannot end a job. A test that needs a job
*killed* - the token file, the reaping - must either use the plain tree with a command that ends
by itself, or go to a box with a cgroup.

bwrap cannot nest here either: `unshare -U true` fails with `No space left on device`
(`kernel.apparmor_restrict_unprivileged_userns`), so `confine::probe` refuses and every
`tests/confine.rs`, `tests/git_child.rs` and seatbelt measurement skips. The part of a bwrap
change that can be measured on this box is the ARGV: `build_argv_with` is private-but-testable
from `confine.rs`'s own tests, and asserting the flag sequence there is the honest instrument.

A fake-ssh fixture (`tests/support/fake_remote.rs`, `Fake` in `tests/remote_sweep.rs`) runs the
command locally, so its jobs are real processes and its ledger is a real directory: the remote
tests that look impossible do measure, and they are the ones that caught the ledger's move.

Correction, 2026-11-04 later the same day: the `No space left on device` is the DEPTH of the
daemon's own namespace chain and not AppArmor. `kernel.apparmor_restrict_unprivileged_userns` is 1
here, but `/proc/sys/user/max_user_namespaces` reads 2147483647, so a 0 limit is not the cause
either: the kernel refuses a user namespace whose parent is already at its nesting limit, and the
daemon this session hangs off is deep. See
`a-bwrap-mask-needs-a-mount-point-that-is-already-there.md`.

## Some boundary tests do not skip - they FAIL (MEASURED 2026-11-05, wt-z-t10)

`crates/harnessd/tests/clear_context.rs`'s `a_childs_clear_is_answered_by_its_own_thread` sets
`Seat::Leticode` and `allow_bash`, and on Linux that seat asks for the WIDE seat, so `Harness::open`
builds a boundary and refuses with `Setup(...)` when it cannot:

```
bwrap: Creating new namespace failed: nesting depth or /proc/sys/user/max_*_namespaces exceeded (ENOSPC)
thread '...' panicked at crates/harnessd/tests/clear_context.rs:84:14: the parent opens: Setup("this session asks for the wide seat and no boundary could be built for it: ...")
```

It reaches no `apparatus::present` skip, so from inside a seat it is RED on every tree - proved
cheaply by the same command the test needs: `/usr/bin/bwrap --dev-bind / / -- /bin/true` exits 1
with that message in this shell, and `/proc/sys/user/max_user_namespaces` reads 2147483647, so it
is the daemon's nesting depth and not a limit.

Two consequences for running the gate from a seat: `cargo test --workspace` stops at the FIRST
failing target, so the crates after it never run - use `--no-fail-fast` and read every
`test result: FAILED` line; and a gate verdict from here cannot be "green", only "every failure is
one of the two known classes (no cgroup, no nesting)".
