<!-- abstract: A gate corpus has three traps: was-it-read-then has three spellings, a transform of a variable reads as the variable, and an unrolled loop walks one body per value against one span. -->

# The parser, measured on 161,099 decisions — and the three rakes under the numbers

The instrument is `crates/tools/src/corpus_replay.rs` with `examples/gate_corpus.rs`: it re-runs
`Baseline::of_command_with`, `shape_of` and the grant key per command over a corpus JSON and
prints unresolved by construct, appears and alone.

"Was it read then?" has three spellings: the marker `the meaning was never resolved`, which only
exists since 2026-10-02; the older `not_run` verdict with no marker; and
`verdict_by == "boundary:normaliser"`. Counting one invented 514 regressions on one host.

The grammar reports the same `variable_name` for `${X}`, `${#X}`, `${X%.*}`, `${!X}` and
`${arr[0]}`, so a resolver keyed on the name splices the wrong text. The check must be lexical
(`parameters::plain`, `shell::bound`).

An unrolled `for` body is walked once per value against ONE span, so splicing twice eats the
neighbour. Two fail-closed guards: no two splices may touch the same byte, and a substitution
that would make a command less parseable is abandoned whole.

`$?` was the largest single reason a command was unread - 4,827 commands. A number is not a
place, so the shell's own numbers are written back as `0` with a note; `$@`, `$*`, `$1` and `$0`
stay unresolved, because those can be paths.
