<!-- abstract: MEASURED 2026-10-26 (RH8): a fanout branch is a job of ANOTHER host's table, so jobwatch has nothing to wait on and the fanout cannot settle into the model's queue — it blocks, bounded by deadline_s. Three rakes found building it: `case "$*" in *gone*)` matched a work directory; a job that exits inside the launch pid poll reports pid 0; serde_json keeps a call's key order and BTreeMap does not. -->

# A fanout branch is a job of another host's table, and that is why `fanout` blocks

MEASURED 2026-10-26, branch `fanout-scenario` (RH8), building `fanout` and `scenario` on RH7's
supervisor.

## The hole, stated as a type

`ToolOutcome::Backgrounded { handle }` is delivered by `harnessd::jobwatch`, whose watcher thread
blocks in `ProcessHost::wait_job(handle)` — **on this session's process host**. A `fanout` branch is
spawned through a *per-host* `RemoteProcesses`, so its handle lives in that machine's job table and
`wait_job` here answers `NeverStarted` for ever (the subagent arm of `watch` exists because of exactly
that shape). Hence: `fanout` **blocks** for its answer, capped by `deadline_s` (default 600 s, ceiling
3600), and its settlement is not a thing the model is woken by.

The restructure that would give it one — and it is a restructure, not a tool change: the session's
process host becomes a **composite** of the hosts it can reach (`SpawnRequest.host: Option<String>`,
`JobView.host`), so `spawn` dispatches, `jobs()` lists every host's rows (the pane and `job_output`
then see branches), `wait_job(fanout)` blocks on N branches, and `JobRecorder::process` writes each
branch's record with ITS host rather than the session's placement. Until then the tool writes the
records itself through `letibot_tools::fleet::ProcessBook` (harnessd's `supervise::StoreBook`), and
the supervisor's tick owns deadline, liveness and teardown.

## Three rakes, each measured here

* **`case "$*" in *gone*)` is not a host test.** The fake ssh that refuses one host matched the word
  anywhere in its argv, and a fanout test named its work directory `fanout-gone` — so a healthy host
  was reported unreachable for a reason that lived in the test. Match the destination
  (`*gone.invalid*`).
* **A job that exits inside the launch's pid poll has pid 0.** `RemoteProcesses::launch` polls for the
  pid and stops early when the exit file already exists, so a five-millisecond command reports
  `pid: 0`. Harmless (the exit file is the settlement; nothing signals pid 0) but a test that looks at
  the process has to give it a moment.
* **`serde_json` keeps a call's key order; `BTreeMap` does not.** A scenario's `roles` is an object, so
  collecting it into a `BTreeMap` and iterating that starts roles **alphabetically** — a plan nobody
  wrote, and a test that pointed at `web` first got `slow`. Keep the declaration order in a `Vec` and
  use it for ties in the topological order.

## And the one that is not a rake

`remote::expand_root` is new because a root is the one path that WANTS its `~` expanded THERE: `mkdir
-p '~/letibot-remote'` makes a directory called `~` (RH1's rake, RH4's rake, now one function).

## UPDATE 2026-10-27 (branch `fanout-settles`, package FB): the composite was NOT needed

The prediction above — *"the session's process host becomes a composite of the hosts it can reach,
so that `wait_job(fanout)` blocks on N branches (a restructure of `HostProcesses`/`RemoteProcesses`
and `JobRecorder`)"* — was **too expensive and not necessary**. A settlement needs four things from
`ProcessHost` (`wait_job`, `job`, `output`, `kill_job_as`) plus `jobs()` so a handle resolves, and all
five can answer for a **job of jobs** without the host learning what a host is:

* `exec::host::Aggregate` — one handle in THIS session's table: `state`, `elapsed`, `produced`,
  `output`, `wait`, `kill`, `report`. `HostProcesses` keeps a second map beside `jobs`, and every
  read path asks it first (`holds`, `jobs()`, `output`, `wait_job`, `kill_job_as`), so `jobwatch`,
  `job_output`, `job_kill`, `job_wait` and the activity's `waiting on: job …` are unchanged code.
* `ProcessHost::hold(&JobId, Arc<dyn Aggregate>)` is the whole new seam (default: a refusal), and
  `ProcessHost::claim_name` lets two fanouts in one session be `fanout` and `fanout-2`.
* `crates/tools/src/builtins/fanout_bg.rs` is the tool's half: `background: true` starts the
  branches, holds the handle, and a thread of its own polls them exactly as the blocking loop does.

**Three things measured while building it, worth keeping:**

* **A settlement that carries more than a row needs a hook.** A fanout's answer IS its table, so
  `ProcessHost::settlement_note` (default `None`) answers with the table, `watch_one` puts it in
  `JobCompletion::detail`, and `completion_notice` prints it under the job's row. `bash`'s notice is
  byte-identical: the hook is `None` for every ordinary job, which is what keeps R93.C3 true.
* **`job_output` on such a handle must read an append-only JOURNAL, not a re-rendered table** — an
  offset into a table that changes as branches settle means something different a second later.
  `<host> · started` becomes `<host> · exited 0 · 1.2s` when the branch ends.
* **`JobRecorder::process` has to skip a job of jobs**, or the local recorder calls
  `identity_of(0)`, refuses it, and writes a `job_not_recorded` warning to the hub for every fanout.
  Guarded on `pid == 0 && placement.is_none()`.
* **A `NoScopes` host is enough apparatus for all of it**: `crates/tools/tests/fanout.rs` drives a
  real background fanout over the fake ssh with no cgroup at all, and `jobwatch`'s own test settles a
  fake `Aggregate` with no process anywhere.

The rest of the note stands: a branch is still a job of the far host's table, and `job_output` on a
*branch's* handle is still not a thing — the parent is the door.
