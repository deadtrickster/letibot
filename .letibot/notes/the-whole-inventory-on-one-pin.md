<!-- abstract: Verifying a batch of rebased branches has three traps of its own: `cargo clean` takes no `--offline`, a shared `CARGO_TARGET_DIR` is not survivable by cleaning only the touched packages, and `cargo test | grep` reports grep's status. -->

# Twenty-five branches on one pin: nineteen rebased and verified, six already there

The branch refs this note recorded are gone; what survives is how a batch is verified.

`cargo clean` takes no `--offline` - the flag made the first clean a silent no-op, and every
worktree then compiled ANOTHER tree's rlibs, with the error naming a file the branch never
opened. It must also run IN the worktree, because it is about the manifest it is run under.

A shared `CARGO_TARGET_DIR` across worktrees is not survivable even with the touched packages
cleaned, because the stale package is often a dependency the branch does not touch. Clean every
first-party package; the third-party deps are what stay warm.

`cargo test … | grep` reports grep's status, so a check that had failed was written down as
`rc=0`.

`git cherry`'s `+` is not a measurement of "unlanded": a branch landed under a rewritten sha
keeps a different patch-id, so four of twenty-five were on main already. Prove it with
`git range-diff` or a patch comparison, not with `git cherry`.
