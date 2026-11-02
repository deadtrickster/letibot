<!-- abstract: A deletion candidate: the head leak it describes is closed by `Drop for Seat`, and the two facts still worth having are that `Answers::ask` reads `deciding_heads` and that every daemon opens a gatekeeper session with nobody attached. -->

# A stopped head can stay in `deciding_heads` for ever, and the `?`s are why

This file is a deletion candidate. The leak it described is closed: `impl Drop for Seat`
(`crates/sessionlog/src/server.rs`) detaches on every path, including the `?`s in `serve_conn` that
return before the explicit detach after the loop, and it is idempotent across a `Switch`. The note
that owns the mechanism, the failing shape and its test is
`a-registered-head-whose-hello-never-landed-lives-for-ever`.

Two facts worth carrying forward. `Hub::deciding_heads()` is read by `Answers::ask` and
`ask_question` before they post a card, so a head left in it makes every later ask on that session
wait out its deadline and come back as "nobody answered". And every daemon opens a gatekeeper
session at startup with nobody attached - its id is per workspace now
(`letibot_sessionlog::gatekeeper_session`), not the old constant `s-gatekeeper`.
