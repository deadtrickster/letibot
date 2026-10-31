<!-- abstract: READ 2026-10-31: neither named suspect holds a descriptor — the EV session env trailer is a String end to end and ShellSession has no production caller; the R98 Seatbelt holds paths and leaks FILES (profile-N.sb per probe), not fds. The only *measured* drain in this tree is the store's: four WAL connections per Harness::open (harness.rs:3416/3432/4626/4657) plus one per monitor-set change (:12009), whose descriptors survive a session's close and are released only when the process's last connection to the file goes (20 sessions → 92 fds, ending with the process). That does NOT explain the 2026-10-27 ENFILE, and the cause is not established: kern.maxfiles was not measurable from the tree and the incident's two artifacts are absent from it. ENFILE excludes no single-process leak — kern.maxfilesperproc is MEASURED 245760 (scripts/letibot:86) — so the census, not the errno, is what settles it. (The FILE's name is an earlier reading's claim; the title is the correction.) -->

# Neither suspect holds a descriptor, and the only measured drain is the store's

READ 2026-10-31, branch `agent/who-drained-the-macs-file-table`, against the tree at
`4fb39324`. The incident: 2026-10-27, `ENFILE` on the Mac at 13:05, 14:12 and ~17:15 local,
the last within a minute of one test in `wt-ev`, each forcing a reboot. Two suspects were
named then.

**What this note proves:** both suspects are innocent, for reasons visible in the code, and
the only descriptor drain this tree has *measured* is the store's.
**What it does not prove:** that the store drained the Mac's table. **The cause of the
2026-10-27 incident is not established here**, and this note does not name it.

> **The file's name is an earlier reading's claim, not this note's.** A note's name cannot be
> changed once it exists, so the title above is the correction and
> `the-file-table-was-drained-by-the-stores-not-the-trailer` is the stale half. Search this
> corpus for `ENFILE`, `kern.num_files` or `fdwatch` rather than for the filename.

## What was not available, so what kind of answer this is

Two artifacts the report of the day rested on are not reachable from this tree:

- `queue-after-restart.md` — `find . -name '*queue-after-restart*'` matches nothing.
- `~/.cache/fdwatch.log`, and `~/.local/bin/fdwatch.sh` itself — both absent on this box.

So **`kern.maxfiles` was never read**, and the incident's holder snapshot (the thing the
watcher took "past 20k") was never read. Every number below is either the tree's own
MEASURED record or a count taken from source. Nothing here is a measurement of the Mac's
table, and the confirmation command in the last section is still outstanding.

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
| the store | 4 WAL connections per `Harness::open` — `harness.rs:3416` (`Store::open`), `:3432` (`StoreCorpus::open`), `:4626` (`StoreTranscripts::open`), `:4657` (`StoreDecisions::open`) — **plus one per monitor-set change at `:12009`** (`sync_monitor_records`, opened on a wake whenever the live set moved) | **NO** — the descriptors outlive the session and the `Store`, measured below |

## The store, and exactly what its measurement says

`crates/harnessd/tests/store_handles.rs:171-234` measures it and
`.letibot/notes/a-stores-descriptors-outlive-every-close-but-the-last.md` states it:

- `Harness::open` opens the store **four times per session**, each a WAL connection at ~2
  descriptors, and `sync_monitor_records` opens a fifth whenever the live monitor set moves.
- Every `Store` *is* given back (`Store::live()` 85 → 5). The **descriptors are not**:
  20 sessions opened and closed left **92 descriptors on `sessions.db`**, ~4 per session,
  released only when the process's *last* connection to that file goes.
- The operator's daemon, 2026-10-30: 253 descriptors of which **210 on `sessions.db`/`-wal`**;
  23 connections at 1 m 20 s, 105 at 1 h.

**Read that as the bounded cost it is.** That test asserts the count it expects
(`5*20+8`-shaped, "the same as one session plus twenty sessions' worth") and it passes: what
it measures is a *retention that ends with the process*, not a leak that fills a table. The
bounded shape is the point, and it is also why the store cannot be named as this incident's
cause on this evidence.

