<!-- abstract: READ 2026-10-31: neither suspect (EV env trailer, R98 Seatbelt) opens or holds a descriptor — the trailer is a String and ShellSession has no production caller, the seatbelt holds paths and its leak is FILES (profile-N.sb), not fds. The one drain the tree has MEASURED is the store's: 4 WAL connections per Harness::open, descriptors surviving a session's close until the process's last connection goes (20 sessions → 92 fds). ENFILE rather than EMFILE is the proof that no single process was it: room_for (10-26) raised a test binary to 4096 on a Mac whose hard limit is unlimited, so the sum across concurrent binaries reaches kern.maxfiles — one day after the loud 256 failure was silenced. -->

# The file table was drained by the stores, not by the trailer

READ 2026-10-31, branch `agent/who-drained-the-macs-file-table`, against the tree at
`4fb39324`. The incident: 2026-10-27, `ENFILE` on the Mac at 13:05, 14:12 and ~17:15 local,
the last within a minute of one test in `wt-ev`, each forcing a reboot. Two suspects were
named then. **Both are innocent, and each is innocent for a reason you can read in ten
lines.** The one descriptor drain this tree has *measured* is the store's, and `ENFILE`
rather than `EMFILE` is itself the evidence that no single process was the culprit.

## (a) The EV session env trailer holds no descriptor at all

The trailer is a shell function that prints `\033]1337;letibot;<status>;<cwd>\n<NAME=v;…>`
(`exec/shell.rs:560-578`), and the daemon side of it is text:

- `take_turn` scans a `Vec<u8>` (`shell.rs:633`), `take_env` is
  `self.env = Some(turn.env.clone())` (`shell.rs:909-913`), `Turn::env` is a `String`.
- `intent::env::SessionEnv::observe` takes the report as `&str`. Nothing in
  `intent/env.rs` opens a file.

The only descriptor the path can touch is the `ShellSession`'s pty, and **`ShellSession`
has no production caller** — grep `ShellSession` outside `exec/shell.rs` and the answers are
`exec/mod.rs`'s `pub use`, two doc comments, and `crates/tools/tests/session_env.rs`. The
`bash` tool is a fresh `/bin/sh -c` per call (`exec/host.rs::spawn`). So the trailer cannot
have drained the box, and neither can its shell: at most one pty per session, in a test.

Its own close path *is* broken, though — see the audit below. It just cannot be this.

## (b) The Seatbelt holds paths, not handles — and its leak is FILES

`pub struct Seatbelt { helper: PathBuf, profile: PathBuf, home: PathBuf, tmp: PathBuf,
cargo_home: Option<PathBuf>, rustup_home: Option<PathBuf>, boundary: Boundary }`
(`exec/seatbelt.rs:517-527`). No `File`, no `OwnedFd`. `wrap_with` builds an **argv**
(`sandbox-exec -f <profile> …`), and the one child the module ever starts is the probe's,
through `.output()` — which waits, reaps and closes both pipes (`seatbelt.rs:607-626`).

What it *does* leak is files: `probe_with` writes `profile-<n>.sb` into
`$TMPDIR/letibot-seatbelt-<pid>/` on **every** probe, and nothing removes them
(`seatbelt.rs:587-595`, `profile_dir`/`next_profile` at `:1016-1038`). A probe happens per
backend construction (`backend.rs:663`) **and per grant that widens the view**
(`backend.rs:532` `with_session_tmp`, `:727` `grant_into_view`), so the files accumulate per
session and per granted path — a directory that grows, not a table that fills. `+5.1 ms` per
command and one extra process per command is what R98 costs; it costs no descriptor.

## The audit: every path in the six named areas

| path | opens | closes on every path? |
|---|---|---|
| `ShellSession::open` (`shell.rs:760-873`) | 1 pty (2 fds) + 3 slave clones | **yes** — `into_master` drops the slave, `drop(cmd)` drops the three `Stdio`, and the readiness `Err` arm calls `close()` before returning |
| `ShellSession::close` (`shell.rs:998-1015`) | — | **NO.** It sets `blocked` and returns. The comment at `:1002` says *"the master read would block for ever, so the descriptor goes"* — the descriptor does not go. `self.master` is a plain `File` field and is released only by `Drop` (`:1094-1102`). `close()` is not a close. |
| the trailer / `intent::env` | nothing | n/a |
| `Pty::open` (`pty.rs:187-248`) | `posix_openpt` + `open(slave)` | **NO, on one error path**: `fcntl(slave_fd, F_SETFD, FD_CLOEXEC) != 0` returns `Err` while `slave_fd` is still a raw `c_int` — one fd lost (`:238-240`). The same shape is in `region.rs`'s `unlinked_file` for `ro` (`:443-457`). |
| `TermSession` (`tools/exec/term.rs:530-646`) | master + a reader clone | yes on drop (`:741-749`), but the daemon **keeps a finished pane until the next `open`** (`harnessd/term.rs:255-259`) — one master per session, bounded |
| `remote.rs`'s masters | a ControlMaster *process* per host per daemon, `ControlPersist=120`; a stale one is `remove_file`d, not killed (`:2200-2220`) | yes for this process — `rpc::run_in_scope` is a `thread::scope` and joins. **Except the timeout arm**, which detaches both drains (`let _ = (out, err)`, `rpc.rs:604-608`): 2 pipe fds until the killed child's ends close. |
| the job capture file | 1 `File` per background/`tty` job (`host.rs:1884-1886`) | **yes** — `waiter.close_spill()` after both drains join (`host.rs:1935`), and the settled-window budget calls the same |
| the job's `Stdin` | 1 fd per `tty` job (a dup of the pty master, `host.rs:1823-1838`) | **NO.** `HostProcesses.jobs` keeps every job the host ever ran (`host.rs:1013-1019`) and nothing removes one, so a `!` run's stdin handle is held for the daemon's life. The capture is budgeted; the stdin is not. |
| the store | 4 WAL connections per `Harness::open` | **NO** — measured, and this is the one that matters |

