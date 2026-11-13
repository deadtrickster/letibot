# Cloud providers: OpenAI, GLM, DeepSeek, Grok, Kimi, Anthropic, Gemini

`crates/provider` (`letibot-provider`) is the one crate in the tree with a TLS stack (ureq +
rustls); the engine above it stays a state machine with no runtime under it.

## Use

```
harnessd --provider deepseek [--model MODEL] …
harnessd --provider glm      [--model MODEL] [--thinking] …
harnessd --provider anthropic [--model MODEL] [--thinking] …
harnessd --provider openai   [--model MODEL] [--thinking] [--effort high] …
```

| `--provider` | endpoint | key |
|---|---|---|
| `openai` | `api.openai.com/v1/responses` | `$OPENAI_API_KEY` |
| `deepseek` | `api.deepseek.com` | `$DEEPSEEK_API_KEY` |
| `glm` | `open.bigmodel.cn` | `$ZHIPUAI_API_KEY`, or `ZHIPU_API_KEY`, `ZAI_API_KEY`, `GLM_API_KEY` |
| `glm-coding` | `api.z.ai/api/coding` | the same key |
| `glm-coding-cn` | `open.bigmodel.cn/api/coding` | the same key |
| `grok` | `api.x.ai` | `$XAI_API_KEY`, or `GROK_API_KEY` |
| `anthropic` | `api.anthropic.com/v1/messages` | `$ANTHROPIC_API_KEY` |
| `deepseek-anthropic` | `api.deepseek.com/anthropic` | `$DEEPSEEK_API_KEY` |
| `glm-anthropic` | `api.z.ai/api/anthropic` | the same GLM key |
| `kimi-code` | `api.kimi.com/coding` | `$KIMI_API_KEY`, or `KIMI_CODE_API_KEY` |
| `kimi-code-global` | `api.kimi.ai/coding` | the same key |
| `kimi-coding` | `api.kimi.ai/coding/v1/messages` | the same key |
| `gemini` | `generativelanguage.googleapis.com` | `$GEMINI_API_KEY`, or `GOOGLE_API_KEY` |

Anything else is refused, naming every provider this build knows. `--model` defaults to the
catalogue's pick for that provider, and `--thinking` asks for a reasoning turn - which on GLM and
Kimi is a field of its own, on OpenAI is `reasoning.effort` and on Gemini `thinkingConfig.thinkingLevel`,
both taken from `--effort` when that is set.
The coding plan is a subscription: the metered GLM name on that key answers a 429 that reads like an
empty account, which is why it is a preset of its own.

Kimi's coding plan is the same shape and is not the pay-per-token Open Platform
(`api.moonshot.ai/v1`, `$MOONSHOT_API_KEY`): those keys and URLs are not interchangeable, and one
Kimi key works on one region's host only. Two things are Kimi's rather than OpenAI's: an assistant
message that proposed a tool call must carry `reasoning_content` (this build sends `""` where no
thought was recorded), and K3.1 Pro refuses a request that turns thinking off, so a cheap turn on it
leaves the switch out. Kimi also admits the coding clients it recognises by `User-Agent`, so every
request there names itself `letibot/<version>`; passing for another agent is a documented violation,
and a 403 says which gate refused it. A 429 carrying `exceeded_current_quota_error` is a spent plan
rather than a wait: it is not retried, while the plan's 5-hour and weekly limits are, with their
reset.

## Your own provider

Any endpoint speaking one of the four protocols is a few lines of
`~/.config/letibot/providers.toml`, or two environment variables. `letibot --providers` lists every
door with its protocol, URL, key variable and where it was defined.

Kimi's coding plan over Messages:

```toml
[provider.kimi-messages]
protocol = "anthropic-messages"    # chat-completions | anthropic-messages | responses | genai
url = "https://api.kimi.ai/coding/v1/messages"
key_env = "KIMI_API_KEY"
model = "k3.1-pro"
catalogue = "kimi-code-plan-global"   # models.dev id: window and prices
fixed = ["temperature", "top_p", "n", "presence_penalty", "frequency_penalty"]
```

An OpenAI-compatible proxy (LiteLLM, vLLM, a gateway):

