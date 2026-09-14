# Cloud providers: GLM, DeepSeek, Grok

D10's mode 3, built 2026-09-14 on the seam D10 reserved. `crates/provider`
(`letibot-provider`) is the one crate in the tree with a TLS stack (ureq +
rustls); the engine stays a state machine with no runtime under it.

## Use

```
harnessd --provider deepseek [--model deepseek-chat] …
harnessd --provider glm      [--model glm-4.6] [--thinking] …
harnessd --provider grok     [--model grok-4-fast] …
```

The key: `$DEEPSEEK_API_KEY` / `$ZHIPUAI_API_KEY` (`ZHIPU_API_KEY`,
`ZAI_API_KEY`, `GLM_API_KEY` also read) / `$XAI_API_KEY` (`GROK_API_KEY`),
`--api-key` for a one-off, or `~/.config/letibot/providers.toml`:

```toml
[deepseek]
key = "sk-…"
url = "https://api.deepseek.com/chat/completions"   # optional: a proxy, a compatible server

[prices."deepseek-chat"]     # USD per million tokens; without this a turn is UNPRICED, not free
input = 0.27
cached = 0.07
output = 1.10
```

A missing key refuses at open, naming the variable and the file — not three
seconds into the first turn as a 401 that names neither.

## What crosses the seam

`MessagesBackend` (in `letibot-backend`): the **transcript** goes over as the
API's messages — system, user text, assistant `content` + `tool_calls`, `tool`
results keyed by `tool_call_id` — and tool schemas as `tools`. Reasoning items
are not sent back (DeepSeek documents that they must not be). The answer
streams as deltas — text, `reasoning_content`, tool-call fragments by index —
and ends with the provider's `usage`.

`TurnEngine::run_turn_messages` drives it and keeps everything else the same:
the events a head draws, §5.7's length verdicts, the salvage budget, steering
at the boundary (an urgent one closes the connection; nothing is committed),
the `TurnOk` the harness loops on. The **token ledger stays the record**: every
produced item is appended through `append_items`, tokenised with the session's
own vocabulary, so the store, the hash chain, resume and the compaction
arithmetic work unchanged. That vocabulary is an encoding for the record and is
never sent — which is why `--vocab` is still required under a provider.

What is honestly different, and disclosed at open as `provider`: METERED; the
cache figures are the provider's coarse ones (`prompt_cache_hit_tokens` on
DeepSeek, `prompt_tokens_details.cached_tokens` on the OpenAI shape); the
structural prefix check is `Skipped` with the backend's own reason (a skip is
said, never counted as a pass); `f_keep` does not exist, so the footer says
tokens and cost instead.

## Verified

- Unit: message conversion, tool-schema wrapping (GLM's bare shape and the
  OpenAI one), delta accumulation, usage → cost, the price table, key
  resolution order.
- `tests/fake_provider.rs`: the request on the wire (bearer, model, stream
  options, max_tokens, messages, tools), a streamed reasoning/text/tool-call
  answer, a 401 carrying the provider's own message, an abort mid-stream.
- End to end through the real harness against a fake DeepSeek: round 1 the
  model called `read`, the harness ran it, round 2 carried the `tool` result,
  the answer came back; footer `320 prompt tokens (120 cached), 35 out, cost
  $0.000101`.
- **Not verified live**: no provider key exists on lab2x1. The first live run
  is the operator's, with a key; the fake speaks the documented wire shape.

## Token saving under a meter — the operator's notes, and where each lands

- **Compaction should be more eager under a meter.** Today compaction is an
  operator's act (`/compact`); there is no automatic threshold. The knob to add
  is a per-provider budget — compact when the prompt passes N tokens — and the
  place is the harness, not the provider crate. Open.
- **The providers' own caches reward a stable prefix.** DeepSeek caches
  prefixes automatically and reports hits; the harness already never rewrites
  message 0 (`StablePrefix`), and the `cached` figure in the footer is how the
  operator sees the prefix holding.
- **GLM and DeepSeek are open weights**, so the selective-attention knowledge
  in `docs/glm-and-dense-attention.md` still describes what the cloud model
  does with the prompt; what changes is only that the KV is not ours to
  inspect.
