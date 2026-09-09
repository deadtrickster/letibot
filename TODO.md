# TODO

Open work, ordered by what blocks what. Each entry says what is undecided, what was
already measured, and what a decision would change — so it can be picked up cold.

Background for anything template-related: `docs/chat-templates.md`.

---

## T1 — How templates are rendered — **SETTLED 2026-09-09: template-driven, minijinja**

Verdict and evidence: `experiments/minijinja-fidelity/RESULTS.md`. Decision recorded
as D11.

**Adopt the template-driven design** on `minijinja` 2.24 with `loop_controls`,
`preserve_order` and the `pycompat` shim, and **keep the CPython differential as a
permanent CI gate** — that last part is not belt-and-braces, see below.

Measured: **4,132 byte-exact renders** across GLM and Qwen templates over the
139-case fixture corpus, an 18-case Qwen-native corpus, edge cases and 2,200
generated conversations; 588 cases where both engines refused identically; **zero**
where one rendered and the other refused. Sentinel provenance works unchanged and
its region maps are **identical to the authority's, 0 differences in 4,132
comparisons**, with 3,617 payload-supplied control-token literals all classified
DATA and none straddling a boundary.

**minijinja does not have minja's scoping bug** — a bare `{% set %}` is per-iteration,
matching CPython. That was the near-decisive check and it passed.

**Why the differential gate stays forever.** Reaching byte-exact took eight
configuration decisions, **four of which fail silently** and three of those were
invisible without diffing the two engines: `serde_json` separators, `BTreeMap` key
sorting without `preserve_order`, Rust's `Display` for `f64` never using exponent
notation, and three Jinja `is` tests answering Python's way in Jinja2 and Rust's way
in minijinja — of which `none is iterable` is reachable, because both templates
branch on it.

The framing that explains all four: **minijinja implements Jinja the language
correctly; `transformers` runs Jinja on top of the Python object model.** `.strip()`,
`.startswith()`, `.items()` are Python, not Jinja. Every silent divergence lives in
that seam. The risk is not that minijinja is wrong today — it is that a new template
reaches an unexercised corner of the shim.

**Known, characterised, unfixed:** integers outside `i64`/`u64` render as floats.
`minijinja::Value` has no bignum and `serde_json`'s `arbitrary_precision` does not
fix it — precision is lost in the conversion into minijinja's own value type. Kept
measured by `cases-edge`.

**Cost:** 7–15 µs to render a 51 KB prompt, 14–30 µs for the double render provenance
needs, against prefill in hundreds of milliseconds. 18 crates, no C, no Python.

**The biggest untested risk**, and the most likely way a third model breaks this:
`{% generation %}`. `transformers` registers an `AssistantTracker` extension for it,
and **minijinja 2.24 has no public custom-tag API**, so a template shipping that tag
would fail to parse with no shim available. Neither of our two templates uses it.

---

## T2 — Revise the `Dialect` contract — **UNBLOCKED by T1, re-scored**

Under the template-driven design, four of the eight defects stop existing or move.
Re-scored by the T1 experiment:

| # | defect | fate under T1 |
|---|---|---|
| 1 | `render_incremental` cannot be pure | **gone** — Jinja has no incremental mode, so there is no boundary state to carry |
| 3 | no home for the generation prompt | **gone** — it is a template argument |
| 5 | resolution must key on literal not role | **gone** — rendering no longer asks for roles |
| 4 | `ControlRole` closed and too small | **moves** to `parse`'s problem |
| 7 | `ControlTokens` forces `&'static str` | **now mandatory** — a runtime-loaded template makes `&'static` impossible |
| 2 | `parse(&[u32])` unimplementable | survives, `parse` side |
| 6 | `stop_tokens()` bare literals | survives, `parse` side |
| 8 | `ControlRole` has no `Ord` | survives, trivial |

So `Dialect` splits: **rendering becomes data** (template source, control-token set,
quirks) and **parsing stays code**. The three survivors are all on the parsing side,
which T1 does not touch.

**One thing gets harder and needs deciding.** The `server-bug-compatible` profile
cannot be produced by a template-driven renderer without deliberately reintroducing
minja's bug, and minijinja has no knob for it. My call: **drop it.** Its purpose was
to prove we understood the template well enough to reproduce minja exactly, which
earned the `faithful` profile the right to declare divergences. Running the real
template through a correct engine is stronger evidence than reproducing a wrong one,
and the INTEROP phase against `/apply-template` still reports how the server differs.

1. **`render_incremental(prev_end, new_items)` cannot be a pure function.** GLM needs
   boundary state — turn open, `<think>` open, previous item a tool result — none of
   it derivable from a `usize`. Worked around with `GlmDialect::for_conversation`;
   `new()` panics rather than guessing. The signature must carry history or a state
   token.
2. **`parse(&[u32])` is unimplementable as specified** — no vocab, yet it must return
   `Content(String)`. Currently takes a caller-supplied `TokenDecoder`.
3. **No home for the generation prompt.** Folding `<|assistant|><think>` into
   `render` breaks the stated invariant for any conversation whose next item is a
   user message. Currently an inherent `generation_prompt()`.
4. **`ControlRole` is closed and too small** — no `<arg_key>`, `<arg_value>`,
   `<sop>`, or the image triplet, all single vocab entries, so all must be `Control`.
   They currently sit under `TurnEnd` as a "no role" bucket.
5. **Resolution must key on literal, not role.** One role with many literals is the
   case that bites, and `ControlTokens::get(role)` silently returns whichever comes
   first. `GLM_TOKENS` is ordered so the canonical entry wins — a convention, not a
   guarantee.
6. **`stop_tokens()` returns bare `&'static str`** with no role, so a failure cannot
   be reported honestly. Correctness argument: a stop token that is silently a
   *sequence* never fires, and the turn runs to `n_ctx`.
7. **`ControlTokens` as `&'static [ControlToken]`** forces `&'static str` through
   every error type. Fine while dialects are compile-time constants; impossible for
   a dialect loaded from a config file or a downloaded template. `Cow<'static, str>`
   costs nothing today.
8. **`ControlRole` has no `Ord`**, so deterministic error listings must preserve
   declaration order rather than sort. Trivial; a derive would do.

---

## T3 — Report the minja scoping bug upstream — **ready, not filed**

`{% set %}` inside `{% for %}` is not scoped per iteration in llama.cpp's minja.
Repro needs no GPU: a template, four messages, one `/apply-template` call.
Full writeup and both engines' output in `docs/chat-templates.md` §2.

