<!-- abstract: MEASURED 2026-10-31 (the room's pane): a request id of `verb + now_ms()` collided twice inside one test, and the room's replay window answered the second act *the same* instead of performing it — silently, so three abandonments sat at two and the raise never fired. Plus: the daemon's `validated` never ran `validate_for`, so no host checked a row's verb against its manifest. -->

# A request id minted from a clock is not unique, and an idempotency window makes the second act vanish

MEASURED 2026-10-31, branch `agent/the-rooms-pane`, `crates/harnessd/src/room/link.rs`'s new
round-trip test (`the_four_controls_reach_the_room_and_change_it`).

## The shape

The room keys its replay window on `(session, id)` — that is what makes a reconnecting client's
repeat answerable instead of acted on twice (`Room::replayed`/`remember`, `REPLAY_WINDOW`). Both of
the daemon's doors minted their id from the clock alone:

```rust
let id = format!("{verb}-{}", letibot_room::server::now_ms());
```

Two round trips on a unix socket are tens of microseconds, so `abandon i1` pressed twice — or two
different verbs on one row — really can share a millisecond. The second frame is then answered from
the window: **Ok, with the FIRST act's reply, and no state change and no error.**

## What it looked like

The test asserted an item at two abandonments, then claimed and abandoned it a third time and
expected a raise. `open_questions()` was empty. Everything about the *state machine* looked wrong
(`ABANDON_THRESHOLD` is 3, `lose()` raises at `count >= 3`) and nothing named the transport.

## The fix

`link::request_id(what)` — the clock **and** a process-wide `AtomicU64`. The tool's door and the
pane's door both use it.

## The rake

**An idempotency key derived from a clock has a collision window as wide as the fastest possible
pair of operations**, and on a local socket that is microseconds. When the window swallows one, the
symptom is *a state machine that did not move*, which is the last place anybody looks. If a test
says an act did nothing, ask whether the act was ever *performed* before asking what it did.

## And the second thing this branch found

`PluginHost::validated` (`crates/harnessd/src/plugins.rs`) called `View::validate()` and **not**
`validate_for(declared)`, so the daemon — the host that performs a row's verbs — never checked them
against the layer's `actions`, although the SDK's doc, the queue layer's manifest and `docs` all
said it did. The head checked it for the panes the head draws; a pane the DAEMON draws was checked
by nobody. Now `validated` takes the answering layer's `actions` (read off the stack beside the
feeds, before the non-reentrant lock) and `plugin test`/`load` pass them too, so a diagnostic asks
the same question the running host asks.
