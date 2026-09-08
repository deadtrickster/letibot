# Agent harness survey — for local GLM-5.3-Flash on llama.cpp

Written 2026-09-08 on lab2x1 (2x RTX PRO 6000, GLM-5.3-Flash UD-Q4_K_XL, llama.cpp,
262,144 ctx per slot, ~677 GB persistent L2 prompt cache on NVMe). Five codebases read
in parallel by subagents; every claim below carries a file and line reference from the
source, not from documentation or marketing.

## Why this survey happened

A full day of debugging opencode produced one finding that reframes everything: **the
harness assumes a metered API, and every default that follows from that is wrong against
a local server with a persistent prefix cache.**

Measured here, in opencode v1.18.29:

- Prior reasoning was not replayed to the model (`interleaved` defaults false,
  `provider/provider.ts:1304`). llama.cpp caches prompt PLUS generated tokens, so the
  cached entry contained the `<think>` block while the next prompt omitted it. Every turn
  diverged from its own cache entry, the strict-prefix supersede test at
  `server-task.cpp:2427` never fired, and entries accumulated to 291 / 612 GB. The server
  was OOM-killed four times in one day.
- Turning replay on took `f_keep` p10 from **0.000 to 0.999** and p99 re-prefill from
  **78,598 tokens to 5,294**. Median barely moved. The gain is entirely in the tail, which
  is what "responsive" means in an interactive loop.
- Compaction flattens the whole conversation into one user message with `system: []` and
  `tools: {}` (`session/compaction.ts:380-445`). It shares no prefix with anything, so it
  cold-prefills: measured 144,436 tokens, cache hit 0, ~13 minutes before the first token
  of summary.
- `OUTPUT_TOKEN_MAX = 32_000` silently capped output (`provider/transform.ts:18`), and
  `finish_reason: "length"` was parsed then discarded (`session/processor.ts`,
  `case "finish": return`). A subagent spent its entire budget thinking, returned an EMPTY
  result, and that was recorded as success. Nine such messages sit in the database.
- The TUI rebuilt the markdown for a reasoning block from the full accumulated text on
  every delta — O(N) per token, quadratic over a long thought.

Separately, two llama.cpp-side findings: spill files were left dirty in the page cache
(no `fadvise`), which is what actually starved the allocator at the OOM — `Dirty` fell
from tens of GiB to 23 MB and `MemAvailable` from 14 GiB to 91 GiB once fixed. And
context checkpoints are ~half of every prompt-cache entry (40.6 KiB/token measured
against 19.25 KiB/token of raw KV), because uniform spacing puts ~24 of them at 145.563
MiB each on a 200k conversation.

## The scorecard

| property | opencode | Pi | omp | Grok Build | dsh |
|---|---|---|---|---|---|
| append-only prompt | no | yes | yes | yes, **tested** | yes |
| reasoning replayed | no | yes | yes, GLM-aware | yes | yes, **mandatory** |
| compaction keeps the prefix | no | no | **no** | yes | yes |
| no hardcoded output wall | no | yes | yes | yes | yes |
| acts on `finish_reason: length` | no | yes | yes | yes | yes |
| render cost independent of length | no | no | yes | yes | not checked |
| arbitrary request body fields | via providerOptions | yes | yes (`extraBody`) | **no** | not checked |
| reads non-standard response fields | no | no | seam only | **no** | not checked |
| byte-exact prefix invariant test | no | no | no | **yes** | not checked |

**Nobody has all of it.** Warm compaction exists only in Grok Build and DeepSeek's
harness. A test that enforces prefix stability exists only in Grok Build. That gap is the
strongest argument for building rather than adopting.

## What each codebase actually contributes

### DeepSeek harness (`deepseek-ai/deepseek-harness`, MIT) — the theory

The most cache-literate codebase read, and the one that explains *why* today's bug
existed everywhere.

**Reasoning replay is not optional.** DeepSeek's documented contract: with `tools` in the
request, "the `reasoning_content` of all previous turns should be passed back … If your
code does not correctly pass back `reasoning_content`, the API will return a 400."
Critically this is a **reversal** — in the R1 era, sending it back was an error. That
legacy rule is the origin of the whole bug class, and of opencode's default.

**The best single idea:** `SystemPromptUpdate: 'in-history'`. The model reads the latest
`system` message at *any position* as the effective system prompt, so a changed prompt is
**appended after the cached history** instead of rewriting message 0. This directly solves
a problem created here today — adding one line to the `<env>` block cost a full cold
re-prefill of a 179k conversation.