Affects any llama.cpp user serving GLM with `messages` — the model is conditioned on
reasoning it never produced. We are insulated because we submit token ids.

---

## T4 — Verify what opencode actually sends — **cheap, decides whether T3 affects it**

`docs/chat-templates.md` §3 lists two ways a client causes prompt-cache divergence.
The second — sending reasoning as its own message rather than fused onto the
assistant message — applies only if opencode does that. Unverified.

The W1 recording proxy answers it directly. Decides whether the residual
divergence after the `interleaved` fix is one mechanism or two.

---

## T7 — Falsifier B — **RUN 2026-09-09. The lazy-compaction assumption SURVIVES.**

Full write-up `experiments/falsifier-b/RESULTS.md`, raw per-sample data in
`raw/samples.jsonl`. 160 samples, 40 per depth, scored objectively on
compiles-and-tests-pass, first attempt, no retries.

| depth | measured tokens | pass rate | 95% CI |
|---|---|---|---|
| 0k (control) | 0 | **0.829** | [0.67, 0.92] |
| 20k | 19,190 | 0.875 | [0.74, 0.95] |
| 60k | 60,046 | **1.000** | [0.91, 1.00] |
| 150k | 149,713 | 0.900 | [0.77, 0.96] |

**20k vs 150k: Fisher exact two-sided p = 1.000.** No monotone trend, no cliff. The
only significant pair is 0k vs 60k (p = 0.008) **and its sign is backwards** — the
deeper condition did better. No task degrades, no position effect, no drift across
reps.

Cache evidence, which is what makes this a quality measurement rather than a prefill
one: at 150k the first request prefilled 149,968 tokens in 144.6 s and every later
one reused 149,715 cached against 391 new in **0.8 s**, ~180× faster.

### The two findings nobody predicted

1. **The 0k control is the worst cell, not the best.** Depth did not hurt; the
   *absence* of a conversation did.
2. **An empty context makes this model reason ~4× longer** — median 6,468 generated
   tokens at 0k against ~1,400 at every real depth, on identical tasks. A long prior
   conversation *anchors* it rather than distracting it. All 5 of 160 runs lost to
   `finish_reason: length` were at 0k; zero at every real depth.

Finding 2 is an argument against aggressive compaction that the plan does not
currently make: **compaction risks paying a cold prefill *and* re-entering the
free-running regime.** It does not merely cost time, it may cost tokens on the far
side too.

### What this settles

§9.1 clause 4 stands. Locally the trigger policy is "compact when you must, as late
as possible", and **§10's leveled design earns its keep only at the context wall and
under KV pressure — not on quality.** That materially reduces what W13 must do for
M1. The metered branch of T9 is untouched: that trade-off is money, not quality.

### Limits — stated because this will be quoted

Tasks are self-contained and the filler deliberately irrelevant, so this measures
whether depth degrades **fresh reasoning**. It does **not** measure whether the model
can still use something stated at turn 3 when it is at turn 200. **That failure mode
is untested and needs its own falsifier.** n=40 per depth can see a collapse but
cannot resolve a 5-point slide. 150k–262k is unmeasured. One model.

### Deviations from the brief, both deliberate and both flagged

- `max_tokens` 16,000 rather than 6,000 — at 6,000 the model truncated far too often.
- One worker per depth on its own pinned slot (concurrency 4) rather than fully
  sequential, which projected to ~4 hours. Running all depths over the same
  wall-clock window *eliminates* the server-drift confound rather than merely
  interleaving against it. Cost: timings are not per-request throughput, and the 0k
  worker finished last and ran largely alone, so its 97 tok/s against ~32 is GPU
  availability, not depth.

---

## T10 — Contract gaps found by W6 — **small, concrete, one blocks a §5.8 feature**

The turn engine is the first real consumer of the crates below, and it found five
things. Listed in the order I would fix them.

1. **`TranscriptItem::Assistant` has no `truncated` field**, and both §5.7 and §5.8
   require one. Currently tracked on the turn record instead, which is why **one
   piece of steering is unbuilt**. Needs a `transcript` crate change — the only item
   here that blocks a feature rather than costing elegance.
2. **`ParsedSpan` carries no token offsets**, so a `Parser` cannot say which ids an
   item owns. Cost an entire module (`crates/turn/src/items.rs`) to work around
   without reimplementing the parser. Adding a span to `ParsedSpan` would delete it.
3. **`DialectSpec` has no `ReasoningField`** — a per-model fact of exactly the kind
   that crate exists to model as data. The engine takes it as config rather than
   guessing, which works but puts a model fact in the wrong place.
4. **`render_incremental(history, new)` replays the whole history** for boundary
   state, so building one ledger row per item is O(n²) replays. Microseconds today
   at our sizes; a resumable state token fixes it. Note T1 may delete this entirely
   when the template-driven renderer lands.
5. **`cargo:rustc-link-arg` does not propagate across crates**, so every crate that
   links `libllama` needs its own `build.rs` to bake the rpath. Without it, test
   binaries link fine and fail at exec looking like a missing library. Fixed in
   `crates/turn`; worth a note so the third crate does not rediscover it.

---

## T11 — §18.1-I1's observable form is not checkable on a hybrid model — **resolved in code, plan text still wrong**

§18.1-I1 states the prefix invariant observably as
`cached_tokens(N+1) >= prompt_tokens(N) + predicted_tokens(N)`. On this box it
**cannot pass**, and not because the invariant is violated.

Qwen3-Next is hybrid/recurrent, so llama.cpp resumes from a **context checkpoint**
and snaps `n_past` back to it (`server-context.cpp:5910`). Measured against prompts
*proven* identical over the shared span: reuse 48 of 52, and 38 of 44. The server is
reusing less than it could, correctly, for reasons of its own memory model.

W6's resolution, which I agree with: make the **exact** form the assertion — hash
turn N's prompt plus its committed generation, re-hash that span of N+1's prompt —
and demote the observable number to a *measurement of the server*, with a warning
that says which it is.

**Second defect in the same sentence:** `predicted_tokens(N)` is the wrong term for
a harness that owns its turn boundaries. A trailing stop token is stripped before
commit — keeping GLM's emitted `<|user|>` would put a second one in the next prompt —
so the witness must record **committed** generated tokens. With `predicted`, every
turn warns by one, forever.

Both need fixing in `docs/implementation-plan.md` §18.1. The code is already right.

---

