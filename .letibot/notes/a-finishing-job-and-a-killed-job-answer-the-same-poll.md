<!-- abstract: A status poll reads two files in one ssh round trip with a fork between them, so a job that finishes inside that window answers "no status" then "pid gone" - the same pair a killed job gives. -->

# A job that finishes inside the poll answers like a job that was killed

The remote status question is one ssh round trip reading two files: the exit status first, then the pid
(`kill -0`), with a fork between them. A launcher that finishes inside that window is read as
status-empty and then pid-gone - letter for letter what a killed launcher answers - so `refresh_within`
settled `Killed`. The launcher writes its status BEFORE it exits, so pid-gone with no status is sound
only if the status was read after the pid.

`crates/tools/src/remote.rs` now cats the status file once more before calling it a kill (a look that
did not answer settles nothing), and `kill` sets `armed_kill` before the signal so its own settle loop
attributes the ending to the caller instead of to a mystery.

The rake: two reads in one script are not one instant, and the pair can be a combination no real state
produces. Put the read that is a precondition first, or ask again once. `builtins/scenario.rs` had the
same shape - it asked `fleet.settled(&running)` before the kill and printed that snapshot as the role's
fate, so a role that finished on its own was reported as stopped.
