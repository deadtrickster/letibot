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

## The trap the sharing sets for the tests written before it

**A test whose point was a SECOND connection now has one connection and measures nothing** — and it
is not always red, so the gate does not catch it. Two shapes, found by the queue's reviewer on this
branch and fixed in the commit after it:

* **Red**: `restart.rs`'s `the_stop_is_recorded_after_a_newer_build_migrated_the_store` raised the
  version and the reader floor through the store it held and asserted `Store::open(...).is_err()`.
  `open_sharing` returns the live `Arc<Shared>` for a path whose inode has not moved and **never
  calls `migrate`**, so the second open was `Ok` and the floor refusal was never reached. The
  behaviour is real in production (a process's first open of the file does run it); only the test
  broke. `Store::open_private` is the fix, and it is what "a fresh open" means.
* **Green and hollow**: four tests made the store refuse a write by renaming a table away "through a
  second connection" (`todos.rs`, `delivery.rs`, `reseat_writes_after_success.rs`,
  `harness/tests/store_writes.rs`). Through the shared handle the rename is the harness renaming its
  own table — still a refused write, but the schema-invalidation-across-connections property they
  were written for had stopped being measured. `open_private` at those four sites restores it.

The rule: **if a test's body needs another connection, it must say `open_private`**, because
`Store::open` now means "whatever connection this process already has". A test that only wants the
store's own view of what it wrote should keep `open` and stop calling it "a second connection".
`Store::open` twice in one function is still fine when the first handle is DROPPED before the
second — the `Weak` is dead and a new connection is made — which is why the floor tests in
`store/tests.rs` and `leases.rs`'s `a_second_connection_sees_the_block_as_held` were already right.
