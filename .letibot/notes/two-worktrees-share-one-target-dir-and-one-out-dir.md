<!-- abstract: MEASURED 2026-10-28: with this box's CARGO_TARGET_DIR pointing at the main checkout's target, two worktrees share a build-script OUT_DIR and the same test-binary hash — a worktree's `cargo test` ran another tree's test binary, and `letibot-builtins`' table named another tree's plugin.toml. Use `<worktree>/target`; the gate already does. -->

# Two worktrees, one `CARGO_TARGET_DIR`: a build script's OUT_DIR and a test binary's name are shared

MEASURED 2026-10-28 in `worktrees/agent-plugin-command-feeds` while another session built in
`worktrees/agent-fold-default-tests`. This box exports `CARGO_TARGET_DIR=/Users/dead/Projects/letibot/letibot/target`
(the main checkout's), and a worktree that just runs `cargo test` inherits it.

## What happened, twice, and both are silent

1. **`letibot-builtins`' build-script OUT_DIR was written by the other worktree.** That crate's
   `build.rs` globs `plugins/builtin/*` and emits `include_str!`/`include_bytes!` of the manifests and
   modules it found — with ABSOLUTE paths. A `cargo build -p letibot-builtins` in one worktree can
   leave the other's `out/table.rs` in place, so a binary silently carries **the other tree's layers**:
   a table listing only `sessions` (that tree predates `plugins/builtin/todo`), with
   `include_str!("/…/worktrees/agent-fold-default-tests/plugins/builtin/sessions/plugin.toml")` in it.
   A test asserting *the built-in `todo` layer is installed* then fails for a reason that is not in
   the diff. Read the file to know which tree you are measuring:
   `grep name: target/debug/build/letibot-builtins-*/out/table.rs`, and `head -1 <that dir>/output`
   names the worktree that last ran the script there.

2. **The test binary was replaced between two runs.** `cargo test -p letibot-tui --lib` passed with a
   just-edited assertion, then the *next* run of the same command failed with the OLD assertion
   (`right: ["p1", "b-sessions"]`, the pre-edit value) — the other worktree's build had written
   `deps/letibot_tui-<hash>` for the same hash in between. **A green run is not proof you measured
   your own tree** when the target dir is shared.

## The fix, and it is one env var

```sh
CARGO_TARGET_DIR=$PWD/target cargo test --offline -p letibot-tui
```

A worktree's own target dir. The merge gate is already immune: `AGENTS.md` says every gate step runs
with `CARGO_TARGET_DIR` at `<worktree>/target` (`mergequeue.rs`'s `GATE_TARGET_DIR`), which is why a
landing is judged by artifacts of the landing. Cost of the private dir: one cold build (~2.3 GiB, a
few minutes for `letibot-tui` on the Mac), and `target/` is gitignored so the worktree stays clean.

## The rake

**A `cargo test` result on this box is evidence about a build directory, not about a worktree.** When
another session may be building the same package, run with a private `CARGO_TARGET_DIR` before
believing either a red or a green — and check `build/letibot-builtins-*/out/table.rs` when a built-in
layer is the subject.
