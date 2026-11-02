# Implementation plan - a local-first agent harness for lab2x1

The design contract for letibot: a long-lived session daemon (`harnessd`) running agent loops against the
operator's llama.cpp fork, with attachable heads, a prompt the harness renders and tokenizes itself,
adjudication as a first-class subsystem, and a ported conformance suite as milestone zero.

Later decisions are in `DECISIONS.md`, cited by D-number. The harness half is built (`crates/`); the fused
server track (§3.3, §17 S0-S7) is not, and the harness runs on the `/completion` token-array fallback
(§5.6). Two operator directives govern everything: *"I want harness and server fused to the 11"* and
*"like Apple ecosystem basically, I don't care about other providers or generalizations."*

Section numbers are the marks `TODO.md`, the notes and the code cite (`§5.7`, `§11.4`, `§18.1-I1`), so
every one is kept. Read four things: §3.1, §4.3, §7.2, §17's last row.

## 0. The thesis

> *"my goal is to have my llm runtime behave like a normal data app, don't see any reasons it
> can't."* - the operator

Every failure that motivated the design is something data systems settled decades ago: an index that never
merges duplicates (§3.10-A), a buffer pool with a page limit, leveled rather than stop-the-world
compaction (§10), an error return that is not swallowed (§5.7, §8.2), admission control and `EXPLAIN`
(§3.4.4, §6). The vocabulary, used because the operator is a Postgres person: the KV cache is a **buffer
pool** (`f_keep`/`f_sim` are a hit-ratio pair, `mlock` is pinning), the sequence registry is a **page
table**, sessions are **tenants** ("seats are EARNED" is admission control), the prompt prefix is a
**radix key**, and durability is a **storage hierarchy** (VRAM, RAM cache, NVMe L2, cold re-prefill).

| § | statement |
|---|---|
| **0.1** | A buffer-pool page is relocatable and a KV cell is not, since its contents depend on its position through RoPE - so reshaping history always pays a `REFRESH` to the end. |

## 1. What this is, and what it is not

A headless session daemon running agent loops against the local llama.cpp router, with attachable heads
(TUI, flowy, ACP) and permissions answered asynchronously by a human who may be elsewhere.

It is **not** a RAG system - retrieval stays behind the `oracle` MCP tools (`ask_corpus`, `ask_code`,
`search_corpus`), with no embedding or reranking (§9.4, §19.3) - and **not** a multi-provider client: the
provider-abstraction *category* is deleted, because an abstraction that exists only for a provider we do
not run is a liability, and it is why the surveyed harnesses default to metered APIs. A fourth model
arrives by someone writing a renderer (§7.4).

D10 reserved the seam a metered API needs in `crates/backend`: `TurnRequest` carries the transcript rather
than a rendered prompt, `BackendCaps` states what a backend cannot guarantee, and `PrefixGuarantee` marks
the one invariant (§4.3) that does not survive a `messages` API - a backend must declare what it cannot
guarantee, and the invariant suites skip loudly rather than pass vacuously. Both sides are ours and fused:
the llama.cpp here is the operator's fork (`glm-all`), carrying a scheduler, an elastic KV pool, earned
seats, an L2 disk tier and a five-rung pressure ladder, so the boundary is chosen for engineering reasons
rather than inherited from HTTP.

## 2. The facts this design is built on

| § | statement |
|---|---|
| **F1** | The prompt is append-only, or the cache is gone: llama.cpp caches prompt *and generated* tokens, so request N+1 must extend request N including turn N's generation (replay off: `f_keep` p10 0.000, p99 re-prefill 78,598 tokens; on: 0.999 and 5,294). |
| **F1a** | Append-only is also the cache's garbage collector: the cache drops any cached prompt fully contained in the current one, so a conversation occupies one entry for its whole life. |
| **F2** | Depth is paid in memory and prefill, never in decode (35.7 t/s below 10k of context, 36.9 above 120k), so a deep *warm* conversation is cheaper per turn than a shallow *cold* one. |
| **F3** | Context occupation defocuses - a big window is higher tolerance, not immunity - so minimising occupation is a quality measure; F2 and F3 oppose each other and §9 resolves them. |
| **F4** | Close the loop; never paper over it with prompt. *Prompts are soft limits, hooks are hard stops*, since a prompt rule is enforced by the component whose failure it is meant to catch. |
| **F5** | The failure shape to hunt is "the system did less than it claimed and said nothing", so every layer boundary must make *"I did not do this"* structurally different from *"here is the result."* |
| **F6** | The server is a preemptive scheduler and this harness is its only client, so every seat measurement here is synthetic until a client asks for concurrency (§17-M6). |
| **F7** | Statistics decide policy, so statistics must be right: a wrong number is worse than a missing one because it produces a confident wrong plan, which is why §6 and §18 exist. |

## 3. Architecture

