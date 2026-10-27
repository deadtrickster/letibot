<!-- abstract: MEASURED 2026-10-27 on the Mac: a job that FINISHES while a status poll is in flight answers "no status" then "pid gone" — the same two answers a job something killed gives — so `refresh_within` recorded `exited 0` as killed; the poll now reads the status file once more, and the scenario's teardown decides a role's row AFTER the kill. -->

# A job that finishes inside the poll answers like a job that was killed

MEASURED 2026-10-27 on the Mac (`bb66c6b1`, branch `scenario-mac`), `cargo test --offline -p
letibot-tools --test scenario` with the binary's other tests beside it: `6/6` red for the
orchestrator, `2/6` then `5/14` here, **0 alone** (`--test-threads=1`, or one test by name). Two
different sentences, one cause each — and the second is not a fix for the first.

## What the two failures actually said

```
client on fake1 · killed by something on the remote ended it and it wrote no exit status · 0.2s · tail: peer=fake1 ⏎ ok=True
client on fake1 · stopped · 0.3s · the scenario ended · ready after 0.0s · tail: peer=fake1 ⏎ ok=True
```

Both about a role that had *done its work*: 19 bytes of output and its own `.exit` file holding
`0` (found in the kept rig — `…/letibot-fleet-after-<pid>-<ns>/home/letibot-remote/scenario-after/.letibot-remote/jobs/7.exit`,
`-rw-r--r-- 2 bytes`). The web role's `1.exit` was absent, which is what a *killed* launcher looks
like.

## The mechanism, which is a fact about `refresh_within`'s two reads

The status question is ONE ssh round trip and it reads two files:

```sh
e=; if [ -f EXIT ]; then e=$(cat EXIT); fi          # the status FIRST
a=0; if [ -f PID ]; then p=$(cat PID); if kill -0 "$p"; then a=$p; fi; fi   # the pid SECOND
```

Between the two sits a **fork** (the `cat`). So a launcher that finishes inside that window is read
as *status empty* at the first read and *pid gone* at the second — which is letter for letter what
a launcher something killed answers. `refresh_within` then settles `Killed`. The ordering is the
whole bug: **the launcher writes its status BEFORE it exits** (`launch`'s `( cmd ) > out; echo $?
> exit`), so *pid gone + no status* is sound only if the status was read after the pid.

Called with the pid-gone answer, the settle also spends the second bug: `kill()`'s settle loop
takes the same poll while its own SIGTERM lands, so a `job_kill` this daemon sent was recorded as
*something on the remote ended it* — and once settled, `settle` is first-wins.

## The second sentence is a different race, in the scenario's teardown

`builtins/scenario.rs` asks `fleet.settled(&running)` **before** the kill and prints that snapshot:
a role still running at the question, dead 40 ms later with `0` in its status file, gets the row
*stopped · the scenario ended*. The trace of one such run (0.462 s launch, 0.533 s poll *alive*,
0.576 s `ps -o pgid=` answering EMPTY, 0.625 s poll `exit="0"`) is the whole story: the teardown did
nothing to that role and said it stopped it.

## What changed

* `remote.rs` `refresh_within`: when the answer is *no status and the pid gone*, `cat` the status
  file **once more** (`reading the exit status one more time`) before calling it a kill. A status
  found then is the job's own exit; a look that did not answer settles nothing (a dropped
  connection is not a kill, and the next tick asks again).
* `remote.rs` `kill`: `armed_kill = Some(by)` before the signal, so a poll inside the settle loop
  attributes the ending to the caller instead of to a mystery.
* `builtins/scenario.rs`: the role's fate is decided **after** the teardown — an `Exited{..}` found
  post-kill is a leg that finished on its own (a signal can only ever leave the status file
  absent), and only `Killed` is printed as *stopped · the scenario ended*.
* A forcing test, `remote::tests::a_poll_that_finds_the_pid_gone_looks_at_the_status_file_again`:
  a fake ssh answers the poll `\t0\t19` (no status, pid gone) and the status `0`, so the window is
  not raced — it is the fixture. It fails on the previous code with exactly the production sentence.

## Suspects I ruled out, so the next session need not

* **`Rig`'s `setsid` IS in force on a Mac.** `tests/support/fake_remote.rs` plants a perl `setsid`
  in the rig's `bin` when the box has none, and `bash -lc` (the fake ssh's far side) does NOT drop
  it: `/usr/libexec/path_helper` only reorders `PATH` — a prefixed directory stays on it, at the
  end. The trace's `pgid="<pid>"` for the surviving role proves the job led its own group.
* **No sibling kills another test's launcher.** `Rig::drop` signals `-<pid>` only for the pid files
  in its OWN remote root, and a pid that is not a group leader has no group to signal.
* **Not `free_port`.** Each role's failure is about its own row, and the ports were distinct.

## The rake

**Two reads in one script are not one instant.** A question of the form *is it over?* with two
sources answers two different moments, and the pair can be a combination no real state produces —
before believing the pair, look at which read decides and put the one that is a *precondition*
first, or ask again once. Reload under one core (`ulimit -Sn 256`, the binary's siblings beside it)
widens the window: this reproduced 1 run in 3 loaded and never alone.
