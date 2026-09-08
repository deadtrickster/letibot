# Design brief — local-first agent harness

Consolidated 2026-09-08 from a working session on lab2x1. This supersedes the nine
messages sent piecemeal to the planning agent; where those contradicted each other, the
retractions are applied here and recorded at the end.

Companion documents:
- `/home/dead/harness-survey.md` — the survey of five existing harnesses, with measurements
- `/home/dead/harness-implementation-plan.md` — the plan being written against this brief
- `/home/dead/Projects/glm-serving-notes/GLM-STATE.md` — the serving stack this runs against

Attribution matters in what follows. Lines marked **[operator]** are the operator's own
position. Everything else is inference, and inference is fallible — three of my diagnoses
during the session that produced this brief were wrong.

---

## 1. The thesis

**[operator]** *"My goal is to have my LLM runtime behave like a normal data app. Don't see
any reasons it can't."*

This is the spine. Every failure measured during the session has a name in data systems,
and the fix is usually the one that discipline settled decades ago:

| what broke | the data-systems equivalent |
|---|---|
| cache entries never superseded — 291 entries, 612 GB, four OOM kills | an index that never merges duplicates |
| a cap on bytes but none on entry count | a buffer pool with no page limit |
| eviction spills a whole entry or nothing | no graded replacement: no LRU-K, CLOCK, ARC, no cost model |
| compaction is a 13-minute stop-the-world pass | pre-LSM compaction |
| `finish_reason: length` parsed then discarded; empty result reported as success | swallowing an error return |
| OOM under a `MAP_POPULATE` burst | no admission control, no backpressure |
| accounted cache 68.3 GiB against RssAnon 54.2 GiB | wrong statistics, so the planner decides wrongly |
| three misdiagnoses in one session | no EXPLAIN, no per-stage metrics |

Two are more than analogy:

**Multi-head compaction with different decay rates is leveled compaction.** LSM trees exist
for this: recent data fine-grained in small levels, older merged into progressively coarser
ones, incrementally and in the background rather than one blocking pass. Applied to
conversation context it does not make the 13-minute stall faster — it removes the category.
It inherits the known cost too: write amplification's analogue is fidelity lost to repeated
re-summarisation.

**Per-stage observability is EXPLAIN.** "Why did this request prefill 144,436 tokens with
zero cache hits?" is a query-plan question. Answering it took hours of log archaeology. A
runtime behaving like a data app answers it directly: which prefix matched, where it
diverged, what that cost, how much was cache and how much compute.

Where the analogy stops: attention is position-dependent through RoPE, so KV "pages" are
not freely relocatable the way database pages are. Say so rather than letting the metaphor
run past its evidence — the operator is a Postgres person and a sloppy database metaphor
reads worse than none.

---

## 2. What was measured, and why any of this matters

All from opencode v1.18.29 against GLM-5.3-Flash on llama.cpp, 2026-09-08:

- **Reasoning was not replayed.** `interleaved` defaults false (`provider/provider.ts:1304`).
  llama.cpp caches prompt *plus generated* tokens, so the cached entry held the `<think>`
  block while the next prompt omitted it. Every turn diverged from its own cache entry, the
  strict-prefix supersede test never fired (`server-task.cpp:2427`), entries grew to 291 /
  612 GB, and the server was OOM-killed four times in a day.
- Turning replay on moved `f_keep` p10 from **0.000 to 0.999** and p99 re-prefill from
  **78,598 tokens to 5,294**. The median barely moved. The entire gain is in the tail, which
  is what "responsive" means in an interactive loop.
- **Compaction cold-prefills by construction.** It flattens the conversation into one user
  message with `system: []` and `tools: {}` (`session/compaction.ts:380-445`). Measured:
  144,436 tokens, cache hit 0, ~13 minutes before the first summary token.
- **`OUTPUT_TOKEN_MAX = 32_000`** silently capped output; `finish_reason: "length"` was
  parsed and discarded (`case "finish": return`). A subagent spent its whole budget thinking,
  returned an empty result, and that was recorded as success. Nine such messages in the DB.
- **The renderer was quadratic** — full markdown re-parse of the reasoning block on every
  delta.
- Server-side: spill files left dirty in page cache starved the allocator (fixed with
  `fadvise`; `Dirty` fell from tens of GiB to 23 MB, `MemAvailable` 14 → 91 GiB). Context
  checkpoints are ~half of every cache entry (40.6 KiB/token measured against 19.25 KiB/token
  of raw KV).

The survey found the same defects distributed across every harness read. Nobody has warm
compaction on the default path. Only Grok Build enforces prefix stability with a test.

---

## 3. Model targets

Three, all local llama.cpp, all first class:

