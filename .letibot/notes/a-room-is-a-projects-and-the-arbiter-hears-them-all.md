<!-- abstract: A room is keyed by a project's `workspace_key`, and the arbiter is one session that hears every room, so a ruling naming no project is resolved by the question. -->

# A room is a project's - and the arbiter is one session that hears them all

The project key is `workspace_key`'s twelve hex, the same key `gatekeeper_session` spells. `crates/room/src/tree.rs` writes it a second time, because the room crate holds no store, and `crates/harnessd/tests/room_tree.rs` pins the two spellings to one table.

The trap is that the arbiter is one session and a room is per project: `Request::Rule { question }` arrives on the arbiter's own connection, which is in the box's room, and lands there. `Server::room_holding` resolves a ruling that names no project by the QUESTION, and only when exactly one room has it open; two rooms number from 1, so an ambiguity is refused.

Membership is a push, and that is forced: a subagent never opens a connection, so connections were never the membership. The daemon sends the tree (`Request::Members`) on the poll that reads the pane's snapshot, so a restarted coordinator is told within a second. A member counts for a ruling, but a subagent is still refused.

A folder that is no repository is `""` - the box's room - not a project per temp dir, because the sessions with no tree open from scratch directories, and minting a project per temp dir put the arbiter and every tag agent in rooms nothing could reach.
