<!-- abstract: MEASURED 2026-10-27: `ETXTBSY` is per INODE, so "write a script, chmod it, exec it" races every other `fork` in the same test binary — the forked child holds the write descriptor until its own exec, and load stretches that window; the tree's answer is `letibot_tools::testing::write_stub` (probe-exec until `ExecutableFileBusy` stops), the forced reproduction is four lines of `fcntl(fd, F_SETFD, 0)` plus a long-lived holder child, and eight fixtures still write their own executable inline. -->

# `ETXTBSY` is a fact about the INODE, so "write it, chmod it, exec it" races every fork in the binary

MEASURED 2026-10-27, from the merge gate's own words (reported by the operator, then reproduced
here): `crates/bridge/src/daemon.rs`'s `daemon::tests::a_daemon_the_bridge_started_is_reaped_when_it_exits`
failed under load with

```text
starting /tmp/letibot-reap-…/harnessd: Text file busy (os error 26)
```

## The mechanism, exactly

* `execve` answers `ETXTBSY` for a file **any process holds open for writing**. It is a property of
  the file, not of its name — so writing to a temp name and renaming into place does **not** help
  (`deny_write_access` is asked of the inode). Retrying is the only answer.
* The holder here is a **`fork` of the test process itself**: libtest runs 32 tests at once, several
  of them start processes, and a child forked while the fixture's write descriptor was open inherits
  it. Rust marks its own descriptors `O_CLOEXEC`, so the child drops it when it `exec`s — the window
  is `fork` → `exec`, microseconds on an idle box and milliseconds when the run queue is full. That
  is why the test passes alone and fails in the gate.
* A 200 ms sleep is not a fix and neither is a bigger one: the child that holds it is arbitrary.

## The tree already has the answer

`letibot_tools::testing::write_stub` (`crates/tools/src/testing.rs`) — write, chmod, then **probe by
really exec'ing it** until `io::ErrorKind::ExecutableFileBusy` stops, 100 tries at 20 ms. One
successful exec is proof that nobody holds a write descriptor and nobody can get one after it. Its
own doc records the 2026-10-20 measurement where a wrong *result* (a `gh` stub "could not be run")
was this race rather than a retry.

It is behind `letibot-tools`'s `testing` feature, which a **dev**-dependency turns on for a test
build (`crates/bridge/Cargo.toml` now does; `flowy`, `harnessd`, `sessionlog` and `tui` already did).
The probe is handed the stub's **empty argv**, so a stub whose real start must block has to answer
`[ $# -eq 0 ]` differently — the bridge's fixture daemon is
`#!/bin/sh\n[ $# -eq 0 ] && exit 0\nsleep 2\n`, and `daemon_args` never passes zero arguments.

## Forcing the race, so a fix can be measured 1 of 1 rather than 1 of 300

Clear `FD_CLOEXEC` on the write descriptor and start a holder that lives a second — the child then
keeps the descriptor across its own exec:

```rust
let mut f = File::create(&script)?;  f.write_all(BODY)?;  f.flush()?;
set_permissions(&script, Permissions::from_mode(0o755))?;
unsafe { fcntl(f.as_raw_fd(), F_SETFD, 0); }        // F_SETFD = 2
let _holder = Command::new("sleep").arg("1").spawn()?;   // inherits the writer, keeps it
drop(f);                                             // the holder is the only writer now
Command::new(&script).status()                       // -> Err(Text file busy (os error 26))
```

MEASURED on lab2x1 (a script is in `scratch/etxtbsy.rs` of the session that wrote this): the naive
`write, chmod, exec` failed **1 of 1** with the gate's exact sentence, and `write_stub`'s probe
**succeeded 1 of 1** after 1.005 s — the holder's own exit.

## The call sites that still write their own executable inline

Same shape, same race, none of them measured:

`crates/tools/tests/remote_confine.rs:280`, `crates/tools/tests/remote_hosts.rs:215`,
`crates/tools/tests/clauses.rs:646`, `crates/tools/src/firecode/held/tests.rs:126`,
`crates/tools/src/firecode.rs:1696`, `crates/tools/src/builtins/task/tests.rs:793`,
`crates/harnessd/tests/operator_shell.rs:496`, `crates/harnessd/tests/sudo_ask_live.rs:602`.

Most are inside the `letibot-tools` lib binary, which is the *worst* place for this: 1,108 tests over
32 threads, several of which start `sh`, `bwrap` and cgroup helpers — that binary is where the
window is widest.

## And the file's own name

The tool turned `exec'd` into `exec39d` when it made this note, and a note's name cannot be changed
once it exists — search this corpus for `ETXTBSY` or `write_stub` rather than for its filename.
