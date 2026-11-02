<!-- abstract: A parent's message already became a row at the engine's step boundary, so the fix is not a second write at the door - it is the message a failing round hands back, and an ADDRESS for the upward edge. -->

# Chat between a parent and a child: the row was already there, twice over

The mid-turn door DOES write a row. `HubSteering`'s `CommandKind::Message` arm writes only a trail
line, but the `SteeringMessage` it returns becomes a `User`/`Speaker::Agent` item through the
engine's `Pending::take_items` and `Session::append_items`, byte-identical to
`message_from_a_session`. A second row at the door would be a copy.

Two windows needed closing. A message absorbed at a step boundary is appended before the round is
sent, so it is a row even if the round fails. A message absorbed mid-generation and handed back by a
failing round returns to `HubSteering::held` instead of being appended, so a child whose next turn
never came left no row at all - closed in `Harness::run_rounds` + `keep_the_words_it_never_read`,
which appends what the turn never read and REMOVES it from the carried set. `Stub::cutting` reaches
that window; a child whose FIRST turn fails leaves its thread, so the doomed turn must be a later one.

The upward edge was missing an ADDRESS, not a door: `PARENT_HANDLE` (`"parent"`) is resolved by
`TaskRunner::parent()` before `resolve_child`, so a slug named `parent` cannot shadow it. And
`message_from_a_session(from, text, mine)` claims the parent relation only when `mine` - the
RECEIVER's own `parent_session_id` - matches.
