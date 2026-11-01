<!-- abstract: MEASURED 2026-11-01: the empty restart's door is now decided by `restart::Owed` (mid-turn / settlements / due rows / plan rows), and the mid-turn arm is `StoredSession::last_item` — with `Speaker::Agent` User rows EXCLUDED, because a filed notice and a wake's notice are the same row and counting them alternates a model call into every idle restart. -->

# A restart's door is what the session OWES, not what was running

MEASURED 2026-11-01, branch `agent/the-restart-wakes-the-session` (`crates/harnessd/src/restart.rs`'s
`Owed`/`mid_turn`, `Harness::owed_after_a_restart` and `say_nothing_was_running`), from the
operator: *"it is not ok that restart notice arrived with my continue … so i expected that after
restart you will continue"*.

`restore_after_restart` has two doors and the choice was *was anything running*. It is now *does
the session owe anything*: `runs/monitors/jobs/settled` (the restore path) keep their turn, and an
otherwise empty restart is a turn when `Owed` holds — a turn in flight, a settlement this daemon
queued and never delivered, a row whose condition is met, or a plan with `Pending`/`InProgress`
rows. Nothing owed keeps the transcript row and spends no call.

## The mid-turn arm is `last_item`, and `Speaker::Agent` is the exclusion that matters

`Store::session(&sid)` already answers `last_item` (the current transcript's last row) — the
session row `restore_after_restart` was reading anyway for `came_back`, so the arm is free. The
rule: `Assistant` with no calls = a finished turn; **everything else is a turn that never got its
answer**, except a `User` row whose `speaker` is `Agent`.

That exclusion is load-bearing and it is the thing to not rediscover: the empty restart's own row
is a `User { speaker: Agent }` with one `Text` part, and **so is a wake's notice** — the store
cannot tell *a sentence filed for later* from *a wake whose turn died*. Counting them makes
restart N+1 read restart N's filed row as work owed: a model call, then an answer row, then the
next restart files a row again — **an idle session alternates a call into every second restart**,
which is exactly the cost the door exists to avoid. What the exclusion misses (a wake whose turn
died) is covered by the arm that caused that wake whenever the row behind it still exists.

## The other three, and what each already did

* **settlements** — `deliver_settlements_left_undelivered` runs at every open and queues into
  `job_watch`, and the wake `put_a_restored_session_to_work` rings at open already drains it, so a
  settlement was already a turn. It returns its count now so the restart sentence can RIDE that
  turn instead of being filed beside it.
* **a due row** — `Harness::wake` reads `due_todo_rows` itself, so that arm was already a turn for
  the same reason; the sentence just rides it now.
* **a plan with rows owed** — nothing woke for it at the open: the idle plan check is a minute out
  and its text is the plan, not the restart. That was the real hole beside the mid-turn one.
* Gatekeeper and subagent sessions are excluded by `a_persons_session` before any of this, which is
  what keeps the cost to a person's sessions.

## Staging a "mid-turn" fixture: write the row behind a CLOSED harness

`Harness::open` creates the session's transcript, so a row can only be staged into it after the
open — and then the harness's own in-memory row count is behind the store's, so the next
`record_notice`/`persist` is refused with *"transcript_item seq must be the next one"* (MEASURED:
the notice was silently not recorded and the test passed on the in-memory items). The shape that
works: stage the row, `drop` the harness, open a second one over the same store — which is also the
honest shape, because a session has ONE writer and the second open is refused while the first still
holds its claim. `seq` 0 needs `tok_offset` = the prefix's token count (`get_stable_prefix`) and the
trigger checks nothing else about the chain.

## Rake

**A cost argument decides a door for the case it was measured on, not for the class it was written
over.** *"A wake is a model call"* was true and decided the door for every empty restart; it was
wrong for the ones with work outstanding, and the correction is not "always wake" but a predicate
over what the session wrote down. Prose — a model that says *"after restart i owe you"* — is not
one of them, and is said so rather than guessed at.
