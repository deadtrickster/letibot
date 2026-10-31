<!-- abstract: MEASURED 2026-10-31: the launcher's runtime dir holds three record kinds (a folder's daemon, the box's room, a moved session's forward) and only ONE is named by a workspace key — so the launcher test's "filename is the key" assertion was wrong, the Rust `retarget` addressed a socket nothing binds, and the shell's `--stop N` on the room sent the session protocol's interrupt and left the record behind. -->

# The runtime dir holds three record kinds, and only one is named by a workspace key

**MEASURED 2026-10-31**, branch `agent/the-room-has-a-record-too`, from `cargo test -p letibot
--bin letibot` red on this box (`main` at `ef025a37`).

## The failure, and the assertion that was wrong

```
record /run/user/1000/letibot/room-lab2x1-home-khaprov-com-585ac5fa.json names a workspace
whose key is not its own filename
  left: "room-lab2x1-home-khaprov-com-585ac5fa"   right: "8d14980a6726"
```

`/run/user/1000/letibot` holds 19 `<12 hex>.json` (folder daemons), one `room-<host key>.json`
(the box's room, written by `scripts/letibot`'s `room_record_write` when it autostarts a
coordinator) — and `migrate` writes a third kind, `migrated-<session>.json`. **Only a folder's
daemon is named by `key(workspace)`.** The test asserted that of every file, so it had been
asserting something it could not know since `migrate` landed; the room only made it visible on a
box that has one. Deterministic, 3/3, not a race — a parent session's "the launcher tests pass"
measurement was taken before the room record existed (its mtime is 21:41; the record set is
unchanged since).

The room's record is **not** written by the coordinator: `crates/room`'s lease is a tab-separated
line beside the socket and nothing else. The launcher's python writes the record, in the same flat
shape, with `role: room`.

## How a record says which kind it is

The marks were already there and the shell already read them (`--ls`, `--daemons`, both stop
paths branch on `role == room`): **`role: "room"`** for the room (a folder daemon's `role` is the
SEAT, and no seat is named room), and a non-empty **`session`** for a forward. The port now derives
`Kind::{Daemon, Room, Forward}` from those two fields rather than adding a `kind` field that a
second writer could spell differently.

## Two defects behind it, both about "the record the number printed"

* **The Rust `retarget` re-derived the socket from the workspace**, so the room's number gave
  `8d14980a6726.sock`, which nothing binds. It now takes the record's own `socket` and its own
  path. (The port has no production caller yet; `scripts/letibot` is the path that runs.)
* **The shell's `retarget` took the socket but not the record** — `RECORD` stayed `<key>.json`, a
  file that does not exist for the room. MEASURED with a binder on a unix socket and a live pid:
  `--stop N` stopped the coordinator and then (a) sent it the session protocol's interrupt, whose
  refusal printed as *"the interrupt did not go through"*, and (b) removed nothing, so
  `room-<host key>.json` stayed and `--ls` went on listing a room that was gone. Both fixed by
  `RECORD="${rec:-$RUNDIR/$KEY.json}"`; `tests/launcher.sh` now pins all three (red on base).

## The live test reads a directory every launcher run writes

`the_numbering_matches_the_glob_order_on_a_box_with_records` globs `rundir()` — the same directory
a concurrent launcher run creates records in. `retarget` re-globs per call, so a record landing
between the outer scan and the Nth call shifts every number after it. The test now freezes a copy
of the box's records first (with a second read for the half-written case) and asserts against
that: real shapes, no clock. It prints what it checked — *"19 folder daemon(s) and 1 other
record(s)"* — so a box with one kind does not look like a green two-kind run.

## Rake

**An apostrophe inside a single-quoted `awk` program terminates the string.** `awk 'index($0, "the
box's room") …'` is a parse error shellcheck reports as SC1011/SC1072 and bash reports as an
unterminated quote at runtime — the line that has to match a label containing `'` should go
through `grep -F "…"` with double quotes, then `awk '{print $1}'`.
