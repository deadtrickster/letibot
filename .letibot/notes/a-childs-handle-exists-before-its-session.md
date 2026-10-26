<!-- abstract: MEASURED 2026-10-26 (package QE): a spawned child's handle is returned before its session is adopted, so every "is this session alive?" question answered `false` during the open — which for a VM-placed reviewer is a clone and a boot, and re-seated live reviewers 2.0 s after they started, three passes running, to `blocked`. -->

# A child's handle exists before its session, and every liveness door said "gone"

MEASURED 2026-10-26 (package QE, branch `reviewer-liveness`), from the operator's daemon
(`~/logs/harnessd-2000f08acfaa.log`, read-only) whose every review was BLOCKED within seconds.

## The measurement, from the store's own queue history

`queue_transition` for one row (`sqlite3`, on a copy of `~/.local/share/letibot/sessions.db`):

```
reviewing -> reviewing  …095383  reviewer s-gatekeeper-2000f08acfaa-sub-… started
reviewing -> waiting    …097403  the reviewer … is not alive in this daemon any more
waiting   -> reviewing  …097461  claimed again
```

**2.0 s, one queue pass** (the worker idles at 2 s and re-runs at once when anything moved). Three
of those and `Reviewing`'s upkeep reaches `MAX_REVIEW_ATTEMPTS` → `blocked`. The same shape twice
more in the same history, with a pre-G2 `s-gatekeeper-sub-…` id as well, so this was never a
G2/T2 rename.

## The cause is the ORDER of two acts, not the re-seat rule

`HarnessTaskRunner::start` mints the subagent id and returns it; the child's own thread then
`Registry::adopt`s its hub — and `adopt` runs only after `Harness::open` has finished, which for a
reviewer is `git clone` + firecode boot (tens of seconds). `Registry::get` answers `None` for the
whole of that window, so `QueueDoor::session_live` — asked by `RepoQueue::review_upkeep`'s
`Reviewing` arm — said *gone* about a child that had just been started and was still opening.

**A rule that reads liveness off `registry.get` reads "not yet" as "no longer", for every child,
in every daemon, on every spawn.** The announcement (`Registry::announce_spawn` /
`spawn_settled` / `opening`, spent by `adopt`, withdrawn by a `Drop` guard on the child thread)
is the missing third state; `RegistryDoor::session_live` is `get().is_some() || opening()`.
Two more readers wanted it afterwards: `resumable()` (else a resume could `create` an entry the
child's `adopt` is about to take, and the child dies of a fake minting bug) and `mint_sub_id`
(an announced id is taken).

## How to make an OPENING child a state a test can stand in

The child's first act before its session exists is `served_ctx(&parent.endpoint)` — a `GET
/props` for its window (`HarnessTaskRunner::start`'s `local_window`). So a loopback endpoint that
**holds that answer** holds the child inside its open, deterministically, with no sleep and no
race. That is `mergequeue::serve::tests`' `HeldProps`, and the test it makes possible:
claim a row, run the real spawn, wait for the ask, run three real supervisor passes (row must stay
`reviewing`, same reviewer, 0 attempts), release, and the child ends without a verdict → the row
IS re-seated with 1 attempt. Without the announcement the first pass re-seats it, so the test is
red on the bug and green on the fix.

Fixture notes that made it cheap: `vocab_gguf = None` gives the byte vocabulary (**no GGUF, so no
apparatus skip**), `unconfined = true` because the reviewer is seated `gatekeeper` and a confined
backend is refused on a Mac, and `endpoint` = the held loopback server so nothing reaches
127.0.0.1:8080. The child's turn then fails for having no model — which is what "ended without a
verdict" costs in a fixture with no provider.

## The general rake

*An id that exists is not a session that exists*, and code that asks "is this one still there?"
by looking for a registry entry must name which of the two it means. The half-live state has a
second symptom nobody has looked for yet: every other reader of `Registry::get`/`holds` — attach,
`ring_project`, worktree cleanup — answers "not here" for a child that is on its way in.
