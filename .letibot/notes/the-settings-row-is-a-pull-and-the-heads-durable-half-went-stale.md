<!-- abstract: MEASURED 2026-10-30: `Registry::set_settings` stores rows for the next `Settings` request and pushes nothing, so every resident line drawn from a settings row is a sentence from attach time — the todo question's line quoted a withdrawn question for ever, and the test that should have caught it sent a `Settings` frame no daemon ever sends. -->

# A settings row is a PULL, and the head's durable half went stale

MEASURED 2026-10-30, branch `agent/a-question-that-outlives-its-row` (commit `c256a2ef`), from the
operator's screen: a `⚠ WAITING ON YOU: 1 — "…" on \`…\` · alt-a to answer · alt-h hides` line
that survived a `todo_write` detach, a completion, and a restart.

## The mechanism, in one sentence

`Registry::set_settings` (`crates/sessionlog/src/registry.rs`) does `entry.settings = rows` and
**nothing else** — it stores the rows for the next `Settings` REQUEST. No frame is pushed to an
attached head. So a head's `self.settings` (and every resident line derived from it) is a
photograph of attach time, and a value that changes during a session never reaches the head that is
looking at it.

The daemon's half is right: `Harness::flush_todos` publishes `operator_question_settled`, calls
`publish_settings()` (so the registry's copy is fresh) and only then publishes `TodosUpdated`.

## The instance

A todo row's question lives on the ROW (`TodoNeed::Operator`). `todo_write`'s `Wait::Detach`
retains that need off (`crates/tools/src/builtins/todo.rs`), so detach, completion and an answer all
end the question on the board. The head drew that fact from two places:

  * the board (`App::todos` → `App::operator_asks`), replaced on every `Todos`/`TodosUpdated`;
  * `App::asks_setting`, the `asks` settings row — **never refreshed**.

`waiting_line` fell back to the settings row whenever the board said nothing, and an empty board
and an unread board were the same value. Hence a card outliving its row.

Fix: `App::asks_board: Option<String>` — the session id whose board this head has read. Set by both
`Todos` arms, cleared on every `Hello`; once set, the board is the only source. The attach and the
resync now also queue `Action::ListTodos` (`refetch_session_facts`, the `Resync` arm) — before this
the board was read only when the settings row mentioned a question, so a head that reattached after
a take-back could never be corrected.

## The rake — a fixture that sends a frame no daemon sends proves nothing

`a_board_waiting_on_the_operator_draws_a_resident_line` (in `app/tests/todos.rs`) cleared the line
with `a.apply(ServerFrame::Settings { rows: Vec::new() })`. That frame is the head's own request
answer; no daemon pushes it when a question is settled. **The fix was written into the fixture**, so
the test was green and the screen was not. When a test for "X clears Y" applies a frame, ask which
REQUEST that frame answers — if the head never sends it at that moment, the test is asserting about
a message that does not exist.

## And the general shape

Any resident line fed by a settings row has this bug in it. `auto_compact_off` and `nags_paused`
read theirs the same way (`Settings` arm) and survive only because the live event (a warning) is the
other channel. When adding one, give the head a fact it can CHECK (the board, the snapshot, a
counter) rather than only a sentence it was told once.
