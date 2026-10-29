<!-- abstract: MEASURED 2026-10-29 building the gate on a declared host: a landing worktree's `.git` is a FILE (so `[ -d tree/.git ]` is false for every worktree that exists), a thin `git bundle` DROPS a branch ref its prerequisite already covers (so `git fetch refs/heads/main` dies with "couldn't find remote ref"), and rsync must NOT be given a quoted destination — it protects its own remote args. -->

# Three things a gate on a declared host got wrong first, and each was silent until a real run

MEASURED 2026-10-29, branch `agent/the-gate-runs-on-a-declared-host` — `crates/harnessd/src/mergequeue/gate_host.rs`
(the gate's steps on a `hosts.toml` machine: rsync + a git bundle + `setsid` steps polled once a
second). All three were found by `crates/harnessd/tests/gate_host.rs` (a fake ssh that really runs
the commands) or by the first real run on lab2x1 — none of them by reading the code.

## 1. A git worktree's `.git` is a FILE, so `[ -d <tree>/.git ]` is false for every worktree

The host side keeps one worktree per repository and resets it each run, so the far-side script
chose between `worktree add` and `checkout/reset/clean` on whether the tree was already there:

```sh
if [ -d '<tree>/.git' ]; then cd <tree> && git checkout --detach --force …   # never taken
else git -C <src> worktree add --detach <tree> <head>; fi                    # always taken
```

The second gate of one repository then died on `fatal: '<tree>' already exists` — a host gate that
could only ever run **once**, which is the shape of a bug that a single-run test cannot see. `-e`.

## 2. A thin bundle DROPS a ref its prerequisite already covers

The bundle is incremental: `git bundle create f HEAD main --not <the tips the host already has>`.
When `main` is covered by a prerequisite, `git bundle create` omits `refs/heads/main` from the
bundle's heads — and a `git fetch f '+HEAD:refs/gate/tip' '+refs/heads/main:refs/heads/main'` then
dies with `fatal: couldn't find remote ref refs/heads/main`. Ask the bundle what it carries:

```sh
git bundle list-heads f        # `<sha> <refname>` per head, and nothing for the dropped ones
```

and build the refspecs from that list, not from the refs you asked for.

## 3. `rsync -e …` with a QUOTED destination path is a literal quote on the far side

`format!("{}:{}", dest, sq(path))` produced `rsync: [Receiver] mkdir "\'…\'" failed: No such file
or directory` — rsync hands its own remote command to the shell with the arguments protected
(`protect_args`, on since 3.0), so quoting again is one layer too many. Pass the path as it is.

## Two smaller things, measured on the way

* **`git bundle` records `HEAD`** when the rev you give it is `HEAD`, so `+HEAD:refs/gate/tip` is
  the refspec that puts the landing commit on a ref the worktree can be created from. A raw sha
  records nothing usable.
* **`~/.cargo/registry` on lab2x1 was already 1.4 GB**, so a first gate there did not have to fetch
  the index; the cost of a cold gate is COMPILATION, and it is per-repository
  (`<root>/gate/<repo>-<hex>/target`, outside both the worktree and the rsync'd tree so neither
  `--delete` nor `git clean -fd` can take it).
