<!-- abstract: A `Construct::ParseError` with `partial == false` is a whole tree this tree's own resolver refused, so it is our bug and not a grammar limit; a brace check must be a PAIR, never one word. -->

# A `parse_error` is a bucket, and a quarter of it was ours

`Normalised::partial` is tree-sitter's own `has_error`, so a `Construct::ParseError` with `partial == false` means the tree is whole and the resolver called it a parse error. Every such command is a bug in `crates/code/src/shell.rs`, not a grammar limit. The instrument is `corpus_replay::parse_cause`, printed by `examples/gate_corpus` under the construct table.

Two spellings that were ours: a `$` that ends a word (`x$$`, `cargo nextest -E run=^$`) arrives as the anonymous `$` token, and a bare `variable_name` in an argument position (`export SKIP_SLT`) was not walked. A compound in the argument position (`time ( … )`) was worse - the subshell read as one opaque word, so the commands inside were never walked; it is `Construct::CompoundArgument` now.

Rake: a refusal that is principled can still cost the 0-broken row. `echo {a b,c}` reads as two words, but `M-}` is a tmux key and `ls {corpus` is a half-typed line, both literals to bash - so the wreckage is only recognisable as a PAIR (`opens_unclosed` / `closes_unopened`). And a quoted brace is not a brace: check the merged text against a parallel string with every quoted character blanked.
