<!-- abstract: `--host NAME` is one placement per daemon and no frame carries it, so harnessd writes `<socket>.placement` naming the socket's (dev, ino), and the bridge refuses a daemon whose placement is not the one asked for. -->

# A placement is a daemon's fact, and the record names its socket

`--host NAME` sets `Placement::Remote(NAME)` for the whole daemon, so every session it opens runs its tools there while the model, the store and the gate stay here. `SessionWiring` carries model, dialect, endpoint and workspace, all per session, and the backend is not among them because it is one per process. So the daemon writes it down: `letibot_sessionlog::placement`, a file named `<socket>.placement` beside the socket and beside the folder's `<socket>.lock`.

The record carries `socket <dev>:<ino>`, and `read()` answers `None` unless the socket at that path is the inode it names. Without that, a daemon killed with SIGKILL leaves a line the next daemon's listener makes look true, and the bridge would let a package run here while its caller believed it was placed. A filename is not an identity.

It is written after the bind, so a refused second start cannot clobber a live daemon's record, and the window before the write fails closed: `None` means unknown, which the bridge refuses in both directions.