```toml
[provider.myproxy]
protocol = "chat-completions"
url = "http://127.0.0.1:4000/v1/chat/completions"
key_env = "MYPROXY_API_KEY"    # or key = "…"; default $MYPROXY_API_KEY
model = "qwen-coder"
window = 131072                # no catalogue: say it, or the window is unknown
aliases = ["proxy"]
thinking = "thinking"          # the body field that switches reasoning, if any

[prices."qwen-coder"]          # optional; without it the model is unpriced, never free
input = 0.2
cached = 0.02
output = 0.6
```

The env form, as Claude Code reads it:

```sh
ANTHROPIC_BASE_URL=https://api.kimi.ai/coding/ ANTHROPIC_API_KEY=sk-… ANTHROPIC_MODEL=k3.1-pro \
  letibot --provider env-anthropic
OPENAI_BASE_URL=http://127.0.0.1:4000/v1 OPENAI_API_KEY=sk-… OPENAI_MODEL=qwen-coder \
  letibot --provider env-openai     # LETIBOT_OPENAI_PROTOCOL=responses for the Responses wire
```

`ANTHROPIC_AUTH_TOKEN` stands in for `ANTHROPIC_API_KEY` on `env-anthropic` and is also sent as a
bearer token. `/v1/messages`, `/chat/completions` or `/responses` is appended to a base URL that
does not already end in it.

Which wins: an entry named for a built-in (`[provider.deepseek]`) changes that built-in field by
field and leaves the rest. An env door has its own name, `env-anthropic` or `env-openai`, and
without a base URL the variables mean what they always did: the built-in `anthropic` and `openai`
keys. A new entry needs `protocol`, `url` and `model`. An unknown protocol, a key the entry does
not take or a `fixed` word that is not a sampling key refuses the daemon at start, naming the file,
the entry and the key. A door with no catalogue lists its models from the endpoint's own
`GET …/models`, fetched once per daemon, and its window and prices come from `window` and
`[prices."<model>"]` or are shown as unknown.

## Four protocols

A preset's `protocol` is the whole of the choice, and `harness::open::build_provider` is where it is
made. Most doors speak OpenAI chat completions (`openai.rs`, `messages.rs`).

The four `anthropic` doors speak Anthropic Messages (`crates/provider/src/anthropic.rs`): the system
prompt is a top-level field, a tool result is a `tool_result` block in the user turn that answers
it, and the stream is named SSE events.

A Messages provider signs its thinking: the block comes back with a `signature` that must go back
unmodified, so it is kept on the reasoning row (`ReasoningField::Signed`, or `Redacted` for the
encrypted form) and replayed from there. A model that only has the old manual thinking mode is asked
again for it when it refuses `adaptive` by name; a history whose signatures the API will not accept
is retried once with every thinking block left out, which is the API's own documented remedy.

`openai` speaks OpenAI's Responses API (`crates/provider/src/responses.rs`): `POST /v1/responses`,
`input` items, and a reasoning item per thought, whose ciphertext the client keeps in memory rather
than on the row because only the issuing account can read it.

`gemini` speaks Google GenAI (`crates/provider/src/gemini.rs`), described in its own section below.

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

## Gemini: its own protocol, not another endpoint

`crates/provider/src/gemini.rs` is its own client for `generateContent`/`streamGenerateContent`,
and everything it shares with the chat-completions client - the
deadlines, the notes closure, the picture marks, the catalogue, `BackendCaps::METERED_API` - is
shared rather than copied.

| the other doors | Gemini |
|---|---|
| `POST /chat/completions`, model in the body | `POST /v1beta/models/{model}:streamGenerateContent?alt=sse`, model in the PATH |
| `Authorization: Bearer KEY` | `x-goog-api-key: KEY` |
| `messages` of `system`/`user`/`assistant`/`tool` | `contents` of `user`/`model`, `systemInstruction`, `functionDeclarations` |
| `reasoning_content`, echoed or not | `thought: true` parts, returned only when asked; `thinkingConfig` is the switch |
| images as `image_url` | `inlineData` blobs, so only a `data:` reference can be sent |
| `tool_call_id` | `functionCall`/`functionResponse`, matched by `name` and by the model's own `id` |

