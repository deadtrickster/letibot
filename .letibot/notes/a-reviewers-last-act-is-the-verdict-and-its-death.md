<!-- abstract: A reviewer's last act is the verdict and its turn ending, so "not alive" and "has judged" are the same two seconds - the queue re-seated the row and the pass that then found the verdict dropped it. -->

# A reviewer's last act is the verdict and its own death, so two loops read one child's fate

Two threads judge a reviewer's fate and they read different things. `RepoQueue::review_upkeep` asks the door *is this child live*, and a reviewer that has finished its turn is not live - so "handed the verdict in" and "gone" are the same two seconds.

`review::serve` applies a waiting verdict only when the row is still `reviewing`, owned by me, and `reviewer` is this child; the re-seat cleared `reviewer`, so `take_verdict` was never called. The operator's symptom was 18 `reviewing` on one branch, no `approved`, and a row re-seated eight seconds after its verdict answered ok.

`ReviewDesk::verdict_waiting(child)` is a peek, and `review_upkeep` defers both its `gone` and its `late` re-seat when it answers yes. Taking stays in `take_verdict`. The desk is made in `cli.rs` before the queues start and put on `Parts`, because `Parts::load` would make a second one - and a second desk is silent: the verdict never reaches the queue.

Rake: the deferral is bounded by the daemon's own clock (`REVIEW_PASS_SECS`), so if that tick goes the rule becomes a row stuck `reviewing` for ever. And the desk being open is not a verdict - `verdict_waiting` asks `handed.is_some()`.
