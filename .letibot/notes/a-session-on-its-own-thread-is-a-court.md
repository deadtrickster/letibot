<!-- abstract: The daemon serves each session on its own thread (`sessions::Court`, armed only by `Daemon::run`), and the doors that must know it - `open_as`, a post instead of an ask, a `Sessions` drop that joins its courts. -->

# One thread per session: the shape, and the four traps

`sessions::Court` (`crates/harnessd/src/sessions.rs`) is one session's thread: the hub is marked
`Owner::OwnThread`, the harness moves onto the thread, and it loops on
`hub.take_own_work_until(next_idle_at())`. `Sessions::serve_each_on_its_own_thread` arms it and
`Daemon::run` is the only caller, so a fixture that drives `Sessions` by hand still runs the
single-threaded path.

The doors that have to know a court exists. `open_as` and `open_the_gatekeeper` ask
`open.contains_key(id) || courts.contains_key(id)`, because `Registry::create` rings `Work::Open` and
by the time the daemon drains that ring the court may already hold the harness. A daemon-side pass
that wants a court's answer must not block on it: `Court::post` is fire-and-forget, `Court::ask` is
for `cli.rs` and tests.

`impl Drop for Sessions` takes the courts and joins them, bounded by `COURT_TEARDOWN`. A court owns its
session's `Harness` and `Parts`, so a `Sessions` that walked away was still freeing the vocabulary
when `exit()` ran: a SIGSEGV with no `panicked at` anywhere. `promote_midflight` is the canary.

`(ulimit -n 256; ...)` drops the HARD limit too - the gate's 256 is the soft one, so use `ulimit -Sn`.
A stub shared by several sessions that answers by arrival order is a race: answer by who is asking.
