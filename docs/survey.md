# Agent harness survey - for local GLM-5.3-Flash on llama.cpp

Five codebases read in parallel by subagents on lab2x1 (2x RTX PRO 6000, GLM-5.3-Flash
UD-Q4_K_XL, llama.cpp, 262,144 ctx per slot, a persistent L2 prompt cache on NVMe), with a file
and line from the source behind every claim.

## Why this survey happened

**A coding harness assumes a metered API, and every default that follows from that is wrong
against a local server with a persistent prefix cache.** In opencode v1.18.29: reasoning was
not replayed, so the cached entry held the `<think>` block the next prompt omitted and every
turn diverged from its own cache entry; compaction flattened the conversation into one user
message with no system and no tools, so it cold-prefilled; and the output wall was silent, so a
subagent could return an EMPTY result recorded as success.

Turning replay on moved the re-prefill tail from 78,598 tokens to 5,294 while the median barely
moved: the gain is entirely in the tail, which is what "responsive" means interactively.

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

**Nobody has all of it.** Warm compaction exists only in Grok Build and DeepSeek's harness; a
test that enforces prefix stability exists only in Grok Build. That gap is the strongest
argument for building rather than adopting.

## What each contributes, and what disqualifies it

| codebase | take | reject |
|---|---|---|
| **DeepSeek** (MIT) | reasoning replay as a hard contract; `SystemPromptUpdate: 'in-history'`; warm compaction; spill-don't-truncate | nothing: it is the reference |
| **Grok Build** (Apache-2.0, Rust) | `assert_prefix_stable_pair`; `LengthPolicy`; compaction that keeps reasoning | no `extra_body`, no unknown-field capture, closed `FinishReason` |
| **omp** (MIT) | `append-only-context.ts`; a fixed render; GLM 5.3 in the catalog | cold compaction; hostname-shaped local-mode detection |
| **Pi** (MIT) | `samplingParams` last-write-wins; length fails the batch; append-only by construction | cold compaction; quadratic renderer |
| **opencode** | the plugin seam; the SQLite schema and part union; the tool bodies | metered-API lineage; a v2 surface booted but unwired |

**DeepSeek: reasoning replay is not optional.** Its contract says the `reasoning_content` of
all previous turns must be passed back or the API returns 400 - a **reversal** of the R1-era
rule, and that legacy rule is the origin of the whole bug class. `SystemPromptUpdate:
'in-history'` is the best single idea in the survey: a changed `system` message is **appended
after the cached history** and read at any position, instead of rewriting message 0. Its
compaction replays the last routed request plus a trailing instruction, so the summarizer hits
the warm prefix.

**Grok Build: the test is the discipline.** `assert_prefix_stable_pair` serializes request N
and N+1 and asserts the former is a byte-exact prefix of the latter, across turns, memory
injection, images and restore. Reasoning is a first-class `ConversationItem` sibling whose
ordering is documented as byte-stable across turns, and `LengthPolicy {Fail,
CompleteToolCalls, CompletePartial}` - where an **empty Length response always fails** - is the
named cure for the empty-subagent failure. Adoption is disqualified: no `extra_body`, no
unknown-field capture, a closed `FinishReason` enum, 1.77M lines.

**omp, Pi, opencode: one thing each.** omp's `append-only-context.ts` is the working
implementation of prefix stability (a frozen prefix behind a fingerprint, a push-only log, a
truncation to the first digest divergence); Pi's `samplingParams` is `Object.assign`-ed into the
request body **last**, which is how `return_progress` reaches a server that does not model it;
opencode's `experimental.chat.messages.transform` receives the live message array before
conversion, so a plugin can replace the whole conversation before every model call - though that
surface is being superseded by a v2 stack that is booted but unwired.

## A requirement this survey did not cover

The harness must be **flowy-native**: a server/client relationship, not a terminal-only tool,
with a private chat to an agent able to **answer permission requests**. That makes permission an
asynchronous request/response with a human who may be elsewhere, so the loop must be
headless-first and permission requests must be events on a bus with stable identity - a design
that cannot be retrofitted onto a blocking local prompt.

opencode's `permission.ask` hook is declared with **zero trigger sites repo-wide**, so the one
seam that would route permissions to flowy does not work. ACP (Agent Client Protocol) is the
obvious candidate transport - Grok Build implements it, opencode ships an `acp` subcommand - but
whether ACP carries permission semantics richly enough was not checked.

## Recommendation

Build, but do not start from a blank page. Take the prefix-stability test from Grok Build
first, before any harness code: it is the invariant everything else serves. Then
`SystemPromptUpdate: 'in-history'` from DeepSeek; `append-only-context.ts` from omp as the
reference implementation, capability-detected rather than hostname-detected; Pi's
`samplingParams` body merge; and DeepSeek's spill-don't-truncate for tool results.

And solve, because nobody has: **compaction that reuses the warm prefix** (replay the real
system, real tools, real messages, instruction appended last), **reading non-standard streamed
response fields** so `prompt_progress` can drive a real progress display, and **permissions over
flowy**.

## Open questions

- Whether GLM-5.3-Flash's shipped jinja template renders `reasoning_content` back into a
  `<think>` block. omp relies on it and emits no `preserve_thinking` knob for GLM (that is
  Qwen-only), and the evidence here is GLM-4.7's template, not 5.3's. **Verify directly.**
- Whether ACP carries permission semantics rich enough for the flowy requirement.
- `CHECKPOINT_EXP_FACTOR = 2` in the local llama.cpp patch is a guess; the right factor depends
  on how far back divergences actually land, which has never been measured.
