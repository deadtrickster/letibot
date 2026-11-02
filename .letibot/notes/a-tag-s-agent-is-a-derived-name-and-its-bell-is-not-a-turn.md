<!-- abstract: A tag's agent is `s-room-<host key>-<escaped tag>`, where the escape (not a sanitisation) is what keeps `8 gb` and `8-gb` two agents; the server's route had to address it explicitly, and the cost rule is two mechanisms - `wake::push` for a note, `bell()` for a turn. -->

# A tag's agent: the name is an escape, and the bell has two mechanisms

`letibot_room::tag_session(host_key, tag)` is `s-room-<host key>-<escaped tag>`, and
`tag_of_session` (`crates/room/src/lease.rs`) inverts it. The suffix is an ESCAPE, not a
sanitisation: `a-z 0-9 -` stay, `_` becomes `__`, and every other UTF-8 BYTE becomes `_` plus two
hex digits. Mapping odd characters to `-` would make `8 gb` and `8-gb` one agent, and the inverse
could not tell them apart. Unescape into bytes and end in `String::from_utf8` - a `char` accumulator
writes each byte of a multi-byte character as a character of its own. `tag_of_session` ends with
`tag_session(k, &tag) == session`, so an id that merely starts with the prefix is no agent.

The bell needed a ROUTING line, not only a filter. `Server::route` addresses the sessions queued
under the tag and whoever the event named, and a tag's agent is neither, so its audience had to be
added there or it hears nothing while a unit test stays green.

The cost rule is two mechanisms that already exist: `wake::push` writes a note into a per-session
queue, and `bell()` rings `ring_wake`, which opens a turn. So claim, beat, done and abandoned push;
`Raised` and `Asked` push and bell. The test asserts the ring count, because a turn is a model call
and cannot be asserted directly. `ensure_tag_agents` (`crates/harnessd/src/room/agents.rs`) takes no
bell at all - a function with nothing to ring cannot open a turn.

`Event::Ruled` carries no tag (`Event::tag()` answers `""`), so the tag-side filter accepts it
unconditionally: its arrival is the addressing.
