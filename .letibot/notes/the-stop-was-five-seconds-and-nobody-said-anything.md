<!-- abstract: MEASURED 2026-10-26: the daemon printed NOTHING during its orderly stop, so `--stop`'s five seconds were compared against bounds it could not see; lease takeover only reaches a lease somebody CLAIMS, which is why dead daemons' rows survived; and `bash -c 'sleep 12'` is EXEC'd, so a test's crafted argv naming a socket vanishes. -->

# The stop was five seconds long, and the daemon said nothing the whole time

MEASURED 2026-10-26, branch `queue-opt-in` (letibot): the queue's opt-in, the lease sweep, and
`letibot --stop`.

## `--stop` was five seconds against a stop that says nothing

The serving path's shutdown (`crates/harnessd/src/cli.rs`) ran
`firecode::down_all()` → `merge_stop` + join the queue workers (a gate in flight is killed: SIGTERM,
5 s, SIGKILL) → `daemon.shutdown()` → `drain_subagents(SUBAGENT_EXIT_GRACE = 5 s)` → exit, and
**printed not one line** until a give-up sentence. So `--stop`'s `wait_gone` (10 tenths = 5 s) could
only compare its patience against bounds it could not see, and it printed *"is ignoring SIGTERM after
5s"* about a daemon that was working through exactly that list. It now prints one
`  stopping: …` line per step, and the launcher relays the last one it finds.

**The launcher reads only the bytes written AFTER the signal** (`log_at` + `tail -c +N` in
`stop_step`): the per-folder log `$HOME/logs/harnessd-<key>.log` is append-only across runs, so its
tail carries the PREVIOUS run's last step until the new one starts printing.

`STOP_TENTHS=60` (30 s) for a plain stop; `--force` keeps `FORCE_TENTHS=10` (5 s) and then SIGKILLs,
because killing is the whole of what `--force` is for and the VMs and children are down by then.

## Takeover needs a CLAIMANT, and that is why `letim`/`fctop`'s leases survived

`Store::acquire_queue_lease` does displace a holder whose heartbeat is past `LEASE_TTL_MS` — but it is
only asked by a supervisor, and a supervisor only asks for the repositories of ITS OWN workspace. A
lease whose repository no daemon serves is never asked about, so it is stale for ever and the pane
draws it as *served by*. Hence `QueueService::sweep_leases`: every pass, over the WHOLE store, with
`Store::release_stale_lease` guarded on the heartbeat the sweep judged (`heartbeat_ms < now - ttl`),
so a holder that beat in between keeps its row.

`lease::gone` releases only when the heartbeat is past the TTL **and** the holder is not a live
process on this host — deleting a live process's row would make that daemon's next heartbeat answer
`false` and stop it serving. `lease::hostname()` falls back to `"localhost"` on macOS (no
`/proc/sys/kernel/hostname`), which is what makes the host comparison work there at all.

## `bash -c 'sleep 12'` is EXEC'd, so a crafted argv disappears

Found in `tests/launcher.sh`: `bash -c 'sleep 12' _ bash --socket "$sock" &` was meant to leave a
process whose argv names the socket (which is how the launcher tells THIS folder's exiting daemon
from a recycled pid). Bash **exec**s a single-command `-c` string, so the surviving process was
`sleep 12`, the `grep -qxF -- "$SOCKET"` found nothing, and the launcher went straight to starting a
second daemon. `bash -c 'sleep 12; :'` keeps bash as the process. The file's older going-daemon case
survived only because its body is a `while` loop, which bash cannot exec.

## Serving only the repositories that asked has one sharp edge

A repository whose target declares no `Merge gate` now gets no lease and no worker, so it gets no
review upkeep either: a `task_start` child that finishes there still has its row written by
`mergequeue::review::enqueue_finished`, and no worker ever claims it. Nothing counts it (it is a
review row, not a `target_health` row, so the pane's `unjudged` and the edge are untouched) — the
only trace is the supervisor's one `is not a queue target` line in the daemon's log. Left as the
brief's rule says, but that is the first place to look if a branch "disappears".

## UPDATE 2026-10-29 — `stop_ends_runs` can be red once in a fully parallel workspace run, and it is not your diff

MEASURED in `agent-fix-tool-mount-landing`: `LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace
--no-fail-fast` (≈150 test binaries at once) failed **one** test —
`crates/harnessd/tests/stop_ends_runs.rs`'s `a_stop_ends_the_run_that_holds_the_worker`:

```
---- the stop took 5.000019541s (the run's own deadline is 120 s) ----
---- the Bye arrived after Some(27.372291ms) ----
panicked at stop_ends_runs.rs:304: the daemon ended a run on the way out and said nothing about it
```

The Bye at 27 ms is the useful fact: `Registry::close` publishes `daemon_stopping_runs` **before**
`h.close()`, so a Bye that early means the enders produced no sentence at all (`said.is_empty()`) —
the `! /bin/cat` run the test had just seen `operator_run_unreadable` for was no longer in
`self.runs`. Nothing here is in the plugins diff that was being landed.

It did not reproduce: the same target alone (1.1 s), under eight CPU hogs (3/3 green), the whole
`-p letibot-harnessd --no-fail-fast` package (≈60 targets in parallel, green), and a second full
`--workspace` run (green, exit 0). So it is a load-sensitive race in the stop path and the ender
ledger, not a regression — but a red on the queue for this test is worth a second run before
anybody reads a diff.
