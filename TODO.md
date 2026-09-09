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

## T14 — Composable KV is a GOAL, not background — **operator requirement, stated twice**

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
