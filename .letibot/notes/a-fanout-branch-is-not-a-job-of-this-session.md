<!-- abstract: A fanout branch is a job of ANOTHER host's table, so `jobwatch` has nothing to wait on and `fanout` blocks; the seam that settles it is `Aggregate` held in this session's table, not a composite process host. -->

# A fanout branch is a job of another host's table, and that is why `fanout` blocks

`ToolOutcome::Backgrounded { handle }` is delivered by `harnessd::jobwatch`, whose watcher thread blocks
in `ProcessHost::wait_job(handle)` on THIS session's process host. A `fanout` branch is spawned through
a per-host `RemoteProcesses`, so its handle lives in that machine's job table and `wait_job` here
answers `NeverStarted` for ever. Hence `fanout` blocks for its answer, capped by `deadline_s`.

The seam that fixed it is not a composite process host - that was predicted and was too expensive.
`exec::host::Aggregate` is one handle in this session's table; `HostProcesses` keeps a second map beside
`jobs` and every read path asks it first; `ProcessHost::hold(&JobId, Arc<dyn Aggregate>)` is the whole
new seam. `jobwatch`, `job_output`, `job_kill` and `job_wait` are unchanged code.

Rakes: `case "$*" in *gone*)` in a fake ssh matches the word anywhere in argv, so a test whose work
directory is named `fanout-gone` reports a healthy host unreachable - match the destination. And `serde_json` keeps a call's key order while `BTreeMap` does not, so a scenario's
`roles` collected into a `BTreeMap` starts them alphabetically.
