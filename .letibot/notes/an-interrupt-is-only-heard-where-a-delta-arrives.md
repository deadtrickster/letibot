<!-- abstract: Steering is polled at a round's start and once per delta, so a round that comes back `429` delivers no delta and an interrupt sent during a retry ladder is not heard until a round that generates - and a test for it needs a round that streams. -->

# An interrupt is heard where a delta arrives, and a refused round delivers none

`crates/turn/src/engine.rs`'s provider path checks steering in exactly two places: once at the
round's start (`pending.absorb(steering)`, so the stream loop's first poll finds it) and once per
delta inside `on_delta`. A round that comes back `429` delivers no delta, and the round-start absorb
only sees what is queued then - so an interrupt sent while a turn walks the retry ladder
(`sleep_unless_closed`, which watches `Hub::is_closed` and nothing else) is not seen until a round
that actually generates.

To test an interrupt on a failing turn, the round AFTER the refusal must stream: a stub that
announces the whole body in `Content-Length` and then dribbles one SSE line per ~200 ms, so each
landing polls the steer. The script is three acts - a tool call, a refusal, then the slow answer.

`watch()` in that test file returns at its deadline WITHOUT failing, so a turn that walked its whole
ladder left the assertion green for the wrong reason. A green test that took 60 s is the tell: the
wall clock is part of what is being asserted.

The provider-name trap beside it: `Config::provider.name` must be a preset this build knows
(`deepseek`, `glm`, `glm-coding`, `glm-coding-cn`, `grok`), so a stub borrows `glm-coding` and
points it at its own port.
