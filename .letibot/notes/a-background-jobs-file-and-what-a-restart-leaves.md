<!-- abstract: MEASURED 2026-10-29: a `bash { background: true }` run now has `<scratch>/job-<id>.out` from its spawn (from=0), and the file is deliberately NOT opened for a run that still has a deadline — an unbounded file behind every `bash` call is the 8 MiB runaway bound gone. And after a daemon restart the row is NOT gone: `adopt` re-creates it with an empty window and no file, while the file sits in the dead daemon's `scratch-<pid>`. -->

# A background job's file, and the half a restart still leaves

MEASURED 2026-10-29, branch `agent/a-background-job-gets-a-file-too` (`f7397ad5` + this), while
closing the operator's 2026-10-26 report — *"when I background a lengthy bash run with Ctrl-o its
output goes to /dev/null, causes agony"* — for the case the first fix did not reach: a run that
**begins** in the background. `spill_to` had exactly one caller (the promotion), so
`bash { background: true }` — the very thing the harness says to a run that outlives its deadline
(*"Re-run it with `background: true`"*) — kept its output only in the 8 MiB ring.

## 1. Where the file is opened now, and why not for every job

`HostProcesses::spawn` calls `spill_output(&job)` **after the `Job` exists and before the drains
start**, and only `if req.background || req.tty`. `from` is 0 because `spill_to` seeds from the
retained window and the window is empty at that instant — not because a constant says 0. The
promotion path is unchanged and a job that already has a file keeps it (`spill_output`'s first
check), so the two callers cannot disagree about the path.

**The "every local job" reading of the brief is a hazard, and the tree already said so.**
`DEFAULT_CAPTURE_BYTES`' own doc is *"small enough that a runaway `yes` cannot take the box"*. An
unbounded file behind every `bash` call removes that bound from the shape a runaway most often
has: a **foreground** command, whose output is unbounded for the whole of its 120 s deadline —
~GBs into `$XDG_RUNTIME_DIR` (tmpfs, i.e. RAM) before the kill. A deadline-bounded run therefore
still gets its file from the promotion, seeded, `from = dropped`. Pinned by
`a_background_job_has_a_file_from_its_spawn_and_a_foreground_one_does_not`.

## 2. The descriptor closes where the DRAINS join, never at `settle`

`kill_job_as` settles the job **before** it ends the cgroup (`job.settle(Killed)` then
`tree.end`), so the drains are still pushing bytes at that instant. Closing the spill at `settle`
would leave a killed job's tail out of the file with `problem: None` — a file that silently
stopped, which is the exact defect `OutputFile::problem` exists against. So `waiter.close_spill()`
sits after both `join()`s in the waiter thread, which is the last moment a byte can arrive; the
`Capture::release` budget path calls the same method.

## 3. What a restart leaves — and the brief's premise was wrong in the worse direction

The brief says *"the job's row is gone but its bytes are on disk"*. **The row is not gone.**
`Harness::open` walks `store.processes_of(session)` and calls `procs.adopt(&Adopted{..})` for
every open record (RH7, schema 34), so `job_output` on the old handle **answers** — with an empty
window and **no `output_file`** (`adopt` builds `Job::new(.., 0, ..)`: zero capture bytes, no
spill, and its only thread polls the pid). Meanwhile the file is at
`<XDG_RUNTIME_DIR>/letibot/scratch-<pid of the dead daemon>/job-<id>.out`, and nothing in this
tree sweeps or looks in a dead daemon's scratch directory (`scratch_dir()` is per-pid on purpose).

So a reader after a restart is told *"`j22008` is still running and has written nothing yet"* about
a run that printed a gigabyte, and the gigabyte is one `read` away at a path nobody names. That is
the half-done state, and closing it is **not** in this package's files: it needs the path on
`ProcessRecord` (a store column + migration), on `Adopted`, and written where the job's record is
written. Do that next rather than re-deriving this.

## 4. Numbers, for the next session

* The host-level test uses `with_capture_bytes(16)` and prints 24 bytes: the drop is exact
  (`dropped = 8`, `retained = 16`) and needs no experiment.
* The tool-level test prints 9 000 011 bytes and takes **0.24 s** — `drain` reads 8 KiB at a time
  and `Capture::push` drains the `VecDeque` per push, so ~1 100 pushes of an 8 MiB window is not
  the cost it looks like.
