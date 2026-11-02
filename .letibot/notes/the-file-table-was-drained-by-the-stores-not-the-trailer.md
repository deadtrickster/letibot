<!-- abstract: Neither suspect in the Mac's ENFILE incident holds a descriptor; the sites that do hold one past their subject are named here, and ENFILE excludes no single process. -->

# Neither suspect holds a descriptor, and the only measured drain is the store's

The two suspects named when the Mac hit `ENFILE` in 2026-10-27 are both innocent. The session env
trailer is text end to end, and the only descriptor on that path belongs to `ShellSession`, which has no
production caller - grep it outside `exec/shell.rs` and the answers are a `pub use`, doc comments and
`crates/tools/tests/session_env.rs`. The Seatbelt (`exec/seatbelt.rs`) holds paths, not handles, and its
leak is FILES: `probe_with` writes `profile-<n>.sb` into `$TMPDIR/letibot-seatbelt-<pid>/` per probe and
nothing removes them.

The sites that do hold a descriptor past their subject: `ShellSession::close` (`shell.rs:998`) sets a
flag and returns - the master goes only in `Drop`; `HostProcesses.jobs` keeps every job the host ran
(`host.rs:1013`), so a `tty` job's stdin lives as long as the daemon; `run_bounded`'s timeout arm
detaches both drains (`rpc.rs:604`); and `Harness::open` opens the store four times plus one per
monitor-set change, released only when the process's last connection to the file goes.

`ENFILE` excludes nothing: `kern.maxfilesperproc` is 245760 (`scripts/letibot`), so one process that
raises its soft limit reaches the box's table long before its own.
