<!-- abstract: `HostProcesses::new` needs a delegated cgroup v2 subtree, which a confined session does not have, so every spawn-based `exec::host` test skips here and reads as passed; `HostProcesses::with_tree(root, PlainTree::over(..))` spawns for real without one, and bwrap cannot nest at all in this session. -->

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
