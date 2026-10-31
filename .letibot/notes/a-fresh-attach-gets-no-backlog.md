<!-- abstract: MEASURED 2026-10-31: `Hub::attach` with `since_seq = 0` hands a fresh head a SNAPSHOT and an EMPTY backlog, so a warning published before a head attaches is never delivered to it — which is why `letibot-mcp wait` (a fresh attach per call) could not see `daemon_start_failed`; the real sequence is that `ensure` returns when the SOCKET answers, before the session opens, so the warning lands live on a head that is already waiting. -->

# A fresh head attach is a snapshot and NO backlog — so a warning before it is invisible

MEASURED 2026-10-31, branch `agent/the-placement-and-bridge-cluster` (R103), after assuming the
opposite and watching a test fail.

`Hub::attach` (`crates/sessionlog/src/hub.rs`, the `if since_seq == 0` arm) answers a fresh attach
with `Some(snapshot)` and `Vec::new()` for the backlog. The replay is only for a head that names a
seq (`since_seq != 0`). So **an event published before a head attaches is not delivered to it** —
unless the snapshot carries that fact. `held_now`, `activity_now` and `filling_now` exist for
exactly this reason: they are re-said (`append_and_fan`) to every new head after its snapshot.
Warnings are not in that list, and the snapshot has no field for them.

## What that means for the bridge

`letibot-mcp wait` attaches a NEW head on every call, so:

* a `daemon_start_failed` warning published *before* `wait` runs is one the waiting head never
  sees. My first cut of the R103 test published it first and asserted exit 20 — it got `0 — the
  turn ended` with the previous turn's words, and the fix looked broken when it was the fixture
  that was wrong;
* the real sequence is the live one, and it is why the bug was an hour rather than a millisecond:
  `daemon::ensure` returns as soon as the SOCKET answers, which is **before** the session opens, so
  the orchestrator's `wait` is already attached when the boot fails and the warning fans out to it
  live.

The fix therefore lives in the head's fold (`State::boot_failed`, read first in `Head::wait`), not
in a replay. A test that means to reproduce it must publish *while* the wait is blocked.

## And the other terminal fact a `wait` ignored

A `Bye` from the daemon (`State::closed`) ends nothing in `wait` either: it sat on a dead socket
until its deadline. Both are now in the same precedence list — ask, boot failure, closed
connection, turn failure, quiet.
