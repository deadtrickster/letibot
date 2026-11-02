# Implementation plan - a local-first agent harness for lab2x1

The design contract for letibot: a long-lived session daemon (`harnessd`) running agent loops against
the operator's llama.cpp fork, with attachable heads, a prompt the harness renders and tokenizes itself,
adjudication as a first-class subsystem, and a ported conformance suite as milestone zero.

Later decisions are in `DECISIONS.md`, cited by D-number. The harness half is built (`crates/`); the
fused server track (§3.3, §17 S0-S7) is not, and the harness runs on the `/completion` token-array
fallback (§5.6). Two operator directives govern everything: *"I want harness and server fused to the
11"* and *"like Apple ecosystem basically, I don't care about other providers or generalizations."*

Section numbers are the marks `TODO.md`, the notes and the code cite (`§5.7`, `§11.4`, `§18.1-I1`), so
every one is kept. Read four things: §3.1, §4.3, §7.2 and §17's last row.

## 0. The thesis

> *"my goal is to have my llm runtime behave like a normal data app, don't see any reasons it
> can't."* - the operator

Every failure that motivated the design is something data systems settled decades ago: an index that
never merges duplicates (§3.10-A), a buffer pool with a page limit, leveled rather than stop-the-world
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
provider-abstraction *category* is deleted, because every abstraction that exists only for a provider we
do not run is a liability and is why the surveyed harnesses default to metered APIs. A fourth model
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
| **F1** | The prompt is append-only, or the cache is gone: llama.cpp caches prompt *and generated* tokens, so request N+1 must extend request N including turn N's generation (replay off gave `f_keep` p10 0.000 and p99 re-prefill 78,598 tokens; on, 0.999 and 5,294). |
| **F1a** | Append-only is also the cache's garbage collector: the cache drops any cached prompt fully contained in the current one, so a conversation occupies one entry for its whole life. |
| **F2** | Depth is paid in memory and prefill, never in decode (35.7 t/s below 10k of context, 36.9 above 120k), so a deep *warm* conversation is cheaper per turn than a shallow *cold* one. |
| **F3** | Context occupation defocuses - a big window is higher tolerance, not immunity - so minimising occupation is a quality measure. F2 and F3 oppose each other and §9 resolves them. |
| **F4** | Close the loop; never paper over it with prompt. *Prompts are soft limits, hooks are hard stops*: a prompt rule is enforced by the component whose failure it is meant to catch. |
| **F5** | The failure shape to hunt is "the system did less than it claimed and said nothing", so every layer boundary must make *"I did not do this"* structurally different from *"here is the result."* |
| **F6** | The server is a preemptive scheduler and this harness is its only client, so every seat measurement here is synthetic until a client asks for concurrency (§17-M6). |
| **F7** | Statistics decide policy, so statistics must be right: a wrong number is worse than a missing one because it produces a confident wrong plan, which is why §6 and §18 exist. |

## 3. Architecture

| § | statement |
|---|---|
| **3.1** | The harness renders the chat template itself, tokenizes it itself and sends token ids; because the transcript is append-only (§4.1) the token array is too - one growing `Vec<llama_token>` per transcript, nothing ever re-rendered - which makes the prefix invariant something the harness *constructs* rather than hopes a server renderer preserves, so a violation is not expressible. |
| **3.2** | Two processes, one system: `harnessd` holds the store (SQLite/WAL), transcript and token ledger, renderers, tokenizer, turn engine, `EXPLAIN`, the tool runner and adjudication; `llama-server` holds the scheduler, elastic pool, earned seats, sequence registry, prompt cache with its L2 tier and the pressure ladder. One release, one version number - not two systems with an API between them, and not one binary. |
| **3.3** | The seam is the third of four candidates: a private, length-prefixed, versioned binary protocol over a Unix socket where both sides refuse a peer whose version is not exactly theirs (they ship together, so a mismatch is a deployment bug), plus one `memfd` region per transcript appended by the harness and mapped read-only by the server, so a request is `{region_id, start, len, prefix_handle}`. One binary was rejected because every harness change would force a C++ link and a 29-31 s reload. The control vocabulary: `Admit{n_sequences, est_tokens}` → `Fits{ceiling, cost_per_id, pool_free} | Refused{why}`, `Prewarm{region_id, upto}`, `Submit{region_id, start, len, sampling, message_spans[], segments[], prefix_handle, want:{progress,timings,tokens}}`, `Spans`, `SegmentHint`, `Handle{id, n_tokens, hash}`, `Explain{turn_id}`, `Stats{}`. What stays HTTP is `/props`, `/apply-template`, `/metrics`, `/slots`, `/health` - the surfaces that keep the black-box suite working from outside (§14.3). |
| **3.4** | Six unlocks, all server-track (§17) and none landed: **3.4.1** checkpoint placement stops being a heuristic, since the harness emitted the tokens the server recovers by string-matching delimiters; **3.4.2** eviction becomes semantic, since only the harness knows a segment is a finished subagent; **3.4.3** one truth for accounting, `prompt_tokens` being `len`; **3.4.4** admission control, `Admit` returning a real number to decide how many subagents to spawn; **3.4.5** `Prewarm` restores a sequence before the request arrives; **3.4.6** `Explain` returns the matched entry, `lcp`, `f_keep`, `f_sim`, the rung and the batch iterations shared, so two logs become one plan. |
| **3.5** | Fusion couples the harness to llama.cpp's internals, and the exit is three named modules - the tokenizer (today `libllama` FFI), the submit path (a token array over HTTP, which vLLM and SGLang both accept) and the fused control vocabulary, which has no equivalent anywhere - so losing the third means going back to being a normal client. It is also why the black-box suite stays. |
| **3.6** | The split is deliberate: renderers, the ledger, compaction, the tool contract, adjudication, flowy, heads and `EXPLAIN` iterate in the daemon alone in seconds, while only the protocol version, the shm submit path and §3.4's six unlocks force a C++ build - kept cheap by additive lockstep versioning, the `/completion` fallback so a server rebuild never blocks (this tree runs on it), and builds in a niced firecode run. |
| **3.7** | Chat is a channel of the loop. > **[operator]** *"I don't want to fiddle with monitors, nor timers which pollute the context."* The poll loop is a timer in disguise whose bookkeeping is a permanent context cost, so: **inbound messages are pushed, never polled**; **a chat message costs exactly what a user message costs, because it is one** (§18.1-I11); **chat is the primitive and adjudication is a usage of it**; and **a flowy chat is a head** (§13.4). |
| **3.8** | What is authoritative: the daemon's session log (append-only events with a monotonic `seq`, single writer per session) and the daemon's token array. The KV cache is only an accelerator - every request must be correct with an empty cache, which `--cache-reuse` violates by restoring recurrent state computed with deleted text present. No head's view is authoritative, including the TUI's. |
| **3.9** | The execution substrate is firecode, the operator's per-run microVM sandbox: a **run** is one VM and everything created for it, its lifetime is a cgroup subtree so one `cgroup.kill` ends the VMM and every VM it started, and the guest is untrusted by construction. Effectful tools run inside a boundary - `Bwrap`, `Seatbelt` or firecode - and placement is **harnessd on the host, tools in the run**, because the log, ledger and audit must survive the VM and the model server cannot move. A session is one run, a subagent a sibling run in the parent's cgroup, a call one `vm_in`. D3 answered the host-caller case: cgroup membership is use-dependent and the persistent shell is byobu's job. A checkpoint restore must append a `ToolResult{outcome: NotRun}`-style marker saying the workspace was rolled back, or the transcript becomes a lie the model acts on. |
| **3.10** | **A.** cross-session sharing is real and block granularity would let *branches* share, but the pool is a budget rather than cells and GLM's 436.7 MiB of per-sequence recurrent state is not token-proportional, so **two ladders at two tiers, never merged** - the cache ladder degrades a whole conversation entry, least-degraded first, while a shared-block index needs refcount plus LRU among refcount-zero blocks at the resident-pool tier. **B.** composing blocks out of order is blocked by physics - a block's KV depends on every preceding token and its absolute position - so **"send blocks A, F, Q plus some new text" cannot work as stated**, and what a handle buys is an explicit, verifiable cache hit instead of a discovered one (`prompt_handle` out, `prefix_handle` in beside the full prompt, `hit`/`miss` with a reason, `prefix_handle_required: true` turning a miss into a 400). **C.** position shifting is measured unsafe on GLM's recurrent components and must be re-earned per architecture; stitching's economics are unknown for MLA + DSA because its premise assumes attention-only layers (UNVERIFIED-16); and accepting the cost always works, re-prefilling linear in the suffix (~11 s at the tail, ~453 s at the head of a 200k conversation), which v1 takes (§19.3). |

