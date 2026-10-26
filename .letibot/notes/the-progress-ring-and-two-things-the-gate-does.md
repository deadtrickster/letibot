<!-- abstract: MEASURED 2026-10-26 on the bridge: there is no JobStarted on the wire (a backgrounded ToolFinished is the only start signal), a bare `sleep` is admitted at writes-allowed with no card while `git push` asks, and a NewSession session IS opened by the worker, so "a second session nobody reads" is not a real failure mode. -->

# The progress ring, and two things the gate does that a test writer will get wrong

MEASURED 2026-10-26, `crates/bridge` (branch `bridge-events`), building the
`leticode_events` ring and extending `tests/end_to_end.rs`.

## There is no `JobStarted` on the wire, and `sessionlog`'s own code says so

`crates/tui/src/app/events/tools.rs` (the `ToolFinished` arm) states it: *"a
backgrounded call is the job STARTING, and the only event that says so … There
is no `JobStarted` on the wire: `JobSettled` is the whole job vocabulary"*. So a
reader that wants job starts folds `ToolFinished { outcome: ToolOutcome::
Backgrounded { handle, ran_for_ms, .. } }`, and the job's ending is `JobSettled
{ job, state, elapsed_ms }` — `state` is the exit (`"exited 0"`), not a number.
A backgrounded call must NOT also be filed as an ordinary tool result: the
outcome *is* the start.

## A bare `sleep` is admitted at `writes-allowed`; `git push` asks

Measured by an end-to-end iteration that waited 60 s for a card that never came:
the fake model proposed `bash {"command":"sleep 30","background":true}` and the
session went on with its later turns — no `asking`, so no card. The same turn
with `git push origin bridge; sleep 30` **did** raise a card, and answering
`allow_once` let it run in the background. So the difference is the command
**text** (the classifier's reading of the action), not `background: true`.

Consequence for any e2e that wants a real job: the command must be one the gate
asks about *and* must keep running. `git push origin bridge; sleep 30` is both —
the push fails in this scratch repo and `;` still runs the sleep, so the job is
still alive when the test ends, and no `JobSettled` starts an unprompted turn
mid-test (which is the race that makes such tests pass on Tuesdays).

## A session made with `NewSession` IS opened by the worker

`Registry::create` ends with `self.bell.ring_open(&id)` — *"Wake the worker so it
opens this session now rather than on the first prompt"*. So the tidy theory
that "a second session has a queue and nobody reading it" is false, and it was
written into a comment before it was checked. The real reason `leticode_start`
uses the session a daemon it just spawned opened is simpler: one brief is one
conversation, and two sessions are two places for the answer and the work to
drift apart — which is exactly what the first real run did.

## The other half: `wait` needs to know whether the session ever turned

`wait` answers `0 — the turn ended` after `START_GRACE` (3 s) when nothing is
running. That is right for a session between turns (a fast `send` must not be
reported as a timeout) and a lie about a session that has never run one — the
first real run's `wait` "finished" a brief that had not started. `State::turned`
is set by a snapshot that carries a turn or a non-empty answer row, and by every
`TurnStarted`/`TurnFinished`/`TurnInterrupted`/`TurnFailed`; a session with it
false waits for the whole deadline.
