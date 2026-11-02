<!-- abstract: The runtime dir holds three record kinds and only one is named by a workspace key, so a record says its kind by its fields and a retarget must take the record's own socket and path. -->

# The runtime dir holds three record kinds, and only one is named by a workspace key

`<12 hex>.json` is a folder's daemon, named by `key(workspace)`; `room-<host key>.json` is the
box's room, written by `scripts/letibot`'s `room_record_write`; `migrated-<session>.json` is a
forward. A test that asserts "the filename is the workspace key" of every record asserts what it
cannot know.

A record says which kind it is by its fields: `role: "room"` for the room (a folder daemon's
`role` is the seat, and no seat is named room), and a non-empty `session` for a forward.
`Kind::{Daemon, Room, Forward}` is derived from those two, rather than a `kind` field a second
writer could spell differently.

A retarget that re-derives the socket from the workspace addresses a path nothing binds. Take
the record's own `socket` and its own path, and in the shell
`RECORD="${rec:-$RUNDIR/$KEY.json}"` - or `--stop N` stops the room, sends it the session
protocol's interrupt and removes nothing, so `--ls` goes on listing a room that is gone.

Rake: an apostrophe inside a single-quoted `awk` program terminates the string (SC1011/SC1072).
Match a label containing `'` with `grep -F` and cut the field with `awk '{print $1}'`.
