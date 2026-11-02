<!-- abstract: `Seat::Coder` and `Seat::Runner` need a confined execution backend that cannot be built on Darwin, so a harnessd test whose fixture seats `coder` measures the absent boundary instead of its subject. -->

# A test fixture that seats `coder` cannot open on macOS — seat `leticode`, or set `cfg.unconfined`

`needs_exec_backend()` (`crates/harnessd/src/config.rs`) is true for those two seats, and
`Harness::open` builds `HostBackend::confined_granting` for them, which cannot be built anywhere
on Darwin.

Where the seat is incidental - messaging, lifecycle, subagent threads, a tool arriving in the
prompt - seat `Seat::Leticode`. It carries the same trio (`task`, `task_result`,
`task_message`), `write`/`edit`/`grep`/`glob` and `bash` behind `allow_bash`, and it is
unconfined by design because opencode's model has no workspace boundary. It is the seat a Mac's
own daemon runs.

Where the seat IS the subject - its tools, its trail, its adjudicator, its denials - set
`cfg.unconfined = cfg!(target_os = "macos")` on a `Seat::Coder` config, as `wired.rs`'s
`seat_config()` does. That is the field a `leticode` subagent inherits. Such a test cannot
measure confinement, so nothing in it may assert one.

Where the boundary is the subject, `apparatus::applies(what, cfg!(target_os = "linux"))` -
absent apparatus as a sentence, not as a failed assertion.