## Why the timestamps do not fit the store — and what is left open

The note's own facts argue against the store as the explanation, and they should be written
down rather than smoothed over:

- **A test process hands its descriptors back when it exits.** The holders that survive for
  hours can only be the long-lived daemons — and the one that was measured held **253**
  descriptors after an hour. That is orders of magnitude below any table whose exhaustion
  produces `ENFILE`.
- The `ENFILE` events each forced a **reboot**, which says the holders were persistent
  processes, not the test process that reported the errno.
- `room_for` is called by **three test files only** — `store_handles`, `restart_restore`,
  `session_lifecycle` — and raises to `WANT.min(hard)` = 4096 (`tests/support/room.rs:37,63`).
  Every other test binary stays at Darwin's soft 256. So "several harnessd test binaries at
  once, each allowed 4096" describes three binaries, not a workspace run, and ~150 binaries
  at 256 summed as much before the 2026-10-26 raise (`a43d2748`) as after it — so that raise
  cannot be the dating of 10-27 either.

**So: the cause of the 2026-10-27 incident is not established.** What the audit above gives
is a set of paths that hold descriptors past their subject, and the store is the largest of
them by two orders of magnitude in the tree's own census.

## The correction that matters: `ENFILE` excludes nothing

An earlier reading of this note argued that `ENFILE` — the box's table, not one process's
rlimit — proves no single process was the culprit. **That is false, and the tree says so.**
`scripts/letibot:86` records `kern.maxfilesperproc` as **MEASURED 245760**, and on this Mac
`ulimit -Hn` is `unlimited`: a process that raises its own soft limit (the launcher's 8192,
`room_for`'s 4096, or `unlimited`) reaches the **global** table long before its own and is
handed `ENFILE` for a leak of its own. That is precisely the shape the brief named as the
strongest evidence — *a loop that leaks in one process* — and the errno is not evidence
against it.

## What confirms it on the Mac in one run

Two lines, and the second is the one that would have named the holder on the day:

```sh
sysctl -n kern.maxfiles kern.maxfilesperproc kern.num_files
sudo lsof -nP 2>/dev/null |
  awk 'NR>1 && $4 ~ /^[0-9]+/ {n[$NF]++} END {for (f in n) printf "%6d %s\n", n[f], f}' |
  sort -rn | head -20
```

The first reads the number this note could not. The second is the whole table **by file
across every process** — the census `fdwatch` never took (it counted holders). `sessions.db`/
`-wal` on top means the store; `/dev/ptmx` means ptys; `TCP`/`unix` means sockets.

Because one process is *not* excluded, the loop has to be watched from inside it as well as
from the box:

```sh
# one process, N times the same path, its own descriptor count — Darwin's /proc/self/fd
BIN=$(ls -t ~/Projects/letibot/letibot/target/debug/deps/store_handles-* | grep -v '\.d$' | head -1)
"$BIN" --test-threads=1 --nocapture 2>&1 | grep -E 'while open|in the process'
sysctl -n kern.num_files
```

and the box-wide version, which is what turns a per-process climb into an `ENFILE`:

```sh
cd ~/Projects/letibot/letibot
for i in $(seq 10); do "$BIN" --test-threads=1 >/dev/null 2>&1; printf '%s ' "$(sysctl -n kern.num_files)"; done; echo
```

Flat across the ten means descriptors die with the process; climbing within one run means a
single process is the holder, and then `lsof -p <that pid>` names the file.

## The rake

**`ENFILE` is not a bigger `EMFILE`, and it does not exonerate a single process.** A soft
limit that can be raised (256 → 4096 → 8192, and `unlimited` means 245760 here) lets one
process reach a *shared* ceiling and be told `ENFILE` for a leak that is entirely its own.
When a table fills, take the census **by file** and also watch the suspect process's own
`/dev/fd` — the errno names neither the culprit nor the scope.
