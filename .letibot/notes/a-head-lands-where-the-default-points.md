<!-- abstract: An `ATTACH` with an empty session id asks for the daemon's current session, so a head that reconnects without naming one lands wherever the last switch left the default; and a rule that reads the parent chain is blind to a row whose edge is ordinary. -->

# A head lands wherever the daemon's default points, and that is not a bug in the daemon

`ClientFrame::Switch` is answered inside `serve_conn` on that connection's own writer, so one head
switching cannot move another. What does move a head is its own reconnect: `Registry::set_default`
follows whoever switched last, and `crates/tui`'s `Link` kept the session id the launcher passed, which
is empty for `leticode` and for `leticode --session B` alike - so `reconnect` sent `""` and
`registry.resolve("")` answered the default.

The rake: an `ATTACH` with an empty session id is not "no session", it is "the daemon's current one".
Anything that re-attaches - a reconnect, a probe, a fixture - has to name the session it means;
`Link::note_session` records it from every `Hello` the head applied.

And a classification rule that reads a GRAPH (the parent chain) is blind to a row whose graph edge is
the ordinary one. A reviewer can be parented on the operator's conversation, and neither its id nor its
chain says review, so the marks must be on the row: `session.role == GATEKEEPER_SEAT`, and a title
starting with `GATEKEEPER_TITLE_PREFIX` for a head, which carries no role at all.
