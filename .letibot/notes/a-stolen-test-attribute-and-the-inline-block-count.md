<!-- abstract: A doc-comment plus `#[test]` pasted above the next function takes the old test's place - that test then compiles and never runs, and the new one is registered twice - so `-- --list` is the check, never a green run. -->

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
