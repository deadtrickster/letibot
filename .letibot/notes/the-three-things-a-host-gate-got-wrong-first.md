<!-- abstract: A gate on a declared host got three things wrong first: a worktree's git entry is a FILE, a thin bundle drops a ref its prerequisite covers, and rsync protects its own remote arguments. -->

# Three things a gate on a declared host got wrong first, and each was silent until a real run

A git worktree's git entry is a FILE, so `[ -d ]` on it is false for every worktree that exists.
The far-side script in `crates/harnessd/src/mergequeue/gate_host.rs` chose `worktree add` every
time and the second gate of one repository died on `fatal: '<tree>' already exists` - a gate
that could run only once, which a single-run test cannot see. `-e`.

A thin bundle DROPS a ref its prerequisite already covers, so a refspec asking for it dies with
`couldn't find remote ref`. Ask the bundle what it carries (`git bundle list-heads`) and build
the refspecs from that list, not from the refs you asked for.

`rsync -e …` with a quoted destination is a literal quote on the far side: rsync protects its
own remote arguments (`protect_args`, on since 3.0), so quoting again is one layer too many.
Pass the path as it is.

Also: `git bundle create f HEAD` records `HEAD`, so `+HEAD:refs/gate/tip` is the refspec that
puts the landing commit on a ref a worktree can be created from. A raw sha records nothing
usable.
