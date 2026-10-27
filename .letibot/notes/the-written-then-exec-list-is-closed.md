<!-- abstract: CLOSED 2026-10-27 (branch `busy-fixtures`): the eight fixtures the ETXTBSY note listed plus eight the sweep found, and the one non-test site `harnessd::sudo::write_shim`, all go through a probe-exec now; `write_stub` gained `LETIBOT_STUB_PROBE` and a forced-reproduction test. -->

# The written-then-exec'd list is closed — where each site went, and the two guards

Closes the call-site list in `a-written-then-exec39d-file-races-every-fork-in-its-binary.md`
(that note's subject is the mechanism; this one is what changed). Worked 2026-10-27, branch
`busy-fixtures`, commits `15803f53` and `7cdc2769`.

## `write_stub` now says what it asks of a stub

`letibot_tools::testing::write_stub` proves the file by exec'ing it with **no arguments**, so a
no-argument run must be harmless and quick. Two spellings, and picking the wrong one is the mistake
to avoid:

* `[ $# -eq 0 ] && exit 0` as the script's first line — the `crates/bridge` fixture-daemon
  convention. Right for a stub that would otherwise log its own argv, sleep, spawn, or block for
  ever. It must sit **above** any prelude or body, so a test's `sleep 300` prelude is not run by
  the probe.
* `LETIBOT_STUB_PROBE` (`STUB_PROBE`), set on the probe's environment and nothing else — for a stub
  whose REAL run also takes no arguments, where `$#` cannot tell the two apart. Exactly one site
  needs it: `sudo_ask_live.rs`'s streaming run, because the operator `!` line runs that script bare.

**A stub that logs is the one that bites.** The suite was green after the first pass except for two
tests in `remote_hosts.rs` that assert on a log's exact contents — the probe had added an empty
`MISE ` line and a third `ARGV ` line to fixtures nobody had guarded. So: guard every stub whose
no-argument run writes, sleeps or does work, before writing anything else.

## The sixteen fixtures

The eight the note listed: `remote_confine.rs` (its `write_executable`: the fake `bwrap`, `ssh`,
`setsid`), `remote_hosts.rs` (the same helper), `clauses.rs` (the mode-survives-an-edit fixture —
never exec'd, probed anyway so the tree has one way to write an executable), `firecode/held/tests.rs`'s
`stubborn_firecracker` (now `[ $# -eq 0 ] && exit 0`, which replaced a hand-written spawn-retry
loop), `firecode.rs`'s `stub_firecode` (**test code** — inside `#[cfg(test)] mod tests`, so the
`1696` line the note named was never a production write), `builtins/task/tests.rs`,
`operator_shell.rs` (a python double, where `if len(sys.argv) < 3: sys.exit(0)` is the same guard in
python), `sudo_ask_live.rs`.

The sweep (`grep -rn 'set_permissions\|from_mode(0o7' crates --include=*.rs`) found eight more:
`tests/support/fake_remote.rs`, `harnessd/tests/remote_host.rs`, `harnessd/tests/sudo_ask.rs`,
`harnessd/tests/sudo_live.rs`, `harnessd/src/harness/tests/citations.rs` (the `gh` stub sleeps 5 s —
the probe would have slept), `letibot/tests/migrate.rs` (its fake `ssh` and its fake `cargo`, which
needed `letibot-tools` added to that crate's **dev**-dependencies) and the test fake in
`harnessd/src/sudo.rs`. What stayed, and why: every `0o600` (data, never exec'd), `backend.rs` and
`notes.rs` (the write tool preserving a mode it did not choose), `mergequeue/carry.rs` (directories).

## The production site, and its probe

`harnessd/src/sudo.rs`'s `write_shim` wrote a `.tmp`, chmod'd it, renamed it into place and returned
— and the daemon execs that shim. The rename buys nothing: `deny_write_access` is asked of the
**inode**, so the tmp's inode is the shim's. It now ends in `wait_until_executable`, whose probe is
`sudo --version` — the one sudo invocation that answers with no password, no terminal and no
question for the head, with its output to `/dev/null`. That fixed its own test as well: the retry
loop in `sudo.rs`'s `run` helper is gone, so a regression there is now a failure rather than a
retried success.

## The race is measured, not argued

`crates/tools/src/testing.rs`'s `a_stub_a_forked_child_holds_is_waited_for` forces it: write the
file by hand, clear `FD_CLOEXEC` with `fcntl(fd, F_SETFD, 0)`, spawn a `sleep 1` holder that
therefore keeps the write descriptor across its own exec, then assert that a plain exec is refused
with `ExecutableFileBusy` **before** `write_stub` is called — and that `write_stub` takes ≥ 500 ms
and still succeeds. One second, one test, no load required.
