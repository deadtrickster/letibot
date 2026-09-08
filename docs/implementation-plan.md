# Implementation plan — a local-first agent harness for lab2x1

Written 2026-09-08. Builds on `/home/dead/harness-survey.md` (measured findings, scorecard),
`/home/dead/Projects/glm-serving-notes/GLM-STATE.md` (the serving stack), `~/bin/glm-flash-server`
and `/home/dead/models/router-presets.ini` (the launchers), the four permissively-licensed
reference harnesses under `/data/scratch/harness-survey/`, and the operator's own `oracle` repo.

This is a plan, not code. Where I could not settle something from the sources it is flagged
**UNVERIFIED** with the procedure that would settle it; they are collected in §20.


**Contents.** §0 the thesis · §1 scope · §2 the facts · §3 architecture — fusion (§3.3 the seam,
§3.4 what it unlocks, §3.5 what it costs, §3.7 pushed chat, §3.9 firecode, §3.10 block addressing) ·
§4 data model · §5 the prompt pipeline · §6 `EXPLAIN` · §7 renderers we own · §8 the tool contract ·
§9 cache stability vs context occupation · §10 compaction · §11 adjudication · §12 what flowy must
provide · §13 multi-head · §14 the ported conformance suite · §15 what to vendor · §16 language ·
§17 phasing (harness and server tracks) · §18 test strategy · §19 futures · §20 risks and open
questions.

**Two operator directives govern this document**, applied throughout rather than noted once:
*"I want harness and server fused to the 11"*, and *"like Apple ecosystem basically, I don't care
about other providers or generalizations."*

**If you read four things:** §3.1 (what fusion changes — the harness renders and tokenizes, so the
token array *is* the invariant), §4.3 (the token ledger), §7.2 (the `/apply-template` CI diff, the
one guard self-rendering needs), and §17's last subsection (the smallest thing that beats today's
setup).

---

## 0. The thesis

> *"my goal is to have my llm runtime behave like a normal data app, don't see any reasons it
> can't."* — the operator, 2026-09-08

That is the spine of this document. Every requirement below hangs off it rather than arriving as a
list of grievances, because every failure measured on this box in the last two days is something
data systems settled decades ago and this stack has simply not adopted yet.

| what broke here | the data-systems name for it |
|---|---|
| cache entries never superseded — 291 entries, 612 GB, four OOM kills | an index that never merges duplicates |
| a byte limit on the cache, no limit on entry **count** | a buffer pool with no page limit |
| eviction spilled a whole entry or nothing | no graded replacement — no clock-sweep, no usage counts, no cost model |
| compaction is a 13-minute stop-the-world full pass | pre-LSM compaction; leveled/incremental compaction exists precisely for this |
| `finish_reason: length` parsed then discarded; an empty result recorded as success | swallowing an error return |
| OOM kill under burst allocation | no admission control, no backpressure |
| accounted cache size 68.3 GiB against RssAnon 54.2 GiB | wrong statistics, so the planner decides wrongly |
| three misdiagnoses in one evening because no stage could be measured alone | no `EXPLAIN`, no per-operator counters |

Two of these are more than analogies and are developed properly below:

- **§10 — multi-rate compaction IS leveled compaction.** Recent turns verbatim in L0; older
  segments merged into progressively coarser levels; work amortised in the background instead of
  one blocking pass. This does not make the 13-minute stall faster, it removes it as a category.
- **§6 — per-stage observability IS `EXPLAIN`.** "Why did this request prefill 144,436 tokens with
  zero cache hits?" is a query-plan question. Today it costs hours of log archaeology. It should
  cost one command.

The rest of the vocabulary, used precisely because the operator is a Postgres person and a sloppy
database metaphor reads worse than none:

- The **KV cache is a buffer pool.** Replacement policy, pinning and a cost model are the right
  words. `mlock` up to 24 GiB literally is pinning. `f_keep`/`f_sim` are a hit-ratio pair.
- The **sequence registry is a page table**: sequence id → resident cells, with cost per entry
  answered by the device (`llama_seq_max_cost`), not by a constant.
- **Sessions are tenants.** Resource accounting, admission control and fairness apply, and "seats
  are EARNED" is admission control stated in the operator's words.
- The **prompt prefix is a radix key.** SGLang's RadixAttention makes that explicit; llama.cpp
  today keeps a flat list and scans it for the longest common prefix.
- **Durability is a storage hierarchy**: VRAM cells → RAM prompt cache → NVMe L2 → cold
  re-prefill, with the usual promotion/demotion questions and, as of tonight, a degrade rung
  before the spill rung.

### 0.1 Where the analogy stops, and it matters

A buffer-pool page is relocatable: it has an identity (relfilenode, blocknumber) independent of
where it sits in memory. **A KV cell is not.** Its contents are a function of its position, through
RoPE. So there is no analogue of "move this page"; a mid-sequence insert or delete does not
invalidate an index entry, it invalidates the *data* of every cell after it.

The closest honest framing is a **materialized view with incremental maintenance only for appends**.
An append is maintained incrementally and is nearly free. An insert or delete in the middle is a
`REFRESH` from that point to the end. Every design below that wants to reshape history — compaction,
segment swapping, memory injection — pays that refresh, and the plan states the price in tokens
rather than hiding it.

---

## 1. What this is, and what it is not

**It is** a headless session daemon running agent loops against the local llama.cpp router on this
box, with attachable heads (TUI, flowy, ACP) and permissions answered asynchronously by a human who
may be elsewhere.

**It is not** a RAG system. Retrieval stays behind the `oracle` MCP tools (`ask_corpus`, `ask_code`,
`search_corpus`). The harness does no embedding, no reranking, no corpus curation. §19.3 tests that
line once (agentic memory) and I still hold it.

**It is not** a multi-provider client, and the provider-abstraction **category is deleted**. No
pluggable provider layer, no compat matrix, no per-provider branches, no adapters for Anthropic /
OpenAI / Bedrock / gateway shapes, no OpenAI wire format on the hot path. Three local models, one
server, one box — *"like Apple ecosystem basically."* Every abstraction that exists only for a
provider we do not run is a liability; it is exactly why opencode's defaults are metered-API
defaults. A fourth model arrives through §7.4, by someone writing a renderer.

This is a scoping decision with a real cost — a cloud fallback later means rewriting the edge — made
knowingly. It is recorded in §20.2 and **not hedged against anywhere in this design**.

**Both sides are ours, and they are fused.** llama.cpp here is the operator's fork (`glm-all`),
already carrying a custom scheduler, an elastic KV pool, earned seats, an L2 disk tier, a five-rung
pressure ladder, `fadvise` on spill and exponential checkpoint thinning. Harness and server are **one
system with one release** (§3): the boundary between them is chosen for engineering reasons rather
than inherited from an HTTP schema, and a harness-side contortion for something better solved one
layer down is called out as such.

---

## 2. The facts this design is built on

Stated once so a later decision can be checked against them rather than re-argued.

**F1 — The prompt is append-only, or the cache is gone.** llama.cpp caches prompt *and generated*
tokens. Request N+1 must extend request N *including turn N's generation*. Measured here: replay
off → `f_keep` p10 0.000, p99 re-prefill 78,598 tokens, 612 GB of cache, four OOM kills in a day.
Replay on → p10 0.999, p99 5,294. Median barely moved; the entire gain is in the tail, which is
what "responsive" means.

**F1a — Append-only is also the cache's garbage collector.** `server_prompt_cache::alloc` removes
"any cached prompts that are fully contained in the current prompt"
(`tools/server/server-task.cpp:2483-2497`, `len == it->prompt.tokens.size()`). Under append-only a
conversation occupies **one** entry for its whole life, because each turn's entry contains and
therefore destroys the previous one. Break append-only and nothing is ever contained, so a
conversation occupies N entries, each nearly as large as the last. The 612 GB was not a leak. It
was the cache doing exactly what it was told.

**F2 — Depth is paid in memory and prefill, never in decode.** Measured over 154 tasks: median
decode 35.7 t/s below 10k, 36.9 t/s above 120k. A 195k conversation decodes no slower than a 7k
one. Prefill of an already-cached prefix is ~free. A deep *warm* conversation is therefore cheaper
per turn than a shallow *cold* one, which inverts the instinct to compact early.

**F3 — Context occupation defocuses.** The operator's Axiom 1: filling the window degrades
reasoning in the 480B model and the local 30B alike; a big window is a higher *tolerance*, not
immunity. Minimising context occupation is a **quality** measure, not a speed one. F2 and F3 point
in opposite directions; §9 resolves them.

**F4 — Close the loop; never paper over it with prompt.** The operator's Axiom 2, stated
mechanically: *prompts are SOFT limits, hooks are HARD STOPS.* A prompt rule is enforced by the
model — the very component whose failure it is meant to catch. Every discipline this harness needs
must be a structure the model cannot route around.

**F5 — The failure shape to hunt is "the system did less than it claimed and said nothing."**
oracle's thesis. Same shape as opencode's empty subagent (spent its budget thinking, returned
nothing, recorded success; nine such rows in the database) and as oracle's
hallucination-with-a-footnote (retrieval honestly abstained; the model wrote a confident answer on
top of the abstention, having invented a search term, relabelled an adjacent passage, dropped two
items and attached a citation). Every layer boundary here must make *"I did not do this"*
structurally different from *"here is the result."*

**F6 — The server is a preemptive scheduler and this harness is its only client.** Seats are earned
from a floor of 1. On 2026-09-08 all 154 tasks ran on seat 0 because opencode issued its `task`
calls sequentially. Every seat measurement on this box is synthetic until a client asks for
concurrency. **The harness is what makes the scheduler real.**

**F7 — Statistics decide policy, so statistics must be right.** The eviction ladder, the regime
classifier (ID_BOUND vs TOKEN_BOUND) and every harness decision below consume numbers the server
reports. A wrong number is worse than a missing one: it produces a confident wrong plan. §6 and §18
exist because of this.

---

## 3. Architecture

> **[operator]** *"I want harness and server fused to the 11."*
> **[operator]** *"Like Apple ecosystem basically. I don't care about other providers or
> generalizations."*

This section takes both literally. The harness and llama.cpp are **one system with one release**,
and the provider-abstraction category is **deleted** — no pluggable provider layer, no compat matrix,
no per-provider branches, no adapters for Anthropic / OpenAI / Bedrock / gateway shapes. Three
models, one server, one stack.

### 3.1 What fusion actually changes, in one paragraph

Without OpenAI wire compatibility as a constraint, the harness does not have to speak
`/v1/chat/completions` at all. llama.cpp's native completion path accepts `prompt` as a **token
array** (`tokenize_input_prompts` → `json_is_array_of_numbers`, `server-common.cpp:989`). The harness
already needs per-model dialects for GLM `zai`, Qwen `qwen` and the 27B dense, so it already owns the
template knowledge. So: **the harness renders the chat template itself, tokenizes it itself, and
sends token ids.**

And because the transcript is append-only (§4.1), **the token array is append-only too.** The harness
keeps one growing `Vec<llama_token>` per transcript. A turn appends the newly rendered items' tokens
and the generated tokens. Nothing is ever re-rendered, re-serialized or re-tokenized.

That single consequence is worth stating as the headline, because it collapses three separate
problems at once:

| before | fused |
|---|---|
| per turn: re-render 200k tokens of template, serialize ~800 KB of JSON, server re-tokenizes | per turn: append the new tokens; send an offset and a length |
| the byte-prefix invariant is asserted after the fact and *hoped* to survive the server's renderer | the token array is *literally the same vector*; a prefix violation is not expressible |
| "does GLM's template reconstruct `<think>` from `reasoning_content`?" — a day of investigation | the question does not arise; we emit the tokens |
| `chat_template_kwargs` guessing, `preserve_thinking`, `clear_thinking`/`drop_thinking` capability probing (`common/jinja/caps.cpp:466-506`) | deleted |
| `extraBody` passthrough as an awkward extension to someone else's schema | fields in a protocol we own |

The prefix-stability invariant stops being something the harness hopes the server's renderer
preserves and becomes something it constructs. That is the strongest possible form of "structural,
not aspirational", and it is only available because the boundary moved.

### 3.2 Shape

```
   ┌──────────────────────────────┐        ┌───────────────────────────────────┐
   │  harnessd                    │        │  llama-server (glm-all)           │
   │                              │        │                                   │
   │  session store (SQLite/WAL)  │◄──────►│  scheduler, elastic pool,         │
   │  transcript + TOKEN ledger   │ private│  earned seats, sequence registry, │
   │  renderers (§7)              │ control│  prompt cache + L2 tier,          │
   │  tokenizer (libllama FFI)    │ channel│  pressure ladder                  │
   │  turn engine, EXPLAIN        │        │                                   │
   │  tool runner → firecode      │◄──────►│                                   │
   │  adjudication, flowy         │  shm   │                                   │
   └──────────────────────────────┘ tokens └───────────────────────────────────┘
        ▲        ▲          ▲
     TUI      remote      ACP            one repo · one release · one version number
```

Two processes, one system. Not two systems with an API between them, and not one binary.

### 3.3 The seam, argued

Fusion is about choosing the boundary for engineering reasons rather than inheriting it from the
OpenAI HTTP shape. Four candidate seams; the choice is the third.

**(a) One binary — harnessd embeds `libllama` and owns the server loop.** Maximum fusion on paper.
Rejected on the inner loop: every harness change would force a C++ link and a **29–31 s model reload**
plus a cold prompt cache. The daemon is the part that changes daily. Also, the inference process
mmaps 199.7 GB and is the thing that gets OOM-killed; a crash there would take every session, every
head and the audit log with it.

**(b) Harness as a plugin inside llama-server.** Same inner-loop problem, plus it puts a SQLite
writer, a WebSocket fan-out and a flowy connector inside the address space that must not stall — and
`slot_maybe_yield` already shows how sensitive the batch loop is to anything that blocks.

**(c) Two processes, a private control channel, shared memory for tokens, one repo, one release.**
**Chosen.** The boundary sits where the failure domains genuinely differ: a 199.7 GB inference
process with a 30 s restart cost, and a long-lived session daemon that must survive that restart with
its heads attached. Everything else about the boundary — the protocol, the vocabulary, the versioning
— is ours.

**(d) Keep HTTP, add fields.** This is where the plan was before the directive. It is strictly worse
than (c) once the provider abstraction is gone: it keeps JSON re-serialization of the whole
conversation per turn, keeps the server re-tokenizing, and keeps a schema shaped by a company we do
not use.

**What (c) is, concretely.**

- **Transport**: a length-prefixed, versioned binary protocol over a Unix domain socket at
  `$XDG_RUNTIME_DIR/harness/ctl.sock`. Not HTTP, not JSON on the hot path. The daemon and the server
  refuse to speak to a peer whose protocol version is not exactly theirs — they ship together, so a
  mismatch is a deployment bug, not a compatibility case to handle.
- **Bulk path**: the token array lives in **shared memory**, one `memfd` region per transcript, owned
  and appended by the harness and mapped read-only by the server. A request is
  `{region_id, start, len, prefix_handle}` — tens of bytes for a 200k-token prompt. The server's
  `server_tokens` is constructed over the mapped span rather than copied from JSON.
- **Control messages** (the fused vocabulary, none of which exists in an OpenAI schema):

  ```
  Admit{n_sequences, est_tokens}            → Fits{ceiling, cost_per_id, pool_free} | Refused{why}
  Prewarm{region_id, upto}                  → restore a sequence before the request arrives
  Submit{region_id, start, len, sampling,
         message_spans[], segments[],
         prefix_handle, want:{progress,timings,tokens}}
  Spans{region_id, spans[]}                 → checkpoint placement, given not guessed  (§3.4.1)
  SegmentHint{segment_id, state: active|finished|abandoned, session_id}   (§3.4.2)
  Handle{id, n_tokens, hash}                → returned; asserted on the next Submit    (§3.9-B)
  Explain{turn_id}                          → the server's half of the plan            (§3.4.6)
  Stats{}                                   → one accounting truth                     (§3.4.3)
  ```

- **What stays HTTP**: `/props`, `/apply-template`, `/metrics`, `/slots` and `/health`. They are
  diagnostic and CI surfaces, they cost nothing, and keeping them means the **black-box conformance
  suite still works from outside the fused system** (§14.3, §3.6).

### 3.4 What fusion unlocks

Six things, each unavailable across an arm's-length boundary.

**3.4.1 The server stops reverse-engineering structure the harness already has.** Context checkpoints
key on `message_delimiters`: the server takes a list of delimiter *strings*, tokenizes them, and
**string-matches the token stream** to recover message spans
(`server-context.cpp:6990`, `task.params.message_spans = task.tokens.find_message_spans(delimiters)`).
The harness *knows* where every turn, tool result and segment begins — it emitted the tokens. Fused,
it sends `message_spans` as `[[start, len], …]` directly, and `Spans` also carries **segment**
boundaries and compaction boundaries, which delimiter matching cannot recover at all. This is a small
server change with an outsized effect: checkpoint placement stops being a heuristic. Rollback waste
already fell 18,000 → 1,639 tokens when placement improved once; this removes the remaining guesswork.

**3.4.2 Eviction becomes semantic instead of LRU.** The prompt-cache ladder picks victims by
`degrade_level` then `t_last_used` (`server-task.cpp:2505-2528`) and `seq_evict` picks by LRU. The
server sees tokens and recency. The harness knows *this segment is a finished subagent*, *that one is
the active thread the human is typing into*, *this conversation was abandoned an hour ago and its
head detached*. `SegmentHint` gives the ladder a **value** input it cannot otherwise have. The
existing eviction-value formula (`rebuild_cost / memory_held`) is already the right shape; this adds
the term that is missing. A finished subagent's sequence should be the first thing demoted no matter
how recently it ran, and today nothing can express that.

**3.4.3 One truth for accounting.** Today opencode keeps its own token counts and llama keeps its
own, and they disagree — measured this week: accounted cache 68.3 GiB against RssAnon 54.2 GiB, and a
footer showing the compaction's own cost while the user waited to learn the compacted size. Fused,
there is one counter: the harness owns the token array, so `prompt_tokens` is `len`, not an estimate,
and `Stats` returns the server's memory accounting in the same call that returns the harness's. F7
says statistics decide policy; this is what makes them one set of statistics.

**3.4.4 Admission control becomes possible — THE SEQUENCE finally has a client.** GLM-STATE's
scheduler grants seats when `llama_seq_max_cost` says another id fits. Today the client asks blindly
and the loser is an OOM kill. Fused, `Admit{n_sequences, est_tokens}` returns
`Fits{ceiling, cost_per_id, pool_free}` and the harness *decides how many subagents to spawn from a
real number*. That is the missing half of the design: the operator's "seats are EARNED" has never had
a client that could participate, and §17-M6 is where it first does. It also converts §8.4's subagent
concurrency from a configured constant into a negotiated one.

**3.4.5 Pre-warming.** The harness knows what it will send before it sends it: a queued prompt while
the user is still typing, a subagent about to spawn, a conversation whose head just attached.
`Prewarm{region_id, upto}` restores the sequence from the RAM or L2 tier *before* the request
arrives. On this box that turns a 9 GB/s NVMe read (or worse, a `--sleep-idle-seconds` model reload)
from user-visible latency into background work. Nothing about the OpenAI request/response shape can
express "I will need this soon".

**3.4.6 `EXPLAIN` spans both layers.** §6's plan currently stitches harness knowledge to server
knowledge. Fused, `Explain{turn_id}` returns the server's half — which entry matched, its `lcp`,
`f_keep`, `f_sim`, which rung the ladder was on, whether admission deferred, how many batch
iterations the prefill shared — and the harness prints **one plan**, not two logs correlated by
timestamp as was done all week.

### 3.5 What fusion costs

Stated as a trade, not a footnote.

- **It couples the harness to llama.cpp's internals**, and makes *"steal from vLLM later"* harder.
  The operator has said he will be living on this llama for a while, so this is his call and it is a
  reasonable one on the evidence — the fork already carries a scheduler, an elastic pool, earned
  seats, an L2 tier and a pressure ladder that no upstream has.
  **The exit, if he changes his mind:** three interfaces would have to be re-abstracted, and they are
  the ones to keep behind named modules from day one so the exit stays cheap —
  (i) the **tokenizer** (today `libllama` FFI; would become a vocab loader),
  (ii) the **submit path** (token array + shm; would become a token array over HTTP, which vLLM and
  SGLang both accept, so this is the *cheapest* of the three),
  (iii) the **fused control vocabulary** (`Admit`/`Prewarm`/`Spans`/`SegmentHint`/`Explain`), which
  has no equivalent anywhere and would simply be lost — the harness would fall back to guessing, i.e.
  to where every surveyed harness is today. Losing (iii) is the real cost of leaving, and it should
  be understood as *"we would go back to being a normal client"*, not as *"we would port some code"*.
- **It cuts against the reversibility argument** behind the Python black-box suite (§16). **Keep the
  suite anyway**, and this is now a stronger requirement rather than a weaker one: it is the only
  thing that can measure a fused system from outside, and the only thing that could compare fused
  against unfused. Hence §3.3's rule that the HTTP diagnostic surfaces stay.
- **Deleting the provider abstraction forecloses a cloud fallback.** A later "run this turn on a
  frontier model" would mean rewriting the edge: renderer, tokenizer, submit path and the whole fused
  vocabulary. Recorded in §20.2 as a knowingly accepted cost, and **not hedged against** anywhere in
  this design, per the directive.

### 3.6 How the inner loop stays fast

The thing that would kill fused development is a C++ build plus a restart in every iteration, where a
restart costs the warm cache and a 30 s model reload. So the split is drawn deliberately:

**Iterable in the daemon alone — no rebuild, no restart, seconds:** renderers and dialects, the
transcript and ledger, compaction and segment policy, the tool contract, adjudication and its policy
table, flowy, heads and rendering, `EXPLAIN` formatting, the whole conformance suite. That is the
overwhelming majority of the work in this plan, and none of it touches the server.

**Forces a C++ build and a restart:** the control-channel protocol version, the shm submit path, and
each of §3.4's six unlocks. These are **six discrete server changes**, not a continuous stream — each
is landed once, measured once, and then iterated from the daemon side.

Three practices that keep even those cheap:

1. **Protocol changes are additive and versioned in lockstep.** Both sides ship from one repo with
   one version number; the socket handshake rejects a mismatch loudly rather than degrading.
2. **A daemon-only fallback path.** The harness can always submit over HTTP with a token array
   (`/completion` accepts one today, unmodified). So a server rebuild is never a *blocker* — it is an
   optimisation that lands. M1 runs entirely on the fallback path.
3. **Build in a firecode run, niced.** With `--no-op-offload` the CPU is on the inference path and a
   `-j 24` build steals prefill and decode from production. This is also the tidiest early use of the
   substrate (§3.9).

### 3.7 Chat is a channel of the loop, and the daemon is what makes that possible