| model | notes |
|---|---|
| GLM-5.3-Flash | production today; thinking format `zai`; ctx 262,144; `reasoning_content` replay is what makes its prefix cache hit |
| Qwen Flash (`qwen-3.8-flash-next`) | thinking format `qwen`; needs `preserve_thinking` / `chat_template_kwargs` |
| Qwen 3.8 27B dense | |

Different chat templates, different reasoning conventions. **Per-model dialects are
architecture, not configuration.** The plan must say how a fourth model is added and what
must be verified when it is.

---

## 4. Non-negotiable properties

Derived from measurement, not preference:

- **The prompt is append-only.** A later request must be a byte-exact prefix extension of
  the earlier one. Grok Build's `assert_prefix_stable_pair`
  (`chat-state/src/actor/tests.rs:4594`) is the model, and **that test should exist before
  the harness does**.
- **Reasoning is replayed verbatim** into the field the server used. Capability-detected,
  never hostname-detected — omp's detection silently disables itself behind a proxy, and the
  symptom is the 612 GB cache growth.
- **Compaction reuses the warm prefix**: real system, real tools, real messages, instruction
  appended last. Nobody surveyed does this on the default path. Original work.
- **System prompt changes are appended, not rewritten** (DeepSeek's
  `SystemPromptUpdate: 'in-history'`: the model reads the latest `system` message at any
  position, so a change is appended after the cached history instead of rewriting message 0).
  Adding one line to an `<env>` block cost a full cold re-prefill of a 179k conversation
  during this session.
- **No hardcoded output cap.** `finish_reason: length` acted on and surfaced.
- **Render cost independent of accumulated output length.**
- **Arbitrary request body fields out** (`return_progress: true`) and **non-standard streamed
  response fields in** (`prompt_progress {total, cache, processed, time_ms}`, `timings`). The
  latter drives a real progress display, which nothing surveyed has.

---

## 5. Execution substrate — firecode

**[operator]** firecode "was the attempt to calm down `--dangerously-skip-permissions`. The
point is to isolate the agent and spare me hitting enter."

`/data/scratch/harness-survey/firecode` (private, 315 commits, shell + python). Read
`ARCHITECTURE.md` and `README.md`.

A **run** is one microVM and everything created for it. Lifetime is a cgroup subtree, not a
process tree, "because a process tree cannot express what a run owns" — `cgroup.kill` ends a
run and every VM started on its behalf, transitively; liveness is "is the cgroup populated".
Root is overlayfs layers (base / docker images by digest / main checkout / per-run writable
upper), so nothing is copied per run. Two hypervisors behind one identical guest image:
firecracker for 60 ms checkpoint restore, libvirt/qemu when a GPU must be passed through.
The guest is untrusted by construction — project paths are resolved from host config and
never taken from the caller, parentage is derived from the connection rather than believed,
and there is deliberately no host-side seed script.

**The harness must not reinvent any of this.** It must say how it composes: does a session
map to a run, a subagent to a child run, and who owns lifetime. Nested agents, delegation
while staying in the conversation, and watching a VM from outside already work.

firecode is the operator's own repo and **changes to it are allowed**. Where the harness
needs something firecode lacks, specify the change rather than routing around it.

---

## 6. Adjudication — pluggable, not doctrinal

**[operator]** *"We are flexible. It is ok to ask me, it is ok to use firecode, and it is ok
if I plug a model for auto mode."*

Three first-class adjudicators behind **one interface**, so a decision has the same shape
whatever produced it:

1. **The boundary** — firecode isolation. No decision is made because the action is
   structurally safe inside the run. This is the default for the overwhelming majority of
   tool calls, and it is why allow-all inside a sandbox is a sane posture.
2. **The human** — over flowy, or firecode's `--ask`. For genuinely ambiguous calls, and for
   decisions the agent cannot make on the information available.
3. **A model** — "auto mode". A separate, probably smaller model adjudicates. Design
   explicitly: what it sees, what it returns, whether it can **escalate** to a human rather
   than only allow/deny, what happens when it is unavailable, and how its decisions are
   logged and reviewed. A drifting auto-adjudicator is exactly the "did less than it claimed
   and said nothing" failure.

Routing is **configuration, not code**: an action class maps to an adjudicator, changeable
without touching the harness. Escalation is a first-class path in both directions.
Everything is auditable — who decided, on what basis, when, and what the agent did next.

Keep firecode's hard-won `--ask` semantics, precisely because asks should be uncommon: post
and wait; if nobody answers, say so and tell the agent to decide and record what it chose;
acknowledge before acting, because silence is indistinguishable from absence; start
listening before you start working. A blocking MCP call cannot be a permanent listener — it
freezes the caller's turn — so the listener is a background process that exits per message.

