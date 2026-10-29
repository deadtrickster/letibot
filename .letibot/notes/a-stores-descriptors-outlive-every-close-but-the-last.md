<!-- abstract: MEASURED 2026-10-29: `Store`'s lifetime is correct (live stores return to 1) yet descriptors on `sessions.db` survive every connection close except the process's LAST one — 20 sessions opened and closed left 92 descriptors on the file and 5 live stores, and 0 after the daemon's own connection closed. So `ulimit`-exhaustion is a function of how many connections a daemon has EVER opened, and the only fix is fewer connections. -->

# A store's descriptors outlive every close except the last one

MEASURED 2026-10-29, branch `agent/one-store-per-session-and-nobody-closes-it`, on the Mac, with
`crates/harnessd/tests/store_handles.rs` (byte vocabulary + a named provider, so no GGUF and no
turn: the subject is the store).

## The numbers

One daemon (`Sessions::open_first`), 20 more sessions opened through `Sessions::open`, then all 20
closed:

| moment | live `Store`s | descriptors on the store file | in the process |
|---|---|---|---|
| baseline, one session | 5 | 20 | 20 |
| 20 sessions open | 85 | 172 | 220 |
| all 20 closed | **5** | **92** | 100 |
| the daemon's own connection closed | 0 | **0** | 6 |

**Every `Store` is given back** — 85 → 5, and 5 is exactly the root's four plus the daemon's one.
And **the descriptors are not**: 92 of them, ~4 per session that was closed, released only when the
*last* connection to that file in the process goes.

So `Store::drop` is not the bug. `Harness::open` opens the store **four times per session** (its
own, `corpus::StoreCorpus`, `transcript_source::StoreTranscripts`, `decision_source::
StoreDecisions`), each a WAL connection; closing one releases its `-wal` and leaves its `db`
descriptor held against the file until nothing in the process has it open. A daemon keeps one
connection for its whole life (`sessions::StoreSessions`, the registry's source), so **every
descriptor it has ever opened is held until it exits**. That is the operator's `pid 99166`: 104
`sessions.db` + 101 `-wal` descriptors, ~one per session that ever opened the store, while the
daemon reported two sessions.

## The instrument

`letibot_tokencore::store::Store::live()` — a process-wide count of open `Store`s, incremented in
`from_connection` and decremented in `Drop`. A descriptor count is a proxy; this is the lifetime
itself, and the two together are what separates "the store outlived the session" (a lifetime bug)
from "the descriptor outlived the store" (this).

## The rakes

* **A bare `Harness` is clean.** `Harness::open_with_registry` + drop takes 9 descriptors to 0, three
  times running, with a `Registry` too. Every leak this file measures needs `Sessions` — so a
  fixture that opens a `Harness` by hand cannot see this class of bug at all.
* **`Sessions::close` did not clear `Sessions::plugins`**, so a `PluginHost` (its layer stack and
  wasm instances) was held for every session ever opened. Fixed in this branch.
* **rusqlite 0.40.2's `Connection::drop` calls `sqlite3_close` and DISCARDS the result**
  (`#[expect(unused_must_use)]`). `sqlite3_close` fails with `SQLITE_BUSY` whenever a statement is
  still open, leaving the connection and its descriptors open with no error anywhere. `Store` now
  takes its connection out of a `ManuallyDrop` and calls `Connection::close`, which reports it.
  MEASURED here: no store in this tree is dropped with a statement open, so this is a guard rather
  than this bug's cause.
* **`Store::live()` drifts if you increment after a fallible step** — `from_connection` did
  `store.migrate()?` before the increment, so a failed migration would decrement without having
  incremented. The increment is after `migrate` but the `Store` is already constructed, so the `?`
  path drops it and under-counts. Watch for it if you move the code.

## What the fix has to be

Fewer connections, not better closes: **one connection per store file per process**. That makes the
count 3 (db + wal + shm) whatever the session count. What it costs is concurrency — every store
access in the daemon serialises behind one mutex, where today a head's picker frame and a
transcript search use different connections — which is why it is a package of its own and not a
patch to `Store::open`.
