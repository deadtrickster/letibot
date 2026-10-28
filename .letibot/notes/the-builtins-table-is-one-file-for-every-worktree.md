<!-- abstract: MEASURED 2026-10-28: `crates/builtins/build.rs` writes `OUT_DIR/table.rs` under the same build-dir hash for every worktree, so whichever tree built last decides what `letibot_builtins::table()` holds — a sibling's `plugins/builtin/todo` made `crates/tui`'s layer assertion red here for a reason in no diff. -->

# The built-ins table is ONE file shared by every worktree, and the last build wins

MEASURED 2026-10-28 in `agent-fold-default-tests` (the tui package, running
`cargo test -p letibot-tui` on `c4c1ebdd`), while every failing test named in the brief
was being fixed.

## The symptom, and why it reads as somebody else's change

`crates/tui`'s `sessions_override::a_projects_sessions_layer_renames_a_group_and_hides_its_rows`
was red with

```text
left:  ["p1", "b-todo", "b-sessions"]
right: ["p1", "b-sessions"]
```

— a `b-todo` built-in layer that **does not exist in this tree**: `ls plugins/builtin/`
gives `abi` and `sessions` only, and the layer lives in the sibling worktree
`~/Projects/letibot/worktrees/agent-plugin-command-feeds/plugins/builtin/todo`. It was
red alone as well as under the whole suite, so not a flake and not ordering.

## The mechanism

`crates/builtins/build.rs` generates `OUT_DIR/table.rs` from `<repo>/plugins/builtin`
and emits `cargo:rerun-if-changed=<absolute path in THIS worktree>` for the tree, each
layer's `plugin.toml`/`Cargo.toml`/`Cargo.lock`/`src` and itself. Two worktrees on this
box share the main tree's `target/` as the build cache (AGENTS.md), and the build-dir
hash is the same in both:

```sh
$ ls -d ~/Projects/letibot/letibot/target/debug/build/letibot-builtins-*/out/table.rs
…/letibot-builtins-bc6564b655a2e49a/out/table.rs   # ONE file, both worktrees
```

So the fingerprint that decides *rerun or not* is the one the LAST build wrote, and its
`rerun-if-changed` paths name the OTHER worktree — which has not changed — so the next
build in this worktree reuses the other tree's table. The reverse holds the moment the
sibling builds: its `todo` layer disappears from their run.

## How to tell, in two greps

```sh
$ grep -o 'name: "[a-z-]*"' ~/Projects/letibot/letibot/target/debug/build/letibot-builtins-*/out/table.rs
name: "sessions"      # the layers the next build will see
$ grep -o 'worktrees/[a-z-]*' ~/Projects/letibot/letibot/target/debug/build/letibot-builtins-*/out/table.rs
worktrees/agent-fold-default-tests   # the tree it was generated FOR
```

When those two disagree with `ls plugins/builtin` and `pwd`, the table is stale. The
recovery is one rerun trigger — `touch crates/builtins/build.rs`, then rebuild (or
`cargo clean -p letibot-builtins`), which regenerates it for THIS tree and silently
leaves the other tree needing the same.

## The tree-level half, which is not the cache's fault

`crates/tui/src/app/tests/sessions_override.rs:72-79` asserts the built-in layer ids as
an exact `vec!["p1", "b-sessions"]`, so **a package that adds `plugins/builtin/<name>`
has to change that assertion in the same change** — otherwise a green branch of theirs
turns this file red when it lands. `sessions_way_out.rs` installs the same table and
asserts nothing about the ids, so it is the one place.

## The worse half: the test BINARY is shared too, so a local green can be another tree's

MEASURED 2026-10-28, same worktree, `git status --porcelain` empty, the same command
three times in a row:

```text
$ cargo test -p letibot-tui --lib
test result: ok. 1015 passed; 0 failed; 5 ignored
test result: ok. 1018 passed; 0 failed; 5 ignored
test result: ok. 1016 passed; 0 failed; 5 ignored
```

Three different test counts from one clean tree, because `letibot_tui-<hash>` lives in
the shared `target/debug/deps` and whichever worktree built `crates/tui` last owns it —
`agent-tui-status-and-copy` and `agent-plugin-command-feeds` were both building `tui`
while this ran. Two consequences worth carrying:

* **A local green (or red) may be about somebody else's tree.** The names give it away:
  `cargo test -p letibot-tui --lib -- --list | awk -F'::' '{print $NF}'` and check them
  against `grep -rn "fn <name>(" crates/tui/src` — a name the run has and the tree does
  not is a foreign binary. The count alone is enough once you know it moves.
* **The gate is not affected**: it runs with `CARGO_TARGET_DIR=<worktree>/target`
  (`mergequeue.rs`'s `GATE_TARGET_DIR`), so a branch is judged with its own artifacts.

To measure a branch honestly while siblings build, give it its own target directory
(the whole dependency tree builds once, wasmtime included, and then it is incremental):

```sh
CARGO_TARGET_DIR=$TMP/isolated-target cargo test -p letibot-tui
```

That is the run to quote. Everything above still holds for the *cache*, but an isolated
`CARGO_TARGET_DIR` fixes the binary half as well.
