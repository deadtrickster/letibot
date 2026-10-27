<!-- abstract: MEASURED 2026-10-27 (wt-ev, D14/D15/R95/R96): `ShellSession` has NO production caller — the `bash` tool is a fresh `/bin/sh -c` per call, so an `export` is gone next call and "the tracked environment" is a SEED (pinned PATH, HOME, build_env, KEEP_ENV) plus a trailer that is tested and dormant; plus four rakes: `program_name()` strips the directory so renaming a stage does not re-key a grant, `printf` with more args than `%s` repeats the FORMAT, `cargo test` exports a 2 KiB LD_LIBRARY_PATH, and an interactive bash EATS a TAB in a line. -->

# The session's environment is a seed and a dormant trailer — and four rakes under it

MEASURED 2026-10-27, `wt-ev` (branch `session-env`, commits afd7d5fb..25718313),
building D14 (`$X` the session holds resolves) and D15 (`$PATH` resolves, and each
stage is renamed to the FILE it will run). Read with
`docs/boundary-and-adjudication.md` §4j and §4i.7.

## There is no persistent shell on the `bash` path — verified twice

```
$ grep -rn "ShellSession" crates --include=*.rs | grep -v exec/shell.rs
crates/tools/src/exec/mod.rs:127       the pub use
crates/tools/src/exec/term.rs:53       a doc comment
crates/sessionlog/src/protocol.rs:364  a doc comment
```

`ShellSession::open` appears only in `exec/shell.rs`'s own `mod tests`. The `bash`
tool runs every call as a **fresh** `/bin/sh -c` (`exec/host.rs::spawn`):
`env_clear()`, then the pinned `PATH` (with the daemon's shim dir in front), the
daemon's `standing_env` (`HOME`, `LETIBOT_*`, `SUDO_ASKPASS`),
`Config::build_env`, then the request's pairs; a confined call is `--clearenv` +
`KEEP_ENV` + `--setenv` per pair.

So an `export X=…` in one call **does not reach the next**, and a resolver that
spliced a previous call's value would be making a false claim. What is true, and
what the build therefore rests on, is the seed: the pairs the harness puts on
every spawn, from `harness::surroundings_for` (pinned `PATH`, `HOME`,
`build_env`, and `KEEP_ENV` for a local placement). The trailer carries the
environment too, and is tested end to end
(`crates/tools/tests/session_env.rs`), but nothing in production constructs that
shell — so it is *dormant*, not live, and §4j.1 says so in as many words.

The parallel package R98 reported the same finding independently and asked for it
to be checked before anything was built on the trailer. It is the same answer:
the seed.

## Rake 1 — `program_name()` strips the directory, so renaming a stage does not re-key a grant

`Baseline::absorb_stage` reads `stage.program_name()`, which is
`/usr/bin/rm` → `rm` on purpose (a grant for `rm` must cover `/bin/rm`). So
writing the resolved file into the stage's `Word` changes the **shape**
(`shell::shape` uses `word.text()`) and does **not** change `grant_program` —
a grant for `cargo` went on settling `/tmp/x/cargo`. The fix is a separate
identity: `name_stage` takes `identity` beside `program`, and the caller passes
the file only for the stages `apply_path` renamed. Reach for that before
"obviously" renaming a word and expecting the key to follow.

## Rake 2 — `printf` with more arguments than `%s` REPEATS THE FORMAT

The trailer function was written as `printf '...;%s;%s\n' "$MARK" "$?" "$PWD"` —
three arguments, two placeholders. printf reuses the format for the leftovers,
so the shell wrote the whole line twice and `take_turn` saw `0` where it wanted
`<status>;<cwd>`; every shell test failed with *"a trailer with no `;` in it: "0""*.
The count is not checked by anything; a `%s` dropped in an edit is a silent
duplicate, not a compile error.

## Rake 3 — `cargo test` on this box exports a 2 KiB `LD_LIBRARY_PATH`

A report with one value over the cap must be **skipped, not refused**: refusing
the report for that one name takes the session's whole environment — `PATH`
included — with it, and every resolution stops. Measured as a failing
integration test before it was a rule.

## Rake 4 — an interactive bash EATS a tab in the line you send it

`ShellSession::run("export X='a\tb'")` with a real TAB: readline treats it as
completion, so the shell's value is `ab` and the test asserts about bytes the
shell never saw. A tab cannot be carried through this channel at all; use `~` or
another metacharacter when the point is "this value is not one plain word".

## And the two that were caught by existing tests, not by me

* `export PATH=/w/bin; cat $PATH` (a G3 test) was **readable** before this work
  and became `not_run` on the first cut: a `PATH` the command's own text fixed is
  not the new capability, so an unnameable file under it must leave the stage
  bare rather than refuse. Hence `parameters::Fixed::path` — where the value that
  resolved `$PATH` came from decides whether a refusal is allowed (`may_refuse`
  in `intent::apply_path`).
* `SSH_AUTH_SOCK` contains `AUTH`; a name-match credential rule that includes
  `AUTH` masks — and therefore refuses to write out — the most commonly exported
  socket in a developer's shell. `AUTH` is not one of the words.

## One about the placement, seen again

Two `edit` calls failed with *"mm_send_fd: sendmsg(0): Message too long"* and one
left a `docs/<file>.letibot-<pid>` temporary behind — `git status` is how you
find those, and `rm` is the fix. The write is a rename, so the target is never
half-written; the leftover is the temp.
