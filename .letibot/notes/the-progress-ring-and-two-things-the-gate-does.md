<!-- abstract: There is no JobStarted on the wire (a backgrounded call is the job starting), the gate classifies a command's TEXT so a bare `sleep` is admitted with no card while `git push` asks, and a NewSession session IS opened by the worker. -->

# The progress ring, and two things the gate does that a test writer will get wrong

There is no `JobStarted` on the wire. A backgrounded call IS the start: fold
`ToolFinished { outcome: ToolOutcome::Backgrounded { handle, .. } }` for a start and
`JobSettled { job, state, elapsed_ms }` for the end, where `state` is the exit sentence
(`exited 0`) and not a number. A backgrounded call must not also be filed as an ordinary tool
result. `crates/tui/src/app/events/tools.rs`'s `ToolFinished` arm says all of this.

The gate reads the command's TEXT, not `background: true`: a bare `sleep 30` backgrounded is
admitted at writes-allowed with no card, while `git push origin x; sleep 30` raises one. An
end-to-end test that wants a live job needs both halves - a command the gate asks about and one
that keeps running.

`Registry::create` ends with `bell.ring_open(&id)`, so a `NewSession` session IS opened by the
worker; "a second session nobody reads" is not a failure mode.

`wait` answers `0 - the turn ended` after `START_GRACE` (3 s) when nothing is running, which is
right between turns and a lie about a session that never turned. `State::turned` tells them
apart.
