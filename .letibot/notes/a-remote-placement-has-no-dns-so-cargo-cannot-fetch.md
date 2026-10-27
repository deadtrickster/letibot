<!-- abstract: MEASURED 2026-10-27 on lab2x1 in a remote placement: the bash boundary has no /etc/resolv.conf and no /etc/hosts, so `cargo fetch` of a new git rev fails on DNS ("failed to resolve address for github.com"); a re-pin needs the rev already in /home/dead/.cargo/git/db, which is the only cargo home this box has. -->

# A remote placement's shell has no DNS, so a new rev cannot be fetched

MEASURED 2026-10-27, worktree `wt-r1b` placed on lab2x1 (`HOME=/home/dead/letibot-remote/.letibot-home/<n>`,
repo `/home/dead/letibot-remote/wt-r1b.git`, worktree 64948000 on branch `remote/64948000`).

## The measurement, in three lines

```sh
$ cargo fetch                    # re-pinning rano to dfcadb0
  unable to update https://github.com/deadtrickster/rano?rev=dfcadb0…
  Caused by: failed to resolve address for github.com: Temporary failure in name resolution
$ getent hosts github.com        # nothing, for github.com or 1.1.1.1
$ ls /etc/resolv.conf /etc/hosts # neither file exists in this view
```

`ip addr` shows `eno2` UP with a LAN address, so this is not "the machine is offline" — it is that the
**sandbox's `/etc` has no resolver configuration at all**, so glibc has nowhere to ask. The brief that
allowed "ONE `cargo fetch`" assumed the box could reach github; it cannot from inside the boundary.

## What that costs

Every cargo command takes `--offline`, which is fine for the tree's own crates but fatal for a **re-pin**:
`cargo` resolves a `git` + `rev` dependency from `/home/dead/.cargo/git/db/<name>-<hash>`, and that store
held `227b53e` and the revs before it and no others. So a task whose first step is "bump rano to a rev
that is not on this box" cannot start here at all — the checkout's API cannot even be READ, let alone
compiled against.

Two ways out, both the operator's: put the rev in `/home/dead/.cargo/git/db` (a `git fetch` from a
machine with DNS into that bare repo is enough — cargo finds a commit already present), or place a rano
checkout on the box and use a path dependency while building.

## The rake

Before promising a build against a rev, ask the box: `git -C /home/dead/.cargo/git/db/<name>-* cat-file -t <rev>`.
`fatal: could not get object info` means no fetch, no build, and a blocked task — and it is one command.

## UPDATE 2026-10-27 (wt-r2): the rev WAS in the db, and the next wall was the read-only `~/.cargo`

The note above ends *"put the rev in `/home/dead/.cargo/git/db`"* — and for rano `bf3fa48` somebody
had: `git -C /home/dead/.cargo/git/db/rano-* cat-file -t bf3fa48…` answers `commit`. A re-pin still
failed, one step later, with a different sentence:

```
failed to make directory '/home/dead/.cargo/git/checkouts/rano-…/bf3fa48': Read-only file system
```

`touch /home/dead/.cargo/git/checkouts/probe` → the same. **`~/.cargo` is mounted READ-ONLY in this
placement**, so a rev that is in the object store still cannot be *checked out*: the db is read, the
checkout directory is written, and there is nowhere to write it. (`~/.cargo/registry` is fine to
read under `--offline`; nothing else in the placement writes there.)

### What works: a session-local `CARGO_HOME` with the read-only halves symlinked

```sh
SESS=$HOME                       # /home/dead/letibot-remote/.letibot-home/<n>
mkdir -p $SESS/cargo-home/git/checkouts
ln -sfn /home/dead/.cargo/registry   $SESS/cargo-home/registry
ln -sfn /home/dead/.cargo/git/db     $SESS/cargo-home/git/db
export CARGO_HOME=$SESS/cargo-home
```

`cargo metadata --offline` then checks `bf3fa48` out under `$SESS/cargo-home/git/checkouts/rano-…/`
from the local db (no network), and every later `cargo build/test --offline` finds it. The registry
symlink is read-only and enough: `--offline` never extracts anything (`registry/src` is already
populated, 1.2 GB, and is read). `~/.cargo/bin` does not need to be in the new home — `PATH` still
points at it.

**Every cargo command in the session needs that export** beside `CARGO_TARGET_DIR`, or the second
command re-resolves against the read-only home and fails with the mkdir sentence again.
