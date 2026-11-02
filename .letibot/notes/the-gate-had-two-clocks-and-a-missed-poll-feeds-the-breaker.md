<!-- abstract: The queue's halt was `POLL_BOUND` and not the gate's deadline, so a missed poll is retried and `Infra` writes the target back to `gating` - but a timeout still feeds the breaker. -->

# The gate had two clocks, and a poll that missed still feeds the breaker

A poll's 10 s bound used to be the same constant that decided a step's fate, so one miss halted both
queues. Three things changed together. `POLL_BOUND` is 30 s and bounds only the health question
(`crates/harnessd/src/mergequeue/gate_host.rs:20`); a missed poll is retried, because the answer is
`tail -c +<offset>` and nothing is lost; and `GateVerdict::Infra` writes the target back to `gating`
with a 60 s retry window (`UNJUDGED_RETRY_MS`, `mergequeue/serve.rs:47`) - `blocked` is for a tip
nobody can judge and for a step killed at the memory cap, which RAN.

What still ends a step on a saturated host is the breaker. `gate_host::run` ends with
`rpc::after(host, pass, answered)`, and `TimedOut` is `answered: false`, so a timeout feeds the streak:
three consecutive misses inside `BREAKER_WINDOW` open it and the fourth poll comes back `Missed::gone`.
If that is ever too short, the change belongs in `crates/tools/src/rpc.rs`, not in the gate.

A bound in a sentence has to name the thing it bounded: `did not answer within 10 s` did not say *a
poll*. Every timeout sentence on this path carries its own clock.