| § | statement |
|---|---|
| **3.1** | The harness renders the chat template itself, tokenizes it itself and sends token ids - so with an append-only transcript the token array is append-only too, one `Vec<llama_token>` per transcript with nothing ever re-rendered, and a prefix violation is not expressible rather than merely tested for. |
| **3.2** | Two processes, one system: `harnessd` holds the store, ledger, renderers, tokenizer, turn engine, `EXPLAIN`, tools and adjudication; `llama-server` holds the scheduler, pool, seats, sequence registry, prompt cache and ladder. One release, one version number - not two systems with an API between them, and not one binary. |
| **3.3** | The seam is a private length-prefixed versioned protocol over a Unix socket where both sides refuse a version that is not exactly theirs (they ship together), plus one `memfd` region per transcript mapped read-only by the server, so a request is `{region_id, start, len, prefix_handle}`. One binary was rejected because every harness change would force a C++ link and a 29-31 s reload. Control vocabulary: `Admit`, `Prewarm`, `Submit`, `Spans`, `SegmentHint`, `Handle`, `Explain`, `Stats`; HTTP keeps `/props`, `/apply-template`, `/metrics`, `/slots`, `/health` for the black-box suite (§14.3). |
| **3.4** | Six unlocks, all server-track (§17) and none landed: **3.4.1** checkpoint placement stops being a heuristic, since the harness emitted the tokens the server recovers by string-matching delimiters; **3.4.2** eviction becomes semantic, since only the harness knows a segment is a finished subagent; **3.4.3** one truth for accounting; **3.4.4** admission control, `Admit` returning a real number to spawn subagents from; **3.4.5** `Prewarm`; **3.4.6** `Explain` returning the matched entry, `lcp`, `f_keep`, `f_sim`, the rung and the shared batch iterations. |
| **3.5** | Fusion couples the harness to llama.cpp's internals, and the exit is three named modules - the tokenizer, the submit path (a token array over HTTP, which vLLM and SGLang both accept) and the fused vocabulary, which has no equivalent anywhere, so losing it means being a normal client again. |
| **3.6** | Everything but the protocol version, the shm submit path and §3.4's six unlocks iterates in the daemon alone in seconds; those are kept cheap by additive lockstep versioning, the `/completion` fallback so a server rebuild never blocks (this tree runs on it), and builds in a niced firecode run. |
| **3.7** | Chat is a channel of the loop: > **[operator]** *"I don't want to fiddle with monitors, nor timers which pollute the context."* The poll loop is a timer in disguise whose bookkeeping costs context, so inbound messages are pushed and never polled, a chat message costs exactly what a user message costs because it is one (§18.1-I11), chat is the primitive and adjudication a usage of it, and a flowy chat is a head (§13.4). |
| **3.8** | The daemon's session log and token array are authoritative; the KV cache is only an accelerator, and every request must be correct with an empty cache - which `--cache-reuse` violates by restoring recurrent state computed with deleted text present. No head's view is authoritative. |
| **3.9** | The substrate is firecode: a run is one VM, its lifetime a cgroup subtree, the guest untrusted by construction. Effectful tools run inside a boundary (`Bwrap`, `Seatbelt`, firecode), with **harnessd on the host and tools in the run** because the log, ledger and audit must survive the VM. A checkpoint restore must append a `NotRun`-style marker saying the workspace was rolled back, or the transcript lies. |
| **3.10** | **A.** sharing is real but the pool is a budget and GLM's 436.7 MiB per sequence is not token-proportional, so **two ladders at two tiers, never merged**: the cache ladder degrades whole entries while a shared-block index needs refcount plus LRU at the resident-pool tier. **B.** composing blocks out of order is blocked by physics, so **"send blocks A, F, Q plus new text" cannot work**, and a handle's value is a verifiable hit rather than a discovered one. **C.** position shifting is unsafe on GLM's recurrent components, stitching is unmeasured for MLA + DSA (UNVERIFIED-16), and paying - linear in the suffix, ~11 s at the tail and ~453 s at the head of 200k - is what v1 does (§19.3). |

## 4. Data model

| § | statement |
|---|---|
| **4.1** | **Layer 1, the Transcript**: the items that become the prompt, immutable with one mutation, `append`; truncation is a new transcript with a new id (§5.5), because it is a cache divergence. **Layer 2, the Journal**: everything else, freely appended and never consulted when building a request. Enforced by types - the request builder takes a `&Transcript` and nothing else. |
| **4.2** | The item type, with reasoning a **sibling** of assistant because the interleaved order must stay byte-stable across turns, which is what lets the cache hit: |

```
TranscriptItem =
  | System { text, origin: Bootstrap | Update }   | User { parts: [Text | Image | FileRef] }
  | Reasoning { text, field }                     | Assistant { text, tool_calls: [ToolCall] }
  | ToolResult { call_id, name, outcome, payload }| SegmentMark { segment_id, label, kind, edge }
```

`ToolOutcome = Ok | Abstained | Failed | Denied | Timeout | NotRun` is closed and `Abstained` is not a
flavour of `Ok` (§8.2); `SegmentMark` renders to nothing and exists only for §19's futures.

| § | statement |
|---|---|
| **4.3** | The ledger, which is what makes append-only structural: |

```
TokenLedger = [ (item_id, tok_offset, tok_len, h_k) ]
   h_k = H(h_{k-1} ‖ tokens(item_k)),   h_{-1} = H(tokens(stable_prefix))
   tokens : Vec<llama_token>, memfd-backed, append-only
```

**There is no rewrite operation to call**, so a prefix violation is not expressible; the unit is the
server's unit; the hasher is fed forward only and `h_k` is the cache identity; a periodic full re-hash
catches renderer non-determinism; and the ledger is persisted so a restart replays spans. It supersedes a
32-bit rolling hash over the message object, which cannot see divergence introduced downstream.

| § | statement |
|---|---|
| **4.4** | Persisted: `session`, `transcript`, `transcript_item`, `stable_prefix` (content-addressed, shared across sessions), `event`, `adjudication_request`/`_decision` (never deleted), `turn_metrics`, `tool_spill`, `segment`. Derived: the head snapshot, rendered markdown, every §6/§18 aggregate, the request body. Not stored: the raw response envelope, which invites a replay that diverges. |
| **4.5** | One event per thing a head must see, all with `(session_id, seq, ts)`: turn start/finish/interrupt, `PromptProgress`, `Delta`, tool proposed/started/progress/finished, decision requested/answered, `TranscriptAppended`, head attached/detached, `Warning` (§18 lands here), `Explain`. `Delta` carries **only the increment**. |

## 5. The prompt pipeline

