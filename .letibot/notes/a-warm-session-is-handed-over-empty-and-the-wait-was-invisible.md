<!-- abstract: MEASURED 2026-10-26 (branch warm-session-open): the warm `leticode --session ID` path DOES restore — a real store, a real HeadClient, every row on the wire — but the head is seated on an empty snapshot while the worker is busy, and a `Filling` is in NO snapshot, so nothing told it a restore was coming. -->

<!-- abstract: MEASURED 2026-10-26 (branch warm-session-open): the warm `leticode --session ID` path DOES restore — a real store, a real HeadClient, every row on the wire — but the head is seated on an empty snapshot while the worker is busy, and a `Filling` is in NO snapshot, so nothing told it a restore was coming. -->

# A warm session is handed over empty, and the wait was invisible

MEASURED 2026-10-26, branch `warm-session-open` (letibot), on the incident where
`leticode --session s-1793…` against a running daemon drew the session's title and an empty
transcript over 166 stored rows.

## The daemon's half was never broken — measured, not reasoned

Two tests, both real:

* `crates/harnessd/tests/warm_open.rs` — a real `Store` with two hand-written conversations,
  a real `HeadClient` over `serve_registry`, and `Sessions::open` driven exactly as
  `Work::Open` drives it. Every stored row reaches the head (announced AND with a body),
  `resume_report` says `rows == N` (the `RESUMED … row(s)` line), and a prompt queued
  BEFORE the open runs on the restored conversation (the fake provider's request body
  carries the earlier text; the store grows from N rows to N+2 in the SAME transcript).
* So "the daemon did not restore" — the first reading of the operator's report — is wrong.
  `sessions.db`'s own evidence agrees (the reply at seq 171 refers to the pptx).

**The order a head sees**: `ResumeSession` → `Sessions{created}` → `Switch` → a `Hello`
whose snapshot is EMPTY (the hub was registered a moment ago and
`Harness::open` runs on the worker, behind whatever turn it is running) → the restored rows
as `TranscriptAppended`/`TranscriptContent` when the worker gets to it.

## What WAS wrong: two sources disagreed and the screen believed the emptier one

The title and `SessionBrief.stored_items` come from the STORE; the rows come from the
SNAPSHOT. For the length of that wait the head held `stored_items: 166` and `items: []`,
and drew the empty-transcript banner — *"attached, and this session has said nothing yet.
Type a question and press enter."* — over a conversation that existed. The prompt typed then
sat under a bare `queued` with nothing saying what it was behind.

Fixes (both small, both in this branch):

* `sessionlog::RESTORING_ROWS` is one constant, because the name has two speakers: the
  socket publishes `Filling{what, "rows", 0, brief.items}` in the `ResumeSession` arm (the
  store's own count is right there), and `Harness::open` walks the same name row by row and
  ENDS it (`republish_after`'s unconditional last tick).
* `App::restoring` (tui): an empty transcript for a session whose brief says
  `stored_items > 0` is drawn as a `system (restore)` row naming the count, not as the empty
  banner; it goes on the first row and on `resume_failed`. Only the `items.is_empty()` case
  counts — `items.len() < stored_items` is the ordinary state of a COMPACTED session and
  would claim a restore that finished an hour ago.

## The rake: a `Filling` is in no snapshot

`Filling` is ephemeral — in no snapshot, and `Hub::attach` replays no backlog — so a fill
published BEFORE a head attaches is never seen by it, which is exactly the case here (the
fill is published at registration, the head attaches at the switch). Hence
`Inner::filling_now`, beside `held_now`/`activity_now`, re-said on attach and on resync.

**And it is kept only while INCOMPLETE** (`done < total`): a restore that finished leaves
its `N of N` tick, and re-announcing that to a head attaching afterwards would say an
operation that is over is still running. A restore that FAILED leaves its `0 of N` there —
a head draws it for its own 15-second no-news window, beside the `resume_failed` warning
that names the reason.

## Testing a head-side frame on this box

`crates/tui/tests/` can serve a REAL registry over a REAL socket (`serve_registry`) and
drive `Link` + `App` with `link.tick` — that is the only way to assert what a head DRAWS
from a switch, and `warm_session.rs` does it with no daemon and no model. The daemon's half
is `harnessd`'s, because the tui crate is below it.
