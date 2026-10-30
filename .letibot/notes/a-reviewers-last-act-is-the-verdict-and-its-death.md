<!-- abstract: MEASURED 2026-10-29 on the operator's daemon: a reviewer's last act is `review_verdict` and its turn ending, so "not alive" and "has judged" are the same two seconds — the queue's worker re-seated the judged row, and the pass that then found the verdict threw it away (nine reviewers, 18 `reviewing`, no `approved`). Fixed by PEEKING the desk in `review_upkeep`. -->

# A reviewer's last act is the verdict and its own death, so two loops read one child's fate

MEASURED 2026-10-29 evening, `main` at `e2fe4611`, from `queue_transition` and the session store of
the operator's daemon: `agent/fix-tool-mount-landing` went `reviewing → waiting` at 17:54:22 with
*"the reviewer … is not alive in this daemon any more (it finished without handing a verdict in,
…)"* — eight seconds after that reviewer's `review_verdict` returned `ok` at 17:54:14 and its last
assistant row said *"Verdict handed in: **accept**."* Across three rows: 34 transitions, **not one
to `approved`**, 18 `reviewing` on one branch, ~nine reviewers, and `verdict`/`reviewed_sha` empty.

## The mechanism, and why it is structural rather than a bug in either loop

Two threads judge a reviewer's fate and they read different things:

* `RepoQueue::review_upkeep` (`crates/harnessd/src/mergequeue/serve.rs`, the queue's own worker,
  every pass) asks the door *is this child live?* — and a reviewer that has finished its turn is
  **not** live (`QueueDoor::session_live` = `Registry::get || Registry::opening`). So "handed the
  verdict in" and "gone" are the same two seconds, and this loop always sees the second one.
* `review::serve` (`crates/harnessd/src/mergequeue/review.rs`, the queue session's loop) applies a
  waiting verdict only when `ours` = row still `reviewing` **and** `owner == me` **and**
  `reviewer == this child`. The re-seat cleared `reviewer`, so the verdict reached a pass that
  found the row was nobody's and dropped it — `take_verdict` is never even called.

## The fix that landed (branch `agent/a-verdict-waiting-is-not-a-dead-reviewer`)

`ReviewDesk::verdict_waiting(child)` — a **peek** (`crates/tools/src/builtins/verdict.rs`) — and
`review_upkeep` defers both its `gone` and its `late` re-seat when it answers yes. Taking stays
where it was: `review::serve`'s `take_verdict`, the only place that can write the row. The desk had
to be threaded to the queue's thread (`QueueEnv::desk`, `spawn_for(…, desk)`, made in `cli.rs`
BEFORE the queues start and then put on `Parts` — `Parts::load` would otherwise make a second one).

## The rakes

* **A deferral here is bounded by the daemon's own clock, not by the worker.** `REVIEW_PASS_SECS`
  is 30 and `daemon.rs` calls `ensure_gatekeeper()` + `serve_reviews()` on that tick regardless of
  any row's state; the child's own settlement wakes the parent sooner. So "defer and it is written
  next pass" is safe *because* the second loop has a clock — if that clock is ever removed, this
  rule becomes a row stuck `reviewing` for ever.
* **`Parts::load` makes the desk, and `cli.rs` starts the queue thread ~450 lines earlier.** The
  two are ordered wrong for a shared desk, which is why `cli.rs` now makes it and assigns it onto
  `parts` after the load. A second desk is silent: the reviewer's verdict simply never reaches the
  queue.
* **The genuinely-dead reviewer must keep its attempt.** The desk being *open* for a child is not
  a verdict — `verdict_waiting` asks `handed.is_some()`, and
  `a_reviewer_that_died_without_handing_a_verdict_in_is_still_re_seated` pins that.
* The two regression tests fail at `e2fe4611` on `attempts == 1, reviewer: None` (verified by
  deleting the `!handed_in` guard): `a_verdict_waiting_at_the_desk_is_not_a_dead_reviewer` and
  `a_verdict_waiting_at_the_deadline_is_not_a_stopped_reviewer`, in
  `crates/harnessd/src/mergequeue/serve/tests.rs`.