## T12 — The live view and the stored view disagree about channels — **real defect in `crates/turn`, found by a real head**

Measured against Qwen3-Next by W8's live end-to-end test. **Not corruption** — the
ledger is token ids and the parser is authoritative — but for the length of every
turn a head shows the model's *reasoning* as its answer, and then the transcript row
replaces it. That is exactly the class of defect §13.2b exists to prevent.

Two causes, both in the engine:

1. **`in_reasoning` starts `false`** (`crates/turn/src/engine.rs`) and is only set by
   a **generated** `ThinkOpen`. But the generation prompt *ends inside* `<think>` —
   the engine's own module diagram says so — so the model is reasoning from token one
   and every head is told it is assistant text.
2. **`Chunk::Token{text}` is emitted as a `Delta` before the ids in that chunk have
   their roles inspected**, so `</think>` reaches every head as visible characters.

W8 did not fix this, correctly — it is W6's semantics. `crates/sessionlog/tests/live_e2e.rs`
asserts the weakest form that passes today **plus a tripwire that fires when the
engine is fixed**, so it cannot be fixed quietly and the test tightened afterwards.

Fix in the engine: seed `in_reasoning` from the dialect's generation prompt rather
than from a generated token, and inspect roles before emitting the `Delta`.

---

## T13 — §4.5's event enum is insufficient for a real head — **five gaps; gap 1 is a priority, see T14**

Found by building one, then by fixing T12. Listed with what was done about each.

**Gap 1 is not a footnote.** T14 argues that an addressable record is the fallback if
composable KV proves impossible on hybrid models — and a log whose transcript events
carry no content cannot be that record. Treat it as blocking for the "sessions that
do not forget" goal, not as a schema nicety.

1. **`TranscriptAppended{item_id, kind, ledger_head}` carries no content**, and
   `EventSink` has no channel for it — so **a head cannot reconstruct a conversation
   from the log at all**. The daemon must reconcile items into the view out of band
   (`Hub::record_item`). W8 kept the event exactly as specified and made the gap
   explicit rather than widening it unilaterally. This is the structural one and it
   needs a decision: either the event carries content, or the log is formally not a
   sufficient record of a session.
2. **`TurnFinished{usage, timings}` is lossy against `turn_metrics`.** Not carried:
   `prefix_check`, `id_slot`, `n_busy_slots`, `cost`, `dialect_template_sha` — and
   §18.2 says **`id_slot` is exactly what distinguishes a scheduling fact from a
   prefix divergence**, so a head cannot today tell those apart. W8 widened `usage`
   to carry `cached_tokens` on the argument that a harness whose point is cache reuse
   must be able to display it.
3. **No event announces who issued a command**, which §13.2 requires twice. Added as
   `CommandIssued`, labelled as an addition rather than smuggled into `Warning`.
4. `ToolStarted` / `ToolProgress` are given by name only in §4.5; their shapes are
   W8's invention and **W9 will find out whether they are right**.
5. **`DeltaTarget` is `Text | Reasoning` — there is no channel for a tool call.**
   Found while fixing T12. A tool call's argument text streams as `Text` while
   `items::produce` puts it in `ToolCall{arguments}` and excludes it from the
   Assistant row, so T12's new per-channel equality assertion **would fail on a
   tool-calling turn — correctly**, as a real live/stored disagreement rather than a
   test defect. No test exercises it yet because `live_e2e`'s prompt makes no calls.
   This is T12's defect, unfixed, in a third channel. The enum was not widened
   unilaterally.

---

## T14 — Composable KV — **SETTLED NEGATIVE 2026-09-09. Prefix reuse is the ceiling.**

`experiments/kv-stitching/RESULTS.md`. Not achievable beyond prefix reuse on this
stack, and not for the reason expected: a recurrent memory's cells *are* sequence ids,
so 36 of 48 layers hold one fixed 111.4 MiB accumulator per sequence with no per-token
structure. A block's contribution there is not extractable, so partial recompute is not
an operation. Recompute fraction 1.0 of everything after the divergence; recomputing
98% of a prompt still diverges at token 5.

**Active memory survives** because it was decoupled first — see `docs/memory.md` §4 and
`docs/compaction.md` §5. What died is the optimisation, not the feature.

The original analysis is kept below because its framing of the two obstacles is what
made the experiment answerable, and because obstacle 1 turned out never to bind.

---

### Original entry


The plan analyses this well (§3.10-C, UNVERIFIED-16) but files it as an open question.
The operator has asked for it twice and it is a **design goal**: a request should be
able to send a *composition* — `[block_hash, block_hash, text, block_hash]` — rather
than a full prompt, and blocks should be reusable **across sessions and across
agents**, not only as a prefix of one conversation.

### What it is FOR — and why "infinite sessions" is the strong claim, not the weak one

I first framed unbounded sessions as the weaker half, on the grounds that attention
still costs per resident token so selection does not go away. That framing was wrong,
and the operator's counter-example is **this session**.

The PFN/stroppy work was done here, in this conversation, and was lost across
compaction. Asked about it directly, I said it was not in context and not in the
summary; the only reason it was recovered is that the operator **said it was in the
transcript**. Without that, the answer would have been "we did not do that here" —
about work this same session produced.

**The asymmetry that makes composition different in kind, not merely cheaper:**

| | compaction | composition |
|---|---|---|
| what it does to detail | **destroys** it | **omits** it |
| can a later turn recover it | no, it is gone | yes, the block is still addressable |
| a selection mistake is | permanent | a turn's choice, revisable next turn |

That is the point. Selection does not disappear, but it stops being a one-way door.

**And the current medium indicts itself.** The session transcript is 77 MB of
append-only jsonl: greppable and nothing else. No index, no addressing, and — the
actual failure — **no way to know what is in it without already knowing what to
search for.** Recovering the PFN work required guessing that the word "stroppy" would
appear; "pfn" alone returns 1,579 hits of noise.

**`crates/sessionlog` currently repeats this mistake**, which is worth saying plainly
about something we built today: it is an append-only log of text events, and T13
records that `TranscriptAppended` carries no content at all, so it is not even a
sufficient record of a session. See T13 — that gap is not a footnote, it is the same
disease one layer up.

So the fallback, if the recurrent layers say stitching cannot work, is **not**
"compaction, oh well". It is that the *record* must be addressable even when the KV
cannot be.

### Two obstacles, and only one of them has a known answer

