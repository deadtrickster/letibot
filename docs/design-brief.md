# Design brief - local-first agent harness

The theses a plan for this harness is written against, the decisions they force, and what a reader must
hold. **[operator]** marks the operator's own position; the rest is inference, and the arguments live in
the companion documents this brief points at.

## 1. The thesis

**[operator]** *"My goal is to have my LLM runtime behave like a normal data app. Don't see any reasons
it can't."*

Every failure measured has a name in data systems:

| what broke | the data-systems equivalent |
|---|---|
| cache entries never superseded | an index that never merges duplicates |
| a byte cap but no entry cap | a buffer pool with no page limit |
| eviction spills a whole entry or nothing | no graded replacement - no LRU-K, CLOCK, ARC |
| one long stop-the-world compaction | pre-LSM compaction |
| `finish_reason: length` parsed then discarded | swallowing an error return |
| OOM under a `MAP_POPULATE` burst | no admission control, no backpressure |
| accounted cache and resident bytes disagree | wrong statistics, so the planner decides wrongly |
| three misdiagnoses in one session | no EXPLAIN, no per-stage metrics |

Two are more than analogy. **Multi-head compaction with different decay rates is leveled compaction**,
which removes the stall rather than shortening it, at the price of fidelity lost to re-summarisation.
**Per-stage observability is EXPLAIN**: "why did this prefill 144,436 tokens with zero cache hits?" is a
query-plan question. Where the analogy stops: attention is position-dependent through RoPE, so KV
"pages" are not freely relocatable the way database pages are.

## 2. What was measured, and why it matters

On a harness built on OpenAI-shaped requests against GLM-5.3-Flash on llama.cpp: **reasoning was not
replayed**, so every turn diverged from its own cache entry; **compaction cold-prefilled by
construction**, at 144,436 tokens and cache hit 0, about 13 minutes to the first summary token; **the
output cap was silent**, so an empty result was recorded as success. Replaying reasoning moved `f_keep`
p10 from **0.000 to 0.999**, and nobody surveyed has warm compaction on the default path.

## 3. Model targets

Three, all local llama.cpp, all first class:

| model | notes |
|---|---|
| GLM-5.3-Flash | production today; thinking format `zai`; ctx 262,144; reasoning replay is what makes its prefix cache hit |
| Qwen Flash (`qwen-3.8-flash-next`) | thinking format `qwen`; needs `preserve_thinking` / `chat_template_kwargs` |
| Qwen 3.8 27B dense | |

**Per-model dialects are architecture, not configuration**: the plan must say how a fourth model is
added and what must be verified when it is.

## 4. Non-negotiable properties

- **The prompt is append-only** - a later request is a byte-exact prefix extension of the earlier one -
  and the test asserting it should exist before the harness does.
- **Reasoning is replayed verbatim** into the field the server used, capability-detected and never
  hostname-detected: a detection that disables itself behind a proxy is the cache growth above.
- **Compaction reuses the warm prefix**: real system, real tools, real messages, instruction last.
- **System prompt changes are appended, not rewritten** - a model reads the latest `system` at any
  position; one line added to an `<env>` block cost a full cold re-prefill of a 179k conversation.
- **No hardcoded output cap**; `finish_reason: length` acted on and surfaced.
- **Render cost independent of accumulated output length.**
- **Arbitrary request body fields out**, **non-standard streamed response fields in**
  (`prompt_progress`, `timings`) - the latter is what a real progress display is built on.

## 5. Execution substrate - firecode

**[operator]** firecode *"was the attempt to calm down `--dangerously-skip-permissions`. The point is to
isolate the agent and spare me hitting enter."*

A **run** is one microVM and everything created for it, and its lifetime is a cgroup subtree, not a
process tree, "because a process tree cannot express what a run owns". Root is overlayfs layers, so
nothing is copied per run, and the guest is untrusted by construction. **The harness must not reinvent
any of this**, and it must say how it composes: session to run? subagent to child run? who owns
lifetime? firecode is the operator's own repo and changes to it are allowed, so specify the change
rather than routing around it.

## 6. Adjudication - pluggable, not doctrinal

**[operator]** *"We are flexible. It is ok to ask me, it is ok to use firecode, and it is ok if I plug a
model for auto mode."*

Three adjudicators behind **one interface**:

1. **The boundary** - isolation. No decision because the action is structurally safe inside the run; the
   default for almost every call, and why allow-all in a sandbox is a sane posture.
