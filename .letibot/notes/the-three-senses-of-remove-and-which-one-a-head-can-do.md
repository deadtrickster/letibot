<!-- abstract: MEASURED 2026-11-01: of the three senses of "remove a session", a head can only hide the row — `Store::delete_empty_session` refuses any session holding transcript rows (a BEFORE DELETE trigger) and NO ClientFrame reaches it, so the only door is `letibot --delete <id>`, a one-shot on the store file; `CloseSession` closes only the session a connection is attached to. -->

# A head can hide a session's row and nothing else — the store's delete has no frame

MEASURED 2026-11-01, branch `agent/the-tree-starts-folded` (worktree of the same name),
building the operator's *"add controls to remove sessions"*. The finding is what decided
the design, so it is here rather than in the diff.

## The three senses, and what each would need

1. **Hide the row** — head-side state, undoable, needs nothing from anyone. Built.
2. **Forget it in the store** — `Store::delete_empty_session`
   (`crates/tokencore/src/store.rs:4277`) exists and **refuses a session holding
   transcript rows**: `transcript_item` carries a `BEFORE DELETE` trigger, because an
   append-only log whose history can be removed is not one. So even the store can only
   forget a session nothing was ever said in.
3. **Close it if it is live, then forget it** — `ClientFrame::CloseSession` closes *the
   session a connection is attached to* (`crates/sessionlog/src/protocol.rs`, and its own
   doc says why: a migration needs one session still). A list cannot close its
   neighbours.

## The finding: no frame reaches the store's delete

`grep -n "delete\|Delete" crates/sessionlog/src/protocol.rs` answers **no variant** —
every hit is prose. `delete_empty_session` has exactly one caller in the tree:
`crates/harnessd/src/cli.rs:3082`, the `Query::Delete` arm of `letibot --delete <id>`, a
one-shot that opens the store file itself. So sense 2 from a head would need a new
`ClientFrame` **and** a protocol bump — which is the one thing this tree's notes keep
warning about merging — and sense 3 needs the same plus a decision about stopping somebody
else's work.

A head's own `CloseSession` is not that door either: it names no session, and the server
answers it on the connection's own thread.

## What was built instead, and why it is not a compromise

* `x` on the session list — remove the row, **asked twice**: the first press names the
  session, the rows drawn under it, and that its store rows stay; the second press removes.
  `u` brings the last one back, in order. Any other key on the pane disarms, so the second
  press is a press.
* `f` — the destructive sense, **refused by name, always**, in one sentence that names the
  live-ness first (`the daemon is holding it, 1 head attached`), what a forget would take
  (rows, transcript, children), and the one door that could (`letibot --delete <id>`, which
  itself refuses a session with rows). A key that only refuses is worth having when the
  refusal is the fact the person needs; the bar says so rather than promising the act.
* The row the head is standing in is refused: the list is the whole answer to *where am I*.

## Two rakes under the implementation

**A removal is a SUBTREE, and both drawings must agree about it.** The unit is the row and
everything under it (a child whose parent is not on a tree has no place on it), so the
predicate is `App::removed_chain` — the id, or any ancestor's. The head's walk applies it in
`push_session` (one guard covers a project's root, a group's root and a child); the built-in
layer's copy is drawn from `App::sessions_feed`, so the feed drops the same rows. Filtering
only one of the two leaves the layer drawing rows the head's keys no longer enumerate.

**The project's count had to move with the removal** — a line saying `(7)` over six rows is
a number that lies about what it hides. `members` is filtered, not just the walk.

## What is deliberately not there

A "show the hidden ones" list, and a count of them on the project's line. `say` names the
count on every removal and every undo (`2 sessions are removed from this list`), and `u` is
the way back — which is what the sense is worth: the row is a view, and the session is one
`/switch <name>` away whatever the list says.
