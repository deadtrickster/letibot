<!-- abstract: Nothing stops capturing when a run is promoted - the bytes are lost to the window's three bounds, not to a dropped reader - so the fix is a file; a promoted run gets `job-<id>.out`, whose `from` is the window's own dropped count. -->

# A promoted run kept its output in process, and lost it anyway

The obvious hypothesis - that promoting a foreground run drops its pipe reader - is false. `HostProcesses` spawns one drain thread per stream, and a promotion moves processes between cgroups and nothing else.

The `/dev/null` the operator watched is the window, which loses a promoted run's output three ways, all of them only after the run is backgrounded: `DEFAULT_CAPTURE_BYTES` is 8 MiB per job and the ring drops from the front; `SETTLED_CAPTURE_BUDGET` releases a settled job's whole ring; and the job table is the process's, so a restart takes every window with it.

So `promote` opens `<scratch>/job-<id>.out` after the migration is measured, seeds it with the retained window, and from then on every byte the drain pushes goes to the ring and to the file. `OutputFile { path, from, bytes, problem }` travels on the record, and `from` is the window's own `dropped` rather than 0, because a file claiming to be the whole output would be a lie a reader cannot check. `Capture::release` closes the descriptor and keeps the file, since the soft `ulimit -n` is 256.

Rake: do not look for the place a promotion stops capturing. Ask what bounds the copy.
