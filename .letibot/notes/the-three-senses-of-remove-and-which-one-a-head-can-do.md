<!-- abstract: MEASURED 2026-11-01: of the three senses of "remove a session", a head can only hide the row — `Store::delete_empty_session` refuses any session holding transcript rows (a BEFORE DELETE trigger) and NO ClientFrame reaches it, so the only door is `letibot --delete <id>`; and the picker's keys must be chords (`del`, `ctrl-z`, `/forget`), never letters, because its typed line is a NAME. -->

# A head can hide a session's row and nothing else — and the picker's keys cannot be letters

MEASURED 2026-11-01, branch `agent/the-tree-starts-folded`, building the operator's
*"add controls to remove sessions"*. Two findings here, and both were paid for.

## The three senses, and what each would need

1. **Hide the row** — head-side state, undoable, needs nothing from anyone. Built.
2. **Forget it in the store** — `Store::delete_empty_session`
   (`crates/tokencore/src/store.rs:4277`) exists and **refuses a session holding
   transcript rows**: `transcript_item` carries a `BEFORE DELETE` trigger, because an
   append-only log whose history can be removed is not one. So even the store can only
   forget a session nothing was ever said in.
3. **Close it if it is live, then forget it** — `ClientFrame::CloseSession` closes *the
   session a connection is attached to*, so a list cannot close its neighbours.

## The finding: no frame reaches the store's delete

`delete_empty_session` has exactly one caller in the tree:
`crates/harnessd/src/cli.rs:3082`, the `Query::Delete` arm of `letibot --delete <id>`, a
one-shot that opens the store file itself. Nothing in `crates/sessionlog/src/protocol.rs`
names a delete (every hit is prose). So sense 2 from a head needs a new `ClientFrame` and
a protocol bump, and sense 3 needs the same plus a decision about stopping somebody else's
work. `/forget ID` therefore refuses by name, always, in a sentence that names the
live-ness, the cost (rows, transcript, children) and the one door that could.

## The picker's keys cannot be letters — REFUSED BY REVIEW, and it was right

The first cut used `x` (remove), `u` (undo) and `f` (forget). The session list is the one
surface in this head whose typed line is a **name** (`Surface::Picker::typed_says`), and
`key_surface` runs before the editor, so a letter bound under `Guard::EmptyComposer` is
matched exactly at the first character of a filter: typing `fix the parser` said a refusal
sentence and left the filter missing its `f`, and `x` armed a removal of whatever the
cursor was on. Every other pane's letters are free only because their typed paths start
with `/`. The controls are now **`del`** (`On::Delete`), **`ctrl-z`** (`Key::Undo` — both
Ctrl+Z and ⌘Z decode to it) and **`/forget ID`**, a verb.

## The invariant, stated as a PREDICATE and not at the call sites

A removal is a subtree, so removing a parent takes the row the head is standing in with
it. Three doors, and the third is the one that survives a bug in the other two:

* `remove_row` refuses the session this head is in **and every session above it**;
* `prune_removed` (on `Hello`) drops a removal the head then lands on — `/switch <name>`
  searches every session, not the drawn rows;
* `removed_chain` itself answers *not removed* for the chain to `here`.

The reviewer found the family branch of `session_rows` pushing the parent's row with no
guard of its own — the head's drawing kept the parent and dropped every child, while the
layer's feed (which did filter) fell out of the family view into the whole project tree:
the pane changed shape on a removal. Putting the rule in the predicate closes the family
branch, the tree's children and the feed at once.

## Rakes

**A test that means to ask the layer must install the layer.** `picker_rows()` falls back
to the head's own `session_list` when `sessions_host` is `None`, so a "both drawings"
test over a bare `App` compares two head-side functions over one source. Install the
built-in through a real `Host` (`Stack::discover` + `letibot_builtins::install`), then
assert `sessions_list(..).is_some()` before trusting its rows — and prove the guard bites
by deleting the filter and watching it fail.
