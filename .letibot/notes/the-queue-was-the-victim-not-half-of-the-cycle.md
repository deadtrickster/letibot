<!-- abstract: The merge-queue wedge was not a lock-order inversion: `JobWatchers::stop` took its own settlement queue twice on a non-reentrant mutex, so nothing reachable from `Hub::publish` may wait on such a lock. -->

# The queue was the victim, not half of the cycle — and "leaf lock" is the rule

The queue's thread holds no queue mutex at `raise`, so it was not one edge of a cycle. Its
damage was that `pass` never returned, so `beat()` was never called again - a lease heartbeat
511 minutes stale.

The other side was one thread taking one mutex twice: `JobWatchers::stop` locked `completions`,
then `take_held` published, the publish refreshed activity, and the activity asked `pending()`,
which locked `completions` again. `std::sync::Mutex` is not reentrant, so the closing session
wedged itself and every publisher on the box queued behind it.

Rule: nothing reachable from `Hub::publish` may block on a lock a non-queue thread can hold.
Every other lock there is a leaf: held across a clone, never across a call that can block, and a
leaf lock cannot be in a cycle. The fix keeps `pending` and `held` as atomics written inside the
lock that owns the queue (`held_now`), and the watchers as one `Arc<Vec<Watched>>` snapshot
behind a leaf mutex.

Rakes: assert a deadlock on a bounded `recv_timeout` before joining the wedged thread, and
rebuild the watcher snapshot after a settlement is removed - its own `refresh_activity` runs
while the handle is still in `mine`.
