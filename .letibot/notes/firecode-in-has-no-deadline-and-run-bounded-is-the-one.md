<!-- abstract: MEASURED 2026-10-29: `firecode in` waits for the guest's reply with no deadline at all (`vsock-exec.py` does `sock.settimeout(None)` after the handshake and blocks in `recv`; `cmd_in` parses only `--cwd`/`--project`), so a reply lost on the vsock is an unbounded wait — one lost reply held a reviewer's turn for eleven minutes and cost the queue forty and an attempt. The tree's answer already existed: `rpc::run_bounded`. -->

# `firecode in` has no deadline anywhere, and `rpc::run_bounded` is the runner that gives it one

MEASURED 2026-10-29, branch `agent/a-lost-reply-fails-fast` (worktree `agent-a-lost-reply-fails-fast`,
`main` at `e2fe4611`), from the operator's stalled review queue: one reviewer had been on **one file
read** for eleven minutes, `firecode in` at 0.0% CPU with its `vsock-exec.py` child, while the same
`base64` command in the same VM answered in 0.5 s.

## There is no timeout to pass — I looked

`crates/tools/src/firecode.rs`'s `guest_in` used `Command::output()`, which waits for the child *and*
for EOF on its stdio. The client it waits for has no bound of its own:

* `~/Projects/firecode/firecode/bin/firecode`'s `cmd_in` parses **`--cwd` and `--project` only**, then
  runs `python3 scripts/vsock-exec.py <sock> <port> <cwd> <command…>`. No `--timeout`.
* `scripts/vsock-exec.py` sets `sock.settimeout(15)` for the connect/handshake and then
  `sock.settimeout(None)` (lines 50-66) before `sendall` and the `recv` loop. **The read loop is
  unbounded by construction.**
* `--timeout 0` in the CLI (line 2621, `nohup setsid "$SELF" exec --timeout 0 …`) belongs to
  `firecode exec`/`claude` — an agent session — not to `in`. Don't go looking there for the `in` bound.
* firecode's own MCP wrapper bounds it from outside: `mcp/firecode-spawn.py` calls
  `_firecode(["in", …], timeout=120)` in four places. **That is where 120 s comes from** — it is
  firecode's own number for `in`, not an invented one.

So the host side is the only place a deadline can exist, and the honest fix is to bound the client.

## The runner already existed, and its group kill is the load-bearing part

`crates/tools/src/rpc.rs`'s `run_bounded(program, args, within)` → `Ended::{Answered, TimedOut,
Unstartable}`: spawns with `.process_group(0)`, drains both pipes on threads, polls `try_wait` every
20 ms, and at the deadline `kill -KILL -<pid>` then `child.kill()`/`wait()`. It was written for ssh
("one bounded runner for everybody"), and a firecode client is the same shape of problem.

**A client-only kill would not have been enough**: `vsock-exec.py` is a *child* of `firecode`, so the
ghost the next call queues behind survives a kill of the client. The group kill is what takes both.
The test (`a_guest_call_that_loses_its_reply_fails_in_seconds_and_leaves_nothing_running`) records
`$! $$` from a stub that hangs, and asserts both pids are gone.

## Two traps in proving it

* **After a group kill, the grandchild answers `kill -0` for a moment.** Its parent died in the same
  signal, so it is reparented and stays a zombie until the new reaper collects it. The test polls
  (20 ms, 10 s budget) rather than asserting once — an immediate `kill -0` assertion would flake on
  a correct fix.
* **`run_bounded`'s *success* path joins its drain threads unboundedly.** A bound on the child is not
  a bound on the call when a grandchild holds the pipe: the join waits for EOF. That is fine for
  `firecode in` (the CLI waits for `vsock-exec.py`, so it cannot exit first), but it is why a stub
  whose `down` arm backgrounds a subshell must redirect that subshell's stdout — the existing
  `down_waits_while_the_copy_out_is_pending` arm already does.

## What the file now bounds (all in `crates/tools/src/firecode.rs`)

| call | deadline | what a timeout means |
|---|---|---|
| `guest_within` — one control command (`read`/`list`/`list_tree`/`stat`/`write`/the `true` probe) | `GUEST_TIMEOUT` 120 s | `Err` naming the timeout; retrying a read is safe |
| `run` (a model's command) | `GUEST_COMMAND_TIMEOUT` 600 s | backstop only — `bash`'s own `MAX_TIMEOUT_MS` is 600 s, so this never cuts real work |
| `layer inherit` | `LAYER_TIMEOUT` 300 s | not fatal: "no layer inheritance", the copy boots on its base image |
| `up` | `UP_TIMEOUT` 600 s | a failed placement; `unwind.vm` is armed so the VM it may have booted is brought down |
| `info`, `down`, `cp` | `CLIENT_TIMEOUT` 120 s | `info`/`cp` say so; `down`'s answer is firecode's copy-out record either way |

`FirecodeBackend::guest_timeout` is a field only so a test can set it to a second
(`with_guest_timeout`, `#[cfg(test)]`) instead of waiting out the constant — the same seam as
`remote::Ssh::with_bound`.
