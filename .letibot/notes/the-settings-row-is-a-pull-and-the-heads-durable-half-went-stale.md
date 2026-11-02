<!-- abstract: `Registry::set_settings` stores rows for the next `Settings` request and pushes nothing, so a resident line drawn from a settings row is from attach time - and a fixture that applies a frame no daemon sends proves nothing. -->

# A settings row is a PULL, and the head's durable half went stale

`Registry::set_settings` (`registry.rs:1303`) does `entry.settings = rows` and nothing else. The
rows are for the next `Settings` REQUEST, so every resident line drawn from them is a sentence
from attach time. The daemon's half is right: `flush_todos` publishes
`operator_question_settled`, then `publish_settings()`, then `TodosUpdated`.

The fix is `App::asks_board`, the session id whose board this head has read. Both `Todos` arms
set it, `Hello` clears it, and once set the board is the only source. Attach and resync also
queue `Action::ListTodos`, because before this the board was read only when the settings row
mentioned a question.

Rake: the test cleared the line with `ServerFrame::Settings { rows: Vec::new() }`, which is the
answer to the head's own request - no daemon pushes it when a question is settled. The fix went
into the fixture, so the test was green and the screen was not. When a test for "X clears Y"
applies a frame, ask which request that frame answers.

Rule: give the head a fact it can re-read, not only a sentence it was told once.
`auto_compact_off` and `nags_paused` read a settings row the same way.
