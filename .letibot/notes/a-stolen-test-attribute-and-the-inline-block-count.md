<!-- abstract: A doc-comment plus `#[test]` pasted above the next function takes the old test's place, and `comment-blocks.py` counts a `//` run inside a `format!` string, which makes the count unable to reach zero: that block is the tool's OUTPUT and `same-code.py` refuses to cut it. -->

# A stolen `#[test]`, and the inline-block numbers nobody had measured

A doc-comment, a `#[test]` and a fn pasted between an existing `#[test]` and its function leaves the
old test compiled and never run (no dead-code warning: a `#[test]` fn is reachable by definition)
and registers the new one twice, so it runs twice. Both are silent;
`crates/harnessd/src/mergequeue/serve/tests.rs` carried one of each.

The check is the listing, not the run:

```sh
cargo test -p <crate> --lib -- --list | grep <name>
```

A filter that matches nothing prints `0 passed; 0 failed; N filtered out`, which reads like a pass.
This is how a settled doc entry came to cite a test that is not one.

A number in a settled entry is a claim like any other, and the ones about the shape of the tree are
cheap to re-measure. R70's "29 inline `mod tests` blocks over 40 %, nothing past 800 lines" counted
54 blocks and 6 over 800 lines, measured by walking `^\s*mod tests {` to the closing brace at the
same indent and comparing to the file's own length (excluding `/tests/` paths, which are all block).

## The other count that cannot reach zero: a comment block inside a `format!` string

`scripts/comment-blocks.py` matches `^\s*//`, so it counts the `//!` and `///` runs inside a
`format!` raw string as this crate's comments - and cutting one is a CODE change, because the string
IS what the tool emits (`scripts/same-code.py` reports the file as differing).

MEASURED 2026-11-05, `wt-z-wave2`, cutting every block over three lines under `crates/`: 75 before,
4 after, and all four are `crates/harnessd/src/plugins/scaffold.rs` - the generated plugin's own
`src/lib.rs` doc inside the two `format!` templates (lines ~561, ~581, ~636, ~690). Those are the
ones to keep and name when the count is reported; every other crate reached zero.

Tell one before touching it: the line sits inside `format!(r#"` ... `"#)`, or a test edit plus
`python3 scripts/same-code.py <base>` answers `differs: <file>`. Dedenting a moved `mod tests` block
does not trip it - the strip drops whitespace outside literals.