**1. Position dependence (RoPE).** The same text at position 5,000 and 50,000 yields
different KV, which is why vLLM's block hash chains the parent and why reuse is
prefix-only everywhere in production today.

arXiv 2608.03893 (*Cross-Model KV Cache Transfer in LLM Families*, Heo et al.) does
the relevant trick as step 2 of its method: **strip RoPE from the keys to make them
position-independent**, map, re-apply. Its own purpose is cross-model transfer within
a family and its accuracy retention (73–98%) is far too lossy for us — but the
position-independence technique is separable from the transfer claim, and it is the
piece composable KV needs.

This also corrects the design brief: it says KV pages "are not freely relocatable the
way database pages are" as a hard property of attention. It is a property of how the
KV is *stored*, not of attention.

**2. Cross-attention context dependence — and on our models this is the binding
constraint.** A block's KV is not a pure function of its own tokens; it depends on
what preceded it. CacheBlend's answer is to recompute a fraction to reconcile.

**But CacheBlend's premise assumes attention-only layers, and both our models are
hybrid.** GLM is 12 MLA attention blocks and **34 KDA recurrent blocks**; Qwen3-Next
is hybrid too — W6 measured llama.cpp resuming it from a context checkpoint and
snapping `n_past` back. In a recurrent layer the state at position *n* is a function
of all *n* tokens, so "recompute the last *k*" is not a partial correction: it is
either exact, because you re-ran from the divergence, or it is wrong.

If that holds, stitching applies to roughly a quarter of GLM and the recompute
fraction is not "a few percent" but "everything after the divergence, for 34 of 46
blocks" — and the economics collapse.

**So the paper solves the obstacle we do not have, and is silent on the one we do.**

### What composable KV is ultimately for: active memory

See `docs/memory.md`. Passive memory is a notebook — you must know to look. Active
memory arrives unbidden because it is relevant. The transcript is passive and it
failed exactly that way this session.

The link to this item is economic: **without composition, surfacing a block costs a
full re-prefill** — 144.6 s against 0.8 s for a prefix hit, measured here. That cost
is why active memory is not standard: you would only dare fire it when already
confident, which is when you did not need it. With composition a miss costs context
rather than minutes, and being liberal becomes affordable.

**Revised 2026-09-09: composable KV is no longer a prerequisite for active memory.**
A suggestion appended to the *user turn* leaves the prefix untouched, so the
re-prefill cost never arises; composition becomes an optimisation of the `recall`
pull rather than a gate on the feature. See `docs/memory.md` §4. What composable KV
still buys is that the pull becomes nearly free rather than a tool call.

It is also a **fused-backend** feature and cannot be built on either side alone: the
server holds the blocks and has no idea what is relevant; the daemon knows relevance
and cannot place a block without re-prefilling it. That is a stronger argument for
fusion than anything in §3.4, where the unlocks are optimisations rather than
capabilities that do not otherwise exist.

### What settles it — UNVERIFIED-16, the experiment already specified

§3.10-C states the method. Build `P = A ++ B ++ C`, take the reference KV by full
prefill, then assemble from a cached `A`, a cached `C` taken from a **different
position**, and a recompute of the first *k* tokens of `C` at its new position. Vary
*k*. Measure exact agreement of 200 greedy continuation tokens, and run it **per
layer class** — the whole question is whether the recurrent blocks behave differently
from the attention ones.

Qwen3-Next is serving and is hybrid, so this is runnable today on the model we
actually use, not only on GLM.

**Why this is the right next experiment:** it decides whether composable KV is
achievable on this stack at all, and §19.3's recommendation against model swapping is
permanent or provisional depending on the answer. Everything else in the composable-KV
direction is unfundable until it is answered.

---

## T15 — Replace §10's compaction design with structural eviction — **design written, not scheduled**

`docs/compaction.md`. Compaction today bundles *reducing what is resident* (necessary)
with *destroying what is recoverable* (an accident). Falsifier B removed the usual
justification — quality does not degrade with depth — so the trigger is the context
wall and memory pressure only.

The key constraint that rules out the easy fix: **plain eviction invalidates the
prefix just as summarisation does**, because the prefix *is* the old turns. Both cost
a re-prefill, so the question is what to get for it. Answer: replace an evicted span
with a **structured, addressable map** rather than prose, maintained incrementally so
there is no stop-the-world summarisation call, with zoom-in appended via `recall`
rather than rewritten in place.

Depends on nothing. Feeds `docs/memory.md`. Gives `SegmentMark` its missing producer.

---

## T16 — W9's open questions — **several want an operator or a strand owner, not me**

From the tool runtime, in descending order of consequence.

1. **§8.1 clause 3 gives the outcome vocabulary but never the mapping.** Which outcome
   is an empty `grep`? W9 decided: matches in scope or elsewhere ⇒ `Ok`; nothing
   anywhere under any relaxation ⇒ `Abstained`; a nonexistent path ⇒ `Failed`. That
   choice determines how often `propagate()` blocks a caller and should be recorded
   rather than inherited.
2. **D6 answered means M1 ships with clause 5 switched off.** `NoBudget` is correct per
   D6 — unset is a genuine no-op — so **nothing spills until a budget is configured**.
   A session-config gap, not a code one, and invisible unless said.
3. **`Gate` will collide with W11.** W9 defined the smallest adjudication seam the
   runtime needs (`admit(name, access, args)`). W11 should absorb it rather than build
   a parallel one.
4. **§8.1 clause 1 and §9.4 pull against each other.** Relaxing a pattern *is*
   rewriting the query, which §9.4 forbids. The reconciling word is **visible**: every
   relaxation appears in the result. Worth stating in §9.4 so nobody deletes one of
   the two.
5. **§8.4's `orchestrator` role has no `read_spill`** though its `read`/`grep` can
   spill — so for that role the omission notice is advisory, which §8.3 says it must
   not be. W9 shipped `read_spill` in its place.
