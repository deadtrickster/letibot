<!-- abstract: MEASURED 2026-10-27: two worktrees sharing CARGO_TARGET_DIR reuse ONE build-script OUT_DIR for `letibot-builtins`, so a `cargo test` can link the built-in table of a DIFFERENT worktree — the same path named `agent-fold-default-tests` and then `agent-plugin-tool-mount`. -->

# The built-in table's OUT_DIR is shared between worktrees, and a test can link the wrong one

MEASURED 2026-10-27 in `agent-plugin-tool-mount`, adding `plugins/builtin/claude_transcript` (package B).
`CARGO_TARGET_DIR` on this box is the main tree's (`/Users/dead/Projects/letibot/letibot/target`), and every
worktree shares it.

## What was seen

`cargo test -p letibot-harnessd --lib plugins::tests::the_builtin_transcript_layer…` failed with *"the
built-in answers `tool_claude_transcript`"* — the layer was not on the stack — although the guest compiled
for wasm and the build script's `guests()` plainly picks up any directory under `plugins/builtin` with a
`Cargo.toml` and a `plugin.toml`.

The evidence, one command:

```
$ find …/target/debug/build -name table.rs -path "*builtins*" | xargs grep -o 'include_str!("[^"]*"'
== …/letibot-builtins-b19fbbe4dba69160/out/table.rs
include_str!("…/worktrees/agent-plugin-command-feeds/plugins/builtin/sessions/plugin.toml")
include_str!("…/worktrees/agent-plugin-command-feeds/plugins/builtin/todo/plugin.toml")
== …/letibot-builtins-bc6564b655a2e49a/out/table.rs
include_str!("…/worktrees/agent-fold-default-tests/plugins/builtin/sessions/plugin.toml")
```

**No `table.rs` named this worktree at all** — so the test binary was linking a `letibot-builtins` rlib
whose `include!`d table came from a sibling worktree, and a new built-in was simply not in it.

## What fixed it

`touch crates/builtins/build.rs` and rebuild: the SAME path
(`letibot-builtins-bc6564b655a2e49a/out/table.rs`) then named `…/worktrees/agent-plugin-tool-mount/` and
both of its layers. So the OUT_DIR is keyed by something that does not include the worktree, and whichever
build script ran last wrote it.

## The rake

* **A built-in that "does not appear" may be another worktree's table**, not a defect in `build.rs` or in
  `guests()`. Before believing a test that a built-in is absent, print the table the build produced:
  `grep include_str! $(find $CARGO_TARGET_DIR -name table.rs -path "*builtins*")`.
* **`cargo build -p letibot-builtins` can answer `Fresh` about another worktree's work.** `touch
  crates/builtins/build.rs` is the cheapest way to force it.
* **A private `CARGO_TARGET_DIR` is the reliable check** when adding or debugging a built-in: with
  `CARGO_TARGET_DIR=<scratch>/tgt cargo build -p letibot-builtins --offline`, the generated `table.rs` is
  certainly this worktree's (that is how the new layer was confirmed to be picked up at all).
* The same sharing means a `cargo test` run in one worktree can be red or green because of another
  session's build. That is not a reason to distrust the code; it is a reason to name the table before
  concluding anything about it.
