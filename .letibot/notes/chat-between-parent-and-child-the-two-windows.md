<!-- abstract: MEASURED 2026-10-29: a parent's message already became a row at the engine's step boundary, so the defect is not "no row" but two windows — a message handed back by a failing round (now written by `run_rounds`) — and the upward edge needed an ADDRESS (`task_message {"task":"parent"}`), not a door. Plus: the head already draws a parent's message as a one-line card and drew a child's answer as a generic `notice` card. -->

# Chat between a parent and a child: the row was already there, twice over

MEASURED 2026-10-29, branch `agent/parent-child-chat` (worktree `agent-parent-child-chat`,
`main` at `c7bfdf39`), while building the operator's *"i definitely would want parents and
children to be able to chat"*.

## 1. The mid-turn door DOES write a row — the row is the injection

`HubSteering`'s `CommandKind::Message` arm writes only a trail line, which is what the brief
read as "the record stops there". It does not: the `SteeringMessage` it returns is turned into
a `User`/`Speaker::Agent` item by the engine's `Pending::take_items`, and
`Session::append_items` appends it through the same writer as every other row. MEASURED by
extending `message_between_turns`'s `a_message_to_a_running_child_lands_in_that_turn` with a
`transcript_item` read of the child's store: one row, byte-identical to
`message_from_a_session(parent, MESSAGE, Some(parent))`.

So a second row written at the door would be a COPY of the injection, and the child would read
its parent's sentence twice. Do not add one.

## 2. The two real windows, and what closes them

* **absorbed at a step boundary → appended before the request.** `engine.rs`'s greedy pre-poll
  does `steering_before = pending.take_items(); session.append_items(...)` BEFORE the round is
  sent, so a message taken there is a row even if the round then fails.
* **absorbed mid-generation → `give_back` → `handed_back`.** A round that fails after a token
  boundary polled the steering queue returns the message to `HubSteering::held` instead of
  appending it. That set survives the turn and was re-offered to the NEXT turn — and a child
  whose next turn never came (parked, or its daemon stopped) left no row at all. That is the
  shape the operator's *"of the child's 278 transcript rows, not one contains the parent's
  words"* fits. Closed in `Harness::run_rounds` + `keep_the_words_it_never_read`: a message
  still carried when the turn ends is appended there (same text, same `Speaker::Agent`, one
  `Text` part) and REMOVED from the carried set, so it cannot be written twice; a failed append
  puts it back and says `message_not_stored`.

To reach that window in a test, `Stub::cutting` answers one request in halves (first frame, wait,
rest minus the last two frames) and closes the socket without the chunked terminator: no `Final`
ever arrives, so the round fails. **A child whose FIRST turn fails leaves its thread**, so the
doomed turn must be a later one (started by a message) if anything after it is to be observed.

## 3. The upward edge was missing an ADDRESS, not a door

`task_message` has always taken any live session's id, so a child could reach its parent if it
were told the id — and nothing told it (`Config::parent_session_id` is on the row and in no
disclosure). `PARENT_HANDLE` (`"parent"`) is resolved by `TaskRunner::parent()` BEFORE
`resolve_child`, so a child whose slug is literally `parent` cannot shadow it, and a session at
the top of its tree is refused by name (*"this session has no parent to speak to"*).

## 4. The sentence had to learn which way the edge runs

`parent_message_text` said *"message from your parent session `X`"* about whatever id it was
handed, which is false the moment a child answers its parent. It is now
`message_from_a_session(from, text, mine)` where `mine` is the RECEIVER's own
`parent_session_id`: the relation is claimed only when it holds, otherwise *"message from session
`X`"*. Two readers had to learn the second opening: `review_card::unwrap_message` (or a refusal
would draw with its envelope on) and `notice_card::message_from_a_session` — before which a
child's answer drew as `session · notice · message from session `s-…`…`, the generic card with
its words clipped, while a parent's message drew as a one-line card. Now both are one line, led
by `parent <id>` or `agent <id>`.

**The TUI's review-card fixtures were re-blessed** (`review_refused_message.txt`,
`review_refused_root_message.txt`, `review_vetoed_message.txt`) because the envelope changed from
`your parent session` to `session`.

## 5. A fixture trap worth keeping

`Sessions::dispatch` is `&mut self` and the teardown guard (`DrainOnDrop`) has to hold the
`Sessions` for the whole test — so `message_between_turns` now keeps them in a `RefCell` and each
caller borrows for its own statement. `Outcome` has no `Debug`, so every arm of a `match` on it
must be named.
