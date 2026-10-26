<!-- abstract: MEASURED 2026-10-26 (package M2): an urgent steering message — the operator's interrupt — is polled only inside `on_delta`, so a round that is REFUSED delivers no delta and an interrupt queued during a 429 ladder is never heard: the ladder walks all six waits (63 s) and the turn ends exactly as it would have. To interrupt a failing turn the round after the refusal must STREAM; and the head's `model_switch_pending` path had the same shape, a 60 s `watch` hiding a turn nobody stopped. -->

# An interrupt is heard where a delta arrives, and a refused round delivers none

MEASURED 2026-10-26, branch `switch-at-turn-end` (package M2), writing
`crates/harnessd/tests/model_switch_at_turn_end.rs` — the scenario the brief asks for as
*"an interrupted turn applies the pending switch"*.

## The measurement

`crates/turn/src/engine.rs`'s provider path (`run_turn_messages_with_tail`) checks steering in
exactly two places: once at the round's start (`pending.absorb(steering)` → `hold_urgent`, so the
stream loop's first poll finds it) and **once per delta** inside `on_delta`
(*"§5.8's escape hatch, checked once per delta"*). A round that comes back `429` delivers no
delta, and the round-start absorb only sees what is queued *then*.

So an interrupt sent while a turn is cycling the retry ladder
(`http_retry_after`: waits of 1, 2, 4, 8, 16, 32 s — 63 s of `sleep_unless_closed`, which watches
`Hub::is_closed` and nothing else) is not seen until a round that actually generates. First run
of the scenario: `http_retries = 6`, an interrupt after the first 429, and the test passed **75 s**
later — one `watch` silently ran its whole 60 s deadline, the turn walked the entire ladder, and
the pending switch landed at the end anyway, so the assertion was green for the wrong reason.

`watch()` in that file returns at its deadline **without failing** (inherited from the file it was
rewritten from, whose follow-up assertions caught the miss). A green test that took 60 s is the
tell: check the wall clock, not only the assertions.

## What makes an interrupt reachable in a test

A stub that announces the whole body in `Content-Length` and then **dribbles it out one SSE line
per ~200 ms** — a streaming reader takes each line as it lands, and each landing polls the steer.
The request must also not be a refusal: hence a three-act script
(`Act::ToolCall, Act::Refuse, Act::Slow`), tool round, 429, then the slow answer.

## The provider-name trap beside it

`Config::provider.name` must be a preset this build knows (`deepseek`, `glm`, `glm-coding`,
`glm-coding-cn`, `grok`) — a `[slow]` block in a test `providers.toml` is refused at open with
*"`slow` is not a provider this build knows; there are five"*. So a third stub borrows the name
`glm-coding` and points it at its own port.

## And the rule this was written to prove

`Harness::set_provider` now defers a switch while `prompt_in_flight` (recorded, said, applied by
`end_turn_clock`), and the test proves the new model's first request ends at a **user** message
with no assistant tool-call round after it — the shape deepseek's thinking mode refused with
`http 400: The reasoning_content in the thinking mode must be passed back to the API`. See
`crates/harnessd/tests/model_switch_at_turn_end.rs`'s header for the operator's screen.
