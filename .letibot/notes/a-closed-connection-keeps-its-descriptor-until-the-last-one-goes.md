<!-- abstract: MEASURED 2026-10-31 on lab2x1: sqlite3 keeps a descriptor per inode in the PROCESS and releases it only when the last connection to that file closes — sequential opens leave theirs behind too (6 → 39), so the count grows with every connection ever made. `Store` is now one connection per file per process (a handle onto an `Arc<Shared>`), and holding a `Conn` guard forbids calling another `Store` method. -->

# A closed connection keeps its descriptor until the process's last one goes

MEASURED 2026-10-31, branch `agent/a-closed-session-gives-its-fds-back`, on lab2x1 (`main` at
`4b2f2113`), with `crates/tokencore/tests/connection_fds.rs` (raw `rusqlite` connections) and
`crates/harnessd/tests/store_handles.rs` (real sessions).

## The mechanism, which decides the shape of any fix

`sqlite3`'s unix VFS keeps the descriptor it opened for a file in the inode's own record, and it is
released when the process's **last** connection to that file closes. Measured through `rusqlite`
on this box:

| what was done | descriptors on the file after |
|---|---|
| one keeper connection | 6 (db, `-wal`, `-shm` and their companions) |
| 20 more opened ONE AT A TIME, each closed | **39** — sequential opens leave theirs behind too |
| 20 more opened at once, then all closed | 43 open, **23** after closing them |
| the keeper closed | 0 |

So the count grows with **every connection the process has ever made**, not with how many are open
at once — which is exactly the operator's daemon (105 connections, two live sessions, over an
hour). *Closing a session's connection returns nothing*; only making fewer connections helps, and a
pool that reuses returned connections buys nothing over what SQLite already does.

(A python `sqlite3` probe on the same box kept the count flat across sequential opens — a different
build's behaviour, and not the one this tree links. Do not carry that result over.)

Before the fix, 20 sessions opened and closed through `Sessions` left **92** descriptors on
`sessions.db` (172 while open), because `Harness::open` opened the store four times per session.
On this box 88 were still there after `drop(sessions)` + `drop(registry)` — the Mac note saw 0, so
something else in this test process (`parts`' `ModeStore`) holds a connection here.

## What the fix is

`Store` is a handle onto `Arc<Shared>`, where `Shared` holds `Mutex<ManuallyDrop<Connection>>`,
the path, the `(dev, ino)` it is open on, and the `newer` flag. `Store::open` keeps a process-wide
`Mutex<HashMap<canonical path, Weak<Shared>>>` and hands out the live one; the entry is replaced
when the path names a different inode (`stat`, so a removed-and-recreated store is not shared with
the dead one). `Store::open_private` is the same file on a connection of its own, for the tests
that measure two connections to one file.

## The rule that comes with it, and the one place it bit

**A `Conn` guard (what `Store::connection()` returns) must not be alive across another `Store`
method call** — that is a self-deadlock, and the compiler will not say so. `LiveView::load` held
one while calling `load_transcript`, so `load_transcript`/`get_stable_prefix` grew `_on(conn, …)`
forms and the view calls those on the connection it already holds.

The busy ladder is unchanged and **the lock is taken per attempt** (`retry_sql(|| self.conn()…`,
`write_tx`'s closure locks inside itself), so a wait on another process's write lock sleeps with
the lock released — the one property that keeps "one connection" from wedging a whole daemon.
