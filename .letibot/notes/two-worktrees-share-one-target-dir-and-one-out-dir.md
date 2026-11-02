<!-- abstract: With `CARGO_TARGET_DIR` pointing at the main checkout's target, two worktrees share a build-script OUT_DIR and a test binary's name, so a `cargo test` can run another tree's binary. -->

# Two worktrees, one `CARGO_TARGET_DIR`: a build script's OUT_DIR and a test binary's name are shared

`letibot-builtins`' build script emits ABSOLUTE `include_str!` paths, so a build in one worktree
can leave the other's `out/table.rs` in place and a binary silently carries the other tree's
layers. A test asserting that a built-in layer is installed then fails for a reason that is not
in the diff.

The test binary is replaced between two runs as well: `cargo test -p letibot-tui --lib` passed
with a just-edited assertion and the next run of the same command failed with the OLD one.

Check which tree you are measuring:
`grep name: <target>/debug/build/letibot-builtins-*/out/table.rs` lists the layers the next
build will see, and `head -1 <that dir>/output` names the worktree that last ran the script
there.

Fix: `CARGO_TARGET_DIR=$PWD/target cargo test …`. The merge gate already runs every step that
way (`GATE_TARGET_DIR`), which is why a landing is judged by artifacts of the landing. Rule: a
`cargo test` result on this box is evidence about a build directory, not about a worktree.