## 4. Data model

| § | statement |
|---|---|
| **4.1** | **Layer 1, the Transcript**: the ordered items that become the prompt, immutable with exactly one mutation, `append`; truncation is a new transcript with a new id (§5.5), because it is a cache divergence and must be visible as one. **Layer 2, the Journal**: display parts, deltas, progress, timings, errors, adjudication, warnings - freely appended, never consulted when building a request. Enforced by types: **the request builder takes a `&Transcript` and nothing else**. |
| **4.2** | The item type, with reasoning a **sibling** of assistant because the interleaved order of `[reasoning, tool_call, reasoning, ..., message]` must stay byte-stable across turns, which is what lets the prefix cache hit: |

```
TranscriptItem =
  | System      { text, origin: Bootstrap | Update }
  | User        { parts: [Text | Image | FileRef] }
  | Reasoning   { text, field: ReasoningField }
  | Assistant   { text, tool_calls: [ToolCall] }
  | ToolResult  { call_id, name, outcome: ToolOutcome, payload }
  | SegmentMark { segment_id, label, kind, edge: Open | Close }
```

`ToolOutcome = Ok | Abstained{reason} | Failed{reason} | Denied{req_id} | Timeout | NotRun{why}` is a
closed vocabulary demanded by F5, and `Abstained` is not a flavour of `Ok` (§8.2). `SegmentMark` renders to
nothing and exists in v1 solely so §19's futures have something to address.

| § | statement |
|---|---|
| **4.3** | The token ledger, which is what makes append-only structural: |

```
TokenLedger = [ (item_id, tok_offset, tok_len, h_k) ]
   h_k = H(h_{k-1} ‖ tokens(item_k)),   h_{-1} = H(tokens(stable_prefix))
   tokens : Vec<llama_token>, memfd-backed, append-only
```

**There is no rewrite operation to call** - request N+1 is `(region, 0, len_{N+1})` and request N was
`(region, 0, len_N)` over the same memory, so a prefix violation is not expressible. The unit is the
server's unit; the hasher is fed forward only and `h_k` is the cache identity; a periodic full re-hash
catches renderer non-determinism; and the ledger is persisted so a restart replays spans rather than
re-rendering. It supersedes a 32-bit rolling hash over the message object, which cannot see divergence
introduced downstream in the encoder.