Also worth taking: its compaction call replays the last routed request byte-for-byte plus
a trailing instruction, so the summarizer itself hits the warm prefix; oversized tool
results **spill rather than truncate** (head/tail plus `Full result stored at <path>`,
original retained in the log); assistant `content` must be `""` and never `null` (a null
"bricks every later turn"); and every one of ~259 package READMEs carries a mandated
"Token effect / KV Cache effect" section. That last one is a culture, not a feature, and
it is probably the most transferable thing in the survey.

### Grok Build (`xai-org/grok-build`, Apache-2.0, Rust) — the discipline

Written by people who fought this exact problem and left the receipts. "KV-cache prefix"
appears as a design constraint in ~30 non-test sites.

**The thing to steal outright:** `assert_prefix_stable_pair`
(`chat-state/src/actor/tests.rs:4594`) serializes request N and N+1 and asserts the former
is a byte-exact prefix of the latter, across turns, memory injection, images and restore.
That single test would have caught every finding in this document mechanically.

Reasoning is a first-class `ConversationItem` sibling, with the rationale written down
(`conversation.rs:81-83`): the interleaved order "stays byte-stable across turns. That
stability is what lets the server-side prefix KV-cache hit." A prior ordering bug that
"defeated the server-side prefix cache" is recorded at `:4212` — they shipped our bug too.
`LengthPolicy {Fail, CompleteToolCalls, CompletePartial}` where an **empty Length response
always fails** is the named cure for the empty-subagent failure. Compaction keeps
`[System, project instructions, recent verbatim, summary]`; `session_recap.rs:728` —
"Reasoning is kept so the prefix KV cache stays warm."

**Disqualifying for adoption:** no `extra_body` anywhere, so `return_progress`,
`cache_prompt` and `n_keep` cannot be sent at all; no unknown-field capture on stream
deltas, so llama.cpp `timings` is invisible; and `FinishReason` is a closed enum with no
`#[serde(other)]`, so an unexpected value fails chunk deserialization outright. 1.77M
lines, 86 crates, 59 internal `xai-*` deps, telemetry in 115 files and not feature-gated.
Forking is a de-xAI-ification project.

### oh-my-pi / omp (MIT) — the closest working implementation

