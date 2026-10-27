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