> **[operator]** *"I don't want to fiddle with monitors, nor timers which pollute the context."*

**What is wrong today, mechanically.** In Claude Code + flowy the only way to hear a message is a
background poll loop: `flowy inbox --as NAME` blocks, prints one line, exits, and is re-armed.
firecode records the identical workaround and the identical reason — *"a blocking MCP call cannot be
a permanent listener, it freezes the caller's turn, so the listener is a background shell running
`firecode chat --inbox`, which exits on each message."* Every arrival therefore lands in the
conversation wrapped as a tool invocation plus a tool result plus the re-arm. **The listener is a
timer in disguise, and its bookkeeping is a permanent context cost.** That is the mechanism being
objected to, and it is a harness defect, not a flowy defect.

**The fused answer, and it is one of the clearest arguments for the daemon.** `harnessd` is
long-lived and owns the session, so it simply *holds the subscription*. A per-turn CLI process never
could. Four rules follow:

1. **Inbound messages are pushed, never polled.** The daemon holds a live subscription (§12.2 ask 6);
   an arriving message becomes an event delivered into the session. There is **no background listener
   process, no re-arm, no interval, and no Monitor-shaped scaffolding anywhere in this design.** If
   any appears, it is a bug.
2. **A chat message costs exactly what a user message costs, because it *is* a user message.** It
   becomes a `TranscriptItem::User` with the sender's identity as metadata in the Journal, not in the
   prompt. No tool envelope, no "you have received a message" wrapper, no polling artefacts. This is
   measurable and should be a test: an inbound message adds its own tokens and nothing else (§18.1-I11).
3. **Chat is the primitive; adjudication is a usage of it.** An `--ask` is a message that blocks on a
   reply. That ordering matters: ordinary conversation is the default case and a decision request is
   the special case, rather than the reverse. §11.6's "permissions and questions are one mechanism"
   is really "both are chat".
4. **A flowy chat is a head** (§13.4), so multi-head and chat are one design and are not solved
   twice.

Mid-turn arrival is the interesting case and it gets its own answer in §5.8 (steering).

### 3.8 What is authoritative

The daemon's **session log** — append-only events in SQLite/WAL with a monotonic `seq`, single writer
per session. Everything a head displays is derived from it.

The daemon's **token array** is authoritative for what the model saw. The server's KV cache remains an
accelerator: every request must be correct, and give the same answer, with an empty cache. Fusion does
not change that, and the one place it is violated (`--cache-reuse` shifting cells and restoring
recurrent state computed with deleted text present) is why it is stated so flatly.

No head's view is authoritative, including the local TUI's.

### 3.9 The execution substrate is firecode

**firecode** (`/data/scratch/harness-survey/firecode`) is the operator's per-run
Firecracker/qemu microVM sandbox. It already does the thing this harness would otherwise build
badly, and it is the reason §11 is short.

What it is, in one paragraph: a **run** is one VM and everything created for it. Lifetime is a
**cgroup subtree** (`firecode.slice/run-<id>/vm`, `/children/`), not a process tree — *"a process
tree cannot express what a run owns"* — so one write to `cgroup.kill` ends the VMM, the relays and
every VM started on its behalf, transitively, and liveness is *"is the cgroup populated"* rather
than a pidfile. The root filesystem is overlayfs layers (base image / imported docker images by
digest / main checkout / per-run writable upper), so nothing is copied per run. Two hypervisors
behind one identical guest image: firecracker for a 60 ms checkpoint restore, libvirt/qemu when a
GPU must be passed through. The guest is untrusted by construction — project paths are resolved
from the spawn server's own config and never taken from the caller, parentage is derived from the
connection rather than believed, and there is deliberately no host-side seed script.

**Where tool execution lives:** effectful tools do **not** run as child processes of the daemon. They
run inside a firecode run. Read-only host tooling (the `oracle` MCP servers, which the operator
deliberately keeps read-only) stays on the host.

#### Placement: harnessd on the HOST, tools in the run

`firecode claude` today runs the whole agent inside the VM. Our daemon does not, and the reasons
are specific:

- the session log, the render ledger and the permission audit must survive the VM (and a checkpoint
  rollback), so they belong to the host;
- heads attach to the daemon, and a head is a human's terminal on the host or another machine;
- **the model server cannot move.** firecode's own note: *"No GPU sharing between VMs on consumer
  silicon: vGPU is fused off… Serving a model from the host and relaying the port into guests is the
  arrangement that does work."* llama.cpp stays on the host either way, so the daemon may as well be
  next to it.

The primitives already exist for this shape: `firecode up` / `firecode in '<cmd>'` / `firecode down`
on the host, and `vm_up`, `vm_in`, `vm_down`, `vm_checkpoint`, `vm_reset` over MCP. The exec channel
(vsock port 1026) *"returns the command's own exit status, so a failing test suite and a suite that
could not start are distinguishable"* — which is exactly the distinction §8.1 clause 3 needs.

#### Mapping

| harness concept | firecode concept | who owns lifetime |
|---|---|---|
| session | one **run** (`firecode up --project <key>`) | the session; ending it runs `down` (graceful, so the work is copied back) then `cgroup.kill` |
| subagent | a **spawned sibling run**, nested in the parent's cgroup | the parent run, transitively — invariant 2 |
| a bash / write / edit call | one `vm_in` on the session's run | the call |
| a long-lived build or server | the run stays up between turns; that is what `firecode up` is for | the session |
| host read-only MCP (`oracle`) | not in the run at all; called by harnessd on the host | the daemon |

One caveat inherited and worth stating: **a run spawned by a caller on the host has no parent run**,
so invariant 2 does not reach it — *"ownership is worked out from the cgroup the caller's connection
came from, and a host process is not in one"*. harnessd is a host process. So harnessd must place
itself in a cgroup that firecode recognises as a parent, or explicitly pass `--parent-run`, or accept
that it is responsible for reaping. §11.8 asks for the first.

#### Subagents: firecode's shape beats opencode's

opencode's subagent is a child *session* in the same process, sharing the machine and the
credentials, with cleanup by process bookkeeping. firecode's is a sibling VM with cgroup lifetime
and an empty MCP config. Take firecode's, including the two policies that come with it:

- **flat by default.** Spawned runs get `--no-mcp` and an empty `--mcp-config`, so a child cannot
  reach the spawn server and fan out further. The three conditions for relaxing it are already
  written down: a depth budget decremented per level, a fresh workspace per child, and the parent
  run recorded on it — *"the second because two runs sharing one workspace layer corrupt it, which
  surfaces as a guest whose root has gone read-only."*
- **"Talking and fanning out are different powers."** A spawned agent has no tools but keeps
  `firecode-chat`, so it can still say it is blocked. Our subagents inherit exactly that: no
  spawn rights, full voice.

#### Checkpoints and the KV cache — the honest answer

A firecracker checkpoint restores in ~60 ms, and a restored VM is **transient**: it runs on the
checkpoint's copies of its drives, so it cannot invalidate what it was taken from. The model's KV
cache lives on the host, outside the VM, keyed by prompt content in the RAM/NVMe prompt cache.

They are **decoupled, and that is mostly fine**: the KV cache is keyed by what the conversation
*says*, not by which VM said it, so a restored run replaying the same transcript still hits the same
entry (until the ladder demotes it, which costs an NVMe read, not a re-prefill). Two things I can
support:

- Because harnessd's session log is on the host (above), a VM rollback does **not** rewind the
  conversation. That is a deliberate asymmetry: the *filesystem* rewinds, the *transcript* does not.
  A transcript that recorded "I created file X" while the restored VM has no X is a lie the model
  will act on, so a checkpoint restore must append a `ToolResult{outcome: NotRun}`-style marker item
  saying the workspace was rolled back to checkpoint C. It is an append, so the prefix survives.
- A checkpoint taken *between* turns is clean; one taken mid-turn is not, because tool calls in
  flight have no meaning after a restore.

What I **cannot** support without measurement: whether it is worth checkpointing the two together
(a VM checkpoint plus a `POST /slots/{id}?action=save` of the conversation's KV) to get a genuine
combined resume. The pieces exist — the L2 tier already persists slot state to `/data/kvcache` — but
the sizes are wildly different (a 4 GiB KV entry against a 60 ms memory-map restore) and nobody has
measured whether the KV half would still be resident when the VM half is restored. Flagged as
**UNVERIFIED-3**; do not build for it in v1.

### 3.10 Block-hash addressing, handles, and what each can and cannot buy

The operator's question: *if KV blocks are hashed and the hashes used as ids, can the harness send a
combination of hashes and text instead of full prompts, and can different sessions and agents reuse
blocks?* The two halves have different answers and must not be run together.

#### A. Finer-grained cross-session sharing — real, and available

Cross-session and cross-agent reuse **is** what prefix caching already is. llama.cpp does it today at
**sequence granularity**: `find_it` scans cached prompts for the longest common prefix and picks the
entry that beats the slot's own prompt on both `f_keep` and `f_sim` (`server-task.cpp:2598-2626`,
`selected sequence by LCP similarity`), with a hard floor at `f_keep < 0.25`. Block-hash addressing
(vLLM) or a radix tree (SGLang RadixAttention) makes this finer-grained and — the part LCP cannot do
— lets **branches** share, not only linear extensions.

**Why that is the big prize for this workload.** `--kv-unified` shares the pool as a *budget*; it
does not share *cells*. N resident sequences behind one system prompt and tool set each hold their
own copy of that prefix's cells. At GLM's 19.25 KiB/token the duplicate is:

| shared prefix | per sequence | duplicated at N=12 | at N=30 |
|---|---|---|---|
| 4,000 tok (system + tools) | 75.2 MiB | 827 MiB | 2.15 GiB |
| 8,000 tok (+ a shared briefing) | 150.4 MiB | 1.62 GiB | 4.26 GiB |
| 20,000 tok (subagents forked off a deep parent) | 376 MiB | 4.04 GiB | 10.65 GiB |

Against a box where CUDA0 had ~1.2 GiB free at ceiling 5 and each additional sequence id costs
~265 MiB there, the 4,000-token row alone is roughly three more sequence ids, and the 20,000-token
row is the difference between the operator's goal being impossible and being comfortable.

**And the honest limit, which matters more than the table.** GLM's per-sequence fixed cost —
**436.7 MiB of recurrent state across 34 KDA layers** — is *not* token-proportional and therefore
**not shareable by any block scheme**. It is a fixed-size state per sequence id. So:

- at N=30 the irreducible floor is 30 × 437 MiB = **12.8 GiB** before a single token;
- block sharing at an 8,000-token shared prefix saves 4.26 GiB against that floor;
- below the ~23,200-token break-even, fixed cost dominates and block sharing is the minority win.

Which is to say: **block sharing helps most exactly where the shared prefix is deep, and helps least
in the ID-bound regime this box is actually short of.** It does not remove the argument for a third
GPU; it changes the constant. Present it that way and nobody will be disappointed later.

**Does the eviction ladder move to block granularity? No — keep two ladders at two tiers.**

The drafted prompt-cache ladder (GLM-STATE "Open, ranked" #3, draft at `1804d53`) degrades a whole
conversation entry: full → thinned → skeletal → spilled → dropped, choosing the *least degraded*
entry first so loss spreads evenly (the shipped rung-one code at `server-task.cpp:2505-2528` already
does this). That policy is about the **fidelity of a restorable conversation**, and its unit —
checkpoints, draft state — is a per-conversation artefact tied to positions. A block has no
checkpoints, so "degrade this block" has no meaning.

A shared-block index needs a different and much simpler policy, and it is the ordinary buffer-pool
one: **refcount, plus LRU among blocks at refcount zero.** Two consequences to write down:

- **Never merge them.** A degrade decision applied to a shared block would silently coarsen every
  conversation referencing it. That is precisely the coupling §10.5 rejects — memory pressure must
  cost latency, never fidelity, and a shared object multiplies the blast radius.
- **They live at different tiers.** Refcount + LRU governs the *resident* pool (VRAM cells); the
  degrade ladder governs the *RAM/NVMe prompt-cache* tier. Sharing vocabulary is fine; sharing
  policy is not.

One new hazard block sharing introduces, worth stating before it is discovered: a shared block held
by a long-idle sequence pins memory for every sharer, so the age-based demotion work (GLM-STATE
"Open, ranked" #2) becomes *more* necessary under sharing, not less.

#### B. Composing arbitrary blocks out of order — blocked by physics, not by protocol

A block's KV is a function of three things: its own tokens, **every preceding token** (causal
attention), and **its absolute position** (RoPE). The same text at position 5,000 and at position
50,000 produces different KV.

That is exactly why vLLM's block hash **chains the parent**:

```
block_hash = H(parent_block_hash, token_ids)
```

The identity of a block *includes its whole prefix*. So a block is reusable only at the same
position, behind the same prefix. **"Send blocks A, F, Q plus some new text" cannot work as stated**,
and no amount of protocol design changes that. Anyone reading this later should take that as the
finding, not as a limitation of the current implementation.

**What a handle protocol would actually buy**, stated so nobody over-invests:

- **Not prefill compute.** The cache hit saves that whether the client sent text or a handle. This is
  the misconception to kill.
- **Bandwidth and tokenization** — and these are *not* obviously negligible, which is worth checking
  rather than asserting. A 200k-token conversation is ~800 KB of JSON per turn; on loopback the
  transfer is nothing, but re-rendering the template and re-tokenizing 200k tokens is real work, and
  on the **warm** path it is competing against a prefill of ~zero rather than against 453 s. The
  measurement already exists: compare wall clock from request-sent to first token against
  `timings.prompt_ms + predicted_ms`. If the gap is tens of milliseconds, forget it; if it is
  hundreds, a handle that lets the server skip re-tokenizing a known prefix is a real latency win on
  every warm turn. **UNVERIFIED-15**, and it is one command in `EXPLAIN`.
- **The real prize: an explicit, verifiable cache hit instead of a discovered one.** Today the
  harness ships 200k tokens and *hopes* the server finds the prefix. A full day went into
  discovering that it had not. A handle turns that into a checked assertion — which is precisely §6's
  `EXPLAIN` idea moved into the wire format, and it is the ground on which this should be argued.

**The shape, if it is built (server track, S6).** The handle is an **assertion, not a substitute**:

- every response carries `prompt_handle: {id, n_tokens, hash}` naming the prompt-plus-generation
  state now cached;
- the next request sends `prefix_handle: <id>` **alongside the full messages**, so correctness never
  depends on cache state (§3.8: the cache is an accelerator);
- the server checks that the handle's token sequence really is a prefix of the newly rendered prompt
  and replies `prompt_handle_status: "hit"`, or `"miss"` with a reason —
  `evicted` / `diverged at token N` / `unknown` — and falls back to the normal LCP scan;
- an optional `prefix_handle_required: true` makes a miss a **400 instead of a silent re-prefill**.
  That mode is worth building for its own sake: it converts the conformance suite's C3/C4 (§14.3)
  from statistical checks into loud assertions, and it is how a prefix regression is caught the day
  it lands rather than a day later at 612 GB.

Note that this composes with **S1** (returning the matched entry's `lcp`/`f_keep`/`f_sim`): S1 tells
you what happened, the handle lets you say what should have happened. Build S1 first; it is smaller
and it is most of the diagnostic value.

#### C. Routes past the position problem — options to measure, not to assume

- **Position shifting.** The primitive exists (the `--cache-reuse` path shifts cells). The hazard is
  measured and model-specific: on GLM it restored a checkpoint whose recurrent state was computed
  with deleted text still present, producing confident wrong answers fast; content-based
  invalidation (T3.5b) shut it. GLM is MLA + DSA with 34 recurrent blocks. Any position shifting must
  be re-earned per architecture, never inherited.
- **Stitching (CacheBlend / PromptCache lineage).** Recompute a fraction of tokens to reconcile
  segments assembled out of order, trading a partial prefill for a full one. **The recompute fraction
  is the entire economics, and it is unknown for MLA + DSA.**

  A concern that must be settled before any effort goes here, and that I have not seen addressed in
  that literature: **CacheBlend's premise assumes attention-only layers.** In a recurrent layer the
  state at position *n* is a function of all *n* tokens through the recurrence, so "recompute the
  last *k*" is not a partial correction — it is either exact (because you re-ran from the divergence)
  or it is wrong. GLM has 12 MLA attention blocks and **34 KDA recurrent blocks**, so stitching may
  be applicable to roughly a quarter of the model and impossible for the rest. If that holds, the
  recompute fraction for GLM is not "a few percent"; it is "everything after the divergence, for 34
  of 46 blocks". **UNVERIFIED-16**, and it is the single question that decides whether stitching is
  worth any effort on this stack.

  **The experiment, stated as a method rather than a hope.** Build prompt `P = A ++ B ++ C` and
  compute the reference KV by full prefill. Then assemble the KV from a cached `A`, a cached `C`
  taken from a *different* position, and a recompute of the first *k* tokens of `C` at its new
  position. Vary *k*. Measure two things: (i) exact agreement of 200 greedy continuation tokens
  against the reference, over ≥20 samples at depths 10k / 50k / 150k; (ii) the next-token KL
  divergence at the join, which degrades smoothly and so localises the failure where (i) only says
  yes or no. Report the smallest *k* that holds (i) at every depth — that number *is* the economics.
  Run it **per layer class**, separating the MLA blocks from the KDA blocks, because the expectation
  above says they will not agree, and an aggregate number would hide it.
- **Accepting the cost honestly.** A swap costs a re-prefill from the swap point to the end: linear
  in the *suffix*, so ~11 s for a tail swap and ~453 s at the head of a 200k conversation on this box
  (§19.3's table). This is the option that always works, and v1 takes it (§19.3 recommends *append,
  do not swap*).

All of C is the honest path to *"infinite memory via explicit search and attention segments that are
swappable"* — see §19.3, which costs it and recommends against building swapping in v1 or v2 on the
measured facts. This subsection is what would change that recommendation: if UNVERIFIED-16 comes back
favourable, stitching moves swapping from "linear in the suffix" to "a measured fraction of the
suffix", and §19.3's conclusion should be revisited at that point and not before.

---

## 4. Data model

### 4.1 Two layers, and only one can change the prompt

The central structural decision.

**Layer 1 — the Transcript.** The ordered sequence of `TranscriptItem`s that becomes the prompt.
Immutable, with exactly one mutation: `append`. No update, no edit, no insert, no remove.
Truncation is not a mutation; it produces a **new** transcript with a new id (§5.5, "fork"),
because a truncation is a cache divergence and must be visible as one.

**Layer 2 — the Journal.** Everything else: display parts, streaming deltas, tool progress, timings,
errors, adjudication requests and decisions, head attach/detach, warnings. Freely appended, freely
re-rendered, never consulted when building a request.

The rule, enforced by types rather than by review: **the request builder takes a `&Transcript` and
nothing else.** It cannot see the Journal. Putting a new field into the prompt therefore requires
moving it into a `TranscriptItem` — a deliberate, reviewable act.

Contrast: opencode's `Part` union carries both roles at once (`session/message-v2.ts`: `text`,
`reasoning`, `tool`, `step-start`, `step-finish`, `compaction`, `patch`) and the request builder
filters it by type. Filtering is a decision made at every call site; separating the layers is a
decision made once. The union is still worth vendoring — as the *Journal's* part type (§15).

### 4.2 TranscriptItem

Close to Grok Build's `ConversationItem`
(`crates/codegen/xai-grok-sampling-types/src/conversation.rs:66-84`), a serde-tagged sum type with
reasoning as a **sibling** of assistant rather than a field on it:

```
TranscriptItem =
  | System      { text, origin: Bootstrap | Update }
  | User        { parts: [Text | Image | FileRef] }
  | Reasoning   { text, field: ReasoningField }
  | Assistant   { text, tool_calls: [ToolCall] }
  | ToolResult  { call_id, name, outcome: ToolOutcome, payload }
  | SegmentMark { segment_id, label, kind, edge: Open | Close }
```

Grok's rationale for the sibling, quoted because it is the whole argument:

> "The interleaved order of `[reasoning, tool_call, reasoning, …, message]` produced by the model
> stays byte-stable across turns. That stability is what lets the server-side prefix KV-cache hit."

Reasoning-as-a-field-on-assistant forces last-write-wins when one turn produces several reasoning
blocks around several tool calls, and loses the interleaving. GLM and Qwen both interleave. Take the
sibling.

Two fields deserve comment.

`ToolResult.outcome` is a **closed vocabulary**, borrowed in spirit from dsh's approval outcomes and
demanded by F5:

```
ToolOutcome = Ok | Abstained{reason} | Failed{reason} | Denied{req_id} | Timeout | NotRun{why}
```

`Abstained` is not a flavour of `Ok`. It is the case oracle measured: retrieval honestly said "the
corpus doesn't cover this" and the model wrote a confident answer on top of it. §8.2 says what the
harness does with each.

`SegmentMark` renders to **nothing** — it is a zero-width delimiter carrying `{segment_id, label,
kind, created_at, last_touched}`. It exists in v1 solely so §19's futures (per-segment decay,
user-assisted compaction, agentic memory) have something to address. §19.1-L2 states what it costs
if those futures never arrive: one row and zero rendered bytes.

### 4.3 The Token Ledger — append-only made structural

Because the harness renders and tokenizes (§3.1), the artefact that must be append-only is not a byte
string it hopes a remote renderer preserves. It is **one growing token vector per transcript**, in
shared memory, which the server reads.

```
TokenLedger = [ (item_id, tok_offset, tok_len, h_k) ]
   h_k = H(h_{k-1} ‖ tokens(item_k)),   h_{-1} = H(tokens(stable_prefix))
   tokens : Vec<llama_token>, memfd-backed, append-only
```

What makes this structural rather than aspirational — and it is stronger than anything in the survey:

1. **There is no rewrite operation to call.** The vector is appended to. Request N+1 is
   `(region, 0, len_{N+1})` and request N was `(region, 0, len_N)` with `len_N ≤ len_{N+1}` over the
   *same memory*. A prefix violation is not something the test catches; it is not expressible.
2. **The unit is the server's unit.** llama.cpp matches prefixes with `get_common_prefix` over
   `server_tokens`. Our ledger is in token ids, so "our invariant" and "what the cache does" are the
   same object. Under a byte-and-template scheme they are two objects joined by a jinja renderer
   nobody controls.
3. **The hasher is fed forward only**, from a resumable state at the last item, so the chain cannot
   be recomputed differently.
4. **`h_k` is the cache identity**, and it is what `prefix_handle` (§3.10-B) asserts. If the head at a
   shared index differs between two requests they are different conversations, and the daemon says so
   before submitting.
5. **A periodic full re-hash** (every request in debug, every 25th in production) catches renderer
   non-determinism — a renderer bug would show up as a token-level mismatch on re-render, and it is
   the *renderer* that is now the thing under suspicion, which is exactly where §7.4's CI diff points.
