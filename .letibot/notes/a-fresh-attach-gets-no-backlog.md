<!-- abstract: `Hub::attach` with `since_seq = 0` hands a fresh head a snapshot and an empty backlog, so a warning published before it attaches is never delivered - only the re-said facts (`held_now`, `activity_now`, `filling_now`) survive, and warnings are not among them. -->

# A fresh head attach is a snapshot and NO backlog — so a warning before it is invisible

`Hub::attach` with `since_seq == 0` answers a fresh head with a snapshot and an EMPTY backlog; the
replay is only for a head that names a seq. So an event published before a head attaches never reaches
it unless the snapshot carries the fact, and only `held_now`, `activity_now` and `filling_now` are
re-said to every new head after its snapshot. Warnings are not, and the snapshot has no field for them.

`letibot-mcp wait` attaches a new head on every call, so a `daemon_start_failed` published before
`wait` runs is invisible to it: a test that publishes first and then waits gets `0 - the turn ended`
. The real sequence is live - `daemon::ensure` returns as soon as the
SOCKET answers, before the session opens, so the orchestrator's `wait` is already attached when the
boot fails.

So the fix lives in the head's fold (`State::boot_failed`, read first in `Head::wait`), not in a
replay, and a test that means to reproduce it must publish WHILE the wait is blocked. A `Bye`
(`State::closed`) used to end nothing either; the precedence is ask, boot failure, closed connection,
turn failure, quiet.
