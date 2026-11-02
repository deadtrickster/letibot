# Implementation plan - a local-first agent harness for lab2x1

The design contract for letibot: a long-lived session daemon (`harnessd`) running agent loops against
the operator's llama.cpp fork, with attachable heads, a prompt the harness renders and tokenizes
itself, adjudication as a first-class subsystem, and a ported conformance suite as milestone zero.
Later decisions are in `DECISIONS.md`, cited by D-number. The harness half is built (`crates/`); the
fused server track (§3.3, §17 S0-S7) is not, and the harness runs on the `/completion` token-array
fallback (§5.6).

Two operator directives govern the document: *"I want harness and server fused to the 11"* and *"like
Apple ecosystem basically, I don't care about other providers or generalizations."* Read four things:
§3.1 (the token array is the invariant), §4.3 (the ledger), §7.2 (the CI diff), §17's last subsection.

## 0. The thesis

> *"my goal is to have my llm runtime behave like a normal data app, don't see any reasons it
> can't."* - the operator

Every failure that motivated the design is something data systems settled decades ago, and each is
answered by a named section rather than by care: an index that never merges duplicates (§3.10-A), a
buffer pool with a page limit, leveled rather than stop-the-world compaction (§10), an error return
that is not swallowed (§5.7, §8.2), admission control and `EXPLAIN` (§3.4.4, §6).

The vocabulary, used precisely because the operator is a Postgres person: the KV cache is a **buffer
pool** (`f_keep`/`f_sim` are a hit-ratio pair, `mlock` is pinning), the sequence registry is a **page
table**, sessions are **tenants** ("seats are EARNED" is admission control), the prompt prefix is a
**radix key**, and durability is a **storage hierarchy** (VRAM, RAM cache, NVMe L2, cold re-prefill).

### 0.1 Where the analogy stops, and it matters

A buffer-pool page is relocatable; **a KV cell is not**, because its contents are a function of its
position through RoPE, so a mid-sequence insert or delete invalidates the data of every cell after it
and any design that reshapes history pays a `REFRESH` from the divergence to the end.

## 1. What this is, and what it is not

A headless session daemon running agent loops against the local llama.cpp router, with attachable
heads (TUI, flowy, ACP) and permissions answered asynchronously by a human who may be elsewhere.

It is **not** a RAG system - retrieval stays behind the `oracle` MCP tools (`ask_corpus`, `ask_code`,
`search_corpus`), with no embedding or reranking (§9.4, §19.3) - and **not** a multi-provider client:
the provider-abstraction *category* is deleted, because every abstraction that exists only for a
provider we do not run is a liability and is why the surveyed harnesses default to metered APIs. A
fourth model arrives by someone writing a renderer (§7.4). D10 reserved the seam a metered API needs
in `crates/backend`: `TurnRequest` carries the transcript rather than a rendered prompt, `BackendCaps`
states what a backend cannot guarantee, and `PrefixGuarantee` marks the one invariant (§4.3) that does
not survive a `messages` API - and a backend must declare what it cannot guarantee, with the invariant
suites skipping loudly rather than passing vacuously.

Both sides are ours and they are fused: the llama.cpp here is the operator's fork (`glm-all`), already
carrying a custom scheduler, an elastic KV pool, earned seats, an L2 disk tier and a five-rung
pressure ladder, so the boundary is chosen for engineering reasons rather than inherited from HTTP.

## 2. The facts this design is built on

**F1 - the prompt is append-only, or the cache is gone.** llama.cpp caches prompt *and generated*
tokens, so request N+1 must extend request N including turn N's generation; measured here, replay off
gave `f_keep` p10 0.000 and p99 re-prefill 78,598 tokens, replay on gave 0.999 and 5,294.

**F1a - append-only is also the cache's garbage collector.** The cache drops any cached prompt fully
contained in the current one, so under append-only a conversation occupies one entry for its whole
life, and break it and nothing is ever contained.

**F2 - depth is paid in memory and prefill, never in decode.** Median decode was 35.7 t/s below 10k of
context and 36.9 t/s above 120k, so a deep *warm* conversation is cheaper per turn than a shallow
*cold* one, which inverts the instinct to compact early.

**F3 - context occupation defocuses.** Filling the window degrades reasoning in the 480B model and the
local 30B alike; a big window is higher tolerance, not immunity. F2 and F3 oppose each other and §9
resolves them.

**F4 - close the loop; never paper over it with prompt.** *Prompts are soft limits, hooks are hard
stops*: a prompt rule is enforced by the component whose failure it is meant to catch.

**F5 - the failure shape to hunt is "the system did less than it claimed and said nothing"**, which is
an empty subagent recorded as success and a hallucination wearing a footnote; every layer boundary
must make *"I did not do this"* structurally different from *"here is the result."*

**F6 - the server is a preemptive scheduler and this harness is its only client**, so every seat
measurement here is synthetic until a client asks for concurrency (§17-M6).

**F7 - statistics decide policy, so statistics must be right**; a wrong number is worse than a missing
one because it produces a confident wrong plan, and §6 and §18 exist because of this.

## 3. Architecture

### 3.1 What fusion actually changes, in one paragraph

Without OpenAI wire compatibility as a constraint the harness need not speak `/v1/chat/completions` at
all: llama.cpp's completion path accepts `prompt` as a token array, and the harness already owns
per-model template knowledge for its dialects. So **the harness renders the chat template itself,
tokenizes it itself, and sends token ids** - and because the transcript is append-only (§4.1) the token
array is append-only too, one growing `Vec<llama_token>` per transcript with nothing ever re-rendered
or re-tokenized. That collapses three problems: the prefix invariant becomes something the harness
*constructs* rather than hopes a server renderer preserves, so a violation is not expressible; "does
this template reconstruct `<think>` from `reasoning_content`?" does not arise; and
`chat_template_kwargs` guessing, `preserve_thinking`, `clear_thinking`/`drop_thinking` probing and
`extraBody` passthrough go away.

### 3.2 Shape

Two processes, one system: `harnessd` holds the session store (SQLite/WAL), the transcript and token
ledger, the renderers (§7), the tokenizer over `libllama`, the turn engine, `EXPLAIN`, the tool runner
into a boundary, adjudication and flowy; `llama-server` holds the scheduler, the elastic pool, earned
seats, the sequence registry, the prompt cache with its L2 tier and the pressure ladder. They speak a
private control channel plus shared memory for tokens; heads (TUI, remote, ACP) attach to the daemon.
One release, one version number - not two systems with an API between them, and not one binary.

### 3.3 The seam, argued

Four candidates and the choice is the third. One binary with `libllama` embedded is rejected on the
inner loop, because every harness change would force a C++ link and a 29-31 s model reload plus a cold
cache and would put every session and head inside the process that gets OOM-killed; a harness plugin
inside the server has the same problem plus a SQLite writer, a fan-out and a flowy connector inside the
address space that must not stall; and keeping HTTP with added fields is strictly worse once the
provider abstraction is gone. **(c) Two processes, a private control channel, shared memory for tokens,
one release** is chosen because the boundary sits where the failure domains differ: a 199.7 GB
inference process with a 30 s restart cost, and a long-lived daemon that must survive that restart
with its heads attached.

Concretely: a length-prefixed versioned binary protocol over a Unix socket where both sides refuse a
peer whose version is not exactly theirs (they ship together, so a mismatch is a deployment bug); one
`memfd` region per transcript, appended by the harness and mapped read-only by the server, so a request
is `{region_id, start, len, prefix_handle}` - tens of bytes for a 200k-token prompt; and this control
vocabulary, none of which exists in an OpenAI schema:

```
Admit{n_sequences, est_tokens} → Fits{ceiling, cost_per_id, pool_free} | Refused{why}
Prewarm{region_id, upto}
Submit{region_id, start, len, sampling, message_spans[], segments[], prefix_handle,
       want:{progress,timings,tokens}}
Spans{region_id, spans[]}   SegmentHint{segment_id, state, session_id}
Handle{id, n_tokens, hash}  Explain{turn_id}  Stats{}
```

What stays HTTP is `/props`, `/apply-template`, `/metrics`, `/slots` and `/health` - diagnostic and CI
surfaces that cost nothing and keep the black-box conformance suite working from outside (§14.3).

### 3.4 What fusion unlocks

Six things, each unavailable across an arm's-length boundary; all six are server-track work (§17) and
none has landed.

- **3.4.1** Checkpoint placement stops being a heuristic: the server recovers message spans by
  tokenizing delimiter strings and string-matching them, while the harness emitted those tokens.
- **3.4.2** Eviction becomes semantic rather than LRU: only the harness knows *this* segment is a
  finished subagent, which should demote first however recently it ran.
- **3.4.3** One truth for accounting: `prompt_tokens` is `len` rather than an estimate, and `Stats`
  returns the server's memory accounting beside the harness's.
- **3.4.4** Admission control becomes possible: `Admit` returns a real number and the harness decides
  how many subagents to spawn from it, making §8.4's concurrency negotiated rather than configured.
- **3.4.5** `Prewarm` restores a sequence before the request arrives, turning an NVMe read or an idle
  model reload into background work.
- **3.4.6** `Explain{turn_id}` returns the matched entry, its `lcp`, `f_keep`, `f_sim`, the ladder
  rung, whether admission deferred and how many batch iterations the prefill shared, so two logs
  correlated by timestamp become one plan.

### 3.5 What fusion costs

It couples the harness to llama.cpp's internals and makes leaving harder - a reasonable trade given
what the fork already carries. The exit is three named modules: the tokenizer (today `libllama` FFI,
would become a vocab loader), the submit path (token array over shm, would become a token array over
HTTP, which vLLM and SGLang both accept), and the fused control vocabulary, which has no equivalent
anywhere - losing the third means going back to being a normal client rather than porting code. It
also cuts against the reversibility argument behind the Python black-box suite, which is exactly why
that suite stays: it is the only thing that can measure a fused system from outside and compare it
against an unfused baseline.

### 3.6 How the inner loop stays fast

The split is deliberate. Iterable in the daemon alone, no rebuild, seconds: renderers, the transcript
and ledger, compaction and segment policy, the tool contract, adjudication and its policy table, flowy,
heads, `EXPLAIN` formatting, the conformance suite. Forcing a C++ build and a restart: the protocol
version, the shm submit path, and §3.4's six unlocks - six discrete changes, each landed once and
measured once. Three practices keep even those cheap: protocol changes are additive and versioned in
lockstep with a handshake that rejects a mismatch loudly; the harness can always submit over HTTP with
a token array, so a server rebuild is never a blocker (M1 runs on it, and so does this tree today); and
builds go in a firecode run, niced, because with `--no-op-offload` the CPU is on the inference path.

### 3.7 Chat is a channel of the loop, and the daemon is what makes that possible

> **[operator]** *"I don't want to fiddle with monitors, nor timers which pollute the context."*

The mechanism objected to is a background poll loop: a blocking inbox command prints one line, exits
and is re-armed, so every arrival lands in the conversation as a tool invocation plus a tool result
plus the re-arm. The daemon is long-lived and owns the session, so it holds the subscription instead,
and four rules follow:

1. **Inbound messages are pushed, never polled** - no listener process, no re-arm, no interval, no
   Monitor-shaped scaffolding; if one appears, it is a bug.
2. **A chat message costs exactly what a user message costs, because it is one**: a
   `TranscriptItem::User` with the sender's identity in the Journal, no envelope (§18.1-I11).
3. **Chat is the primitive and adjudication is a usage of it** - an `--ask` is a message that blocks on
   a reply, so §11.6's "permissions and questions are one mechanism" is really "both are chat".
4. **A flowy chat is a head** (§13.4), so multi-head and chat are one design.

Mid-turn arrival is §5.8.

### 3.8 What is authoritative

The daemon's session log: append-only events in SQLite/WAL with a monotonic `seq`, single writer per
session, from which everything a head displays is derived. The daemon's token array is authoritative
for what the model saw, and the server's KV cache is only an accelerator - every request must be
correct and give the same answer with an empty cache, which is the one place `--cache-reuse` violates
(restoring recurrent state computed with deleted text present). No head's view is authoritative,
including the local TUI's.

### 3.9 The execution substrate is firecode

**firecode** is the operator's per-run Firecracker/qemu microVM sandbox. A **run** is one VM and
everything created for it; lifetime is a cgroup subtree rather than a process tree, so one write to
`cgroup.kill` ends the VMM, the relays and every VM started on its behalf; the root filesystem is
overlayfs layers, so nothing is copied per run; and the guest is untrusted by construction, since
project paths come from the spawn server's config, parentage is derived from the connection, and there
is no host-side seed script.

