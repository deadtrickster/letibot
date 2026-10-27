<!-- abstract: MEASURED 2026-10-26 (RH3): wrapping a remote command in bwrap has four traps — `--new-session` kills the process-group kill, the keep-list must be the far shell's, an empty $HOME makes rustup unable to choose a cargo, and the remote arm of Harness::open hardcoded `backend_confined = true` so `allow-all`'s Confinement prerequisite was satisfied by nothing. -->

# A remote command inside bwrap, and the four things that broke it

MEASURED 2026-10-26, branch `remote-confine` (RH3), `crates/tools/src/exec/confine.rs` and
`crates/tools/src/remote.rs`. Each of these is a real defect that landed and was found, not a
hypothesis.

## `--new-session` is the one flag that costs you the group kill

A remote command and a remote job are both killed by `kill -TERM -<pgid>`: `grouped()` puts the
command in a `setsid` shell whose pid is its group, and the launcher is `setsid`'d for the same
reason. `bwrap --new-session` moves the sandbox **out of that group**, so the group signal reaches
the launcher and the work keeps running. The flag is now `exec::Session` — `Detached` for a local
spawn (`TIOCSTI` has a shell to reach there), `Kept` for a pane's pty and for every remote spawn
(stdio is pipes and files, so there is no terminal to protect). The probe measures the
`--new-session` argv because a local spawn asks for it; the difference is one flag and it is
stated rather than measured twice.

## The keep-list must be the FAR shell's

`--clearenv` puts back `KEEP_ENV`, and its *values* read here are this box's: a Mac's `PATH`
inside a Linux sandbox is a list of directories that do not exist. The builder emits typed pieces
now, and `EnvSource::Remote` emits `--setenv PATH "$PATH"` as a `Piece::Shell`, joined verbatim so
the far shell expands it. A literal would put the five characters `$PATH` in the environment —
silent, and only visible as `command not found` for everything.

## An empty `$HOME` makes rustup unable to choose a cargo — measured on lab2x1

With `$HOME` the session's own directory: `cargo --version` →
`error: rustup could not choose a version of cargo to run, because one wasn't specified
explicitly, and no default is configured.` `cargo` is on `PATH` (the far shell's), but the
*proxy* looks for its toolchain under `$HOME/.rustup`. Fix: set `CARGO_HOME`/`RUSTUP_HOME` (and
mise's data/state variables) to the read-only grants, which is also what makes a build work with
everything already installed. A read-only `CARGO_HOME` **does** build (measured, warm registry);
what it cannot do is fetch a *new* dependency.

## `backend_confined` was a `true` nobody measured

`Harness::open`'s remote arm returned `(placeholder, true)` for a placement that confined nothing.
That bool is `Prereq::Confinement`, which is what `allow-all` requires — so a remote session could
sit at the coordinate whose whole promise is that a structural boundary decides. It is
`in_vm || backend.boundary().is_some()` now (`ExecBackend::boundary`, by value because a local
grant replaces the confinement behind a lock). The same fact feeds
`AdjudicatedGate::with_boundary`, which replaced a hardcoded card sentence —
*"the host filesystem is not sandboxed; §11.4's boundary arrives with firecode"* — that was false
for every confined session, local ones included.

## Two smaller ones, both of which cost a run

**A remote probe cannot be `Bwrap::probe`.** The helper is there, the namespaces are that kernel's,
and the denominator (which inode means *not entered*) is that box's own `/proc/self/ns/*`. So
`probe_script` prints the outer map first and the readback second, in one ssh call, and
`measured_from_probe` compares them with the same `state_of` the local probe uses.

**The probe cache is keyed by host name, so two test rigs sharing a name share one answer** — the
same shape as `MISE`'s. My "this host has no bwrap" rig read the "confined" answer a different rig
had cached, and the test failed with the opposite of what it asserted. Each rig names its own host
(`conf-<tag>`).

**How to test this without a kernel:** put a `bwrap` shim behind the fake ssh that answers
`--version`, prints a **fabricated readback** in the probe's own vocabulary (`ns <n> <inode>`,
`seal <n> <verdict>`), and otherwise runs the command after `--` with the `--clearenv`/`--setenv`
environment it was handed. The assertions are then about the argv that crosses the connection and
they hold on a Mac with no bwrap at all — `crates/tools/tests/remote_confine.rs`.
