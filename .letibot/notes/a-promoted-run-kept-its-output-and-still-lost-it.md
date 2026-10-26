<!-- abstract: MEASURED 2026-10-26: a Ctrl+O'd run never lost bytes IN PROCESS — the ring keeps filling after the promotion and job_output returns both sides; what the operator watched "go to /dev/null" is the window's three bounds (8 MiB tail, settled release, daemon death), so the fix is a file, not a reader. -->

# A promoted run kept its output in process, and lost it anyway

MEASURED 2026-10-26, branch `promote-keeps-output` (package J1), on the operator's report:
*"when I background a lengthy bash run with Ctrl-o its output goes to /dev/null, causes agony,
should go to a tmp file in a scratch dir"*.

## The finding that reframes the report: nothing stops capturing at the promotion

The obvious hypothesis — that promoting a foreground run drops its pipe/pty reader, or
redirects it — is **false**, and a probe proved it before any code changed. `HostProcesses`
spawns one drain thread per stream (`drain()` → `job.capture.push`), and a promotion moves
*processes* between cgroups (`ScopeTree::migrate`) and nothing else: the drain thread, the ring
and the job's row are untouched. Measured: a `for i in 1..8; do echo line-$i; sleep 0.4; done`
promoted at 1.2 s returned `Backgrounded` with lines 1–4 in its body, and `job_output` a few
seconds later returned **all eight** (`produced=56, retained=56, dropped=0`).

So the operator's `/dev/null` is not a redirect. It is the **window**, which loses a promoted
run's output in three ways — and all three only bite once the run is in the background, which is
exactly when nobody is holding its result any more:

1. `DEFAULT_CAPTURE_BYTES` is 8 MiB per job, and the ring drops from the front past it. A
   lengthy build prints more than 8 MiB, so the head (then the middle) is gone for good.
2. `SETTLED_CAPTURE_BUDGET` (64 MiB across the host) **releases** a settled job's whole ring
   (`Capture::release`) — the row and the counts stay, the bytes do not.
3. The job table is the `HostProcesses`'s; a daemon restart takes every window with it. And a
   promoted run is the one kind that is *supposed* to outlive things.

## What was built

`promote` now opens `<scratch>/job-<id>.out` (**after** the migration is measured, never before —
a file for a move that failed is a promise nobody kept), seeds it with the retained window, and
from then on every byte the drain pushes goes to the ring **and** the file. `Capture` owns the
file handle, so there is one writer per job's bytes and no second path to drift. The record
carries it: `Promotion.output_file` and `JobView.output_file` (`OutputFile { path, from, bytes,
problem }`), `Promotion::summary()` names it (so `job_list`'s promotions section does too), the
`bash` result names the handle and the path, and `job_output` says where the whole of it is.

Two details that are load-bearing rather than tidy:

* **`from` is the window's own `dropped`, not 0.** A foreground run that already outgrew 8 MiB
  before the promotion has lost its head for good, and a file claiming to be the whole output
  would be a lie a reader could not check.
* **`Capture::release()` closes the spill's descriptor and keeps the file.** The box's soft
  `ulimit -n` is 256 (see `the-macs-256-open-files.md`), so a descriptor per settled promoted job
  is not free; the path and the byte count stay, because the bytes are still on disk.

## Where it is wired

`HostBackend::with_scratch_dir` — the daemon's ONE call (`harness::scratch_dir()`) — now sets the
host's copy too, so the tools and the process host cannot disagree about the directory. A VM
session needed one line of its own: `FirecodeBackend`'s `HostProcesses` is built inside
`firecode.rs`, and the drain is still *this* process's pipe, so
`harness.rs`'s `in_vm` branch calls `fc.host_processes().set_scratch_dir(&scratch)`.

## Remote promote already had this

`RemoteProcesses::promote` moves nothing, because a remote job is `setsid`-detached at launch and
its command is redirected into `{worktree}/.letibot-remote/jobs/{n}.out` **before it starts** —
the output was never in memory there, and `job_output` reads it with `tail -c` over the
connection. The note the promotion returns now says so and names the file.

## The rake

Do not go looking for the place a promotion "stops capturing". Nothing does. The question is
*what bounds the copy*, and the answer was three constants. A fix that had "restarted the drain"
would have looked right, tested green, and changed nothing.