Effectful tools do not run as children of the daemon but inside a boundary - three implementations
exist in `crates/tools`, `Bwrap` (namespaces), `Seatbelt` (macOS mandatory access) and firecode (a VM),
and a seat that needs one the box cannot build is refused. Placement is **harnessd on the host, tools
in the run**: the log, ledger and audit must survive the VM and a checkpoint rollback, heads attach to
the daemon, and the model server cannot move. A session is one run (`firecode up --project <key>`),
ended with `down` so work is copied back and then `cgroup.kill`; a subagent is a sibling run nested in
the parent's cgroup and owned transitively; a bash/write/edit call is one `vm_in`; a long-lived build
keeps the run up between turns; host read-only MCP stays on the host. A run spawned by a host caller
has no parent run, so D3 answered this: cgroup membership is use-dependent, and the persistent-shell
ask was withdrawn in favour of byobu.

Subagents take firecode's shape, not opencode's: a sibling VM with cgroup lifetime and an empty MCP
config, flat by default so a child cannot reach the spawn server and fan out further, and "talking and
fanning out are different powers" - a spawned agent has no tools but keeps its voice. Checkpoints and
the KV cache are decoupled, since the cache is keyed by what the conversation says rather than by which
VM said it; because the log is on the host a rollback does not rewind the conversation, so a restore
must append a `ToolResult{outcome: NotRun}`-style marker saying the workspace was rolled back, or the
transcript becomes a lie the model acts on.

### 3.10 Block-hash addressing, handles, and what each can and cannot buy

**A. Finer-grained cross-session sharing is real.** Prefix caching already shares at sequence
granularity, and block-hash addressing or a radix tree makes that finer and lets *branches* share. The
pool is a budget rather than cells, so N sequences behind one system prompt each hold their own copy -
a 4,000-token prefix duplicated 12 ways is 827 MiB, a 20,000-token prefix 30 ways is 10.65 GiB. The
limit matters more than the prize: GLM's per-sequence fixed cost, 436.7 MiB of recurrent state across
34 KDA layers, is not token-proportional and so is shareable by no block scheme, which puts the floor
at 12.8 GiB for 30 sequences before a single token. Sharing changes the constant, not the argument for
a third GPU.

