<!-- abstract: Two fixtures assert on the box rather than the subject and go red under a full parallel run: `dispatch.rs` counts stderr lines (the seed install is a third, once per home) and `ps.rs` caps the table at 5 lines while a neighbour's `python3 -c` command line carries newlines. Both pass alone, and on the base commit. -->

# A fixture that counts a child's stderr lines counts the home it runs in

MEASURED 2026-11-05, whole-workspace gate in `wt-z-t10` (session home
`/home/dead/letibot-remote/.letibot-home/74356000`, `$HOME/.config/letibot/permission.json` absent):

```
test an_unknown_session_id_names_the_real_ones_and_where_to_list_them ... FAILED
panicked at crates/letibot/tests/dispatch.rs:246:9:
assertion `left == right` failed: two lines, not a wall:
"letibot: installed the preapproved list at .../permission.json — it is yours to edit\nharnessd: no such session: s-nope\n         `letibot --sessions` lists the sessions this store holds\n"
  left: 3
 right: 2
```

`parse_permission()` (`crates/harnessd/src/config.rs:1436`) prints the install line ONCE PER HOME, on
the first process that parses its config. The fixture runs `letibot daemon --store ... --delete
s-nope` as a child and asserts exactly two lines, so what it asserts is the home's state as much as
the refusal.

The tell: re-running the same test passes, because the first run left the seed behind. So a single
red in `-p letibot --test dispatch` on a fresh home is this and not the change under test - and since
`cargo test --workspace` stops at the first failing target, that one red also hides every crate after
`letibot` in the alphabet.

What does NOT fix it: a temp `HOME` (a fresh home installs the seed again and prints the same line)
and `LETIBOT_PERMISSION` (the install block runs before the env layer is read). A fix has to stop
counting: assert on the last two lines, or pre-seed the home before the child runs.

## A second one, from the same run: a table's height against a command line with newlines

`crates/tools/src/builtins/ps.rs:270` asserts `top.payload.lines().count() <= 5` for a `top: mem,
limit: 3` answer. A neighbouring process whose command line contains newlines - here a firecode
helper, `python3 -c '\nimport socket, sys, time\n...'` - is one ROW and several LINES, so the cap is
exceeded by a process the test never mentions. It passed on the base commit and alone; it went red in
the 298-target `--no-fail-fast` run, where a multi-line helper was running.

Both of these are the same class as `a-picture-fixture-that-was-not-a-picture`: the assertion counts
something about the machine (a home's files, a process table) rather than about the subject. The
practical rule for a gate verdict on this box: a single red in a target whose subject is not the
change, which passes when the target is run alone, is this class - re-run it before believing it.
