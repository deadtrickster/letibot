<!-- abstract: MEASURED 2026-10-30: a restart with nothing running has exactly one piece of per-session evidence (`session.created_at < Registry::started_ms`), and the daemon's open RINGS the session — so anything left in `restart_notices` becomes a turn; the empty sentence is a transcript row instead, one-shotted through `queue_told`. -->

# A restart with nothing running: one row of evidence, and a row in the transcript — never a turn

MEASURED 2026-10-30, branch `agent/a-restart-always-says-when` (`crates/harnessd/src/restart.rs`,
`Harness::restore_after_restart`, `crates/tui/src/ui/transcript/restart_card.rs`).

The operator: *"mb always tell agent about restart? with the current time lol"* — a session whose
daemon came back with nothing to restore was told nothing, so the model went on believing no time
had passed.

## What a session that had nothing running actually leaves behind

Nothing that names a daemon. The restart module's whole trigger was *a record stamped with a
daemon that is not me*, and an idle session has none:

* `subagent_run` / `session_monitor` / `job_context` — absent by definition here;
* `session_writer` — **deleted when the harness drops**, so an orderly stop leaves nothing (it
  survives only a crash);
* `daemon_life` — keyed by instance, and the store is box-wide, so nothing on the session says
  WHICH row is its predecessor.

The one fact left is the session row's own age: **`session.created_at < Registry::started_ms`**
means this session existed before this daemon started, so a daemon that is gone held it. That is
`came_back`. A session this daemon created has `created_at >= started_ms` and is never told.

## The open RINGS the session, so `restart_notices` is a turn

`Sessions::open_first` and `Sessions::harness_as` both end with
`put_a_restored_session_to_work` = `Bell::ring_wake`, and `Harness::wake` runs a turn whenever its
notice list is non-empty (`if notices.is_empty() { return Ok(None) }`). So **putting the empty
sentence in `restart_notices` would spend a model call on it** — the thing the operator's request
does not ask for. `begin_turn_clock`'s own doc records that same ring as the source of the old
"empty wake published a turn that never ran" flake.

So the empty sentence takes a different door (`Harness::say_without_a_turn`):

* **idle** → `record_notice` (a `Speaker::Agent` transcript row): the model reads it at its next
  turn, whenever that is, and no generation is spent;
* **a turn running** → `self.injected` (the step-boundary queue the intent nudges ride), so it
  lands in the turn already open — `HubSteering::try_next` hands it to the engine at the next
  round boundary. Never append a User row mid-turn: it can land between a `tool_use` and its
  results.

The restore case keeps its wake: a resumed child is work to continue, so a turn is the point.

## The one-shot is `queue_told`

`Store::tell_once(key, now)` on the generic "said once" table, key = `restart:<session>:<instance>`.
Spent on the FIRST open whatever that open says, so a second open by the same daemon cannot fall
through to the empty sentence after the records were handled. A store error is treated as *not yet
said* (`unwrap_or(true)`): said twice is dull, never said is the defect.

## Down time in the empty case

Dated from the daemons the session's OWN rows name in **any** state — a monitor it retired, a child
that finished, the newest job row's `job_context`. A session that names nobody says
`down unknown, and no record says when the previous one stopped` rather than borrowing the newest
stop in the box-wide store (that is another folder's daemon).

## Rake

`warning_detail(notice, &[])` used to end with a stray `"\n"` (it always joined a second line), so
the empty restart's warning detail is now the sentence alone. And in the TUI, a header with nothing
under it is a card **only** for the `[restart] the daemon came back at …` opening — `… was down for
4s (…) — under it:` with no counted lines stays a fallback, which `a_restart_or_stale_shape_this_head_does_not_know_falls_back`
pins.
