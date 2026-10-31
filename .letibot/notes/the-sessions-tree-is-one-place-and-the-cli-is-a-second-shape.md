<!-- abstract: MEASURED 2026-10-31 finishing `agent/sessions-as-a-tree`: `App::session_rows()` returns ROWS (a node's line included) and `SessionRow::idx` is the session index — indexing `App::sessions` with a row position panicked at 8 of 7; the tree's two facts come from one function (`App::placements`) and reach the wasm layer through the feed, while the row WORDING is deliberately two copies held by one equality test and two committed pictures; and `letibot --sessions` still prints the five `SessionGroup`s, not the tree. -->

# The sessions tree: one place for where a row goes, two copies of what it says — and a second listing in the CLI

MEASURED 2026-10-31, branch `agent/finish-the-sessions-tree` (the operator's *"top level nodes are
projects, then next level are top-level sessions for that project, then their subsessions for
worktrees … gatekeepers and reviewers and all other system agents are under
`<project>/<system>/<gatekeepers>`"*).

## 1. `session_rows()` returns ROWS, and a row is not a session

`App::session_rows()` (`crates/tui/src/app/session.rs`) draws a project's line, a `system` line, a
group's line and a session's row into **one** `Vec<SessionRow>`. `SessionRow::idx: Option<usize>` is
the index into `App::sessions`; the row's POSITION in the vec is not, and a node's line has no
session at all. The interrupted branch indexed `a.sessions[at]` with `at = row_of(...)` and the test
died `index out of bounds: the len is 7 but the index is 8` — which reads as a fixture with too few
sessions. **Rake: every reader of this list gets a position and must go through `idx`.**

The other half of the same rule: the numbers (`1`, `2`, …) are the n-th SESSION drawn and are carried
through the whole walk in `session_list` — a node's line takes none, because a number is somewhere to
go and a node's line is not.

## 2. Where a row goes is ONE function; what it says is two copies, on purpose

* `App::placements()` decides `project` (from the ROOT of the parent chain, not the row's own
  workspace — a review clone under `/tmp` is not a project) and `system`
  (`App::system_kind`: the gatekeeper id, then `origin == Queue`, then the two title prefixes).
* `sessions_feed()` (`app/sessions_pane.rs`) ships both facts to the built-in `sessions` layer in the
  `sessions` feed, so the guest never recomputes them. **A guest cannot walk a store, and a second
  opinion about where a session goes is the one thing the layer must not have.**
* The row WORDING (`row_name`, `session_row_facts`, `row_under`) exists twice — the head's and the
  layer's — because the layer is the copy a project overrides. They are held together by
  `app/tests/sessions_list.rs::the_heads_list_and_the_plugins_list_draw_the_same_rows`, which renders
  both paths at one width and requires them identical, and by two committed pictures in
  `app/tests/data/sessions-list-{closed,open}.txt` (`LETIBOT_BLESS=1 cargo test -p letibot-tui
  sessions_list`). **A change to the tree's shape is a change to those two files, and the diff is the
  change.**

One divergence the guard cannot see: the layer runs `clean()` (control chars → space) over a row's
`under` line and the head does not. A workspace or id with an escape in it draws two ways.

## 3. `letibot --sessions` is a SECOND shape over the same marks

`harnessd::cli::grouped` still prints the five `SessionGroup`s (`mine`, `subagents`, `review queue`,
`packages`, `one-shots`) from `letibot_sessionlog::group_of`. The picker draws the project tree. They
read the same marks (the queue's id, the parent edge, the `origin` column, the title prefixes) and
answer different questions, so a session can be in a different place in the two pictures. Both
places used to claim "the same rule the head's picker draws"; that sentence is now false and was
corrected rather than obeyed. **Unifying them is real work in `harnessd` and nobody has asked.**

## 4. What is gone, so nobody greps for it

`App::session_picker`, `App::say_mine_always_open`, `App::open_groups` and `tui::app::tests::groups`
do not exist. `App::session_list` is the way out (the head's own tree, drawn through the same rano
`TreeList` the layer's answer is drawn through), and `App::picker_list` is the one door both the
drawing and the keys read. `SessionGroup`/`GroupRow` survive in `letibot-sessionlog` for the CLI.

## The rake

**A list whose rows are not its items has two indices, and the compiler will not tell them apart** —
both are `usize`. When a test on `session_rows` panics with an index one past the end, suspect the
row position before the fixture.
