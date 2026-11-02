<!-- abstract: Of the three senses of removing a session a head can only hide the row, and the picker's keys must be chords, never letters. -->

# A head can hide a session's row and nothing else — and the picker's keys cannot be letters

Hide the row is head state, undoable. Forget it in the store is `Store::delete_empty_session`,
which refuses a session holding transcript rows, because `transcript_item` carries a trigger
that refuses the delete. Close a live one and forget it needs a frame no list can send:
`CloseSession` closes the session a connection is attached to.

`delete_empty_session`'s one caller is `letibot --delete <id>` in `crates/harnessd/src/cli.rs`,
and nothing in `crates/sessionlog/src/protocol.rs` names a delete. So `/forget ID` refuses by
name, in a sentence naming the cost and the one door.

The picker's keys cannot be letters: its typed line is a NAME, and a letter bound under
`Guard::EmptyComposer` is matched at the first character of a filter. The controls are `del`,
`ctrl-z` and `/forget ID`.

The invariant is a predicate and not three call sites: `removed_chain` answers *not removed* for
the chain to `here`, so removing a parent cannot take the row the head is standing in, and
putting the rule there closes the family branch, the tree's children and the feed at once.

Rake: a test that means to ask the layer must install it - `picker_rows()` falls back to the
head's own list when `sessions_host` is `None`.