`--effort` is Gemini's thinking LEVEL (`LOW`/`MEDIUM`/`HIGH`/`XHIGH`, and `xhigh` only on
`gemini-4-hydrogen`); nothing is sent unless the operator asks, because `thinkingConfig` on a model
that does not think is an error rather than a no-op. `--thinking` adds `includeThoughts`.

Three facts worth knowing before reading a refusal:

- **`Schema` is a subset of OpenAPI 3.0.** A tool schema is translated into it, annotation keys
  (`additionalProperties`, `$schema`, `default`) are dropped, and the constructs with no equivalent
  (`$ref`, `oneOf`, `allOf`, `const`, `patternProperties`, a non-string `enum`) are REFUSED before
  the post, naming the parameter and the keyword. A union of types (`type: ["boolean","integer"]`)
  becomes `anyOf`.
- **Thought signatures** ride on parts in this protocol and Gemini 4/3 refuse the turn whose current
  function calls lost them. `TranscriptItem` has no field for one, so the client keeps them itself,
  by call id, in memory: a turn's rounds share a provider and the validation does not reach back
  past the current turn.
- **A block is a refusal, not an empty answer.** `promptFeedback.blockReason`, or a candidate with
  `finishReason: SAFETY`/`RECITATION`/`MALFORMED_FUNCTION_CALL` and no content, arrives as HTTP 200
  and is reported as `BackendError::Refused` carrying that status, so the retry ladder does not take
  the round again and the operator is told which filter fired.

`429 RESOURCE_EXHAUSTED` is the rate limit and reaches `Trouble::RateLimit` through its status; the
`retryDelay` in the error's `RetryInfo` detail (or a `Retry-After` header, which wins) is the instant
a caller may sleep on. `thoughtsTokenCount` is billed as output and `candidatesTokenCount` excludes
it, so the turn's generated count is both.

## When the model you are on is OUT

Four things can be wrong with the provider side, and they are four different facts. Each one is a
typed event, so a program can branch on it without reading English:

| `kind` | what happened | a wait lifts it? |
|---|---|---|
| `rate_limit` | the provider said *not now*: a 429 or a 408 whose body is not about money | yes, and the event carries `reset_ms` when the provider said when |
| `quota` | the account behind the key cannot pay | no |
| `timeout` | the request never answered | maybe: the route may be fine a minute later |
| `stall` | a cloud round produced nothing for fifteen seconds | not by its own: a slow provider and a gone one look the same |

A 429 that names money is a refusal and is asked once, with no backoff: no wait lifts it. A 5xx, a 408
and a 429 that does not name money are weather, and are waited out by the retry ladder. A stall is
raised by the daemon's own clock and only on a cloud round, because a local round is legitimately
silent for half a minute while it reloads a model.

### The event

`ProviderTrouble` is published where the daemon knows the round is over for good, not once per
attempt, so a ladder of five calls against one 429 is one fact. It carries the kind, the model the
round was on, the provider's own words on one line, `reset_ms` when the provider said when, a `count`
of how many of this kind in a row, and `switched_to` when the daemon's own policy moved the session.
A caller that sees `switched_to` absent knows the session is still where it was, which is a different
answer from *moved*.

Where an automation reads it:

- `leticode_events` shows it as a `provider` entry, in order with the calls and turns, carrying the
  kind as its code and the lift instant when there is one; a move the daemon made itself is one more
  entry, `model switched`, naming the model it went to.
- `leticode_status` carries the current `model` and the last one as `last_trouble`, `null` for a
  session whose provider has not failed since the bridge attached.
- `letibot-mcp wait <worktree> <session> --until attention` wakes on it and exits `40` with the event
  itself, so the caller's next move (sleep until `reset_ms`, or move the session) needs no second
  question. The other codes are `0` ended, `10` an ask, `20` failed and `30` the deadline passed.

A person sees the same fact as a warning on the session's log: `model_slow_first_byte` for a stall,
and the retry and recovery lines for a failed round.

A 5xx that is not a quota is the one case with no `kind`: it moves the session all the same, and the
switch itself is the announcement, so a caller reading only `ProviderTrouble` hears nothing about it and
a caller reading `leticode_status.model` sees the move.

### Moving the session

Three doors to the same verb, and all of them take at the **next turn boundary**: a switch that
arrives mid-turn cannot re-issue the round already in flight.

