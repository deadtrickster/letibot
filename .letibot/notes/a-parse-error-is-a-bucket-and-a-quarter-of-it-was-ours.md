<!-- abstract: MEASURED 2026-10-27 (R94): grouping the 577 lab2x1 parse errors by cause found 152 that were never the grammar's — a `$` at a word's end and `export NAME` fall through our own node-kind matcher — and the census is now `corpus_replay::parse_cause`; plus the pair rule that keeps "0 commands stopped being readable" true. -->

# A `parse_error` is a bucket, and a quarter of it was ours — MEASURED 2026-10-27 (R94)

Branch `parser-unreads` (wt-r94), the operator's three corpora, 161,097 `bash` decisions.
The doc is `docs/boundary-and-adjudication.md` §4i.7; the instrument is
`corpus_replay::parse_cause`, printed by `examples/gate_corpus` under the construct table.

## `partial == false` is the whole trick

`Normalised::partial` is tree-sitter's own `has_error`. So a `Construct::ParseError` with
`partial == false` means **the tree is whole and WE called it a parse error** — the resolver's
fall-through for a node kind it does not name (`Resolved::Blocked { ParseError }` in
`resolve`). Every such command is a bug in `crates/code/src/shell.rs`, not a grammar limit.

Two spellings, both fixed:

* **a `$` that ENDS a word.** This grammar wraps `$?`, `$#`, `$@` and even `$$` in a
  `simple_expansion` when something follows, but `$$` or `$` in the last position comes back
  as the ANONYMOUS `$` token (`x$$`, `cargo nextest -E run=^$`, and a bare `echo $$` as a
  whole argument the stage used to drop silently). 133 commands on lab2x1.
* **a bare `variable_name` in an argument position**: `export SKIP_SLT`, `declare -x FOO`.
  24 commands.

And one more class that was ALSO called a parse error while the tree was whole: a **compound
in the argument position** (`time ( … )`, `! ( … )`). That one was worse than a wrong name —
`stage` read the `subshell` as one opaque word, so the commands inside were NEVER WALKED and
their redirects and programs did not exist for the gate. It is `Construct::CompoundArgument`
now, walked, and still unread.

## What is genuinely the grammar (and is written up, NOT filed)

`exec 3<>/dev/tcp/h/p` (no `<>` in `file_redirect`), `cat <<'EOF' 2>&1 | tail` (a second
redirect between a here-document operator and its continuation), `git for-each-ref
--format=%(refname)` (a MISSING `;`). The first two are 207 + 122 of the 425 left on lab2x1.

## The rake: a refusal that is principled can still cost the 0-broken row

`echo {a b,c}` arrives as TWO words `{a` and `b,c}` and each reads as a literal, which claims
two arguments where bash delivers three — so refusing a word whose braces do not pair looks
right. It is not, alone: `M-}` is a tmux key name and `ls {corpus` is a half-typed line, both
literals to bash, and refusing them took the replay's "commands that stopped being readable"
from 0 to 2. **The wreckage is only recognisable as a PAIR** — one word opening a brace it
never closes and another closing one it never opened, in one argument list. That is where the
check lives (`opens_unclosed`/`closes_unopened`, called from `stage`).

And the same lesson one level down: a brace check on a concatenation's MERGED text loses the
fact that a quoted brace is not a brace. `gh api graphql query='{repository(…)}'` was refused
28 times on the mac for that. The fix is a parallel string with every quoted character
blanked, run through the same `has_brace_expansion`.

## Numbers, so the next session can tell drift from regression

Unread 10,065 → 9,916 (7,533 / 2,353 / 30); 149 more read, **0** made unread; parse errors
577 → 425 and 64 → 34. `brace_expansion` and `ansi_c_escapes` are 0 on every host.
`corpus_replay`'s `command_substitution_value:date`, ALONE, went 92 → 156 — the same fact from
the other side: the parse error above it was masking it.