6. **The ledger is persisted** with the transcript, so a daemon restart resumes the same chain, and
   the token vector is rebuilt by replaying the ledger's spans rather than re-rendering.

This supersedes omp's digest scheme (a 32-bit rolling hash over the *normalized message object*,
`append-only-context.ts:311-334`) on two counts: 32 bits is a collision budget I do not want to spend
on a 300-turn conversation, and hashing the message object cannot see divergence introduced
downstream in the encoder — which is the whole failure class. Hash the tokens that go on the wire.

### 4.4 What is persisted vs derived

**Persisted** (SQLite/WAL, one file per box):

- `session` — id, title, model id, dialect id (**= the dialect's `template_sha`**, §7.1),
  workspace root, owner, approvers, created.
- `transcript` — id, session_id, parent_transcript_id (non-null after a fork), stable_prefix_id.
- `transcript_item` — ordered, immutable, with its ledger row.
- `stable_prefix` — frozen system text + normalized tool schemas + fingerprint, **content
  addressed**. Shared across sessions with the same one, which is also how a new session inherits a
  warm prefix for free. (The operator measured this exact effect in oracle: sharing one prompt
  prefix across features took an identical request from 9,325 tokens processed to **4**.)
- `event` — the session log, `(session_id, seq)`.
- `adjudication_request` / `adjudication_decision` — full audit, never deleted (§11).
- `turn_metrics` — per request: prompt/cached/predicted tokens, `finish_reason`, `prompt_ms`,
  `predicted_ms`, `draft_n`, `draft_n_accepted`, model, dialect, ledger head sent, and the
  computed expectations of §18.
- `tool_spill` — full tool outputs too large to inline, by content hash (§8.3).
- `segment` — id, label, kind, level (§10), created, last_touched.

**Derived** (rebuildable, droppable): the head snapshot (§13.2), rendered markdown, every aggregate
in §6/§18, and the request body itself.

**Not stored at all**: the model's raw HTTP response envelope. Its content becomes transcript items
and journal parts; keeping the envelope invites someone to replay it and diverge.

Borrowed from pi's durable harness spec (`packages/agent/docs/harness.md`), which is the closest
prior art for crash-safe local persistence and worth reading before implementing this:

- three stores and one invariant — *"every payload is in an entry, a bound value/list, or the
  ledger; there is no third place"*;
- **pending** rows for content that exists durably before it has a place in the tree (queued user
  input, streamed assistant frames, out-of-order tool results), deleted in the same transaction that
  places the real entry — so no partial transaction is ever visible;
- *"a compaction is a self-contained checkpoint, not a pointer into history"*, with a complete
  retained tail rather than an implicit "read further back".

### 4.5 Events

One event per thing a head must see, all with `(session_id, seq, ts)`:

```
TurnStarted{turn_id, model, ledger_head}
PromptProgress{turn_id, total, cache, processed, time_ms}   ← nothing surveyed reads this
Delta{turn_id, target: Text|Reasoning, text}
ToolCallProposed{turn_id, call_id, name, args_digest}
DecisionRequested{req_id, kind, call_id, summary, options, deadline, on_timeout}
DecisionAnswered{req_id, outcome, by, basis, late: bool}
ToolStarted / ToolProgress / ToolFinished{call_id, outcome, ...}
TurnFinished{turn_id, finish_reason, usage, timings}
TurnInterrupted{turn_id, reason, partial_kept: bool}
TranscriptAppended{item_id, kind, ledger_head}
HeadAttached / HeadDetached{head_id, kind, identity}
Warning{code, detail}                                       ← §18 assertions land here
Explain{turn_id, plan}                                      ← §6
```

`Delta` carries **only the increment**. No event ever carries accumulated text. That single rule is
half of "render cost independent of output length"; §13.3 is the other half.

---

## 5. The prompt pipeline

### 5.1 End to end

```
  StablePrefix (frozen, content-addressed)      system text + normalized tool schemas
        │
        ▼
  Transcript items 0..k                          append-only (§4.1)
        │
        ▼
  RENDER — our renderer for this dialect (§7)    items → the model's control tokens, verbatim
        │                                        no jinja, no chat_template_kwargs, no probing
        ▼
  TOKENIZE — libllama, this model's vocab        appended to the transcript's token vector
        │                                        NOTHING earlier is re-rendered or re-tokenized
        ▼
  Token ledger append + hash chain (§4.3)        h_k becomes the prefix_handle for the next turn
        │
        ▼
  Submit{region_id, start:0, len, sampling,      over the control channel (§3.3);
         message_spans[], segments[],            HTTP /completion with a token array is the
         prefix_handle, want:{...}}              always-available fallback
        │
        ├── prompt_progress {total, cache, processed, time_ms}   → PromptProgress events
        ├── token deltas + reasoning-channel flag                → Delta events
        ├── tool-call spans, detected by OUR parser              → ToolCallProposed
        └── final: usage, timings, prompt_handle
        │
        ▼
  Post-flight assertions (§18.2) → Warning events, turn_metrics, EXPLAIN plan
        │
        ▼
  Append: Reasoning, Assistant, ToolResult …     generated tokens appended verbatim to the vector
```

Two consequences of the last line worth making explicit, because they are the fused payoff:

- **Generated tokens are appended as tokens, not re-derived from text.** So §18.1-I1 — the
  generation-inclusive prefix invariant — holds *by construction* rather than by assertion. The model
  emitted token ids; we keep the token ids; the next prompt contains them exactly. No detokenize /
  retokenize round trip, which is where a whitespace or special-token difference would otherwise
  creep in.
- **Parsing is ours too.** Reasoning-block boundaries and tool calls are recovered by our own parser
  over the token stream, using the same control tokens our renderer emitted. `--reasoning-format`,
  `reasoning_content` reconstruction and the whole `common/chat.cpp` grammar-per-model layer are not
  in the path.

### 5.2 The stable prefix

System text plus normalized tool schemas, frozen behind a fingerprint, exactly as omp's
`StablePrefix` does (`packages/agent/src/append-only-context.ts:49-91`) — but content-addressed and
persisted, so it is shared between sessions and survives a restart. Normalization: canonical JSON,
sorted object keys, tools in a fixed order, no incidental whitespace.

**A changed stable prefix is a new prefix and therefore a cold start.** That is the correct cost and
the harness must not hide it — but it must also not incur it needlessly. Two rules follow:

1. **Nothing volatile goes in the stable prefix.** No timestamp, no cwd listing, no git branch, no
   "files recently changed". The operator paid for this: adding one line to an `<env>` block cost a
   full cold re-prefill of a 179k conversation.
2. **Everything volatile arrives as an appended item**, i.e. §5.3.

Two content rules on the prefix itself, both from oracle and both worth stating as hard rules
because they cost the operator real debugging time:

- **A prompt may describe how to use a tool; it must never describe what the data contains.** An
  example list read as an inventory: asked about Kubernetes, the model answered "we have nothing"
  without calling a tool, reading back a stale parenthetical from the system prompt and a second
  copy in the tool's own docstring. **The tool description is part of the prompt** and needs the
  same scrutiny and the same expiry date.
- **State the answer language.** oracle's prompt never did, and a Chinese-trained model drifted into
  Chinese mid-sentence under Russian input. That is a prompt-completeness failure, not a model
  failure. Our targets are the same model families.

### 5.3 System prompt changes are appended (`in-history`)

Take DeepSeek's mechanism, which is more precisely specified than the name suggests.
`SystemPromptProjection.project()`
(`packages/core/agent-loop/src/runtime-context.ts:81-96`):

- compare the newly rendered prompt against **the last non-empty system node**, not against node 0;
- equal → emit nothing at all (not even a no-op event);
- different, and the route declares `in-history` → **append a new `role: "system"` message** at the
  tail, with a plugin-source marker and no wrapper tags;
- otherwise (capability absent, a series boundary, or the prompt cleared) → fall back: blank every
  later system node and rewrite node 0, normalizing history back to single-system form.

Two things to keep from that shape: the comparison target is *latest*, not *head*; and the fallback
is explicit and normalizing rather than a silent divergence.

**Under self-rendering this stops being a gamble on someone else's template.** The earlier draft
flagged it as UNVERIFIED — many jinja templates honour only `messages[0]`, so a mid-history system
message might vanish. We render, so *we* decide: the renderer emits the model's own system-turn
control tokens at the tail. The remaining question is not "will it be rendered" but "was this model
trained to accept a system turn mid-conversation", which is a **behavioural** question, answered by
an eval rather than by reading a template.

The conservative default until that eval exists, per model: emit the update inside the model's *user*
turn with a fixed envelope (`<system-update seq=N>…</system-update>`). Still an append, still
prefix-stable, and it only costs the model reading it as user text. Promote a model to true
in-history system turns when its eval says the behaviour is there. Both forms are one field in the
dialect (§7.2, `system_update_mode`).

### 5.4 Reasoning is not "replayed" — it is simply still there

Under a fused, self-rendering harness, the survey's central defect stops existing as a category.
There is no `reasoning_content` field to remember to send back, no field-name detection, no
capability probe, and no chance that a template drops an earlier turn's thinking. **The tokens the
model generated — including its thinking-block control tokens — are in the vector, and the vector is
never rewritten.**

What that deletes, itemised, because each item cost somebody real time:

- pi's `foundReasoningField` scan and per-provider alias override
  (`openai-completions.ts:597-619`, `:1310-1318`);
- omp's `replayReasoningContent` compat flag and its hostname/RFC1918 auto-detection
  (`config/append-only-context-mode.ts:28-52`), whose silent self-disabling behind a proxy is the
  documented cause of the 612 GB growth;
- omp's dual-emission `preserve_thinking` (top-level *and* `chat_template_kwargs`,
  `openai-shared.ts:1064-1079`);
- llama.cpp's own `clear_thinking` / `drop_thinking` capability probing
  (`common/jinja/caps.cpp:466-506`);
- **UNVERIFIED-1 and UNVERIFIED-2 from earlier drafts of this plan** — whether GLM-5.3-Flash's
  template reconstructs `<think>` from `reasoning_content`, and whether it honours a mid-history
  system message. Neither question arises when we emit the tokens.

Two obligations replace them, and they are smaller:

1. **The renderer must place thinking blocks exactly as the model was trained to see them** — which
   is a property of *our* renderer and is checked against the shipped jinja in CI (§7.4).
2. **Assistant turns with no visible text still exist** and must render correctly. DeepSeek's rule —
   `content: ""`, never `null`, because *"the message sits durably in the session log, so a null here
   bricks every later turn of that session"* (`llm-deepseek/src/types.ts:82-99`) — becomes, in our
   terms: an assistant item with empty text must still emit its turn-boundary control tokens. Same
   bug, one layer down, and worth a test for the same reason.

The §9 debate about whether replaying reasoning is worth doubling occupancy is **unchanged** — that
is a quality question, not a mechanism question, and §9.3's falsifiers still decide it.

### 5.5 Fork, not truncate

Any operation that would shorten or rewrite history — hard compaction, rewinding to an earlier turn,
dropping an image — creates a **new transcript** with `parent_transcript_id` set, and the session
points at the new one. Consequences, all of them wanted:

- the old transcript is intact, so a rewind is reversible and `EXPLAIN` can still say what the old
  prefix was;
- the divergence is a first-class object with an id, so §6 can name it as the cause of a cold
  prefill instead of leaving it to be inferred;
- the old prompt-cache entry is **not** superseded (it is not contained in the new prompt), so it
  survives on the server until the ladder demotes it — which is exactly right, because a rewind may
  come back.

The cost is stated honestly, not hidden: a fork costs a re-prefill from the divergence point to the
end, `(n_total − n_common) / 441 t/s` on this box. §10 is largely about making that number small.

### 5.6 Submission — a protocol we own, and its fallback

With the provider abstraction deleted there is no "extra body seam", because there is no foreign
schema to extend. pi's `Object.assign(params, extra)` last-write-wins trick
(`api/openai-completions.ts:989-992`) was the right answer to a problem we no longer have; it is
recorded here as the fallback path's rule and nothing more.

**Primary path** — `Submit` over the control channel (§3.3), carrying what an OpenAI schema cannot
express: the shm region and span, `message_spans` and `segments` given rather than string-matched
(§3.4.1), the `prefix_handle` assertion (§3.10-B), and the `want` set for progress, timings and
tokens.

**Fallback path** — `POST /completion` with `prompt` as a **token array**, which today's unmodified
server accepts (`json_is_array_of_numbers`, `server-common.cpp:989`). This is what M1 runs on, and it
is what keeps a server rebuild from ever blocking a harness milestone (§3.6). On this path:

| field | why |
|---|---|
| `cache_prompt: true` | the whole design |
| `return_progress: true` | `result_prompt_progress{total, cache, processed, time_ms}` (`server-task.h:264-271`), which drives §6's progress display |
| `timings_per_token: true` | live decode rate and draft acceptance |
| `return_tokens: true` | generated **token ids**, so §5.1's "append tokens, not text" holds on the fallback path too (`server-schema.cpp:35`) |
| `message_delimiters` | the pre-fusion way to get checkpoint placement; superseded by `Spans` |
| **no** `n_predict` cap | there is no output wall; §5.7 |

Note what has moved: `return_progress` and `timings` were, in every surveyed harness, awkward
extensions bolted onto someone else's schema — and omp's progress-chunk classifier
(`openai-completions.ts:478-510`) does not even count a llama.cpp progress chunk as progress, so a
long prefill races a 300 s idle timeout. Here they are first-class fields in a protocol we control.
That is the fused-seam argument in miniature.

### 5.7 No output cap, and `finish_reason: length` is acted on

opencode capped at `OUTPUT_TOKEN_MAX = 32_000` (`provider/transform.ts:18`) and then parsed
`finish_reason` and threw it away (`session/processor.ts`, `case "finish": return`). A subagent spent
its whole budget thinking and returned an empty result, recorded as success — nine such rows in the
database. That is F5 exactly.

The policy, taking Grok's `LengthPolicy`
(`xai-grok-sampling-types/src/conversation.rs:513-575`) and pi's message:

```
on finish_reason == "length":
  if the turn produced NO visible content and NO tool calls   → HARD FAIL the turn.
  if the turn produced ONLY reasoning                         → HARD FAIL the turn.
  if there are tool calls:
      all arguments parse as complete JSON → keep them, continue
      any argument truncated              → FAIL THE WHOLE BATCH, none executed
  otherwise (real text)                                       → keep, mark truncated, continue
```

Grok's `empty_reason()` names the two fail cases `NoVisibleContent` and `ReasoningOnly`, and *an
empty Length response fails under every policy value* — confirmed at `:569-573`. Take that.

When a batch is failed, tell the model why, in the tool result, not in a log. pi's wording is good
enough to port: *"Tool call X was not executed: the response hit the output token limit, so its
arguments may be truncated. Re-issue the tool call with complete arguments."* And bound the retries:
Grok's `length_salvage_streak_proceeds_to_the_cap_then_exhausts` exists because an unbounded salvage
loop is its own failure mode.

Surfacing: `TurnFinished{finish_reason}` reaches every head, and a `length` finish is rendered
distinctly. A truncated turn is never reported as a completed one.

### 5.8 Steering — messages that arrive mid-turn

A human may say something while the model is generating. §3.7 makes that a pushed event rather than a
poll; this is what the turn does with it.

**Three options, and the choice.**