| § | statement |
|---|---|
| **5.1** | Prefix → items → **render** (ours, control tokens verbatim, no jinja) → **tokenize** (appended, nothing earlier re-rendered) → ledger and hash chain → **submit** → progress, deltas, tool calls from our parser, usage → post-flight assertions → append. Generated tokens are appended as tokens, so I1 holds by construction, and parsing is ours too. |
| **5.2** | The prefix is system text plus normalized tool schemas, frozen behind a fingerprint and content-addressed. **A changed prefix is a cold start** - correct, but nothing volatile goes in it (one `<env>` line once cost a full re-prefill of 179k) and everything volatile arrives as an appended item. A prompt may describe how to use a tool but never what the data contains; state the answer language. |
| **5.3** | System changes are appended (`in-history`): compare against **the last non-empty system node**, equal emits nothing, different appends at the tail, otherwise fall back explicitly and normalize. The default per model is the user-turn envelope (`<system-update seq=N>`); both forms are one field, `system_update_mode`. |
| **5.4** | Reasoning is not replayed, it is simply still there - no field to send back, no field detection, no probe, no template that can drop it. Two obligations replace them: the renderer must place thinking blocks as the model was trained to see them, and an assistant turn with no visible text must still emit its turn-boundary tokens (`content: ""`, never `null`). |
| **5.5** | Fork, not truncate: anything that would shorten history creates a new transcript with `parent_transcript_id`, so a rewind is reversible and the divergence has an id. The cost is a re-prefill from the divergence to the end, `(n_total - n_common) / 441 t/s` here. |
| **5.6** | `Submit` carries the shm region, `message_spans` and `segments` given rather than string-matched, the `prefix_handle` assertion and the `want` set. The fallback is `POST /completion` with a token array - what this tree runs on - setting `cache_prompt`, `return_progress`, `timings_per_token`, `return_tokens`, `message_delimiters`, and no `n_predict` cap. |
| **5.7** | No output cap, and `finish_reason: length` is acted on: |

```
on finish_reason == "length":
  if the turn produced NO visible content and NO tool calls   → HARD FAIL the turn.
  if the turn produced ONLY reasoning                         → HARD FAIL the turn.
  if there are tool calls:
      all arguments parse as complete JSON → keep them, continue
      any argument truncated              → FAIL THE WHOLE BATCH, none executed
  otherwise (real text)                                       → keep, mark truncated, continue
```

The fail cases are `NoVisibleContent` and `ReasoningOnly`; a failed batch says why **in the tool result,
not a log**; retries are bounded; a truncated turn is never reported as completed.

| § | statement |
|---|---|
| **5.8** | Steering injects **at the next step boundary**, because queueing to the end wastes a generation the message was correcting and interrupting strands a partial turn that must be kept for cache reasons anyway. `urgent` (`ABORT`) interrupts at the next token, keeping the partial as `truncated: true`; nothing already in the vector is rewritten. |

## 6. `EXPLAIN` - per-stage observability as a feature, not as logging

| § | statement |
|---|---|
| **6.1** | Three of oracle's eight diagnoses in one evening were wrong for one reason - no way to measure a stage alone - so "why did this prefill 144,436 tokens with zero cache hits?" should cost one command, not an evening of log archaeology. |
| **6.2** | Every turn records a plan; the cold-turn rendering is the deliverable - which prefix matched, where it diverged, what caused it, what it cost: |

```
Cache            prompt_tokens 144,436   cached_tokens 0   f_keep 0.000
  divergence     at item 3 (System, origin=Update), cause: stable prefix changed, field <env>
  cost           144,436 tok re-prefill ≈ 327 s at 441 t/s
```

`ExplainPlan` has no field list yet, so the event carries the plan untyped - deliberate, since a field list
invented before the counters exist would be guesses.

| § | statement |
|---|---|
| **6.3** | The numbers come from `usage.prompt_tokens_details.cached_tokens`, `prompt_progress`, `timings`, `/metrics`, `/slots`, `Explain{turn_id}` (or the `-lv 4` log until S5b) and the ledger for the expected prefix - and that last one is the diagnosis, since only the harness knows what *should* have matched. |
| **6.4** | Every stage gets its own counters: prompt construction, cache, prefill and decode **separately and with their concurrency**, tools per call, permissions, per-head render. |

## 7. Per-model dialects - renderers we own

| § | statement |
|---|---|
| **7.1** | A dialect becomes **a renderer** rather than compat flags, with five jobs - `render`, `render_incremental`, `parse`, `control_tokens`, `stop_tokens` - plus `system_update_mode`, `guards` and `template_sha`. Two invariants are the tests: **`render_incremental` agrees with `render`** (the append-only design rests on it, and being pure it is property-testable) and **`parse ∘ render` is the identity** on the round-trippable parts. |
| **7.2** | Validation against the shipped jinja is a required CI check and the mitigation for the one hazard self-rendering introduces - a silent divergence from what the model was trained on. `POST /apply-template` touches no slot and no GPU: |

```
for each dialect D, for each fixture F in the corpus:
    assert detokenize(D.render(F)) == POST /apply-template {messages: F.messages, tools: F.tools}
```

Equal renders for every prefix of every fixture subsume the byte-prefix assertion. It is required (a
failing dialect does not ship, a failing model is not admitted, no "compat quirk" hatch), runs on every
commit and every `template_sha` mismatch, and the corpus must cover the shapes that break: a plain turn,
reasoning, reasoning interleaved with two tool calls, a tool result, an *empty* assistant turn, a system
message at index > 0, an image, a very long turn, and each truncated at every prefix boundary. Adopting a
template change forks every live transcript on that model.