6. **Retrieval is inert — no MCP server is running anywhere.** Confirmed by lubuntu3
   on 2026-09-09, checked rather than recalled.
   - `192.168.1.55:9755` is the **flowy node**, not oracle. Never was the address.
   - **RAGFlow and oracle are on lubuntu3 (192.168.1.82)** — but its MCP server **is
     not started by that deployment**. Docker publishes 9382-9384, so a **TCP connect
     succeeds while nothing is behind it** (`curl` → HTTP 000, zero mcp processes in
     the container). That is why a client reports "unable to connect to the url": the
     connection is fine, the service is absent. Same shape as the embed wedge — a
     check that succeeds without touching the thing it checks.
   - `:8100/sse` **looks** like an SSE endpoint and is not — it is arxiv-search, whose
     catch-all returns identical HTML for every path. Do not use it.
   - **Use the REST API instead: `192.168.1.82:9380`, base `/api/v1`, bearer auth.**
     Do not ask for an MCP listener to be started while ingestion is live.
   - `ask_code` is a **separate problem**: per-box, against the local codebase, and
     **nothing is running on this box** (checked — only qwen/bge/postgres listen).
   - **Open and load-bearing:** does RAGFlow signal "the corpus does not cover this"
     in a *field*, or only in prose? §8.1 clause 3 requires abstention to be
     structurally distinguishable. If it is a field, it maps to `Abstained` and the
     harness can never report it as grounded. **If it is only prose, we will not parse
     the prose and pretend** — the tool returns `Ok` with the text and the hole stays
     documented. Asked; awaiting an answer.
   - Note from lubuntu3: `.82:11434` enforces **one request per backend and will hold
     a call rather than refuse it**. Do not point volume at it unannounced.
7. **Call ids are positional per turn** (`call_0`; GLM carries none), so an id-derived
   mark repeats across turns. Fine now, wrong once a head correlates across a session.

---

## T17 — harnessd — **BUILT 2026-09-09. The loop runs; M1's exit criterion is NOT met as written.**

`crates/harnessd`. Scripted 30-turn session against `qwen-3.8-flash-next`: 53
submissions, 25 tool calls, 161 persisted rows, 22,201 final prompt tokens, 85 s.

| check | result |
|---|---|
| C1 exact prefix extension | **PASS**, off the ledger's token vectors |
| C3 generation-inclusive prefix | never violated; 3/52 server-side shortfalls |
| **C4 `f_keep` p10** | **0.8771 — FAILS ≥0.99** |
| C4b cache efficiency p10 | **1.0000** |
| C5 reasoning replay | PASS (worst 0.9859) |
| C9 mid-session system change | PASS — stable prefix byte-identical |
| C10 disjoint cache accounting | PASS on all 53 |
| C6 / C7 / C8 | not run — see below |

**See T22 first — C4 is mis-specified, and the run measured what the plan asked for
rather than what the plan meant.**

**On the failure, and why I do not think it is the harness.** `f_keep`'s denominator
is the *whole* prompt, so a submission appending a 1,093-token tool result to a
9,000-token prompt cannot exceed 0.88 however perfect the cache is. 19 of 52
submissions add more than 1% of their own prompt. The **ceiling the script allows is
p10 0.8813, and we got 0.8771 of it**. Session-wide, 14,901 of 640,076 prompt tokens
were prefilled — **97.7% avoided**.

So **§17's exit criterion measures the script's shape as much as the harness**, and
≥0.99 is unreachable for *any* tool-using session. The harness-only number is C4b, at
p10 1.0000. **The criterion needs rewriting; the plan is wrong here, not the code** —
but it is recorded as a failure because that is what it is against the stated bar.

C6/C7/C8 could not run: C6 needs a tool-call-only assistant turn, which did not occur;
C7/C8 need a `length` finish, which requires either the `n_predict` cap §5.7 removed or
a 262k context on a production box.

