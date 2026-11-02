<!-- abstract: A `Peek`/`Peeked` pair has no client request id (a refusal is `Rejected` with `peek:<session>`), a row cursor counts against `items_dropped`, and a bridge fixture that seats `--bash` cannot open a session without a delegated cgroup. -->

# B8's footnotes: a peek has no request id, and a fixture's seat decides whether it can open

`ClientFrame::Peek { session_id, shape }` carries no `client_request_id` - it is a read, not a verb -
and `ServerFrame::Peeked` carries none either. The answer is matched by `session_id`; a refusal is a
`ServerFrame::Rejected` whose `client_request_id` the SERVER mints as `peek:<session_id>`. A waiter
that counts answers only sits out its whole patience on a refused read, which is why
`crates/bridge/src/head.rs` keeps `peek_error: Option<(session, reason)>` beside `peek`.

A row cursor is a SESSION row number, not a window position: a `Snapshot` is the tail of a
conversation, so `items[0]` is row `items_dropped`. `crates/bridge/src/rows.rs`'s `page()` is
`skip = since.saturating_sub(items_dropped)`, `next = items_dropped + len`, with a `dropped` count
when `since < items_dropped`.

A bridge fixture that seats `--bash` cannot open a session on this box: `Seat::Leticode` +
`allow_bash` picks `HostBackend::executable("/")`, which needs a delegated cgroup v2 subtree, and the
daemon dies at its first open. A test that runs no commands sets `cfg.allow_bash = false`, and a
session is REGISTERED before it is OPEN, so a `Settings` read in that window is an honest empty list
with no `mode` row.
