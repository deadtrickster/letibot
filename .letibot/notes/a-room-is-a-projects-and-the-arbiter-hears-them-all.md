<!-- abstract: MEASURED 2026-11-01 (branch agent/the-room-is-the-tree): the project key is `workspace_key`'s twelve hex (pinned cross-crate), a room is `<host key>/<project key>`, membership is pushed by the daemon because a subagent never connects — and the arbiter is ONE session that hears every room, so `Rule`'s `project: None` must be resolved by the QUESTION (exactly one room holding it open) or a ruling lands in the arbiter's own room and is refused "no question #1". -->

# A room is a project's — and the arbiter is one session that hears them all

MEASURED 2026-11-01, branch `agent/the-room-is-the-tree` (worktree of the same name), building
`docs/room.md` § *Slice 6*.

## The key was already in the tree, twice

`letibot_sessionlog::workspace_key(path)` is the twelve hex of `sha256(canonical path)` and
`gatekeeper_session` spells `s-gatekeeper-<key>` from it; `scripts/letibot` cuts the same twelve
characters for a folder's socket and lock. So the room's project key is **that key**, written a
second time in `crates/room/src/tree.rs` (the room crate holds no store and no `tokencore`, so it
cannot import `sessionlog`) and pinned to `sessionlog`'s by `crates/harnessd/tests/room_tree.rs` —
the same two-spellings-one-table shape `lease::host_key_of` already has against the launcher's bash.
A project's `system/gatekeepers` row is therefore recognisable: its session id ENDS with the key the
room is keyed by.

## The trap: the arbiter is one session, and a room is per project

The coordinator holds `BTreeMap<project, Room>`, so `Request::Rule { question }` arrives on the
arbiter's connection — which is in ITS OWN room (the coordinator's workspace, which is no project at
all) — and lands there. `room_coordinator.rs`'s test caught it in one run: *"there is no question #1
in the room"*, with the question sitting in another room. The fix is `Server::room_holding`: a ruling
that names no project is resolved by the **question**, and only when exactly one room has it open
(two rooms number from 1; an ambiguity is refused rather than applied to the wrong item).
`Question::project` is stamped by the server as the answer leaves the room, and `Request::Rule`'s
`project` is how a client that hears more than one says which.

## Membership is a PUSH, and that is forced

A subagent never opens a connection (the door refuses it) — so the connections were never the
membership, and the operator's sentence names subagents twice. The room holds no store, so the
daemon sends the tree (`Request::Members`, replaced wholesale) on the same one-second poll that
reads the pane's snapshot; a restarted coordinator is told again within a second, which is why
sending it every poll rather than on a change is worth the frame.

## A member is a session the room has MET

`Room::rule`'s `Decision::Hand` used to accept only a vouched session; now a member counts. That is
what *the room holds what the tree holds* buys a ruling. A member that is a subagent is still
refused (`Refused::Subagent`) — the tree widens *has the room met this one*, not *may a subagent
speak*.

## What is deliberately left, so nobody re-derives it

- **A tag's agent is still the box's.** `tag_session` is `s-room-<host key>-<tag>` — no project in
  the name — so `Server::live_tags` answers the box's own room only and a project's tags have no
  agent. Making it per project means `--session host:ram` must name a project: the launcher's
  spelling, its own slice.
- **The pane does not draw the tree** (`RoomSnapshot.members` carries it; `plugins/builtin/room`
  draws queues per tag), and **a head's view is unchanged** (`letibot --sessions` still prints the
  five groups).

## Rake

**A name that two readers derive needs its inverse to be a CHECK, not a `strip_prefix`.** The first
`room_of_name` accepted `…/../etc` because the round trip alone passes for any suffix; the shape
(twelve hex, or empty for the box's own room) is the second half. `a_name_that_does_not_derive_back`
now pins both.

## A folder that is no repository is NO project — and that is forced, not tidy

`Roster::project_of` answers `""` (the box's room) for a workspace outside any repository, and the
first cut answered the workspace itself. The reason it must be empty: the sessions that HAVE no
project — a one-shot, a fixture, the coordinator's own arbiter, and the tag agents it seats with
`Registry::create_under` — are opened from a scratch/data directory. Minting a project per temp dir
put the arbiter and every tag agent in a room of their own, where `Server::live_tags` (the box's
room's tags) and `route`'s `tag_session` audience (also the box's) could not reach them. So the
default project is not a fallback: it is where the sessions with no tree belong, and the box's
tag-agent machinery lives there.

## A room is only made for a key somebody derived

`with_room` creates a room on demand, so `Request::Rule { project: Some("../../etc") }` used to
create a room with no membership and no name that could ever be reached again. `tree::is_project_key`
(twelve hex, or empty) is now checked at that door and `Refused::UnknownRoom` is the sentence.