Solves defect #1 deliberately and by name. `openai-completions.ts:2163-2185`,
`replayReasoningContent`: *"Local llama.cpp-style servers … Qwen3 / DeepSeek-R1 / GLM chat
templates reconstruct the prior assistant turn's `<think>` block from `reasoning_content`;
if we drop the field … the rendered tokens diverge from the slot's existing KV cache, and
llama.cpp falls back to full prompt re-processing (#3528)."* Regression-tested at
`test/issue-3528-repro.test.ts`.

`append-only-context.ts` (374 lines) is the prefix-stability mechanism: `StablePrefix`
freezes system prompt + normalized tools behind a fingerprint, `AppendOnlyLog` is push-only
except `replaceTail`/`truncate`, and `syncMessages` truncates to the first digest
divergence then appends. Auto-enables for llama.cpp and RFC1918 hosts. The TUI render is
genuinely fixed — frozen stable prefix, re-lexing confined to the tail past the last block
boundary. GLM 5.3 is first-class in the catalog (`glm5` tokenizer at revision >= 5,
`revGte("5.2")` branches, a taxonomy entry with an explicit `umans-glm-5.3-flash-lab`
override), so no new dialect would be needed.

**But its compaction is cold**, and this reverses an earlier reading. The warm-prefix
language at `compaction.ts:1030` belongs to the *handoff* path (`:1095-1129`). The default
summarizer at `:988-990` builds `{systemPrompt: [SUMMARIZATION_SYSTEM_PROMPT], messages:
[one synthetic user message]}` — no real system, no tools, no real message list. Same
shape as opencode.

Three further silent-failure surfaces: local-mode detection is **hostname-shaped**, so
llama.cpp behind a proxy or a non-RFC1918 address turns both `replayReasoningContent` and
append-only mode off with no warning — the symptom is the 612 GB cache growth. There is
**no byte-exact prefix invariant test**; the digest is a 32-bit hash over the *normalized
message*, so any divergence introduced downstream in the encoder is invisible. And
`isOpenAICompletionsProgressChunk` does not count a llama.cpp progress chunk as progress,
so a long prefill races the 300 s idle timeout.

Practical risk is churn, not neglect: 597 MB, ~410 contributors, ~4,000 commits in 30
days.

### Pi (`earendil-works/pi`, MIT) — the clean seams

Append-only by construction (`agent-loop.ts` only pushes; `harness/messages.ts:160-162`
passes assistant messages through untouched). Reasoning round-trips **through the field
the server used** — `thinkingSignature = foundReasoningField`, with llama.cpp named in the
comment. `stopReason: "length"` fails the whole tool batch and tells the model why. And
`samplingParams` is `Object.assign`-ed into the body **last**, documented for "llama.cpp,
vLLM, SGLang… parameters pi does not model" — `return_progress` goes straight through.

Same two gaps as the others: compaction flattens (and sets `cacheRetention: "none"`
deliberately), and the renderer is quadratic — `contentContainer.clear()` then rebuilds
from the full accumulated text every delta, defeating its own cache.

The "smallest, clearest harness" framing is stale: 177,925 non-test LOC across 11
packages. What is small is the system prompt and the four-tool default set.

### opencode (`anomalyco/opencode`, v1.18.29) — what is worth keeping

The lineage is uniformly metered-API: Kilo Code is the only genuine fork and reproduces
all four defects verbatim; `oh-my-openagent` is a plugin and structurally cannot reach the
files where they live (and is Sustainable Use licensed — do not borrow code from it).

A plugin layer is more capable than first assumed. `experimental.chat.messages.transform`
(`prompt.ts:1255`) receives the live message array before conversion at `:1262`, so a
plugin can replace the entire conversation before every model call.
`provider.ts:1794` lifts `options["fetch"]` as the transport; `:1838-1856` dynamic-imports
a `file://` package as a whole custom provider. With `compaction.auto: false` a plugin can
own retention too. It cannot own the loop, compaction selection, built-in tool removal, or
the middleware array.

**But the extension surface is being superseded.** `AISDK`/`Catalog`/`AgentV2` are booted
but unwired (`core/src/location-services.ts:42-70`) and the live config path runs a
449-line v2 to v1 **downgrade**. The v2 `aisdk.language` hook would substitute the language
model outright — exactly the seam a local-first harness wants — and it is inert.

Worth vendoring regardless of path: the SQLite schema and the part union
(text/reasoning/tool/step-start/step-finish/compaction/patch), the tool bodies, the
permission model, and `packages/llm`.

## Requirements this survey did not cover

The harness must be **flowy-native**: a server/client relationship, not a terminal-only
tool, with a private chat to an agent able to **answer permission requests**. That makes
permission an asynchronous request/response with a human who may be elsewhere, which means
the loop must be headless-first and permission requests must be events on a bus with
stable identity — a design that cannot be retrofitted onto a blocking local prompt.

Bearing directly on the plugin path: opencode's `permission.ask` hook is declared at
`plugin/index.ts:261` with **zero trigger sites repo-wide**. It is a dead hook. The one
seam that would route permissions to flowy does not work.

ACP (Agent Client Protocol) is the obvious candidate transport — Grok Build implements it
against crates.io `agent-client-protocol 0.10.4` and opencode ships an `acp` subcommand —
but whether ACP carries permission semantics richly enough was not checked.

## Recommendation

Build, but do not start from a blank page. Take:

1. **The prefix-stability test from Grok Build**, first, before any harness code. It is the
   invariant everything else serves, and it is the only mechanical defence against
   silently losing it again.
2. **`SystemPromptUpdate: 'in-history'` from DeepSeek** — append a changed system prompt
   late rather than rewriting message 0.
3. **`append-only-context.ts` from omp** as the reference implementation of the mechanism
   (374 lines), but capability-detected rather than hostname-detected.
4. **Pi's `samplingParams` last-write-wins body merge** — three lines, and it is how
   `return_progress` reaches the server.
5. **DeepSeek's spill-don't-truncate** for tool results, and its "KV Cache effect" README
   discipline.

And solve, because nobody has: **compaction that reuses the warm prefix** (replay the real
system, real tools, real messages, instruction appended last), **reading non-standard
streamed response fields** so `prompt_progress` can drive a real progress display, and
**permissions over flowy**.

## Open questions

- Whether GLM-5.3-Flash's shipped jinja template renders `reasoning_content` back into a
  `<think>` block. omp relies on it and does not emit a `preserve_thinking` knob for GLM
  (that is Qwen-only); llama.cpp probes template capability at `common/jinja/caps.cpp:466-506`.
  The evidence here is `GLM-4.7-Flash.jinja:52`, not the 5.3 template. **Verify directly.**
- Whether ACP carries permission semantics rich enough for the flowy requirement.
- `CHECKPOINT_EXP_FACTOR = 2` in the local llama.cpp patch is a guess; the right factor
  depends on how far back divergences actually land, which has never been measured.
- The prompt-cache eviction ladder (degrade before evict, least-degraded victim first) is
  drafted and compiles but its tests have never been run — pytest is not installed here.
