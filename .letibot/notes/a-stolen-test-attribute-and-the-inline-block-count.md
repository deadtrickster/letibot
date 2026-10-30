<!-- abstract: MEASURED 2026-10-31: a `#[test]` inserted above the *next* function leaves the old test compiled and never run (`--list` is the only check), the same fn registered twice runs twice, and this tree has 54 inline `mod tests` blocks over 40% — 6 over 800 lines. -->

# A stolen `#[test]`, and the inline-block numbers nobody had measured

MEASURED 2026-10-31, worktree `agent-verify-the-settled-board`, verifying `70e8abda`
(the twenty `TODO.md` entries settled against main).

## 1. A `#[test]` inserted above the next function takes the OLD one's place

`crates/harnessd/src/mergequeue/serve/tests.rs`, `70e8abda`'s parent:

```text
1461  /// **A red target rings its project's root sessions …** (R76 item 1)
1464  #[test]                     <- belongs to the fn at 1499
1465  /// **A red target puts its sentence on a session …**   <- added by 7730d482
1472  #[test]                     <- and so does this one
1473  fn a_red_target_is_told_to_a_session_not_only_rung()
…
1499  fn a_red_target_and_a_blocked_review_ring_the_project_once_each()   <- no attribute
```

`7730d482` (2026-10-30, on main) pasted a whole doc-comment + `#[test]` + fn *between*
an existing `#[test]` and its function. Consequences, both silent:

* `a_red_target_and_a_blocked_review_ring_the_project_once_each` **compiles and never
  runs** — no dead-code warning, because a `#[test]` fn is reachable by definition and
  this one is not.
* `a_red_target_is_told_to_a_session_not_only_rung` has **two** `#[test]` attributes and
  is **registered twice**: `cargo test -- --list` prints it twice, and it runs twice.

The check is `cargo test -p <crate> --lib -- --list | grep <name>` — the *listing*, not
the run. A filter that matches nothing prints `0 passed; 0 failed; N filtered out`,
which reads like a pass. This is how a doc entry came to cite a test that is not one:
the file says it exists, the suite is green, and nothing covers what it names.

## 2. The inline-`mod tests` count, measured (R70's claim was wrong)

R70's settled entry said *29 blocks over 40 %, all under 700 lines, nothing past 800
inline*. A brace walk over every `mod tests {` in `crates/` (source files only; block
lines against the file's own) answers differently:

| | |
|---|---|
| blocks over 40 % of their file | **54** |
| over 700 lines | **13** |
| over 800 lines | **6** |
| largest | `harnessd/src/plugins.rs` — **2,469** lines, 53 % of 4,648 |
| next | `mergequeue/gate.rs` 1,302 · `tools/src/runtime.rs` 1,281 · `tools/src/firecode.rs` 975 · `sessionlog/src/protocol.rs` 888 · `harnessd/src/cli.rs` 863 |
| over-40 % blocks in a file over 900 lines | 10 |

The script is `/run/user/1000/letibot/scratch-2677604/inline_tests.py` (a throwaway; the
method is: find `^\s*mod tests {`, walk to the closing `}` at the same indent, compare
to the file's line count). Exclude `/tests/` paths or `crates/llama/tests/gguf.rs`
(859 of 862 lines) counts as a block.

## 3. The rake

**A number in a settled entry is a claim like any other, and the ones about the shape of
the tree are cheap to re-measure.** Both defects here were found by counting rather than
reading — the block sizes by a walk, the missing test by `--list` — and both had been
written down as facts.