## The one that does not close, and the numbers the tree already took

`crates/harnessd/tests/store_handles.rs:171-234` measures it and
`.letibot/notes/a-stores-descriptors-outlive-every-close-but-the-last.md` states it:

- `Harness::open` opens the store **four times per session** — its own, `corpus`,
  `transcript_source`, `decision_source` — each a WAL connection at ~2 descriptors.
- Every `Store` *is* given back (`Store::live()` 85 → 5). The **descriptors are not**:
  20 sessions opened and closed left **92 descriptors on `sessions.db`**, ~4 per session,
  released only when the process's *last* connection to that file goes.
- The operator's daemon, 2026-10-30: 253 descriptors of which **210 on `sessions.db`/`-wal`**;
  23 connections at 1 m 20 s, 105 at 1 h. Two samples ten minutes apart were identical.

That is the only bucket in the tree's own census that is large. Everything else in it —
sockets, pipes, kqueues — was 11.

## Why ENFILE, and why the three timestamps

`EMFILE` is one process out of room. `ENFILE` is **the box** out of room: `kern.num_files`
against `kern.maxfiles`, shared by every process. So the culprit cannot be a process that
was capped at its own limit — and until 2026-10-26 12:23 (`a43d2748`) every test binary *was*
capped at Darwin's soft 256 and failed **loudly** at 256. That fix is
`crates/harnessd/tests/support/room.rs:37`: `WANT = 4096`, and on this Mac `ulimit -Hn` is
**unlimited**, so a harnessd test binary asks for 4096 and gets it.

So from 10-26 the loud per-process failure became a quiet 4096-descriptor allowance, while
the drain — 4 store connections per session, held until the process exits — kept making a
binary's count a function of **how many sessions it had ever opened**. Add them up: several
harnessd test binaries at once, each allowed 4096, plus the daemons (256 apiece on 10-27),
plus the box's own. That sum is what `kern.maxfiles` sees, and no rlimit prevents it.

The timestamps fit that and not a test's own leak: three heavy runs in one day, the third at
17:15 where one test in `wt-ev` was the process whose first `open()` was handed the errno.
The test did not leak 20 000 descriptors; it was the straw. And each time it took a reboot
because the daemons and test processes holding them stayed up.

## What confirms it on the Mac in one run

Two lines, and the second is the one that would have named the holder on the day:

```sh
sysctl -n kern.maxfiles kern.maxfilesperproc kern.num_files
sudo lsof -nP 2>/dev/null |
  awk 'NR>1 && $4 ~ /^[0-9]+/ {n[$NF]++} END {for (f in n) printf "%6d %s\n", n[f], f}' |
  sort -rn | head -20
```

The first says whether a full table is a process's rlimit or the box's. The second is the
whole table **by file across every process** — the census `fdwatch` never took (it counted
holders). `sessions.db`/`-wal` at the top means the store; `/dev/ptmx` means ptys; `TCP`/
`unix` means sockets.

Then the mechanism, in one command — the same store path ten times, watching the box's own
counter:

```sh
cd ~/Projects/letibot/letibot
BIN=$(ls -t target/debug/deps/store_handles-* | grep -v '\.d$' | head -1)
for i in $(seq 10); do "$BIN" --test-threads=1 >/dev/null 2>&1; printf '%s ' "$(sysctl -n kern.num_files)"; done; echo
```

Flat means the descriptors die with the process and the ENFILE is a **sum over concurrent
processes** — which is this note's claim, and `seq 10 | xargs -P 10` re-running the same line
is the reproduction. Climbing means a child outlives its parent and holds them, which would
be a different finding and the one to chase next.

## The rake

**`ENFILE` is not a bigger `EMFILE`.** A per-process limit that is raised (256 → 4096, and
the launcher's 8192 on 10-30) converts a loud per-process failure into a silent contribution
to a *shared* ceiling, and the symptom then arrives somewhere else entirely — in whichever
unrelated process next asks the kernel for a descriptor. When a table fills, ask for the
census **by file**, not by process: the process that reports the errno is never the one
holding it.
