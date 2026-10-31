<!-- abstract: MEASURED 2026-11-01 merging the bell onto the tree's room: both branches bumped ROOM_PROTOCOL_VERSION 3→4 for different frames, the tree's route had an early return that swallows the bell's whole purpose, and the arbiter is one session reached by two mechanisms. -->

# Two branches bumped one protocol to 4, and a room's route had an early return that would have eaten the bell

MEASURED 2026-11-01, branch `agent/the-bell-per-client` rebased onto `main` at `67da3d9b` (the
tree's room, `42ebdeae`), resolving `crates/room/src/wire.rs`, `crates/room/src/server.rs` and
`crates/harnessd/src/room/link.rs`.

## The version number, which is the one thing a merge cannot keep from both sides

Both sides bumped `ROOM_PROTOCOL_VERSION` **3 → 4** and each wrote its own `# 4:` section on the
same const: `main` for *a room is a PROJECT's* (`Request::Members`, `Response::Room`'s `members`,
`Question::project`), the bell branch for *the room says what it could NOT deliver*
(`Response::Welcome`'s `missed` and `missed_since_ms`). Each branch also bumped the **same pin
test** — `assert_eq!(ROOM_PROTOCOL_VERSION, 4)` in `server.rs`'s `the_room_speaks_protocol_four` —
so the pin agrees with both and catches neither.

The resolution: **one number, two sections.** Neither branch ever shipped a client at 4, so no
binary speaks "4" without both frames, and renumbering one of them to 5 would have been a claim
about a client that does not exist (and would have invalidated every `wire`'s version-4 note in
`client.rs`/`server.rs`). A rebase is not the place to invent a version bump: the number is a
statement about two BINARIES, and this tree ships one.

## `route`'s early return is exactly the case the bell exists for

`main`'s `Server::route` opened with

```rust
if conns.is_empty() && boxed.is_empty() { return; }
```

— nobody is connected in this room, so there is nobody to write to. The bell's whole rule is that
**an audience with no connection is the case to count** (`Inner::missed`), so that early return is
not an optimisation to keep and re-order: it deletes the feature. The merged function derives and
counts the audience whether or not a single socket is open.

## The arbiter is one session reached by two mechanisms

`main` writes to the arbiter through a second list, `boxed` (its connection is in the BOX's room,
while the event was routed in a project's), and tests `audience.contains(session) || arbiter ==
conn.who.session`. The bell inserts the arbiter into the **audience** instead, because a session
that has no connection has to be counted. Keeping both is fine, but `here` — the set of sessions a
write will actually reach — must then be `conns ∪ boxed`: built from `conns` alone, the arbiter's
own live connection is not in it, so every event routed in a project the arbiter is not in would be
counted as a gap for a session that is connected and being written to.

## The test that says both rules at once

`crates/harnessd/tests/room_tree.rs`'s `a_bell_in_one_projects_room_does_not_ring_in_another`: two
projects each post under `ram`, project one claims its own, and project one's bell rings while
project two's does not move. Proved by mutation, not by green — forcing `project_of_hello` to answer
`DEFAULT_PROJECT` (one room per box) fails it at *project two's bell must not ring*, and routing an
answer's events in `DEFAULT_PROJECT` fails it at *each session is rung for the item it made*.

## Rakes

- **A branch that grows a function's signature is a conflict even where git says nothing.** `main`
  added `project` and `roster` to `SessionRoom::connect`; the bell's four fixture call sites are in
  files `main` never touched, so they merged clean and then failed to compile. A clean `git rebase`
  says nothing about a caller.
- **`git add` on a half-resolved file is the trap this tree has paid for twice.** After resolving
  three of four hunks in `server.rs` I ran `git add` and `git rebase --continue`; the markers at
  line 439 were still there and only `git grep -n -E '^(<<<<<<< |>>>>>>> |=======$)'` said so.
- **The base moves while you work.** `main` was `67da3d9b` when the brief was written and
  `e5c5543b` four commits later; `git diff main..HEAD` then reports the branch as *deleting*
  another session's landed work, which reads as a botched rebase and is a stale base.
