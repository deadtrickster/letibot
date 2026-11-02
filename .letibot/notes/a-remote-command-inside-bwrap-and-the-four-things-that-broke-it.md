<!-- abstract: Four things break when a remote command is wrapped in bwrap: `--new-session` puts the sandbox outside the process group the kill targets, the keep-list must be the far shell's, an empty `$HOME` breaks rustup, and `backend_confined` was a `true` nobody measured. -->

# A remote command inside bwrap, and the four things that broke it

`--new-session` costs you the group kill. A remote command and a remote job are both ended by `kill -TERM -<pgid>`, and the flag moves the sandbox out of that group, so the signal reaches the launcher and the work keeps running. It is `exec::Session::Detached` for a local spawn and `Kept` for a pane's pty and every remote spawn.

The keep-list must be the FAR shell's. `--clearenv` puts back `KEEP_ENV`, whose values read here are this box's, and a Mac's `PATH` inside a Linux sandbox names directories that do not exist. `EnvSource::Remote` emits `--setenv PATH "$PATH"` as a shell piece; a literal would put the five characters `$PATH` in the environment.

An empty `$HOME` leaves rustup unable to choose a cargo, because the proxy looks for its toolchain under `$HOME/.rustup`. Point `CARGO_HOME`/`RUSTUP_HOME` at the read-only grants.

`backend_confined` was a `true` nobody measured: `Harness::open`'s remote arm returned it for a placement that confined nothing, and that bool is the `Confinement` prerequisite `allow-all` requires. It is `in_vm || backend.boundary().is_some()` now, and a remote probe cannot be `Bwrap::probe`.