2. **The human** - over flowy or `--ask`, for genuinely ambiguous calls.
3. **A model** - "auto mode". Say what it sees, what it returns, whether it can **escalate** rather than
   only allow/deny, what happens when it is unavailable, and how its decisions are reviewed - a drifting
   auto-adjudicator is the "did less than it claimed and said nothing" failure.

Routing is **configuration, not code**, escalation is first-class both ways, and everything is
auditable. Keep `--ask`'s semantics, because asks should be uncommon: post and wait; if nobody answers,
say so and tell the agent to decide and record what it chose; acknowledge before acting, since silence
is indistinguishable from absence. Asks-per-hour is an **observable, not a budget**.

## 7. flowy - chat and permissions

**[operator]** flowy is a **server/client** relationship, not a terminal tool: permissions answerable
from a **private chat with the agent**, and **first-class chatting** - converse with a running agent,
not merely dispatch tasks to it. **[operator]** *"I don't want to fiddle with monitors, nor timers which
pollute the context."*

**Chat is a first-class channel of the agent loop - not a transport for adjudication, and not a tool.**
Today the only way to hear a message is a background poll that blocks, prints one line, exits and is
re-armed, so every arrival costs a tool call plus a result plus the re-arm.

1. **Pushed, not polled** - the daemon holds the connection and a message arrives as an event.
2. **A chat message costs exactly what a user message costs - it IS a user message.**
3. **Messages may arrive mid-turn**: say what happens to a turn in flight, and argue the choice.
4. **Chat is symmetric with adjudication** - an `--ask` is a message that blocks on a reply.
5. **It is a head** (§8), so the multi-head design and the chat design are one design.

### What flowy must provide

- **Three wake levels, not four**: all / addressed / mentions-only, two orthogonal scopes and a per-room
  mute; a human's *unaddressed* broadcast wakes an addressed reader. **Per-room policy does not exist
  and cannot** - the level is sent per poll and stored nowhere - so the harness should provide it on a
  per-room subscription row, flowy's *definitions* kept and its one-flag shape dropped.
- **Chat is not on a pushed transport** - SSE covers todos and queues only - so pushed chat needs a
  flowy change. File it verbatim:

  > `GET /api/inbox/stream?as=NAME&addressed=&mentions=&focus=&room=` - same wake predicate, same
  > reader row, same enrichments, same heartbeat, one JSON message per SSE event,
  > `POST /api/inbox/ack` unchanged.

  Two smaller asks: a fourth `waiter_kind` for a streaming listener, and `LISTEN`/`NOTIFY` to remove the
  internal 250 ms tick.
- **No blocking ask.** The nearest thing is a board-visible, self-clearing waiting row: any note or write
  by the named person is their answer. Worth considering for §6.

**Do not copy:** exit-per-message delivery, the forked-successor handover, per-poll filter policy,
preferences in an untyped note body. Open question: flowy and the firecode room are two chat systems -
unify, bridge, or one as the other's transport?

## 8. Multi-head attachment

**[operator]** One agent session, **multiple clients attached and detached at will** - like tmux/byobu:
a local TUI, a flowy chat and a remote head at once. The plan must address authoritative state, how a
late head catches up, how output fans out, two heads acting at once, and every head detaching mid-turn
- the agent must keep working.

Adopt from flowy rather than invent: a cursor per named reader acked **after** the write, so a crash
costs a duplicate and never a silence; **the mark advances over everything read, not everything
delivered**; report what was filtered out; one waiter per name, shared by all heads; bounded scrollback
that discloses the drop; non-blocking fan-out that drops and tells the slow head; and **discard a queued
reply rather than hold it** - *"a reply to a question asked ten minutes ago is not an answer, it is input
arriving from nowhere."*

## 9. The tool contract

**[operator]** *"The harness must close the loop; never paper over it with prompt."* A `find` tool must
return what is actually needed, so the model needs no workarounds; malformed calls are salvaged rather
than dropped; a miss returns the surrounding context and what *would* have worked instead of dead-ending
on "not found".

**Every tool's failure mode is a design decision, paid for in permanent context, not in one wasted
call**, and a miss should be self-correcting within the same call.

- **Abstention must be structural.** "No answer" must stay distinguishable from "here is the answer" as
  it propagates up - *"a hallucination wearing a footnote is more dangerous than a naked one."*
- **Per-stage observability, or you will misdiagnose.**
- **Tool count is a measured ceiling for small models**: past roughly 5-7 servers they choose worse.
- **Discipline is structural, not prompted**: a step that must happen is a node the model cannot route
  around.

