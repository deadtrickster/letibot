<!-- abstract: The launcher raises the daemon's open-file soft limit to 8192 before anything is forked, because a soft limit may only be raised to the hard limit a process was born with. -->

# The launcher raises the daemon's open-file limit — and `lsof -p | wc -l` is not a descriptor count

A daemon on the Mac's default 256 soft limit reaches it in an hour of ordinary work, and then a
reviewer's `git checkout` dies of `Too many open files (os error 24)`. `scripts/letibot:98`'s
`raise_open_files "$OPEN_FILES_WANTED"` runs right after `set -euo pipefail`, before anything is forked:
`ulimit -Sn` up to `min(8192, hard)`, never lowering a limit already above the request. Because rlimits
survive `fork` and `exec`, that one call reaches the daemon, a one-shot, every head and every `bash`,
`git` and `cargo` a turn runs. It cannot live in the daemon: a soft limit may only be raised to the HARD
limit a process was born with.

`lsof -p PID | wc -l` is not a descriptor count - it counts `txt`/`mem` mappings too, 24 rows over the
real number here. The FD column is the fourth field, so `$4 ~ /^[0-9]+/` is the count that means what
`RLIMIT_NOFILE` means.

`ulimit -n N` sets the soft AND the hard limit, silently and irreversibly for that process tree; use
`ulimit -Sn`. And `unlimited` is not the number to ask for either: it means `kern.maxfilesperproc`
(245760 here), so a leak would hold a quarter of a million descriptors before anything said so.
