<!-- abstract: MEASURED 2026-10-27 porting letibot's lists onto rano `dfcadb0`'s tree_list: SessionPicker::tree() is private so the head cannot read hint_line(); TreeList has no "nothing selected" so a tree block inside a bigger pane must strip ids; rano's Row::detail is the faint line UNDER a row, not C3's Enter-prose; and the drawn number is the n-th SESSION. -->

# Four edges of rano's one tree list, found while porting letibot onto it

MEASURED 2026-10-27, branch `rano-tree-list` (letibot pinned to rano `dfcadb0`). The primitive is
`rano::agent::tree_list` — `Row`/`Cell`/`Action`/`flatten`/`TreeList`, with
`rano::agent::picker::{SessionPicker, SessionItem, SessionGroup}` rebuilt on top of it and the
subagents/jobs panes' footers pulled into it.

## 1. `SessionPicker::tree()` is private, so `hint_line()` is unreachable from the head

`TreeList::hint_line()` exists and is exactly what a host should feed `HintBar::line_with_tail` —
*the selected row's actions, then the caller's fixed keys*. But the only route from a
`SessionPicker` is `content(w) -> PaneLines`; the `tree()` that builds the `TreeList` is a private
method. So a head that wants the picker's cursor-dependent sentence has to compose it from its own
data: letibot does it in `Surface::row_says` (the seam its subagents/jobs panes already used), and
the two spellings — rano's `Action::label` and the head's words — can drift until rano exposes
`SessionPicker::hint_line()`.

## 2. `TreeList` cannot say *nothing is selected*, and a block in a bigger pane needs it

`TreeList::selected` is an index into the stops and is **clamped** (`selected.min(stops-1)`), so
there is no value that draws no mark. That is right for a pane that IS a list and wrong for a
`Tree` block inside a plugin's dashboard, where the cursor may be on a finding two panels down.
letibot's `tree_lines` hands the list the rows with their `id`s **cleared** when the cursor is not
in that block: the marks go (nothing is a stop) and the module's drawing is unchanged. Stop-ness for
the PANE is still C3's `id`, read by the pane's own walk, and the pane writes its own
`PushStop`/`stop_rows` record — it never reads the list's.

## 3. `Row::detail` is the faint line UNDER a row, not C3's Enter-prose

rano's `Row::detail: Vec<String>` draws faint lines under the row (the session picker's id line, a
job's redirected path). C3's `Row.detail` is markdown **read whole on Enter**. Mapping one to the
other draws every plugin row's whole prose beneath it — so the plugin port maps `detail` to
*nothing* and leaves Enter to read it, and `open`/`number` are `None` too (C3 has no fold and no
numbering; a `-` that folds nothing is a promise the contract cannot keep).

## 4. A number is the n-th SESSION, and that is a semantic change worth its tests

`agent::picker` numbers `SessionItem::Session` rows and nothing else: a group header takes no
number (`SessionGroup` carries none). So `/switch N` and the picker's typed line now mean *the n-th
session drawn*, not *the n-th row* — a list with `[mine]` above three conversations answers 1, 2, 3
for those conversations, and `[subagents]` between them shifts nothing. `letibot` reads it through
one function, `App::session_at_number`.

## Two smaller things

* **A group's line is a stop** and carries its own `Action` (`enter folds it away` / `unfolds it`),
  which is the defect the primitive was written for: the picker's old footer said *enter switches*
  over a group because the sentence was written once for the list.
* **letibot's `mine` group is the one row rano's `SessionGroup.open: bool` cannot describe**
  (it is always open and cannot fold). The head's bar says so ("on mine does nothing — it is always
  open"); rano's own drawn hint says *folds it away* on that row, because it reads `open`.