| § | statement |
|---|---|
| **7.3** | **GLM-5.3-Flash** is production (`zai`, 262,144 per slot, MTP at `n_max=2`, vision via `--mmproj`, and a collapse past ~78k that poisons the slot, so §8.5's guard is built regardless); **Qwen Flash** is `qwen`, where thinking blocks for prior turns are emitted always; **Qwen 3.8 27B dense** needs a preset (D7: derive it by measurement). The model is a property of the **session**: a switch costs 29-31 s and **forks** the transcript. |
| **7.4** | Adding a fourth model: a preset, a dialect or a declared reuse, the **`/apply-template` diff as a hard gate**, the two property tests, a short generation plus one `n_predict: 1` call, then the dialect-parameterized suite. If the diff fails, fix the renderer or patch the template - never ship a divergence. |

## 8. The tool contract

F4 says close the loop and F5 says never let a component's "I did not do this" be reported upward as
success; a tool's failure mode is a design decision whose cost is paid in permanent context.

| § | statement |
|---|---|
| **8.1** | Six clauses, and a tool that misses one does not ship: (1) **a miss is self-correcting in the SAME call**; (2) **malformed input is salvaged, not rejected**, with a note of what was repaired; (3) **outcome is a closed vocabulary and abstention is not success**, *structurally* distinguishable in what the model sees; (4) **read/write is declared in the schema** as `access: read | write | exec | network`; (5) **output is bounded and spilled, never truncated**; (6) **the description says how to use the tool, never what the data contains**. |
| **8.2** | Abstention cannot be papered over: the outcome class is carried, so `NO_RESULT` is a distinct token sequence; abstention propagates upward, so a subagent whose calls all abstained cannot return `Ok`; and a turn with no visible content fails. It does not judge whether prose is supported by output - that is an eval. |
| **8.3** | Spill, never truncate: `max_inline_bytes` with **no default** (unset is a genuine no-op, and D6 makes the threshold a *decider*), a preview budget of `cap - reserve` so the notice never overflows the cap, a head/tail split, and the original written `0600`. Ours adds a content hash into `tool_spill` plus `read_spill(hash, range)`, so the hint is actionable. |
| **8.4** | A hard ceiling, because past ~5-7 MCP servers small models get *worse* at choosing tools: `orchestrator` (6), `coder` (7), `researcher` (6), `reviewer` (5), with the daemon **refusing to start a session** over `max_tools` and MCP servers attached per role. Subagents pay off through **context hygiene, not decode speed** (F2), so concurrency is configured. |
| **8.5** | Two free guards: a rolling n-gram check turning GLM's `@@@@...` collapse into `Warning{code: repetition_collapse}`, and a warming state with no idle timeout below ~120 s that counts `prompt_progress` chunks as liveness. |

## 9. Context occupation vs cache stability - the tension, resolved

F1/F2 say the prompt must be append-only and byte-stable; F3 says occupation defocuses, and reasoning
measured ~51.7% of a conversation's content, so replaying it roughly doubles occupation.

| § | statement |
|---|---|
| **9.1** | **Keep the prompt append-only and byte-stable, make what goes into it dense, reclaim occupancy at compaction on a schedule.** (1) nothing enters the transcript that does not have to be there; (2) **reasoning is replayed verbatim, and that is not negotiable while llama.cpp caches generated tokens** - dropping it produced the 612 GB; (3) occupancy is reclaimed by compaction (§10), scheduled and cheap, which is what makes (2) affordable; (4) **depth is preferred over premature compaction**, since a 150k warm conversation costs about the same per turn as a 15k one. |
| **9.2** | Reasoning is the *right* half to keep: it is what the model produced to reach the answer, the part a summary reconstructs worst, and not something the harness can make denser. Tool output is the opposite on all three counts. |
| **9.3** | **Falsifier A**: replay on and off, scoring quality - if better without, reasoning is noise. **Falsifier B**: quality against depth at 20k/60k/150k - if it falls off before the window, §9.1 clause 4 is wrong and compaction must be early; this is M3.5 (§17). **Falsifier C**: if the summarizer cannot hit the warm prefix (I5), clause 3 is unfunded. |
| **9.4** | Retrieval stays out - `ask_corpus` / `ask_code` / `search_corpus` are tools and their abstention is §8.2's. The harness must not silently "improve" a tool's query, and if it ever does, the rewrite must be visible. |

## 10. Compaction, as leveled compaction

| § | statement |
|---|---|
| **10.1** | The surveyed default is a stop-the-world full pass sharing a prefix with nothing (144,436 tokens, cache hit 0, ~13 minutes), which is pre-LSM compaction. The mapping is L0 = recent turns verbatim (the memtable), L1 = a segment summarised once, L2+ coarser, incrementally in the background, with write amplification becoming **fidelity amplification** - which does not make the stall faster, **it removes it as a category**. |
| **10.2** | L0 is the last N turns (default 8) verbatim where the cache is warm; L1 the last 3 closed segments at ~1:4; L2 older at ~1:4 of L1; Lc one running "what this session is about" paragraph plus an index of labels, never discarded. **Levels are monotone in age but not merged across a live boundary** (`last_touched` moves a segment, `created_at` does not), and fidelity amplification is bounded at one re-summarisation per level over 4 levels. |
| **10.3** | Warm compaction: **the summarizer request is the last routed request, byte-for-byte, plus one appended user message**, which is literally `render(prefix, items[0..k]) ++ render(instruction)` because the transcript is append-only and ledger-chained. Expected `f_keep` ≈ 1.0 (I5). A segment summary names its span by `SegmentMark` ids and keeps the full live prefix, so it costs the same as a tail summary. |
| **10.4** | **Soft compaction (default)** appends the summary as an in-history update: zero prefill, no tokens freed, attention pressure reduced. **Hard compaction (at the wall)** forks into `[prefix][Lc][L2][L1][L0]` for one cold prefill of the shorter prompt, with recent verbatim before the summary and project instructions re-injected verbatim. Hard runs only at `high_water` (80% of `n_ctx`), by which time every segment already has a summary - which is what removes the stall. |
| **10.5** | The cache ladder and compaction have the same shape and **stay independent, sharing vocabulary rather than policy**: a degraded entry still restores, a coarsened segment does not, and coupling them would let memory pressure degrade answer quality when it should cost latency. |
| **10.6** | Recent verbatim before the summary, and reasoning kept in the retained tail, so the prefix cache stays warm. |

## 11. Adjudication - one interface, three adjudicators

| § | statement |
|---|---|
| **11.1** | > *"we are flexible, it is ok to ask me, it is ok to use firecode and it is ok if i plug a model for auto mode."* A pluggable layer, not a doctrine: **boundary** decides by structure, **human** by judgement, **model** by a smaller model for the middle band - all first class and interchangeable, with the volume of asks an **observable, not a constraint**. |
| **11.2** | One decision shape: `AdjudicationRequest{id, session, turn, tool_call, agent, action{class, tool, summary, arguments, digest}, context{run_id, workspace, boundary_facts, prior_decisions}, kind: Permission|Question, options[{id, label, kind}], deadline, on_timeout}` and `AdjudicationDecision{request_id, outcome: Selected|Escalate|Unavailable|Cancelled|Timeout, by, basis, at, latency_ms}`. *A throwing answerer becomes `unavailable`, never silently opens the gate.* |
| **11.3** | The **action class** is the routing key, derived from `tool.access`, `effect_scope`, `reversibility` and `cost`. Policy is a table the operator edits, first match wins: |

```
# class pattern                              → adjudicator      timeout   on_timeout
exec,in_run,*,free                            → boundary
write,in_run,*,free                           → boundary
read,*,*,free                                 → boundary
network,in_run,reversible,free                → model            20s       Escalate
write,host_project,reversible,free            → model            20s       Escalate
*,host_other,*,*                              → human            15m       Deny
*,external,irreversible,*                     → human            30m       Deny
*,*,*,metered                                 → human            30m       Deny
question                                      → human            15m       AgentDecides
```

A whole band moves with one edit; a class never seen in the log is one nobody has thought about, and §6
lists observed classes with counts; `boundary` rows cost nothing at runtime.

| § | statement |
|---|---|
| **11.4** | The boundary adjudicator decides by structure: the guest sees one project and nothing else of the host; `--add-dir`/`--workdir` **refuse outright** to carry `.ssh`, `.gnupg`, `.aws`, `.kube`, `.config/gh`, `.password-store` or a browser profile; project paths come from the spawn server's config; lifetime is a cgroup. It does **not** protect API credits or the network, which is why `metered` and `network,external` route away from it. A denial **escalates rather than hard-failing**. |
| **11.5** | The human path, kept precisely because it is rare and a rare ask that hangs is worse than a frequent one: **post and wait**, then **if nobody answers say so and tell the agent to decide and record what it chose** (`AgentDecides` for a question, `Deny` for an irreversible effect); **acknowledge before you act**; and **start listening before you start working**, so a lost subscription raises `no_human_channel` and *changes the effective policy* (not raised anywhere in this tree yet). The primitive is a **visible row, not a held connection**; first valid answer wins with a later one recorded `late`; every step is an immutable row never in the transcript. |
| **11.6** | Permissions and questions are one mechanism, differing only in `kind` and in the default on no answer - because the volume is low and because §19.2 is a question, not a permission. |
| **11.7** | The model sees a **bounded brief and never the conversation** - not the transcript, for cost and for independence. In the tree that is `ModelBrief`, which also carries the operator's past answers as worked examples, the agent's claim as evidence and never a citation, and the gate's prior record as evidence and never a promotion. It returns the same decision constrained to `{option_id | "escalate", basis}`; not the session's model (the 27B dense, UNVERIFIED-7); unavailable routes to `on_timeout` and never a silent allow. Audit: digest, raw output, option and basis per decision, **shadow mode** recording both answers, and per-class counts, allow rate and agreement in `EXPLAIN`. |
| **11.8** | Firecode: D3 answered the cgroup and shell asks; the rest is one request - give the room the request/response object §11.2 needs (ids on asks, a deadline and default on `--ask`, an `--ack` verb, a per-run room or run tag, structured options) and the flowy bridge becomes a mapping. |
| **11.9** | opencode's `permission.ask` has zero trigger sites, a dead hook; what is worth taking is its permission *model* and one discipline - the `never` policy is enforced inside the service before dispatch, so a `deny` row is evaluated before any adjudicator and none can override it. |

## 12. What flowy must provide

| § | statement |
|---|---|
| **12.1** | flowy already has projects, rooms, DMs, a single-holder named-reader inbox, attachments, a live WebSocket, task delegation, announcements with `ack`/`resolve`, `todo waiting-on`, ULIDs and an MCP server, with an authenticated `actor` ULID that satisfies §11.5. Three corrections: **SSE exists but chat is not on it**; **there are three wake levels, not four**, with an *unaddressed* broadcast waking you inside `addressed`; and **per-room policy does not exist and cannot**. |
| **12.2** | The asks: (1) **a `decision` object** with options, a server-enforced deadline, `/answer` exactly once, `/ack` and `/withdraw` - *nothing else matters as much*; (2) options as choices; (3) server-enforced deadlines; (4) delivery receipts; (5) multiple readers per seat or a namespace; (6) **a pushed inbox stream** (`GET /api/inbox/stream?as=NAME&...`, same `wakesFor` and reader row, one JSON message per event) plus a fourth `waiter_kind` and `LISTEN`/`NOTIFY`; (7) updatable message content; (8) idempotency keys; (9) cross-project reach, or an explicit "not reachable" error; (10) optionally agent-to-agent RPC. |
| **12.2b** | Per-room policy is ours, since the one-flag limit follows from one reader per name: a per-room subscription row `(room, level)`, flowy's three definitions kept exactly, the one-flag shape dropped, and the **human-broadcast clause made an explicit named option** because it has failed in **both** directions. |
| **12.3** | The degraded v1, so flowy never blocks: a DM with `[decision <id>]` and one option per line, a strict answer grammar, the harness's own ack, harness-side deadlines, and `no_human_channel` falling back to `GET /api/presence` - labelled as the proxy it is. |
| **12.4** | flowy and the firecode room stay **separate, bridged by harnessd on the host**, because a flowy token inside an untrusted guest would give it a seat's voice and the room must work with no harness at all. If flowy ever supports a **capability token scoped to one run and one decision id**, unification becomes better. |

## 13. Multi-head attach and detach

| § | statement |
|---|---|
| **13.1** | There is no prior art: nothing surveyed lets two clients concurrently observe and drive *one* live conversation, so "tmux for an agent" must be designed. |
| **13.2** | A head sends `ATTACH {session_id, since_seq, identity, caps}` with the event log authoritative. **Late join** gets a snapshot then events from S+1; **resume** gets the gap or `RESYNC` past a bound (5,000 events or 4 MiB); **fan-out** demotes an overflowing head to resync, so *the model stream is never blocked on a head*; **two heads** serialize on a queue with `expected_seq`, a mid-turn prompt queues as a follow-up item, and there is **no input lease**; **all heads detaching does nothing** - **TCP close is detach, never abort**; and a **restart mid-turn keeps the partial output**, because those tokens are in the cached entry and discarding them guarantees a divergence. |
| **13.2b** | Nine mechanics from flowy: a cursor acked **after** the client writes out; the mark advancing over everything **read**, not delivered; **reporting what was filtered out**; one waiter per name, so **several heads share ONE reader**; **idle means quiet**; scrollback that **discloses the drop** with the count present-and-zero; snapshot and register under one lock; non-blocking fan-out; and **scrubbing the stored copy of anything interactive**, with a queued reply discarded rather than held. |
| **13.3** | Render cost independent of length: events carry increments only, and the head lexes from a stable block boundary (the last `\n\n` with balanced fences), licensed by `lex(prefix) ++ lex(tail) === lex(prefix+tail)`. Never rebuild every child per delta. |
| **13.4** | TUI over the socket, remote over WebSocket+TLS, flowy as an in-daemon connector holding a push subscription, ACP as stdio JSON-RPC. ACP is an **adapter, not the protocol**: its permission objects have the right shape and no durability (no timeout, no request id, cancellation as a returned value, a non-exhaustive enum), so adopt its vocabulary and keep our own `{id, deadline, on_timeout, audit}`. |

## 14. Ported conformance suite - milestone zero

A test encodes the bug and the invariant, not the design, so it ports across languages; the suite exists
before the harness does, and it is what tells us a milestone beat opencode rather than felt like it. It
lives in `tests/fidelity` - the `/apply-template` gate, its corpus and the oracle.

| § | statement |
|---|---|
| **14.1** | From **grok-build (Apache-2.0)**: the prefix-stability suite, whose helper asserts `ext_input[..base_input.len()] == base_input` and deliberately does not compare `model` or `tools` - our equivalent going through the real builder and encoder or it proves nothing - with its twelve tests (baseline, injected reminder, reasoning-as-sibling ordering, schema change, model switch, synthetic messages appending, **image pruning** as the documented exception, budget pass on the copy, tool-result pruning, server-executed calls, resume byte-identical), where the three exception tests matter as much as the nine strict ones because they encode where the invariant may break. Also its length-policy family. From **omp (MIT)**: the reasoning-replay contract, the append-only-context family (byte-equality rather than reference-equality is the contract), and the thinking round-trip. From **pi (MIT)**: no tool calls from a length-truncated message. From **deepseek-harness (MIT)**: the system-prompt projection tests, `content: ""` never `null`, the cache-accounting split. From **opencode (MIT)**: port with care and **read the assertions, not the names** (three overflow tests are named `BUG:` and assert the correct behaviour). **oh-my-openagent is excluded** (Sustainable Use). From **flowy (D4)**: its delivery tests, every one of which our per-room subscription could reintroduce, plus its author-test inversion - the wire format carries everything the model carries **minus a named exclusion list** by reflection, the antidote to the enumerating-renderer bug class. |
| **14.2** | Licence mechanics: no surveyed repo stamps SPDX per file, so attribution must be added - a suite-level `NOTICE` (repo, URL, SHA, licence, files) and a per-file header with the SPDX id, upstream range, SHA and what changed, which is also how you find what a failing ported test asserted. D8 settled the harness as Apache-2.0; the suite should be Apache-2.0 in its own directory regardless. |
| **14.3** | The checks that run against a real server plus *any* client, carried by a recording proxy and `POST /apply-template`: |

| # | check | how |
|---|---|---|
| C1 | prefix extension, exact | request N's prompt must be an exact prefix of N+1's; ours holds by construction (§4.3), so the test measures other clients and catches a ledger regression |
| C2 | renderer fidelity | `/apply-template` per fixture, exact equality against our renderer; the required CI gate |
| C3 | generation-inclusive prefix | `H(prompt_N ‖ committed_generated_N)` must re-hash as a span of N+1's prompt; a mismatch is `Warning{prefix_divergence}` and the only defect in us (§18.1-I1) |
| C4 | `f_keep` distribution | `lcp / cached_entry` over a scripted 30-turn session, reporting p10/p50/p99 and p99 re-prefill (D11: not `cached/prompt`, which is `f_sim`) |
| C5 | reasoning replay | `cached_tokens` must not collapse on the turn that adds a prior assistant turn plus a nudge |
| C6 | `content: ""` never `null` | on a tool-call-only assistant history turn, `content === ""` |
| C7 | truncated tool args are not executed | `max_tokens` low enough to cut a call; the client refuses and re-prompts |
| C8 | `finish_reason` handling | `max_tokens: 1`; the client surfaces `length` rather than success |
| C9 | in-history system prompt | message 0 unchanged, a new system message at the tail, `cached_tokens` high |
| C10 | cache accounting is disjoint | "new input" tokens equal `prompt_tokens - cached_tokens` |

C3's token-count form is a **measurement of the server**: a hybrid/recurrent model undershoots by design
because llama.cpp resumes from a context checkpoint, so a shortfall is not a violation.

| § | statement |
|---|---|
| **14.4** | What no suite covers and we write: warm-prefix compaction (I5), `prompt_progress`/`timings` and the idle-timeout interaction, adjudication (routing, escalation, timeouts, first-answer-wins, audit, shadow agreement), multi-head, renderer conformance over every dialect, and `EXPLAIN` correctness against an induced divergence. |

## 15. What to vendor or port, and from where

Licences: grok-build **Apache-2.0**; omp, pi, deepseek-harness, opencode **MIT**; oh-my-openagent
**excluded**. From grok-build: the prefix test first, the `ConversationItem` shape, `LengthPolicy`, and the
compaction assembly order with recent before summary. From deepseek-harness: the in-history prompt, the
warm summarizer, spill-don't-truncate, `content: ""` never `null` **and the comment**, the README
convention. From omp: incremental markdown lexing, and `append-only-context` whose 32-bit digest §4.3
supersedes. From opencode: the Part union as the **Journal's** type, plus schema shapes, tool bodies and the
permission ladder as starting points. From pi: steering into a running loop.

**Not taken**: grok-build's transport layer, where no `extra_body` means `return_progress` and
`cache_prompt` cannot be sent, a closed `FinishReason` fails on an unexpected value, and no unknown-field
capture makes `timings` invisible - exactly what a local-first harness needs. Two rules adopted from those
omissions: every wire enum has an `Unknown(String)` fallback, and every streamed delta captures unknown
fields, so a field the server adds tomorrow is visible today.

## 16. Language and runtime

The requirements: byte-exact serialization and cheap hashing; a long-lived daemon with streams,
backpressure and cancellation; fan-out without head-of-line blocking; exhaustive sum types; SQLite; a good
TUI; portable tests.

Python is out for the daemon (a multiplexing daemon under the GIL is the wrong shape) and right for the
suite. TypeScript is fastest to working code, but `JSON.stringify` key order is insertion-order dependent -
a discipline rather than a guarantee. Go's one specific cost is that its data model is almost entirely sum
types and interface-plus-type-switch drops exhaustiveness checking, which is what protects "did the
renderer handle the reasoning variant".

Rust is the fit: a private constructor and one `append` is how §4.1 becomes structural rather than a review
rule; `serde` makes the two silent failure modes explicit; the crown jewel of the ported suite is already
Rust; one static binary; `tokio` for bounded per-head channels and `ratatui` for the TUI; sharing the vocab
makes our token ids the server's by construction; and a `memfd` mapped as an append-only `&[llama_token]`
is what the aliasing rules are for.

> **Daemon and TUI in Rust. Conformance suite in Python (pytest), deliberately, and it must not depend on
> the daemon.**

The hedge is load-bearing: because the suite is black-box, it measures the Rust daemon, today's opencode and
any candidate on one axis, including fused against unfused. The language decision stays reversible and **the
fusion decision stays measurable**.

## 17. Phasing

Each milestone is independently testable, and each names the C-tests or I-invariants that certify it. M0-M8
have largely landed; the server track has not.

| § | milestone | exit |
|---|---|---|
| **M0** | the conformance suite with no harness: proxy, corpus, gate, then C1-C10 against opencode | a baseline of `f_keep` p10/p50/p99, p99 re-prefill and C1-C10 pass/fail |
| **M1** | `harnessd`, one head and read-only tools, entirely on the fallback | C1-C10 pass where opencode failed, `f_keep` p10 >= 0.99 over a scripted session |
| **M2** | firecode integration and write tools, with the `boundary` adjudicator | the host project unmodified, a destructive `bash` harmless, a row per gated call |
| **M3** | the flowy connector, chat first, before M4 because the async decision shape cannot be retrofitted | a decision answered from a phone, and `no_human_channel` exercised |
| **M3.5** | Falsifier B, one measurement at 20k/60k/150k | a number, and a written decision to keep or revise §9.1 |
| **M4** | leveled compaction, high in the phasing on purpose | I5, and no stall longer than one cold prefill of the compacted prompt |
| **M5** | multi-head plus the ACP adapter | the §14.4 suite, and a turn surviving every client closing |
| **M6** | concurrent subagents, the first real concurrent client here | seats > 1 under load in `/metrics` and `/slots` |
| **M7** | the model adjudicator in shadow, promoted per class | agreement per class, and a documented promotion |
| **M8** | the other two models | the dialect-parameterized suite green on both |

**Server track** - same release, its own build loop, none landed and none blocking a milestone: **S0** the
control channel and shm submit; **S1b** `Spans`; **S2b** `Admit`/`Stats`; **S3b** `SegmentHint`; **S4b**
`Prewarm`; **S5b** `Explain`; **S1** `lcp`/`f_keep`/`f_sim` in `timings` (highest value, lowest risk,
first); **S2** age-based demotion; **S3** the eviction-ladder tests; **S4** the block-hash index; **S5** the
recompute-fraction experiment; **S6** the prefix-handle protocol with `required` early; **S7** stitching.

**The smallest thing that beats today's opencode setup** is M0 + M1, and neither needs a line of C++. M1
removes all four measured defects: the cache stops growing without bound, the tail collapses from 78,598 to
~5,000 tokens of re-prefill, an empty subagent becomes a failure, and a long prefill shows a progress bar.

## 18. Test strategy

**I1, the generation-inclusive prefix invariant**, is the real one: > Request N's prompt **plus the generated
tokens the harness committed in turn N** must be a prefix of request N+1's prompt. Grok pins only
prompt-to-prompt, but llama.cpp caches generated tokens too, so a harness can satisfy Grok and still diverge
- which is what the 612 GB was. The generation term is **committed** tokens, not the server's `predicted`.
**Exact** form: hash turn N's prompt with its committed generation and re-hash that span of N+1's,
post-flight every turn, raising `prefix_divergence` into `TurnMetrics.prefix_check`; and because it needs
only token ids we rendered, the gate is the backend's `may_assert_structural_prefix`, which over a
`messages` API skips **loudly** (`prefix_check_skipped`, `held()` still false) rather than passing green.
**Token-count** form: `cached_tokens(N+1) >= prompt_tokens(N) + committed_generated_N`, whose shortfall is
a measurement of the server (`cache_reuse_shortfall`), not a divergence.

| id | invariant |
|---|---|
| I2 | exact prefix extension of the token array - holds by construction (§4.3), and the test exists because that is a claim about code someone will edit |
| I3 | renderer fidelity - `/apply-template` over the corpus, exact equality per dialect; required CI (§7.2) |
| I3b | `render_incremental ≡ render` and `parse ∘ render ≡ id` - pure functions, property-tested |
| I4 | the ledger is a chain - a periodic full re-hash equals the incremental head |
| I5 | compaction is warm - the summarizer's own `f_keep >= 0.99`, prefill bounded by the instruction |
| I6 | the stable prefix is stable - its fingerprint changes only on a declared event, which forks |
| I7 | `finish_reason` is never dropped, and an empty `length` turn always fails |
| I8 | abstention does not become success, including across a subagent boundary |
| I9 | no adjudicator can override a `deny` row - evaluated before dispatch |
| I10 | a head cannot block the model stream - a stalled head resyncs and latency is unchanged |
| I11 | an inbound chat message costs exactly its own tokens - one user item, no envelope |
| I12 | no polling anywhere, and `no_human_channel` from a dropped subscription, never a missed poll |

| § | statement |
|---|---|
| **18.2** | Measured as `f_keep` = `lcp / cached_entry`, p99 re-prefill = `prompt_tokens - cached_tokens`, rates from `timings`, progress from `prompt_progress`, box state from `/metrics` and `/slots`, over **a scripted 30-turn session** with a mid-session system change, an oversized result, a truncation, a compaction crossing and a fork. Traps paid for: a tok/s number without its concurrency is not a number; two conversations cannot test residency; a captured pid is a proxy; `${X:-d}` substitutes on empty; a full model load looks like OOM; and with `--no-op-offload` a `-j 24` build steals prefill. |
| **18.3** | Property tests: random transcripts asserting I2/I3b on every adjacent pair (pure functions, so full volume on every commit - the biggest single testing win of self-rendering), random tool outputs asserting `len(preview + notice) <= cap`, and random policies asserting every action matches exactly one rule. |
| **18.4** | Culture: the README convention verbatim per prompt-contributing feature - **what the model sees**, **token effect**, **KV cache effect**. |

## 19. Future directions, and what they demand of v1

Not v1 features, but each constrains the v1 data model, and all three want **context as addressable
segments** - which is why `SegmentMark` and the `segment` table are in v1.

| § | statement |
|---|---|
| **19.1** | **L1**: compaction must never be a whole-history operation, or everything downstream assumes one summary. **L2**: a segment carries `level` and `last_touched`, and `last_touched` promotes it - the whole of "different decay rates" for two columns. **L3**: summaries live per segment in the journal, so a segment can be re-levelled without re-summarising. |
| **19.2** | **L4**: compaction can ask a human mid-operation over §11.6's one channel. **L5**: **a segment is labelled at creation** - reconstructing labels for 200 segments means running a model over the whole history, which is the pass §10 exists to abolish - so the label is the first ~80 characters of the opening user message plus the tools used. |
| **19.3** | A swap re-prefills linear in the suffix (~11 s at the tail, ~453 s at the head of 200k), so swappable memory is **not free** and **the recommendation is: append, do not swap**. **L6**: segments are addressable and serialisable with rendered bytes a contiguous ledger span - the one v1 decision a later server change depends on. **L7**: the pipeline takes an *ordered selection* of items, always "all of them" in v1. Memory is built on the **oracle pattern** - a searchable corpus behind a tool - plus **segment-to-corpus ingestion**, so a demoted segment's text stays retrievable while its summary stays in the prompt. Flagged disagreement: the brief accepts that memory fights append-only prompts, but swapping should not be built here - decode is flat against depth, the pool is elastic and the L2 tier makes a cold conversation cheap. **Stay deep and stay warm**; if Falsifier B shows otherwise, that inverts. |

## 20. Risks and open questions

| § | statement |
|---|---|
| **20.1** | Those that change a decision: **UNVERIFIED-16** (can stitching work on GLM's 34 recurrent KDA blocks? the highest-value open question, settled by §3.10-C per layer class), **-13** (`--cache-reuse` inertness - grep the startup log), **-15** (tokenization's share of warm latency), **-7/-8** (auto-mode model and the 27B dialect), **-14** (the collapse does not reproduce to 147k; build the guard anyway). Cheaper: **-1/-2** are *dissolved*, since under self-rendering no template can drop reasoning or a mid-history system message, leaving a behavioural eval; **-2b** by §7.2; **-3** by measurement (do not build checkpointed KV in v1); **-4** by checking the tag; **-5** is no longer a dependency of I1; **-6** by one call; **-9/-10/-17/-18** by asking whoever owns flowy; **-11/-12** by measuring the exec and file channels; **-19** by D4. |
| **20.2** | **No cloud fallback**, mitigated not by an abstraction layer but by D10's `crates/backend` seam. **Fusion couples us to llama.cpp's internals**, and the vocabulary is the part that cannot be ported. **Self-rendering shifts template drift onto us**, with §7.2's CI diff as the entire mitigation - if that check is ever made advisory this becomes the most dangerous risk in the plan. **Rust is slower to write**, mitigated by reversibility. **Building here steals inference.** **flowy is a moving target**, mitigated by §12.3. **Auto mode drifting** is the most dangerous new component, since a slowly loosening adjudicator is precisely F5 - mitigated by permanent shadow mode. **Segments may be the wrong abstraction**, for two columns. And **concurrency could destabilise the server**, which is why M6 watches `/metrics`. |
| **20.3** | Corrections, flagged rather than smoothed: **the supersede rule is containment, not "exact full-prefix match"**, and **line numbers in the surveyed repositories drift**, so re-check a citation before repeating it. |
| **20.4** | Unsettled: **whether §9's position is right** (Falsifier B load-bearing); **whether the volume of human adjudication will be tolerable** - the policy table is configuration precisely because it will be wrong; and **whether one daemon is enough** for one deep orchestrator plus a dozen to thirty shallow sessions, where the arithmetic says the binding constraint is a third GPU. Revisit at M6. |
