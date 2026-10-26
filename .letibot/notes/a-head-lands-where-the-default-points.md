<!-- abstract: MEASURED 2026-10-26 (S2): the daemon cannot move a head that did not ask — the operator's "the habr head switched to the rust session" was the head's own RECONNECT. And the grouping rule needs THREE marks for the review queue: a reviewer can be parented on the operator's conversation, where neither the id nor the chain says what it is. -->

# A head lands wherever the daemon's default points, and that is not a bug in the daemon

MEASURED 2026-10-26, branch `sessions-grouped` (package S2, letibot), from the operator's
report: *"I had a session running in stroppy-io with that habr article, and then I resumed
another session that does the rust rewrite, and the habr head switched to the rust session
wtf"*.

## The frame layer is per-CONNECTION, and that is what makes the report confusing

`ClientFrame::Switch` is answered inside `serve_conn` with a second `Hello` written on **that
connection's** `writer`; `leave` detaches only `seat.head_id` from the hub it is in; the pump
is per connection. So a second head switching sessions cannot move the first, and
`crates/sessionlog/tests/sessions.rs`'s
`one_head_resuming_another_session_leaves_the_head_that_was_there` pins it (the first head
sees a `HeadAttached` for the newcomer on its own session's log and **no** `Hello`).

## What DOES move a head: its own reconnect, and the daemon's `default`

Two facts compose, and neither is wrong on its own:

* `Registry::set_default` is called by the `Switch` arm — *the session a bare `ATTACH` lands
  on* is whoever switched last (there is a test for that too:
  `the_default_session_follows_whoever_was_last_used`).
* `crates/tui`'s `Link` kept the session id **the launcher passed**, and the launcher passes
  `--resume ID` — never `--session` — so for `leticode` and for `leticode --session B` alike
  that string is **empty**. `Link::reconnect` sent it, and `registry.resolve("")` answers the
  default.

So: head 1 in A → another terminal switches to B → the default is B → head 1's link hiccups →
it comes back **in B**, silently, with `since_seq` (a mark in A's log) sent against B.

Fix: `Link::note_session` from every `Hello` the head applied (called in `tick`'s drain loop,
and from `seated_by` for the first attach), and `reconnect` names that. Regression-checked by
commenting the call out: the reconnect test then fails with `left: "s-rust", right: "s-habr"`.

**The rake:** an `ATTACH` with an empty session id is NOT "no session" — it is "the daemon's
current one". Anything that re-attaches (a reconnect, a probe, a test fixture) has to name the
session it means, or it is asking the daemon to choose for it.

## Three things found on the way, none of which are this test's subject

* **rano's picker has no row that is not a session.** `rano::agent::picker::SessionRow` is
  `{depth, name, session_id, here, children, generating, live, rows, heads, model, workspace}`
  and every row is drawn as TWO lines (the row, then `      <session_id>  <workspace>`, not a
  stop). A group header therefore costs two lines and must carry `session_id: ""`; the second
  line is a good place for the group's legend, which is what the picker does. Its footer says
  *"enter switches"*, which is now inexact on a group line (enter folds) — rano's text, not
  this tree's.
* **`origin` cannot ride `SessionRecord` cheaply.** That struct has 58 literal construction
  sites across the workspace (most in tests). The column is written by
  `Store::set_session_origin` (an `UPDATE ... WHERE origin IS NULL`, so *set once* is the SQL)
  after `put_session`, which also matches the order the daemon has the facts in: the origin is
  known when a session is CREATED, and the row is written when the worker OPENS it.
* **`--list-sessions --tsv` is parsed positionally by `scripts/letibot --daemons`**
  (`read -r id title live running heads rows model`), so a grouping column or a header line in
  the TSV would break the launcher. The human listing and the TSV are two formats; only the
  first is grouped.

## UPDATE 2026-10-26 — the Linux gate found the third mark, and it is the one that matters

`-p letibot-harnessd --test continue_picks`, `the_line_on_stdout_is_the_conversation_and_not_the_reviewer`,
went red on the gate: it asserts that `--list-sessions` offers no reviewer, and the first rule
(walk the parent chain for the queue's session) put **ten `s-gk-*` rows in `subagents`**.

The fixture is not an odd shape — it is the layout from BEFORE the queue had a session of its
own, which real stores from older daemons still hold: a reviewer is `s-gk-3`, its parent is the
operator's conversation `s-mine`, and nothing about its id or its chain says *review*. The marks
that do are **on the row**:

* `session.role = "gatekeeper"` — `Seat::Gatekeeper.as_str()`, now
  `letibot_sessionlog::GATEKEEPER_SEAT` so the daemon's `is_conversation`, `Seat::parse` and the
  grouping rule read one constant;
* the title starting with `GATEKEEPER_TITLE_PREFIX` — the mark a HEAD has, because
  `SessionBrief` carries no role at all (`App::fold_subagents` fills the same gap the same way).

So `GroupRow` carries `role` and `title`, and `is_queue_work` is `origin == Queue || is_gatekeeper_session(id)
|| role == GATEKEEPER_SEAT || title.starts_with(GATEKEEPER_TITLE_PREFIX)`.

**The general trap, and it is not about sessions:** a classification rule that reads a GRAPH
(the parent chain) is blind to a row whose graph edge is the ordinary one. When the fact is about
**what the row IS** rather than **where it hangs**, the row has to say it — and the way to find
out which facts those are is to look at what the OTHER readers in the tree already test
(`is_conversation` had both marks, four months earlier).