Asks-per-hour is an **observable, not a budget**. Frequent human adjudication is a signal
that the boundary is drawn wrong or the policy is misrouted.

---

## 7. flowy — chat and permissions

**[operator]** The harness must work with flowy as a **server/client** relationship, not a
terminal tool: permissions answerable from a **private chat with the agent**, and **first-class
chatting** — you can converse with a running agent, not merely dispatch tasks to it.

**[operator]** *"I don't want to fiddle with monitors, nor timers which pollute the context."*

**Chat is a first-class channel of the agent loop — not a transport for adjudication, and not a
tool.** What is wrong today, mechanically: the only way to hear a message is a background poll loop
(`flowy inbox --as NAME`) that blocks, prints one line, exits, and is re-armed. firecode records the
same workaround and the reason — *"a blocking MCP call cannot be a permanent listener, it freezes the
caller's turn, so the listener is a background shell running `firecode chat --inbox`, which exits on
each message."* Every arrival therefore lands in context as a tool invocation plus a tool result plus
the re-arm. The listener is a timer in disguise and its bookkeeping costs context permanently.

What the fused harness must do instead:

1. **Inbound messages are pushed, not polled.** `harnessd` holds the connection; a message arrives as
   an EVENT delivered into the session. No background listener, no re-arm, no interval, no
   Monitor-shaped scaffolding anywhere in the design.
2. **A chat message costs exactly what a user message costs — it IS a user message.** No tool
   envelope, no "you received a message" wrapper, no polling artefacts. Measurable: an inbound
   message adds its own tokens and nothing else.
3. **Messages may arrive mid-turn.** Design for steering; pi already does this
   (`agent-loop.ts:205-206` pushes steering messages into the running loop). Say what happens to a
   turn in flight — injected at the next step boundary, interrupts generation, or queued — and argue
   the choice.
4. **Chat is bidirectional and symmetric with adjudication.** An `--ask` is a message that blocks on
   a reply: one channel, two usages. Chat is the primitive and a decision request is the special
   case, not the reverse.
5. **It is a head** (§8). A flowy chat is one attached client among several, so the multi-head design
   and the chat design are the same design; do not solve them twice.

Fold into "What flowy must provide" whatever push/streaming primitive is needed so a client is
delivered messages without polling — a long-lived stream, a websocket, a subscription. Do not design
around `flowy inbox`'s exit-per-message shape.

Note the fusion angle: because `harnessd` is long-lived and owns the session, holding a subscription
is natural, and a per-turn CLI process could never do it. This is one of the clearer arguments FOR
the fused daemon architecture.

### 7.1 What flowy's delivery machinery actually does

Surveyed from source at `/data/scratch/harness-survey/flowy/`. Four findings that change the asks:

- **Three wake levels, not four; "human-only" does not exist.** One predicate, `wakesFor`
  (`internal/flowy/inbox.go:189-331`): **all** / **addressed** (`--to-me`) / **mentionsOnly**
  (`--mentions`), plus two orthogonal scopes (`--room`, `--focus P`) and a per-room binary mute.
  `saidByAPerson` (`:340-351`) is a *widening* clause inside `addressed` — a human's UNADDRESSED
  broadcast still wakes you, because agents pass those by habit (the operator's words are quoted at
  `:305-308`). `--mentions` turns that clause off. Author kind is stamped from the token at write
  time (`chat.go:86-91`), so a client cannot claim to be human.
- **Per-room policy does not exist, and the source says why it cannot.** The level is sent as query
  params per poll and stored nowhere; the reader row (`schema.sql:143-202`) has no filter columns.
  `inbox.go:132-137`: *"IT IS ONE FLAG BECAUSE IT HAS TO BE… a reader belongs to a NAME, and a second
  waiter on that name is refused."* The operator remembers per-room policy because it is what he
  wants, not what exists. **The harness should provide it**: put the level on a per-room subscription
  row (off / mentions / addressed / all), keep flowy's *definitions*, drop its one-flag shape, and
  make the human-broadcast clause an explicit named option rather than baking it into "addressed" —
  the fleet paid for that being implicit twice, once in each direction.
- **Chat is NOT on a pushed transport.** SSE exists (`GET /api/stream`) but `streamTopics`
  (`stream.go:92-119`) is todos and queue only; chat is long-poll, exit-per-message by construction.
  So chat-as-pushed-event **requires a flowy change**. File it verbatim:

  > `GET /api/inbox/stream?as=NAME&addressed=&mentions=&focus=&room=` — same `wakesFor`, same reader
  > row, same enrichments, same heartbeat, one JSON message per SSE event, `POST /api/inbox/ack`
  > unchanged.

  Two smaller asks: a fourth `waiter_kind` for a streaming listener (`schema.sql:164-180`), or the
  roster misreports it; and Postgres `LISTEN`/`NOTIFY` to remove the internal 250 ms tick, named as
  the known gap at `stream.go:71-76`.