**Two ladders at two tiers, never merged.** The prompt-cache ladder degrades a whole conversation entry
(full, thinned, skeletal, spilled, dropped), least-degraded first, because its subject is the fidelity
of a restorable conversation, and a block has no checkpoints so "degrade this block" has no meaning. A
shared-block index needs the ordinary buffer-pool policy - refcount plus LRU among blocks at refcount
zero - at the resident-pool tier, because a degrade decision on a shared block would coarsen every
conversation referencing it (§10.5's coupling). Sharing also makes age-based demotion more necessary,
since a block held by a long-idle sequence pins memory for every sharer.

**B. Composing arbitrary blocks out of order is blocked by physics, not protocol.** A block's KV is a
function of its own tokens, every preceding token and its absolute position, which is why vLLM's block
hash chains the parent (`block_hash = H(parent_block_hash, token_ids)`); a block is reusable only at
the same position behind the same prefix, so **"send blocks A, F, Q plus some new text" cannot work as
stated**. What a handle buys is not prefill compute but bandwidth, tokenization, and the real prize -
an explicit, verifiable cache hit instead of a discovered one: every response carries
`prompt_handle: {id, n_tokens, hash}`, the next request sends `prefix_handle: <id>` alongside the full
prompt so correctness never depends on cache state, the server answers `hit` or `miss` with a reason
(`evicted`, `diverged at token N`, `unknown`), and `prefix_handle_required: true` makes a miss a 400
instead of a silent re-prefill, which turns C3/C4 into loud assertions. Build S1 first.

**C. Routes past the position problem, to measure rather than assume.** Position shifting exists (the
`--cache-reuse` path shifts cells) and its hazard is measured and model-specific - on GLM it restored a
checkpoint whose recurrent state was computed with deleted text present - so it must be re-earned per
architecture. Stitching (recompute a fraction to reconcile out-of-order segments) has unknown economics
for MLA + DSA, because its premise assumes attention-only layers while in a recurrent layer the state
at position n depends on all n tokens; GLM's 12 MLA and 34 KDA blocks mean it may work for a quarter of
the model and be impossible for the rest (UNVERIFIED-16). The experiment: build `P = A ++ B ++ C`, take
the reference KV from a full prefill, assemble from a cached `A`, a cached `C` from a different
position and a recompute of the first *k* tokens of `C`, vary *k*, measure exact agreement of 200 greedy
continuation tokens over 20 samples at depths 10k/50k/150k plus the next-token KL divergence at the
join, and report the smallest *k* that holds per layer class. Accepting the cost always works: a swap
re-prefills linear in the suffix, ~11 s for a tail swap and ~453 s at the head of a 200k conversation,
and v1 takes it (§19.3).

## 4. Data model

### 4.1 Two layers, and only one can change the prompt

**Layer 1, the Transcript**: the ordered `TranscriptItem`s that become the prompt, immutable with
exactly one mutation, `append` - truncation is not a mutation but a new transcript with a new id (§5.5),
because a truncation is a cache divergence and must be visible as one. **Layer 2, the Journal**:
display parts, deltas, progress, timings, errors, adjudication, head attach/detach and warnings, freely
appended and re-rendered, never consulted when building a request. The rule is enforced by types rather
than review - **the request builder takes a `&Transcript` and nothing else** - so putting a new field
into the prompt means moving it into a `TranscriptItem`, a deliberate and reviewable act.

### 4.2 TranscriptItem

```
TranscriptItem =
  | System      { text, origin: Bootstrap | Update }
  | User        { parts: [Text | Image | FileRef] }
  | Reasoning   { text, field: ReasoningField }
  | Assistant   { text, tool_calls: [ToolCall] }
  | ToolResult  { call_id, name, outcome: ToolOutcome, payload }
  | SegmentMark { segment_id, label, kind, edge: Open | Close }
```

Reasoning is a **sibling** of assistant, not a field on it, because the interleaved order of
`[reasoning, tool_call, reasoning, ..., message]` must stay byte-stable across turns - that stability is
what lets the prefix cache hit, and GLM and Qwen both interleave.
`ToolOutcome = Ok | Abstained{reason} | Failed{reason} | Denied{req_id} | Timeout | NotRun{why}` is a
closed vocabulary demanded by F5, and `Abstained` is not a flavour of `Ok` (§8.2). `SegmentMark` renders
to nothing - a zero-width delimiter carrying `{segment_id, label, kind, created_at, last_touched}` -
and exists in v1 solely so §19's futures have something to address.

### 4.3 The Token Ledger - append-only made structural

```
TokenLedger = [ (item_id, tok_offset, tok_len, h_k) ]
   h_k = H(h_{k-1} ‖ tokens(item_k)),   h_{-1} = H(tokens(stable_prefix))
   tokens : Vec<llama_token>, memfd-backed, append-only
```

**There is no rewrite operation to call**: request N+1 is `(region, 0, len_{N+1})` and request N was
`(region, 0, len_N)` over the same memory, so a prefix violation is not something a test catches, it is
not expressible. The unit is the server's unit, since the cache matches prefixes over token ids, so
"our invariant" and "what the cache does" are one object. The hasher is fed forward only, from a
resumable state at the last item, and `h_k` is the cache identity and what `prefix_handle` asserts. A
periodic full re-hash (every request in debug, every 25th in production) catches renderer
non-determinism, which is where §7.2 points. And the ledger is persisted with the transcript, so a
restart resumes the chain and the vector is rebuilt by replaying spans rather than re-rendering. This
supersedes a 32-bit rolling hash over the normalized message object: 32 bits is a collision budget not
worth spending on a 300-turn conversation, and hashing the message object cannot see divergence
introduced downstream in the encoder.

### 4.4 What is persisted vs derived

**Persisted** (SQLite/WAL): `session` (id, title, model, dialect id = the dialect's `template_sha`,
workspace root, owner, approvers); `transcript` (id, session, `parent_transcript_id`,
`stable_prefix_id`); `transcript_item` (ordered, immutable, with its ledger row); `stable_prefix`
(frozen system text plus normalized tool schemas, content-addressed and shared across sessions with the
same one); `event` (`(session_id, seq)`); `adjudication_request` / `adjudication_decision` (never
deleted); `turn_metrics` (prompt/cached/predicted tokens, `finish_reason`, timings, draft counts, model,
dialect, ledger head sent, §18's computed expectations); `tool_spill` (by content hash); `segment`.
**Derived**: the head snapshot (§13.2), rendered markdown, every aggregate in §6/§18, the request body.
**Not stored at all**: the raw response envelope, because keeping it invites someone to replay it and
diverge.

Three rules from the closest prior art: every payload is in an entry, a bound value or the ledger, with
no third place; **pending** rows hold content that is durable before it has a place in the tree, deleted
in the same transaction that places the real entry, so no partial transaction is visible; and a
compaction is a self-contained checkpoint with a complete retained tail, not a pointer into history.

### 4.5 Events

One event per thing a head must see, all with `(session_id, seq, ts)`: `TurnStarted`,
`PromptProgress`, `Delta`, `ToolCallProposed`, `DecisionRequested`, `DecisionAnswered`, `ToolStarted` /
`ToolProgress` / `ToolFinished`, `TurnFinished`, `TurnInterrupted`, `TranscriptAppended`,
`HeadAttached` / `HeadDetached`, `Warning{code, detail}` (where §18's assertions land) and
`Explain{turn_id, plan}` (§6). `Delta` carries **only the increment**; no event ever carries accumulated
text, which is half of "render cost independent of output length" (§13.3 is the other half).

## 5. The prompt pipeline

### 5.1 End to end

Stable prefix (frozen, content-addressed) → items 0..k → **render** (our renderer, the model's control
tokens verbatim, no jinja, no probing) → **tokenize** (libllama, this model's vocab, appended to the
vector, nothing earlier re-rendered) → ledger append and hash chain (§4.3) → **submit** (§3.3, or
`/completion` with a token array) → progress, deltas, tool-call spans from our parser, usage and timings
→ post-flight assertions (§18.2) → append Reasoning / Assistant / ToolResult.

Two consequences. **Generated tokens are appended as tokens, not re-derived from text**, so §18.1-I1
holds by construction with no detokenize/retokenize round trip where a whitespace or special-token
difference would creep in. And **parsing is ours too**: reasoning boundaries and tool calls come from
our parser over the token stream using the control tokens our renderer emitted, so `--reasoning-format`,
`reasoning_content` reconstruction and the whole grammar-per-model layer are not in the path.

### 5.2 The stable prefix

System text plus normalized tool schemas (canonical JSON, sorted keys, tools in a fixed order, no
incidental whitespace), frozen behind a fingerprint, content-addressed and persisted so it is shared
between sessions and survives a restart. **A changed stable prefix is a new prefix and therefore a cold
start** - the correct cost, which the harness must not hide but must not incur needlessly:

1. **Nothing volatile goes in the stable prefix** - no timestamp, no cwd listing, no git branch, no
   "files recently changed". Adding one line to an `<env>` block once cost a full cold re-prefill of a
   179k conversation.
2. **Everything volatile arrives as an appended item** (§5.3).

Two content rules, both hard because they cost real debugging time: **a prompt may describe how to use a
tool but never what the data contains** (an example list read as an inventory, so the model answered "we
have nothing" without calling a tool - the tool description is prompt and needs the same expiry date),
and **state the answer language**, or a Chinese-trained model drifts into Chinese mid-sentence.

### 5.3 System prompt changes are appended (`in-history`)

Compare the newly rendered prompt against **the last non-empty system node**, not node 0. Equal: emit
nothing, not even a no-op event. Different and the route declares `in-history`: append a new
`role: "system"` message at the tail with a source marker and no wrapper tags. Otherwise (capability
absent, a series boundary, the prompt cleared): fall back explicitly, blanking every later system node
and rewriting node 0, normalizing history back to single-system form. Under self-rendering this stops
being a gamble on someone else's template, and what remains is behavioural - was this model trained to
accept a system turn mid-conversation - which is an eval. The conservative default until then, per
model, is to emit the update inside the user turn with a fixed envelope
(`<system-update seq=N>...</system-update>`): still an append, still prefix-stable. Both forms are one
dialect field, `system_update_mode`.

### 5.4 Reasoning is not "replayed" - it is simply still there

There is no `reasoning_content` field to send back, no field-name detection, no capability probe and no
chance a template drops an earlier turn's thinking: the tokens are in the vector and the vector is never
rewritten. That deletes a reasoning-field scan, a compat flag with hostname auto-detection (whose silent
self-disabling behind a proxy was the documented cause of the 612 GB growth), a dual-emission
`preserve_thinking`, and llama.cpp's `clear_thinking`/`drop_thinking` probing. Two smaller obligations
replace them: the renderer must place thinking blocks exactly as the model was trained to see them
(checked in CI, §7.2), and an assistant turn with no visible text must still emit its turn-boundary
control tokens - `content: ""`, never `null`, because the message sits durably in the log and a null
there bricks every later turn.

### 5.5 Fork, not truncate

Any operation that would shorten or rewrite history - hard compaction, rewinding, dropping an image -
creates a **new transcript** with `parent_transcript_id` set, and the session points at the new one. All
the consequences are wanted: the old transcript stays intact so a rewind is reversible and `EXPLAIN` can
still say what the old prefix was, the divergence becomes a first-class object with an id, and the old
cache entry is not superseded so it survives until the ladder demotes it. The cost is a re-prefill from
the divergence to the end, `(n_total - n_common) / 441 t/s` here, and §10 is largely about making that
number small.

### 5.6 Submission - a protocol we own, and its fallback

The primary path is `Submit` over the control channel (§3.3), carrying what an OpenAI schema cannot
express: the shm region and span, `message_spans` and `segments` given rather than string-matched, the
`prefix_handle` assertion, and the `want` set. The fallback is `POST /completion` with `prompt` as a
token array, which today's unmodified server accepts - this is what M1 runs on and what this tree runs
on, and it keeps a server rebuild from ever blocking a harness milestone. On that path
`cache_prompt: true` (the whole design), `return_progress: true`
(`result_prompt_progress{total, cache, processed, time_ms}`, which drives §6's display),
`timings_per_token: true`, `return_tokens: true` (generated token ids, so "append tokens, not text"
holds here too), `message_delimiters` (the pre-fusion way to get checkpoint placement), and **no**
`n_predict` cap. In every surveyed harness `return_progress` and `timings` were awkward extensions
bolted onto someone else's schema, and one of them did not count a llama.cpp progress chunk as
progress, so a long prefill raced a 300 s idle timeout.

### 5.7 No output cap, and `finish_reason: length` is acted on

opencode capped output at 32,000 tokens, then parsed `finish_reason` and threw it away - so a subagent
spent its whole budget thinking and returned an empty result recorded as success. That is F5 exactly.
The policy:

```
on finish_reason == "length":
  if the turn produced NO visible content and NO tool calls   → HARD FAIL the turn.
  if the turn produced ONLY reasoning                         → HARD FAIL the turn.
  if there are tool calls:
      all arguments parse as complete JSON → keep them, continue
      any argument truncated              → FAIL THE WHOLE BATCH, none executed
  otherwise (real text)                                       → keep, mark truncated, continue
```

An empty `length` response fails under every policy value, and the two fail cases are named
`NoVisibleContent` and `ReasoningOnly`. When a batch fails, tell the model why **in the tool result, not
in a log**: *"Tool call X was not executed: the response hit the output token limit, so its arguments may
be truncated. Re-issue the tool call with complete arguments."* Bound the retries, because an unbounded
salvage loop is its own failure mode. `TurnFinished{finish_reason}` reaches every head, a `length` finish
is rendered distinctly, and a truncated turn is never reported as completed.

### 5.8 Steering - messages that arrive mid-turn

Queue until the turn ends is wrong for the common case, because the message is usually a correction and
waiting for a 90-second generation wastes it; interrupting immediately truncates mid-stream and leaves a
partial turn that must be kept for cache reasons while being useless as content. **Inject at the next
step boundary** - after the current generation completes or after the current tool batch settles,
whichever comes first - where the message is appended as a `User` item and the loop continues with it in
context, keeping the vector append-only with no partial-turn bookkeeping. A message flagged `urgent`
(`ABORT` is its extreme form) interrupts at the next token and the partial output is kept with
`truncated: true`. Steering never rewrites or reorders anything already in the vector, which is why this
is three lines of policy rather than a subsystem.

## 6. `EXPLAIN` - per-stage observability as a feature, not as logging

### 6.1 Why it is a feature

Three of oracle's eight diagnoses in one evening were wrong, and the cause each time was the same: no way
to measure one stage alone, so a wrong answer could not be attributed. "Why did this request prefill
144,436 tokens with zero cache hits?" is a query-plan question that cost hours of log archaeology and
should cost one command.

### 6.2 The plan object

Every turn records a plan, persisted with `turn_metrics` and emitted as an `Explain` event. The cold-turn
rendering is the deliverable:

```
Cache                      prompt_tokens 144,436   cached_tokens 0   f_keep 0.000
  divergence               at item 3 (System, origin=Update)
                           expected h 7a19c2…, computed h 91be03…
                           cause: stable prefix changed (0c81… → 55ad…), field <env>
  cost                     144,436 tok re-prefill ≈ 327 s at 441 t/s
```

It names which prefix matched, where it diverged, what caused it and what it cost - the four things that
took an evening to establish by hand. The type does not exist yet: `ExplainPlan` has no field list, so
the event carries the plan untyped, which is deliberate, because a field list invented before the
counters exist would be a list of guesses.

### 6.3 Where the numbers come from

`prompt_tokens` and `cached_tokens` from `usage.prompt_tokens_details.cached_tokens`, a standard OpenAI
field fed from the server's own cache counter with no extra body; live progress from
`prompt_progress {total, cache, processed, time_ms}`; prefill and decode rates plus draft acceptance from
`timings`; box-level cache health and per-slot residency from `GET /metrics` and `GET /slots`; which entry
matched and its `lcp` from `Explain{turn_id}` (§3.4.6), or until then the server log at `-lv 4` for
`explain --deep`; admission, seats, ladder rung and batch-iteration sharing from `Explain` and `Stats`,
unobtainable across an HTTP boundary; and the expected common prefix from the ledger (§4.3). That last
one is what turns a measurement into a diagnosis: the server can say *what* matched, only the harness
knows what *should* have.

### 6.4 Stage boundaries that must be independently measurable

Prompt construction (items, rendered bytes, ledger head, build time); cache (expected vs actual common
prefix, divergence point and cause); prefill and decode **separately, always with their concurrency**;
tool execution (per call: wall time, bytes returned, bytes inlined, outcome class); permissions (asked,
answered, by whom, after how long); render (per head: events delivered, bytes, drops, resyncs).

## 7. Per-model dialects - renderers we own

### 7.1 What a dialect is now

A dialect stops being compat flags describing someone else's renderer and becomes **a renderer**: the
flag approach expresses every quirk as a knob on a foreign engine, and every knob is a guess that can
silently stop being true. The trait has five jobs - `render` (prefix plus items to `RenderSpan`s),
`render_incremental(prev_end, new_items)`, `parse(tokens)` to reasoning / tool-call / content spans,
`control_tokens()` and `stop_tokens()` - plus `system_update_mode()`, per-dialect `guards()` and
`template_sha()`.

Two invariants, and they are what the tests check: **`render_incremental` must agree with `render`**
(rendering `0..k+1` equals rendering `0..k` and appending item `k+1`, which the whole append-only design
rests on, and being a pure function it is property-testable without a model), and **`parse ∘ render` is
the identity on the round-trippable parts** (render an assistant turn with reasoning and two tool calls,
parse the tokens, get the same structure back), which catches a control-token mistake before a model
does.

### 7.2 Validation against the shipped jinja - a required CI check

This is the mitigation for the one real hazard self-rendering introduces: a silent divergence between our
renderer and what the model was trained on. `POST /apply-template` runs the same parse as the chat
endpoint and returns the rendered string, touching no slot, no sequence and no GPU, so it is a free
oracle:

```
for each dialect D, for each fixture F in the corpus:
    ours   = detokenize(D.render(F))
    theirs = POST /apply-template {messages: F.messages, tools: F.tools, ...}
    assert ours == theirs        # exact string equality
```

That subsumes the byte-prefix assertion: equal renders for every prefix of every fixture establish prefix
stability and template fidelity in one check. It is **required** - a dialect whose diff fails does not
ship and a model whose diff fails is not admitted, with no "compat quirk" escape hatch; it runs on every
commit since it costs no inference, and again on every server upgrade and model file change keyed by
`template_sha` mismatch; and the fixture corpus must cover the shapes that break - a plain turn, an
assistant turn with reasoning, reasoning interleaved with two tool calls, a tool result, an *empty*
assistant turn, a system message at index > 0, an image, a very long turn, and the same fixture truncated
at every prefix boundary. Where they legitimately differ the dialect records it with a reason and the eval
that justifies it - a named, reviewed field, not a silent inequality. When a model ships a template change
`template_sha` mismatches, the diff reports which fixtures moved, and adopting it forks every live
transcript on that model (§5.5).

### 7.3 The three targets

**GLM-5.3-Flash** is production: thinking family `zai`, 262,144 per slot against a 327,680 elastic pool,
MTP draft at `n_max=2`, vision through `--mmproj`, and a historical repeating-token collapse past ~78k at
`-ub 512` that poisons the slot rather than erroring, so §8.5's guard is built regardless. **Qwen Flash**
is thinking family `qwen`, where what was a `preserve_thinking` puzzle across three transports becomes
one decision: thinking blocks for prior turns are emitted, always. **Qwen 3.8 27B dense** has a model file
but no preset, and adding one is a prerequisite (D7: derive it by measurement).

Model switching is expensive and the harness must know it: with `--models-max 1` a switch evicts the
other model, GLM reloads in 29-31 s, and the evicted child spills its cache and re-indexes on return. So
the model is a property of the **session**, not the turn; a session that changes model **forks** its
transcript, because the renderer and therefore every token changed; and the client batches same-model
work.

### 7.4 Adding a fourth model

Add a preset to `router-presets.ini`; write the dialect or declare that it reuses one; run the
**`/apply-template` diff across the whole corpus as a hard gate**; property-test `render_incremental ≡
render` and `parse ∘ render ≡ id`; do a short generation to confirm the parser recovers reasoning and tool
calls from real output plus one `n_predict: 1` call to confirm `finish_reason` classification; then admit a
session and run the dialect-parameterized conformance suite. If the diff fails, the honest options are to
fix our renderer or patch the shipped template, not to ship a divergence.

## 8. The tool contract

F4 says close the loop and F5 says never let a component's "I did not do this" be reported upward as
success; both live here, because a tool's failure mode is a design decision whose cost is paid in
permanent context.

### 8.1 The contract every built-in tool must meet

Six clauses, and a tool that does not meet all six does not ship.

1. **A miss is self-correcting in the SAME call.** A bare "not found" leaves the model nothing to correct
   itself with, so it guesses again and the retry dance bloats context: a `grep` that finds nothing under
   a scoped path reports where the term *does* occur, a `find` that misses returns the surrounding
   listing, and a too-strict anchor is auto-relaxed to the bare identifier with the relaxation reported.
2. **Malformed input is salvaged, not rejected** - a best-effort parse plus an explicit note of what was
   repaired.
3. **Outcome is a closed vocabulary, and abstention is not success** (§4.2), and `Abstained` must be
   *structurally* distinguishable in what the model sees, not merely worded differently (§8.2).
4. **Read/write is declared in the schema, not decided per call** - `access: read | write | exec |
   network`, which is what §11's policy keys on.
5. **Output is bounded and spilled, never truncated** (§8.3).
6. **The description says how to use the tool and never what the data contains** (§5.2); tool descriptions
   are prompt, are never audited, and go stale.

### 8.2 Making abstention impossible to paper over

The measured failure: retrieval said "the corpus doesn't cover this", and the model invented a search
term, found an adjacent passage, relabelled it, dropped two items and attached a citation - a
hallucination wearing a footnote is more dangerous than a naked one. The harness cannot stop a model
writing a confident sentence, but it can stop the *system* reporting that sentence as grounded: the
outcome class is carried rather than merely worded, so an `Abstained` result renders with an unmistakable
envelope and the model sees `NO_RESULT` as a distinct token sequence; abstention propagates upward, so a
subagent whose calls all abstained cannot return `Ok`; and a turn that produced no visible content fails
(§5.7), because empty is not a value. What it does not do is judge whether the prose is supported by the
output - that is a quality question and belongs in an eval.

### 8.3 Spill, never truncate

One configured threshold, `max_inline_bytes` (UTF-8 bytes), with **no default**, so unset means the policy
is a genuine no-op rather than a silent guess - and D6 makes the threshold a *decider* whose simplest
implementation is a fixed count, with a per-tool or predicted budget plugging into the same seam. The
preview budget is `cap - reserve`, where `reserve` is the exact byte cost of the notice computed against a
worst-case digit count, so `preview + "\n\n" + notice` never exceeds the cap; if the notice alone would
exceed it, spill is abandoned and the original left inline. The split is `head = ceil(budget/2)`, `tail =
floor(budget/2)`, the notice reads `(Omitted <N> bytes. Full <kind> result stored at: <locator>. <hint>)`,
and the full original is written `0600` under a per-session hashed directory, with a storage failure
falling back to the untouched inline content rather than erroring the call. Ours differs in one way: the
locator is a content hash into the `tool_spill` table and there is a `read_spill(hash, range)` tool, so
the hint is actionable rather than advisory.

### 8.4 Tool budget - a hard ceiling, enforced

Past ~5-7 MCP servers small models get *worse* at choosing tools (15 tested fine, 23 was near the edge),
and our targets are local models, so this binds and the consequence is task-focused roles rather than one
mega-agent: `orchestrator` (task, read, grep, glob, ask_code, ask_corpus - 6), `coder` (read, write, edit,
grep, glob, bash, read_spill - 7), `researcher` (ask_corpus, search_corpus, ask_code, read, grep,
read_spill - 6) and `reviewer` (read, grep, glob, git read-only, read_spill - 5). Enforcement in the F4
sense: a role declares its tool set and the daemon **refuses to start a session** whose resolved count
exceeds `max_tools` (default 8), naming the overflow; MCP servers are attached per role, and one
advertising more tools than the role's remaining budget is refused with its tool list rather than silently
truncated; and the count is in the `EXPLAIN` header.

This ceiling is also the argument for subagents, and the naive argument is wrong: **subagents pay off
through context hygiene, not decode speed**, since decode is flat against depth (F2). A subagent's
exploration never enters the parent, so the parent's depth and prefill bill stay bounded; the cost is that
many shallow contexts is the ID-bound regime this box is short of, so concurrency is a configured number
reported in `EXPLAIN`.

### 8.5 Two guards that cost nothing

A rolling n-gram check over the last K generated tokens turns GLM's `@@@@...` collapse - ~3 t/s and a
poisoned slot until restart - into `Warning{code: repetition_collapse}` (a per-dialect guard, §7.1). And
after an idle sleep the first request pays a 29-31 s reload, so the harness needs an explicit warming
state with no idle timeout below ~120 s and must count `prompt_progress` chunks as liveness.

## 9. Context occupation vs cache stability - the tension, resolved

F1/F2 say the prompt must be append-only and byte-stable, reasoning replayed verbatim, history never
reshaped mid-flight; F3 says occupation defocuses. The cost is not small: reasoning measured ~51.7% of a
conversation's content, so replaying it roughly doubles occupation.

### 9.1 The position

**Keep the prompt append-only and byte-stable. Make what goes into it dense. Reclaim occupancy at
compaction, on a schedule, never by reshaping history mid-flight.** In priority order:

1. **Nothing enters the transcript that does not have to be there** - tools return what is needed and no
   more (§8.1), oversized results spill (§8.3), no restated framing, no re-injected reminders repeating
   what is three turns up, no "here is what I will do next" narration recorded as an item.
2. **Reasoning is replayed verbatim, and that is not negotiable while llama.cpp caches generated tokens**;
   dropping it is what produced the 612 GB and the 78,598-token p99, and the doubling is the price.
3. **Occupancy is reclaimed by compaction (§10), scheduled, incremental and cheap** - which is what makes
   (2) affordable and why §10 is high in the phasing.
4. **Depth is preferred over premature compaction.** A 150k warm conversation costs about the same per turn
   as a 15k one, so compacting early trades a real cold prefill for a speculative quality gain; the wall
   we compact against is the window and the memory budget plus §9.3's signal.

### 9.2 Why reasoning is the *right* half to keep

Reasoning tokens are the part the model produced to reach the answer, so replaying them is the closest
thing to giving it its working memory back; the part a summary reconstructs worst; and not something the
harness can make denser, because a tool result can be spilled and a thought cannot be re-run. Tool output
is the opposite on all three counts, which is why §8.3 spills aggressively and §10 compacts tool-heavy
segments first.

### 9.3 What would falsify this position

**Falsifier A**: the same task set with reasoning replay on and off, scoring answer quality - if quality
is better without replay at equal depth, reasoning is noise and the right answer becomes a server change
with thinking in its own segment. **Falsifier B**: quality against depth at 20k, 60k and 150k of preceding
context - if quality falls off a cliff well before the window, §9.1 clause 4 is wrong and compaction must
be aggressive and early whatever it costs in prefill. This is the important one and it is milestone M3.5
(§17). **Falsifier C**: if §10's incremental compaction cannot hold summarization cost near zero - if the
summarizer call does not hit the warm prefix (§18.1-I5) - then clause 3 is unfunded and the fallback is to
cap depth much lower.

### 9.4 Retrieval stays out

The survey's retrieval findings (the embedder measures resemblance not truth, reranking can only reorder
what stage 1 found, recall@64 is the number that matters) are about RAG. This harness calls `ask_corpus` /
`ask_code` / `search_corpus` as tools and treats their abstention per §8.2. The one carry-across is
negative: **the harness must not silently "improve" a tool's query**, and if it ever does, the rewrite must
be visible in the result.

## 10. Compaction, as leveled compaction

### 10.1 The reframe

The surveyed default is a stop-the-world full pass: flatten the whole conversation into one user message
sharing a prefix with nothing - 144,436 tokens, cache hit 0, ~13 minutes before the first token of summary
- which is pre-LSM compaction, one blocking pass over everything at an arbitrary moment, costing more the
better the system has been working. The mapping is L0 = the last N turns verbatim (the memtable), L1 = a
segment summarised once and still detailed, L2 and beyond = older segments merged and re-summarised
coarser, compaction running incrementally in the background, and write amplification becoming **fidelity
amplification** because each re-summarisation of the same material loses detail. It does not make the
stall faster; **it removes it as a category**, because no single operation ever spans the whole history.

### 10.2 Levels

L0 is the last N turns (default 8) and everything since the last user boundary, verbatim, never
summarised - the append tail and where the cache is warm. L1 is closed but recent segments (default: the
last 3), summarised once at ~1:4. L2 is older segments merged in groups and re-summarised at ~1:4 of L1
(≈1:16 overall). Lc is the cold floor: one running "what this session is about" paragraph per session plus
an index of segment labels and their spill locators, never discarded.

Two properties hold. **Levels are monotone in age but not merged across a live boundary**: a segment the
operator is actively working stays at L0/L1 regardless of clock age, because `last_touched` moves it and
`created_at` does not - which is "different decay rates" (§19.1) falling out of the level rule. And
**fidelity amplification is bounded by construction**: at most one re-summarisation per level with the
number of levels fixed at 4, so no piece of history is summarised more than three times, and the count per
segment is visible in `EXPLAIN`.

### 10.3 Warm compaction - the original work

**The summarizer request is the last routed request, byte-for-byte, plus one appended user message.**
Nobody in the survey does this on the default path; two references do, and both say why - keeping the
conversation's own system prompt, tools and message prefix in front of the auxiliary call makes it a
genuine prefix of the last routed request, so the KV cache is reused instead of invalidated. Because the
transcript is append-only and ledger-chained this is not an approximation: it is literally
`render(stable_prefix, items[0..k]) ++ render(instruction_item)`, and the ledger proves it. Expected
`f_keep` ≈ 1.0 and prefill ≈ the instruction's own length; §18.1-I5 asserts it.

For a **segment** summary the warm prefix is the whole conversation up to the segment's end, so the
instruction names the segment by its `SegmentMark` ids - *"summarise only the span between marks S7 and
S8"* - and the prefix stays the full live one, which makes a mid-conversation segment summary cost the
same warm prefix as a tail summary.

### 10.4 Applying the result: soft vs hard

**Soft compaction (default)** appends the summary as an in-history system update (§5.3) or a marked user
item: *"Segments S3-S5 are summarised below; rely on the summary rather than re-reading those turns."*
Nothing is removed, so it costs zero prefill but frees no tokens either - it reduces attention pressure by
telling the model what to lean on, and it is right whenever the constraint is F3 and there is window room.

**Hard compaction (at the wall)** forks the transcript (§5.5) into `[stable prefix][Lc floor][L2
summaries][L1 summaries][L0 verbatim tail]`, costing one cold prefill of the shorter prompt (~91 s for a
40k post-compaction prompt, once) and freeing real tokens and real KV. Two ordering details, both tested by
someone else and neither obvious: recent verbatim before the summary, and project instructions re-injected
verbatim so they survive compaction independently of what the summarizer chose to keep.

The scheduler for both: soft runs opportunistically when a segment closes and the box is quiet; hard runs
only when `prompt_tokens > high_water` (default 80% of the slot's `n_ctx`), and by then every segment
already has a summary in the journal, so the fork is an assembly rather than a model call. That is what
removes the stall.

### 10.5 Should this share a policy with the server's cache ladder?

They have the same shape - degrade the least-degraded first, spread loss evenly, LRU within a rung - and
**they should stay independent, sharing vocabulary rather than policy**. They optimise different things:
the ladder trades restore cost against bytes and a degraded entry still restores the same conversation,
while compaction trades information against tokens and a coarsened segment does not restore. Their units
differ, a whole cached prompt against a part of one. And coupling them would let memory pressure on the
server silently degrade *answer quality* in the harness, where memory pressure should cost latency and
never fidelity. What they should share is the vocabulary (`degrade` / `spill` / `drop`), the
"least-degraded first, LRU within a rung" rule, and the `EXPLAIN` surface.

### 10.6 Where the summary lands in the prompt

Recent verbatim before the summary, and reasoning kept in the retained tail explicitly, so the prefix cache
stays warm.

## 11. Adjudication - one interface, three adjudicators

### 11.1 The requirement, in the operator's words

> *"we are flexible, it is ok to ask me, it is ok to use firecode and it is ok if i plug a model for
> auto mode."*

A pluggable adjudication layer, not a doctrine: **boundary** decides by structure (the action is safe
because of where it runs), **human** by judgement over flowy or a guest's `--ask`, and **model** ("auto
mode") by a separate, probably smaller model, for the middle band that is too frequent to ask about and too
consequential to wave through. All three are first class and the design's job is to make them
interchangeable rather than to rank them. The volume of human asks is an **observable, not a constraint**:
if it is high, either the boundary is drawn wrong or the policy is misrouted, and §6 is where that shows
up.

### 11.2 One decision shape

Every adjudicator answers the same question and returns the same object, which is what makes them swappable
and the audit uniform.

```
AdjudicationRequest {
  id ULID; session_id, turn_id, tool_call_id; agent
  action   { class, tool, summary, arguments, arguments_digest }
  context  { run_id, workspace, boundary_facts, prior_decisions_for_this_class }
  kind     Permission | Question                                   // §11.6
  options  [ {id, label, kind: allow_once|allow_session|allow_always|deny|deny_and_tell|<free>} ]
  deadline instant; on_timeout Deny | Allow | AgentDecides         // §11.5
}

AdjudicationDecision {
  request_id
  outcome  Selected{option_id} | Escalate{to, why} | Unavailable | Cancelled | Timeout
  by       adjudicator id ("boundary:firecode", "human:deadtrickster", "model:qwen-3.8-27b")
  basis    short text - the reason, always present, never optional
  at, latency_ms
}
```

One property of the closed outcome vocabulary matters most: *a throwing or non-conforming answerer becomes
`unavailable`, never silently opens the gate.* `Escalate` is first class rather than an error path (§11.4).

### 11.3 Routing policy is configuration

The **action class** is the routing key, derived rather than free text from four facts the harness already
has: `tool.access` (read | write | exec | network, declared in the schema per §8.1 clause 4),
`effect_scope` (in_run | host_project | host_other | external), `reversibility` (reversible |
irreversible) and `cost` (free | metered). Policy is a table the operator edits, first match wins:

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

Three properties this buys: the operator moves a whole band between adjudicators with one edit; a class
never seen in the log is a class nobody has thought about, and §6 lists observed classes with counts so the
policy can be reviewed against reality; and `boundary` rows cost nothing at runtime - evaluated in-process,
a `Selected` decision with `basis = "in-run, no host effect"`, recorded but delivered nowhere.

### 11.4 The boundary adjudicator

It decides by structure. What makes an in-run action safe: the guest sees a copy of one project and nothing
else of the host; `--add-dir` and `--workdir` **refuse outright** to carry `.ssh`, `.gnupg`, `.aws`,
`.kube`, `.config/gh`, `.password-store` or a browser profile out of the home directory; the host project
directory is not modified by a run not asked to write back, since work is copied to a sibling directory on
delivery; nothing a VM writes reaches the host except through the project directory or a disk explicitly
attached writable; project paths are resolved from the spawn server's config, never from the caller; and
lifetime is a cgroup, so cleanup is total and transitive.

What it explicitly does **not** protect is API credits and the network - the agent has real credentials
because otherwise it cannot work, so `metered` and `network,external` are routed away from `boundary` in
§11.3. **When the boundary denies** an action whose class says `in_run` but whose arguments would leave the
run, it **escalates rather than hard-failing**, because the common case is a call that is merely
misaddressed, and it escalates to whatever the policy names for the *actual* class of the action.

### 11.5 The human adjudicator, and `--ask` semantics

The semantics are kept precisely because the human path is rare, and a rare ask that silently hangs is
worse than a frequent one.

- **Post and wait**, then **if nobody answers, say so and tell the agent to decide and record what it
  chose** - `on_timeout: AgentDecides`, right for a *question* and not for a permission over an
  irreversible external effect, where the default is `Deny`.
- **Acknowledge before you act.** Silence is indistinguishable from absence: an agent once waited three
  minutes, concluded nobody was coming, and went and fixed the thing itself. A human client that opens a
  request sends an `Ack`, which the daemon records and which extends the deadline.
- **Start listening before you start working.** `SessionStarted` is not emitted until the connector reports
  a live subscription, and a lost subscription raises `Warning{code: no_human_channel}` and *changes the
  effective policy*, so `human` rows fall through to their `on_timeout` immediately instead of waiting
  fifteen minutes for a message nobody will see. (That warning code is not raised anywhere in this tree
  yet.)

The human primitive is a **visible row, not a held connection**: non-blocking, board-visible and
self-clearing, where any relevant write by the named person *is* their answer. Delivery and audit: fan-out
to every registered channel with first valid answer wins and a later answer recorded as `late`; authority
checked against the session's `approvers` list using the authenticated actor id, never a display name;
cancellation is a withdrawal rather than an interrupt; and every request, fan-out, ack, answer and timeout
is an immutable row, log-only and never in the model transcript.

### 11.6 Permissions and questions are one mechanism

They differ only in `kind` and in the default on no answer. A **permission** ("may I do X?") offers the
allow/deny ladder and defaults to `Deny` for anything irreversible. A **question** ("install from source,
or stop?") offers domain choices, accepts free text, and defaults to `AgentDecides`: the agent is told
nobody answered, decides, and records what it chose as a transcript item, so the choice is in the
conversation rather than in a log. One mechanism, because the combined volume is low and because §19.2 is a
question, not a permission, and must not need a second channel built for it.

### 11.7 The model adjudicator ("auto mode")

**What it sees** is a bounded, structured brief and never the conversation: the action class, tool name and
schema, the arguments verbatim with any value over 2 KiB spilled to a digest plus head/tail, the boundary
facts, the session's stated goal, the last N decisions in this class, and the option list. Not the
transcript, for cost and for independence - an adjudicator that reads the agent's own reasoning is being
argued with by the thing it is meant to check. In the tree this is `ModelBrief` in
`crates/tools/src/authorise.rs`, which also carries the operator's past answers as worked examples, the
agent's claim (evidence, never a citation) and the gate's prior record on this shape (evidence, never a
promotion).

**What it returns** is the same `AdjudicationDecision`, constrained to `{option_id | "escalate", basis}` by
a grammar/JSON-schema constraint so a malformed answer is impossible rather than salvaged; `basis` is
mandatory and is what makes the log reviewable. **Which model**: not the session's, because a different
model is a genuine second opinion, a smaller one is cheap, and using the session's model means every gated
call evicts the other model or contends for the same seats - the recommendation is the 27B dense as a
second router entry (UNVERIFIED-7). Unavailable (not loaded, request failed, malformed output twice) gives
`Unavailable`, which routes to the row's `on_timeout` and never a silent allow; unsure returns `escalate`,
and escalation to the human is a normal outcome rather than a failure.

**Auditing auto mode**, because this is where "did less than it claimed and said nothing" would live: every
model decision stores the brief digest, the raw output, the chosen option and the basis; a **shadow mode**
(`adjudicator: model, shadow: true`) lets the model decide without applying it, routing the row to its
fallback and recording both answers; and `EXPLAIN` plus an adjudications report show per class the counts,
allow rate, escalation rate and agreement with the human on the shadow sample.

### 11.8 What firecode must provide

D3 answered the first two - a parent cgroup is use-dependent, and the persistent shell was withdrawn in
favour of byobu - and asks 3-7 stand as one request: give the room the request/response object §11.2
already needs, and the flowy bridge becomes a mapping rather than a parser.

1. Room messages need ids, so an ask carries `{id, in_reply_to}` and a reader can say which ask it answers.
2. A deadline and a stated default on `--ask`, machine-readable rather than a convention in prose.
3. An `--ack` verb, because "acknowledge before you act" is currently a rule people follow.
4. A per-run room, or a run tag on every message, since one global room means the bridge cannot address the
   right agent and two concurrent runs asking questions are indistinguishable.
5. Structured `--ask` options - `--ask --option 'source:Install from source' --option 'stop:Stop and
   report'` - returning the chosen id, with free text as a fallback.

### 11.9 Where opencode's prior art is, and is not

There is none for the async path: opencode's `permission.ask` is declared with zero trigger sites
repo-wide, a dead hook. Worth taking is its permission *model* (the allow/ask/deny ladder and its
persistence shape) and one service discipline: the `never` policy is enforced inside the service before
dispatch, so no listener can bypass it. Ours needs the same - a `deny` row in §11.3's table is evaluated
before any adjudicator is consulted, and no adjudicator can override it.

## 12. What flowy must provide

### 12.1 What flowy already has

More than assumed, from its own route table: projects, rooms, DMs, a named-reader inbox with single-holder
semantics and server-verified attachment, attachments, a WebSocket for live agent/shell/VM sessions, a
task-delegation state machine, an announcement lifecycle with `ack` and `resolve`, `todo waiting-on`, ULIDs,
HLC ordering, signed rows and an MCP server. Messages carry an authenticated `actor` ULID plus `actor_kind`,
enough for §11.5's authority check, and long-poll with a deadline is the delivery model everywhere.

Three corrections to the optimistic reading. **SSE exists but chat is not on it** - the stream covers
`todos` and `queue` only, so chat-as-a-pushed-event requires a flowy change and is not a matter of finding
the right endpoint. **There are three wake levels, not four** - all, addressed, mentions-only - plus two
orthogonal scopes and a per-room mute, and inside `addressed` a person's *unaddressed* broadcast wakes you,
because agents address each other by habit. And **per-room policy does not exist, and cannot**: the level is
sent as query params per poll and stored nowhere, because a reader belongs to a *name* and a second waiter
on that name is refused.

### 12.2 The asks

1. **A `decision` object** - `POST /api/decision` returning `{id}`, body `{project, to, kind:
   permission|question, title, body, options: [{id, label, style}], deadline, on_timeout, meta}`;
   `GET /api/decision/{id}` for its state; `POST /api/decision/{id}/answer {option_id | text}` answering
   exactly once, with a second answer refused naming the winner and the time; `POST /api/decision/{id}/ack`;
   `POST /api/decision/{id}/withdraw`. This generalises what announcement plus `ack`/`resolve` plus
   `waiting-on` already do. *Nothing else here matters as much.*
2. **Options rendered as choices**, so a client shows them as buttons and the answer is `{decision_id,
   option_id}`, with free text still valid.
3. **Server-enforced deadlines** on the object, not only on the poll: the node marks it expired, notifies the
   requester, and clients stop offering the choice.
4. **Delivery receipts** - `sent` / `delivered` / `seen` per addressee - because without them "the human has
   not answered" is indistinguishable from "the message never arrived", and this fleet has already paid for
   that.
5. **Multiple concurrent readers per seat, or a reader namespace.** The inbox is single-holder, harnessd
   needs its own long-lived reader, and the operator's interactive session already holds the seat's: either
   readers are named `(seat, purpose)` so both coexist, or the harness gets a seat per agent - the second is
   better anyway, since a permission request should be addressable to which agent and which session.
6. **A pushed inbox stream**, the highest-priority ask after the decision object, filed verbatim:

   > `GET /api/inbox/stream?as=NAME&addressed=&mentions=&focus=&room=` - same `wakesFor`, same reader row,
   > same enrichments, same heartbeat, one JSON message per SSE event, `POST /api/inbox/ack` unchanged.

   That turns N exit-per-message polls into one connection while preserving the ack-after-write contract
   exactly. Two smaller asks ride with it: a fourth `waiter_kind` for a streaming listener, or the roster
   misreports what is attached; and `LISTEN`/`NOTIFY` to remove the internal 250 ms tick.
7. **Streaming/updatable message content**, so tokens stream into flowy rather than arriving as a wall after
   two minutes.
8. **Idempotency keys on send/answer**, so a connector retry cannot duplicate a decision.
9. **Cross-project reach for a decision**: a decision is addressed to a *person*, and that person's presence
   in the right project is currently a precondition the sender must know - either decisions route by actor
   or the API returns an explicit "not reachable in this project" error rather than accepting the write and
   delivering nothing.
10. **Optional, lower priority: agent-to-agent RPC** with the same object shape.

### 12.2b Per-room policy is ours to provide, not flowy's to fix

flowy's one-flag limit follows from one reader per name, so the harness provides the policy: a **per-room
subscription row** `(room, level: off | mentions | addressed | all)`, persisted with a session default;
**flowy's three definitions kept exactly**, because they are well-reasoned and the fleet has paid for their
edges; the one-flag shape dropped, so `all` in the home project plus `mentions` elsewhere is one
subscription and a table; and the **human-broadcast clause made an explicit named option**
(`wake_on_human_broadcast: bool`) rather than baked into `addressed`, because that clause has failed in
**both** directions - first a person's unaddressed "who is here?" reaching nobody, then a message addressed
to one agent waking every waiter so a seat acted on work meant for someone else.

### 12.3 What we build if none of this lands

A degraded v1, so flowy work is never a blocker: a decision is a DM whose first line is `[decision 01J...]
<title>` followed by the options one per line, answers are parsed with a strict grammar (`<id>.<option>` or
`<id> <free text>`) with anything unparseable answered by a reply saying what was expected, the harness
sends its own ack so at least its liveness is visible, deadlines are enforced harness-side with expiry
appended to the message, and `no_human_channel` falls back to "the reader is attached per `GET
/api/presence`", which is a proxy and is labelled as one. The cost is that a lost message looks like a slow
human, which is why ask 4 is high on the list.

### 12.4 flowy and the firecode room: separate, bridged

**They stay separate, and harnessd bridges them on the host**, for a trust reason rather than convenience:
firecode's guest is untrusted by construction, and making `firecode-chat` a flowy client would require a
flowy token inside the guest, granting a compromised guest the ability to speak and to answer decisions as
a seat, whereas `firecode-chat` today needs no credentials. The room must also keep working with no harness
at all, and flowy is the fleet bus (identity, projects, presence, a console the human already watches)
while the room is a local channel for processes that have nothing else.

So the room is the guest-side channel, flowy is the human-side bus, and harnessd is a host-side loop
republishing room messages into flowy with the run id and project attached and posting answers back - it
needs firecode asks 3, 6 and 7 to be a mapping rather than a parser. Our own asks never go through the room,
because harnessd holds the loop. The alternative, "flowy grows to subsume the room", is rejected only on
the credential point: if flowy ever supports a **capability token scoped to one run, with no write authority
beyond one decision id**, that objection disappears.

## 13. Multi-head attach and detach

### 13.1 There is no prior art; this is original work too

The closest surveyed thing composes each client's own agent session from a preset, and nothing says two
clients can concurrently observe and drive *one* live conversation; ACP is single-client by shape and
opencode's server is per-client. So "tmux for an agent" is unsolved and must be designed.

### 13.2 The model

- The daemon holds the session; a head sends `ATTACH {session_id, since_seq, identity, caps}`, and
  **authoritative state is the event log** (§4.5), monotonic `seq` per session.
- **Late join**: `since_seq = 0` gets a **snapshot** (a materialized view of messages and parts as of seq S,
  derived per §4.4 and rebuildable) then events from S+1, so a head joining a 300-turn session never replays
  300 turns of deltas.
- **Resume**: a returning head reconnects with its last `seq` and receives the gap; if the gap exceeds a
  bound (default 5,000 events or 4 MiB) the daemon answers `RESYNC` and the head takes a fresh snapshot.
  Resync is a normal outcome, never an error.
- **Fan-out**: the daemon is the only reader of the model stream, and each head has a bounded queue - on
  overflow the head is **demoted to resync**, not dropped and not blocked. *The model stream is never
  blocked on a head.*
- **Two heads acting at once**: every mutating command carries `expected_seq` and a `client_request_id`,
  commands are serialized on a per-session queue, and a prompt arriving mid-turn is **queued as a follow-up
  user item** rather than rejected, with the queuing announced as an event so both heads see it and who did
  it. Interrupt is idempotent, any attached head may issue it, and it is announced with the issuer's
  identity.
- **No input lease**: announce-and-serialize is sufficient for two humans, and a lease is one more thing to
  get stuck; automation may want one later and `expected_seq` is the field that would carry it.
- **All heads detach mid-turn: nothing happens.** The turn continues and events keep appending, the request
  is owned by the daemon's turn task and cancelled only by an explicit `ABORT` or shutdown. **TCP close is
  detach, never abort.** On completion with no head attached, a `turn_complete` notification routes to flowy.
- **Daemon restart mid-turn**: the turn is marked `interrupted` **and the partial assistant output is kept
  as a real transcript item**, because those tokens are in the server's cached entry and keeping them makes
  the next request a warm prefix extension while discarding them guarantees a divergence. The item carries
  `truncated: true` so the model is told.

### 13.2b Adopt flowy's delivery mechanics - several of §13.2's hard problems are solved there

Nine mechanics, taken rather than re-derived:

1. **A server-side cursor per named reader, acked after the client has written the messages out**, so a
   crash costs a duplicate, never a silence - our head protocol advances `seq` after the head has rendered,
   not on receipt.
2. **The mark advances over everything READ, not everything delivered**, because a filtering consumer that
   advances only over what it kept rereads its own output forever, and §12.2b makes the harness a filtering
   consumer.
3. **Report what was filtered out**: "busy and none of it was for me" is a different fact from "silent", and
   a head that suppresses events must say how many.
4. **One waiter per name, enforced by a written claim plus `kill -0`, with the refusal naming the pid.** The
   consequence is precise: **several heads share ONE authoritative reader; they must not each hold one.**
5. **Idle means quiet, not unwatched** - reaping on detach kills a twenty-minute build when a tab closes.
6. **Bounded scrollback that discloses the drop**, with the count present-and-zero rather than omitted,
   because an absent field and a zero field must not look the same when the field is the disclosure. Ours is
   `RESYNC`.
7. **Snapshot and register under one lock**, or the head misses exactly the event that made it attach.
8. **Non-blocking fan-out; drop the slow head and tell it.**
9. **Scrub the stored copy of anything interactive** - permission prompts, questions, progress frames and
   partial tool output replayed to a late head would be answered or acted on twice, so the snapshot a late
   head receives is the scrubbed projection: settled decisions render as their outcome, not as an open
   prompt. Related and equally sharp: an answer is owed only while no reader is attached, and a queued reply
   is **discarded rather than held**, because a reply to a question asked ten minutes ago is input arriving
   from nowhere.

What not to copy: exit-per-message delivery; the forked-successor handover (a detached listener that hears
everything and can wake nobody, which a persistent connection eliminates entirely); a 250 ms poll behind an
SSE facade; filter policy passed per-poll and stored nowhere; and preferences kept in an untyped note body.

### 13.3 Render cost independent of output length

Two halves, and every surveyed harness gets at least one wrong. **Wire**: events carry increments only
(§4.5). **Head**: incremental lexing with a frozen stable prefix - find the offset past the last token whose
raw text ends in `\n\n`, subject to guards (the break strictly inside the text, the next char starting real
block content, a preceding list provably closed because CommonMark can continue a list across a blank line),
then lex only the grown tail and concatenate, which is licensed by block tokenization being local across a
`\n\n` boundary with balanced fences: `lex(prefix) ++ lex(tail) === lex(prefix+tail)`. What not to do is
clear the container and rebuild every child from the whole accumulated text on every delta, which is O(n²)
over a message.

### 13.4 Heads, concretely

The **TUI** is a unix socket at `$XDG_RUNTIME_DIR/harnessd.sock`, with filesystem permissions as the auth.
The **remote** head is WebSocket plus TLS with a bearer token, carrying the same frames. **flowy** is an
in-daemon connector holding a push subscription (§3.7, §12.2 ask 6) and is a full head: inbound messages are
user items, outbound turn output is posted, decisions ride the same channel, and it streams token-by-token
once flowy has updatable messages - until then it posts at step boundaries, a batching of *output only*.
**ACP** is stdio JSON-RPC over `agent-client-protocol`.

ACP is an **adapter, not the protocol**. It does carry permission semantics -
`RequestPermissionRequest{session_id, tool_call_update, options}` to
`RequestPermissionResponse{outcome: Cancelled | Selected{option_id}, meta}` with allow-once /
allow-always / reject-once / reject-always - so the shape is right and the durability is not: no timeout at
the protocol layer, no request id in the body (correlation is the JSON-RPC envelope, so a request does not
survive a reconnect), cancellation is a value the client returns rather than an interrupt, and the outcome
enum is `#[non_exhaustive]`. Adopt its vocabulary in §11.2 and keep our own `{id, deadline, on_timeout,
audit}`, which ACP has none of; build the adapter early anyway, because it is cheap and earns an editor head.

## 14. Ported conformance suite - milestone zero

A test encodes the bug and the invariant, not the design, so it ports across languages in a way
implementation code does not. The suite exists before the harness does, and it is what tells us a milestone
beat opencode rather than felt like it. It lives in `tests/fidelity` - the `/apply-template` gate, its
fixture corpus and the oracle - with the C-checks below as its axis.

### 14.1 What to take, by source

**grok-build (Apache-2.0)** has the prefix-stability suite, whose own thesis is that prefix stability within
a compaction epoch is what keeps the cache hitting. The helper worth porting is `assert_prefix_stable_pair`,
which asserts `ext_input[..base_input.len()] == base_input` on the serialized `input` array, reports the
first divergent index, and deliberately does not compare `model`, `tools` and other fields outside `input`,
which are allowed to change; our equivalent must go through the real request builder and the real dialect
encoder or it proves nothing. Its twelve tests cover the three-turn baseline, an identical injected
reminder, reasoning-as-sibling ordering, a tool-schema change, a model switch, synthetic user messages
appending rather than inserting, **image pruning** (the documented exception, where strict prefix is not
asserted), small old images preserved, a budget pass mutating the outgoing copy only, tool-result pruning on
a clone, server-executed tool calls holding a fixed wire position, and session resume reproducing
byte-identical output. The three exception tests are as valuable as the nine strict ones: they encode where
the invariant is allowed to break, which is what a plan needs when it introduces forks and compaction. Also
port its length-policy family: a `length` stop is a hard failure unless explicitly salvaged, a rate-limit
error must never be reclassified as "truncated but complete", a `max_tokens` stop with a completed tool_use
keeps `Length` rather than being rewritten to `ToolCalls`, the salvage-streak cap, and a reasoning estimate
that takes the max of text and encrypted rather than their sum.

**omp (MIT)** has the reasoning-replay contract as one test - system prompt and tool catalogue byte-stable
across requests, then one turn adds the prior assistant turn plus a synthetic nudge and `cached_tokens`
collapses to 0 - plus its append-only-context family (prefix preserved when a *deep* message is rewritten,
divergence on a tool-result-metadata-only rewrite, divergence on a provider-payload-only rewrite, rewriting
message 0 forcing a full resync, and fresh-object clones with identical bytes treated as stable, i.e.
byte-equality rather than reference-equality is the contract) and its thinking-round-trip suite.

**pi (MIT)** has *"should not execute tool calls from a length-truncated assistant message"*: a stream
ending `stopReason: "length"` with a tool call whose arguments validate but are truncated, asserting nothing
executed, the end event carrying an error mentioning the output token limit, and the loop re-prompting.

**deepseek-harness (MIT)** has the system-prompt projection tests (the prompt is committed as a node in
history, an unchanged prompt is skipped rather than re-appended, a changed prompt on a cached route is
appended after cached history), the `content: ""` never `null` rule, and the cache-accounting split plus an
`it.each` over malformed-usage shapes that degrades gracefully rather than emitting a wrong total.

**opencode (MIT)** ports with care - right about mechanics, wrong about defaults. Worth porting: reasoning
details surviving the provider transform, omitting provider metadata when the assistant model differs,
pending/running tool calls converting to error results, and the overflow-headroom tests. **Read the
assertions, not the names** - three overflow tests are literally named `BUG: ...` and yet assert the
currently correct behaviour. Do not port anything keyed to vendor wire formats llama.cpp does not speak.

**oh-my-openagent is excluded** - Sustainable Use Licence, nothing copied, tests included.

**flowy (the operator's own; licence settled by D4)** has delivery tests that encode bugs this fleet has
paid for, and every one is a bug our per-room subscription (§12.2b) could reintroduce: a `--to-me` waiter
must still hear a person's unaddressed message; a pasted email address wakes nobody; a person naming another
agent must not wake you; mentions beat addressed when both are set; a note reaches the row's assignee and
nobody else, and on an unowned row wakes its raiser, with the guard that two empty strings must not compare
equal; presence windows derive from the waiter's own numbers; and an absent thread-standing serialises as
absent, not false. Port also the inversion from its author test: assert that the wire format carries
everything the persisted model carries **minus a named exclusion list**, computed by reflecting over the
type - one test, and the antidote to the whole enumerating-renderer bug class, where someone adds a field
and forgets the serializer and a message quietly loses its addressee. We want it for `TranscriptItem`, the
head protocol and the control channel.

### 14.2 Licence and attribution mechanics

Every surveyed repo carries its licence only in a root `LICENSE` file and none stamps SPDX or copyright
headers on individual test files, so attribution cannot be inherited by copying a file - it has to be added.
That forces **a suite-level `NOTICE`** listing, per source, the repo, upstream URL, the commit SHA read, the
licence and the files ported from (Apache-2.0 §4(d) requires carrying a `NOTICE` if the original has one),
and **a per-file header on every ported test**: `SPDX-License-Identifier`, the upstream path and line range,
the SHA, and one line saying what was changed - which is also how you find what a failing ported test
originally asserted. MIT and Apache-2.0 can sit in one repository and neither is copyleft, so neither
constrains the harness's own licence; D8 settled that as Apache-2.0. The recommendation that still stands is
to license the *conformance suite* Apache-2.0 in its own directory with its own `LICENSE` and `NOTICE`,
because it is the artifact whose value depends on other people running it against their own harness.

### 14.3 Which tests run WITHOUT our harness

These are black-box against a real llama.cpp endpoint plus any client, so the same numbers describe opencode,
our harness and any third-party candidate. Two mechanisms carry all of them: a recording proxy in front of
the server capturing every outgoing request body and every response keyed by turn, and `POST
/apply-template`, which renders any message array through the real template with no inference and no slot.

| # | check | how |
|---|---|---|
| C1 | prefix extension, exact | proxy: request N's prompt must be an exact prefix of N+1's, as `messages` for an unfused client and as the token array for ours. For our harness it holds by construction (§4.3); the test exists to measure other clients and to catch a ledger regression. |
| C2 | renderer fidelity | `/apply-template` on each fixture, exact string equality against our renderer (§7.2); the required CI gate. |
| C3 | generation-inclusive prefix | asserted exact: `H(prompt_N ‖ committed_generated_N)` must re-hash as a span of N+1's prompt; a mismatch is `Warning{prefix_divergence}` and the only defect in us (§18.1-I1). |
| C4 | `f_keep` distribution | `lcp / cached_entry` over a scripted 30-turn session, reporting p10/p50/p99 and p99 re-prefill (D11: not `cached/prompt`, which is `f_sim` under `f_keep`'s name). |
| C5 | reasoning replay | the omp scenario end to end: `cached_tokens` must not collapse on the turn that adds a prior assistant turn plus a synthetic nudge. |
| C6 | `content: ""` never `null` | proxy: on a tool-call-only assistant history turn, `content === ""`. |
| C7 | truncated tool args are not executed | `max_tokens` low enough to cut a tool call; the client refuses to execute and re-prompts. |
| C8 | `finish_reason` handling | `max_tokens: 1`; the client surfaces `length` rather than reporting success. |
| C9 | in-history system prompt | proxy: change the system text mid-session; message 0 is unchanged, a new system message appears at the tail, and `cached_tokens` stays high. |
| C10 | cache accounting is disjoint | reported "new input" tokens equal `prompt_tokens - cached_tokens`. |

The token-count form of C3, `cached_tokens(N+1) >= prompt_tokens(N) + committed_generated_tokens(N)`, is a
**measurement of the server** rather than an invariant: on a hybrid/recurrent model it undershoots by design,
because llama.cpp resumes from a context checkpoint and snaps `n_past` back to it, so a shortfall there is
not a violation and the check says so in its own words.

### 14.4 What no existing suite covers - we write these

Warm-prefix compaction (§18.1-I5, the summarizer's own `f_keep` >= 0.99); reading `prompt_progress` and
`timings` off the stream plus the idle-timeout interaction; adjudication (routing by class, escalation,
timeout defaults, first-answer-wins, late-answer rejection, audit completeness, auto-mode shadow agreement);
multi-head (late join via snapshot, resync on overflow, two heads at once, all heads detaching mid-turn,
restart keeping partial output); renderer conformance (the `/apply-template` diff plus `render_incremental ≡
render` and `parse ∘ render ≡ id`, over every dialect); and `EXPLAIN` correctness, where a deliberately
induced divergence must be named with the right item and the right cause.

## 15. What to vendor or port, and from where

Licences: grok-build **Apache-2.0**; omp, pi, deepseek-harness, opencode **MIT**; oh-my-openagent
**Sustainable Use, excluded entirely**.

From **grok-build**: the prefix-stability test plus its 12-test suite (§14.1) first, before harness code; the
`ConversationItem` shape with `Reasoning` as a sibling of `Assistant`; `LengthPolicy` with `empty_reason()`,
where empty length always fails; and the compaction assembly order - system, user-meta, project instructions,
last user query, recent verbatim, summary - with recent **before** summary. From **deepseek-harness**: the
in-history system prompt (compare against *latest*, append, and the normalizing fallback); warm summarizer
construction with the instruction appended last on the real live prefix; spill-don't-truncate with its
reserve arithmetic, head/tail split, notice wording, `0600` storage and best-effort fallback; the `content:
""` never `null` rule **and the comment** explaining why; and the README convention (§18.4). From **omp**:
incremental markdown lexing with the stable block boundary, tail-only re-lex and the guards, and
`append-only-context` as a reference - read it for the mechanism but do not copy the 32-bit digest (§4.3
supersedes it). From **opencode**: the Part union as the **Journal's** type (§4.1), never the transcript's,
plus SQLite schema shapes, tool bodies and the permission ladder as starting points (not the loop, not the
defaults). From **pi**: steering into a running loop, the shape §5.8 adopts.

**Deliberately not taken.** grok-build's transport layer: no `extra_body` anywhere, so `return_progress` and
`cache_prompt` cannot be sent at all; a closed `FinishReason` enum with no unknown fallback, so an unexpected
value fails chunk deserialization outright; and no unknown-field capture on stream deltas, so `timings` is
invisible. Those three are exactly what a local-first harness needs most and they are structural in that
codebase, whose tests and data model are the asset. Two rules adopted from those omissions as positive
requirements: every wire enum has an `Unknown(String)` fallback variant, and every streamed delta struct
captures unknown fields (`#[serde(flatten)] extra: Map<String, Value>`), so a field the server adds tomorrow
is visible today - softened by fusion, but still applying on the `/completion` fallback path (§5.6).

## 16. Language and runtime

The requirements, in order: byte-exact serialization control and cheap hashing; a long-lived daemon with
concurrent streams, backpressure and cancellation; fan-out to N heads without head-of-line blocking;
exhaustive handling of several sum types; SQLite; a good TUI; and the ability to port tests from Rust and
TypeScript.

Python is out for the daemon, because a long-lived multiplexing daemon under the GIL is the wrong shape and
the ecosystem pulls toward convenience exactly where this design needs canonical bytes, and stays right for
the suite. TypeScript is fastest to a working thing and the references are TypeScript, but `JSON.stringify`
key order is insertion-order dependent - a live hazard for canonical serialization and a discipline rather
than a guarantee. Go is an excellent daemon language whose one specific cost is the data model: it is almost
entirely sum types, and interface-plus-type-switch drops the exhaustiveness checking that protects "did the
renderer handle the reasoning variant".

Fusion changes the weights decisively: FFI to `libllama` for tokenization, a binary control protocol and
`memfd` shared memory with a `Vec<llama_token>` across a process boundary, and code that ships in one release
with a C++ server - a systems-programming shape. Rust is the fit for reasons that are specific: a private
constructor and a single `append` is how §4.1 becomes structural rather than a review rule; `serde` makes the
two silent failure modes explicit decisions; the crown jewel of the ported suite is already Rust; one static
binary for a daemon that must be restartable; `tokio` gives per-head bounded channels and `ratatui` is a real
TUI; sharing the vocab means our token ids are the server's by construction rather than by agreement; and
mapping a `memfd` as an append-only `&[llama_token]` across a process boundary is exactly what the aliasing
rules are for.

> **Daemon and TUI in Rust. Conformance suite in Python (pytest), deliberately, and it must not depend on the
> daemon.**

The hedge is load-bearing: because the suite is black-box and the HTTP surfaces stay, it measures the Rust
daemon, today's opencode and any candidate on the same axis, including a fused system against an unfused
baseline. The language decision stays reversible and, more importantly, **the fusion decision stays
measurable**.

## 17. Phasing

Each milestone is independently testable and independently useful, and each names the C-tests (§14.3) or
I-invariants (§18.1) that certify it. M0-M8 have largely landed; the server track has not.

### M0 - the conformance suite (no harness)

The recording proxy, the fixture corpus (§7.2) and the `/apply-template` diff harness, then C1-C10 run
against opencode to record the baseline. *Useful by itself*: it turns "opencode is wrong about caching" into
a number. *Exit*: a baseline table of `f_keep` p10/p50/p99, p99 re-prefill, and pass/fail on C1-C10.

### M1 - `harnessd` + one head + read-only tools

A headless daemon with one session against GLM, a TUI head and read-only tools only - no compaction, no
adjudication beyond `boundary`-for-reads, no subagents, no firecode - running entirely on the `/completion`
token-array fallback, with the GLM renderer written against M0's diff. It fixes on day one: reasoning is
simply present because we emit the tokens, the vector is append-only with the ledger, no output cap and
`finish_reason: length` acted on, incremental render, and a real progress display from `prompt_progress`.
*Exit*: C1-C10 pass against harnessd where opencode failed, and `f_keep` p10 >= 0.99 over a scripted 30-turn
session.

### M2 - firecode integration and write tools

Session-to-run mapping, `vm_in` as the exec path, files over the tar channel, spill-don't-truncate, and the
`boundary` adjudicator with the §11.3 policy table - write and exec tools become available because the
boundary now exists. *Exit*: the host project is not modified under our driving, a `bash` tool that deletes
everything harms nothing, and adjudication rows are recorded for every gated call even though almost none are
delivered anywhere.

### M3 - the flowy connector: chat first, decisions as a usage of it

Chat is the primitive, not adjudication. The connector holds a push subscription if flowy has one and the
degraded reader if not; an inbound message becomes a user item with no envelope (I11); outbound turn output
is posted; steering at step boundaries works; and decisions then ride the same channel using §12.3's text
protocol. It comes *before* M4 deliberately, because the async decision shape is the thing that cannot be
retrofitted. *Exit*: a real decision answered from a phone with the audit row to show for it, and the
`no_human_channel` degradation exercised by killing the connector mid-request.

### M3.5 - Falsifier B: quality against depth

Not a feature but one measurement, and §9's whole position rests on it: the same fixed task at 20k, 60k and
150k of preceding context on GLM, scored on answer quality rather than speed. *Exit*: a number, and a written
decision to keep or revise §9.1.

### M4 - leveled compaction

Segments, levels, opportunistic warm segment summaries, soft compaction by default, hard at the wall - high
in the phasing on purpose, because §9 concludes compaction is the primary quality lever rather than cost
control and it is what makes reasoning replay affordable. *Exit*: I5, and a session that crosses the
high-water mark without a stall longer than one cold prefill of the *compacted* prompt.

### M5 - multi-head

Attach/detach, snapshot plus resume, resync on overflow, two heads at once, all-detach mid-turn, the
partial-output-on-restart rule and the ACP adapter. *Exit*: the §14.4 multi-head suite, and a turn that
survives closing every client.

### M6 - subagents, concurrently

Spawned runs, per-role tool budgets enforced, abstention propagation and deliberately **concurrent**
dispatch - F6, the first time this box's scheduler is asked for more than one seat by a real client. *Exit*:
seats > 1 observed under real load in `/metrics` and `/slots`.

### M7 - the model adjudicator, in shadow

Auto mode built, run in shadow against the human for a fortnight with the agreement report, and promoted per
class rather than globally. *Exit*: an agreement number per action class, and a documented promotion
decision.

### M8 - the other two models

Probe, admit, run the dialect-parameterized suite. It can move earlier: it is gated only by M0's probe and by
a preset existing for the 27B dense model.

### Server track - same release, its own build loop

Fused means one system and one version number, not one schedule. These land as discrete server changes, each
measured once and then iterated from the daemon side; none blocks a harness milestone, because §5.6's
fallback always works. **None has landed.**

- **S0** - the control channel and the shm submit path (§3.3), the enabling change everything else rides on,
  landing between M1 and M2.
- **S1b** - `Spans`: accept `message_spans` and segment boundaries directly (§3.4.1).
- **S2b** - `Admit` / `Stats` (§3.4.4, §3.4.3), gated with M6.
- **S3b** - `SegmentHint`: semantic eviction (§3.4.2).
- **S4b** - `Prewarm` (§3.4.5).
- **S5b** - `Explain` (§3.4.6), which retires the `-lv 4` log scraping.
- **S1** - return the matched entry's `lcp` / `f_keep` / `f_sim` / id in the response `timings`: highest
  value, lowest risk, and build it before S6.
- **S2** - age-based demotion, a hard cap for actives plus a timer for idles, replacing an idle timeout that
  dumps everything including the conversation about to be touched.
- **S3** - run the drafted prompt-cache eviction-ladder tests, which have never been run.
- **S4** - block-hash / radix prompt-cache index with cross-sequence sharing (§3.10-A): refcount plus LRU
  among refcount-zero blocks at the resident-pool tier, with the degrade ladder staying at entry granularity.
- **S5** - the recompute-fraction experiment for stitching (§3.10-C), per layer class - a measurement that
  decides whether S7 exists at all.
- **S6** - the prefix-handle protocol (§3.10-B), argued on verifiability rather than speed, with the
  `required` mode built early because it is what makes the conformance suite loud.
- **S7** - stitching itself, only if S5 says the fraction is small.

### The smallest thing that beats today's opencode setup

**M0 + M1**, and neither needs a line of C++. M0 is an afternoon and converts opinion into a baseline. M1 is
a headless daemon with five read-only tools, one owned renderer and the token-array fallback, and on the day
it runs it removes all four measured defects: the cache stops growing without bound, the tail collapses from
78,598 to ~5,000 tokens of re-prefill, an empty subagent becomes a failure instead of a success, and a long
prefill shows a progress bar instead of a hang.

## 18. Test strategy

### 18.1 The invariants that must be mechanically enforced

**I1 - the generation-inclusive prefix invariant.** *This is the real one, and it is stronger than anything
in the survey:*

> Request N's prompt **plus the generated tokens the harness committed in turn N** must be a prefix of
> request N+1's prompt.

Grok's test pins only "request N is a prefix of request N+1", but llama.cpp caches prompt *and* generated
tokens, so the invariant that protects the cache includes the generation; a harness can satisfy Grok's test
and still diverge, and that divergence is exactly what the 612 GB was. The generation term is **committed**
generated tokens, not the server's `predicted`: a trailing stop token is a boundary the harness owns and is
stripped before commit, so with `predicted` every turn would warn by one, forever.

Two forms, and they are not the same kind of thing. **Exact** - hash turn N's submitted prompt together with
the committed generated tokens and re-hash that span of N+1's submitted prompt; a mismatch is the violation
and the only verdict here that is a defect in us. It runs post-flight on every turn, raising
`Warning{code: prefix_divergence}` and recording the verdict in `TurnMetrics.prefix_check`. Because the form
is a property of submitting token ids we rendered ourselves, the gate is the backend's
`may_assert_structural_prefix`: over a `messages` API the check is skipped loudly -
`Warning{code: prefix_check_skipped}` carrying the reason, `held()` still false - never silently green.
**Token-count** - `cached_tokens(N+1) >= prompt_tokens(N) + committed_generated_N`, every term from the
standard `usage` object; the shortfall sizes how far the server's reuse fell below what the invariant
permits, and raises `Warning{code: cache_reuse_shortfall}` whose text says it is a measurement of the server.

| id | invariant |
|---|---|
| I2 | **exact prefix extension of the token array** - the grok helper re-expressed over token ids through the real renderer and tokenizer. Under §4.3 it holds by construction; the test exists because "by construction" is a claim about code someone will edit. |
| I3 | **renderer fidelity** - our render equals the shipped jinja's, `/apply-template` over the whole corpus, exact equality, per dialect. Required CI check (§7.2), and it subsumes I2's template-side concern. |
| I3b | **`render_incremental ≡ render` and `parse ∘ render ≡ id`** - pure functions, property-tested with no model and no server (§18.3). |
| I4 | **the ledger is a chain** - a periodic full re-hash equals the incremental head; any inequality is a renderer non-determinism bug and fails loudly. |
| I5 | **compaction is warm** - the summarizer request's own `f_keep >= 0.99` and its prefill bounded by the instruction's own token count. The one number that distinguishes our compaction from everyone else's. |
| I6 | **the stable prefix is stable** - its fingerprint does not change within a session unless a declared event (model change, tool-set change) changed it, and such an event forks the transcript. |
| I7 | **`finish_reason` is never dropped** - every terminal value reaches `TurnFinished`, every `length` is classified per §5.7, and an empty `length` turn is always a failure. |
| I8 | **abstention does not become success** - a result with `outcome != Ok` cannot be reported upward as `Ok`, including across a subagent boundary. |
| I9 | **no adjudicator can override a `deny` row** - evaluated before dispatch. |
| I10 | **a head cannot block the model stream** - under an artificially stalled head, turn latency is unchanged and the head is resynced. |
| I11 | **an inbound chat message costs exactly its own tokens** - deliver n tokens into an idle session and the transcript grows by exactly the rendered length of one user item containing them, with no envelope and no listener bookkeeping. |
| I12 | **no polling anywhere** - a static check plus a runtime assertion: the daemon spawns no timer or poll loop for message delivery, and `no_human_channel` is raised by a *dropped subscription*, never by a missed poll. |

### 18.2 How they are measured against a real llama.cpp

| signal | where |
|---|---|
| `f_keep` per request | `lcp / cached_entry`; the token-count form is `usage.prompt_tokens_details.cached_tokens / usage.prompt_tokens` |
| p99 re-prefill | `prompt_tokens - cached_tokens`, distribution over a scripted session |
| prefill / decode rate, draft acceptance | response `timings`, per chunk under `timings_per_token` |
| live prefill progress | `prompt_progress {total, cache, processed, time_ms}` |
| which entry matched, and its `lcp` | `Explain{turn_id}` once S5b lands; until then the server log at `-lv 4` |
| box-level cache and queue state | `GET /metrics`, `GET /slots` |

**The scripted session** is the unit of measurement: a fixed 30-turn conversation with a fixed tool script,
replayed identically against any client, including a mid-session system-prompt change (C9), an oversized tool
result, a `length` truncation (C7/C8), a compaction crossing (I5) and a fork.

Traps this box has already paid for, which the test runner must respect: **a tok/s number without its
concurrency is not a number** (a contended run gave prefill 391.7 with decode 21.4, and the halved decode was
the tell, so every rate carries `n_busy_slots`); **two conversations cannot test residency**, so grep the
wording and not the clock; **a captured pid is a proxy** (read it from `ss -ltnp` at kill time); **`${X:-d}`
substitutes on empty as well as unset**; **a full model load looks like OOM** to anything watching `free`,
since GLM mmaps 199.7 GB; and **with `--no-op-offload` the CPU is on the inference path**, so a `-j 24` build
steals prefill and decode from production - build niced or in a quiet window.

### 18.3 Property tests worth having

Generate random transcripts (turn counts, reasoning presence, tool-call fan-out, images, forks) and assert
I2/I3b on every adjacent pair - pure functions, no server, no GPU, so this runs on every commit at full
volume and is where the exception surface gets exercised rather than assumed; **the biggest single testing win
of self-rendering**, since the invariant that used to need a live model is now a property of a function.
Generate random tool outputs around the spill threshold and assert `len(preview + notice) <= cap` exactly.
And generate adjudication policies and action classes and assert every action matches exactly one rule, or
the config is rejected at load.

### 18.4 Culture, not code

Adopt the README convention verbatim, per prompt-contributing feature: **"What the model sees"** (the literal
text), **"Token effect"**, **"KV Cache effect"** (does this vary per request, per process, or never). It is
what stops the `<env>` regression from happening twice.

## 19. Future directions, and what they demand of v1

Not v1 features, but each is expensive to retrofit, so each constrains the v1 data model. All three want the
same thing: **context as addressable segments, not a flat message list** - which is why `SegmentMark` (§4.2)
and the `segment` table (§4.4) are in v1 despite doing nothing in v1.

### 19.1 Multi-head compaction with different decay rates

Several compaction heads over one conversation, each decaying at its own rate: a thread the operator is
working stays fine-grained while an abandoned one coarsens fast.

- **L1.** Compaction must never be a whole-history operation; a single `summarize(history)` entry point in v1
  would slam this door, because everything downstream would assume one summary.
- **L2.** A segment carries its own `level` and `last_touched`, and `last_touched` is what promotes or demotes
  it, not `created_at` - that is the whole of "different decay rates", falling out of the level rule rather
  than being a separate feature, and if the futures never arrive it costs two columns and a zero-width item.
- **L3.** Summaries are stored per segment in the journal, not folded irreversibly into the transcript, so a
  segment can be re-levelled without re-summarising from scratch.

Free to revisit later: the number of levels, the ratios, the promotion policy, and whether a "head" is a named
view or just the current level assignment - start with the latter. Shared policy with the server's cache
ladder? No (§10.5).

### 19.2 User-assisted compaction

*"These are the topics I have; which do you most want preserved?"*

- **L4.** Compaction is interruptible and can ask a human mid-operation over the same channel as adjudication;
  §11.6 already makes questions and permissions one mechanism, explicitly so that this needs no second
  channel built later.
- **L5.** **A segment is labelled at creation, not reconstructed later** - a data-model requirement, not a UI
  one, and the awkward one to retrofit, because reconstructing topic labels for 200 old segments means running
  a model over the whole history, which is the stop-the-world pass §10 exists to abolish. Without a model
  call: a segment opens at a user boundary, and its label is the first ~80 characters of the user message that
  opened it plus the tools used within it. A model-generated label can replace it later, and because the label
  renders to nothing, replacing it costs no prefix.

Free to revisit later: the interaction shape, and whether the answer is advisory or binding.

### 19.3 Agentic memory - "infinite memory via explicit search and attention segments that are swappable"

**The cost model, stated honestly.** Swapping a segment back in mid-conversation rewrites the prefix from that
point: ~5,000 tokens re-prefilled and ~11 s in the last 5k, ~75,000 and ~170 s halfway through a 150k
conversation, ~200,000 and ~453 s at the head of a 200k one. Swappable memory is **not free**, it is linear in
the suffix, and any design that presents it otherwise is wrong; that is the physics of §0.1 and no cache index
changes it. The three ways out are §3.10-C's: position shifting (measured unsafe on GLM's recurrent
components), stitching, or paying.

**So the recommendation is: append, do not swap.** A retrieved memory is *appended* as a new item - free,
prefix-preserving, costing occupancy rather than prefill. Swapping is the optimisation you reach for when
occupancy actually binds, and by then §10's levels have reclaimed most of it.

- **L6.** Segments are **addressable and independently serialisable**: a segment has an id, its items are
  recoverable by that id, and its rendered bytes are a contiguous span in the ledger. Without contiguity
  stitching has nothing to address, so this is the one v1 decision a later server-side change depends on - and
  it costs nothing today because the ledger is already a span map.
- **L7.** Nothing in the prompt pipeline may assume the transcript is exactly what the session contains; the
  pipeline takes an *ordered selection* of items, and in v1 that selection is always "all of them", but the
  seam exists.

What would slam the door: rendering items directly from a message array without a span map, or letting
compaction rewrite items in place instead of forking (§5.5).

Memory is **built on the oracle pattern** - a searchable corpus behind a tool - with one harness-side
addition. It keeps the harness out of retrieval (§1, §9.4), it is prefix-friendly by construction, and it
composes with §8.2, since a memory search that finds nothing *abstains* structurally. The addition oracle
cannot do is **segment-to-corpus ingestion**: when a segment is demoted to L2 or below, its verbatim text is
written to a per-session corpus behind a `search_session_memory` tool, so the coarse summary stays in the
prompt and the detail stays retrievable. That is what makes fidelity amplification survivable rather than
lossy, and it is a genuine answer to "infinite memory" that needs no swapping at all.

Where this disagrees with the brief, flagged rather than smoothed: the brief says it is acceptable that
agentic memory fights append-only prompts. It is acceptable *as a research direction*, and it is costed above.
But segment swapping should not be built into v1 or v2 on this stack, because decode is flat against depth
(F2), the pool is elastic, and the L2 tier makes a cold conversation cheap to bring back whole. The cheap move
here is **stay deep and stay warm**, not **swap**; if Falsifier B shows quality collapsing with depth well
before the window, that inverts.

## 20. Risks and open questions

### 20.1 Unverified, with the procedure that settles each

| id | question | settled by |
|---|---|---|
| UNVERIFIED-1, -2 | *dissolved, not answered*: under self-rendering there is no template to re-render `reasoning_content` into `<think>` and no mid-history system message for a template to drop. What replaces them is behavioural - was this model trained to attend to a system turn arriving mid-conversation - with §5.3's envelope as the conservative default until it exists | an eval per model |
| UNVERIFIED-2b | does our renderer match the shipped jinja for every fixture? | §7.2's CI diff, continuously |
| UNVERIFIED-3 | should a firecode checkpoint and the conversation's KV state be checkpointed together? The sizes are mismatched and nobody has measured whether the KV half is still resident when the VM half is restored. Do not build for it in v1 | measurement |
| UNVERIFIED-4 | opencode's `OUTPUT_TOKEN_MAX = 32_000`: a repo-wide search found no hardcoded 32k output default, every occurrence being a test fixture. The behavioural finding is independently supported | check the exact tag before repeating the claim |
| UNVERIFIED-5 | can the chat-completions path return generated token ids? No longer a dependency of I1, whose exact form hashes what we rendered and committed | one call |
| UNVERIFIED-6 | does `/apply-template` behave identically under the model router? It is proxied there and may trigger a load; the workaround is to run the fidelity gate against a directly launched single-model server | one call with a stopwatch |
| UNVERIFIED-7 | which model for auto mode - a recommendation from availability and latency predictability, not a measurement | M7's shadow mode |
| UNVERIFIED-8 | the 27B dense model's dialect: predicted Qwen thinking format, unconfirmed | the §7.2 diff |
| UNVERIFIED-9, -10 | is flowy's stream cursor-resumable, and can two flowy clients drive one session? Prior art only; §13 does not depend on the second | ask, do not guess |
| UNVERIFIED-11, -12 | the per-call cost of the exec channel and the throughput of the file channel; a `bash` tool paying a connection setup per call may or may not be a per-call tax that matters | measure before designing around it |
| UNVERIFIED-13 | is `--cache-reuse` actually inert on the live build? Two documents disagree, and a fork is exactly the mid-prompt divergence it acts on | grep the startup log for the refusal line |
| UNVERIFIED-14 | GLM's depth-collapse guard: it does not reproduce to 147k, and 1M has never been run. Build the guard regardless, but do not read "does not reproduce" as "cannot happen" | build the guard; probe higher |
| UNVERIFIED-15 | is per-turn tokenization a measurable share of warm-path latency? | wall clock to first token minus `timings.prompt_ms + predicted_ms`, at three depths |
| UNVERIFIED-16 | can stitching work at all on GLM's 34 recurrent KDA blocks? **The highest-value unanswered question in the plan**, because it decides whether §19.3's recommendation against swapping is permanent | §3.10-C's experiment, per layer class |
| UNVERIFIED-17 | is more than one named reader per principal intended in flowy? It decides §12.2 ask 5 | ask whoever owns flowy |
| UNVERIFIED-18 | how should a streaming listener be classified by `waiter_kind`? A listener misreported as absent would silently shorten every human timeout | unsettled in the source |
| UNVERIFIED-19 | flowy's licence | settled by D4 |

### 20.2 Risks

- **No cloud fallback, and the edge would have to be rewritten to get one.** Deleting the provider
  abstraction means a later "run this turn on a frontier model" is not a config change: the renderer, the
  tokenizer, the submit path and the fused control vocabulary have no cloud equivalent, and the append-only
  token vector has no meaning against an API that tokenizes server-side. The mitigation is not an abstraction
  layer but D10's `crates/backend` seam.
- **Fusion couples us to llama.cpp's internals** (§3.5), and the interface that cannot be ported is the fused
  control vocabulary - losing it means going back to being a normal client.
- **Self-rendering shifts template drift onto us.** §7.2's CI diff is the entire mitigation, and if that check
  is ever made advisory this becomes the most dangerous risk in the plan: silent failure, symptom a cache leak
  in hundreds of gigabytes.
- **A renderer could diverge from a shipped template we cannot match**, and the honest responses are to fix
  the renderer or patch the template, not a compat quirk that papers over a divergence.
- **Rust is slower to write than the schedule wants**, mitigated by the decision being reversible: the suite
  is language-independent, so switching costs the daemon and not the measurements.
- **Building on this box steals inference**, so a Rust workspace this size is a real `-j 24` build: nice it,
  use a quiet window, or build in a run.
- **flowy is a moving target and is not ours**, mitigated by §12.3's degraded mode: no milestone may block on
  a flowy change.
- **Auto mode drifting** is the single most dangerous new component, because a model adjudicator that slowly
  loosens is precisely F5 - mitigated by shadow mode running forever on a traffic sample.
- **Segments turning out to be the wrong abstraction** costs two columns and a zero-width item, and all three
  §19 futures need them.
- **The harness finally exercising concurrency could destabilise the server**: M6 is the first real concurrent
  client this box has had, which is why it observes `/metrics` and `/slots` rather than only passing or
  failing.

### 20.3 Corrections to the brief and the survey

Flagged disagreements rather than smoothed over. The two that still matter: **the supersede rule is
containment, not "exact full-prefix match"** (the cache removes any cached prompt fully contained in the
current one, which strengthens F1a), and **line numbers in the surveyed repositories drift**, so a citation is
worth re-checking against the checkout before it is repeated.

### 20.4 What I could not settle at all

- **Whether §9's position is right** - a judgement built on two measurements pointing in opposite directions,
  with §9.3's three experiments as the way to settle it and Falsifier B load-bearing.
- **Whether the volume of human adjudication will be tolerable.** The policy table (§11.3) is a first guess,
  configuration precisely because it will be wrong, and §6 is where being wrong shows up.
- **Whether one daemon is enough** for "one deep orchestrator at up to 1M tokens plus a dozen to thirty shallow
  agent conversations." Nothing forbids it, but nothing has been sized for thirty concurrent sessions either,
  and the server-side arithmetic says the binding constraint is a third GPU, not the harness. Revisit at M6.
