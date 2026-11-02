<!-- abstract: Darwin ships BSD's `/usr/bin/su`, which has no `-c`, so a fixture that starts a run with `su -c true` has no run in flight at all - and on a Mac `waiting_for_an_answer` is `Waiting::Unreadable` unconditionally, because the signal it reads is `/proc`'s. -->

# `su -c true` is a Linux command; on the Mac it is a usage error in four milliseconds

`/usr/bin/su: illegal option -- c\nusage: su [-] [-flm] [login [args]]` and exit 1 - so the fixture's
run finished before it began and nothing was ever in flight. The test's premise was Linux's: a root
process blocked on the pty the daemon holds, so `/proc/<pid>/fd/0` answers `EACCES`. util-linux's `su`
reads its password from that pty and blocks; Darwin ships BSD's, whose usage line is the whole truth.

`letibot_tools::exec::ask::waiting_for_an_answer` (`crates/tools/src/exec/ask.rs:50`) is why the same
state needs a different command per box: on a Mac it answers `Waiting::Unreadable` unconditionally,
because the signal it would read is `/proc`'s and there is none - so any run that blocks is the same
state, and `cat` reading the pty is the plainest one. On Linux the run must contain a process of another
uid.

One more thing beside it: a test that panics before stopping its daemon leaves the daemon up with its
socket bound, so hold it in a `struct Daemon(Child)` whose `Drop` kills and waits
(`crates/harnessd/tests/stop_ends_runs.rs:109`).