| door | |
|---|---|
| `leticode_model {session, model}` | the MCP verb. Answers with the daemon's own sentence, which says when the switch takes |
| `--models NAME` | the head flag: prints that sentence, exit `0` switched and `1` refused. The name is the whole flag, since a bare `/models` is a listing and not a switch |
| `/models NAME` | the same verb typed in a session |

The name is one `/models` takes: a provider (`deepseek`), `provider/model`, a `[model."…"]` fleet name,
or `local` for this box's own server. A name this box cannot serve is refused by name and nothing
changes.

### The daemon moving it by itself

`[fallback]` says where else this box may go, in the order to try. It is read from `leticode.toml`,
and a project's list replaces the operator's rather than joining it, because a project that names two
models must not inherit a third.

```toml
[fallback]
models = ["deepseek/deepseek-chat", "glm-coding/glm-5.3"]
fallback_after_timeouts = 3
```

| key | default | |
|---|---|---|
| `models` | none | the list, tried in this order. Empty or absent means the turn fails |
| `fallback_after_timeouts` | `3` | how many consecutive model timeouts earn a move. `0` is refused: a move on the first timeout is written `1` |

What moves the session: a refusal by the provider, a spent account, a 5xx, and a run of timeouts long
enough to stop reading as weather. What never does: a transport failure (the model may be fine and the
route to it down), a context-length refusal, and a stall on its own, since a slow round may still
answer. Only a **metered** name is taken automatically: a local server or a fleet model can re-render
the conversation into a fork, which is not something an unbidden switch should do mid-turn.

The refused round is not re-issued, because a model's first request has to follow a user message.
Instead a new turn opens on the model the list names, with the notice as its user item:

```text
[fallback] deepseek/deepseek-chat refused (http 429); continuing this task on glm-coding/glm-5.3.
The turn before this one ended on that refusal and nothing it did was lost ...
```

so the work goes on with its tool results above it. The switch is announced, written to the session
row and shown in the header, as `/models` would have done it, and `/models` moves it back. A name
that could not be taken is said rather than skipped in silence, with the reason and that the next name
is being tried. Each name is tried once, and never the one that just refused the round.

## What crosses the seam

`MessagesBackend` (in `letibot-backend`): the transcript goes over as the API's messages - system,
user text, assistant `content` + `tool_calls`, `tool` results keyed by `tool_call_id` - and tool
schemas as `tools`. Reasoning items are not sent back, which DeepSeek documents as a requirement.
The answer streams as deltas (text, `reasoning_content`, tool-call fragments by index) and ends with
the provider's `usage`.

That is the chat-completions half. On the Responses protocol the same transcript goes over as input
ITEMS (a message, a `function_call`, a `function_call_output`, a `reasoning` item), tool schemas are
flat, and a thought IS sent back: `store: false` means the account keeps nothing, so the reasoning
item's `encrypted_content` is carried on the next request or the model has no memory of why it made
a call. The transcript has no field for an opaque ciphertext, so the client keeps what it issued and
pairs it to a row by the summary text - see `responses.rs`'s module comment.

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

**The overcount is bytes, and the remedy is a carried ratio.** On the byte vocabulary the ledger's
"tokens" are the bytes of the dialect rendering, markup and control tokens included: one real session
read `context_ledger` 2,704,409 against the provider's `context_tokens` 792,991, a ratio of 3.41 at
about 2.5 characters per token. Images are not the cause - fourteen `ToolResult.media` rows carrying
1,995,136 base64 characters cost 228-243 ledger ids each, because the renderer emits
`VISION_START`/`IMAGE_PAD`/`VISION_END` where the part sat and never the `data:` URI. The ledger
cannot be re-counted in place either: its ids are the hash chain and a store trigger pins `tok_len`
to the blob. So `Config::carried_scale` keeps the last measured pair, named after the model that
measured it, until a round on the current model measures again - and a provider switch keeps it
rather than clearing it.

## Under a meter

Compaction is an operator's act (`/compact`); there is no automatic threshold, and a per-provider
prompt budget would be the knob. GLM and DeepSeek are open weights, so
[docs/glm-and-dense-attention.md](glm-and-dense-attention.md) still describes what the cloud model
does with the prompt; the KV is simply not ours to inspect.