- *Queue until the turn ends.* Simple, and wrong for the common case: the message is usually a
  correction (*"the spec changed — RFC 2812 rather than 1459"*, firecode's own example) and waiting
  for a 90-second generation to finish before acting on it wastes the generation.
- *Interrupt generation immediately.* Responsive, but it truncates mid-token-stream, and a partial
  assistant turn has to be kept for cache reasons (§13.2) while being useless as content.
- **Inject at the next step boundary. Chosen.** A step boundary is: after the current generation
  completes, or after the current tool batch settles, whichever comes first. The message is appended
  as a `User` item at that point and the loop continues with it in context.

This is pi's shape (`agent-loop.ts:205-206` pushes steering messages into the running loop) and it is
right for the same reason: it keeps the token vector append-only with no partial-turn bookkeeping,
while bounding latency to one generation or one tool batch — seconds to a minute on this box, not the
whole task.

**With an explicit escape hatch.** A message flagged `urgent` (a client affordance, and `ABORT` is
its extreme form) interrupts generation at the next token, and the partial assistant output is kept
as a transcript item with `truncated: true` — for the cache reason in §13.2, and because the model
should see what it had started to say.

**What steering must not do:** it must never rewrite or reorder anything already in the vector. A
steering message is an append like any other. That is why this is three lines of policy rather than a
subsystem.


---

## 6. `EXPLAIN` — per-stage observability as a feature, not as logging

### 6.1 Why it is a feature

Three of oracle's eight diagnoses that evening were wrong, and the cause each time was the same:
*"I have no way to measure search on its own. My tests grade the final answer — so when an answer is
wrong, I can't tell whether search missed it or qwen fumbled it."* The same shape recurred here: a
misread `f_sim`, a change wired into a module that never executes, a compaction fix that was inert.

"Why did this request prefill 144,436 tokens with zero cache hits?" is a query-plan question. It took
hours of log archaeology. It should take one command.

### 6.2 The plan object

Every turn records an `ExplainPlan`, persisted with `turn_metrics` and emitted as an `Explain` event:

```
Turn 8f3c  session=lab/glm  model=glm-5.3-flash  dialect=d41d8c…  transcript=T7 (fork of T6 @ item 212)

Prompt construction        items 0..268   rendered 402,118 B   ledger head 7a19c2…
  stable prefix            id 0c81…   4,192 tok    UNCHANGED since turn 1
  transcript               264,001 tok
  appended this turn       1 user item, 512 tok
  expected common prefix   with turn 7:  268,193 tok   (ledger match to item 267)

Cache                      prompt_tokens 268,705   cached_tokens 268,193   f_keep 0.998
  divergence               none
  server entry             matched, lcp 268,193, f_sim 0.998

Prefill                    processed 512 tok in 1.16 s     (441 t/s)
Decode                     1,284 tok in 31.7 s  (40.5 t/s)  draft 2,412/2,904 accepted (0.83)
Finish                     stop

Tools                      3 calls  |  2 ok  1 abstained  |  perms: 1 asked, allowed in 42 s
```

A cold turn shows the interesting case:

```
Cache                      prompt_tokens 144,436   cached_tokens 0   f_keep 0.000
  divergence               at item 3 (System, origin=Update)
                           expected h 7a19c2…, computed h 91be03…
                           cause: stable prefix changed (0c81… → 55ad…), field <env>
  cost                     144,436 tok re-prefill ≈ 327 s at 441 t/s
```

That last block is the deliverable. It names **which prefix matched, where it diverged, what caused
the divergence, and what it cost** — the four things that took an evening to establish by hand.

### 6.3 Where the numbers come from

All of it is already on the wire; nothing surveyed reads most of it.

| number | source |
|---|---|
| `prompt_tokens`, `cached_tokens` | `usage.prompt_tokens_details.cached_tokens` — a **standard** OpenAI field, populated from `n_prompt_tokens_cache` (`server-task.cpp:380`). No extra body needed. |
| live prefill progress | `prompt_progress {total, cache, processed, time_ms}` under `return_progress: true` |
| prefill / decode rates, draft acceptance | `timings` (per chunk under `timings_per_token`, and on the final chunk) |
| box-level cache health | `GET /metrics` (Prometheus; `--metrics` is on) |
| per-slot residency | `GET /slots` |
| which cached entry matched, and its `lcp` | `Explain{turn_id}` over the control channel (§3.4.6). Fallback before that lands: the server log at `-lv 4`, one line per entry with `f_keep`/`f_sim`/`lcp` — **chatty** (161 entries = 161 lines/request), so enabled only for an explicit `explain --deep`. |
| admission, seats, ladder rung, batch-iteration sharing | `Explain` and `Stats` (§3.3). Not obtainable at all across an HTTP boundary. |
| expected common prefix | ours, from the ledger (§4.3) |

The last row is the one that turns a measurement into a diagnosis: the server can say *what* matched;
only the harness knows what *should* have matched, and the difference is the cause.

**Fused, this is one plan and not two logs.** `Explain{turn_id}` over the control channel returns the
server's half — the matched entry and its `lcp` / `f_keep` / `f_sim`, which ladder rung was active,
whether admission deferred the request, and how many batch iterations the prefill shared with other
slots (the third resource, §3.4.6) — and the harness prints it inline with its own half. Every
correlation that was done by timestamp this week becomes a field. Before that lands, the same
information is recoverable by scraping the server log at `-lv 4`, which `~/bin/glm-why-no-cache`
already does; that is the fallback, not the design.

### 6.4 Stage boundaries that must be independently measurable

Not just the model call. Every stage gets its own counters and its own line in the plan:

- **prompt construction** — items, rendered bytes, ledger head, time to build;
- **cache** — expected vs actual common prefix, divergence point and cause;
- **prefill** and **decode** — separately, always with their concurrency (a tok/s number without its
  concurrency is not a number: a contended run gave prefill 391.7 with decode 21.4, and the halved
  *decode* was the tell);
- **tool execution** — per call: wall time, bytes returned, bytes inlined, outcome class;
- **permissions** — asked, answered, by whom, after how long;
- **render** — per head: events delivered, bytes, drops, resyncs.

---

## 7. Per-model dialects — renderers we own

### 7.1 What a dialect is now

Under §3.1 a dialect stops being *a set of compat flags describing someone else's renderer* and
becomes **a renderer**. This is a bigger change than it sounds: the compat-flag approach means every
model's quirks are expressed as knobs on a foreign template engine, and every knob is a guess that
can silently stop being true. Owning the renderer means the quirks are code we wrote, tested against
the shipped template in CI.

```
trait Dialect {
    fn render(&self, prefix: &StablePrefix, items: &[TranscriptItem]) -> Vec<RenderSpan>;
    fn render_incremental(&self, prev_end: usize, new_items: &[TranscriptItem]) -> Vec<RenderSpan>;
    fn parse(&self, tokens: &[llama_token]) -> Vec<ParsedSpan>;   // reasoning, tool calls, content
    fn control_tokens(&self) -> ControlTokens;                    // turn starts/ends, think open/close
    fn stop_tokens(&self) -> &[llama_token];
    fn system_update_mode(&self) -> SystemUpdateMode;             // in-history | envelope
    fn guards(&self) -> &[Guard];                                 // e.g. repetition collapse
    fn template_sha(&self) -> [u8; 32];                           // of the jinja it was validated against
}
```

Two invariants the trait must satisfy, and they are what the tests check:

- **`render_incremental` must agree with `render`.** Rendering items `0..k+1` from scratch must equal
  rendering `0..k` and appending item `k+1`. This is the property the whole append-only design rests
  on, and it is a pure function, so it is property-testable exhaustively without a model
  (§18.3).
- **`parse ∘ render` is the identity on the round-trippable parts.** Render an assistant turn with
  reasoning and two tool calls; parse the resulting tokens; get the same structure back. This is what
  catches a control-token mistake before a model does.

A dialect is versioned by `template_sha` — the hash of the shipped jinja it was last validated
against. A model update that changes the template invalidates the dialect automatically, which is
cheap and closes a class of silent failure.

### 7.2 Validation against the shipped jinja — a required CI check

This is the mitigation for the one real hazard self-rendering introduces: **a silent divergence
between our renderer and what the model was trained on** is exactly the failure mode this whole plan
exists to eliminate, so it must be a gate, not a nice-to-have.

`POST /apply-template` runs the *same* `oaicompat_chat_params_parse` as `/v1/chat/completions` and
returns `{"prompt": "<rendered string>"}` (`server-context.cpp:7745-7755`). It touches no slot, no
sequence and no GPU. So it is a free oracle:

```
for each dialect D, for each fixture F in the corpus:
    ours   = detokenize(D.render(F))
    theirs = POST /apply-template {messages: F.messages, tools: F.tools, ...}
    assert ours == theirs        # exact string equality
```

**This is a stronger test than the byte-prefix assertion alone**, and it subsumes it: if our render
equals the shipped template's render for every prefix of every fixture, then prefix stability and
template fidelity are both established in one check.

Rules around it:

- **Required check.** A dialect whose diff fails does not ship, and a model whose diff fails is not
  admitted (§7.4). No "compat quirk" escape hatch — a divergence is either a bug in our renderer or a
  template change we must adopt.
- **Runs in CI on every commit**, because it costs no inference. Runs again on every server upgrade
  and every model file change, keyed by `template_sha` mismatch.
- **The fixture corpus is the interesting artefact**, and it must cover the shapes that actually
  break: a plain turn; an assistant turn with reasoning; reasoning interleaved with two tool calls; a
  tool result; an *empty* assistant turn (the `content: ""` case); a system message at index > 0; an
  image; a very long turn that crosses whatever internal limit the template has; and the same fixture
  truncated at every prefix boundary.
- **Where they legitimately differ**, the dialect records it explicitly with a reason — e.g. we may
  deliberately emit a mid-history system turn the shipped template drops. Such an exception is a
  named, reviewed field, not a silent inequality, and each one carries the eval that justifies it.

**When a model ships a template change:** `template_sha` mismatches, the dialect is marked stale, the
diff runs and reports exactly which fixtures moved, and someone decides whether to adopt the change.
Adopting it forks every live transcript on that model (§5.5), because the render changed — which is
correct and which should be visible, not silent.

### 7.3 The three targets

**GLM-5.3-Flash** — production. Thinking family `zai`; ctx 262,144 per slot against a 327,680 elastic
pool (`n_ctx_train` is 1,048,576, so 262k is our choice); MTP draft at `n_max=2`; vision via
`--mmproj` with the projector in host RAM. Guard: the historical repeating-token collapse past ~78k
at `-ub 512` **poisons the slot** rather than erroring; it does not reproduce on this build (probed
to 147,042 tokens) but §8.5's n-gram guard costs nothing and 1M has never been run.

**Qwen Flash (`qwen-3.8-flash-next`)** — thinking family `qwen`. What was a `preserve_thinking`
puzzle across three transports (top-level for llama.cpp's `--jinja` hook, `chat_template_kwargs` for
schemas with `additionalProperties: false`, per `openai-shared.ts:1064-1079`) becomes one decision in
our renderer: **thinking blocks for prior turns are emitted, always.** Its preset also forces the
50.66 GB PLE table to CPU (`override-tensor per_layer_token_embd\.weight=CPU`) and runs
`spec-draft-n-max 5` with a separate MTP GGUF — server-side, and relevant to the harness only in that
switching models is expensive.

**Qwen 3.8 27B dense** — the model file exists (`Qwen3.8-27B-UD-Q6_K_XL.gguf` plus an mmproj) but
**there is no preset in `router-presets.ini`**; adding one is a prerequisite. Expected to share the
Qwen renderer; §7.2's diff decides, and if it diverges it gets its own.

**Model switching is expensive and the harness must know it.** With `--models-max 1` a switch evicts
the other model; GLM reloads in 29–31 s and the evicted child spills its prompt cache to disk and
re-indexes on return. So the model is a property of the **session**, not the turn; a session that
changes model **forks** its transcript (§5.5), because the renderer and therefore every token
changed; and the model client batches same-model work rather than interleaving per turn.

### 7.4 Adding a fourth model

1. Add a preset to `router-presets.ini`.
2. Write the dialect — or declare that it reuses an existing one.
3. **`/apply-template` diff across the whole fixture corpus. Hard gate.**
4. Property-test `render_incremental ≡ render` and `parse ∘ render ≡ id`.
5. A short generation to confirm the parser recovers reasoning and tool calls from real output, and
   one `n_predict: 1` call to confirm `finish_reason` classification (§5.7).
6. `Admit` a session on it and run the dialect-parameterized conformance suite (§14) — one command,
   no new test code.

**What must be verified, as the checklist:** exact render equality against the shipped template for
every fixture and every prefix; incremental-equals-full rendering; parse round-trip; reasoning and
tool-call recovery on real output; `finish_reason` classification; and the generation-inclusive cache
invariant (§18.1-I1) over two real turns. If step 3 fails, the honest options are to fix our renderer
or to patch the shipped template — **not** to ship a divergence.

## 8. The tool contract

F4 says close the loop; F5 says never let a component's "I did not do this" be reported upward as
success. Both live here, because a tool's failure mode is a design decision whose cost is paid in
**permanent context**, not in one wasted call.

### 8.1 The contract every built-in tool must meet

Six clauses. A tool that does not meet all six does not ship.

1. **A miss is self-correcting in the SAME call.** oracle's rule, and its measured example: the model
   guessed a CSS selector, the tool answered *"that selector was a guess and the page does not have
   it — call read_page with NO selector"* **and handed back the whole page anyway**; the model
   immediately found the real tabs. A bare "not found" leaves the model with nothing to correct
   itself with, so it guesses again — and the retry dance is what bloats context. Concretely: a
   `grep` that finds nothing under a scoped path reports where the term *does* occur; a `find` that
   misses returns the surrounding listing and what *would* have matched; a too-strict anchor is
   auto-relaxed to the bare identifier and the relaxation is reported.
2. **Malformed input is salvaged, not rejected.** oracle's shim took leaked `<function=…>` tool calls
   from 33% dropped to 0% by parsing them rather than asking for better formatting. Our targets are
   the same size class of model. A best-effort parse plus an explicit note of what was repaired.
3. **Outcome is a closed vocabulary, and abstention is not success.** `Ok | Abstained | Failed |
   Denied | Timeout | NotRun` (§4.2). `Abstained` must be **structurally** distinguishable in the
   tool result the model sees, not merely worded differently — see §8.2.
4. **Read/write is declared in the schema, not decided per call.** oracle deliberately shipped no
   write-capable filesystem tool: *"you want the model advising, not editing your tree unattended."*
   Our harness does need write tools, but the split is in the contract: `access: read | write |
   exec | network`, which is what §11's policy keys on. A read-only tool never prompts; a write tool
   always does unless policy says otherwise.
5. **Output is bounded and spilled, never truncated.** §8.3.
6. **The description says how to use the tool and never what the data contains.** §5.2. Tool
   descriptions are prompt, are never audited, and go stale.

### 8.2 Making abstention impossible to paper over

The measured failure: retrieval said "The corpus doesn't cover this"; the model invented a search
term, found an adjacent passage, relabelled it, dropped two items and attached a citation.
*"A hallucination wearing a footnote is more dangerous than a naked one."*

The harness cannot stop a model from writing a confident sentence. It can stop the *system* from
reporting that sentence as grounded. Three mechanisms, all structural:

- **The outcome class is carried, not just the text.** A `ToolResult` with `outcome = Abstained`
  renders into the prompt with an unmistakable envelope and is recorded in the journal with its
  class. The model sees `NO_RESULT` as a distinct token sequence, not as prose it can quote around.
- **Abstention propagates upward.** A subagent whose tool calls all abstained cannot return
  `Ok`. Its result carries `Abstained` and the parent's `ToolResult` is `Abstained`. This is the
  direct fix for the empty-subagent bug one layer up: **the harness never converts "no result" into
  "a result".**
- **A turn that produced no visible content fails (§5.7).** Empty is not a value.

What the harness does **not** do: judge whether the model's prose is supported by the tool output.
That is a quality question and it belongs in an eval, not in the loop.

### 8.3 Spill, never truncate

Take DeepSeek's mechanics (`packages/spill/spill-policy/src/index.ts`, `util/output-retention`):

- one configured threshold, `max_inline_bytes` (UTF-8 bytes), no default — unset means the policy is
  a genuine no-op rather than a silent guess;
- the preview budget is `cap − reserve`, where `reserve` is the exact byte cost of the notice
  computed against a worst-case digit count, so `preview + "\n\n" + notice` **never exceeds the
  cap**; if the notice alone would exceed it, spill is abandoned and the original is left inline
  rather than truncated silently;
- head/tail split of the *preview* budget: `head = ceil(budget/2)`, `tail = floor(budget/2)`;
- notice wording: `(Omitted <N> bytes. Full <kind> result stored at: <locator>. <hint>)`;
- the full original is written `0600` under a per-session hashed directory, and a storage failure
  falls back to the untouched inline content rather than erroring the call.

Ours differs in one way: the locator is a **content hash into the `tool_spill` table**, and there is
a `read_spill(hash, range)` tool. So the "hint" is actionable rather than advisory, which is
clause 1 applied to spilling itself.

### 8.4 Tool budget — a hard ceiling, enforced

oracle measured it on comparable local models: past ~5–7 MCP servers small models get *worse* at
choosing tools. On his box, 15 tools tested OK; 23 was *"near the edge"*. Our three targets are local
models, so this binds.

The design consequence is **task-focused agent roles, not one mega-agent**, and the ceiling is
enforced rather than hoped for:

| role | tools | budget |
|---|---|---|
| `orchestrator` | task, read, grep, glob, ask_code, ask_corpus | 6 |
| `coder` | read, write, edit, grep, glob, bash, read_spill | 7 |
| `researcher` | ask_corpus, search_corpus, ask_code, read, grep, read_spill | 6 |
| `reviewer` | read, grep, glob, git(read-only), read_spill | 5 |

Enforcement, in the F4 sense (a hard stop, not a guideline):

- a role declares its tool set; the daemon **refuses to start a session** whose resolved tool count
  exceeds `max_tools` (default 8), naming the overflow;
- MCP servers are attached **per role**, not globally, and an MCP server that advertises more tools
  than the role's remaining budget is refused with its tool list, not silently truncated;
- the count is in the `EXPLAIN` header, so nobody has to guess.

This ceiling is also the argument for subagents on this box, and it is worth stating precisely
because the naive argument is wrong: **subagents pay off through context hygiene, not decode speed**
— decode is flat against depth (F2). A subagent's exploration never enters the parent, so the
parent's depth, and therefore its *prefill* bill, stays bounded. The cost is that many shallow
contexts is the ID-BOUND regime, which is the regime this box is short of (437 MiB fixed per
sequence, ceiling 5 on CUDA0). So subagent concurrency is a **configured** number the operator can
move, reported in `EXPLAIN`, not an unbounded fan-out.

### 8.5 Two guards that cost nothing

- **Repetition collapse.** GLM's historical failure mode emits endless `@@@@…` at ~3 t/s and
  *poisons the slot* until restart. A rolling n-gram check over the last K generated tokens, aborting
  the turn and raising `Warning{code: repetition_collapse}`, is a few lines and turns a silent
  garbage answer into a named event. Per-dialect `guards` (§7.1).
- **Server-warming.** After `--sleep-idle-seconds 600` the first request pays a 29–31 s model reload.
  The harness must have an explicit *warming* state with no idle timeout below ~120 s, and must count
  `prompt_progress` chunks as liveness. omp's cautionary tale: `isOpenAICompletionsProgressChunk`
  (`openai-completions.ts:478-510`) counts only `usage`, `finish_reason` and non-empty deltas, so a
  llama.cpp progress chunk resets nothing and a long prefill races the 300 s idle timeout.

---

## 9. Context occupation vs cache stability — the tension, resolved

Two load-bearing facts point in opposite directions.

- **F1/F2 (cache):** the prompt must be append-only and byte-stable; reasoning must be replayed
  verbatim; history must never be reshaped mid-flight.
- **F3 (quality):** context occupation defocuses; minimising it is a quality measure.

And the cost is not small: measured today, reasoning was **~51.7%** of the conversation's content —
69,537 reasoning tokens against 6,721 tokens of assistant text. Replaying it roughly **doubles**
occupation.

### 9.1 The position

**Keep the prompt append-only and byte-stable. Make what goes into it dense. Reclaim occupancy at
compaction, on a schedule, never by reshaping history mid-flight.**

Concretely, in priority order:

1. **Nothing enters the transcript that does not have to be there.** This is where the quality budget
   is actually spent, and it is entirely within the harness's control: tools return what is needed
   and no more (§8.1), oversized results spill (§8.3), no restated framing, no padding, no
   re-injected reminders that repeat what is already three turns up, no "here is what I will do
   next" narration recorded as a transcript item. The Journal exists precisely so that UI-shaped
   content never reaches the prompt.
2. **Reasoning is replayed verbatim, and that is not negotiable while llama.cpp caches generated
   tokens.** Dropping it is what produced the 612 GB and the 78,598-token p99. The doubling is real
   and it is the price.
3. **Occupancy is reclaimed by compaction (§10), which is scheduled, incremental, and cheap** —
   which is what makes (2) affordable. This is the whole reason §10 is high in the phasing rather
   than a late nicety.
4. **Depth is preferred over premature compaction.** Given F2, a 150k warm conversation costs about
   the same per turn as a 15k one, and compacting it early trades a real cold prefill for a
   speculative quality gain. The wall we compact against is the *context window and the memory
   budget*, plus the quality signal in §9.3 — not a token count someone picked.

### 9.2 Why reasoning is the *right* half to keep

An asymmetry that makes the trade less painful than the 51.7% suggests. Reasoning tokens are:

- the part the model itself produced to reach the answer, so replaying them is the closest thing to
  giving it its own working memory back;
- the part a *summary* reconstructs worst — a summarizer restating a chain of thought is where
  fidelity loss is largest;
- and, unlike tool output, not something the harness can make denser: a tool result can be spilled,
  a thought cannot be re-run.

Tool output is the opposite on all three counts, which is why §8.3 spills aggressively and §10
compacts tool-heavy segments first.

### 9.3 What would falsify this position

State it, because the position is a judgement and not a measurement:

- **Falsifier A.** Run the same task set with reasoning replay on and off (accepting the cache cost
  of "off"), scoring answer quality, not speed. If quality is *better* without replay at equal
  depth, then reasoning is noise rather than working memory and the trade inverts — at which point
  the right answer is a server change (a `reasoning_content` field the template renders into the KV
  but that a later turn can drop without shifting positions, i.e. thinking in its own segment).
- **Falsifier B.** Measure quality against depth on this stack: same task at 20k, 60k, 150k of
  preceding context. If quality falls off a cliff well before the context window, then §9.1 clause 4
  is wrong and compaction must be aggressive and early, whatever it costs in prefill.
- **Falsifier C.** If §10's incremental compaction cannot hold summarization cost near zero — if the
  measured summarizer call does not hit the warm prefix (§18.1-I5) — then clause 3 is unfunded, and
  the honest fallback is to cap depth much lower.

Falsifier B is the important one and nobody has run it. It is milestone M3.5 in §17, not a footnote.

### 9.4 Retrieval stays out

oracle's retrieval findings — the embedder measures resemblance not truth (a bats passage at 0.762
against the correct answer at 0.471), reranking can only reorder what stage 1 found, recall@64 is the
number that matters — are about RAG. This harness does no retrieval. It calls `ask_corpus` /
`ask_code` / `search_corpus` as tools and treats their abstention per §8.2.

The one carry-across is negative and worth stating: **the harness must not silently "improve" a
tool's query.** oracle's actual bug was the model rewriting its own search query and then relabelling
what came back. If the harness ever adds query rewriting for a retrieval tool, the rewrite must be
visible in the tool result. It should not add one.

---

## 10. Compaction, as leveled compaction

### 10.1 The reframe

Today's compaction is a stop-the-world full pass: opencode flattens the whole conversation into one
user message with `system: []` and `tools: {}` (`session/compaction.ts:380-445`), sharing a prefix
with nothing — measured 144,436 tokens, cache hit 0, **~13 minutes** before the first token of
summary. omp's default summarizer has the same shape (`compaction/compaction.ts:965-1004`:
`{systemPrompt: [SUMMARIZATION_SYSTEM_PROMPT], messages: [one synthetic user message]}`). pi
flattens too and sets `cacheRetention: "none"` deliberately.

That is pre-LSM compaction: one blocking pass over everything, at an arbitrary moment, costing more
the better the system has been working.

LSM trees solved this. The mapping is clean:

| LSM | here |
|---|---|
| memtable / L0 — recent, unmerged | the last N turns, verbatim |
| L1 — small runs, merged | a segment summarised once, still detailed |
| L2… — progressively larger, coarser | older segments merged and re-summarised, coarser each level |
| compaction runs in the background, incrementally | summarize one segment per idle window, never the whole history |
| write amplification | **fidelity amplification**: each re-summarisation of the same material loses detail |
| levelled vs tiered | how aggressively segments are merged before being re-summarised |

The reframe does not make the 13-minute stall faster. **It removes it as a category**, because no
single operation ever spans the whole history again.

### 10.2 Levels

```
L0   the last N turns (default 8) and everything since the last user boundary.  VERBATIM. Never
     summarised. This is the append tail and it is where the KV cache is warm.

L1   segments closed but recent (default: last 3 closed segments). Summarised ONCE, at ~1:4.
     Tool-heavy content already spilled to §8.3, so mostly reasoning and decisions.

L2   older segments. Merged in groups and re-summarised at ~1:4 of L1 (≈1:16 overall).

Lc   the cold floor: one running "what this session is about" paragraph per session, plus an index
     of segment labels and their spill locators. Never discarded.
```

Two properties to hold:

- **Levels are monotone in age but not merged across a live boundary.** A segment the operator is
  actively working stays at L0/L1 regardless of clock age; `last_touched` on the segment is what
  moves it, not `created_at`. That is the operator's "different decay rates" requirement (§19.1)
  falling out of the level rule rather than being bolted on.
- **Fidelity amplification is bounded by construction.** A segment is re-summarised at most once per
  level, and the number of levels is fixed at 4. So no piece of history is ever summarised more than
  three times, and the plan records for each segment how many times it has been through — visible in
  `EXPLAIN`, so "why does this read vague" has an answer.

### 10.3 Warm compaction — the original work