## 10. The tension: cache stability versus context occupation

*"A big window is a higher tolerance, not immunity. So minimising context occupation is a quality
measure, not a speed one."* §4 pushes the other way: reasoning was **51.7%** of one measured
conversation's content, so replaying it roughly doubles occupancy. Keep the **shape** append-only and
byte-stable and make the **content** dense - return what is needed, spill rather than truncate, no
restated framing - and reclaim occupancy at compaction rather than by reshaping history mid-flight. If
occupation is a quality measure, **compaction is the primary quality lever**, not cost control.

## 11. The server is ours - fuse to it

**[operator]** *"I want harness and server fused to the 11."* *"Like Apple ecosystem basically, I don't
care about other providers or generalizations."* The fork already carries a custom scheduler, an elastic
KV pool, earned batch seats, an L2 disk spill tier, a five-rung pressure ladder and `fadvise` on spill.

### 11.1 Fused, not client/server

Harness and server are **one system with one release**, and the boundary is chosen for engineering
reasons rather than inherited from the OpenAI HTTP shape. "Fused" need not mean one binary: the plan
must state *what the seam is* - shared memory, a private control channel, a plugin, llama as a library,
or the server growing the harness's session model - and argue it.

### 11.2 Delete the provider-abstraction category

No pluggable provider layer, no compat matrix, no per-provider branches: three models, one server, one
stack, and no generalisation for a hypothetical future provider. A cloud fallback later then means
rewriting the edge; record the cost and do not hedge against it.

**The payoff is bigger than the deletion.** Without OpenAI wire compatibility the harness need not speak
`/v1/chat/completions` at all, and llama.cpp's native `/completion` takes `prompt` as a **token array**:
the harness renders the chat template itself, which makes prefix stability something it constructs
rather than hopes for, and `POST /apply-template` becomes the verification oracle - run the server's
renderer, diff against ours, and a divergence fails CI instead of leaking cache. That check is
**required**, because rendering locally means tracking template changes.

So the dialect section is about **renderers we own**: what a dialect implements, how it is validated
against the shipped jinja, and what happens when a model ships a template change.

### 11.3 What fusion unlocks

1. **The server stops reverse-engineering structure the harness has** - checkpoint placement keys on
   delimiter *strings* it tokenizes and string-matches, where the harness knows where turns and tool
   results begin, so it sends the spans.
2. **Eviction becomes semantic instead of LRU** - the harness knows which segment is a finished subagent
   and which is the active thread.
3. **One truth for accounting**, instead of two sets of counts that disagree.
4. **Admission control becomes possible**: the harness wants N concurrent agents, the server knows what
   fits (436.7 MiB per sequence id plus 19.25 KiB/token), and today they negotiate blindly and the loser
   is an OOM kill.
5. **Pre-warming**, and **EXPLAIN spanning both layers** instead of correlating two logs by timestamp.

### 11.4 What fusion costs - state it as a trade

- It couples the harness to llama.cpp's internals, making *"steal from vLLM later"* harder: say what the
  exit looks like and which interfaces would have to be re-abstracted.
- It cuts against the reversibility behind the black-box conformance suite. **Keep the suite** - it is
  what lets a fused system be measured from outside.
- Fused development has a C++ build plus a server restart in the loop, and a restart costs the warm
  cache. Say how the inner loop stays fast.

### 11.5 Where the analogy and the physics stop

llama.cpp matches on longest common prefix and destroys a cached entry contained in the new prompt. That
is **policy**, and changeable. The deeper constraint is **physical**: KV entries are position-dependent
through RoPE, so inserting or removing a segment mid-conversation invalidates everything after it
whatever the indexing looks like.

**Finer-grained cross-session sharing is real and available** - block-hash addressing or a radix tree
lets branches share rather than only extend, which matters because N subagents behind one system prompt
currently pay for that prefix N times as separate resident sequences. The honest limit: **the 436.7 MiB
recurrent state is not token-proportional and no block scheme can share it.**

**Composing arbitrary blocks out of order is blocked by physics**: a block's KV depends on its own
tokens, every preceding token and its absolute position, which is why a block hash **chains the parent**,
and "send blocks A, F, Q plus new text" cannot work as stated. What a handle protocol *would* buy, so
nobody over-invests: not prefill compute, not bandwidth, but **an explicit, verifiable cache hit instead
of a discovered one** - the EXPLAIN idea moved into the wire format, which is the argument for building
it.