- **flowy has no blocking ask.** `agentanswer.go` is a terminal query answerer (the write-side twin
  of `agentscrub.go`), not asks. The nearest thing is `todo_waiting_on` (`mcp_waiting.go`):
  non-blocking, board-visible, **self-clearing** — any note or write by the named person is their
  answer. Consider this shape for §6 adjudication instead of a blocked call; it composes with
  firecode's `--ask` and does not hold a turn hostage.

**Do not copy:** exit-per-message delivery; the forked-successor handover (`inboxhandover.go`) — a
detached listener that hears everything and can wake nobody, which needed a schema column and a
stand-down protocol to stop it lying, and which a persistent connection eliminates entirely; 250 ms
polling behind an SSE facade; filter policy passed per-poll and stored nowhere; preferences in an
untyped note body.

**Open, unsettled by the source:** whether multiple named readers per principal is intended
(mechanically allowed, used nowhere), and how a streaming listener should be classified by
`waiter_kind`.

**[operator]** *It is fine if flowy does not support something yet — the host agent will
extend it.* So the plan carries a section **"What flowy must provide"**, written as a request
another engineer can implement. Design what is needed; do not design around current limits.

Open question the plan must answer: **flowy and the firecode room are two chat systems.** Do
they unify, does one become a transport for the other, or do they stay separate with a
bridge? "flowy grows to subsume the firecode room" is a legitimate answer.

Note: opencode's `permission.ask` hook is declared (`plugin/index.ts:261`) with **zero
trigger sites repo-wide**. It is dead. There is no prior art to copy there.

---

## 8. Multi-head attachment

**[operator]** One agent session, **multiple clients attached and detached at will** — like
tmux/byobu, and like Claude Code's `/remote`. A local TUI, a flowy chat and a remote head may
all be attached at once.

The plan must address: what is authoritative state, how a late-joining head catches up, how
streaming output fans out, what happens when two heads act simultaneously, and what happens
when every head detaches mid-turn — the agent must keep working.

**Several of these are already solved in flowy; adopt rather than invent.**

- Server-side cursor per named reader, acked **after** the client has written the messages out
  (`inbox.go:489-493`) — a crash costs a duplicate, never a silence.
- **The mark advances over everything READ, not everything delivered** (`:421-428`) — the single most
  reusable bug in that repo: a filtering consumer that skips this rereads its own output forever.
- **Report what was filtered out** (`Skipped`, `:55-58`): "busy and none of it was for me" is a
  different fact from "silent". It makes a broken filter visible.
- **One waiter per name**, enforced by a written claim plus `kill -0`, refusal naming the pid
  (`waiterlock.go`). For multi-head: several heads share ONE authoritative reader; they must not each
  hold one.
- **Idle means quiet, not unwatched** — reaping on detach kills a twenty-minute build when the tab
  closes. That is "every head detaches mid-turn", already solved.
- **Bounded scrollback that discloses the drop** (`agent_ws.go:106-108`; `hello.Dropped`
  present-and-zero, not `omitempty`) — a late head is told what it will never see.
- **Snapshot and register under one lock** — *"the missing byte is usually the prompt."*
- **Non-blocking fan-out; drop the slow head and tell it** — one stalled client must never stall the
  agent.
- **`agentscrub` generalises**: live and stored are different artifacts of the same stream, and
  anything *interactive* must be stripped from the stored copy. For us that is permission prompts,
  `--ask` questions, progress frames and partial tool output replayed to a late head — otherwise they
  are answered or acted on twice. Also: answers are owed only while no reader is attached, and queued
  replies are **discarded rather than held** — *"a reply to a question asked ten minutes ago is not an
  answer, it is input arriving from nowhere."*

---

## 9. The tool contract

From the operator's own `oracle` project (`/data/scratch/harness-survey/oracle`; read
`FINDINGS.md`, `design-grounded-agent.md`, `RECOMMENDATIONS.md`, `RUBRIC.md`).

**[operator]** *"The harness must close the loop; never paper over it with prompt."* And, on
this session: a `find` tool must return what is actually needed, so the model needs no
workarounds and context is not bloated by the retry dance.

Concretely, from oracle: malformed tool calls are salvaged rather than dropped (33% drop rate
to 0%); a tool that misses a selector returns the surrounding context and what *would* have
worked instead of dead-ending on "not found".