**C3's shortfalls have an exact signature** — every one equalled `previous generation +
3`: the server reused to the end of the last committed item and discarded the 4-token
generation prompt *and* the whole generation. It is intermittent (14/55 on an earlier
run of the same script), so it is a server-side cache event, not structural. Settling it
needs `-lv 4` and `glm-why-no-cache` — a server-log question the harness cannot answer
from its own metrics.

---

## T20 — Where the assembled parts did not fit — **integration findings, several serious**

1. **Double envelope.** `ToolRuntime::transcript_item` renders §8.2's `NO_RESULT`
   envelope into `payload`, and **both** dialect renderers wrapped it again — the model
   received two. Fixed: renderers emit `payload` verbatim.
2. **`Registry::tools_json()` is not prompt-ready** and this one was nearly invisible.
   It emits `,`/`:`; both templates use HF `tojson`'s `, `/`: `. Using it directly for
   `StablePrefix::tools_json` differs in **the first bytes of the stable prefix** — a
   cold prefill every single turn, with nothing in the tree to catch it. Now routed
   through the dialect's own tool-JSON function, with a test asserting the two differ.
3. **C10 was unmeasurable as specified** — `prompt_tokens − cached_tokens` from
   `TurnMetrics` alone is a tautology. Needed the server's own frame numbers.
4. **`ToolRuntime` is not `Send`** — `Gate` lacks `Send + Sync`, alone among the
   runtime's traits. Blocks §13.2's multi-head worker. Left for W11 to absorb (T16.3).
5. **`Session::append_items` renders one item at a time**, so Qwen's consecutive tool
   results cannot merge into one user turn without a fourth trait method. Declared as a
   gate divergence.
6. **Qwen's `SystemUpdateMode` is `Envelope`, not `InHistory`** — its template *raises*
   on a mid-history system message, so §5.3's conservative default is the only
   renderable form. The model can and did argue with it.
7. **`ask_code`/`ask_corpus` return `NotRun`, not `Abstained`** — nothing ran, which is
   a different fact. The daemon's startup disclosure says so.
8. The `/apply-template` oracle caught a renderer bug **no unit test would have**: an
   `Assistant` following a `Reasoning` opened a second message.

---

## T21 — Three failure modes the operator wants solved — **stated requirement**

All three are instances of F5 — *never let a component's "I did not do this" be
reported upward as success* — applied where nobody applies it.

**1 and 2 are the same bug: a process predicate that matches the process evaluating
it.** `pkill -f X` where the pattern matches the shell running it. `until pgrep -f
model; do sleep; done` where the condition matches the waiter — so it never fires, or
fires at once.

Harness-solvable because the harness **owns the bash tool** and knows what the model
cannot see: its own pid, its command line, the parent chain, and the pids of the
servers it manages. Before running, test the pattern against `/proc/self/cmdline`;
refuse with the diagnosis and what it currently matches, per §8.1 clause 1 — a hazard
that is self-correcting in the same call.

The waiter form gets a second check the model cannot do: **is this predicate matching
the model server that is serving you.** Waiting on your own backend is a deadlock the
harness can see.

*(The author of this entry committed failure mode 1 twice in one day, and it was
already in memory as a lesson learned.)*

**3 — announced, then not done.** The model says "I'll start X" and the turn ends with
nothing started. Not a process bug: an **intent-versus-action gap**, solvable because
the harness sees the whole turn while the model sees only its own output.

Mechanism: a post-flight assertion, the same shape as the prefix check and the length
policy — the turn ends with a future-tense commitment and no corresponding tool call.
The response is **steering, not failure**: §5.8 already injects at a step boundary, so
append *"you said you would X — do it or say why not"* and continue.

Honest limit: detecting intent is a judgement, not a parse, so there will be false
positives. The harness does not need certainty — it needs the gap **visible rather than
silent**, and a wrong steer costs a few tokens. Same trade as the recall suggestion.

---

## T22 — `f_keep` names two different quantities, and C4 applies one's threshold to the other

Found while answering "how is `f_keep` computed". The answer is: **two ways, and the
plan uses both under one name.**

### The two

`server-task.cpp:2603,2614` — a cache **selection** heuristic, with a hard floor
refusing any entry under `f_keep < 0.25`:

```
f_keep = lcp / cached_entry.size()      denominator = the CACHED entry
f_sim  = lcp / new_prompt.size()        denominator = the NEW prompt
```

`lcp` is the longest common prefix — how many leading tokens of the new prompt match
the cached entry, token for token, via `server_tokens::get_common_prefix`.

§18.2's **C4** defines it as `usage.prompt_tokens_details.cached_tokens /
prompt_tokens`. **That is `f_sim`, under `f_keep`'s name.**

### Why it matters — one turn of the M1 run

```
cached entry (turn N)     9,000 tokens
new prompt  (turn N+1)   10,093 tokens   ← appended a 1,093-token tool result
lcp                       9,000 tokens   ← the whole entry is a prefix; nothing rewritten
```

That is a **perfect** turn — full reuse, nothing recomputed.

| | formula | value |
|---|---|---|
| llama's `f_keep` | lcp / cached | **1.000** |
| llama's `f_sim` | lcp / new prompt | 0.892 |
| **C4's `f_keep`** | cached_tokens / prompt_tokens | **0.892** |

The gap is not a cache miss. **It is the tool result that was just appended.** C4's
number falls purely as a function of how much the conversation grew.

### The defect

C4 says it is *"directly comparable to the 0.000 → 0.999 measurement"*. **It is not.**
That 0.999 was read off the server's trace lines, so it was llama's `f_keep` —
denominator the cached entry, and therefore **indifferent to growth**. The plan even
notes at line 64 that the two are "a hit-ratio pair", then migrates a threshold from
one to the other.

So a bar measured on metric A is applied to metric B. On A, 0.99 is reachable and was
reached. On B it is arithmetically impossible for any session that returns tool
output.

### SETTLED 2026-09-09 — option 1, see D11

`f_keep = lcp / cached_entry`, computed as
`cached_tokens(N+1) / (prompt_tokens(N) + committed_generated(N))` — **no server change
needed**, since the denominator is what we left in the cache and the numerator is what
the server already returns. It is the ratio form of C3's inequality, over C3's own
quantities.

**M1 must be re-measured.** 0.8771 was a correct measurement of the wrong metric.

### The choice as it stood — operator's

1. **Restate C4 as `lcp / cached_entry`** — directly comparable to the 0.999 that
   motivated it, indifferent to growth, and what the append-only invariant actually
   claims. Needs the server's `lcp`, which is S1 or `-lv 4`.
2. **Keep `cached_tokens / prompt_tokens`** and set a bar reachable for tool-using
   sessions. The harness-only version of this is T17's C4b, which measured p10 1.0000.

Until it is decided, **M1 remains formally unexited** and the run stands as recorded.

This is the seventh instance in one day of a number whose meaning was not checked
before use, and unlike the others it is **ours** — it is in the plan, not in something
we inherited.

---

## T23 — A frame advancing `tokens_predicted` by 2 while carrying 1 id — **do NOT relax the guard**

`crates/turn/src/stream.rs:172` refuses a turn when a frame's `ids.len()` does not equal
its advance in `tokens_predicted`. It fired in **3 of 6 M1 runs on 2026-09-09** (once
twice in one run) and in **neither** of the two runs recorded before that. It costs a
whole turn when it fires and it is upstream of everything C4 measures.

**First reading, mine, and wrong to act on:** the server runs `--spec-type draft-mtp
--spec-draft-n-max 5`, and a frame advancing the counter by 2 while carrying one id is
the shape of speculative acceptance — so the guard assumes one frame is one token and
is too strict for MTP.

**Measured instead of assumed, and it does not hold up.** A 400-token request against
the live server: 39 frames, 35 advancing, **advance histogram `{1: 35}`** — every
advancing frame carried exactly its own ids, and the ids on advancing frames reconciled
exactly with final `tokens_predicted`. The mismatch did not reproduce. What *did*
reproduce is the other trap: **4 non-advancing progress frames carrying 3 fabricated
ids**, which the guard's `n_decoded <= self.n_decoded` early return correctly discards.

**So the guard may be right and the server may be dropping an id.** That inverts the
fix. If an id is genuinely absent then the turn's token identity is unknown, and
refusing it is *correct* — the ledger's whole claim is that it holds the exact ids the
model produced. **Relaxing the check to make the failure go away would trade a loud
turn failure for a silent ledger corruption**, which is the one outcome this design
exists to prevent.

**Recommendation: instrument, do not relax.** On `FrameMismatch`, capture and log the
raw frame — its `tokens`, `tokens_predicted`, `timings`, and the frames either side.
The next occurrence then says whether the id arrives late, arrives elsewhere, or never
arrives, and *that* decides the fix. Until then the guard is doing its job.

Conditions worth noting for whoever chases it: it is intermittent, it appeared only
after the server was OOM-killed and restarted at 14:35 (same unit, same flags, cache
lost), and the same runs show slot migration mid-session — run 1's turn 19 moved from
slot 2 to slot 0 and got a cold slot. Concurrency across five slots with other agents
using the server is the obvious variable nobody has controlled for.

---

## T5 — Operator decisions still open

W6 parses tool calls; W9's runtime executes them; W7 logs; W8 renders. **No crate ties
them together.** There is no `harnessd`. `ToolRuntime::transcript_item` and
`ToolLogSink` are the two ends of that wire and both are tested; the loop between them
does not exist.

That loop *is* M1: submit, stream, parse a tool call, execute it, append the result,
resubmit. Everything it needs is built and merged.

---

## T18 — The prompt cache costs ~120 KiB/token on recurrent models — **operational, caused a fifth OOM**

Found while running UNVERIFIED-16, which **OOM-killed `qwen-flash-next` at 14:35 on
2026-09-09** (158.1 GiB peak; systemd restarted it, `qwen-slots-restore` put the slots
back, ~40 s down, in-flight requests from two other agents lost).

Cause, and it generalises: **every distinct prompt prefilled becomes a prompt-cache
entry at ~120 KiB/token, because 111 MiB of each entry is the recurrent accumulator** —
fixed per sequence, independent of length. A sweep over many short distinct prompts is
therefore far more expensive than its token count suggests. `server-context.cpp:4549`
records four previous occurrences; this was the fifth.

Mitigation used, and worth generalising: the sweep harness refuses to start a condition
below 40 GiB `MemAvailable`. Any future experiment that prefills many distinct prompts
needs the same guard.

---

## T19 — Rewrite or not, on the KV representation — **the angle UNVERIFIED-16 should be read from**

The operator's framing, and it is the right one: llama.cpp's data structures are not
the question. The question is **what would require a rewrite, and whether that rewrite
pays.** Separating the two changes what T14's negative actually settles.

### llama.cpp's choices — changeable

- recurrent cells *are* sequence ids, so a save is one 111.4 MiB blob per sequence
- attention KV and recurrent state are saved as a single unit, all-or-nothing
- rewind has **three fixed re-entry points**: free to `d ≤ 3`, one checkpoint to
  `d ≤ 516`, then **the entire prefix from token 0**

### Mathematics — survives any rewrite

A recurrent state is a **fold**: the state at position *n* encodes all *n* tokens, so
there is no "block B's contribution" independent of what preceded it. It is not stored
badly; it does not exist as a separable quantity.

This is also why CacheBlend works on attention-only models and cannot here. Attention
has cross-block dependence too, but it is **diffuse** — a weighted average whose
distribution barely shifts under a different prefix, so recomputing the ~15% that
deviate most corrects the rest. A fold has no diffuseness to exploit: there is no
subset whose recomputation repairs the remainder.

### The split

| | verdict | why |
|---|---|---|
| composition across a divergence | **do not rewrite** | mathematically blocked, not an implementation limit |
| splitting attention from recurrent in the save format | **do not rewrite** | after a divergence every layer differs anyway, so there is nothing to reuse |
| **checkpoint density / rewind granularity** | **worth doing** | pure implementation; the `d > 516` cliff is checkpoint placement and nothing else |

**The third row is the actionable one and it was measured by accident.** Rewind is what
fork (§5.5), backtracking and eviction all actually need, and today it falls off a
cliff at 516 tokens. Storing recurrent state at more positions makes rewind cheap. That
is a parameter and a representation choice, not a redesign.

---

## T22 — `f_keep` names two different quantities, and C4 applies one's threshold to the other

Found while answering "how is `f_keep` computed". The answer is: **two ways, and the
plan uses both under one name.**

### The two

`server-task.cpp:2603,2614` — a cache **selection** heuristic, with a hard floor
refusing any entry under `f_keep < 0.25`:

```
f_keep = lcp / cached_entry.size()      denominator = the CACHED entry
f_sim  = lcp / new_prompt.size()        denominator = the NEW prompt
```

`lcp` is the longest common prefix — how many leading tokens of the new prompt match
the cached entry, token for token, via `server_tokens::get_common_prefix`.

§18.2's **C4** defines it as `usage.prompt_tokens_details.cached_tokens /
prompt_tokens`. **That is `f_sim`, under `f_keep`'s name.**

### Why it matters — one turn of the M1 run

```
cached entry (turn N)     9,000 tokens
new prompt  (turn N+1)   10,093 tokens   ← appended a 1,093-token tool result
lcp                       9,000 tokens   ← the whole entry is a prefix; nothing rewritten
```

That is a **perfect** turn — full reuse, nothing recomputed.

| | formula | value |
|---|---|---|
| llama's `f_keep` | lcp / cached | **1.000** |
| llama's `f_sim` | lcp / new prompt | 0.892 |
| **C4's `f_keep`** | cached_tokens / prompt_tokens | **0.892** |

The gap is not a cache miss. **It is the tool result that was just appended.** C4's
number falls purely as a function of how much the conversation grew.

### The defect

C4 says it is *"directly comparable to the 0.000 → 0.999 measurement"*. **It is not.**
That 0.999 was read off the server's trace lines, so it was llama's `f_keep` —
denominator the cached entry, and therefore **indifferent to growth**. The plan even
notes at line 64 that the two are "a hit-ratio pair", then migrates a threshold from
one to the other.

So a bar measured on metric A is applied to metric B. On A, 0.99 is reachable and was
reached. On B it is arithmetically impossible for any session that returns tool
output.

### SETTLED 2026-09-09 — option 1, see D11

`f_keep = lcp / cached_entry`, computed as
`cached_tokens(N+1) / (prompt_tokens(N) + committed_generated(N))` — **no server change
needed**, since the denominator is what we left in the cache and the numerator is what
the server already returns. It is the ratio form of C3's inequality, over C3's own
quantities.

**M1 must be re-measured.** 0.8771 was a correct measurement of the wrong metric.

### The choice as it stood — operator's

1. **Restate C4 as `lcp / cached_entry`** — directly comparable to the 0.999 that
   motivated it, indifferent to growth, and what the append-only invariant actually
   claims. Needs the server's `lcp`, which is S1 or `-lv 4`.
2. **Keep `cached_tokens / prompt_tokens`** and set a bar reachable for tool-using
   sessions. The harness-only version of this is T17's C4b, which measured p10 1.0000.

Until it is decided, **M1 remains formally unexited** and the run stands as recorded.

This is the seventh instance in one day of a number whose meaning was not checked
before use, and unlike the others it is **ours** — it is in the plan, not in something
we inherited.

---

## T23 — A frame advancing `tokens_predicted` by 2 while carrying 1 id — **do NOT relax the guard**

`crates/turn/src/stream.rs:172` refuses a turn when a frame's `ids.len()` does not equal
its advance in `tokens_predicted`. It fired in **3 of 6 M1 runs on 2026-09-09** (once
twice in one run) and in **neither** of the two runs recorded before that. It costs a
whole turn when it fires and it is upstream of everything C4 measures.

**First reading, mine, and wrong to act on:** the server runs `--spec-type draft-mtp
--spec-draft-n-max 5`, and a frame advancing the counter by 2 while carrying one id is
the shape of speculative acceptance — so the guard assumes one frame is one token and
is too strict for MTP.

**Measured instead of assumed, and it does not hold up.** A 400-token request against
the live server: 39 frames, 35 advancing, **advance histogram `{1: 35}`** — every
advancing frame carried exactly its own ids, and the ids on advancing frames reconciled
exactly with final `tokens_predicted`. The mismatch did not reproduce. What *did*
reproduce is the other trap: **4 non-advancing progress frames carrying 3 fabricated
ids**, which the guard's `n_decoded <= self.n_decoded` early return correctly discards.

**So the guard may be right and the server may be dropping an id.** That inverts the
fix. If an id is genuinely absent then the turn's token identity is unknown, and
refusing it is *correct* — the ledger's whole claim is that it holds the exact ids the
model produced. **Relaxing the check to make the failure go away would trade a loud
turn failure for a silent ledger corruption**, which is the one outcome this design
exists to prevent.

**Recommendation: instrument, do not relax.** On `FrameMismatch`, capture and log the
raw frame — its `tokens`, `tokens_predicted`, `timings`, and the frames either side.
The next occurrence then says whether the id arrives late, arrives elsewhere, or never
arrives, and *that* decides the fix. Until then the guard is doing its job.

Conditions worth noting for whoever chases it: it is intermittent, it appeared only
after the server was OOM-killed and restarted at 14:35 (same unit, same flags, cache
lost), and the same runs show slot migration mid-session — run 1's turn 19 moved from
slot 2 to slot 0 and got a cold slot. Concurrency across five slots with other agents
using the server is the obvious variable nobody has controlled for.

---

## T5 — Operator decisions still open

Carried from `DECISIONS.md`; see there for the full statement of each.

- **D3** — firecode's two asks (parent cgroup, persistent shell). A written "no" is a
  complete answer and unblocks the work; it selects which of two tool runtimes gets
  built.
- **D5** — Falsifier B scoring rubric.
- **D6** — `max_inline_bytes` (deliberately has no default).
- **D7** — the 27B preset.
- **D8** — the harness licence. The workspace currently declares Apache-2.0 and there
  is no GitHub remote yet; the repo is intended to be public, so this should not stay
  open.

---

## T6 — Not started, from `docs/workstreams.md`

W1 measurement rig · W2 prefix-invariant suite (grok port, Apache-2.0) · W6 turn
engine (**critical path**) · W7 session log and head protocol · W8 heads · W9 tool
runtime · W10 firecode substrate · W11 adjudication · W12 flowy connector · W13
segments and compaction · W14 EXPLAIN · W15 server track · W16 experiments.

`pytest` is still not installed; `docs/workstreams.md` calls that blocking for W1/W2.

---

## T24 — The harness owns what a subagent leaves behind, and monitors need the same owner

Operator, 2026-09-09: *"you guys like to accumulate monitors and shells"* — and, on
being shown the measurement, *"you are fine, others nt"*. That correction is the
requirement. The leak is not the top-level agent's own housekeeping; it is **everything
its children spawn and do not clean up.**

Measured on this box the same evening, mid-session:

| | |
|---|---|
| git worktrees in `.claude/worktrees/` | **13**, most from agents that finished hours ago |
| orphan tmux sessions | `nano_test`, **30 hours** old, from a finished agent |
| the top-level agent's own leak | 1 listener, 1 poll loop — correct |

`docs/tui-testing.md` already says *"Kill your sessions. An orphaned tmux session holds a
`harnessd` and its socket, and the next run then attaches to a daemon it did not start."*
Written here, loaded in context, and it did not bind. That is `docs/closed-loop.md` §1
again and the fix is the same: not a better rule.

### The mechanism was already chosen

D4, answered by the operator earlier the same day: *"just do byobu, which connects to
cgroups — use dependent. if a vm is temporary then it is a session cgroup otherwise not.
still must be clearly reapable."*

So: **every process a turn spawns lands in a cgroup owned by a scope.** Three scopes and
no fourth — `turn`, `session`, and `explicit` (survives the session because somebody said
so, and is listed as such). Reaping is `cgroup.kill`, and a subagent's cgroup is a child
of its parent's, so a parent that ends reaps its children by construction rather than by
remembering to.

### The part that pays for itself immediately

**Cgroups retire pattern matching for both reaping and liveness**, which is T21.1 and
T21.2 dissolved rather than guarded:

- `pkill -f X` becomes "kill this cgroup" — no pattern, so nothing to self-match.
- `until pgrep -f X` becomes "is this cgroup non-empty" — no predicate that can match its
  own waiter.

The evidence that a guard is not enough: on 2026-09-09 a process check self-matched
**five times in one session**, with `process-checks-that-self-match` loaded in memory and
T21 open in this file. The fifth was `grep -E '[h]arnessd'` — the bracket trick defeats
`pgrep`, but the shell wrapper echoes the expanded pattern back into its own command line,
so the literal string was there to be found. A hazard with that many spellings is not
one you check for; it is one you make unspellable.

### Monitors, which are the same problem wearing a different hat

Operator, same message: *"btw we need monitors in letibot"*. A monitor is a condition
watched **between** turns that wakes the loop when it fires — the encoder running while
the model is not, in `docs/closed-loop.md`'s terms, and the only correct shape for
listening. The seat brief pays for that distinction: *a Stop hook fires when a session
goes idle, and a seat that is rate limited, has crashed, or never started is not running
a session, so no stop event ever fires and the silence looks exactly like a quiet room.*

A monitor is also, structurally, a long-lived process. **Adding monitors before the
lifetime work multiplies the leak this entry exists to stop**, so they land together:

1. **Scoped at creation.** No monitor without an owner; the default is the session.
2. **Listable and attributable.** The head can show what is watching and for whom, the
   way it now shows sessions. An invisible watcher is an unreapable one.
3. **Reports why it fired**, not just that it did.
4. **One waiter per name**, enforced. The fleet already learned this: two processes under
   one reader means the roster shows a seat attached while the real one hears nothing.
5. **Bounded.** A TTL or an explicit renewal, so a monitor whose reason has passed dies
   without anybody remembering it.

### Blocks on

Nothing. This is substrate for M6 subagents (S8) and it is **cheaper to build before them
than after**: the leak measured above came from subagents the harness does not yet have,
run by an agent that does. The requirement was discovered before the feature, which is the
rare ordering.
