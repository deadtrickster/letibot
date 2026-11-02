<!-- abstract: `firecode in` has no timeout of its own, so every guest call in `crates/tools/src/firecode.rs` is bounded by `rpc::run_bounded` - and its process-group kill, not a client kill, is what takes the orphan. -->

# `firecode in` has no deadline anywhere, and `rpc::run_bounded` is the runner that gives it one

`~/Projects/firecode/firecode/bin/firecode`'s `cmd_in` parses `--cwd` and `--project` only, and
`scripts/vsock-exec.py` sets `sock.settimeout(15)` for the handshake and then `settimeout(None)` before
the read loop. The `--timeout 0` in that CLI belongs to `firecode exec`, not to `in`. A reply lost on
the vsock is an unbounded wait: one held a reviewer's turn for eleven minutes at 0.0% CPU.

`rpc::run_bounded` (`crates/tools/src/rpc.rs:443`) spawns with `.process_group(0)`, drains both pipes on
threads, and at the deadline kills the whole group. A client-only kill is not enough - `vsock-exec.py`
is a child of `firecode`, so the ghost the next call queues behind survives it.

The deadlines are constants in `firecode.rs`: `GUEST_TIMEOUT` 120 s for one control command
(`guest_within`), `GUEST_COMMAND_TIMEOUT` 600 s for a model's command, `LAYER_TIMEOUT` 300 s,
`UP_TIMEOUT` 600 s, `CLIENT_TIMEOUT` 120 s. After a group kill the grandchild answers `kill -0` for a
moment - reparented, a zombie - so poll, never assert once.