**Every tool's failure mode is a design decision, and a bad one is paid for in permanent
context, not in one wasted call.** A miss should be self-correcting within the same call
wherever possible. Make this a contract every built-in tool meets, and say how it is tested.

Four more principles from the same source:

- **Tool abstention must be structural, not advisory.** In `FINDINGS.md` the retrieval tool
  honestly reported "the corpus doesn't cover this" — and the model wrote a confident answer
  on top of it, inventing a term, finding an adjacent passage, relabelling it, dropping two
  items, and attaching a citation. *"A hallucination wearing a footnote is more dangerous
  than a naked one."* A result that says "no answer" must be structurally distinguishable
  from one that says "here is the answer", and must stay distinguishable as it propagates up.
- **Per-stage observability, or you will misdiagnose.** *"I have no way to measure search on
  its own… that ambiguity is exactly the hole I fell into three times tonight."*
- **Tool count is a measured ceiling for small models.** Past ~5–7 MCP servers they get worse
  at choosing; 15 tools tested fine, 23 was "near the edge". The stated fix is task-focused
  agents over one mega-agent. Our three targets are local models, so this binds: state the
  tool budget per role and how the harness enforces it.
- **Discipline is structural, not prompted.** *"Make the extract-then-answer discipline
  STRUCTURAL, not just prompted… the scratchpad is a real intermediate artifact between
  nodes, so it cannot be skipped."* Sub-agents given `tools: []` deliberately, to shrink the
  hallucination surface. Where a step must happen, make it a node the model cannot route
  around.

Smaller, worth a line: their prompt never said what language to answer in, and a
Chinese-trained model drifted into Chinese mid-sentence under Russian input. A
prompt-completeness failure, not a model failure.

---

## 10. The tension: cache stability versus context occupation

Oracle: *"Filling a window with material degrades reasoning… a big window is a higher
tolerance, not immunity. So minimising context occupation is a quality measure, not a speed
one."*

Everything in §4 pushes the other way. Reasoning was **51.7%** of the measured conversation's
content (69,537 reasoning tokens against 6,721 of assistant text), so replaying it roughly
doubles occupancy.

Both are load-bearing. Do not silently pick one. The position to argue: keep the **prompt
shape** append-only and byte-stable for the cache, but make the **content** dense — tools
return what is needed and no more, spill rather than truncate, no restated framing, and
reclaim occupancy at compaction rather than by reshaping history mid-flight.

A consequence worth stating: if context occupation is a quality measure, **compaction is the
primary quality lever**, not cost control. That raises its priority in the phasing — and it
is also the one thing nobody surveyed does warm.

---

## 11. The server is ours — fuse to it

**[operator]** *"We will be living on our llama for a while. Maybe stealing from vLLM and such
later."*
**[operator]** *"I want harness and server fused to the 11."*
**[operator]** *"Like Apple ecosystem basically, I don't care about other providers or
generalizations."*

llama.cpp here is the operator's fork (`/home/dead/Projects/llama.cpp`, branch `glm-all`) already
carrying a custom scheduler, an elastic KV pool, earned batch seats, an L2 disk spill tier, a
five-rung sequence pressure ladder, and as of this session `fadvise` on spill plus exponential
checkpoint thinning.

### 11.1 Fused, not client/server

