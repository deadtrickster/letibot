<!-- abstract: MEASURED 2026-11-01: every door that CREATES a child writes `parent_session_id` (store census: 1720 sessions, 0 orphans, 0 parentless `-sub-` ids, 384 `subagent_run` rows all parented) — the hole was that the HARNESS's own config for a session the launcher reopens by id had none, so a child reopened with `--session` came back a root; and `put_session` writes the row once, so a NULL written before this branch stays NULL for ever. -->

# The parent was written by every door, and read as none by the open

MEASURED 2026-11-01, branch `agent/a-child-knows-its-parent`, against a read-only copy of the
operator's `~/.local/share/letibot/sessions.db` (1720 session rows) and the code.

## The census, which is the part that was not obvious

| query | answer |
|---|---|
| sessions | **1720**, of which **1582** have a parent |
| parentless ids containing `-sub-` (the mint's own mark) | **0** |
| rows whose `parent_session_id` is not itself a session id (orphans) | **0** |
| `subagent_run` rows | **384**, every one whose child row exists has a parent on it |
| `subagent_run` rows whose child session row does not exist | **3** — depth-3 children that failed *before* their open (`put_session` runs at the open, so a refused spawn leaves no row) |

So `mint_sub_id`'s `-sub-` is never the only record of the relation, and nothing in that store is a
child drawn at the top of its project. The four parentless rows whose *titles* look like a child's
(`The reviewer of …`, three `You are working in a copy`) are `NewSession`-shaped ids with no
`-sub-`: sessions a person or a package created as top-level, which is what the rule says they are.

## What was actually missing: the HARNESS's own answer, not the row

`parent_session_id` was written by every door that creates a session (a `task`/`task_start` spawn
via `sub_cfg` + `adopt`, the daemon's startup `create_under`, a head's `ResumeSession`, a parked
child's `reopen_parked`, the room's tag agents). What a session *reopened by id* got was different:
`Sessions::open_first_with_seat` opens the daemon's first session from `self.base` verbatim, and
`harnessd --session <id>` sets the id and nothing else — so a **child reopened that way opened as a
root**:

* `top_level = cfg.parent_session_id.is_none()` → it opened a **room link** (a subagent must speak
  through its parent, and the door that refuses a subagent never saw one);
* `SeatFacts.subagent` false → the ROOT paragraph in its prompt;
* the flowy door registered on a session that is somebody's child;
* no parent id to address, so it could not `task_message` its parent at all.

`Registry::parent_for(said, entry, row)` is now the rule and `Harness::open_with_registry` applies it
once. `Harness::parent_session()` is the settled fact.

## The trap that decides where it goes: resolve AFTER the mode block

`open_with_registry`'s mode block reads `cfg.parent_session_id` to ask a *different* question — does
this session inherit a LIVE parent's point — and takes the session's own row when the answer is no.
A session opened as the daemon's first has no live parent even when it is a child, so resolving the
parent earlier hands a resumed child the daemon's `--mode` instead of its own. Measured by writing
the resolution at the top first: `a_child_a_door_opened_without_its_parent_still_has_one` still
passed, but the mode came from the wrong place.

## Why "one place" is a function and not a call site

At the `task` child's open it is **not in the registry yet** (`adopt` runs after the open), so
`parent_for`'s entry source answers `None` there and the row does not exist yet either. `sub_cfg`'s
`Some(parent)` is therefore load-bearing: drop it and the child's row is written NULL for ever.
The rule is one function with three inputs; the spawn states one of them.

## `put_session` writes the row ONCE

`Harness::open` calls `put_session` only `if stored.is_none()`, so a row written with a NULL parent
by an older build is never repaired — by this branch or any later one. The four rows above are that
case, and they are why the operator's tree can still show an old "child" at top level.

## The other readers of the same field, and their sources

* the head (`App::placements` / `session_rows`) — `SessionBrief`, which is the registry's entry with
  the store's row as the fallback (`Registry::list`);
* `harnessd --sessions` and `--continue`'s `is_conversation` — the **store row**, directly
  (`Store::list_sessions`); `--continue` resolves only NULL-parent rows, which is the corollary;
* `unconfined_by_inheritance` — the **store row**, walking the ancestor chain (a third source, and
  deliberately the row: it asks what the session was seated with);
* `App::system_kind`'s `origin == Queue` arm — `parent_session_id.is_none()` decides gatekeeper vs
  reviewer, so this field also decides *where* in the tree a row is drawn.

## Left open

`cfg.depth` for a reopened child is still the daemon's `0`, so its own children are minted at depth
1 and `max_subagent_depth` counts the tree one level shallow. `depth` is documented as a spawn-time
increment and fixing it means walking the chain at open — a different change.
