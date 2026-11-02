<!-- abstract: A background `bash` gets `<scratch>/job-<id>.out` from its spawn while a deadline-bounded run gets its file only at promotion, and after a daemon restart the adopted row answers with an empty window and no file at all. -->

# A background job's file, and the half a restart still leaves

`HostProcesses::spawn` calls `spill_output` only for `req.background || req.tty`. A foreground run's
output is unbounded for the whole of its deadline, so a file behind it would remove the bound
`DEFAULT_CAPTURE_BYTES` exists for; it gets its file from the promotion instead, seeded, with
`from = dropped`. Pinned by `a_background_job_has_a_file_from_its_spawn_and_a_foreground_one_does_not`.

Close the spill at `waiter.close_spill()`, after the drains join, never at `settle`: `kill_job_as`
settles the job before it ends the cgroup, so a close at settle truncates a killed job's tail with
`problem: None`.

After a restart the row is NOT gone - `Harness::open` adopts every open process record - so
`job_output` says "still running and has written nothing yet" about a run that printed a gigabyte,
whose bytes sit unswept in the dead daemon's `scratch-<pid>/job-<id>.out`. Closing that needs the
path on `ProcessRecord`.
