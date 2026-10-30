<!-- abstract: MEASURED 2026-10-30: the daemon serving this worktree (pid 99166) held 253 descriptors with the highest numbered 254 of a 256 soft limit, so a reviewer's `git checkout` died of EMFILE; 210 of those 253 were `sessions.db`/`-wal`; two samples ten minutes apart were identical, so it is a plateau, not per-call growth; `lsof -p PID | wc -l` is 277 for the same process because it counts `txt`/`mem` mappings, and `ulimit -n N` moves the HARD limit as well as the soft one. -->

# The launcher raises the daemon's open-file limit — and `lsof -p | wc -l` is not a descriptor count

MEASURED 2026-10-30 on the operator's Mac, branch `agent/the-launcher-raises-the-file-limit`,
from the incident the operator reported: a reviewer for `agent/a-one-line-citation-is-filled`
could not start —

```
git checkout --quiet --detach 0200fe9a…: Too many open files (os error 24)
```

## The numbers, as measured

| | |
|---|---|
| `ulimit -Sn` in a shell | **256** |
| `ulimit -Hn` / `launchctl limit maxfiles` | **unlimited** (`256 unlimited`) |
| the daemon pid 99166, `ps -o etime=` | **1 h 02 m** (the brief said eleven hours — it had been restarted since that was written) |
| its descriptors (`lsof -p 99166`, FD column numeric) | **253**, highest numbered **254** |
| its `lsof -p` ROW count | **277** |

So it was three descriptors from the ceiling, which is why a forked `git` could not open a
file — and why, while writing this change, **my own `bash` tool calls failed three times** with
`the process did not start: Too many open files (os error 24)`. A tool call's child is one of
the processes that inherits the limit; the failure is not confined to reviewers. A spawn needs
several descriptors at once (a pty pair, pipes, the output file), so three free ones is a coin
toss per call rather than a clean failure.

## The census of those 253, which is the interesting part

| bucket | count |
|---|---|
| `sessions.db` | **105** |
| `sessions.db-wal` | **104** |
| `sessions.db-shm` | 1 |
| the token ledger's temp files (`letibot-tokens-…lgydiW`, two fds each, 11 files) | 22 |
| `metrics.db` + `-wal` + `-shm`, logs, a job's `.out` | ~6 |
| unix sockets / pipes / `IPv4` / `kqueue` | 11 |

**210 of 253 are one sqlite database.** A harness opens two connections (*"two connections is
the store's declared shape"*) and sqlite in WAL keeps db + `-wal` + `-shm`, so 105 handles on
`sessions.db` is ~52 harnesses' worth. A handful of sessions were LIVE; the count tracks how
many have been OPENED since the daemon started, not how many are running.

**Two samples ten minutes apart were identical — 253 descriptors, 105 `sessions.db` handles
(23:00 and 23:10:15) — with a dozen tool calls and two full `tests/launcher.sh` runs in
between.** So it does not grow per tool call or per turn: it is a plateau a daemon reaches
within an hour of ordinary work, and what made it fatal is that the plateau sits at the limit.
Whether the plateau is a leak (connections never released when a harness ends) or the honest
count of ~52 harnesses that are all still alive is **not measured** — that needs a census of
live sessions against the connection count, which is a different investigation. Do not call it
a leak on this evidence.

## Rake 1 — `lsof -p PID | wc -l` counts the executable and its libraries

277 rows, 253 descriptors: the 24-row difference is `txt` (22 — the binary and its dylibs),
`cwd`, `rtld`. The operator's brief says the daemon "has 271 open files", which is this shape
of number. The FD column is the fourth field; `$4 ~ /^[0-9]+/` is the count that means what
`RLIMIT_NOFILE` means.

## Rake 2 — `ulimit -n N` sets the HARD limit too

MEASURED here, in one bash 3.2 and one bash 5.3:

| call | after |
|---|---|
| `ulimit -Sn 8192` | `soft=8192 hard=unlimited` |
| `ulimit -n 8192` | `soft=8192 **hard=8192**` |
| `ulimit -Sn unlimited` | `soft=unlimited hard=unlimited` |

Lowering the hard limit is silent and irreversible for that process tree, and a daemon whose
hard limit is 8192 can never be given more. `a-session-on-its-own-thread-is-a-court.md` (trap 4)
already had this rake from the other side: a fixture's `(ulimit -n 256; …)` dropped the hard
limit and made its own room check fail.

`unlimited` is not the number to ask for either: it means `kern.maxfilesperproc`
(**MEASURED: 245760** here), so a leak would hold a quarter of a million descriptors before
anything said so.

## What was built, and where the raise lives

`scripts/letibot`, right after `set -euo pipefail`, before anything is forked:
`raise_open_files 8192` — `ulimit -Sn` up to `min(8192, hard)`, never lowering a limit already
above the request, one line naming got/asked/hard when the box gives less. Because rlimits
survive `fork` and `exec`, that one call reaches the daemon (through `detach`: perl's
`setsid`+`exec` on a Mac, `systemd-run --scope setsid` on Linux), a one-shot, every
`letibot-tui` — the head and the `--probe`/`--list-sessions` utility calls — and every `bash`,
`git` and `cargo` the daemon runs in a turn. It is in the LAUNCHER and cannot be in the daemon:
a soft limit may only be raised to the HARD limit a process was born with.

The cost, said plainly: a process that leaks descriptors now leaks them at 8192 instead of
dying at 256. The symptom arrives later and 8192 is the number to grep for when it does.

## The four cases the function was exercised with

Run under both `/bin/bash` 3.2.57 and bash 5.3.20, each in a subshell because a lowered HARD
limit cannot be undone:

| hard | soft before | after | said |
|---|---|---|---|
| unlimited | 256 | 8192 | nothing |
| 1024 | 256 | 1024 | one line: got 1024, not the 8192 asked for (hard 1024) |
| unlimited | 20000 | 20000 | nothing — never lowered |
| 8192 | 256 | 8192 | nothing |

## A third sample, and it is the one that says the count CLIMBS

The old daemon was restarted at 22:11 UTC (its 253 descriptors went with it, which is the
operator's workaround and not a fix). The fresh one — pid 18496, **`ps -o etime=` 1 m 20 s** —
held **74 descriptors, 23 of them handles on `sessions.db`**, where the hour-old one had 253
and 105.

So the three samples together: **23 store connections at 1 m 20 s, 105 at 1 h, and the same
253 descriptors ten minutes apart in between.** The count climbs with the harnesses a daemon
opens (a restart restores sessions and reopens them, which is why 23 arrive in the first
minute) and it did not fall while nothing was being opened. That is consistent with
connections never being released when a harness ends, and it is still not proof: the census
that would settle it is *live harnesses* against *open connections* on one process, and this
session did not take it.
