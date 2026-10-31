<!-- abstract: MEASURED 2026-10-31 on lab2x1: sqlite3 keeps a descriptor per inode in the PROCESS and releases it only when the last connection to that file closes — so a closed connection's descriptor is kept while another lives, and a sequential open takes the one the last close left (3 → 4). The count is the high-water mark of CONCURRENT connections, so a pool bounds it at its size: `Store` is one connection per file per process, the pool of size one. -->

# A closed connection keeps its descriptor until the process's last one goes

MEASURED 2026-10-31, branch `agent/a-closed-session-gives-its-fds-back`, on lab2x1 (`main` at
`4b2f2113`), with `crates/tokencore/tests/connection_fds.rs` (raw `rusqlite` connections) and
`crates/harnessd/tests/store_handles.rs` (real sessions).

## The mechanism, which decides the shape of any fix

`sqlite3`'s unix VFS keeps the descriptor it opened for a file in the inode's own record, and it is
released when the process's **last** connection to that file closes — not when the connection that
opened it does. Measured through `rusqlite` on this box:

| what was done | descriptors on the file after |
|---|---|
| one keeper connection | 3 — the db, `-wal` and `-shm` |
| 20 more opened ONE AT A TIME, each closed | **4** — each open takes the descriptor the last close left |
| 20 more opened at once, then all closed | 43 while open, **23** after closing them |
| the keeper closed | 0 |

So the count is the **high-water mark of concurrent connections**, not a debt per connection ever
made: a daemon that opens and closes one at a time does not grow, and a daemon that holds N at once
holds N connections' worth until it exits. The operator's daemon held 210 descriptors on
`sessions.db` because it had **105 connections alive at once** — four per session (`Harness::open`
opened the store four times per session) across twenty-six sessions, none of them closed — not
because it had ever opened 105.

This note first said the opposite, and the number it said it with was a mis-measurement: a census
that matched the file's NAME alone counted a sibling test's twenty connections as this one's and
read **39 where the truth was 4** (commit `2bf28f56`, the commit that added this note, says so in
its own message; `sequential_open_and_close_reuses_the_descriptor` asserts `after <= base + 1`). A
census must match the directory *and* the name — every fixture in that binary opens a
`sessions.db`.

## What a pool would buy, and what it would not

Because a descriptor goes back only when the process's last connection to the file closes, a pool
of N connections to one file bounds the process at N connections' worth — three descriptors each
for a WAL store — however many sessions borrow one and hand it back. That is exactly the property
`Store::open`'s single shared connection has with **N = 1**: three descriptors, whatever the session
count.

So what a pool buys is **concurrency, not fewer descriptors**:

* several threads could read at once, so a search that takes a second would not hold the lock a
  turn needs to append its next row — the property `transcript_source`'s doc says the four
  connections existed for, and the one thing one connection gives up;
* a write waiting on another process's lock would not have to block this process's readers — which
  the retry-closure fix below already gives back without a pool.

What it does not buy: a smaller count (3N against 3), a descriptor returned on close (a pooled
connection is kept open on purpose, and that is what makes the bound a bound), or anything at all
about how many store FILES the process opens. One connection is the pool of size one — the same
mechanism at the smallest bound, and the only one that costs nothing to hold.

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

**The busy ladder is unchanged, and every wait now happens with the process lock RELEASED** —
which is a property this note claimed before the code had it. `write_tx` takes the connection
*inside* the retry closure, so a `BEGIN IMMEDIATE` that comes back busy drops the guard before the
backoff sleep. The queue's own write transactions did not: `acquire_queue_lease`,
`enqueue_review`, `approve_review`, `cas`/`move_review`, `release_queue_lease`,
`release_stale_lease`, `set_target_health`, `allocate_lease` and `release_session_leases` each did
`let conn = self.conn(); let tx = Self::immediate(&conn)?`, so the whole ladder — up to
`BUSY_RETRY_TOTAL` plus one 30 s `busy_timeout` — ran with the process's **only** connection held.
Those are exactly the paths whose lock spans processes (*"two daemons on this box share one
`sessions.db`"*, `leases.rs`'s own words), so a reader in the daemon waited out another daemon's
commit and WAL's one-writer-many-readers was gone when it was needed. They all go through
`write_tx` now, and `busy/tests.rs`'s
`a_queue_write_waiting_on_another_process_leaves_the_store_readable` measures it: with another
connection holding the write lock for 700 ms, a read in this process must not wait 350 ms — it
waited **1.05 s** before the change.

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
