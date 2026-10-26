<!-- abstract: MEASURED 2026-10-26: Darwin's `/usr/bin/su` is BSD's (`su [-] [-flm] [login [args]]`) and has no `-c`, so `! /usr/bin/su -c true` exits 1 in 4 ms with `illegal option -- c` — the operator-run fixture in `stop_ends_runs.rs` never had a run in flight on the Mac. -->

# `su -c true` is a Linux command; on the Mac it is a usage error in four milliseconds

MEASURED 2026-10-26 on the Mac, `crates/harnessd/tests/stop_ends_runs.rs`
(`a_stop_ends_the_run_that_holds_the_worker`, the gate's last red), by printing every frame
the attached head received while it waited for `operator_run_unreadable`:

```
DEBUG warning operator_shell_ran: `dead` ran `/usr/bin/su -c true` from their own console …
DEBUG event TranscriptContent { item_id: "stop-ends-runs#t0.1", item: ToolResult { call_id:
  "bang-1", name: "bash", outcome: Ok, payload: "/usr/bin/su: illegal option -- c\nusage:
  su [-] [-flm] [login [args]]\n\n[exit 1] the command ran and exited non-zero. …
```

So the run **finished before it began** and nothing was ever in flight. The test's whole
premise — a root process blocked on the pty the daemon holds, `/proc/<pid>/fd/0` `EACCES`
— is Linux's `su`; Darwin ships BSD's, whose usage line is the whole truth of it.

## What the state actually needs, per box

* **Linux**: *unreadable* means `/proc` refused — so the run must contain a process of
  another uid. `su -c true` is exactly that (util-linux's `su` reads its password from the
  pty, so it blocks).
* **macOS**: `letibot_tools::exec::ask::waiting_for_an_answer` is `Waiting::Unreadable`
  **unconditionally** — the signal is `/proc`'s and Darwin has none — so *any* run that
  blocks is the same state, and `cat` reading the pty is the plainest one. The test now
  picks the command per platform and asserts the same three things either way.

## The other half of that red: a leaked daemon

The failing path panics at the assertion *before* anything stops the daemon, so it stayed
up with its socket bound (pids I found: my own run's 64956, killed; the rest under
`/tmp/letibot-stop-runs-*` were the merge gate's and `wt-qc`'s). The test now holds its
daemon in a `struct Daemon(Child)` whose `Drop` kills and waits, so a panic — which is the
only way this file fails — takes the daemon with it.

## And what a `!` run leaves in the store on a failure

Not the "job row" the report asked about: `settle_operator_shell` appends the two rows
through `append_imported`, which does **not** persist — the persist is the turn's
(`submit_items_then`), and the settle path runs `run_rounds()` directly. So a `!` line
whose turn fails hard leaves 0 rows in the store even when everything worked. Do not read
an empty `transcript_item` after a failed run as evidence that the run never happened; read
the hub's events (the attached head sees them) instead.