Harness and server are **one system with one release**. The boundary is chosen for engineering
reasons, not inherited from the OpenAI HTTP shape. "Fused" does not necessarily mean one binary —
the plan must state *what the seam is* (shared memory, a private control channel, a plugin, embedding
llama as a library, or the server growing the harness's session model) and argue the choice.

### 11.2 Delete the provider-abstraction category

No pluggable provider layer, no compat matrix, no per-provider branches, no `sdkKey`-style mapping,
no adapters for Anthropic / OpenAI / Bedrock / gateway shapes. Three models, one server, one stack.
Every generalisation for a hypothetical future provider: do not build it.

This has a real cost — a cloud fallback later means rewriting the edge — and the operator has made
the call knowingly. Record the cost and move on; **do not hedge the design against it.**

**The payoff is bigger than the deletion, and it is the point.** Without OpenAI wire compatibility as
a constraint the harness need not speak `/v1/chat/completions` at all. llama.cpp's native `/completion`
accepts `prompt` as a **token array**. Therefore:

1. **The harness renders the chat template itself.** It already needs per-model dialects for GLM
   `zai`, Qwen `qwen` and the 27B dense, so it already owns that knowledge. Render locally, send
   tokens.
2. **Byte-exact control of the token sequence**, which dissolves the class of bug this week was spent
   on: no wondering whether the server reconstructs `<think>` from `reasoning_content`; no
   template-version surprises; no `--reasoning-preserve` / `clear_thinking` / `drop_thinking` probing
   (`common/jinja/caps.cpp:466-506`); no `chat_template_kwargs` guessing for Qwen's
   `preserve_thinking`. Prefix stability stops being something the harness *hopes* the server's
   renderer preserves and becomes something it constructs.
3. **`POST /apply-template` becomes the verification oracle rather than the mechanism** — run the
   server's renderer, diff against ours, and any divergence is caught in CI instead of as a 612 GB
   cache leak. Stronger than the byte-prefix assertion alone, and free: no inference, no slot.
   **This must be a required check, not a nice-to-have** — rendering locally means tracking template
   changes when a model is updated, and a silent divergence is exactly the failure mode being
   eliminated.
4. It removes the ambiguity the survey could not settle (whether GLM-5.3-Flash's template
   reconstructs reasoning from `reasoning_content`). If we render, the question does not arise.

So the plan's dialect section is now about **renderers we own**, not compat flags describing someone else's renderer.
The plan must say what a dialect implements, how it is validated against the shipped jinja, and what
happens when a model ships a template change. Anything that assumed OpenAI-shaped requests or
responses — `extraBody` passthrough, `return_progress`, reading `prompt_progress` and `timings` off
SSE — stops being an awkward extension to a foreign schema and becomes a field in a protocol we
control, which strengthens the fused-seam argument.

### 11.3 What fusion unlocks

1. **The server stops reverse-engineering structure the harness already has.** Checkpoint placement
   keys on `message_delimiters` — the server tokenizes delimiter *strings* and string-matches the
   token stream (`server-context.cpp:6990`). The harness knows where turns, tool results and segments
   begin. Fused, it sends the spans; the server stops guessing. Same for compaction boundaries and
   for what a segment is.
2. **Eviction becomes semantic instead of LRU.** The ladder picks victims by `t_last_used`. The
   harness knows this segment is a finished subagent, that one is the active thread, this
   conversation was abandoned an hour ago. Strictly better policy, unavailable across an API
   boundary.
3. **One truth for accounting.** Today opencode and llama each keep their own counts and they
   disagree — accounted cache 68.3 GiB against RssAnon 54.2 GiB; a footer showing compaction's own
   cost while the user waited for the compacted size. Fused, one number.
4. **Admission control becomes possible.** The harness wants N concurrent agents; the server knows
   what fits (436.7 MiB per sequence id plus 19.25 KiB/token, elastic pool, earned seats). Today they
   negotiate blindly and the loser is an OOM kill. This is THE SEQUENCE from `GLM-STATE.md`, finally
   with a client that can participate in it.
5. **Pre-warming.** The harness knows what it will send before it sends it — a queued prompt, a
   subagent about to spawn, a conversation just clicked. It can have a sequence restored before the
   request arrives.
6. **EXPLAIN spans both layers** — one plan covering prompt construction, cache match, admission,
   prefill and decode, instead of correlating two logs by timestamp as was done all week.

### 11.4 What fusion costs — state it as a trade

- It couples the harness to llama.cpp's internals, making *"steal from vLLM later"* harder. Say what
  the exit looks like and which fused interfaces would have to be re-abstracted.
- It cuts against the reversibility argument behind the black-box Python suite. **Keep the suite
  anyway** — it is what lets a fused system be measured from outside, and the only thing that could
  compare fused against unfused.
- Fused development has a C++ build plus a server restart in the loop, and a restart costs the warm
  cache plus a 30 s model reload. Say how the inner loop stays fast: what is still iterable in the
  daemon alone, and what forces a rebuild.

### 11.5 Where the analogy and the physics stop

llama.cpp matches on longest common prefix (`f_keep = lcp / cached_size`, `f_sim = lcp / new_size`)
and destroys a cached entry when it is fully contained in the new prompt. That is **policy**, and it
is changeable. The deeper constraint is **physical**: KV entries are position-dependent through RoPE,
so inserting or removing a segment mid-conversation invalidates everything after it whatever the
cache indexing looks like.

**A. Finer-grained cross-session sharing — real, and available.** Cross-session and cross-agent reuse
*is* what prefix caching already is; llama.cpp does it today at sequence granularity via LCP
matching. Block-hash addressing (vLLM) or a radix tree (SGLang RadixAttention) makes it finer-grained
and lets branches share rather than only linear extensions. For this workload that is the big prize:
N subagents behind one system prompt and tool set currently pay for that prefix N times as separate
resident sequences (`--kv-unified` shares the pool as a *budget*, not as cells). Quantify it against
the operator's actual pattern — many concurrent subagents, 436.7 MiB fixed per sequence id plus
19.25 KiB/token — and state the honest limit: **the 436.7 MiB recurrent state is not
token-proportional and no block scheme can share it**, so sharing helps most where the shared prefix
is deep and least in the ID-bound regime this box is short of.

Interaction with the eviction ladder (item 3 in `GLM-STATE.md` "Open, ranked", draft `1804d53`): a
ladder over whole conversation entries and a ladder over shared blocks are different policies, since
blocks are shared and need refcounting. The plan must say whether the ladder is rebuilt at block
granularity or stays at entry granularity.

**B. Composing arbitrary blocks out of order — blocked by physics, not protocol.** A block's KV is a
function of its own tokens AND every preceding token (causal attention) AND its absolute position
(RoPE). The same text at position 5,000 and 50,000 yields different KV. That is why vLLM's block hash
**chains the parent**: `block_hash = H(parent_block_hash, token_ids)`. Identity includes the whole
prefix, so a block is reusable only at the same position behind the same prefix. **"Send blocks A, F,
Q plus new text" cannot work as stated.**

State plainly what a handle protocol *would* buy, so nobody over-invests: **not** prefill compute
(the cache hit saves that whether the client sends text or a handle); bandwidth and tokenization
(measure before assuming — on a warm turn there is no prefill left for it to be negligible against);
and, importantly, **an explicit, verifiable cache hit instead of a discovered one.** Today the
harness ships 200k tokens and hopes the server finds the prefix, and a full day went into discovering
it had not. A handle turns that into a checked assertion — the plan's `EXPLAIN` idea moved into the
wire format. **That is the argument for building it, and it should be made on those grounds rather
than on speed.**

**C. Routes past the position problem — options to measure, not assume.**
- **Position shifting** — llama.cpp has the primitive (the `--cache-reuse` path). Hazard is measured
  and model-specific: it corrupted recurrent state on GLM, giving wrong answers fast, fixed by
  content-based invalidation (T3.5b). GLM is MLA + DSA with recurrent components.
- **Stitching** (CacheBlend / PromptCache lineage) — recompute a fraction of tokens to reconcile
  segments assembled out of order, trading a partial prefill for a full one. **The recompute fraction
  is the entire economics and it is unknown for MLA + DSA.** Make measuring it an explicit experiment
  with a stated method, not a design assumption — and note the open concern that the premise may
  assume attention-only layers, which GLM's 34 recurrent KDA blocks are not.
- **Accepting the cost** — a swap costs a re-prefill from the swap point: cheap at the tail, total at
  the head.

This is the honest path to *"infinite memory via explicit search and attention segments that are
swappable"* (§13), so cross-reference it there.

### 11.6 Track discipline

Server work has a much longer feedback loop than harness work. Prefer harness-side solutions for v1
where they are honest, and give server changes their own milestones — but *within one repo and one
release*, not as a separate project. The fork is an asset, not just a dependency: the sequence
registry, the pressure ladder and the L2 tier already solve problems a harness might otherwise solve
badly in userspace.

## 12. Ported conformance suite

**[operator]** *"We can steal tests from all this nice harnesses."*

A test encodes the bug and the invariant, not the design, so it ports across language and
architecture in a way implementation code does not. Four of the five sources are permissively
licensed. **This is milestone zero: the suite exists before the harness does**, and it is what
tells us any milestone actually beat opencode rather than feeling like it did.

| source | licence | what to take |
|---|---|---|
| grok-build | Apache-2.0 | `assert_prefix_stable_pair` and the suite at `actor/tests.rs:4569-5241` (turns, memory injection, images, restore). ~32.6k test fns overall |
| omp | MIT | `ai/test/issue-3528-repro.test.ts` (the reasoning-replay / KV-divergence contract), `agent/test/append-only-context.test.ts` |
| pi | MIT | 538 test files; the `length` stop-reason behaviour and the provider `compat` matrix |
| deepseek-harness | MIT | in-history system prompt, cache-hit accounting split, `content: "" never null` |
| opencode | `test/session/`, 421 tests | loop, tool-call and message-shape tests |
| flowy | operator's own; licence to confirm | the delivery tests — they encode paid-for bugs (see below) |
| **oh-my-openagent** | **Sustainable Use** | **excluded — do not copy anything, tests included** |

flowy's delivery tests specifically: a `--to-me` waiter must still hear a person's unaddressed
message; a pasted email address wakes nobody; a person naming *another* agent must not wake you;
`--mentions` beats `--to-me` when both are set; a note reaches the row's assignee and nobody else,
and on an unowned row wakes its raiser — with the guard that **two empty strings must not compare
equal** or the fallback becomes a broadcast; presence windows derived from the waiter's own numbers;
absent thread-standing serialises as absent, not false. Also port the *inversion* in
`inboxauthor_test.go`: assert the wire format carries everything the persisted model carries minus a
**named** exclusion list, computed by reflecting the type — one test, and it is the antidote to the
enumerating-renderer bug class.

State the attribution mechanics per source, and whether MIT and Apache-2.0 test code may sit
in one suite and under what notice.

Identify which tests can run **without** our harness — as black-box conformance tests against
a real llama.cpp plus any client. Those measure today's opencode, tomorrow's harness and any
third-party candidate on one axis: `f_keep`, `n_prompt_tokens_cache`, p99 re-prefill,
`finish_reason` handling, prefix-extension byte equality.

What no existing suite covers, so we write it: warm-prefix compaction, reading
`prompt_progress`/`timings` off the stream, adjudication over flowy, multi-head attach.

---

## 13. Future directions, and what they demand of v1

**[operator]** Not v1 features, but expensive to retrofit, so they constrain the v1 data
model:

1. **Multi-head compaction with different decay rates** — several heads over one
   conversation, each decaying at its own rate, so an active thread stays fine-grained while
   an abandoned one coarsens fast. Implies compaction is **not** a whole-history operation:
   context must be segmented and independently compactable, each segment carrying its own
   fidelity level and last-touched time. Same shape as the prompt-cache eviction ladder
   drafted for llama.cpp (item 3 in `GLM-STATE.md`, "Open, ranked") — say whether they share
   a policy or stay independent.
2. **User-assisted compaction** — the harness asks first: "these are the topics, which do you
   most want preserved?" Implies compaction is interruptible and can ask a human mid-operation
   over the same channel as adjudication, and that segments carry human-legible labels
   assigned at creation rather than reconstructed later.
3. **Agentic memory** — **[operator]** *"infinite memory via explicit search and attention
   segments that are swappable."* Implies context is not a flat message list but a set of
   addressable, independently serialisable segments that can be searched, evicted and swapped
   back in. Note this is what the llama.cpp layer already does one level down with sequences.
   **[operator]** It is acceptable that this fights append-only prompts — see §11.

All three want the same thing from the data model: **context as addressable segments, not a
flat message list.** One decision unlocks all three; getting it wrong forecloses all three.

The operator's existing `oracle` MCP tools (`ask_corpus`, `ask_code`, `search_corpus`) are
the existing "explicit search" surface. Say whether agentic memory builds on that pattern —
memory as a searchable corpus behind a tool — or is a first-class harness subsystem.

---

## 14. What the plan must contain

1. Architecture — processes, boundaries, what is authoritative, justified against multi-head
   and flowy rather than asserted.
2. Data model — sessions, messages, parts, events, segments. Persisted versus derived.
3. The prompt pipeline end to end, with append-only made structural rather than aspirational.
4. Per-model dialects for the three targets.
5. Adjudication and chat, plus "What flowy must provide".
6. Multi-head attach/detach.
7. Composition with firecode, and any firecode changes needed.
8. What to vendor or port and from where, with licences respected.
9. Language/runtime choice, argued against these requirements specifically.
10. Phasing — individually testable, individually useful milestones. What is the smallest
    thing that beats today's opencode setup?
11. Test strategy, with the ported suite as milestone zero.
12. Risks and open questions, including everything that could not be settled from sources.

---

## 15. Corrections made during this session

Recorded because the reasoning is more useful than the conclusions, and because two of these
were mine rather than the operator's:

- I first briefed "build a first-class async permission channel over flowy", then reversed to
  "a permission prompt is a failure of isolation design". **Both were wrong.** The operator's
  position is §6: flexible, pluggable, three adjudicators, all acceptable.
- I invented an asks-per-hour budget. **Retracted** — it is an observable, not a constraint.
- I told the planning agent append-only was non-negotiable. **Softened** — it is what today's
  server demands, and the server is ours (§11).
- I relayed that omp's compaction was warm, based on a code comment. **Wrong** — the comment
  belongs to the handoff path; the default summarizer is cold (`compaction.ts:988-990`).
- I misread `f_sim` as a match-quality score. It is `lcp / new_prompt_size`, a length ratio,
  and cannot reach 1.000 whenever the prompt grew. Two messages were built on that error
  before I checked the definition.
- I briefed the fork survey to lead with lineage, which produced a bibliography instead of an
  understanding, and had to re-task it.
- I framed server work as a separate track behind an HTTP boundary. **Superseded** by §11: fused to
  the 11, one system, one release, and the seam argued rather than inherited.
- I let flowy chat be diluted into a transport for adjudication. **Corrected** in §7: chat is a
  first-class channel of the loop and adjudication is a usage of it, not the other way round.
