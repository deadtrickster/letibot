<!-- abstract: A room is restarted through the launcher and never by the daemon that noticed it went - nothing inside a daemon may fork another daemon, and the launcher writes the record `--stop` reads. -->

# The room's supervisor is the launcher, and on this branch it has no verbs

`harnessd --room` is a whole daemon (`crates/harnessd/src/room/coordinator.rs`), and nothing
inside a daemon may fork another daemon, so the restart is the launcher's:
`room::supervise::Launcher` runs `<launcher> --room --stop` then `<launcher> --room`. A launcher
that predates those verbs refuses with `unknown flag: --room`, as `room_restart_refused`. They
have landed since: `scripts/letibot`'s `room_ensure` (`:1637`).

Why the launcher and not a spawn of our own: the launcher writes the record `--ls` and `--stop`
read, so a coordinator the daemon started itself would have no record and `--stop` would not see
it.

The restart marker is keyed on the lease's `started_ms` (`<socket>.lease`,
`CoordinatorLease::write`) - a number two readers compare - and not on `Server::identity()`,
which is prose whose age moves every second.

Rakes. `write_stub`'s probe is an EXEC with no arguments, so a launcher fixture that logs `"$*"`
records the probe as an empty first line: put `[ -n "${LETIBOT_STUB_PROBE:-}" ] && exit 0`
first. And a feed test that waits for `unreadable.is_none()` but not for the item can exit on a
legitimately empty snapshot, because the reader's first read is taken at connect.
