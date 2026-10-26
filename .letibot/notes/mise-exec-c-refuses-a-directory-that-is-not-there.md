<!-- abstract: MEASURED 2026-10-26 (RH6, from the Linux gate): `mise exec -C <dir>` REFUSES a directory that does not exist, so pointing the wrapper at the checkout before `git worktree add` made it failed every placement on every host with mise — and mise prints `$HOME` as `~` in that error, which reads like a quoting bug and is not one. -->

# `mise exec -C` refuses a directory that is not there, and its error says `~`

MEASURED 2026-10-26 on lab2x1 (the orchestrator's Linux gate), then reproduced on the Mac with a
stub. `crates/harnessd/tests/remote_host.rs`'s placement failed with:

    Setup("io: placing this session on fake: io: making the mirror on fake: exit 1:
     mise ERROR Directory specified with --cd does not exist:
     ~/letibot-remote/the-repo/01abcdef")

## Two facts, and the second is the trap

1. **`mise exec -C <dir>` exits 1 when `<dir>` is not a directory.** There is no "run it where I am"
   fallback. `mise exec -C /tmp/definitely-not-here -- true` on this Mac (2026.10.3) prints
   `mise ERROR Directory specified with --cd does not exist: /tmp/definitely-not-here` and exits 1.
2. **mise abbreviates `$HOME` to `~` in that message** — with `HOME=/tmp` it printed
   `~/definitely-not-here-xyz` for a path it was given as `/tmp/definitely-not-here-xyz`. So the
   gate's `~/letibot-remote/...` was mise's own rendering of an **absolute** path, not a tilde that
   was passed through quoting. The obvious diagnosis ("the wrapper hands over a quoted `~`") is
   wrong, and chasing it wastes a session.

## What it broke

`SshBackend::place` set `mise exec -C`'s directory to the checkout the moment it could be spelled —
before `git worktree add` had made it — so on every machine whose toolchain comes from mise (all
four of the operator's) the **first step after that** (`making the mirror`) died. Nothing about it
is visible on a box without mise: the wrapper is skipped entirely, and the fake-ssh tests all pass.

The fix is two rules that now live in the code:
* the wrapper points at the host's `root` (which `place` has already `mkdir -p`ed) until the
  checkout exists, and at the checkout afterwards;
* `dir_arg()` expands a leading `~` the way `path` entries are (`one_entry` → `"$HOME/…"`), so no
  caller can reintroduce a literal tilde for mise to resolve against the wrong directory.

## The test that catches it, and why the earlier ones could not

A mise stub that **validates its `-C`** — `[ -d "$3" ] || exit 1` — planted at
`$HOME/.local/bin/mise` in the fake remote's home, with no `mise` line in `hosts.toml`, so the
default probe finds it and every step of a placement goes through it
(`remote_hosts.rs`'s `a_placement_through_a_mise_that_insists_its_directory_exists`). Verified to
fail before the fix with the gate's exact sentence. The stubs written earlier in the day all did
`shift 4; exec "$@"` — they checked the argv *shape* and never whether the directory was usable,
which is the difference between "the wrapper is spelled as documented" and "the wrapper works".

Fixtures on a box that has mise must also say `mise = false` (the harnessd remote fixture now does):
a fake ssh inherits this process's `PATH`, so the default probe finds the developer's own mise
behind it.
