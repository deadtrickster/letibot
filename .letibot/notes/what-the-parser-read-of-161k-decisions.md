<!-- abstract: MEASURED 2026-10-26 against every gate decision on three hosts: `$?` alone was why 4,827 commands were unread (a number is not a place), unrolling a literal `for` recovered the `$f` bucket, and two resolver defects (a transform read as its value, one span spliced twice) were in G3's code. 17,555 unread commands became 10,065; the harness is `examples/gate_corpus`. -->

# The parser, measured on 161,099 decisions — and the three rakes under the numbers

MEASURED 2026-10-26, branch `gate-corpus` (worktree `wt-g4`), on lab2x1 against
`~/letibot-corpus/{lab2x1,mac,lubuntu1}.json` — 187,930 rows, 161,099 of them `bash`.

## The instrument

`crates/tools/src/corpus_replay.rs` + `examples/gate_corpus.rs`:

```sh
cargo run --offline -p letibot-tools --example gate_corpus -- \
    --home /home/dead --workspace /home/dead/Projects/<repo> ~/letibot-corpus/lab2x1.json
```

It streams the array, re-runs `Baseline::of_command_with` + `shape_of` + the grant key per
command, and prints: resolved then/now, unresolved by construct (**appears / alone** — the
second is what fixing one construct buys), shapes, what a standing approval would now settle
(replayed per session, in `decided_ms` order), and the oracle-brief diff. `LETIBOT_CORPUS_SHOW=n`
adds samples by request id and READING — never command text. Its grounds:
`Surroundings::with_pinned_shell` (the daemon pins; unpinned makes 88% unread for a reason
that is not the parser) and `scripts: &[]` (a corpus is a record, not the tree).

## Rake 1 — "was it read then?" has THREE spellings, and reading one of them invents 514 regressions

The `baseline` column is layer A's summary. `the meaning was never resolved` is the marker
appended **since 2026-10-02**; before that the verdict token was `not_run` and the summary had
no marker; and `verdict_by == "boundary:normaliser"` is the one arm that means it. Counted on
the marker alone, 514 lab2x1 rows read as *resolved then, unresolved now* and every one was a
pre-marker row. **A count whose denominator is a rule that changed halfway through the data is
not a measurement.**

## Rake 2 — a transform of a value was being read as the value (G3's own bug)

The grammar gives the SAME `variable_name` for `${X}`, `${#X}` (a length), `${X%.*}`,
`${X/a/b}`, `${X:0:3}`, `${!X}` (the value of the variable X *names*) and `${arr[0]}` — the
transform lives in an `operator`/`subscript` child. `named()` reports `X` for all of them, so
G3's resolver spliced `$X`'s value for all of them. Fixed by a LEXICAL check on the expansion's
own text (`parameters::plain`, `shell::bound`), not by re-reading the node. And for
`${PIPESTATUS[0]}` the reported name is the whole braces content, `{PIPESTATUS[0]}` — so a
check keyed on the name cannot even recognise it.

## Rake 3 — one span, spliced twice: unrolling walks a body once per pass with the SAME offsets

`parameters::resolve` splices values into the text at the parser's spans. A `for` body unrolled
over three values is walked three times against ONE place in the text, so `$?` inside the body
is reported three times with one span; splicing twice replaces the value AND its neighbour —
`echo "rc=$?"` became `echo "rc=0` with the quote eaten, and the rest of the line stopped
parsing (72 commands on lab2x1 before the guard). Two guards now, both fail-closed: no two
splices may touch the same byte, and a substitution that would make a command *less* parseable
than it was is abandoned whole.

## The two findings that paid for the work

* **`$?` was the largest single reason a command was unread** — 6,019 commands, 4,827 of them
  for that alone, one idiom (`echo "exit=$?"` after a build). "A number is not a place": the
  shell's own numbers (`?`, `$`, `!`, `#`, `RANDOM`, `LINENO`, `PIPESTATUS`, …) are written back
  as `0` with a note on the audit row. `$@`, `$*`, `$1`, `$0` stay unresolved — those can be paths.
* **`for x in <literal list>` unrolled** — the loop variable was second (1,615 commands for `$f`,
  759 alone). Body walked once per value with the variable bound; globs, expansions, quoted
  multi-word values and >32 values are refused, and certainty stays conditional.

## What was left, on purpose

`$PATH` (527 lab2x1 / 1,347 mac, by far the largest left) — resolving `export PATH=…:$PATH`
lets a shape or a session grant settle a command whose later stages may run a different binary.
It needs `permission::sets_state_for_later` extended to grants and shapes FIRST. Also left:
`$(…)` (its value is a runtime fact), variables a previous command assigned (needs a
session-scoped environment — a design decision), `parse_error` (grammar), process substitution,
arithmetic. Brace expansion and ANSI-C quoting are decidable and are the next two wins.

## The numbers

Unread commands: 14,285 / 3,210 / 60 → 7,633 / 2,402 / 30 (lab2x1 / mac / lubuntu1); 12,845
commands became readable and **0 stopped being**. Oracle decisions taken on an unread command:
215, of which **151 are readable now** (4 at G3's tip). Human answers a standing approval would
now settle: 167 → 192 (lab2x1), 72 → 78 (mac). The doc section with the full table is
`docs/boundary-and-adjudication.md` §4i.