**Nobody in the survey does this for the default path.** Grok Build does
(`build_compaction_chat_history`, `prepared_compaction_history.rs:49-58` — takes the live
`chat_history`, pushes exactly one `ConversationItem::user(prompt)`, and by default builds the
history via `prepare_conversation_for_verbatim_summarization`, i.e. the real, full, live items) and
DeepSeek does (`buildSummarizationInput`, `region.ts:528-546` — system node 0 + the header's tool
schemas + every shadowed node's derived message in surface order, then `summarizeWithLlm` appends
`COMPACTION_INSTRUCTION` as the final user message, with the rationale written down: *"Keeping the
conversation's own system prompt, tools, and message prefix in front of it makes the auxiliary call
a genuine prefix of the last routed request, so the provider's KV cache is reused instead of
invalidated"*). Neither opencode, omp's default path, nor pi does.

Our rule: **the summarizer request is the last routed request, byte-for-byte, plus one appended user
message.**

Because the transcript is append-only and ledger-chained, this is not an approximation — it is
literally `render(stable_prefix, items[0..k]) ++ render(instruction_item)`, and the ledger proves it.
Expected `f_keep` ≈ 1.0 and prefill ≈ the instruction's own length. §18.1-I5 asserts it.

For a **segment** summary rather than a whole-history one, the same trick applies with a wrinkle: the
summarizer needs the segment *and* enough surrounding context to summarise it well, but the warm
prefix is the whole conversation up to the segment's end. So the instruction names the segment by its
`SegmentMark` ids — *"summarise only the span between marks S7 and S8"* — and the prefix stays the
full live one. Cost: a segment summary of a mid-conversation span costs the same warm prefix as a
tail summary. Benefit: it is free.

### 10.4 Applying the result: soft vs hard

Two distinct operations, and the distinction is not made anywhere in the survey:

**Soft compaction (default).** Append the summary as an `in-history` system update (§5.3) or a
marked user item: *"Segments S3–S5 are summarised below; rely on the summary rather than re-reading
those turns."* Nothing is removed.
- Cost: **zero** prefill. It is an append.
- Frees: no tokens. It reduces *attention pressure* by telling the model what to lean on, not
  occupancy.
- Use when the constraint is F3 (quality) and there is context-window room. On this box, with 262k
  per slot and F2's flat decode, that is most of the time.

**Hard compaction (at the wall).** Fork the transcript (§5.5): `[stable prefix][Lc floor][L2
summaries][L1 summaries][L0 verbatim tail]`. This is Grok's shape
(`code_compaction/assemble.rs:62-102`: system → user-meta → project instructions → last user query →
recent verbatim → summary), with the ordering detail they tested for
(`grok_build_order_recent_before_summary`) and the reason for re-injecting project instructions
verbatim: so they survive compaction independently of what the summarizer chose to keep.
- Cost: one cold prefill of the new, shorter prompt — `(n_new)/441 t/s`. For a 40k post-compaction
  prompt that is ~91 s, once.
- Frees: real tokens and real KV.
- Use when the context window or the memory budget actually binds.

**The scheduler for both.** Soft compaction runs opportunistically: when a segment closes and the box
is quiet, summarise it (warm, ~free) and append. Hard compaction runs only when
`prompt_tokens > high_water` (default 80% of the slot's `n_ctx`) — and by then every segment already
has a summary sitting in the journal, so the fork is an assembly, not a model call. **That is the
thing that removes the 13-minute stall**: at the moment compaction is needed, all the summarizing has
already happened.

### 10.5 Should this share a policy with the server's cache ladder?

The server's prompt-cache eviction ladder (GLM-STATE "Open, ranked" item 3) has the same shape:
degrade the least-degraded entry first, spread the loss evenly, LRU within a rung, spill only when
skeletal. Our compaction is: coarsen the least-coarsened segment first, spread fidelity loss evenly,
oldest-touched within a level.

**They should stay independent, and share vocabulary rather than policy.** Three reasons:

1. They optimise different things. The server ladder trades *restore cost* against *bytes*; a
   degraded entry still restores the same conversation. Compaction trades *information* against
   *tokens*; a coarsened segment does not restore — the detail is gone unless it was spilled.
2. Their units differ. The server's unit is a whole cached prompt (one conversation). Ours is a
   segment (part of one).
3. Coupling them creates a feedback loop nobody wants: memory pressure on the server would silently
   degrade *answer quality* in the harness. Memory pressure should cost latency, never fidelity.

What they *should* share: the vocabulary (`degrade` / `spill` / `drop`), the "least-degraded first,
LRU within a rung" rule because it is right in both places for the same reason, and the `EXPLAIN`
surface — one place that says what fidelity everything currently has, at both layers.

### 10.6 Where the summary lands in the prompt

Grok tested this and the answer is not obvious: **recent verbatim before the summary**
(`grok_build_order_recent_before_summary`). And they keep reasoning in the retained tail explicitly
— `session_recap.rs`: *"Reasoning is kept so the prefix KV cache stays warm."*

---

## 11. Adjudication — one interface, three adjudicators

### 11.1 The requirement, in the operator's words

> *"we are flexible, it is ok to ask me, it is ok to use firecode and it is ok if i plug a model for
> auto mode."*

So this is **a pluggable adjudication layer, not a doctrine**. An action that needs a decision is
routed to an adjudicator. There are three, all first class, and the design's job is to make them
interchangeable rather than to rank them:

| adjudicator | decides by | typical case |
|---|---|---|
| **boundary** | structure — the action is safe because of where it runs | almost every tool call inside a firecode run |
| **human** | judgement, over flowy or `firecode-chat --ask` | genuinely ambiguous effects; *decisions the agent cannot make on the information available* |
| **model** ("auto mode") | a separate, probably smaller model | the middle band: too many to ask about, too consequential to wave through |

The volume of human asks is an **observable, not a constraint**. If it is high, either the boundary
is drawn wrong or the policy is misrouted — and §6's `EXPLAIN` surface is where that shows up.

### 11.2 One decision shape

Every adjudicator answers the same question and returns the same object, which is what makes them
swappable and the audit uniform.

```
AdjudicationRequest {
  id            ULID
  session_id, turn_id, tool_call_id
  agent         "claude-lab2x1" | subagent seat
  action        { class, tool, summary, arguments, arguments_digest }
  context       { run_id, workspace, boundary_facts, prior_decisions_for_this_class }
  kind          Permission | Question          // §11.6
  options       [ {id, label, kind: allow_once|allow_session|allow_always|deny|deny_and_tell|<free>} ]
  deadline      instant
  on_timeout    Deny | Allow | AgentDecides    // §11.5
}

AdjudicationDecision {
  request_id
  outcome       Selected{option_id} | Escalate{to, why} | Unavailable | Cancelled | Timeout
  by            adjudicator id ("boundary:firecode", "human:deadtrickster", "model:qwen-3.8-27b")
  basis         short text — the reason, always present, never optional
  at, latency_ms
}
```

The closed outcome vocabulary is dsh's, and its property matters: *a throwing or non-conforming
answerer becomes `unavailable`, never silently opens the gate* (`docs/subsystems/approval.md`).
`Escalate` is the addition, and it is first class rather than an error path (§11.4).

### 11.3 Routing policy is configuration

An **action class** is the routing key. It is derived, never free text, from four facts the harness
already has:

```
class = (tool.access, effect_scope, reversibility, cost)

tool.access     read | write | exec | network         declared in the tool schema (§8.1 clause 4)
effect_scope    in_run | host_project | host_other | external
reversibility   reversible | irreversible
cost            free | metered
```

Policy is a table the operator edits, first match wins:

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

Three properties this shape buys:

- The operator moves a whole band between adjudicators by editing one line — "put writes to the host
  project on auto for tonight" is one edit, not a code change.
- A class that is never seen in the log is a class nobody has thought about; §6 lists observed
  classes with counts, so the policy can be reviewed against reality.
- `boundary` rows cost nothing at runtime: they are evaluated in-process, produce a `Selected`
  decision with `basis = "in-run, no host effect"`, and are recorded but not delivered anywhere.

### 11.4 The boundary adjudicator

It decides by structure, and the structure is firecode's (§3.9). What makes an in-run action safe is
worth writing out, because the policy above leans on all of it:

- the guest sees a copy of one project and nothing else of the host: no host filesystem, no host
  processes, no host devices;
- `--add-dir` and `--workdir` **refuse outright** to carry `.ssh`, `.gnupg`, `.aws`, `.kube`,
  `.config/gh`, `.password-store` or a browser profile out of the home directory;
- the host project directory is not modified by a run that was not asked to write back (invariant 6);
  work is copied to a sibling directory on delivery;
- nothing a VM writes reaches the host except through the project directory or a disk explicitly
  attached writable (invariant 11);
- project paths are resolved from the spawn server's config, never from the caller;
- lifetime is a cgroup, so cleanup is total and transitive.

What the boundary explicitly does **not** protect, stated because the policy must not pretend
otherwise: **API credits and the network**. The agent has real credentials because otherwise it
cannot work. So `metered` and `network,external` are routed away from `boundary` in §11.3, and that
is not an oversight.

**When the boundary denies** — an action whose class says `in_run` but whose arguments would leave
the run — it **escalates rather than hard-failing**, because the common case is a tool call that is
merely misaddressed (a path outside the workspace, usually a mistake). It escalates to whatever the
policy names for the *actual* class of the action, which is the honest routing: the agent asked to
do something else than it thought.

### 11.5 The human adjudicator, and `--ask` semantics

The semantics here are firecode's, learned by getting them wrong, and they are kept **precisely
because the human path is comparatively rare**: a rare ask that silently hangs is worse than a
frequent one.

- **Post and wait.** `--ask` blocks.
- **If nobody answers, say so, and tell the agent to decide and record what it chose.** That is
  `on_timeout: AgentDecides`, and it is the right default for a *question* (§11.6) — not for a
  permission over an irreversible external effect, where the default is `Deny`.
- **Acknowledge before you act.** *"Silence is indistinguishable from absence: an agent here waited
  three minutes for an answer, concluded nobody was coming, and went and fixed the thing itself."*
  Mechanically: a human client that opens a request sends an `Ack`, which the daemon records and
  which extends the deadline. Without an ack the agent knows nobody is there and the timeout runs to
  its stated default rather than to a hope.
- **Start listening before you start working**, and start again each time the reader returns.
  In our shape the daemon holds the listener for the life of the session, so this becomes a daemon
  invariant rather than a convention: `SessionStarted` is not emitted until the flowy connector
  reports a live subscription, and a lost subscription raises `Warning{code: no_human_channel}` and
  *changes the effective policy* — with no channel, `human` rows fall through to their `on_timeout`
  immediately rather than waiting fifteen minutes for a message nobody will see. This is the direct
  fix for the failure `~/.claude/CLAUDE.md` already records: a message addressed by name into the
  wrong project was never delivered, and silent non-delivery is indistinguishable from a quiet room.

**flowy has no blocking ask at all**, and its nearest primitive is a better shape than one.
`agentanswer.go` is a terminal *query answerer* (the write-side twin of `agentscrub.go`), not an ask.
What does exist is `todo_waiting_on` (`mcp_waiting.go`): **non-blocking, board-visible, and
self-clearing** — any note or write by the named person *is* their answer.

Adopt that shape for the human adjudicator rather than a blocked call:

- a pending decision is a **visible row** with an owner, not a held connection — so it survives a
  connector restart, appears in the roster, and can be seen by someone other than the asker;
- **any relevant write by the named person clears it.** If the operator answers a question by simply
  doing the thing, or by saying the answer in the room without quoting a request id, that counts.
  This is the single most human-friendly property in flowy's design and it costs us nothing to keep;
- it **composes with firecode's `--ask`** (which does block, inside a guest that has nothing else)
  rather than competing with it, and it never holds a turn hostage: the turn parks the tool call and
  proceeds with whatever else is independent (§11.2).

Delivery, identity and audit:

- **Fan-out to every registered channel; first valid answer wins.** A later answer is recorded as
  `late` and refused with a message saying who answered and when.
- **Answer authority is checked against the session's `approvers` list**, using the authenticated
  actor id the transport reports (flowy carries `actor` as a ULID plus `actor_kind`), never a display
  name.
- **Cancellation is a withdrawal**, not an interrupt: aborting a turn withdraws its open requests and
  a late answer is discarded. This is dsh's `AbortSignal` behaviour and ACP's `Cancelled` outcome.
- **Every request, fan-out, ack, answer and timeout is a row.** Immutable. The audit is log-only and
  never enters the model transcript — dsh's rule, and the right one: the model sees the derived tool
  outcome, not the deliberation.

### 11.6 Permissions and questions are one mechanism

They differ only in `kind` and in the default on no answer:

- **Permission** — "may I do X?" Options are the allow/deny ladder. `on_timeout` is `Deny` for
  anything irreversible.
- **Question** — "install from source, or stop?" Options are domain choices, free text is accepted,
  and `on_timeout` is `AgentDecides`: the agent is told nobody answered, decides, and **records what
  it chose as a transcript item**, so the choice is in the conversation rather than in a log.

One mechanism, because the combined volume is low and because §19.2 (user-assisted compaction —
*"these are the topics I have, which do you most want preserved?"*) is a question, not a permission,
and must not need a second channel built for it.

### 11.7 The model adjudicator ("auto mode")

New requirement, designed explicitly.

**What it sees.** A bounded, structured brief — never the conversation:

```
action class, tool name, tool schema (description + params)
the arguments, verbatim, with any value over 2 KiB spilled to a digest + head/tail
boundary facts: run id, workspace root, whether the path is inside it, network policy
the session's stated goal (the last user message, truncated)
the last N decisions in this class and their outcomes
the option list it must choose from
```

Explicitly **not** the transcript. Two reasons: cost (this runs per gated call and must be cheap),
and independence — an adjudicator that reads the agent's own reasoning is being argued with by the
thing it is meant to check. F4's *"a safety interlock must not depend on the correctness of the thing
it constrains."*

**What it returns.** The same `AdjudicationDecision`, constrained to a JSON object with
`{option_id | "escalate", basis}` via a grammar/JSON-schema constraint so a malformed answer is
impossible rather than salvaged. `basis` is **mandatory** and is what makes the log reviewable.

**Which model.** Not the session's model. Reasons: a different model is a genuine second opinion; a
smaller one is cheap; and — the operational one — using the session's model means every gated call
either evicts the other model in the router or contends for the same seats. **Recommendation:
Qwen 3.8 27B dense**, hosted as a second router entry, chosen because it is already on the box and
because a dense model's latency is predictable in a way an MoE's is not when it has to share the
card. This is a recommendation, not a measurement; §20-R7.

**When it is unavailable or unsure.**

- unavailable (model not loaded, request failed, malformed output twice) → `Unavailable`, which
  routes to the row's `on_timeout` — never a silent allow;
- unsure → it returns `escalate`, and escalation to the human is a normal outcome rather than a
  failure. This is why `Escalate` is in the vocabulary.

**Auditing auto mode, because this is where "did less than it claimed and said nothing" would
live.** Three mechanisms:

1. every model decision stores the full brief digest, the raw output, the chosen option and the
   basis;
2. a **shadow mode** (`adjudicator: model, shadow: true`) where the model decides but the decision is
   *not applied* — the row is routed to its fallback and both answers are recorded. This is how auto
   mode is qualified before it is trusted, and it is how drift is detected afterwards: run shadow on
   a sample of traffic forever;
3. `EXPLAIN` and a `harness adjudications` report show, per class: counts, allow rate, escalation
   rate, and agreement with the human on the shadow sample. A drifting adjudicator shows up as a
   moving allow rate on a stationary class.

### 11.8 What firecode must provide

Written as a request; the operator owns the repo.

1. **A parent cgroup for a host-side daemon.** Today a run spawned by a host caller has no parent, so
   invariant 2 does not reach it. harnessd needs either to place itself in a cgroup firecode accepts
   as a parent, or an explicit `--parent-cgroup <path>` on the spawn/`up` paths, so that killing
   harnessd's session reaps its runs transitively the same way a guest-initiated spawn does.
2. **A persistent shell channel.** `firecode in` is one command, its output, its exit status — the
   right primitive, but an agent's `bash` tool wants a session with a cwd, exported variables and
   background jobs. Either a `firecode shell --attach` channel with a session id, or a documented
   statement that it will not exist so the harness maintains cwd/env itself and says so.
3. **Room messages need ids.** `firecode chat --ask` posts and waits, but an answer is a new message
   correlated by prose. For the flowy bridge (§12.4) an ask needs `{id, in_reply_to}` and the reader
   needs to be able to say which ask it is answering.
4. **A deadline and a stated default on `--ask`.** `--ask --deadline 15m --on-timeout decide` makes
   the existing behaviour explicit and machine-readable instead of a convention in prose.
5. **An `--ack` verb.** "Acknowledge before you act" is currently a rule people follow. One verb
   makes it mechanical and lets a waiting agent distinguish "someone is composing an answer" from
   "nobody is there".
6. **A per-run room, or a run tag on every message.** One global room means the bridge cannot address
   the right agent, and two concurrent runs asking questions are indistinguishable to a human
   reading the room.
7. **Structured `--ask` options.** `--ask --option 'source:Install from source' --option 'stop:Stop
   and report'` returning the chosen id. Free text stays valid as a fallback.

Items 3–7 are all one feature: give the room the request/response object §11.2 already needs. If
that lands, the flowy bridge becomes a mapping rather than a parser.

### 11.9 Where opencode's prior art is, and is not

There is none to copy for the async path: `permission.ask` is declared at `plugin/index.ts:261` with
**zero trigger sites repo-wide** — a dead hook. What *is* worth vendoring is opencode's permission
*model* (the allow/ask/deny ladder and its persistence shape), and dsh's service discipline: the
`never` policy is enforced **inside the service before dispatch**, so no listener can bypass it
(`packages/interaction/user-approval`). Ours needs the same: a `deny` row in §11.3's table is
evaluated before any adjudicator is consulted, and no adjudicator can override it.

---

## 12. What flowy must provide

### 12.1 What flowy already has

More than assumed. From `GET /api/node`'s own route table (the best available spec; there is no
source, README or OpenAPI on this machine): projects, rooms (`POST /api/chat/{room}/say`,
`GET /api/chat/{room}/wait`), **DMs** (`POST /api/dm/{to}`, `GET /api/dm/wait`), a named-reader
**inbox** with single-holder semantics and server-verified attachment (`GET /api/presence` reports
`attached` / `state: listening|lost` with pid and host), **attachments** (`POST /api/attachment`,
JSON base64, 4 MiB client cap), **a WebSocket** at `GET /api/agent/socket` that already carries an explicit `{type: "attach"}`
frame for the console's live agent/shell/VM sessions, a **task delegation** state machine
(`POST /api/assign`, `/api/task/{id}/delegate`, `/api/task/{id}/state`), an **announcement**
lifecycle with `ack` and `resolve`, `todo waiting-on --of WHO`, ULID identifiers throughout, HLC
ordering, signed rows, and an **MCP server** (`flowy mcp`).

Messages carry an authenticated `actor` ULID plus `actor_kind`/`actor_name`, which is enough for
§11.5's authority check. Long-poll with a deadline is the delivery model everywhere.

**Three corrections to the optimistic reading, from the source.**

- **SSE exists but chat is not on it.** `GET /api/stream` is real, and `streamTopics`
  (`internal/flowy/stream.go:92-119`) covers exactly two topics — `todos` and `queue`. Chat is
  long-poll, exit-per-message, by construction. So *chat-as-a-pushed-event requires a flowy change*;
  it is not a matter of finding the right endpoint. (And note the care already taken there: an
  unknown topic is refused rather than answered 200, because *"a connection that is alive,
  heartbeating, and will never tell it anything reads as 'the queue is quiet' forever."* That is the
  right instinct and our subscription must inherit it.)
- **There are three wake levels, not four, and "human-only" is not one of them.** One predicate,
  `wakesFor` (`internal/flowy/inbox.go:189-331`): **all** / **addressed** (`--to-me`) /
  **mentionsOnly** (`--mentions`), plus two orthogonal scopes (`--room`, `--focus P`) and a per-room
  binary mute. Inside `addressed` there is a *widening* clause: a person's **unaddressed broadcast**
  wakes you, because agents address each other by habit and the operator's messages were
  *"structurally the least likely in the room to be answered"*. `--mentions` turns that clause off.
  Author kind is stamped from the token at write time (`chat.go:86-91`), so a client cannot claim to
  be human to force attention.
- **Per-room policy does not exist, and the source explains why it cannot.** The level is sent as
  query params per poll and stored nowhere; the reader row (`schema.sql:143-202`) has no filter
  columns. `inbox.go:132-137`: *"IT IS ONE FLAG BECAUSE IT HAS TO BE… a reader belongs to a NAME, and
  a second waiter on that name is refused."* Per-room policy is what the operator wants, not what
  exists.

So most of what §11 needs as *transport* exists. What is missing is the **decision object**, a few
semantics around it, and — the one the operator named directly — **a push primitive that does not
require a poll loop**.

### 12.2 The asks

Written so another engineer can implement them.

1. **A `decision` object.** `POST /api/decision` → `{id}` with body
   `{project, to, kind: permission|question, title, body, options: [{id, label, style}],
   deadline, on_timeout, meta}`. `GET /api/decision/{id}` returns its state.
   `POST /api/decision/{id}/answer {option_id | text}` answers it exactly once; a second answer is
   refused with the winner's identity and time. `POST /api/decision/{id}/ack` records that a human
   has seen it and extends the deadline. `POST /api/decision/{id}/withdraw` cancels it.
   This is close to what `announcement` + `ack`/`resolve` + `waiting-on` already do; the ask is to
   generalise those into one object with **options** and a **deadline that the node enforces**.
   *Nothing else in this list matters as much as this one.*

2. **Options rendered as choices.** A client shows `options` as buttons; the answer is
   `{decision_id, option_id}`. Free text remains valid. Today there is no structured-choice payload
   at all — reactions, pins, bookmarks, todos and prose.

3. **Server-enforced deadlines.** `deadline` on the object, not just on the poll. The node marks it
   expired, notifies the requester, and clients stop offering the choice. Today every `--deadline` is
   a long-poll window on the *reader*, which is a different thing.

4. **Delivery receipts.** `sent` / `delivered` / `seen` per addressee. Without them, "the human has
   not answered" is indistinguishable from "the message never arrived" — and this fleet has already
   paid for that: a message addressed by name into the wrong project's `#general` was silently never
   delivered. For a decision channel, silent non-delivery is disqualifying (§11.5).

5. **Multiple concurrent readers per seat, or a reader namespace.** This is a hard blocker as things
   stand. `flowy inbox --as NAME` is **single-holder** — a second waiter is refused with
   `LISTENER REFUSED`. harnessd needs its own long-lived reader, and the operator's interactive
   Claude Code session already holds `claude-lab2x1`'s. Two acceptable resolutions: (a) readers are
   named `(seat, purpose)` so `claude-lab2x1/harnessd` and `claude-lab2x1/session` coexist; or
   (b) the harness gets its own seat per agent. **(b) is better anyway** — a permission request
   should be addressable to *which agent, which session*, and a seat per agent role gives identity
   for free. But minting a seat per session is not something the current `flowy mint` flow is shaped
   for, so say which is intended.

6. **A pushed inbox stream — the highest-priority ask after the decision object, and the one the
   operator named.** File it verbatim as:

   > `GET /api/inbox/stream?as=NAME&addressed=&mentions=&focus=&room=` — same `wakesFor`, same
   > reader row, same enrichments, same heartbeat, one JSON message per SSE event,
   > `POST /api/inbox/ack` unchanged.

   That turns N exit-per-message polls into one connection while preserving the ack-after-write
   contract exactly. It is deliberately the smallest possible change: no new predicate, no new
   storage, no new ack semantics.

   **Why the current shape is not enough.** `flowy inbox --as NAME` blocks, prints one message, and
   **exits**; the caller re-arms. In a terminal agent every arrival lands in the conversation as a
   tool invocation plus a tool result plus the re-arm — the operator's *"monitors and timers which
   pollute the context"*. `harnessd` is long-lived and can hold one connection for the life of a
   session, so the shape it needs is one connection and N deliveries. This is also what permits
   §3.7's rule that an inbound message costs exactly its own tokens.

   Two smaller asks that ride with it:

   - **A fourth `waiter_kind` for a streaming listener** (`schema.sql:164-180`), or the roster
     misreports what is attached — and `GET /api/presence` is the one server-verified answer to "is
     anybody listening", which §11.5 depends on.
   - **Postgres `LISTEN`/`NOTIFY`** to remove the internal 250 ms tick, named as the known gap at
     `stream.go:71-76`. Not required for correctness; it is the difference between a stream and a
     poll loop behind a stream's facade.

7. **Streaming/updatable message content.**7. **Streaming/updatable message content.** For §13's "chat with a running agent", the ability to
   append to or replace a message in place, so tokens stream into flowy rather than arriving as a
   wall after two minutes. Without it the flowy head is a batch head, which is usable but poor.

8. **Idempotency keys on send/answer**, so a connector retry cannot duplicate a decision or an answer.

9. **Cross-project reach for a decision.** A room in project *Lab* cannot be read by a seat whose
   token reaches only *flowy*, and vice versa; this is by design and correct. But a decision is
   addressed to a **person**, and the person's presence in the right project is currently a
   precondition the sender must know about. Either decisions route by actor rather than by room, or
   the API returns an explicit "not reachable in this project" error rather than accepting the write
   and delivering nothing.

10. **Optional, and lower priority: agent-to-agent RPC** with the same object shape, so harnessd can
    ask `claude-host-lab` a question programmatically rather than in prose.

### 12.2b Per-room policy is ours to provide, not flowy's to fix

flowy's one-flag limit is a real consequence of one reader per name, and the fix does not belong
there — it belongs in a client that holds a subscription and can keep per-room state. So the harness
provides it:

- a **per-room subscription row**: `(room, level: off | mentions | addressed | all)`, persisted, with
  a session default;
- **keep flowy's definitions exactly** — `wakesFor`'s three levels are well-reasoned and the fleet
  has paid for their edges; re-deriving them would re-pay;
- **drop the one-flag shape**: the harness evaluates the level per room after delivery, so `all` in
  the home project and `mentions` elsewhere is one subscription and a table;
- **make the human-broadcast clause an explicit named option** (`wake_on_human_broadcast: bool`)
  rather than baking it into `addressed`. The fleet has paid for that being implicit **twice, once in
  each direction**: first a person's unaddressed *"who is here?"* reached nobody, then a person's
  message addressed to *one* agent woke every `--to-me` waiter in the room and a seat acted on work
  meant for someone else. A clause that has failed in both directions should be a named switch.

### 12.3 What we build if none of this lands

A degraded v1, so flowy work is never a blocker:

- a decision is a DM whose first line is `[decision 01J…] <title>` followed by the options, one per
  line, as `01J….a  Install from source`;
- answers are parsed with a strict grammar (`<id>.<option>` or `<id> <free text>`), and anything
  unparseable gets a reply saying what was expected — a tool contract (§8.1 clause 1) applied to the
  human;
- **the harness sends its own ack**: on receiving a decision the connector immediately replies "got
  it, waiting" so at least *its* liveness is visible; the human's ack is unavailable;
- deadlines are enforced harness-side; the message is edited-by-appending "expired, defaulted to X";
- no delivery receipts, so §11.5's `no_human_channel` warning falls back to "the reader is attached
  per `GET /api/presence`", which is a proxy and is labelled as one.

Cost of the degraded version, stated honestly: no receipts means a lost message looks like a slow
human, and the mitigation is a shorter default timeout with a louder default. That is worse, and it
is the reason ask #4 is high on the list.

### 12.4 flowy and the firecode room: separate, bridged

**Recommendation: they stay separate, and harnessd bridges them on the host.** The deciding argument
is the trust model, not convenience.

- firecode's guest is **untrusted by construction**. Making `firecode-chat` a flowy client requires a
  flowy token inside the guest, which grants a compromised guest the ability to speak — and to
  answer decisions — as a seat. That is a real regression in a model the whole design leans on.
  `firecode-chat` today needs no credentials: it talks to a host service over vsock.
- The room must keep working with **no harness at all**. `firecode claude … --dangerously-skip-permissions`
  is a supported standalone mode, and spawned children deliberately have no MCP and no tools but keep
  their voice. A room that depends on harnessd being up would take that away.
- flowy is the fleet bus: identity, projects, presence, a console the human already watches, and
  reach to other machines. The room is a local channel for processes that have nothing else.

So: **the room is the guest-side channel; flowy is the human-side bus; harnessd is the bridge.**
Concretely, the bridge is a host-side loop running `firecode chat --inbox` (which exits per message,
per firecode's own note that a blocking MCP call cannot be a permanent listener), republishing into
flowy with the run id and project attached, and posting answers back with `firecode chat`. It needs
firecode asks #3, #6 and #7 (§11.8) to be a mapping rather than a parser.

**And note what this does not require**: our own harness's asks do not go through the room at all.
harnessd holds the loop, so an adjudication request is an event on its bus and reaches flowy
directly. The bridge exists for interop with runs the harness did not start.

The legitimate alternative — "flowy grows to subsume the room" — is rejected only on the credential
point. If flowy ever supports a **capability token scoped to one run, with no write authority beyond
one decision id**, that objection disappears and unification becomes the better answer. Worth
recording as the condition rather than as a closed door.

---

## 13. Multi-head attach and detach

### 13.1 There is no prior art; this is original work too

Worth saying plainly, because it changes the risk estimate. dsh's profiles are the closest thing
surveyed (`web` is a long-running server with multiple browser clients over HTTP + a single
`/api/remote.mux` WebSocket; `headless` is a one-shot in-process driver; `sdk` and `acp` are stdio
JSON-RPC servers) — but **each browser session composes its own agent session** from a preset, and
nothing in the docs says two clients can concurrently observe and drive *one* live conversation. ACP
is single-client by shape. opencode's server is per-client. So the "tmux for an agent" requirement is
unsolved in the survey and must be designed rather than borrowed.

### 13.2 The model

- The daemon holds the session. A head sends `ATTACH {session_id, since_seq, identity, caps}`.
- **Authoritative state is the event log** (§4.5), monotonic `seq` per session.
- **Late join.** `since_seq = 0` gets a **snapshot** — a materialized view of messages and parts as
  of seq S, maintained by the daemon — followed by events from S+1. A head that joins a 300-turn
  session never replays 300 turns of deltas. The snapshot is derived (§4.4) and can be rebuilt.
- **Resume.** A head that was attached reconnects with its last `seq` and receives the gap. If the
  gap exceeds a bound (default 5,000 events or 4 MiB) the daemon answers `RESYNC` and the head takes
  a fresh snapshot instead. Resync is a normal outcome, never an error.
- **Fan-out.** The daemon is the **only** reader of the model stream. It converts deltas into events,
  appends them to the log, and broadcasts. Each head has a bounded queue; on overflow the head is
  **demoted to resync**, not dropped and not blocked. *The model stream is never blocked on a head.*
  That rule is what keeps a slow flowy connector from stalling a turn.
- **Two heads acting at once.** Every mutating command carries `expected_seq` and a
  `client_request_id`. Commands are serialized on a per-session queue. If a turn is already running,
  a second head's prompt is **queued as a follow-up user item** rather than rejected — that is what a
  human expects from a shared session — and the queuing is announced as an event so both heads see
  it and who did it. Interrupt/abort is idempotent, any attached head may issue it, and it is
  announced with the issuer's identity.
- **No input lease.** Announce-and-serialize is sufficient for two humans, and a lease is one more
  thing to get stuck. (A lease *is* wanted for automation later; the `expected_seq` field is what
  would carry it, so the door is open.)
- **All heads detach mid-turn: nothing happens.** The turn continues, events keep appending. The HTTP
  request to llama.cpp is owned by the daemon's turn task and is cancelled only by an explicit
  `ABORT` or daemon shutdown. **TCP close is detach, never abort.** On completion with no head
  attached, a `turn_complete` notification routes to flowy.
- **Daemon restart mid-turn.** The turn is marked `interrupted` in the log **and the partial
  assistant output is kept as a real transcript item.** That is not a UX nicety: those tokens are in
  the server's cached entry, so keeping them makes the next request a prefix extension and warm,
  while discarding them guarantees a divergence. The item carries `truncated: true` so the model is
  told.

### 13.2b Adopt flowy's delivery mechanics — several of §13.2's hard problems are solved there

flowy's inbox and agent-socket code has paid for these already. Take them rather than re-deriving.

- **A server-side cursor per named reader, acked _after_ the client has written the messages out**
  (`inbox.go:489-493`). A crash costs a duplicate, never a silence. Our head protocol acks the same
  way: `seq` advances after the head has rendered, not on receipt.
- **The mark advances over everything READ, not everything delivered** (`inbox.go:421-428`) — called
  the single most reusable bug in that repo, and it is one we would otherwise hit exactly: *a
  filtering consumer that advances only over what it kept rereads its own output forever.* Our
  per-room levels (§12.2b) make the harness a filtering consumer, so this applies directly.
- **Report what was filtered out** (`Skipped`, `inbox.go:55-58`). *"Busy and none of it was for me"*
  is a different fact from *"silent"*, and it is what makes a broken filter visible instead of
  indistinguishable from a quiet room. A head that suppresses events must say how many.
- **One waiter per name, enforced by a written claim plus `kill -0`, with the refusal naming the
  pid** (`waiterlock.go`). The multi-head consequence is precise: **several heads share ONE
  authoritative reader; they must not each hold one.** The daemon holds it; heads attach to the
  daemon.
- **Idle means quiet, not unwatched.** Reaping on detach kills a twenty-minute build when a tab
  closes. This is §13.2's "every head detaches mid-turn", already settled the same way.
- **Bounded scrollback that discloses the drop** (`agent_ws.go:106-108`, with `hello.Dropped`
  present-and-zero rather than `omitempty`). A late head is told what it will never see. Ours is
  §13.2's `RESYNC`, and the `omitempty` detail is the real lesson: *an absent field and a zero field
  must not look the same* when the field is the disclosure.
- **Snapshot and register under one lock** — *"the missing byte is usually the prompt."* §13.2's
  late-join snapshot must be taken and the head subscribed in one critical section, or the head
  misses exactly the event that made it attach.
- **Non-blocking fan-out; drop the slow head and tell it.** Identical to §13.2's bounded queue and
  demote-to-resync. One stalled client must never stall the agent.
- **`agentscrub` generalises, and this one is subtle.** Live and stored are different artifacts of
  the same stream, and anything *interactive* must be stripped from the stored copy. For us that is
  permission prompts, `--ask` questions, progress frames and partial tool output replayed to a late
  head — **otherwise they are answered or acted on twice.** So the snapshot a late head receives is
  the *scrubbed* projection: settled decisions render as their outcome, not as an open prompt.
  Related and equally sharp: an answer is owed only while no reader is attached, and a queued reply
  is **discarded rather than held**, because *"a reply to a question asked ten minutes ago is not an
  answer, it is input arriving from nowhere."*

**What not to copy from flowy:** exit-per-message delivery; the forked-successor handover
(`inboxhandover.go`) — a detached listener that hears everything and can wake nobody, which then
needed a schema column and a stand-down protocol to stop it lying, and which a persistent connection
eliminates entirely; a 250 ms poll behind an SSE facade; filter policy passed per-poll and stored
nowhere; and preferences kept in an untyped note body.

### 13.3 Render cost independent of output length

Two halves, and every harness surveyed gets at least one of them wrong.

- **Wire:** events carry increments only (§4.5). No event ever carries accumulated text.
- **Head:** incremental lexing with a frozen stable prefix. omp's mechanism is the one to copy
  (`packages/tui/src/components/markdown.ts`): `stableBlockBoundary` finds the offset past the last
  token whose raw text ends in `\n\n`, subject to guards (the break must be strictly inside the text,
  the next char must start real block content, a preceding list must be provably closed since
  CommonMark can continue a list across a blank line); `#lexTokens` then lexes only the grown tail
  and concatenates. Their own note states the licence for it: *"block tokenization is local across a
  `\n\n` boundary with balanced fences, so `lex(prefix) ++ lex(tail) === lex(prefix+tail)`."*
- **What not to do:** pi's `updateContent` calls `contentContainer.clear()` and rebuilds every child
  from the whole accumulated `message.content` on **every** `message_update`
  (`modes/interactive/components/assistant-message.ts:91-96`), i.e. once per delta — O(n²) over a
  message. opencode did the same for reasoning blocks.

### 13.4 Heads, concretely

| head | transport | notes |
|---|---|---|
| TUI | unix socket at `$XDG_RUNTIME_DIR/harnessd.sock` | filesystem permissions are the auth; no ceremony |
| remote | WebSocket + TLS, bearer token | same frames as the socket |
| flowy | in-daemon connector holding a **push subscription** (§3.7, §12.2 ask 6) | a full head: inbound messages are user items, outbound turn output is posted, decisions ride the same channel. Streams token-by-token once flowy has updatable messages (§12.2 ask 7); until then it posts at step boundaries — a batching of *output only*, never of input. |
| ACP | stdio JSON-RPC, `agent-client-protocol` | an **adapter**, not the native protocol |

**Why ACP is an adapter and not the protocol.** Grok Build implements it against crates.io
`agent-client-protocol 0.10.4` and it does carry permission semantics —
`RequestPermissionRequest{session_id, tool_call_update, options: [PermissionOption{option_id, label,
kind}]}` → `RequestPermissionResponse{outcome: Cancelled | Selected{option_id}, meta}`, with
`PermissionOptionKind` in `AllowOnce | AllowAlways | RejectOnce | RejectAlways`. That answers the
survey's open question, and the answer is *the shape is right, the durability is not*:

- **no timeout at the protocol layer** — `acp_send` awaits a `oneshot` with no `tokio::time::timeout`
  anywhere in the prompter, gateway or channel;
- **no request id in the body** — correlation is the JSON-RPC envelope id, so a request does not
  survive a reconnect;
- **cancellation is a value the client returns**, not an interrupt: `session/cancel` is a separate
  fire-and-forget notification and the agent side does not race it against the pending
  `request_permission`;
- **`RequestPermissionOutcome` is `#[non_exhaustive]`**, so a client must already handle unknown
  outcomes.

So: adopt ACP's **vocabulary** (`PermissionOption`, `option_id`, outcome kinds) in §11.2 so the
adapter is a mapping, and keep our own `{id, deadline, on_timeout, audit}` because ACP has none of
them. Build the adapter early anyway — it is cheap and it earns an editor head for free.

---

## 14. Ported conformance suite — milestone zero

A test encodes the bug and the invariant, not the design, so it ports across languages in a way
implementation code does not. **The suite exists before the harness does**, and it is what tells us a
milestone actually beat opencode rather than felt like it.

### 14.1 What to take, by source

**grok-build (Apache-2.0)** — `crates/codegen/xai-chat-state/src/actor/tests.rs`. (Note: the brief's
path `chat-state/…:4594` is one directory level short and one line off in the current checkout; the
function is at `:4596-4626`.)

The section header states its own thesis: *"Prefix stability within a compaction epoch is what keeps
the inference prefix cache hitting."* Two helpers and twelve tests:

- `serialize_via_public_api` (`:4575-4592`) — serializes through the **real** `From<&ConversationRequest>
  for rs::CreateResponse` plus the same post-serialization JSON patch production applies, not a test
  double. Porting note: *our* equivalent must go through the real request builder and the real
  dialect encoder, or the test proves nothing.
- `assert_prefix_stable_pair` (`:4596-4626`) — asserts `ext_input[..base_input.len()] == base_input`
  on the serialized `input` array, reporting the first divergent index. Note what it deliberately
  does **not** compare: `model`, `tools` and other body fields outside `input`, which are allowed to
  change.

| test | what it pins |
|---|---|
| `prefix_stable_across_user_assistant_turns` (:4647) | the baseline, three turns |
| `prefix_stable_with_consistent_memory_injection` (:4689) | an identical injected reminder must not perturb the prefix |
| `prefix_stable_with_reasoning_siblings_through_build_request` (:4730) | reasoning-as-sibling ordering across three turns |
| `prefix_stable_after_tool_schema_change` (:4783) | tool schemas live outside `input` |
| `prefix_stable_after_model_switch` (:4833) | model lives outside `input` |
| `prefix_stable_with_synthetic_user_messages` (:4869) | synthetic items append, never insert |
| `prefix_stable_after_image_pruning` (:4903) | **the documented exception** — image eviction mutates an old turn, so strict prefix is *not* asserted; instead system-prompt identity, item-count growth and relative order |
| `build_request_preserves_small_old_images` (:5003) | *"Rewriting old images every turn busted the KV-cache prefix"* |
| `build_request_budgets_tool_images_on_request_copy_only` (:5043) | the budget pass mutates the outgoing **copy**, never the stored conversation |
| `prefix_stable_after_tool_result_pruning` (:5115) | pruning happens on a clone; untouched items stay identical |
| `prefix_stable_with_backend_tool_calls` (:5189) | server-executed tool calls hold a fixed wire position |
| `prefix_stable_after_session_resume` (:5245) | snapshot → replace → restore reproduces **byte-identical** output |

The three exception tests are as valuable as the nine strict ones: **they encode where the invariant
is allowed to break**, which is exactly what a plan needs when it introduces forks (§5.5) and
compaction (§10).

Also worth porting from grok-build: the length-policy family
(`acp_session_tests/turn/length_salvage_tests.rs:236, 289, 325`; `stream/messages_tests.rs:454, 493,
526`) — a `length` stop is a hard failure unless explicitly salvaged, a rate-limit error must never
be reclassified as "truncated but complete", a `max_tokens` stop with a *completed* tool_use keeps
`Length` rather than being rewritten to `ToolCalls`; the salvage-streak cap
(`sampler_turn_tests.rs:17, 39`); and `reasoning_estimate_takes_max_of_text_and_encrypted_not_sum`
(`actor/state.rs:295`), which is a token-accounting bug we would otherwise repeat.

**omp (MIT)**

- `packages/ai/test/issue-3528-repro.test.ts` — the reasoning-replay contract, stated as prose in the
  file's own header and worth quoting in our suite verbatim: *"System prompt and tool catalogue were
  byte-stable across requests 3–12… Request 12 … added the prior assistant turn plus the synthetic
  user nudge, and `cached_tokens` collapsed to 0. Full prompt re-processing on llama.cpp."* This is
  the single most directly portable test in the survey (§14.3).
- `packages/agent/test/append-only-context.test.ts` — 59 cases across 8 blocks; the KV-relevant one is
  `describe("message sync")` (`:419-852`, **18** cases, not the 17 the brief cites). The valuable ones
  are the `#3406` family: prefix preserved when a *deep* message is rewritten; divergence detected on
  a **tool-result metadata-only** rewrite; divergence detected on a **providerPayload-only** rewrite
  (a change buried in vendor-native history with visible content unchanged); rewriting message 0
  forces a full resync; and `treats fresh-object clones with identical bytes as stable` — i.e.
  **byte-equality, not reference-equality, is the contract**. Our ledger (§4.3) satisfies all of
  these by construction, which is exactly why they are worth running: they are the cases where a
  hand-rolled digest quietly fails.
- `packages/ai/test/dialect-thinking.test.ts` — a parameterized *"every dialect round-trips thinking
  (no missing thinking element)"* plus *"unterminated thinking at stream end"*. Port the **shape**:
  our §7 dialect registry gets the same two parameterized suites over our three targets.

**pi (MIT)** — `packages/agent/test/agent-loop.test.ts:371-442`, *"should not execute tool calls from
a length-truncated assistant message"*: scripts a stream ending `stopReason: "length"` with a tool
call whose arguments *validate but are truncated* (`{value: "hel"}`), asserts nothing executed, the
tool-execution-end event carries `isError: true` with text containing "output token limit", and the
loop re-prompts. Clean and fully portable. Also the `it.each` compat-matrix pattern from
`openai-responses-compat.test.ts` — the *pattern*, not the OpenAI model list.

**deepseek-harness (MIT)** — `agent-loop/tests/system-prompt-projection.spec.ts` (8 cases) and
`system-prompt-admission.spec.ts` (4): the prompt is committed as a node in history; an unchanged
prompt is **skipped, not re-appended**; a changed prompt on a cached route is **appended after**
cached history rather than rewriting the head. `llm-deepseek/tests/serialize.spec.ts:119-138` — the
`content: ""` never `null` rule, with `reasoning_content` present on tool-call turns.
`llm-deepseek/tests/translate.spec.ts:284-328` — the cache-accounting split (wire `prompt_tokens` 283
minus 256 cached → `inputTokens: 27`), plus an `it.each` over six malformed-usage shapes that
**degrades gracefully rather than emitting a wrong total** — which is F7 as a test.

**opencode (MIT)** — port with care; the source is right about mechanics and wrong about defaults.

- Worth porting: `compaction.test.ts:1044` (reasoning details survive the provider transform);
  `message-v2.test.ts:604` (omit provider metadata when the assistant model differs) and `:1172`
  (pending/running tool calls convert to error results, so no dangling tool_use); the `isOverflow`
  headroom tests at `:468-533`.
- **Read the assertions, not the names.** Three of the `isOverflow` tests are literally named
  `"BUG: …"` and yet assert the *currently correct* behaviour. Porting by title would invert them.
- Do not port: anything keyed to Anthropic signed-reasoning or OpenAI Responses `encrypted_content`
  (vendor wire formats llama.cpp does not speak — port the *shape*, "never lose reasoning on
  re-serialize", not the assertions), and the pure Effect/Schema decode tests, which have zero
  external behaviour.
- One correction to the brief: a repo-wide search for a hardcoded `32_000` output default in
  `packages/opencode/src` and `packages/core/src` found **none** in this checkout — every occurrence
  is a fixture value on a fake test model. The survey's `provider/transform.ts:18` finding should be
  re-confirmed against the exact version measured before it is cited again. **UNVERIFIED-4.**

**oh-my-openagent — excluded.** Sustainable Use Licence. Nothing is copied, tests included, and the
clones at `/data/scratch/harness-survey/omo` and `omo-slim` are not read by anyone building this.

**flowy (the operator's own; licence to confirm — §14.2)** — its delivery tests encode bugs this
fleet has already paid for, and every one of them is a bug our per-room subscription (§12.2b) could
reintroduce:

- a `--to-me` waiter must still hear a **person's unaddressed** message;
- a pasted **email address** wakes nobody (the mention matcher must not fire on `@` in an address);
- a person naming **another** agent must not wake you;
- `--mentions` **beats** `--to-me` when both are set;
- a note reaches the row's **assignee and nobody else**, and on an unowned row wakes its **raiser** —
  with the guard that **two empty strings must not compare equal**, or the fallback silently becomes
  a broadcast;
- presence windows are derived from the waiter's **own** numbers, not from wall clock;
- an absent thread-standing serialises as **absent, not false**.

And port the *inversion* in `inboxauthor_test.go`, which is worth more than the rest combined: assert
that the wire format carries everything the persisted model carries **minus a named exclusion list**,
computed by reflecting over the type. One test, and it is the antidote to the entire
enumerating-renderer bug class — the shape where someone adds a field to the model and forgets the
serializer, which is how a message quietly loses its addressee. We should have this for
`TranscriptItem`, for the head protocol and for the control channel (§3.3).

### 14.2 Licence and attribution mechanics

The facts: every one of the five repos carries its licence **only in a root `LICENSE` file**. None
stamps SPDX or copyright headers on individual test files (0 hits across ~3,000 `.rs` files in
grok-build; 0 in omp, pi, deepseek-harness; one unrelated vendored snippet in opencode). So
attribution cannot be inherited by copying a file — it has to be added.

What that forces:

- **A suite-level `NOTICE`** listing, per source: repo, upstream URL, the commit SHA read, the
  licence, and the files ported from. Apache-2.0 §4(d) requires carrying a `NOTICE` if the original
  has one; grok-build's `LICENSE` carries the copyright line and no separate `NOTICE`, so ours
  attributes it and reproduces the Apache text.
- **A per-file header on every ported test**, added by us: `SPDX-License-Identifier: Apache-2.0` (or
  `MIT`), the upstream path and line range, the SHA, and one line saying what was changed. This is
  also the practically useful thing: when a ported test fails, the header is how you find what it
  originally asserted.
- **MIT and Apache-2.0 can sit in one repository**, and both are compatible with essentially any
  outbound licence. Apache-2.0 adds a patent grant and the §4 notice/attribution obligations; MIT
  adds only the copyright-notice obligation. Neither is copyleft, so neither constrains the harness's
  own licence.

**If the harness's licence is undecided**, what each choice forces:

| choice | consequence for the ported suite |
|---|---|
| MIT or Apache-2.0 | nothing. Both sources drop straight in with per-file headers plus `NOTICE`. Apache-2.0 outbound is the tidier match for the Apache-2.0 inbound. |
| GPL-3.0 / AGPL | still fine inbound — MIT and Apache-2.0 are GPL-3-compatible — but the suite can no longer be reused by a permissively licensed third party, which defeats the "measure any candidate on one axis" goal in §14.3. |
| proprietary / unlicensed | legal inbound (both permit it) but the attribution obligations still bind, and an unlicensed public repo is licensed to nobody. |

**Recommendation:** license the *conformance suite* Apache-2.0 regardless of what the harness is,
in its own directory with its own `LICENSE` and `NOTICE`. It is the artifact whose value depends on
other people being able to run it against their own harness, and separating it means the harness's
licence can be decided later without touching the suite.

### 14.3 Which tests run WITHOUT our harness

This is the part that makes the suite milestone zero rather than a chore. These are black-box against
a real llama.cpp endpoint plus *any* client, so the same numbers describe opencode today, our harness
tomorrow, and any third-party candidate.

Two mechanisms carry all of them:

- **A recording proxy** in front of :8080 that captures every outgoing request body and every
  response, keyed by turn. Nothing about it is harness-specific.
- **`POST /apply-template`** (§7.3), which renders any message array through the real server template
  with no inference and no slot.

| # | check | how |
|---|---|---|
| C1 | **prefix extension, exact** | proxy: request N's prompt must be an exact prefix of N+1's — as `messages` for an unfused client, as the token array for ours. grok's `assert_prefix_stable_pair`, re-expressed. For our harness it holds by construction (§4.3); the test exists to measure *other* clients on the same axis and to catch a regression in the ledger. |
| C2 | **renderer fidelity** | `/apply-template` on each fixture → exact string equality against our renderer (§7.2). Subsumes render-level prefix stability, and is the required CI gate. |
| C3 | **generation-inclusive prefix** | `cached_tokens(N+1) ≥ prompt_tokens(N) + predicted_tokens(N)`. §18.1-I1. **This is the real invariant and nothing surveyed tests it.** |
| C4 | **`f_keep` distribution** | from `usage.prompt_tokens_details.cached_tokens / prompt_tokens` over a scripted 30-turn session; report p10/p50/p99 and p99 re-prefill tokens. Directly comparable to the 0.000 → 0.999 measurement. |
| C5 | **reasoning replay** | omp's #3528 scenario, scripted end to end: assert `cached_tokens` does not collapse on the turn that adds a prior assistant turn plus a synthetic nudge. |
| C6 | **`content: ""` never `null`** | proxy: on a tool-call-only assistant history turn, `content === ""`. |
| C7 | **truncated tool args are not executed** | `max_tokens` low enough to cut a tool call; assert the client refuses to execute and re-prompts. |
| C8 | **`finish_reason` handling** | `max_tokens: 1`; assert the client surfaces `length` rather than reporting success. |
| C9 | **in-history system prompt** | proxy: change the system text mid-session; assert message 0 is unchanged and a new system message appears at the tail — and that `cached_tokens` stays high. |
| C10 | **cache accounting is disjoint** | reported "new input" tokens equal `prompt_tokens − cached_tokens`. deepseek's invariant, generalized. |

C1–C10 are the axis. Running them against opencode v1.18.29 today produces the baseline every
milestone is measured against, and it costs one afternoon.

### 14.4 What no existing suite covers — we write these

- **warm-prefix compaction** (§18.1-I5): the summarizer call's own `f_keep` must be ≥ 0.99;
- **reading `prompt_progress` / `timings`** off the stream, and the idle-timeout interaction (a long
  prefill must not race a timeout because progress chunks are not counted as progress);
- **adjudication**: routing by class, escalation, timeout defaults, first-answer-wins, late-answer
  rejection, audit completeness, and auto-mode shadow agreement;
- **multi-head**: late join via snapshot, resync on overflow, two heads acting at once, all heads
  detaching mid-turn, daemon restart keeping partial output;
- **renderer conformance**: the `/apply-template` diff over the fixture corpus (§7.2), plus
  `render_incremental ≡ render` and `parse ∘ render ≡ id`, parameterized over every dialect;
- **`EXPLAIN` correctness**: given a deliberately induced divergence, the plan must name the right
  item and the right cause.

**Prerequisite, and it is blocking:** GLM-STATE records that pytest is not installed on this box, and
that is why the drafted prompt-cache eviction tests have never been run. Milestone zero starts with
installing a test runner.

---

## 15. What to vendor or port, and from where

Licences: grok-build **Apache-2.0**; omp, pi, deepseek-harness, opencode **MIT**; `oh-my-openagent`
**Sustainable Use — excluded entirely, code and tests**.

| take | from | what exactly |
|---|---|---|
| **the prefix-stability test** | grok-build, Apache-2.0 | `assert_prefix_stable_pair` + the 12-test suite (§14.1). **First, before harness code.** Re-expressed over token ids (§18.1-I2). |
| the `ConversationItem` shape | grok-build | sum type with `Reasoning` as a sibling of `Assistant`; the wire tags |
| `LengthPolicy` + `empty_reason()` | grok-build | `Fail / CompleteToolCalls / CompletePartial`, empty-Length always fails |
| the compaction assembly order | grok-build | `[System, user-meta, project instructions, last user query, recent verbatim, summary]`, recent **before** summary |
| `in-history` system prompt | deepseek-harness, MIT | `SystemPromptProjection.project()` — compare against *latest*, append, and the normalizing fallback |
| warm summarizer construction | deepseek-harness | `buildSummarizationInput` + instruction appended last; the surface-replace commit |
| spill-don't-truncate | deepseek-harness | the reserve arithmetic, head/tail split, notice wording, 0600 storage, best-effort fallback |
| `content: ""` never `null` | deepseek-harness | the rule **and the comment**, which explains why |
| the README convention | deepseek-harness | *"What the model sees / Token effect / KV Cache effect"* per prompt-contributing feature. Culture, not code, and the most transferable thing in the survey. |
| `samplingParams` last-write-wins merge | pi, MIT | `Object.assign(params, extra)` after named fields; `{...model, ...request}` merge order |
| reasoning field round-trip | pi | `foundReasoningField` capture + replay into the same field |
| length → fail the whole tool batch | pi | including the message told to the model |
| the durable-harness persistence spec | pi | `packages/agent/docs/harness.md`: entries / bound values / usage ledger, pending rows deleted in the placing transaction, *"a compaction is a self-contained checkpoint, not a pointer into history"* |
| incremental markdown lexing | omp, MIT | `stableBlockBoundary` + tail-only re-lex, and the guards |
| `append-only-context` as a reference | omp | read it for the mechanism; **do not** copy the 32-bit digest — §4.3 supersedes it |
| the Part union | opencode, MIT | `text / reasoning / tool / step-start / step-finish / compaction / patch` — as the **Journal's** type (§4.1), never the transcript's |
| steering into a running loop | pi, MIT | `agent-loop.ts:205-206` — the shape §5.8 adopts |
| SQLite schema shapes, tool bodies, permission ladder | opencode | starting points; the loop and the defaults are not |
| `packages/llm` | opencode | worth reading before writing a model client |

**Deliberately not taken.** Grok Build's transport layer: no `extra_body` anywhere in 1.77M lines, so
`return_progress` and `cache_prompt` cannot be sent at all; `FinishReason` is a closed enum with no
`#[serde(other)]`, so an unexpected value fails chunk deserialization outright; and no unknown-field
capture on stream deltas, so llama.cpp's `timings` is invisible. Those three are precisely what a
local-first harness needs most, and they are structural in that codebase — 86 crates, 59 internal
`xai-*` deps, telemetry in 115 files and not feature-gated. Forking it is a de-xAI-ification project.
Its **tests and its data model** are the asset.

**Two rules we adopt from those omissions**, as positive requirements: every wire enum has an
`Unknown(String)` fallback variant, and every streamed delta struct captures unknown fields
(`#[serde(flatten)] extra: Map<String, Value>`) so a field the server adds tomorrow is visible today.
Fusion softens both — the protocol is ours and versioned in lockstep — but they still apply on the
`/completion` fallback path (§5.6), which is where M1 lives and where an upstream llama.cpp change
could still surprise us.

---

## 16. Language and runtime

Requirements this choice actually has to serve, in order: byte-exact serialization control and cheap
hashing; a long-lived daemon with concurrent streams, backpressure and cancellation; fan-out to N
heads without head-of-line blocking; exhaustive handling of several sum types; SQLite; a good TUI;
and the ability to port tests from Rust and TypeScript.

**Python** — out. A long-lived multiplexing daemon under the GIL is the wrong shape, and the ecosystem
pulls toward convenience exactly where this design needs canonical bytes. (It stays the right choice
for the *conformance suite*; see below.)

**TypeScript / Bun** — fastest to a working thing, and the two closest references (omp, pi) plus
opencode's part union and tool bodies are TypeScript, so porting is nearly free. Bun ships SQLite and
a good runtime. Against: `JSON.stringify` key order is insertion-order dependent, which is a live
hazard for canonical serialization (manageable, but it is a *discipline* rather than a *guarantee* —
and this whole design is about preferring guarantees); no exhaustiveness checking on unions without
extra ceremony; and every renderer bug in the survey is in a TypeScript codebase, which is a
correlation rather than a cause but is worth noticing.

**Go** — an excellent daemon language: concurrency, backpressure, one static binary, solid SQLite,
bubbletea for the TUI. Against, and it is specific to this design: **the data model is almost
entirely sum types** — the transcript item union, the part union, the tool outcome, the adjudication
outcome, the finish reason. Go's answer is an interface plus a type switch, which works but drops
exhaustiveness checking. Exhaustiveness is exactly what protects "did the renderer handle the
reasoning variant", and forgetting a variant is the shape of the bug that caused all of this.

**Fusion changes the weights decisively.** Three new requirements arrive with §3: FFI to `libllama`
for tokenization and vocab (the harness now tokenizes); a binary control protocol and `memfd` shared
memory with a `Vec<llama_token>` mapped across a process boundary; and code that ships in one repo
and one release with a C++ server. That is a systems-programming shape, and it moves the decision from
"Rust is tidier" to "Rust is the obvious choice, Go is the only other candidate, and TypeScript is
out". TypeScript can do FFI and shared memory, but doing the token vector, the hash chain and the
protocol framing in a GC'd dynamic language, in the same repo as the C++ it must byte-match, is
fighting the tool.

**Rust** — the best fit, and the argument is specific rather than aesthetic:

1. **The correctness story is a type-level story about what may mutate.** A private constructor and a
   single `append` method is how §4.1 becomes structural rather than a code-review rule.
2. **The two most expensive failures in the survey are silent**, and `serde` makes both explicit
   decisions: a closed enum failing to deserialize (`#[serde(other)]`) and an unknown field being
   dropped (`#[serde(flatten)]`). In Rust you write those down; in a dynamic language you find out
   later.
3. **The crown jewel of the ported suite is already Rust** (grok-build's prefix suite), so milestone
   zero's most valuable tests port with the least distortion.
4. A single static binary for a daemon that must be trivially restartable and survive reboots.
5. `tokio` gives per-head bounded channels with the backpressure semantics §13.2 needs, and `ratatui`
   is a real TUI.
6. **`libllama` FFI is a solved, boring problem in Rust** (`bindgen` over `llama.h`), and the
   tokenizer is the one piece of the model stack the harness genuinely must share with the server —
   `llama_tokenize` against the same vocab, so our token ids are the server's token ids by
   construction rather than by agreement.
7. Mapping a `memfd` and treating it as an append-only `&[llama_token]` across a process boundary is
   exactly what Rust's aliasing rules are for: the harness holds the only writer, the server maps it
   read-only, and that is expressible rather than merely intended.

Cost, stated honestly: slower to write, and the TypeScript references become reading rather than
copy-paste. That is a real week or two.

**Recommendation, with a hedge that has teeth.**

> **Daemon and TUI in Rust. Conformance suite in Python (pytest), deliberately, and it must not
> depend on the daemon.**

The hedge is not decoration, and fusion makes it *more* important rather than less. Because the suite
is black-box (§14.3) and the HTTP diagnostic surfaces stay (§3.3), it can be written first, in the
fastest language, and it will still measure the Rust daemon, today's opencode, and any candidate on
the same axis — including a fused system against an unfused baseline, which is the comparison nobody
can otherwise make. The language decision stays reversible; more importantly, **the fusion decision
stays measurable**, which is the only honest way to defend it.

Python for the suite also matches llama.cpp's own server tests, so the harness suite can live
alongside them and the drafted-but-never-run prompt-cache tests get a runner at the same time.

---

## 17. Phasing

Each milestone is independently testable and independently useful, and each names the C-tests (§14.3)
or I-invariants (§18.1) that certify it.

### M0 — the conformance suite (no harness)

Install pytest. Build the recording proxy, the fixture corpus (§7.2) and the `/apply-template` diff
harness — the last of which is also the acceptance test every renderer in M1 is written against.
Implement C1–C10.
**Run them against opencode v1.18.29 today** and record the baseline.

*Useful by itself*: it turns "opencode is wrong about caching" into a number, and it is the only way
any later claim of improvement can be checked. Also produces the fixture corpus and the
`/apply-template` diff harness (§7.2), which cost no inference at all and which the renderers in M1
are then written against.

*Exit*: a baseline table of `f_keep` p10/p50/p99, p99 re-prefill, and pass/fail on C1–C10 for
opencode.

### M1 — `harnessd` + one head + read-only tools

The smallest thing that beats today's setup. A headless daemon with one session against GLM,
a TUI head, and read-only tools only (read, grep, glob, ask_code, ask_corpus). No compaction, no
adjudication beyond `boundary`-for-reads, no subagents, no firecode yet.

It runs entirely on the **`/completion` token-array fallback** (§5.6) against today's unmodified
server, so nothing here waits on a C++ change. The GLM renderer is written against M0's
`/apply-template` diff, which is the acceptance test for it.

What it fixes on day one: reasoning is simply present, because we emit the tokens; the token vector is
append-only with the ledger; no output cap and `finish_reason: length` acted on; incremental render;
and a real progress display from `prompt_progress` — which nothing surveyed has and which is the most
visible difference to a person using it.

*Exit*: C1–C10 pass against harnessd where opencode failed; `f_keep` p10 ≥ 0.99 over a scripted
30-turn session.

### M2 — firecode integration and write tools

Session ↔ run mapping, `vm_in` as the exec path, files over the tar channel, spill-don't-truncate,
and the `boundary` adjudicator with the §11.3 policy table. Write and exec tools become available
because the boundary now exists.

*Exit*: invariant 6 and 11 hold under our driving (the host project is not modified); a `bash` tool
that deletes everything harms nothing; adjudication rows are recorded for every gated call even
though almost none are delivered anywhere.

### M3 — the flowy connector: chat first, decisions as a usage of it

**Chat is the primitive here, not adjudication.** The connector holds a push subscription if flowy has
one, and the degraded reader if it does not; an inbound message becomes a user item in the session
with no envelope (§3.7, I11); outbound turn output is posted; steering at step boundaries works
(§5.8). Decisions and questions then ride the same channel using §12.3's text protocol — answers
parsed, the harness's own ack, timeouts and defaults, the full audit.

Ship "What flowy must provide" (§12.2) to whoever extends flowy, with ask 1 (the decision object) and
ask 6 (the push subscription) marked as the two that matter.

*Before* M4 deliberately: the async decision shape is the thing that cannot be retrofitted, and
proving it end to end against a real human is worth more than a second head.

*Exit*: a real decision answered from a phone, with the audit row to show for it; and the
`no_human_channel` degradation exercised by killing the connector mid-request.

### M3.5 — Falsifier B: quality against depth

Not a feature. One measurement, and §9's whole position rests on it: run the same fixed task at 20k,
60k and 150k of preceding context on GLM and score answer quality, not speed. If quality falls off
well before the context window, §9.1 clause 4 is wrong, compaction must be aggressive and early
whatever it costs in prefill, and §19.3's swapping becomes worth its price.

Placed here because M0–M3 give it the machinery it needs (a scripted session, a stable prefix, real
tools) and M4's design depends on its answer.

*Exit*: a number, and a written decision to keep or revise §9.1.

### M4 — leveled compaction

Segments, levels, opportunistic warm segment summaries, soft compaction by default, hard compaction
at the wall. **This is high in the phasing on purpose**: §9 concludes that compaction is the primary
quality lever, not cost control, and it is what makes reasoning replay affordable.

*Exit*: I5 (the summarizer call's own `f_keep` ≥ 0.99) — the measurement nobody in the survey can
make; and a session that crosses the high-water mark without a stall longer than one cold prefill of
the *compacted* prompt.

### M5 — multi-head

Attach/detach, snapshot + resume, resync on overflow, two heads at once, all-detach mid-turn, the
partial-output-on-restart rule. The ACP adapter, which is cheap once the head protocol exists.

*Exit*: the §14.4 multi-head suite; and a turn that survives closing every client.

### M6 — subagents, concurrently

Spawned firecode runs, per-role tool budgets enforced, abstention propagation, and — the point —
**deliberately concurrent** dispatch. This is F6: the first time this box's scheduler is asked for
more than one seat by a real client.

*Exit*: seats > 1 observed under real load in `/metrics` and `/slots`. Every seat measurement on this
box is synthetic until this milestone lands.

### M7 — the model adjudicator, in shadow

Auto mode built, run in shadow against the human for a fortnight, with the agreement report. Promoted
per class, not globally.

*Exit*: an agreement number per action class, and a documented promotion decision.

### M8 — the other two models

Probe, admit, run the dialect-parameterized suite. Can move earlier — it is gated only by M0's probe
and by a preset existing for the 27B dense model.

### Server track — same repo, same release, but its own build loop

Fused means one system and one version number, not one schedule. These land as discrete server
changes, each measured once and then iterated from the daemon side (§3.6). None of them blocks a
harness milestone, because §5.6's `/completion` token-array fallback always works.

- **S0** — **the control channel and the shm submit path** (§3.3). The enabling change; everything
  else in this list rides on it. Lands between M1 and M2.
- **S1b** — **`Spans`: accept `message_spans` and segment boundaries directly** instead of
  string-matching delimiter tokens (§3.4.1). Small, and it removes a heuristic from checkpoint
  placement.
- **S2b** — **`Admit` / `Stats`** (§3.4.4, §3.4.3): a real answer to "can I afford another agent", and
  one accounting truth. Gated with M6, which is the first time a client asks for concurrency.
- **S3b** — **`SegmentHint`: semantic eviction** (§3.4.2). The ladder gains a value input it cannot
  otherwise have — a finished subagent should demote before a stale-but-active thread, and today
  nothing can express that.
- **S4b** — **`Prewarm`** (§3.4.5).
- **S5b** — **`Explain`** (§3.4.6), which retires the `-lv 4` log scraping.

And the pre-existing server items, unchanged in substance:

- **S1** — return the matched entry's `lcp` / `f_keep` / `f_sim` / id in the response `timings`, so
  `EXPLAIN --deep` stops scraping logs at verbosity 4. Highest value, lowest risk.
- **S2** — age-based demotion (GLM-STATE "Open, ranked" #2): a hard cap for actives plus a timer for
  idles, replacing `--sleep-idle-seconds 600`, which dumps everything including the conversation
  about to be touched.
- **S3** — run the drafted prompt-cache eviction-ladder tests, which have never been run.
- **S4** — **block-hash / radix prompt-cache index with cross-sequence sharing** (§3.10-A). Refcount
  plus LRU among refcount-zero blocks, at the *resident pool* tier; the existing degrade ladder stays
  at entry granularity in the RAM/NVMe tier and the two are never merged. Gated on M6 making many
  concurrent conversations real, because the win is quantified per shared prefix per sequence and
  today there is only ever one sequence to share with.
- **S5** — **the recompute-fraction experiment for stitching** (§3.10-C), per layer class, MLA blocks
  separated from KDA blocks. A measurement, not a feature. It decides whether S7 exists at all, and
  it is cheap compared with everything downstream of it.
- **S6** — **the prefix-handle protocol** (§3.10-B): `prompt_handle` out, `prefix_handle` +
  `prefix_handle_required` in, `prompt_handle_status` back. Argued on verifiability, not speed. Build
  S1 first — it is most of the diagnostic value for a fraction of the work — and build the
  `required` mode early because it is what makes the conformance suite loud.
- **S7** — stitching itself, only if S5 says the fraction is small and only if §9.3's Falsifier B or
  §19.3's cost model says swapping is wanted.

### The smallest thing that beats today's opencode setup

**M0 + M1**, and neither needs a line of C++. M0 is an afternoon and it converts opinion into a
baseline. M1 is a headless daemon with five read-only tools, one owned renderer, and the
`/completion` token-array fallback; on the day it runs it removes all four measured defects: the cache stops
growing without bound, the tail latency collapses from 78,598 to ~5,000 tokens of re-prefill, an
empty subagent becomes a failure instead of a success, and a long prefill shows a progress bar
instead of a hang. None of that requires compaction, permissions, firecode or a second head.

---

## 18. Test strategy

### 18.1 The invariants that must be mechanically enforced

**I1 — the generation-inclusive prefix invariant.** *This is the real one, and it is stronger than
anything in the survey.*

> Request N's prompt **plus what the model generated in turn N** must be a prefix of request N+1's
> prompt.

Grok's test pins only "request N is a prefix of request N+1". But llama.cpp caches prompt *and*
generated tokens — that is the whole mechanism of F1 — so the invariant that actually protects the
cache includes the generation. A harness can satisfy Grok's test and still diverge, and that
divergence is exactly what the 612 GB was.

Two ways to check it, and both should exist:

- **Observable, cheap, runs in production:**
  `cached_tokens(N+1) ≥ prompt_tokens(N) + predicted_tokens(N)`.
  Every term is in the standard `usage` object. A shortfall is the divergence, and its size says
  roughly where. This runs as a post-flight assertion on every turn, raising
  `Warning{code: prefix_divergence, shortfall}` and feeding §6.
- **Exact, offline, in the suite:** `/tokenize` the rendered prefix N+1 and compare against
  `tokens(prefix N) ++ generated_token_ids(N)`. **UNVERIFIED-5**: whether the chat-completions path
  can return generated token ids (`/completion` has `return_tokens`); if not, the observable form is
  the only one and that is acceptable.

**I2 — exact prefix extension of the token array.** grok's `assert_prefix_stable_pair`, ported, but
running over token ids through the real renderer and the real tokenizer. Under §4.3 it holds by
construction; the test exists because "by construction" is a claim about code that someone will edit.

**I3 — renderer fidelity: our render equals the shipped jinja's.** `/apply-template` over the whole
fixture corpus, exact string equality, parameterized over every dialect. **Required CI check** (§7.2).
This is the single guard on the one hazard self-rendering introduces, and it subsumes I2's
template-side concern entirely.

**I3b — `render_incremental ≡ render`, and `parse ∘ render ≡ id`.** Pure functions, property-tested
without a model or a server (§18.3).

**I4 — the ledger is a chain.** A periodic full re-hash equals the incremental head. Any inequality
is a renderer non-determinism bug and fails loudly.

**I5 — compaction is warm.** The summarizer request's own `f_keep ≥ 0.99` and its prefill is bounded
by the instruction's own token count. This is the measurement that distinguishes our compaction from
everyone else's, and it is a single number.

**I6 — the stable prefix is stable.** Its fingerprint does not change within a session unless a
declared event (model change, tool set change) changed it, and such an event forks the transcript.

**I7 — `finish_reason` is never dropped.** Every terminal value reaches `TurnFinished` and every
`length` is classified per §5.7. An empty `length` turn is a failure, always.

**I8 — abstention does not become success.** A tool result with `outcome != Ok` cannot be reported
upward as `Ok`, including across a subagent boundary.

**I9 — no adjudicator can override a `deny` row.** Evaluated before dispatch, dsh-style.

**I10 — a head cannot block the model stream.** Under an artificially stalled head, turn latency is
unchanged and the head is resynced.

**I11 — an inbound chat message costs exactly its own tokens.** Deliver a message of *n* tokens into
an idle session; the transcript's token count grows by exactly the rendered length of one user item
containing it. No tool envelope, no wrapper, no listener bookkeeping. This is the operator's
context-pollution requirement (§3.7) expressed as a number, and it is the kind of thing that quietly
regresses the first time someone adds "helpful" metadata to the prompt.

**I12 — no polling anywhere.** A static check plus a runtime assertion: the daemon spawns no timer or
poll loop for message delivery, and `Warning{code: no_human_channel}` is raised by a *dropped
subscription*, never by a missed poll.

### 18.2 How they are measured against a real llama.cpp

The signals, all of which exist today:

| signal | where |
|---|---|
| `f_keep` per request | `usage.prompt_tokens_details.cached_tokens / usage.prompt_tokens` |
| `n_prompt_tokens_cache` | the same field; it *is* that counter (`server-task.cpp:380`) |
| p99 re-prefill | `prompt_tokens − cached_tokens`, distribution over a scripted session |
| prefill / decode rate, draft acceptance | response `timings`, per chunk under `timings_per_token` |
| live prefill progress | `prompt_progress {total, cache, processed, time_ms}` |
| which cached entry matched, and its `lcp` | server log at `-lv 4`, one line per entry with `f_keep`/`f_sim`/`lcp`; `~/bin/glm-why-no-cache` reads it. S1 would make this an API field. |
| box-level cache and queue state | `GET /metrics`, `GET /slots` |

**The scripted session** is the unit of measurement: a fixed 30-turn conversation with a fixed tool
script, replayed identically against any client. It must include a mid-session system-prompt change
(C9), an oversized tool result (spill), a `length` truncation (C7/C8), a compaction crossing (I5) and
a fork. One script, one command, comparable numbers.

**Traps this box has already paid for, which the harness's own test runner must respect:**

- **A tok/s number without its concurrency is not a number.** A contended run gave prefill 391.7 with
  decode 21.4; the halved *decode* was the tell. Every rate is reported with `n_busy_slots`.
- **Two conversations cannot test residency.** With a floor of 1 and nothing displaced, `on its seat`
  and `resident without a seat` are timing-identical. Grep the wording, not the clock.
- **A captured pid is a proxy.** Read the pid from `ss -ltnp | grep :8080` at kill time.
- **`${X:-d}` substitutes on empty as well as unset** — an empty `SLOT_SAVE_PATH` once had a side
  server indexing production's spill directory.
- **A full model load looks like OOM** to anything watching `free`: GLM mmaps 199.7 GB.
- **With `--no-op-offload` the CPU is on the inference path.** A `-j 24` build steals prefill and
  decode from production. Build niced or in a quiet window — which applies directly to compiling a
  Rust daemon on this box.

### 18.3 Property tests worth having

- Generate random transcripts (turn counts, reasoning presence, tool-call fan-out, images, forks) and
  assert I2/I3b on every adjacent pair. Pure functions, no server, no GPU — so this can run on every
  commit at full volume, and it is where the exception surface (§14.1's three non-strict tests) gets
  exercised rather than assumed. **This is the biggest single testing win of self-rendering:** the
  invariant that used to need a live model to check is now a property of a function.
- Generate random tool outputs around the spill threshold and assert `len(preview + notice) ≤ cap`
  exactly — deepseek's reserve arithmetic is fiddly and off-by-one there is a silent truncation.
- Generate adjudication policies and action classes and assert every action matches exactly one rule,
  or the config is rejected at load.

### 18.4 Culture, not code

Adopt deepseek-harness's README convention verbatim, per prompt-contributing feature: **"What the
model sees"** (the literal text), **"Token effect"**, **"KV Cache effect"** (does this vary per
request, per process, or never). ~259 of their package READMEs carry it. It is the single most
transferable thing in the survey, and it is what stops the `<env>` regression from happening twice.

---

## 19. Future directions, and what they demand of v1

Not v1 features. But each is expensive to retrofit, so each constrains the v1 data model. For every
one: what must be true of v1 so the door stays open, and what would slam it.

All three want the same thing: **context as addressable segments, not a flat message list.** One
decision unlocks all three. That is why `SegmentMark` (§4.2) and the `segment` table (§4.4) are in
v1 despite doing nothing in v1.

### 19.1 Multi-head compaction with different decay rates

Several compaction heads over one conversation, each decaying at its own rate: a thread the operator
is working stays fine-grained while an abandoned one coarsens fast, independently.

**Load-bearing in v1:**

- **L1.** Compaction must never be a whole-history operation. §10 is already written this way: levels
  operate on segments, and no operation spans the conversation. Building a single `summarize(history)`
  entry point in v1 would slam this door, because everything downstream would assume one summary.
- **L2.** A segment carries its own `level` and `last_touched`, and `last_touched` is what promotes
  or demotes it — not `created_at`. This is the whole of "different decay rates"; it falls out of the
  level rule rather than being a separate feature. Cost if the futures never arrive: two columns and
  a zero-width transcript item. That is the cheapest option this plan buys.
- **L3.** Summaries are stored per segment in the journal, not folded irreversibly into the
  transcript, so a segment can be re-levelled without re-summarising from scratch.

**Free to revisit later:** the number of levels, the ratios, the promotion policy, and whether a
"head" is a named view (a saved set of per-segment levels) or just the current level assignment. I
would start with the latter and add named views only if the operator actually wants two
simultaneously.

**Shared policy with the server's cache ladder? No — §10.5.** Same vocabulary, same "least-degraded
first, LRU within a rung" rule, independent policies, because memory pressure must cost latency and
never fidelity.

### 19.2 User-assisted compaction

*"These are the topics I have; which do you most want preserved?"*

**Load-bearing in v1:**

- **L4.** Compaction is interruptible and can ask a human mid-operation over the same channel as
  adjudication. §11.6 already makes questions and permissions one mechanism, explicitly so that this
  does not need a second channel built later. Building a permission-only path would slam this.
- **L5.** **A segment is labelled at creation, not reconstructed later.** This is a data-model
  requirement, not a UI one, and it is the one that is genuinely awkward to retrofit: reconstructing
  topic labels for 200 old segments means running a model over the whole history, which is the
  stop-the-world pass §10 exists to abolish. So v1 must assign a label when a segment opens.

  How, without a model call: a segment opens at a user boundary, and its label is the first ~80
  characters of the user message that opened it, plus the tools used within it. Crude and adequate —
  a human recognises "the RCCL / PCIe atomics thing" from that. A model-generated label can replace
  it later opportunistically, and because the label lives in the `segment` table and renders to
  nothing, replacing it costs no prefix.

**Free to revisit later:** the interaction shape (a list to tick, free text, a ranking), and whether
the answer is advisory or binding.

### 19.3 Agentic memory — "infinite memory via explicit search and attention segments that are swappable"

The hardest of the three, and the one where I will not pretend the cost is small.

**The cost model, stated honestly.** Swapping a segment back in mid-conversation rewrites the prefix
from that point. Concretely, on this box:

| where the swapped segment sits | tokens re-prefilled | seconds at 441 t/s |
|---|---|---|
| in the last 5k tokens | ~5,000 | ~11 s |
| halfway through a 150k conversation | ~75,000 | ~170 s |
| at the head of a 200k conversation | ~200,000 | ~453 s |

Swappable memory is **not free**, it is *linear in the suffix*, and any design that presents it
otherwise is wrong. That is the physics of §0.1 and no cache index changes it. The three ways out
are §3.6's: position shifting (measured unsafe on GLM's recurrent components, must be re-earned),
stitching (a server change, CacheBlend lineage, recompute a fraction), or paying.

**So the honest v1 recommendation is: append, do not swap.** A retrieved memory is *appended* as a
new item — free, prefix-preserving, and it costs occupancy rather than prefill. Swapping is the
optimisation you reach for when occupancy actually binds, and by then §10's levels have already
reclaimed most of it.

**Load-bearing in v1:**

- **L6.** Segments are **addressable and independently serialisable**: a segment has an id, its
  transcript items are recoverable by that id, and its rendered bytes are a contiguous span in the
  ledger. Without contiguity, stitching has nothing to address, so this is the one v1 decision that
  a later server-side change genuinely depends on. It costs nothing today because the ledger is
  already a span map.
- **L7.** Nothing in the prompt pipeline may assume the transcript is exactly what the session
  contains. The pipeline takes an *ordered selection* of items; in v1 that selection is always "all
  of them", but the seam exists.

**What would slam the door:** rendering items directly from a message array without a span map, or
letting compaction rewrite items in place instead of forking (§5.5).

**Built on the `oracle` pattern, or a first-class subsystem?** **On the oracle pattern — memory as a
searchable corpus behind a tool** — with one harness-side addition. Reasons:

1. It keeps the harness out of retrieval (§1, §9.4), which is a line worth holding: retrieval is a
   whole discipline with its own failure modes, and oracle has already paid for the lessons
   (resemblance is not truth; the first stage sets the ceiling; recall@64 is the number).
2. It is prefix-friendly by construction: a tool call and its result are appends.
3. It composes with §8.2 — a memory search that finds nothing *abstains*, structurally, and that
   abstention cannot be laundered into an answer.

The harness-side addition is small and it is the part oracle cannot do: **the harness owns
segment-to-corpus ingestion.** When a segment is demoted to L2 or below, its verbatim text is written
to a per-session corpus behind a `search_session_memory` tool. So the coarse summary stays in the
prompt and the detail stays retrievable — which is what makes fidelity amplification (§10.1)
survivable rather than lossy. That is a genuine answer to "infinite memory", and it needs no
swapping at all.

**Where I disagree with the brief, flagged rather than smoothed:** §13 of the brief says it is
acceptable that agentic memory fights append-only prompts. It is acceptable *as a research
direction*, and I have costed it above. But I do not recommend building segment swapping into v1 or
v2 on this stack, because the measured facts point away from it: decode is flat against depth (F2),
the pool is elastic and a deep conversation is 10× better *value* to keep than a shallow one by the
server's own eviction arithmetic, and the L2 tier makes a cold conversation cheap to bring back
whole. On this box the cheap move is **stay deep and stay warm**, not **swap**. If §9.3's Falsifier B
shows quality collapsing with depth well before the context window, that conclusion inverts and
swapping becomes worth its price — which is another reason Falsifier B is a milestone and not a
footnote.

---

## 20. Risks and open questions

### 20.1 Unverified, with the procedure that settles each

- **UNVERIFIED-1 and UNVERIFIED-2 are DISSOLVED, not answered.** Earlier drafts flagged "does GLM's
  template re-render `reasoning_content` into `<think>`?" and "does it honour a system message at
  index > 0?" as gating unknowns. Under self-rendering (§3.1, §5.4) neither question arises — we emit
  the tokens. What replaces them is *behavioural*: was this model trained to attend to a system turn
  arriving mid-conversation? That is an eval, not a template read, and §5.3's envelope is the
  conservative default until the eval exists.
- **UNVERIFIED-2b — Does our renderer match the shipped jinja for every fixture?** The new form of the
  risk, and the reason §7.2 is a required CI check rather than a nice-to-have. Unlike the old
  questions this one is answered continuously and automatically, which is the improvement.
- **UNVERIFIED-3 — Should a firecode checkpoint and the conversation's KV state be checkpointed
  together?** The pieces exist (the L2 tier persists slot state) but the sizes are wildly mismatched
  and nobody has measured whether the KV half is still resident when the VM half is restored. Do not
  build for it in v1.
- **UNVERIFIED-4 — opencode's `OUTPUT_TOKEN_MAX = 32_000`.** A repo-wide search of
  `packages/opencode/src` and `packages/core/src` in the current checkout found no hardcoded 32k
  output default; every occurrence is a test fixture. The survey cites `provider/transform.ts:18`.
  Either the checkout moved or the citation is off. *Settled by:* checking the exact v1.18.29 tag
  before the claim is repeated. The *behavioural* finding (an empty subagent recorded as success) is
  independently supported by the nine database rows and is not in doubt.
- **UNVERIFIED-5 — Can the chat-completions path return generated token ids?** Would make I1's exact
  form available offline. `/completion` has `return_tokens`; the chat path is unchecked. The
  observable form of I1 works regardless.
- **UNVERIFIED-6 — Does `/apply-template` behave identically under the model router
  (`--models-max`)?** `server.cpp:239` routes it to `models_routes->proxy_post` in router mode, which
  is a different code path from the single-model handler at `server-context.cpp:7745`. If it proxies
  to a sleeping child it may trigger a load. *Settled by:* one call against the router with a
  stopwatch, in a quiet window.
- **UNVERIFIED-7 — Which model for auto mode.** Qwen 3.8 27B dense is a recommendation from
  availability and latency predictability, not a measurement. It also has no preset in
  `router-presets.ini` yet. M7's shadow mode is how it gets decided.
- **UNVERIFIED-8 — Qwen 3.8 27B dense's dialect.** Predicted `qwen` thinking format with
  `preserve_thinking`; P4 decides. It is a *dense* model, and every `preserve_thinking` reference in
  omp is about the Qwen3.6+/3.8 template family generally, so the prediction is reasonable and
  unconfirmed.
- **UNVERIFIED-9 — Whether flowy's `GET /api/stream` is cursor-resumable.** §12.2 ask 6. The bridge
  must not guess.
- **UNVERIFIED-10 — Whether two flowy clients can drive one dsh `web` session.** Reported as not
  stated in dsh's docs. It only matters as prior art; §13 does not depend on it.
- **UNVERIFIED-11 — `firecode in`'s cost per call.** The exec channel is one command per connection.
  A `bash` tool that pays a connection setup per call may be fine or may be a per-call tax that
  matters at agent-loop rates. Measure before designing around it.
- **UNVERIFIED-12 — The file channel's throughput** (vsock port 1025, tar in and out) for
  read/write/edit tools. Same shape of question.
- **UNVERIFIED-13 — Whether `--cache-reuse` is actually inert on the live build.** GLM-STATE says it
  is *"PASSED BUT REFUSED AT LOAD"* and that the per-request field is ignored, while the launcher
  still passes `--cache-reuse 256` and the launcher's own comment describes it as live. Two documents
  disagree. It matters because a fork (§5.5) is exactly the mid-prompt divergence cache-reuse acts
  on, and cache-reuse was measured returning *wrong answers* on GLM. *Settled by:* grep the startup
  log for the refusal line. **Do this before M4.**
- **UNVERIFIED-17 — Is more than one named reader per principal intended in flowy?** It is
  mechanically allowed and used nowhere. It decides §12.2 ask 5: whether `claude-lab2x1/harnessd` and
  `claude-lab2x1/session` can coexist, or whether the harness needs its own seat per agent. *Settled
  by:* asking whoever owns flowy, not by testing — a mechanism that works but was never intended is
  not a feature.
- **UNVERIFIED-18 — How should a streaming listener be classified by `waiter_kind`?**
  (`schema.sql:164-180`.) Unsettled in the source. It matters because `GET /api/presence` is the only
  server-verified answer to "is a human reachable", and §11.5 changes policy on that answer — a
  streaming listener misreported as absent would silently shorten every human timeout.
- **UNVERIFIED-19 — flowy's licence.** Needed before any of its tests are ported (§14.2). It is the
  operator's own code, so this is a question of intent rather than of law, but the ported suite's
  `NOTICE` needs an answer.
- **UNVERIFIED-15 — Is per-turn tokenization a measurable share of warm-path latency?** On a request
  with `f_keep` ≈ 1.0 the server does almost no prefill, so re-rendering and re-tokenizing ~200k
  tokens competes against roughly nothing. *Settled by:* wall clock from request-sent to first token,
  minus `timings.prompt_ms + predicted_ms`, over the scripted session at three depths. Tens of
  milliseconds means the handle protocol (§3.10-B) is a verifiability feature only; hundreds means it
  is also a latency feature.
- **UNVERIFIED-16 — Can stitching work at all on GLM's 34 recurrent KDA blocks?** CacheBlend's
  "recompute a fraction" premise appears to assume attention-only layers; a recurrent state at
  position *n* depends on all *n* tokens, so a partial recompute is either exact or wrong. If so,
  stitching applies to 12 of 46 blocks and the economics collapse. *Settled by:* §3.10-C's experiment,
  run per layer class. **This is the highest-value unanswered question in the whole plan**, because
  it decides whether §19.3's recommendation against swapping is permanent or provisional.
- **UNVERIFIED-14 — GLM's depth-collapse guard.** The launcher says the collapse does not reproduce
  on this build, probed to 147,042 tokens; GLM-STATE says depth collapse *poisons the slot* and that
  1M has never been run. The repetition guard (§8.5) costs nothing, so build it regardless, but do
  not treat "does not reproduce" as "cannot happen" above 147k.

### 20.2 Risks

- **No cloud fallback, and the edge would have to be rewritten to get one.** Deleting the provider
  abstraction means a later *"run this turn on a frontier model"* is not a config change: the
  renderer, the tokenizer, the submit path and the whole fused control vocabulary
  (`Admit`/`Prewarm`/`Spans`/`SegmentHint`/`Explain`) have no cloud equivalent, and the append-only
  token vector — the mechanism this design rests on — has no meaning against an API that tokenizes
  server-side. Knowingly accepted; recorded so nobody is surprised later. The mitigation is **not** an
  abstraction layer (that forfeits the payoff); it is §3.5's named-module discipline, so if the
  decision reverses, the three interfaces to rewrite are already identified.
- **Fusion couples us to llama.cpp's internals.** §3.5 states the exit and the three interfaces that
  would need re-abstracting. The one that cannot be ported is the fused control vocabulary, and losing
  it means *going back to being a normal client* — i.e. back to where every harness in the survey is.
- **Self-rendering shifts template drift onto us.** §7.2's CI diff is the entire mitigation. If that
  check is ever made advisory, this becomes the most dangerous risk in the plan: its failure mode is
  silent and its symptom is a cache leak measured in hundreds of gigabytes.
- **Rust is slower to write than the schedule wants.** Mitigated by making the decision reversible
  (§16): the suite is language-independent, so switching to Bun later costs the daemon, not the
  measurements. Watch for it at M2; if M1 has not landed in three weeks, take the hedge.
- **Building on this box steals inference.** With `--no-op-offload` the CPU is on the inference path.
  A Rust workspace of this size is a real `-j 24` build. Nice it, or build in a quiet window, or
  build in a firecode run — which is a pleasing use of the substrate and worth trying early.
- **flowy is a moving target and is not ours.** Mitigated by §12.3's degraded mode, which is a
  deliberate design constraint: no milestone may block on a flowy change.
- **A renderer could diverge from a shipped template we cannot match.** Then either our renderer is
  wrong, or the template is doing something we chose not to reproduce. The honest responses are to fix
  the renderer or to patch the template — not a "compat quirk" that papers over a divergence. Say so
  out loud if it happens.
- **Self-rendering makes us responsible for template drift.** A model update that changes the jinja is
  now our problem rather than the server's. Mitigated entirely by §7.2 running on every commit and on
  every `template_sha` change; unmitigated only if someone makes that check advisory.
- **Auto mode drifting.** The single most dangerous new component, because a model adjudicator that
  slowly loosens is precisely F5. Mitigated by shadow mode running *forever* on a traffic sample, not
  just during qualification (§11.7).
- **Segments turning out to be the wrong abstraction.** They cost two columns and a zero-width item
  in v1, so the downside is bounded; the upside is that all three §19 futures need them.
- **The harness finally exercising concurrency could destabilise the server.** M6 is the first real
  concurrent client this box has had. Expect to find scheduler bugs; that is a *feature* of the
  milestone, and it is why M6 has explicit `/metrics` and `/slots` observation rather than just a
  pass/fail.

### 20.3 Corrections to the brief and the survey

Offered as flagged disagreements rather than smoothed over, per instruction.

- **The supersede rule is containment, not "exact full-prefix match".** The brief cites
  `server-task.cpp:2427`. In the current checkout the mechanism is `server_prompt_cache::alloc`
  removing *"any cached prompts that are fully contained in the current prompt"* (`:2483-2497`,
  `len == it->prompt.tokens.size()`), plus `contains()` at `:2449-2458` skipping the save when the
  new prompt is already covered. This strengthens the point rather than weakening it — see F1a,
  "append-only is the cache's garbage collector" — but the line number and the wording should be
  corrected, and line numbers across `server-task.cpp` have moved since the brief was written because
  tonight's cache-ladder landed. `f_keep`/`f_sim` are now at `:2598-2626`, not `:2515-2541`.
- **grok-build's path.** `crates/codegen/xai-chat-state/src/actor/tests.rs:4596`, not
  `chat-state/src/actor/tests.rs:4594`.
- **omp's append-only test count.** 59 cases across 8 blocks; the relevant `describe("message sync")`
  block has 18, not 17.
- **opencode's `test/session/` count.** A static count found ~363 across 19 files, not 421 — likely
  an undercount of `it.each` expansions, so both numbers may be defensible. Not important, but the
  suite should count them itself rather than inherit a number.
- **The brief's §13.3 acceptance that agentic memory may fight append-only** — I have costed it in
  §19.3 and recommended against building swapping in v1 or v2, with the falsifier that would change
  my mind. This is a disagreement, not a misunderstanding.
- **Operational note, not a plan matter:** during the reconnaissance for this plan a research
  subagent read `/home/dead/.config/flowy/env-claude-lab2x1` with `cat` before recognising that it
  contains the live `FLOWY_TOKEN` value rather than only variable names, so the token appeared in a
  tool transcript. Nothing was done with it and it is not reproduced anywhere in this document, but
  the operator may wish to `flowy mint` a fresh token for `claude-lab2x1`.

### 20.4 What I could not settle at all

- **Whether §9's position is right.** It is a judgement built on two measurements pointing in
  opposite directions, and §9.3 states the three experiments that would settle it. Falsifier B —
  quality against depth on this stack — has never been run by anyone, and it is the load-bearing one.
- **Whether the volume of human adjudication will be tolerable.** The policy table (§11.3) is a first
  guess. It is configuration precisely because it will be wrong, and §6 is where being wrong shows up.
- **Whether one daemon is enough** when the operator wants "one deep orchestrator at up to 1M tokens
  plus a dozen to thirty shallow agent conversations." Nothing in this design forbids it, but nothing
  in it has been sized for thirty concurrent sessions either, and the server-side arithmetic
  (437 MiB fixed per sequence, ceiling 5 on CUDA0 today) says the binding constraint is a third GPU,
  not the harness. Revisit at M6 with real numbers.
