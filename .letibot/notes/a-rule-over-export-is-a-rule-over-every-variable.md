<!-- abstract: MEASURED 2026-10-26 (G3, gate-grants): `the_rule_that_admits` derived `export*` for the operator's own measured command shape, and `export*` admits `export LD_PRELOAD=…; git status` whole — unasked, for ever; also `Grant::covers` refuses an unresolved call BEFORE coverage, so the session rung the card offered over such a call could never fire. -->

# A durable rule over `export*` is a rule about every variable a later program trusts

MEASURED 2026-10-26, `wt-g3` (package G3, branch `gate-grants`), from one package
session's 178 gate decisions: 15 of its 22 asks were the same shape,

```text
cd <dir> && export CARGO_TARGET_DIR=$PWD/target && cargo test --offline -p <crate>
```

and three separate things were wrong with the card it drew. This note keeps the two
measurements that are not obvious from reading the code.

## `export*` — what the derivation offered, and what that rule admits

`permission::the_rule_that_admits` tests each segment against the rules and writes ONE
pattern for the segment(s) still asking. For the shape above the asking segment is
`export CARGO_TARGET_DIR=…`, so `always_pattern_for_command` gives **`export*`** and
that is what *Always allow* offered — written into `~/.config/letibot/permission.json`.

`export*` is not a rule about `export`. It matches any `export` of anything:

```text
wildcard_match("export LD_PRELOAD=/tmp/evil.so", "export*") == true
wildcard_match("export BASH_ENV=/tmp/evil.sh",        "export*") == true
```

and `bash_segments` splits a compound command, `evaluate_bash` allows the whole when
every segment is allowed, and the seed already allows `git status*` — so

```text
evaluate_bash("export LD_PRELOAD=/tmp/evil.so; git status", &[export*, seed]) == Allow
```

That is arbitrary code execution with nobody asked, permanently, from one *Always
allow* over a build command. `permission::SHELL_STATE` (and `sets_state_for_later`) now
makes the derivation return `None` for a segment that changes what a LATER segment
means; `cd` stays *skipped* rather than refused (it moves the shell and changes nothing
a later segment reads — the 2026-09-20 fix), and the test
`a_rule_over_export_would_admit_every_variable` pins both halves.

## The session rung the card offered could never fire

`Grant::covers` checks `!resolved` FIRST and returns `NotCovered::Unresolved` before it
looks at coverage — correct, and it means a grant an `allow_session` answer records over
an unresolved command can never cover anything. The card offered it anyway, so the
operator took it and was asked again for ever. The rung is now offered only when
`Grant::covers` answers `Ok` for the very grant the answer would record
(`AdjudicatedGate::session_grant`, read by both the ladder and the recording site), and
the card carries a one-line `options_note` saying which refusal applied and naming the
word nobody could read (`$X`).

## And the resolution that was there all along

`$PWD` is the directory the call runs in — the same one every relative operand is
already placed against (`Baseline::cwd`) — and `$HOME` is the harness's own. Writing
those out (`crates/tools/src/intent/parameters.rs`) makes the measured shape `resolved`,
which is what lets the card offer the session rung at all. A value is only written back
when the shell would deliver it as one plain word: no whitespace (field splitting), no
`*?[` (globbing), and nothing needing a quote of ours — the splice lands where the
expansion was, which may be inside double quotes.
