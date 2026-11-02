# Cloud providers: GLM, DeepSeek, Grok

`crates/provider` (`letibot-provider`) is the one crate in the tree with a TLS stack (ureq +
rustls); the engine above it stays a state machine with no runtime under it.

## Use

```
harnessd --provider deepseek [--model MODEL] …
harnessd --provider glm      [--model MODEL] [--thinking] …
```

| `--provider` | endpoint | key |
|---|---|---|
| `deepseek` | `api.deepseek.com` | `$DEEPSEEK_API_KEY` |
| `glm` | `open.bigmodel.cn` | `$ZHIPUAI_API_KEY`, or `ZHIPU_API_KEY`, `ZAI_API_KEY`, `GLM_API_KEY` |
| `glm-coding` | `api.z.ai/api/coding` | the same key |
| `glm-coding-cn` | `open.bigmodel.cn/api/coding` | the same key |
| `grok` | `api.x.ai` | `$XAI_API_KEY`, or `GROK_API_KEY` |

Anything else is refused, naming all five. `--model` defaults to the catalogue's pick for that
provider, and `--thinking` asks for a reasoning turn, which only the GLM presets have a field for.
The coding plan is a subscription: the metered GLM name on that key answers a 429 that reads like an
empty account, which is why it is a preset of its own.

The key comes from the environment above, `--api-key` for a one-off, or
`~/.config/letibot/providers.toml`:

```toml
[deepseek]
key = "sk-…"
url = "https://api.deepseek.com/chat/completions"   # optional: a proxy, a compatible server

[prices."deepseek-chat"]     # USD per million tokens
input = 0.27
cached = 0.07
output = 1.10
```

A missing key refuses at open, naming the variable and the file. A price comes from this table first,
then from the model catalogue; the operator's number wins where they wrote one, because it may be a
contract price. A model neither prices is **unpriced**, not free: the turn reports no cost, not `$0`.

## When the model you are on is OUT

A 429 that names money - a quota, a credit balance - is not weather: no backoff lifts it, so the
retry policy (`crates/http/src/retry.rs`) classes it as a refusal and asks once. A 5xx, a 408 and a
429 that does not name money are weather, and are waited out. `[fallback]` says where else this box
may go, in the order to try:

```toml
[fallback]
models = ["deepseek/deepseek-flash", "dense78"]
```

Names are the ones `/models` takes (`provider/model`, a `[model."…"]` fleet name, `local`), resolved
through the same door the verb uses, so a name here cannot be one `/models` would refuse.

The refused round is not re-issued: a model's first request has to follow a user message. Instead a
new turn opens on the model the list names, with the refusal as its user item - `[fallback]
deepseek-chat refused (http 429); continuing this task on …` - so the work goes on with its tool
results above it. The switch is announced, written to the session row and shown in the header, as
`/models` would have done it; `/models` moves it back.

The list is the whole policy: empty or absent means the turn fails. Only a **metered** name is taken
automatically (a local server or a fleet model can re-render the conversation into a fork, which an
unbidden switch should not do mid-turn), and a **transport** failure never moves it: the model may
be fine and the route to it down.

## What crosses the seam

`MessagesBackend` (in `letibot-backend`): the transcript goes over as the API's messages - system,
user text, assistant `content` + `tool_calls`, `tool` results keyed by `tool_call_id` - and tool
schemas as `tools`. Reasoning items are not sent back, which DeepSeek documents as a requirement.
The answer streams as deltas (text, `reasoning_content`, tool-call fragments by index) and ends with
the provider's `usage`.

`TurnEngine::run_turn_messages` drives it and changes nothing above: the events a head draws, the
length verdicts, the salvage budget, boundary steering and the `TurnOk` are the local engine's. The
token ledger stays the record - every item appended through `append_items`, tokenised with the
session's own vocabulary - so the store, the hash chain, resume and the compaction arithmetic are
unchanged. That vocabulary is an encoding for the record and is never sent, which is why `--vocab` is
still required; on the byte vocabulary (`Vocab::bytes`, one id per byte) the ledger counts bytes where
the provider counts tokens.

What is different, and disclosed at open as `provider`: the session is METERED; the cache figures are
the provider's coarse ones (`prompt_cache_hit_tokens` on DeepSeek,
`prompt_tokens_details.cached_tokens` on the OpenAI shape); the structural prefix check is `Skipped`
with the backend's own reason, never counted as a pass; and there is no `f_keep`, so the footer
gives tokens and cost instead.

## Under a meter

Compaction is an operator's act (`/compact`); there is no automatic threshold, and a per-provider
prompt budget would be the knob. GLM and DeepSeek are open weights, so
[docs/glm-and-dense-attention.md](glm-and-dense-attention.md) still describes what the cloud model
does with the prompt; the KV is simply not ours to inspect.