**Routes past the position problem, to measure and not assume.** *Position shifting*: llama.cpp has the
primitive, and the hazard is measured and model-specific - it corrupted recurrent state on GLM, fixed by
content-based invalidation. *Stitching*: recompute a fraction of tokens, where **the recompute fraction
is the entire economics and it is unknown for MLA + DSA** - an experiment with a stated method, noting
that the premise may assume attention-only layers, which GLM's recurrent blocks are not. *Accepting the
cost*: a swap costs a re-prefill from the swap point - cheap at the tail, total at the head.

### 11.6 Track discipline

Server work has a much longer feedback loop than harness work. Prefer harness-side solutions for v1
where they are honest, and give server changes their own milestones - but *within one repo and one
release*. The fork is an asset, not just a dependency.

## 12. Ported conformance suite

A test encodes the bug and the invariant, not the design, so it ports across language and architecture.
**This is milestone zero: the suite exists before the harness does**, and it is what tells us a
milestone actually beat the current setup rather than feeling like it did.

| source | licence | what to take |
|---|---|---|
| grok-build | Apache-2.0 | the prefix-stability pair assertion; its suite over turns, memory injection, images, restore |
| omp | MIT | the reasoning-replay / KV-divergence repro, the append-only-context test |
| pi | MIT | the `length` stop-reason behaviour, the provider `compat` matrix |
| deepseek-harness | MIT | in-history system prompt, cache-hit accounting split, `content: "" never null` |
| opencode | MIT | loop, tool-call and message-shape tests |
| flowy | operator's own | the delivery tests - they encode paid-for bugs |
| **oh-my-openagent** | **Sustainable Use** | **excluded - do not copy anything, tests included** |

flowy's delivery tests are the ones worth having - a `--to-me` waiter still hears a person's unaddressed
message, a pasted address wakes nobody, `--mentions` beats `--to-me`, and a note reaches its assignee and
nobody else, with the guard that **two empty strings must not compare equal**. Also port the *inversion*:
assert the wire format carries everything the persisted model carries minus a **named** exclusion list
computed by reflecting the type.

Identify which tests run **without** our harness, as black-box conformance against a real llama.cpp plus
any client, measuring `f_keep`, `n_prompt_tokens_cache`, p99 re-prefill, `finish_reason` handling and
prefix-extension byte equality. What no existing suite covers, so we write it: warm-prefix compaction,
reading `prompt_progress`/`timings` off the stream, adjudication over flowy, multi-head attach.

## 13. Future directions, and what they demand of v1

**[operator]** Not v1 features, but expensive to retrofit, so they constrain the v1 data model:

1. **Multi-head compaction with different decay rates** - several heads over one conversation, each
   decaying at its own rate. Implies compaction is **not** a whole-history operation: context must be
   segmented and independently compactable, each segment carrying its own fidelity level and last-touched
   time.
2. **User-assisted compaction** - the harness asks first: "these are the topics, which do you most want
   preserved?" Implies compaction is interruptible, can ask a human over the same channel as
   adjudication, and that segments carry human-legible labels assigned at creation.
3. **Agentic memory** - **[operator]** *"infinite memory via explicit search and attention segments that
   are swappable."* Implies context is a set of addressable, independently serialisable segments that can
   be searched, evicted and swapped back in, which is acceptable even though it fights append-only
   prompts (§11).

All three want the same thing: **context as addressable segments, not a flat message list.** One decision
unlocks all three. The operator's existing `oracle` tools (`ask_corpus`, `ask_code`, `search_corpus`) are
the existing "explicit search" surface - say whether agentic memory builds on that pattern or is a
first-class harness subsystem.

## 14. What the plan must contain

1. Architecture - processes, boundaries, what is authoritative, justified against multi-head and flowy
   rather than asserted.
2. Data model - sessions, messages, parts, events, segments. Persisted versus derived.
3. The prompt pipeline end to end, with append-only made structural rather than aspirational.
4. Per-model dialects for the three targets.
5. Adjudication and chat, plus "What flowy must provide".
6. Multi-head attach/detach.
7. Composition with firecode, and any firecode changes needed.
8. What to vendor or port and from where, with licences respected.
9. Language/runtime choice, argued against these requirements specifically.
10. Phasing - individually testable, individually useful milestones. What is the smallest thing that
    beats today's setup?
11. Test strategy, with the ported suite as milestone zero.
12. Risks and open questions, including everything that could not be settled from sources.
