<!-- abstract: MEASURED 2026-10-26 (RH2): a placement (`--host NAME`) is one per daemon and the protocol's `Hello`/`SessionWiring` carries model, dialect, endpoint and workspace but no backend — so harnessd writes `<socket>.placement` beside the socket, naming the socket's (dev, ino) so a record from a SIGKILLed daemon is not read as the new one's answer. -->

# A placement is a daemon's fact, and the record names its socket

MEASURED 2026-10-26, branch `bridge-host` (package RH2: `leticode_start { host }`).

## There is no placement on the wire, and there is nowhere to put one

`--host NAME` sets `cfg.placement = Placement::Remote(NAME)` for the DAEMON (crates/harnessd/src/cli.rs),
so every session it opens runs its `bash`/`read`/`write`/jobs there while the model, the store and the
gate stay here. A head that attaches is told `SessionWiring { model, dialect, endpoint, workspace }`
(crates/sessionlog/src/registry.rs:471) — all per SESSION — and the backend is not among them, because
it is one per PROCESS. So the daemon writes it down: `letibot_sessionlog::placement`, a file named
`<socket>.placement` beside the socket and beside `FolderClaim`'s `<socket>.lock`.

## The rake: a record outliving its daemon reads as the next daemon's answer

A daemon killed with SIGKILL leaves its `.placement` behind. `is_up` is a *connect*, so a new daemon's
listener says "somebody is here" — and a reader that trusted the old line would answer for a daemon
that no longer exists. That is not cosmetic: the bridge refuses to join a daemon whose placement is
not the one its caller asked for, so a stale `host:lab2x1` read under a new LOCAL daemon would let a
package run on this box while its caller believed it was placed. The record therefore carries
`socket <dev>:<ino>` and `read()` answers `None` unless the socket at that path is the inode the
record names — the same pair `unlink_if_ours` compares, for the same reason (a filename is not an
identity).

Ordering, and why: **written AFTER the bind** (harnessd, once `Daemon::serve` returns), so a second
start refused for a live daemon cannot clobber the live one's record. The cost is a microsecond
window in which a listener has no record yet — and that window fails CLOSED, because the reader's
`None` means *unknown*, which the bridge refuses in both directions.

## What the bridge does with it

`daemon::ensure` (crates/bridge/src/daemon.rs) compares the record with what the call asked for —
`Placement::Host` when it asked for nothing, so *nothing* is a placement and not an absence — and
refuses a mismatch naming the running daemon's placement and `letibot --stop`, because the folder's
daemon is usually the operator's and stopping it is theirs to do. `Placement::Host` and
`Placement::Remote(name)` come from `letibot_tools::builtins::task` (the bridge now depends on
`letibot-tools` for that and for `Hosts`, the tree's one strict `hosts.toml` reader — the same
refusal `where: host:NAME` gives). `status`/`list`/`attach`/`start` all answer `placement`
(`host`, `host:NAME`, or `null` = nothing known).

## The edge to know before believing a green test

A daemon from a build before this change recorded nothing, so `leticode_start` REFUSES it (both
directions) until it is stopped and started again. The two bridge test fixtures (`end_to_end.rs`,
`no_spin.rs`) had to write the record themselves after `Daemon::serve`, because `Daemon::serve` is
sessionlog's and knows nothing about placements; without it every `leticode_start` in those suites
refuses. `end_to_end.rs` deliberately records `host:lab2x1` while its daemon is local: the record is
the INPUT the answers are asserted on, not a claim about where that fixture's processes run.