| § | statement |
|---|---|
| **4.4** | Persisted: `session`, `transcript`, `transcript_item`, `stable_prefix` (content-addressed and shared across sessions with the same one), `event`, `adjudication_request`/`adjudication_decision` (never deleted), `turn_metrics`, `tool_spill`, `segment`. Derived: the head snapshot (§13.2), rendered markdown, every aggregate in §6/§18, the request body. Not stored: the raw response envelope, because keeping it invites a replay that diverges. Three rules from the closest prior art: every payload is in an entry, a bound value or the ledger, with no third place; **pending** rows hold content durable before it has a place in the tree, deleted in the transaction that places it; and a compaction is a self-contained checkpoint with a complete retained tail. |
| **4.5** | Events, one per thing a head must see, all with `(session_id, seq, ts)`: `TurnStarted`, `PromptProgress`, `Delta`, `ToolCallProposed`, `DecisionRequested`, `DecisionAnswered`, `ToolStarted`/`ToolProgress`/`ToolFinished`, `TurnFinished`, `TurnInterrupted`, `TranscriptAppended`, `HeadAttached`/`HeadDetached`, `Warning{code, detail}` (§18's assertions land here) and `Explain{turn_id, plan}`. `Delta` carries **only the increment**, which is half of "render cost independent of output length" (§13.3 is the other). |

## 5. The prompt pipeline

| § | statement |
|---|---|
| **5.1** | Stable prefix → items 0..k → **render** (our renderer, control tokens verbatim, no jinja) → **tokenize** (appended to the vector, nothing earlier re-rendered) → ledger append and hash chain → **submit** → progress, deltas, tool calls from our parser, usage and timings → post-flight assertions → append. **Generated tokens are appended as tokens, not re-derived from text**, so I1 holds by construction with no detokenize round trip where a whitespace difference would creep in; and **parsing is ours too**, so `--reasoning-format` and the grammar-per-model layer are not in the path. |
| **5.2** | The stable prefix is system text plus normalized tool schemas, frozen behind a fingerprint and content-addressed. **A changed prefix is a cold start** - correct, but not incurred needlessly: **nothing volatile goes in the prefix** (adding one line to an `<env>` block once cost a full re-prefill of a 179k conversation) and **everything volatile arrives as an appended item**. Two content rules: **a prompt may describe how to use a tool but never what the data contains**, and **state the answer language**. |
| **5.3** | System prompt changes are appended (`in-history`): compare against **the last non-empty system node**, not node 0; equal emits nothing; different appends a `role: "system"` message at the tail; otherwise fall back explicitly, normalizing history back to single-system form. The default per model is the user-turn envelope (`<system-update seq=N>...`), still an append and still prefix-stable; both forms are one field, `system_update_mode`. |
| **5.4** | Reasoning is not "replayed", it is simply still there: no field to send back, no field-name detection, no capability probe, no template that can drop an earlier turn's thinking. Two obligations replace them - the renderer must place thinking blocks exactly as the model was trained to see them (§7.2), and an assistant turn with no visible text must still emit its turn-boundary control tokens (`content: ""`, never `null`, which would brick every later turn). |
| **5.5** | Fork, not truncate: anything that would shorten or rewrite history creates a **new transcript** with `parent_transcript_id` set, so a rewind stays reversible, the divergence is a first-class object with an id, and the old cache entry is not superseded. The cost is a re-prefill from the divergence to the end, `(n_total - n_common) / 441 t/s` here. |
| **5.6** | `Submit` over the control channel carries the shm region, `message_spans` and `segments` given rather than string-matched, the `prefix_handle` assertion and the `want` set. The fallback is `POST /completion` with a token array, which today's unmodified server accepts and which this tree runs on; it sets `cache_prompt`, `return_progress`, `timings_per_token` and `return_tokens`, and **no** `n_predict` cap - fields every surveyed harness had to bolt onto someone else's schema, one so badly that a long prefill raced a 300 s idle timeout. |
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

The two fail cases are `NoVisibleContent` and `ReasoningOnly`; a failed batch tells the model why **in the
tool result, not in a log**; retries are bounded, because an unbounded salvage loop is its own failure
mode; and a truncated turn is never reported as completed.

| § | statement |
|---|---|
| **5.8** | Steering: **inject at the next step boundary** - after the current generation completes or after the current tool batch settles - because queueing to the end wastes a generation the message was correcting and interrupting truncates a partial turn that must be kept for cache reasons anyway. A message flagged `urgent` (`ABORT` is its extreme form) interrupts at the next token with the partial output kept as `truncated: true`. Steering never rewrites or reorders the vector. |

## 6. `EXPLAIN` - per-stage observability as a feature, not as logging

| § | statement |
|---|---|
| **6.1** | Three of oracle's eight diagnoses in one evening were wrong for the same reason: no way to measure one stage alone, so a wrong answer could not be attributed. "Why did this request prefill 144,436 tokens with zero cache hits?" is a query-plan question and should cost one command. |
| **6.2** | Every turn records a plan; the cold-turn rendering is the deliverable - which prefix matched, where it diverged, what caused it, what it cost: |

```
Cache                      prompt_tokens 144,436   cached_tokens 0   f_keep 0.000
  divergence               at item 3 (System, origin=Update)
                           expected h 7a19c2…, computed h 91be03…
                           cause: stable prefix changed (0c81… → 55ad…), field <env>
  cost                     144,436 tok re-prefill ≈ 327 s at 441 t/s
```

`ExplainPlan` has no field list yet, so the event carries the plan untyped - deliberate, since a field list
invented before the counters exist would be a list of guesses.

| § | statement |
|---|---|
| **6.3** | The numbers come from `usage.prompt_tokens_details.cached_tokens` (a standard field fed from the server's own counter), `prompt_progress`, `timings`, `GET /metrics`, `GET /slots`, `Explain{turn_id}` (or the `-lv 4` log until S5b), and the ledger for the expected common prefix - and that last one is what turns a measurement into a diagnosis, since the server can say *what* matched and only the harness knows what *should* have. |
| **6.4** | Every stage gets its own counters: prompt construction, cache (expected vs actual prefix and the cause), prefill and decode **separately and always with their concurrency**, tool execution per call, permissions (asked, answered, by whom, after how long), and per-head render. |

## 7. Per-model dialects - renderers we own

| § | statement |
|---|---|
| **7.1** | A dialect becomes **a renderer** rather than compat flags, with five jobs - `render`, `render_incremental`, `parse`, `control_tokens`, `stop_tokens` - plus `system_update_mode`, `guards` and `template_sha`. Two invariants are the tests: **`render_incremental` agrees with `render`** (the append-only design rests on it, and being pure it is property-testable without a model) and **`parse ∘ render` is the identity on the round-trippable parts**. |
| **7.2** | Validation against the shipped jinja is a required CI check and the mitigation for the one real hazard self-rendering introduces - a silent divergence between our renderer and what the model was trained on. `POST /apply-template` runs the same parse as the chat endpoint and touches no slot, no sequence and no GPU: |

```
for each dialect D, for each fixture F in the corpus:
    ours   = detokenize(D.render(F))
    theirs = POST /apply-template {messages: F.messages, tools: F.tools, ...}
    assert ours == theirs        # exact string equality
```

Equal renders for every prefix of every fixture subsume the byte-prefix assertion. It is required (a
failing dialect does not ship and a failing model is not admitted, with no "compat quirk" hatch), runs on
every commit and on every `template_sha` mismatch, and the corpus must cover the shapes that break: a plain
turn, reasoning, reasoning interleaved with two tool calls, a tool result, an *empty* assistant turn, a
system message at index > 0, an image, a very long turn, and each truncated at every prefix boundary. A
legitimate exception is a named, reviewed field with the eval that justifies it. Adopting a template change
forks every live transcript on that model (§5.5).

| § | statement |
|---|---|
| **7.3** | The three targets: **GLM-5.3-Flash** (thinking family `zai`, 262,144 per slot against a 327,680 elastic pool, MTP draft at `n_max=2`, vision via `--mmproj`, and a repeating-token collapse past ~78k that poisons the slot, so §8.5's guard is built regardless); **Qwen Flash** (`qwen`, where thinking blocks for prior turns are emitted, always); **Qwen 3.8 27B dense** (a model file and no preset, which is a prerequisite - D7 says derive it by measurement). The model is a property of the **session**, not the turn: a switch evicts the other model and costs 29-31 s, so changing model **forks** the transcript and the client batches same-model work. |
| **7.4** | Adding a fourth model: a preset, a dialect or a declared reuse, the **`/apply-template` diff as a hard gate**, the two property tests, a short generation plus one `n_predict: 1` call to confirm `finish_reason` classification, then the dialect-parameterized suite. If the diff fails, fix the renderer or patch the template - never ship a divergence. |

## 8. The tool contract

F4 says close the loop and F5 says never let a component's "I did not do this" be reported upward as
success; a tool's failure mode is a design decision whose cost is paid in permanent context.

| § | statement |
|---|---|
| **8.1** | Six clauses, and a tool that misses one does not ship: (1) **a miss is self-correcting in the SAME call** - a `grep` that finds nothing reports where the term does occur, a `find` that misses returns the surrounding listing, a too-strict anchor is relaxed and says so; (2) **malformed input is salvaged, not rejected**, with a note of what was repaired; (3) **outcome is a closed vocabulary and abstention is not success** (§4.2), *structurally* distinguishable in what the model sees; (4) **read/write is declared in the schema**, `access: read | write | exec | network`, which is what §11's policy keys on; (5) **output is bounded and spilled, never truncated**; (6) **the description says how to use the tool and never what the data contains**. |
| **8.2** | Abstention cannot be papered over: the outcome class is carried rather than merely worded, so `NO_RESULT` is a distinct token sequence; abstention propagates upward, so a subagent whose calls all abstained cannot return `Ok`; and a turn with no visible content fails (§5.7). It does not judge whether the prose is supported by the output - that is an eval. |
| **8.3** | Spill, never truncate: `max_inline_bytes` with **no default** (unset is a genuine no-op, and D6 makes the threshold a *decider* whose simplest form is a fixed count), a preview budget of `cap - reserve` so `preview + "\n\n" + notice` never exceeds the cap, a head/tail split, the notice `(Omitted <N> bytes. Full <kind> result stored at: <locator>. <hint>)`, and the original written `0600` with a storage failure falling back to inline. Ours adds a content hash into `tool_spill` plus `read_spill(hash, range)`, so the hint is actionable. |
| **8.4** | A hard ceiling, because past ~5-7 MCP servers small models get *worse* at choosing tools: `orchestrator` (6), `coder` (7), `researcher` (6), `reviewer` (5), with the daemon **refusing to start a session** whose resolved count exceeds `max_tools` (default 8) and MCP servers attached per role. It is also the argument for subagents, and the naive argument is wrong: **subagents pay off through context hygiene, not decode speed** (F2), so concurrency is a configured number in `EXPLAIN`. |
| **8.5** | Two guards that cost nothing: a rolling n-gram check turning GLM's `@@@@...` collapse into `Warning{code: repetition_collapse}`, and an explicit warming state with no idle timeout below ~120 s that counts `prompt_progress` chunks as liveness. |

## 9. Context occupation vs cache stability - the tension, resolved

F1/F2 say the prompt must be append-only and byte-stable; F3 says occupation defocuses. Reasoning measured
~51.7% of a conversation's content, so replaying it roughly doubles occupation.

| § | statement |
|---|---|
| **9.1** | **Keep the prompt append-only and byte-stable, make what goes into it dense, reclaim occupancy at compaction on a schedule.** In priority order: (1) **nothing enters the transcript that does not have to be there** - no restated framing, no re-injected reminders, no narration recorded as an item; (2) **reasoning is replayed verbatim, and that is not negotiable while llama.cpp caches generated tokens** - dropping it produced the 612 GB and the 78,598-token p99; (3) **occupancy is reclaimed by compaction (§10), scheduled and cheap**, which is what makes (2) affordable; (4) **depth is preferred over premature compaction**, since a 150k warm conversation costs about the same per turn as a 15k one, so the wall is the window and the memory budget plus §9.3's signal. |
| **9.2** | Reasoning is the *right* half to keep: it is what the model produced to reach the answer, the part a summary reconstructs worst, and not something the harness can make denser. Tool output is the opposite on all three counts, which is why §8.3 spills and §10 compacts tool-heavy segments first. |
| **9.3** | **Falsifier A**: replay on and off, scoring quality - if quality is better without replay, reasoning is noise and the answer becomes a server change. **Falsifier B**: quality against depth at 20k, 60k and 150k - if it falls off a cliff well before the window, §9.1 clause 4 is wrong and compaction must be early and aggressive; this is milestone M3.5 (§17). **Falsifier C**: if the summarizer cannot hit the warm prefix (§18.1-I5), clause 3 is unfunded and depth must be capped lower. |
| **9.4** | Retrieval stays out: `ask_corpus` / `ask_code` / `search_corpus` are tools and their abstention is §8.2's. The one carry-across is negative - **the harness must not silently "improve" a tool's query**, and if it ever does, the rewrite must be visible. |

## 10. Compaction, as leveled compaction

| § | statement |
|---|---|
| **10.1** | The surveyed default is a stop-the-world full pass sharing a prefix with nothing (144,436 tokens, cache hit 0, ~13 minutes), which is pre-LSM compaction. The mapping is L0 = recent turns verbatim (the memtable), L1 = a segment summarised once, L2+ = older segments merged coarser, running incrementally in the background, with write amplification becoming **fidelity amplification**. It does not make the stall faster; **it removes it as a category**. |
| **10.2** | L0 is the last N turns (default 8) verbatim, where the cache is warm; L1 the last 3 closed segments at ~1:4; L2 older segments at ~1:4 of L1; Lc one running "what this session is about" paragraph plus an index of labels and spill locators, never discarded. **Levels are monotone in age but not merged across a live boundary**, because `last_touched` moves a segment and `created_at` does not - which is "different decay rates" (§19.1) falling out of the level rule - and **fidelity amplification is bounded by construction** at one re-summarisation per level over 4 levels. |
| **10.3** | Warm compaction, the original work: **the summarizer request is the last routed request, byte-for-byte, plus one appended user message**, which is literally `render(stable_prefix, items[0..k]) ++ render(instruction_item)` because the transcript is append-only and ledger-chained. Expected `f_keep` ≈ 1.0 and prefill ≈ the instruction's own length (§18.1-I5). A segment summary names the span by its `SegmentMark` ids and keeps the full live prefix, so it costs the same as a tail summary. |
| **10.4** | **Soft compaction (default)** appends the summary as an in-history update or a marked user item: zero prefill, no tokens freed, attention pressure reduced. **Hard compaction (at the wall)** forks the transcript into `[stable prefix][Lc][L2][L1][L0]`, costing one cold prefill of the shorter prompt. Recent verbatim goes before the summary and project instructions are re-injected verbatim. Soft runs when a segment closes and the box is quiet; hard only at `high_water` (default 80% of `n_ctx`), by which time every segment already has a summary - which is what removes the stall. |
| **10.5** | The cache ladder and compaction have the same shape and **stay independent, sharing vocabulary rather than policy**: the ladder trades restore cost against bytes and a degraded entry still restores, while a coarsened segment does not. Coupling them would let memory pressure degrade answer quality, where pressure should cost latency and never fidelity. |
| **10.6** | Recent verbatim before the summary, and reasoning kept in the retained tail, so the prefix cache stays warm. |

## 11. Adjudication - one interface, three adjudicators

| § | statement |
|---|---|
| **11.1** | > *"we are flexible, it is ok to ask me, it is ok to use firecode and it is ok if i plug a model for auto mode."* A pluggable layer, not a doctrine: **boundary** decides by structure, **human** by judgement over flowy or a guest's `--ask`, and **model** by a separate, probably smaller model for the middle band. All three are first class and interchangeable; the volume of human asks is an **observable, not a constraint**. |
| **11.2** | One decision shape, which is what makes the three swappable and the audit uniform: `AdjudicationRequest{id, session_id, turn_id, tool_call_id, agent, action{class, tool, summary, arguments, arguments_digest}, context{run_id, workspace, boundary_facts, prior_decisions_for_this_class}, kind: Permission|Question, options[{id, label, kind}], deadline, on_timeout}` and `AdjudicationDecision{request_id, outcome: Selected{option_id} | Escalate{to, why} | Unavailable | Cancelled | Timeout, by, basis, at, latency_ms}`. *A throwing or non-conforming answerer becomes `unavailable`, never silently opens the gate*, and `Escalate` is first class rather than an error path. |
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

A whole band moves between adjudicators with one edit; a class never seen in the log is one nobody has
thought about, and §6 lists observed classes with counts; and `boundary` rows cost nothing at runtime.

| § | statement |
|---|---|
| **11.4** | The boundary adjudicator decides by structure: the guest sees a copy of one project and nothing else of the host; `--add-dir`/`--workdir` **refuse outright** to carry `.ssh`, `.gnupg`, `.aws`, `.kube`, `.config/gh`, `.password-store` or a browser profile out of the home; the host project is not modified by a run not asked to write back; project paths come from the spawn server's config; and lifetime is a cgroup. It does **not** protect API credits or the network, which is why `metered` and `network,external` route away from it. A denial **escalates rather than hard-failing**, because the common case is a call that is merely misaddressed. |
| **11.5** | The human adjudicator, kept precisely because the human path is rare and a rare ask that silently hangs is worse than a frequent one: **post and wait**, then **if nobody answers say so and tell the agent to decide and record what it chose** (`AgentDecides` for a question, `Deny` for an irreversible effect); **acknowledge before you act**, since silence is indistinguishable from absence; and **start listening before you start working**, so `SessionStarted` waits for a live subscription and a lost one raises `Warning{code: no_human_channel}` and *changes the effective policy* (not raised anywhere in this tree yet). The primitive is a **visible row, not a held connection**, self-clearing on any relevant write by the named person. Delivery: first valid answer wins with a later one recorded `late`, authority checked against the session's `approvers` by authenticated actor id, cancellation as withdrawal, and every step an immutable row never in the model transcript. |
| **11.6** | Permissions and questions are one mechanism, differing only in `kind` and in the default on no answer - one mechanism because the volume is low and because §19.2 is a question, not a permission. |
| **11.7** | The model adjudicator sees a **bounded brief and never the conversation** (class, tool schema, arguments with anything over 2 KiB spilled, boundary facts, the stated goal, the last N decisions in this class, the options) - not the transcript, for cost and for independence. In the tree this is `ModelBrief` in `crates/tools/src/authorise.rs`, which also carries the operator's past answers as worked examples, the agent's claim as evidence and never a citation, and the gate's prior record as evidence and never a promotion. It returns the same decision constrained to `{option_id | "escalate", basis}`; not the session's model (a genuine second opinion, and no seat contention - the 27B dense as a second router entry, UNVERIFIED-7); unavailable routes to `on_timeout` and never a silent allow; unsure escalates. Audit: every decision stores the brief digest, raw output, option and basis, **shadow mode** decides without applying and records both answers, and `EXPLAIN` shows counts, allow rate, escalation rate and shadow agreement per class. |
| **11.8** | What firecode must provide: D3 answered the first two (a parent cgroup is use-dependent, and the persistent shell was withdrawn for byobu) and the rest is one request - give the room the request/response object §11.2 needs (ids on asks, a deadline and stated default on `--ask`, an `--ack` verb, a per-run room or a run tag, structured options) and the flowy bridge becomes a mapping. |
| **11.9** | opencode's `permission.ask` has zero trigger sites, a dead hook; what is worth taking is its permission *model* and one discipline - the `never` policy is enforced inside the service before dispatch, so a `deny` row in §11.3 is evaluated before any adjudicator is consulted and none can override it. |

## 12. What flowy must provide

| § | statement |
|---|---|
| **12.1** | flowy already has projects, rooms, DMs, a single-holder named-reader inbox, attachments, a WebSocket for live sessions, task delegation, announcements with `ack`/`resolve`, `todo waiting-on`, ULIDs, HLC ordering and an MCP server, with an authenticated `actor` ULID that satisfies §11.5's authority check. Three corrections: **SSE exists but chat is not on it**; **there are three wake levels, not four**, with an *unaddressed* broadcast waking you inside `addressed`; and **per-room policy does not exist and cannot**, because the level is per-poll query params and a reader belongs to a name. |
| **12.2** | The asks: (1) **a `decision` object** (`POST /api/decision` with options, deadline and `on_timeout`; `GET`; `/answer` exactly once with a second answer refused naming the winner; `/ack`; `/withdraw`) - *nothing else here matters as much*; (2) options as choices; (3) server-enforced deadlines; (4) delivery receipts, without which "not answered" is indistinguishable from "never arrived"; (5) multiple readers per seat or a reader namespace; (6) **a pushed inbox stream** at `GET /api/inbox/stream?as=NAME&addressed=&mentions=&focus=&room=`, same `wakesFor`, same reader row, same heartbeat, one JSON message per SSE event, plus a fourth `waiter_kind` and `LISTEN`/`NOTIFY` to remove the 250 ms tick; (7) updatable message content; (8) idempotency keys; (9) cross-project reach, routing by actor or returning an explicit error rather than accepting a write that delivers nothing; (10) optionally agent-to-agent RPC. |
| **12.2b** | Per-room policy is ours, since the one-flag limit follows from one reader per name: a per-room subscription row `(room, level)`, flowy's three definitions kept exactly, the one-flag shape dropped, and the **human-broadcast clause made an explicit named option** because it has failed in **both** directions. |
| **12.3** | The degraded v1, so flowy is never a blocker: a DM with `[decision <id>]` and one option per line, a strict answer grammar with a reply saying what was expected, the harness's own ack, harness-side deadlines, and `no_human_channel` falling back to `GET /api/presence` - labelled as the proxy it is. |
| **12.4** | flowy and the firecode room stay **separate, bridged by harnessd on the host**, because a flowy token inside an untrusted guest would grant it a seat's voice and because the room must keep working with no harness at all. The bridge is a host-side loop; our own asks never go through the room. If flowy ever supports a **capability token scoped to one run, with no write authority beyond one decision id**, unification becomes the better answer. |

## 13. Multi-head attach and detach

| § | statement |
|---|---|
| **13.1** | There is no prior art: nothing surveyed lets two clients concurrently observe and drive *one* live conversation, so "tmux for an agent" must be designed. |
| **13.2** | The daemon holds the session and a head sends `ATTACH {session_id, since_seq, identity, caps}`, with the event log (§4.5) authoritative. **Late join** gets a snapshot then events from S+1; **resume** gets the gap, or `RESYNC` past a bound (5,000 events or 4 MiB), which is a normal outcome; **fan-out** demotes an overflowing head to resync rather than dropping or blocking it, so *the model stream is never blocked on a head*; **two heads at once** serialize on a per-session queue with `expected_seq` and `client_request_id`, and a mid-turn prompt queues as a follow-up user item with the queuing announced; there is **no input lease**; **all heads detaching mid-turn does nothing**, since **TCP close is detach, never abort**; and a **restart mid-turn keeps the partial assistant output as a real transcript item**, because those tokens are in the server's cached entry and discarding them guarantees a divergence. |
| **13.2b** | Nine mechanics taken from flowy rather than re-derived: a cursor acked **after** the client has written out; the mark advancing over everything **read**, not everything delivered (or a filtering consumer rereads its own output forever); **reporting what was filtered out**; one waiter per name with the refusal naming the pid, so **several heads share ONE reader**; **idle means quiet, not unwatched**; bounded scrollback that **discloses the drop** with the count present-and-zero; snapshot and register under one lock; non-blocking fan-out; and **scrubbing the stored copy of anything interactive**, so a late head sees settled decisions as outcomes and a queued reply is discarded rather than held. Not to copy: exit-per-message delivery, the forked-successor handover, a poll behind an SSE facade, filter policy stored nowhere, and preferences in an untyped note. |
| **13.3** | Render cost independent of output length in two halves: events carry increments only (§4.5), and the head lexes incrementally from a stable block boundary (the last `\n\n` with balanced fences), licensed by `lex(prefix) ++ lex(tail) === lex(prefix+tail)`. Never rebuild every child from the whole accumulated text per delta. |
| **13.4** | The heads: **TUI** over the socket (filesystem permissions as the auth), **remote** over WebSocket+TLS with the same frames, **flowy** as an in-daemon connector holding a push subscription, **ACP** as stdio JSON-RPC. ACP is an **adapter, not the protocol**: its permission objects have the right shape and no durability (no timeout, no request id in the body, cancellation as a returned value, a non-exhaustive outcome enum), so adopt its vocabulary in §11.2 and keep our own `{id, deadline, on_timeout, audit}`. |

## 14. Ported conformance suite - milestone zero

A test encodes the bug and the invariant, not the design, so it ports across languages. The suite exists
before the harness does, and it is what tells us a milestone beat opencode rather than felt like it. It
lives in `tests/fidelity` - the `/apply-template` gate, its corpus and the oracle.

| § | statement |
|---|---|
| **14.1** | What to take. **grok-build (Apache-2.0)**: the prefix-stability suite, whose `assert_prefix_stable_pair` asserts `ext_input[..base_input.len()] == base_input` on the serialized `input` array, reports the first divergent index, and deliberately does not compare `model` or `tools` - and our equivalent must go through the real request builder and dialect encoder or it proves nothing. Its twelve tests cover the baseline, an identical injected reminder, reasoning-as-sibling ordering, a tool-schema change, a model switch, synthetic user messages appending, **image pruning** (the documented exception), small images preserved, a budget pass on the outgoing copy, tool-result pruning on a clone, server-executed calls holding a wire position, and resume reproducing byte-identical output - the three exception tests mattering as much as the nine strict ones, because they encode where the invariant may break. Also its length-policy family (a `length` stop is a hard failure unless salvaged, a rate-limit error is never "truncated but complete", a completed tool_use keeps `Length`, the salvage cap, the max-of-text-and-encrypted estimate). **omp (MIT)**: the reasoning-replay contract as one test, its append-only-context family (deep rewrite, metadata-only rewrite, provider-payload rewrite, message 0, byte-equality rather than reference-equality as the contract), and its thinking round-trip. **pi (MIT)**: *"should not execute tool calls from a length-truncated assistant message"*. **deepseek-harness (MIT)**: the system-prompt projection tests, `content: ""` never `null`, and the cache-accounting split that degrades rather than emitting a wrong total. **opencode (MIT)**: port with care and **read the assertions, not the names**, since three overflow tests are named `BUG: ...` and assert the correct behaviour. **oh-my-openagent is excluded** - Sustainable Use Licence. **flowy (the operator's own; D4)**: delivery tests encoding bugs this fleet has paid for, every one of which our per-room subscription (§12.2b) could reintroduce, plus its author-test inversion - assert the wire format carries everything the persisted model carries **minus a named exclusion list** computed by reflection, the antidote to the whole enumerating-renderer bug class. |
| **14.2** | Licence mechanics: no surveyed repo stamps SPDX on individual test files, so attribution must be added - a suite-level `NOTICE` (repo, URL, SHA read, licence, files) and a per-file header with the SPDX id, upstream path and range, SHA and what changed, which is also how you find what a failing ported test asserted. MIT and Apache-2.0 coexist and neither is copyleft; D8 settled the harness as Apache-2.0, and the suite should be Apache-2.0 in its own directory regardless. |
| **14.3** | The checks that run against a real server plus *any* client, carried by a recording proxy and `POST /apply-template`: |

| # | check | how |
|---|---|---|
| C1 | prefix extension, exact | request N's prompt must be an exact prefix of N+1's, as `messages` for an unfused client and as the token array for ours; ours holds by construction (§4.3), so the test measures other clients and catches a ledger regression |
| C2 | renderer fidelity | `/apply-template` per fixture, exact string equality against our renderer; the required CI gate |
| C3 | generation-inclusive prefix | `H(prompt_N ‖ committed_generated_N)` must re-hash as a span of N+1's prompt; a mismatch is `Warning{prefix_divergence}` and the only defect in us (§18.1-I1) |
| C4 | `f_keep` distribution | `lcp / cached_entry` over a scripted 30-turn session, reporting p10/p50/p99 and p99 re-prefill (D11: not `cached/prompt`, which is `f_sim`) |
| C5 | reasoning replay | the omp scenario end to end: `cached_tokens` must not collapse on the turn that adds a prior assistant turn plus a nudge |
| C6 | `content: ""` never `null` | on a tool-call-only assistant history turn, `content === ""` |
| C7 | truncated tool args are not executed | `max_tokens` low enough to cut a call; the client refuses and re-prompts |
| C8 | `finish_reason` handling | `max_tokens: 1`; the client surfaces `length` rather than success |
| C9 | in-history system prompt | change the system text mid-session; message 0 unchanged, a new system message at the tail, `cached_tokens` high |
| C10 | cache accounting is disjoint | "new input" tokens equal `prompt_tokens - cached_tokens` |

C3's token-count form, `cached_tokens(N+1) >= prompt_tokens(N) + committed_generated_tokens(N)`, is a
**measurement of the server**: a hybrid/recurrent model undershoots by design because llama.cpp resumes
from a context checkpoint, so a shortfall is not a violation.

| § | statement |
|---|---|
| **14.4** | What no suite covers and we write: warm-prefix compaction (I5), `prompt_progress`/`timings` and the idle-timeout interaction, adjudication (routing, escalation, timeouts, first-answer-wins, late-answer rejection, audit, shadow agreement), multi-head, renderer conformance over every dialect, and `EXPLAIN` correctness against an induced divergence. |

## 15. What to vendor or port, and from where

Licences: grok-build **Apache-2.0**; omp, pi, deepseek-harness, opencode **MIT**; oh-my-openagent
**Sustainable Use, excluded**. From **grok-build**: the prefix-stability test first, the
`ConversationItem` shape with `Reasoning` a sibling, `LengthPolicy`/`empty_reason()`, and the compaction
assembly order with recent before summary. From **deepseek-harness**: the in-history system prompt, warm
summarizer construction, spill-don't-truncate, `content: ""` never `null` **and the comment**, and the
README convention (§18.4). From **omp**: incremental markdown lexing, and `append-only-context` as a
reference whose 32-bit digest §4.3 supersedes. From **opencode**: the Part union as the **Journal's** type
(§4.1), plus schema shapes, tool bodies and the permission ladder as starting points. From **pi**: steering
into a running loop.

**Deliberately not taken**: grok-build's transport layer, where no `extra_body` means `return_progress` and
`cache_prompt` cannot be sent, a closed `FinishReason` means an unexpected value fails deserialization, and
no unknown-field capture makes `timings` invisible - exactly what a local-first harness needs. Two rules
adopted from those omissions: every wire enum has an `Unknown(String)` fallback, and every streamed delta
captures unknown fields, so a field the server adds tomorrow is visible today.

## 16. Language and runtime

The requirements: byte-exact serialization and cheap hashing; a long-lived daemon with concurrent streams,
backpressure and cancellation; fan-out without head-of-line blocking; exhaustive sum types; SQLite; a good
TUI; and portable tests.

Python is out for the daemon (a multiplexing daemon under the GIL is the wrong shape, and the ecosystem
pulls toward convenience where this design needs canonical bytes) and right for the suite. TypeScript is
fastest to working code, but `JSON.stringify` key order is insertion-order dependent - a discipline rather
than a guarantee. Go is an excellent daemon language whose cost is that its data model is almost entirely
sum types and interface-plus-type-switch drops exhaustiveness checking.

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
| **M0** | The conformance suite with no harness: proxy, corpus, `/apply-template` gate, then C1-C10 against opencode | a baseline of `f_keep` p10/p50/p99, p99 re-prefill and C1-C10 pass/fail |
| **M1** | `harnessd`, one head and read-only tools, entirely on the token-array fallback: reasoning present because we emit the tokens, an append-only vector with the ledger, no output cap and `length` acted on, incremental render, real progress from `prompt_progress` | C1-C10 pass where opencode failed, and `f_keep` p10 >= 0.99 over a scripted session |
| **M2** | firecode integration and write tools: run mapping, `vm_in`, the tar channel, spill, the `boundary` adjudicator with §11.3's table | the host project unmodified, a destructive `bash` harmless, and a row for every gated call |
| **M3** | the flowy connector, chat first: a push subscription or the degraded reader, inbound messages as bare user items (I11), steering at step boundaries, decisions over §12.3's protocol - *before* M4 because the async decision shape cannot be retrofitted | a real decision answered from a phone, and `no_human_channel` exercised by killing the connector |
| **M3.5** | Falsifier B, one measurement at 20k/60k/150k of context | a number, and a written decision to keep or revise §9.1 |
| **M4** | leveled compaction, high in the phasing on purpose | I5, and no stall longer than one cold prefill of the compacted prompt |
| **M5** | multi-head, plus the ACP adapter | the §14.4 suite, and a turn surviving every client closing |
| **M6** | concurrent subagents, the first time this box's scheduler is asked for more than one seat by a real client | seats > 1 under load in `/metrics` and `/slots` |
| **M7** | the model adjudicator in shadow, promoted per class | agreement per class, and a documented promotion |
| **M8** | the other two models: probe, admit, run the dialect-parameterized suite | the suite green on both |

**Server track** - same release, its own build loop, none of it landed and none blocking a milestone because
§5.6's fallback works: **S0** the control channel and shm submit path; **S1b** `Spans`; **S2b**
`Admit`/`Stats`; **S3b** `SegmentHint`; **S4b** `Prewarm`; **S5b** `Explain`; **S1** the matched entry's
`lcp`/`f_keep`/`f_sim` in `timings` (highest value, lowest risk, first); **S2** age-based demotion; **S3**
the eviction-ladder tests; **S4** the block-hash index; **S5** the recompute-fraction experiment; **S6** the
prefix-handle protocol with `required` early; **S7** stitching, only if S5 says so.

**The smallest thing that beats today's opencode setup** is M0 + M1, and neither needs a line of C++. M1
removes all four measured defects: the cache stops growing without bound, the tail collapses from 78,598 to
~5,000 tokens of re-prefill, an empty subagent becomes a failure, and a long prefill shows a progress bar
instead of a hang.

## 18. Test strategy

**I1, the generation-inclusive prefix invariant**, is the real one: > Request N's prompt **plus the generated
tokens the harness committed in turn N** must be a prefix of request N+1's prompt. Grok pins only
prompt-to-prompt, but llama.cpp caches generated tokens too, so a harness can satisfy Grok and still diverge
- which is exactly what the 612 GB was. The generation term is **committed** tokens, not the server's
`predicted`. Two forms: **exact**, hashing turn N's prompt with its committed generation and re-hashing that
span of N+1's, post-flight on every turn, raising `Warning{code: prefix_divergence}` into
`TurnMetrics.prefix_check` - and because it needs only token ids we rendered, the gate is the backend's
`may_assert_structural_prefix`, which over a `messages` API skips **loudly** (`prefix_check_skipped`,
`held()` still false) rather than passing green; and **token-count**, `cached_tokens(N+1) >= prompt_tokens(N)
+ committed_generated_N`, whose shortfall is a measurement of the server (`cache_reuse_shortfall`) and not a
divergence.

| id | invariant |
|---|---|
| I2 | exact prefix extension of the token array - the grok helper over token ids through the real renderer and tokenizer; it holds by construction (§4.3) and the test exists because that is a claim about code someone will edit |
| I3 | renderer fidelity - `/apply-template` over the whole corpus, exact equality per dialect; required CI (§7.2), and it subsumes I2's template side |
| I3b | `render_incremental ≡ render` and `parse ∘ render ≡ id` - pure functions, property-tested |
| I4 | the ledger is a chain - a periodic full re-hash equals the incremental head |
| I5 | compaction is warm - the summarizer's own `f_keep >= 0.99` and its prefill bounded by the instruction |
| I6 | the stable prefix is stable - its fingerprint changes only on a declared event, which forks |
| I7 | `finish_reason` is never dropped, and an empty `length` turn always fails (§5.7) |
| I8 | abstention does not become success, including across a subagent boundary |
| I9 | no adjudicator can override a `deny` row - evaluated before dispatch |
| I10 | a head cannot block the model stream - a stalled head resyncs and latency is unchanged |
| I11 | an inbound chat message costs exactly its own tokens - one user item, no envelope |
| I12 | no polling anywhere - no timer or poll loop for delivery, and `no_human_channel` from a dropped subscription |

| § | statement |
|---|---|
| **18.2** | Measured as `f_keep` = `lcp / cached_entry`, p99 re-prefill = `prompt_tokens - cached_tokens`, rates and draft acceptance from `timings`, progress from `prompt_progress`, and box state from `/metrics` and `/slots`, over **a scripted 30-turn session** that includes a mid-session system change, an oversized result, a truncation, a compaction crossing and a fork. Traps already paid for: **a tok/s number without its concurrency is not a number** (report `n_busy_slots`); **two conversations cannot test residency**; **a captured pid is a proxy**; **`${X:-d}` substitutes on empty as well as unset**; **a full model load looks like OOM**; and **with `--no-op-offload` the CPU is on the inference path**, so a `-j 24` build steals prefill. |
| **18.3** | Property tests: random transcripts asserting I2/I3b on every adjacent pair (pure functions, so full volume on every commit - the biggest single testing win of self-rendering), random tool outputs asserting `len(preview + notice) <= cap`, and random policies asserting every action matches exactly one rule. |
| **18.4** | Culture: the README convention verbatim per prompt-contributing feature - **what the model sees**, **token effect**, **KV cache effect**. |

## 19. Future directions, and what they demand of v1

Not v1 features, but each constrains the v1 data model, and all three want **context as addressable
segments** - which is why `SegmentMark` (§4.2) and the `segment` table (§4.4) are in v1.

| § | statement |
|---|---|
| **19.1** | Multi-head compaction with different decay rates. **L1**: compaction must never be a whole-history operation, or everything downstream assumes one summary. **L2**: a segment carries `level` and `last_touched`, and `last_touched` is what promotes it - the whole of "different decay rates", for two columns and a zero-width item. **L3**: summaries live per segment in the journal, so a segment can be re-levelled without re-summarising. |
| **19.2** | User-assisted compaction. **L4**: compaction can ask a human mid-operation over §11.6's one channel. **L5**: **a segment is labelled at creation** - reconstructing labels for 200 old segments means running a model over the whole history, which is the stop-the-world pass §10 exists to abolish - so the label is the first ~80 characters of the opening user message plus the tools used, replaceable later at no prefix cost. |
| **19.3** | Agentic memory. **The cost model**: a swap re-prefills linear in the suffix - ~11 s at the tail, ~170 s mid-way through 150k, ~453 s at the head of 200k - so swappable memory is **not free**, and **the recommendation is: append, do not swap**, since a retrieved memory appended as a new item is free and prefix-preserving and §10's levels have usually reclaimed the occupancy. **L6**: segments are addressable and independently serialisable, with rendered bytes a contiguous ledger span - the one v1 decision a later server-side change depends on. **L7**: the pipeline takes an *ordered selection* of items, always "all of them" in v1. What would slam the door is rendering from a message array without a span map, or compaction rewriting in place instead of forking. Memory is **built on the oracle pattern** - a searchable corpus behind a tool, which keeps the harness out of retrieval and lets a search that finds nothing abstain structurally - with one harness-side addition: **segment-to-corpus ingestion**, so a demoted segment's verbatim text stays retrievable behind `search_session_memory` while the summary stays in the prompt. Flagged disagreement: the brief accepts that agentic memory fights append-only prompts, and it is costed above, but segment swapping should not be built into v1 or v2 here - decode is flat against depth (F2), the pool is elastic and the L2 tier makes a cold conversation cheap to bring back. **Stay deep and stay warm**; if Falsifier B shows quality collapsing with depth, that inverts. |

## 20. Risks and open questions

| § | statement |
|---|---|
| **20.1** | Unverified, with the procedure that settles each. Those that change a decision: **UNVERIFIED-16** (can stitching work on GLM's 34 recurrent KDA blocks? the highest-value open question here, settled by §3.10-C's per-layer-class experiment), **-13** (`--cache-reuse` inertness - grep the startup log), **-15** (is tokenization a measurable share of warm latency? wall clock minus `timings.prompt_ms + predicted_ms`), **-7/-8** (auto-mode model and the 27B dialect - M7's shadow and the §7.2 diff), **-14** (the depth-collapse guard does not reproduce to 147k; build it anyway). Cheaper: **-1/-2** are *dissolved* rather than answered, since under self-rendering no template can drop reasoning or a mid-history system message, leaving a behavioural eval with §5.3's envelope as the default; **-2b** by §7.2; **-3** by measurement, and do not build checkpointed KV in v1; **-4** by checking the tag before repeating the 32k claim; **-5** is no longer a dependency of I1; **-6** by one call, or by running the fidelity gate against a directly launched server; **-9/-10/-17/-18** by asking whoever owns flowy; **-11/-12** by measuring the exec and file channels; **-19** is settled by D4. |
| **20.2** | Risks. **No cloud fallback, and the edge would have to be rewritten for one** - mitigated not by an abstraction layer but by D10's `crates/backend` seam. **Fusion couples us to llama.cpp's internals**, and the vocabulary is the part that cannot be ported. **Self-rendering shifts template drift onto us**, with §7.2's CI diff as the entire mitigation - if that check is ever made advisory this becomes the most dangerous risk in the plan, silent in failure and a cache leak in hundreds of gigabytes as its symptom. **A renderer could diverge from a template we cannot match**, and the honest answers are to fix the renderer or patch the template. **Rust is slower to write**, mitigated by reversibility. **Building on this box steals inference.** **flowy is a moving target and is not ours**, mitigated by §12.3. **Auto mode drifting** is the most dangerous new component, since a model adjudicator that slowly loosens is precisely F5 - mitigated by permanent shadow mode. **Segments turning out to be the wrong abstraction** costs two columns. And **the harness finally exercising concurrency could destabilise the server**, which is why M6 watches `/metrics` and `/slots`. |
| **20.3** | Corrections, flagged rather than smoothed: **the supersede rule is containment, not "exact full-prefix match"** (which strengthens F1a), and **line numbers in the surveyed repositories drift**, so re-check a citation against the checkout before repeating it. |
| **20.4** | What I could not settle: **whether §9's position is right**, with §9.3's three experiments as the way to settle it and Falsifier B load-bearing; **whether the volume of human adjudication will be tolerable**, the policy table being a first guess and configuration precisely because it will be wrong; and **whether one daemon is enough** for one deep orchestrator plus a dozen to thirty shallow sessions, which nothing forbids but nothing has been sized for either, the server-side arithmetic saying the binding constraint is a third GPU. Revisit at M6. |
