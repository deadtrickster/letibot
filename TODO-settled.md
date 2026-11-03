# TODO — settled

Closed work, kept because open items in `TODO.md` rest on these measurements and
cite them by number. Split out of `TODO.md` on 2026-09-10 so the open list is the
open list.

Nothing here needs doing. If one of these turns out to be wrong, it moves back.

Also closed and not reproduced here, because the record is elsewhere:
**T5a** (claimed the M1 loop did not exist; superseded by T17, which records
harnessd built and serving), **D9**/**D10** (answered on arrival, in git history),
and **D19** (the dense-27B control "cannot run here" — refuted 2026-09-10; it runs
under `~/bin/qwen-dense-server`, see that script's comments for the measurements).

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

---

## T2 — Revise the `Dialect` contract — **SETTLED 2026-11-03: split by 8ca0d4b3, all eight defects closed**

Checked against the tree rather than against the score, because the score was written
before the work and four of its eight rows had already moved. Where each defect stands:

| # | defect | on main | the pin |
|---|---|---|---|
| 1 | `render_incremental` cannot be pure | gone from the contract: `crates/dialect` has no render trait at all, only `DialectSpec` (template, sha, tokens, stops, quirks) | `render_incremental_agrees_with_render` and friends, glm `tests/invariants.rs`; `render_incremental_equals_render_at_every_prefix`, qwen |
| 3 | no home for the generation prompt | gone: an inherent function per dialect, outside `DialectSpec`, so it never has to be part of a render | `generation_prompt_is_the_head_of_the_next_assistant_turn` (glm), `the_generation_prompt_keeps_the_newline_after_think` (qwen) |
| 5 | resolution must key on literal | `ControlTokens::by_literal` is the lookup and `all_with_role` returns an iterator, so "this role has three literals" cannot be missed | `lookup_is_by_literal_and_a_role_may_own_several` (contract), `lookup_is_by_literal` (glm) |
| 4 | `ControlRole` closed and too small | `SequenceStart`, `ArgKeyOpen/Close`, `ArgValueOpen/Close`, `ImageOpen`/`Image`/`ImageClose`, `Other` | `every_glm_token_has_a_real_role`; `the_image_triplet_has_three_roles_and_media_reads_them` and `im_start_is_other_on_purpose` (qwen) |
| 2 | `parse(&[u32])` unimplementable | `Parser::parse` takes `&dyn TokenDecoder`; `letibot-tokencore`'s `VocabDecoder` is the one implementation that has a vocabulary | the `TableDecoder` round trips: `parse_round_trips_reasoning_and_two_tool_calls` (glm), `parse_after_render_recovers_the_call_it_rendered` (qwen) |
| 6 | `stop_tokens()` returns bare literals | `StopToken { literal, role }`, and `resolve_stops` reports a failure by role through `ControlFailure` | `every_stop_token_is_a_declared_control_token`, in both crates |
| 7 | `ControlTokens` forces `&'static str` | `Cow<'static, str>` on the literal, `Cow<'static, [ControlToken]>` on the table | `a_spec_can_be_built_entirely_at_runtime`, `an_owned_table_answers_exactly_as_a_borrowed_one_does` |
| 8 | `ControlRole` has no `Ord` | derived, and `tokencore::control::sorted` orders failures by it | `roles_sort_so_a_listing_can_be_deterministic` |

**The one thing that needed deciding is decided.** `server-bug-compatible` is dropped:
`letibot-render` rejects the profile by name, `tests/fidelity/run_gate.py` says why in
its header, and `docs/chat-templates.md` records that there is one profile.

**What the split does not include, said plainly.** Rendering is still hand-written in
`crates/dialect-glm/src/render.rs` and `crates/dialect-qwen/src/render.rs`, so "rendering
is data" holds in the contract and not yet in the shipped path. That is T1's remaining
wiring, not T2's: a `DialectSpec` already carries the template and its sha, and nothing
here blocks it. The fidelity gate is green on the hand-written pair - **139/139 cases
byte-identical to CPython Jinja2** - which is the baseline the template renderer must
match.

**There is no deepseek dialect to update.** Deepseek is a provider in `providers.toml`
that renders server-side as chat messages; the dialect crates are glm and qwen.

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

---

## T23 — A frame advancing `tokens_predicted` by 2 while carrying 1 id — **SETTLED 2026-09-10: the server drops a frame; the guard was right**

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

### SETTLED 2026-09-10 — the server drops a token's frame on an incomplete UTF-8 tail

**Cause, in llama.cpp.** `process_token` (`server-context.cpp:4067`) computes

```cpp
bool incomplete = validate_utf8(slot.generated_text) < slot.generated_text.size();
```

and puts `send_partial_response` inside `if (!incomplete)`. `slot.stats.n_gen` has
already been incremented — in **both** the plain (`:6450`) and the speculative
(`:6615`) decode paths. So a token whose bytes leave the generated text ending
mid-character produces **no frame at all**, while the counter it advanced is
reported by the *next* frame, which carries only its own id.

Captured verbatim from the live stream (control server, no draft model):

```json
{"index":0,"content":"😀","tokens":[141334],"stop":false,"tokens_predicted":1}
{"index":0,"content":" 😂","tokens":[224],   "stop":false,"tokens_predicted":3}
```

`" 😂"` is `[26525, 224]`. `26525` spells a space plus the **first three bytes** of a
four-byte emoji, so its frame was suppressed; `224` is the fourth byte and carries
the accumulated text. **Id `26525` is never transmitted** — in stream mode the
terminal frame's `tokens` array is empty (`server-context.cpp:4326`), so there is
nowhere else for it to appear. The guard was right: the id is genuinely absent.

**Why the shape was always `advance 2, ids 1`.** Never `2 → 0`, because a suppressed
token emits nothing rather than an empty frame. `3 → 1` when a character is spread
over three tokens. And it is content-dependent, not periodic — ASCII and common
typographic punctuation are single vocabulary entries. The operator's two
occurrences were both `advance 2`, which is what a three-byte character makes; `√`
(U+221A) and `∞` (U+221E) were measured doing exactly that in a technical report.

**The MTP hypothesis is dead, measured both ways** (2026-09-10):

| server | content | advancing frames |
|---|---|---|
| `:8080`, `--spec-type draft-mtp` (215 drafted, 150 accepted) | ASCII | `{adv 1 / ids 1: 190}` — **0 mismatches** |
| `:8080`, same | emoji | `adv2 ×29, adv3 ×11` — 40 mismatches |
| control, **no `-md`, no `--spec-type`** | ASCII | `{adv 1 / ids 1: 112}` — 0 mismatches |
| control, same | emoji | `adv2 ×67` — **67 mismatches** |

The variable is content. The draft head is not involved. The control was a different
model and arch (`Qwen3-4B-Instruct-2507-Q6_K`, killed after the run), so its clean
ASCII row proves nothing on its own — the **positive** row is what carries the
argument: the mismatch reproduces with speculative decoding entirely absent.

Base rate on agent-style output with light Unicode: **4 in 269 tokens**. On pure
ASCII prose, 0 in 400.

**Fix, and where it is not.** Not in the guard, and not in the harness's arithmetic
either — there is no expected-advance the harness could compute, because the
suppressed frame's id is not anywhere on the wire. This is an **upstream defect
worth reporting**: `process_token` should either send the frame with its id and an
empty `text_to_send`, or defer the counter along with the frame. Today it defers one
and not the other. UNVERIFIED: nothing has been filed upstream.

**Landed in `crates/turn` (branch `t23/frame-capture-and-partial-keep`):**

1. `capture.rs` — on a refusal, the offending frame, the two before it and the next
   three go to a file verbatim (`LETIBOT_FRAME_CAPTURE_DIR`, on by default). The
   *trailing* frames are the point: "the id arrives late" and "the id never arrives"
   produce the same error message and want opposite fixes.
2. The refusal is now `AbortCause::FrameMismatch` rather than `TurnFailure::Stream`,
   so **the ids already accounted for survive it**. The guard did not move — the
   refused id and everything after it still never reach the ledger — but the turn is
   now `TurnInterrupted{partial_kept}` with a named seam instead of *"nothing was
   recorded"*. The same trade §5.8 already makes for an urgent steering message.
3. Known limit, in the safe direction: if the cut lands inside an unterminated tool
   call, the parser emits no call and `rows_cover_every_token` refuses the turn
   wholesale. Nothing half-formed is committed or executed; there is simply no
   partial in that case.

Conditions worth noting for whoever chases it: it is intermittent, it appeared only
after the server was OOM-killed and restarted at 14:35 (same unit, same flags, cache
lost), and the same runs show slot migration mid-session — run 1's turn 19 moved from
slot 2 to slot 0 and got a cold slot. Concurrency across five slots with other agents
using the server is the obvious variable nobody has controlled for.

---

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

### T21.3 — **JOINED 2026-09-10.** Both halves existed; nobody called the boundary

`intent::close_the_turn` and `letibot_turn::steering` were both built, both tested, and
had no join: T21.3 sat announced-but-not-done, which is the failure it exists to catch.
The session loop now calls the diff at the turn boundary and routes findings through the
steering source, which injects at the next step boundary.

**Two decisions the join forced, neither of them obvious.**

**The prose half is a flag and the tool-declared half is not**, and the split is the rule
this repo is under rather than caution. `IntentLedger::reconcile(turn_id, "")` — the
deterministic half — cannot fire unless the model used `todo` or `goal`, and only a role
that names them seats them, so a default session is behaviourally identical with it on.
`intent::close_the_turn` also reads the assistant's prose, and `commitments` fires on any
turn that ran no tools — which for a plain answer is the *normal* shape. Switching that on
for every existing session would put *"you said you would X"* into conversations that were
working. It is the more useful half and the one with an unmeasured false-positive rate, so
it is `--intent-prose`.

**The nudge is capped at one per user turn.** T21.3's response is *"append … and
continue"*, and continuing means the answer turn is not the end of the loop — the
`calls.is_empty()` branch appends the notice and goes round again rather than returning.
Uncapped, a model that answers the nudge with more prose and no tool call produces a
second finding, a third, and burns `max_tool_rounds` on an argument. One is the error
signal; two is the harness insisting. If the model explains itself and stops, that is a
legitimate answer to the check — `docs/closed-loop.md`'s tolerance band (T22-adjacent, and
D22's open question), set at its narrowest until somebody measures a better one.

**Still open:** the false-positive rate of the prose half is unmeasured, which is exactly
why it is off. Measuring it needs a corpus of turns labelled *"did this commitment matter"*,
and the honest source for one is the same as §4c's: run with it on and record the
overrides.

### T21.4 — **DONE 2026-09-10.** The round counter was an open-loop guard and it cut working turns twice

`max_tool_rounds: 12` was the only thing that could end a runaway turn, so it also
ended two legitimate ones — a session asked to *"look at the project and suggest
improvements"*, cut mid-investigation, and before that a session doing genuine
exploration two rounds from finishing, every round of it new work.

**A count of rounds measures effort.** It never looks at the effect, only at the
command, which is `docs/closed-loop.md` §1 exactly. And the message blamed the model
for working — *"the model called tools 12 times without answering"* — when it was
answering and had not finished. Same defect as `grep` reporting absence having opened
no files.

`crates/harnessd/src/progress.rs` is the encoder. **A round made progress when at
least one of its calls returned `Ok` with a result this turn had not already seen.**
Every input is a fact the harness already computed: `args_digest` for the repeated
call, `payload_digest` for the repeated result, and `intent::ledger::is_effect` —
lifted out of `record_effect` so the ledger and the detector cannot come to hold two
nearly-identical predicates that disagree silently.

**Why payload novelty and not the repeated call**, which is the stronger raw signal: a
model that edits a file and reads it back makes a byte-identical call and gets a
different answer, and that is progress. Only the payload separates it from the third
read of an unchanged file. The repeated call is kept for the *evidence*.

`max_tool_rounds` moves to 200 and is now a backstop for a different failure — a turn
producing genuinely new results forever — with a sentence that no longer accuses.
`--stall-rounds N` (default 5) is the band, `0` is off and disclosed.

**The operator's own prompt was replayed at the new defaults.** *"Look at the
project and suggest improvements."*, the session that was cut, ran to completion:
**28 rounds, 55 tool calls, exit 0**, ending in a ranked list with an ordering
argument. Every round produced at least one `Ok` carrying bytes the turn had not
seen, so the progress check never came within four rounds of firing. The old cap
stopped this turn less than half way through it.

**The false positive is measured, not assumed.** Live against the running server: a
turn told to run three searches one per step, each correctly finding nothing, is a run
of stalled rounds even though every query was new and every answer was right. At the
default it survives (3 < 5); at `--stall-rounds 2` it was cut. The nudge at N-1 is the
mitigation and it now names *which* case it saw — repeated calls, or new questions
nothing could answer — so the operator can tell the two apart in the stop.

**Still open:** whether a run of *distinct* fruitless queries should get a longer band
than a run of repeats. It is one threshold today and they are plainly not the same
failure. Nobody has measured how often either occurs.

---


---

> **Both verified implemented on 2026-09-10** while still filed as open.
> T12: `crates/turn/src/engine.rs:482,794` seeds `in_reasoning` from
> `items::lead_opens_reasoning(&lead, ...)` — the generation prompt — and the
> channel is computed before the delta is emitted.
> T21.1/T21.2: `crates/tools/src/exec/predicate.rs` exists, with the
> refuse-and-hand-back-the-handle-form design and coverage in
> `crates/tools/tests/exec.rs`. T21.3 was recorded JOINED the same day.


---

## R1 — `TranscriptItem::Assistant` has no `truncated` field — **SETTLED 2026-09-11**

Field added with `#[serde(default, skip_serializing_if)]`; a row written before the
field existed reads as `truncated: false` (test pinned). The engine stamps it from the
same formula `TurnOk::truncated` uses — interrupt or `LengthVerdict::TruncatedText` —
so the committed rows and the turn record answer the question identically and cannot
drift. §5.8's steering was already built (`crates/turn/src/steering.rs`); the flag
completes the kept-partial path (`AbortCause::Steering` → `mark_truncated(true)`).
Nothing was left to file as a follow-up.

*Bookkeeping note:* the working-tree change was committed by another agent's
`git add -A` inside `e5a1137` (session-role work) — that commit message acknowledges
the sweep. The content is this item's and was verified before and after.

Verified: `cargo check --workspace --all-targets` clean; transcript 6, turn --lib 63,
turn restore 3, tokencore 40, sessionlog --lib 71, harnessd --lib 45, tools --lib 392,
tui --lib 88, dialect-glm 22, dialect-qwen 24 — 0 failed. Live-model tests were not
run: the server on :8080 serves the operator's own session.

---

## R2 — `ParsedSpan` carries no token offsets — **SETTLED 2026-09-11 — reduced, not deleted, and why**

`ParsedSpan` variants now carry `Range<usize>` into the parsed slice, and
`Parser::parse` takes `reasoning_open` — the channel state the caller owns because it
owns the lead. With offsets in the parser, two things fell out:

- **GLM's final flush now honours its mode**: a turn cut mid-thought is a `Reasoning`
  span, not content. Qwen's parser always did this; GLM's did not, and that defect is
  why `items.rs` re-segmented the stream by control roles after the fact. That whole
  pass — `segment()`, `SegKind`, the unterminated-think rescue — is deleted.
- **A new invariant is tested**: parsed spans tile the token stream contiguously from
  zero (`dialect-glm/tests/invariants.rs`).

`items.rs` went 571 → 506 lines. What remains is span→item grouping, the lead/stop
policies and the coverage invariant — genuinely not parser work, because
`TranscriptItem` lives in `letibot-transcript` and the dialect crate's Cargo.toml
declines dependencies ("no dependencies, and that is the point"). Deleting the module
outright would mean either that dependency or a duplicated item type; reported rather
than done, per the item's own terms.

Verified: workspace `--all-targets` clean; the same eleven crate targets green as R1;
fidelity gate GATE PASS. `engine_decisions.rs` is compile-verified only — waived
because it drives live inference on :8080, which the operator forbade for this session.

---

## R3 — `DialectSpec` has no `ReasoningField` — **SETTLED 2026-09-11**

The spec carries `reasoning_field`; GLM declares `ReasoningContent`, Qwen `Inline`.
It is a dialect-local enum rather than transcript's: the dialect crate's written
zero-dependency contract decides, and `crates/turn` maps between the two at its single
`produce` call, with a keep-the-variants-in-step note on the type. The engine reads it
from the spec; the config parameter is gone from `TurnEngine::new` (now seven
arguments) and `harnessd`'s `Dialect::reasoning_field()` accessor with it.

Verified: `--all-targets` clean; dialect 4, glm 22, qwen 24, turn --lib 63, restore 3,
harnessd --lib 45, transcript 6; GATE PASS.

---

## R4 — `cargo:rustc-link-arg` does not propagate across crates — **SETTLED 2026-09-11**

`docs/build-notes.md` (new): the exec-time failure signature, why link-args stop at
the emitting package, the per-crate build.rs fix, the `LETIBOT_LLAMA_LIB` override,
and which crates are already covered. `crates/turn/build.rs`'s doc comment points at
it (that one-line pointer landed inside `e5a1137` via the same sweep as R1). No code
change, as specified.

---

## R5 — §18.1-I1's observable form is wrong in the plan text — **SETTLED 2026-09-11**

`docs/implementation-plan.md` §18.1 now states the exact hash form
(`H(prompt_N ‖ committed_generated_N)`) as the assertion and demotes the token-count
form to **a measurement of the server**, with the hybrid/recurrent caveat stated —
llama.cpp resumes from a context checkpoint and snaps `n_past` back, measured 48/52
and 38/44 against prompts proven identical over the shared span, so a shortfall there
is not a violation. The generation term is **committed** tokens throughout, with the
stop-token-stripping reason. The §18.2 C3 row and the UNVERIFIED-5 registry entry were
aligned; no `predicted_tokens` remains in the plan. Prose only, as specified.

---

## R6 — Verify what opencode actually sends — **SETTLED 2026-09-11 — mechanism 2 REFUTED**

**Answer: reasoning is replayed fused onto the assistant message as
`reasoning_content` — never as its own message.** opencode stores reasoning as
separate parts, and its OpenAI-compatible converter (`OpenAIChat.lowerAssistantMessage`,
read out of the compiled binary) joins every part of a turn into one
`reasoning_content` field on that same assistant message. The feared extra turn
marker cannot arise from message structure. docs/chat-templates.md §3 is updated; the
residual p99 re-prefill after `interleaved` is mechanism 1 or a third, unidentified
cause.

Raw evidence: `docs/evidence/opencode-reasoning-2026-09-11.md` — provider config
excerpt, the stored part shape from `opencode.db`, the converter code verbatim, and
the chain between them. Two findings beyond the question: the GLM provider points
**directly** at :8080 (the qwen-proxy is not in its path — this file's premise was
stale), and the provider declares no `reasoning` capability flag, which gates
presentation, not replay shape.

*Method, stated because it is unusual:* no wire capture was possible (no root, tcpdump
without capabilities, ptrace_scope 1), so the evidence is client-side storage plus the
sending code, which determines the wire bytes deterministically. Two subagents died
mid-turn on this task — the control-token emission hazard R7 names — leaving nothing;
the third attempt wrote the evidence file incrementally as it went and was done by hand.

---

## R8 — `edit`'s near-miss recovery vs the missing-space artefact — **SETTLED 2026-09-12 — ad23f39**

**The defect was narrower than "find a shorter token".** The model sent
`if!text.is_empty() {` where the file holds `if !text.is_empty() {`. The missing space
fuses `if` and `!text` into one token that occurs nowhere, and `anchor_lines` took a
single longest token with no fallback — the refusal then dumped the top of the file,
which for a 5k-line file is noise, not a hint. A fallback to the *next-longest token*
cannot save this case: there are no other tokens over three characters. The fallback has
to be able to cut into the merged token.

`anchor_lines` now tries whole whitespace-delimited tokens longest-first, and only when
none occurs anywhere splits each on non-alphanumeric boundaries and tries the pieces
longest-first (minimum four characters). `text` and `is_empty` land on the line the model
meant. The near-miss test that previously passed through the file-top dump now exercises
the anchor branch; the new test asserts the R8 case verbatim — the real line and its
number come back, nothing is written.

Done-when: an `old_string` differing from a real line by one inserted whitespace
character returns that line and its number — asserted on the `if !text` case verbatim.
Verified by `cargo test -p letibot-tools --lib` (395/396 green before/after, new and
upgraded tests both pass). Commit carries a live agent's rustfmt reflow of the file.

---

## R10 — layer 1's two env-hygiene lines in the host spawn — **SETTLED 2026-09-12 — 61afc57**

**Done-when, all four clauses.** (1) The spawn `env_clear()`s before setting anything.
One wrinkle the filing did not name: the exec tool passes `env: vec![]`, so a bare clear
leaves the child with no `PATH` at all — every bare name would fail with ENOENT. So
`HostProcesses` captures the parent's `PATH` at construction ("seat time") and sets it
explicitly per spawn: pinned for the session's life, immune to mid-session environment
change, with a fallback default when the parent has none. (2) `BASH_ENV`, `ENV`,
`SHELLOPTS`, `BASHOPTS` are filtered even from a session's own env pairs — the clear
kills what the parent planted, the filter keeps the configuration from re-arming it.
(3) `a_bash_env_planted_in_the_parent_never_reaches_the_child` plants `BASH_ENV` in the
test process, re-adds it via the request's pairs, spawns for real through a real cgroup
tree, and asserts the child sees neither and a pinned `PATH` (both `printf` and
`${var-}` are dash builtins, so the assertion depends on nothing outside the pin).
(4) `surroundings_for` in harnessd now calls `Surroundings::with_pinned_shell` — the
call previously only described — for seats with an exec backend, with a falsifying
`how` sentence; seats without one keep `Unknown`, under which a bare name is `not_run`.
`the_shell_is_declared_pinned_only_where_an_exec_backend_exists` holds the wiring.

Verified by `cargo test -p letibot-tools --lib` (396 green) and
`cargo test -p letibot-harnessd --lib` (46 green). harnessd's harness.rs was staged
surgically: the commit contains HEAD plus the `surroundings_for` wiring and its test
only; the worktree's other harness.rs hunks are a live agent's in-flight work and are
deliberately absent from 61afc57.

---

## R9 — a refusal says `host_other` about a path inside the workspace — **SETTLED 2026-09-12 — 545d9d7**

**Root cause, narrower than "region_of is wrong".** `region_of` places absolute paths
under the workspace fine; what it cannot place is a **relative** path — `under("crates/
tui/src/app.rs", "/home/dead/Projects/letibot")` is false, so the path fell through
every prefix to `Region::HostOther`. The deciding classifier (`path_is_inside`) had it
right; the summary printed the other one's opinion.

**The filing offered two fixes and preferred the second; that is the one taken.**
Option 1 (teach `region_of` the workspace) would have fixed the report by changing
**decisions**: `Region` feeds `settle`'s all-workspace Auto rule and the
Destroy-outside trigger, so relative shell arguments would have started satisfying
rules they currently fail — a policy widening that deserves its own analysis, not a
rider on a reporting fix. Option 2: `Baseline::of_paths` sets `path_decided`, and
`summary()` omits the `over [...]` clause for such actions. The regions themselves are
unchanged and still drive the tier rules.

Done-when met on the rendered payload: a gated `edit` of `src/lib.rs` under a deny
gate renders `reading: ask — intents [write_file]` — no `host_other`, nothing lost
that was decided. New tests: the path-decided summary is silent while its regions
stay intact internally; a shell baseline still reports `over [system_config]`; the
harness-level refusal test. A `deny_all` fixture (with a workspace in its
surroundings) was added to testing.rs. Verified by tools `--lib` 399 green, the full
workspace `--lib` battery green, harnessd `resume` green, fidelity gate GATE PASS.

---

## C1 — compaction, the cache-friendly summary call — **SETTLED 2026-09-13 — dbf4bdc, f0d9c9a, 72e9d52, b1f5240**

Shipped as three commits and measured live on the box. **dbf4bdc** (turn side):
`run_compaction` appends `SUMMARY_INSTRUCTION` as a `SystemOrigin::Update` item and
runs an ordinary turn — which *is* the cached strategy by construction, because the
engine's prompt is the ledger's whole token region and the prefix invariant proves
each turn extends the last. **f0d9c9a** (daemon): `Harness::compact` refuses a
summary that proposed tool calls, flushes the old transcript whole, then
`fork_to_summary` writes a new transcript row linked to the parent (`put_fork`, fork
point = the session log's seq) whose body is one system-update item carrying the
summary verbatim; the session swaps to it; the prefix stays the session's own, the
same rule a resume follows. Nothing is deleted — compaction stops carrying history,
it does not destroy it. Protocol grew `ClientFrame::CompactSession` and bumped to
version 8; `HubSteering` now pops through `try_steering_command`, which leaves
non-steering kinds queued (a compact queued behind a running turn was being swallowed
by the mid-turn pickup and lost). **72e9d52**: `/compact` in the TUI, scoped to the
session the head sits in. **b1f5240**: `CompactReport` carries the summary turn's
numbers, and `compact_live.rs` measures them against the serving GLM endpoint.

**Measured live, temp 0, GLM-5.3-Flash, this box** (opencode's flatten measured cache
0 and ~13 min at 182 t/s on 144,436 tokens): two turns to a 1352-token base, then
compact — the summary turn reported **cached 1352 of 1352 carryable** (full reuse,
395-token summary), the first turn on the fork answered **41, 427, 9001** from the
summary alone, and its prompt was the cold base §3 predicted (cached 0 on a fresh
transcript's first turn — disclosed, not denied). On a two-turn toy the base *grew*
(1352 → 1435): prefix + summary exceeded the short history, which is the honest shape
of the trade — compaction pays when the history is longer than its summary.

Two defects found and fixed on the way: `list_sessions` broke `created_at` ties by
rowid (a fork lands in the same millisecond as its parent; the tie used to be
arbitrary, so a resume could come back on the parent) and now counts only the current
transcript's rows (a cross-transcript count double-counts the dropped history).
Verified by harnessd `--lib` 46 + `compact` 3 + `compact_live` 1 green, sessionlog
`--lib` 71, tokencore 40, tui `--lib` 95, fidelity gate GATE PASS. Live-gated by the
serving preflight; the GLM endpoint is a singleton and this session runs on it — the
test starts nothing and evicts nothing.

Not shipped, deliberately: auto-at-the-wall triggering (needs `docs/compaction.md` §1's
policy, not a constant) and §4's structural map (the instruction already asks for the
shape a map would keep). The simple version the operator asked for is what shipped.

---

## C2 — todos pane: session todos from a model-callable tool, plus the repo's TODO.md

**Operator, 2026-09-13:** *"Both sources."*

Shipped in three commits. **5ecd72e**: the storage and wire half — a mutable `todo`
table in the tokencore store (schema v3, the `set_title` class: one row per session,
FK cascade, the whole list as JSON), `SessionEvent::TodosUpdated` (durable in the
replay scrub: "the list is what it is as of this seq, and a head replaying the
backlog keeps the last one it saw"), protocol version 9 with the `ListTodos`/`Todos`
frame pair. **c8052ec**: the model-callable half — `todo_write`, `Access::Session`
(the exact class `docs/tool-survey.md` §1.4 flagged as the under-declared case),
whole-list replace behind a `TodoBoard` seam; the harness compares the board's
version at every round boundary and flushes the store row and the announcement as
one act, because a pane that could see one without the other would disagree with
itself after a resume. Seated in `m1_orchestrator` (the default seat) and
`m3_researcher` — the two roles with spare seats against §8.4's ceiling; the other
roles keep the intent board's `todo`, a different instrument (op-based,
completion-checked against effects), and unifying them would have meant synthetic
turn ids in the intent trail, so the two schemas say what each is for. **3d874bd**:
ctrl-p opens the pane — section one the session's list, restored from the store on
attach and live via TodosUpdated while open (events arrive Filtered when closed, so
a closed pane never backlogs state it is not showing); section two the workspace's
`TODO.md` parsed into its `##` sections with open/done checkbox counts, re-read
fresh on every open, read-only by design: the file is the operator's to edit, the
pane only mirrors it, and an agent's todo list and the operator's queue are
different things the pane labels as such.

Proven live (`todos_live.rs`, the serving GLM endpoint, temp 0): the model called
`todo_write` on a blunt instruction (2 rounds, 1 tool call, reply "done"), the store
held the list, and a head attaching after the turn found the announcement in its
38-envelope resume backlog — the bootstrap read exists because the attach snapshot
carries items, not events. Verified: tools `--lib` 402, sessionlog `--lib` 72,
harnessd `--lib` 46 + `compact` 3 + `compact_live` 1 + `resume` 2 + `todos` 2 +
`todos_live` 1, tui `--lib` 97, turn `--lib` 66 + `restore` 3 + `engine_decisions`
13 + `compaction` 3, tokencore 42, fidelity gate GATE PASS. The GLM endpoint is a
singleton and this session runs on it; the live test starts nothing and evicts
nothing.

Not shipped, deliberately: a `todo_write` seat in coder/planner/runner — those roles
have no spare seats and already keep the intent board's `todo` as their working
list; giving the pane a writer there is a seat decision, not a default.

---

## R7 — a turn that ends inside its own reasoning is reported as success — **SETTLED 2026-09-13, commit a6b970e**

The live finding replayed as a canned-frame test (`engine_decisions.rs`): a stream
that stops with `eos` — a *normal* stop, which is the whole point — while the
reasoning block is still open, with nothing but reasoning produced. The length
classifier has no opinion on a `stop`, so this turn used to commit as an ordinary
empty one.

The fix is where the facts already live. `items::produce` parses the same tokens the
spans came from, so it now folds `ThinkOpen`/`ThinkClose` over the generated body
with the lead's answer as the seed — `Produced::ended_in_reasoning`, the same
`decoder.control_role` primitive `lead_opens_reasoning` and the stream loop use, so
the item view and the stream view cannot disagree about where the block ended. At the
turn boundary: ended in-reasoning, no visible text, no tool call, and **no abort in
progress** — §5.8's kept partial and the steering interrupt name their own outcomes,
and folding an interrupted mid-reasoning stream into the new failure would misname a
turn we deliberately kept (this gate is what the first test run caught: the urgent-
steering test broke until `outcome.aborted.is_none()` was added). The turn fails as
`TurnFailure::UnfinishedReasoning`, emits a warning that names the check
(`ended_in_reasoning`), still emits `TurnFinished` with the true finish reason,
skips the prefix check (nothing committed, no new prefix), and is bounded by the
salvage budget exactly like §5.7's failures.

The harness answers with steering, not a hard stop — *"your previous turn ended
inside a reasoning block and said nothing; continue or say why not"* — through the
same append-and-continue shape as the length failures, and the startup disclosure
names the check as always-armed: a list of only the optional checks implies the
unconditional ones are absent. One design note: the condition is "no assistant
*content*", not "no Assistant item" — §5.4's coverage fallback emits an empty
`Assistant` item whose tokens must tile, and counting it as content would make the
check miss the empty-think case.

Refuted on the way, and recorded in the TODO entry so nobody retries it:
`--logit-bias` banning GLM's two role-opening EOG tokens measurably breaks the
model's ordinary stop.

## R11 — the audit rows are written and never read — **SETTLED 2026-09-13, commit 74abade**

§4h's input 4, closed. The gate reads its own rows back when it builds the request,
keyed the way the circuit breaker is — `(tool, intent set, effect scope, region set)`
— never on arguments, which change on every re-spelling. `TaskDirection::of_parts`
exists so `request_from` computes the key **before** the request exists: the history
shown in the brief and the row the next call reads back are keyed by construction,
not by two implementations agreeing to stay in step.

The history rides on the request (`AdjudicationRequest::prior` → `ModelBrief::prior`)
and is rendered with the discipline enforced where the brief is built, not trusted to
a prompt: counts and ages together ("admit — 2 time(s), most recent 1 turn(s) ago,
first 9 turn(s) ago"; ages are turn distances, the honest unit for an in-session log,
and an unparseable turn id renders as unknown rather than inventing a number);
denials beside approvals, because aggregation is by the gate's effect (`admit` /
`refuse`) and a brief of only approvals is unbuildable; absence stated ("none
recorded") rather than left for the oracle to guess; and the *"evidence, never
precedent"* line is part of the render itself, so it travels with the data it
constrains. Nothing in the admit path reads the field — the tier is layer A's, and
the test drives two approvals into a `sudo` (privilege-escalation, always-ask)
direction and the third call still asks: tier unmoved, no standing grant offered,
and with nobody to answer, nothing ran.

The raw command text still never reaches the oracle: a `PriorAnswer` carries the
effect, a count and two ages, and nothing else — the existing leak test caught a
brief smuggling once and it stays green.

## §11.3 (A.1) — a notice's TTL in time, not frames — **SETTLED 2026-09-22 — 87b67ee**

`head-parity-2026-09-21.md` §11's item 3, and R13's second symptom. The unit moved and
the number did not: the old constant was 60 **frames**, and leticl measured it on a live
head at 1.6 s of wall time, so `NOTICE_MS` is 1600 and the two heads keep one sentence up
for one second and a half. `say` sets a deadline on the head's own clock, a key moves
that deadline to now (the acknowledgement it always was), and `screen` reads it instead
of decrementing a counter — which also closes the state the old guard made permanent: a
notice whose count had already reached zero could never be cleared at all.

`Resync`'s direct write to the slot went through `say` with it, so there is one writer
and one clock source, and the field's doc records what a note with no clock means. The
criterion is asserted the way §11's entry asked: present at `t + 1600 - 1`, gone at
`t + 1600`, with 200 repaints at one millisecond in between and no `screen()` in the
interval — the mirror of leticl's `a-notice-expires-and-an-alarm-does-not`. leticl's
`chrome.lisp` carries the same 1600 from the same 60 frames, which is the cross-check
§11's entry asked for by name.

## §11.6 (A.2) — a job that never ran is not a job that wrote nothing — **SETTLED 2026-09-22 — e1cd2b0**

`head-parity-2026-09-21.md` §11's item 6. `NotScoped` is the one `JobState` where the
wrapper could not put the process in its cgroup, so **nothing ran** — and its empty window
was drawn with the sentence a process that ran and wrote nothing gets, under a header
that had just said it never started. Both heads had it identically.

The line is `it never ran, so there is nothing it could have written.` — §11.6's ruling
is *A rules the words; both heads render the same string*, so the literal is the whole of
what the two agree about, and it is in the commit message as well as the code.

**The fact travels instead of the sentence.** `JobState::never_ran()` sits beside
`word()` — the companion that answers the question the word alone cannot — and the daemon
puts it on the window (`SessionEvent::JobOutput`) and on the row (`JobEntry`). Both are
`#[serde(default)]`, so an older head ignores them and renders exactly what it rendered
before, `false` is the honest reading of silence, and no `PROTOCOL_VERSION` moves: the
precedent `ModelAdvice::consulted` set for an added, defaulted field on an existing
variant.

The same contradiction was fixed in the two other places a reader met it: the jobs
pane's row (`not run (could not join its scope) · 0 B out · ran 0.0s` — a job with no run
has no duration, so the clause goes and the byte count stays) and the model-facing
`job_output` text.

**The pairing is what keeps it from drifting.** `letibot-tools` names every `JobState`
variant, classifies it, and pins its word literally; the head maps those same words to
the three sentences. A sixth state cannot fall through to a head's sentence, and a reword
on either side of the wire breaks the other's test.

## §11.7 (A.3) — R18's card says why it is asking — **SETTLED 2026-09-22 — 11f07e7**

`head-parity-2026-09-21.md` §11's item 7, and the last of the three A owned. R18's card
was five wrong things; four were fixed, and what was left was the sentence saying *why
the card is up at all*. It printed two statements that read as a contradiction — the
headline's **exec access** (the tool's declaration) and the dim line's **auto** (layer
A's reading of the action) — with nothing joining them, so a card that went up anyway
read as an argument against its own evidence.

**The missing clause is the access**, and the fact had been in the request as
`ActionClass::access` all along without ever reaching the wire. `DecisionRequested`
gains `access` (`#[serde(default)]`, no `PROTOCOL_VERSION` bump — the
added/defaulted-field precedent `ModelAdvice::consulted` set), the daemon fills it from
the class it adjudicated, and the view carries it, so the live and snapshot paths draw
the same card.

Drawn **only where the declaration is what asks** (`exec`), and not for read, write or
network: something else asks for those, and a sentence about the declaration would be
false on the very card carrying it — which is what `because: workspace: /` was. Empty
is a daemon older than the field: no clause, and no guess.

**The clause, verbatim** — dim, two spaces in, under the `detail` line, and drawn only
when the declared access is `exec` and the kind is not a question:

    the access is what asks: a tool declared to `exec` is asked about on its declaration, and the line above is a reading of this action

§11.6's string is a shared one and this one is not — item 7 is *A's, and A's own
wording*, so leticl owes it nothing. It is quoted anyway, for the reason §11.6's is:
leticl's card carries the same two statements with nothing joining them, and a head
that wants to say the same thing should not have to guess at the sentence.

**The tier ladder did not move.** §11.7 records A's `da4a576` refusal to implement *"a
decision already marked auto is not re-asked"* as stated (`mode.rs:622-636` shows it
would make `bash` unaskable for every read command); that refusal stands, and this was
the wording that removes the *appearance* of contradiction without moving what asks.

**That closes A's three of §11's eight** — item 3 (§11.3, `87b67ee`), item 6 (§11.6,
`e1cd2b0`) and item 7 (§11.7, `11f07e7`). Items 1, 2, 4, 5 and 8 are B's.

## R19 (A's half) — a fresh attach does not open with old news — **SETTLED 2026-09-22 — `50914ec` + `d6f2d21`**

`head-parity-2026-09-21.md` **R19**, ruled by the operator on restarting a head and being
met by twelve red lines: *"i dont want to see that on restart."* **Three faults, and A owned
two of them** — the third was already built here, which is worth stating because the ruling
reads as a list of three jobs.

What they saw: four notes — `daemon_stopping`, `compacted` and two `auto_compact` — folded
correctly to three lines each, in the failure colour, at the top of a session that had just
started. **None had been dismissed**, so persisting a retired set would not have helped:
a fresh head plants a snapshot's warnings at position 0 because *everything in a snapshot is
history and none of it is anchored* — true about where it goes and wrong about what it is.

### Fault 1 — history arrives as news (`50914ec`)

The distinction is not age. **A warning is how a head shows a fact ONCE**, and the facts a
snapshot carries are not old, they are **prior**: this head was not there, so replaying them
as though they had just happened puts them above a conversation they did not precede.

A note is therefore told apart by where it came from rather than by an anchor that lies:

    enum Placed { Seam(usize), Before }

`Seam` is the row count the conversation had when it arrived — what `note()` always did, and
where the history walk puts it back. `Before` is a fact that arrived with a snapshot: listed
by `/notes`, counted by `/status`, **not drawn**. Three consequences, all deliberate:

- **`load` sorts rather than replaces.** This head's own notes keep their seam — it filed them
  while watching, at rows of this very conversation — and a note the snapshot also carries is
  not planted a second time beside them (one identity now: `note_key`'s, shared by the walk,
  the listing, the file and the load path).
- **A seam the new transcript no longer has is not a seam.** A compaction or a reseat forks
  the conversation, so a note filed at row 200 of a 250-row transcript is no longer between
  any two rows of this one; it joins the history rather than being drawn at a place that
  stopped existing.
- `/status`'s notes row gained the third number — `12 · 3 retired · 4 from before this window`
  — because *the reader retired it* and *it happened before this head attached* are two
  different reasons for a line not being on the screen.

The four the operator saw are four notes and twelve lines; after this they are four lines in
`/notes` and a number on `/status`.

### Fault 2 — routine is painted as failure (`d6f2d21`)

**The split is a table and it lives in `letibot_sessionlog::warning`**, not in a head: the
codes are the log's vocabulary, the daemon and the turn engine publish through it, and every
head renders it — a split only `letibot-tui` knew would be copied and would drift. §11.6 rules
the same way about `JobState::word`.

The rule, so a code nobody has written yet can be judged: **routine** is a sentence about
something that worked, something the operator asked for, or something the session is doing by
design — delete it and the reader is no worse off. **Failure** is about something that did not
work, did not happen, was refused, or could not be checked. A caveat that a check did not
happen (`reseat_unchecked`, `prefix_check_skipped`, `monitor_wake_not_armed`) is a **failure**
by that rule: the register's job is to say *look at this*, which is right for both.

Routine — drawn dim, with the `·` the head already uses for a factual line, code kept:

| code | why |
|---|---|
| `auto_compact` | the daemon saying it is about to compact, and afterwards what it did |
| `compacted` | the conversation compacted itself, as configured |
| `reseated` | the compaction also picked up the tools this daemon seats; already paid for |
| `frame_capture_written` | the evidence for a refused frame was written where it can be read |
| `frame_capture_disabled` | capture is switched off, so nothing was written |
| `mode_set` | the answer to the operator's own `/mode` |
| `mode_set_next_session_only` | the project row moved; this one cannot carry the point |
| `mode_session_only` | a consented `allow-all` is this session's, deliberately not written to the row |
| `model_endpoint_retry` | the server did not answer and the round is being taken again — one row per outage, carrying the count |
| `model_endpoint_answered` | the endpoint that did not answer came back; said once, with the count and how long it was out |
| `interrupt_idle`, `promote_idle` | their interrupt / promote arrived between turns, with nothing to do |
| `daemon_stopping` | somebody asked this daemon to stop; the session is on disk |
| `resume_note`, `open_note` | the store's own note about what a resume or an open did |
| `reattached` | this head's own: it got its connection back |
| `slash` | a slash command's answer (`slash_refused` is the failure) |
| `imported`, `import_scrap`, `imported_summary` | the import's three routine notes |
| `steering_urgent` | the operator's steering arrived, so the turn was aborted to take it |
| `cache_reuse_shortfall` | a diagnostic of the server's prefix cache; off the screen unless debug |
| `test` | the `testing` helper's code; there is no production fact behind it |

Failure — red, prefixed `!`: `turn_failed`, `context_wall`, `auto_compact_skipped`,
`auto_compact_no_progress`, `auto_compact_failed`, `length_empty_turn`, `length_batch_refused`,
`ended_in_reasoning`, `repetition_collapse`, `reasoning_stall`, `prefix_divergence`,
`prefix_check_skipped`, `ledger_chain_mismatch`, `row_coverage_gap`, `record_item_pairing`,
`reseat_unchecked`, `monitor_wake_not_armed`, `reseat_refused`, `frame_capture_failed`,
`secret_late`, `mode_unknown`, `mode_set_refused`, `mode_unpersisted`, `flowy_not_seated`,
`answer_unclaimed`, `job_output_refused`, `slash_refused`, `session_unavailable`,
`transcript_store`, `decision_corpus`, `title_not_stored`, `resume_failed`, `import_no_db`,
`import_no_session`, `import_failed`, `fabric_refresh_failed`, `log_gap`, `unreadable_frame`,
`protocol_skew`, `orphan_body`, `sudo`, and four that exist only in fixtures (`gate`,
`gate_timeout`, `gap`, `guard.empty`).

**A code the table has never heard of is a FAILURE**, and that default is the point: it is
drawn loudly, so forgetting one costs a red line somebody asks about rather than a quiet line
nobody notices.

**The guard reads this tree** (`crates/sessionlog/tests/warning_codes.rs`) — the instrument
§11.5 ruled for the last table that had this shape. It finds `Warning { code: "…" }`,
`Warned { code: "…" }` (including a `code:` whose value is an `if`/`else`, which is how the
daemon says `slash` and `slash_refused` from one call site) and `import_note("…", …)`, and
fails until each code has a row. It cannot see a code that reaches the wire through a
variable — the engine's `code: trip.code`, the prefix check's `code` from `check.warning()`
— which is why the table's own doc says so and why the default is loud. **Falsified by hand**:
a stray `code: "unheard_of_yet"` in `harnessd/src/sessions.rs` fails it, naming the file.
Two vacuity guards, because a scan that found nothing passes every assertion: a floor on the
count, and a named few — one per shape and one per emitting crate.

### Fault 3 — a dismissal must survive a restart: **already A's, and now proved harder**

A writes the retired set to `~/.config/letibot/head.toml` (`prefs.rs`, keyed
`hash(code|ts|detail)`, capped at 512) and reads it in `load_prefs()` **before** the attach,
so it was never the fault on this side — §11.2's ruling that *restart is a different
requirement* was about B. The R10 test moved with fault 1 and got stronger: the incident is
delivered **live** to a head whose retired set came off the file, which is the one delivery
nothing else suppresses — a snapshot's copy is prior, and a redelivery is not filed twice —
so what stops it is the retirement and nothing else.

### What the operator has to do, and what B has to do

**Only the head changed.** Unlike §11.6 and §11.7 this carries no wire field, so no daemon
restart is needed to see it — but the head binary must be restarted, and it is 11:19 rather
than the 07:13 the operator is running.

**B's half is the same three faults in `leticl`**, and the codes are not all shared: each head
emits some of its own. The split above is A's answer to *"decide the severity split with your
own codes rather than mine"*; where the two heads disagree about a code they both emit, that
is a drift row before it is a commit.

## R20 — the ladder is pinned to the bottom, and the wall above it scrolls — **SETTLED 2026-09-22 — `a413f9a`**

`head-parity-2026-09-21.md` **R20**, ruled on a permission card carrying a giant `replace` or
a commit message: *"I'm shown a permission prompt and I just cant see the selector."*

**The mechanism was the screen-fit loop's `dec_rows -= 1`,** and it trims **from the end** —
which on this card is the ladder, the deadline and the hint. So the rows the operator had to
act on were the first given up, and what stayed was the wall at full length. **Measured
first**, 80x24 on a card whose content is 42 rows: the headline, the target, and nineteen
lines of layer A's reading, with the three options **gone** — and the composer still there,
so the screen held a question with no way to answer it.

The card is two lists now (`decision_card`), split at the ladder: **content** (the question,
the target, the reading, the §11.7 clause, the `because`, the model's verdict) is the
viewport — the fit loop shrinks it and nothing else of the card, and `card_window` gives it a
window plus **one seam row** saying how many lines are out of view and which key moves. The
seam changes ends with the scroll and never says *"there is more"*: a reader has to know
whether one line or four hundred are missing before deciding to scroll at all, which is
`OutputSlice::denominator`'s rule for a job's output. **choices** (the ladder, the §1.6
deadline, the hint, the `deny_and_tell` line) is never trimmed and never scrolled.

Scrolling is `pgup`/`pgdn`/wheel, taken by the card **only while it has something out of
view** — the numbers are the last frame's, because the length is a function of the width and
only the draw knows it. When the content fits, nothing fires and the transcript keeps the
keys it always had, which is asserted. The offset resets in one place (`screen`, on a change
of `open[0].req_id`), where one site cannot be forgotten. **leticl must check its own fit
path** — a faithful port of that loop has the same order.

## R21 — a shell that only reads is a read — **SETTLED 2026-09-22 — `76e1b31`**

`head-parity-2026-09-21.md` **R21**, raised looking at a gate on `head`. Both halves of the
complaint had answers and neither was the defect: `head` **is** a read in the classifier, and
the oracle was never asked to allow it. **The defect was that the vehicle outranked the
work** — running anything through a shell is exec access whatever the program does, so a call
whose every computed intent was a read still gated as exec and the card said *exec access*
over intents that said *read*.

**Measured first**, over every `bash` call this box has gated (7318 rows, the command text out
of the store): **1008 rows (13.8%) are read-only by the rule, and 977 of the 5153 rows that
were ASKED — 19.0% — stop gating.** The tempting reading of *inside the boundary*
(`Tier::Auto`'s own) would have freed 7 of 7318, which is why the rule asks the narrower
question it does.

One function, `judged_access`: a shell call whose baseline `reads_only()` is judged at
`Access::Read`, and that is *narrowing only*. Three readers, the three places the vehicle
spoke for the work — the `ActionClass` (card headline, shape key, corpus class), the exec
clause above the mode arm, and `Mode::admits_unasked` (a read is clause 4 and is admitted at
every point, `always-ask` included, which is what clause 4 *is*).

`Baseline::reads_only`'s conjuncts are the ruling's own guardrails: every intent a look or a
read, nothing unresolved, nothing empty, and **no region this classifier can point at and
call outside**. That last one is a judgement and it is not `Tier::Auto`: `Auto` requires every
region to be `Workspace` or `None`, and a **relative path** lands in `HostOther` — the
classifier holds the workspace, not the command's working directory, and `cd X && reader` is
exactly the shape the ruling names (the operator's own card read `over [workspace
host_other]`). Reading an unplaceable path as an outside one is the defect R9 already paid
for, so the rule voids on the regions the table **names as outside** — `Secret`, `Remote`,
`Home`, `SystemConfig`, `SystemBinaries`, `Device`, `Temp`, `Root` — and `cat /etc/passwd`
keeps the exec access it has today. **Which makes this stricter than the `read` tool**, whose
gate is never consulted at all; the asymmetry can only narrow what runs unasked.

**What it deliberately does not free:** `sed -n …` computes `read_file` **and**
`execute_code` (GNU sed's `e`, `s///e`, `w`), so the 1690 corpus rows carrying a `sed`
segment still ask. The table is the judge of what a program is and this adds nothing to it —
an unknown program, a pipeline into a writer and anything the parser could not resolve are
all still not reads, and the last is *refused* rather than asked.

**Where it lands in the family**, and the operator asked for this to be named: **R18 was one
accessor answering a different question; `NotScoped` is one integer carrying two meanings;
this is one declared access outranking the intents the classifier actually computed. In each,
a fact about the MECHANISM was allowed to stand in for a fact about the WORK.** The fourth
member is the same family one layer down — the oracle being asked *did the operator ask for
this* when the operator's own trail is not the only source of intent — and R21's sibling below.

**Six fixtures in three crates** used a read-only shell call as a generic *"a call that
asks"* (`cat /w/src/lib.rs`, `ls`, `grep`); each is a `sed -n` or an `awk` now, with the
reason at the site.

## R21's sibling — a command that exits 125 ran — **SETTLED 2026-09-22 — `06ec17b`**

`JobState::NotScoped` was derived **from the exit code alone**, and 125 is a legitimate exit
code: `bash -c "exit 125"` was listed `not run (could not join its scope)`, the head drew
§11.6's *it never ran…*, and the model read the state word and concluded *"the 125 exit code
was never produced"*. Handed over by leticl, who found it and did not choose a fix.

**Chosen: require the wrapper's marker before classifying** — because that is what the layer
next door already does. `Bwrap::launcher_failure` refuses to classify a launcher failure by
exit code at all and reads the launcher's stderr instead; the scope wrapper, which owns its
own code and so could have, took the shortcut. **A launcher failure is recognised by what the
launcher SAID.** The alternative (a marker *file* in a harness-owned directory) is stronger in
the abstract and costs a directory with its own lifecycle inside a tree whose `prune` counts a
leftover file as debris — for a case that cannot arise, since a wrapper that could not join its
cgroup ran no command, so the marker is the entire output.

`NOT_SCOPED_MARKER` is one string read by both ends (the script is built from it; a test fails
if either is edited alone), `Capture::contains` is the substring test, and `EXIT_NOT_SCOPED`
stays as the code — *not the evidence*. Reproduced first and falsified in both directions:
`bash -c "echo ran-anyway >&2; exit 125"` reads `exited 125` with its own stderr in the
capture, and a scope that cannot be joined still says *not run (could not join its scope)*
with the marker present and the command's own line absent.

## R12 — an oracle that ran out of room is not an oracle that could not be read — **SETTLED 2026-09-22 — `3d8754d`**

The operator *"an oracle that ran out of budget is not an oracle that could not be read"*, and
they asked for the measurement first. **Measured against the store**: 175 rows recorded *"gave
no verdict this seam could read"* — **134 were the model ANSWERING `UNSURE`** (R11's defect,
re-worded by `452d7dd`), **41 had no verdict line at all, and 38 of those end mid-clause** (a
word, no full stop — the ceiling's signature; 21 in this session's daemon, 15 in the other
head's, 2 elsewhere). The newest two are from today, so it is live. 848 consultations since
R11, of which 3 recorded the sentence.

Three things. **The verdict goes first** — the prompt asks for it on the FIRST line with the
reasoning after, and the parser reads *either* end, so a cut reply still carries its answer and
every reply written under the old shape still parses. Measured live against this box's guard
model (`qwen-3.8-27b` at 127.0.0.1:8080): three of three samples put `ALLOW 0` on line 1 with
`finish_reason: stop`, and at `max_tokens: 8` the reply is `ALLOW 0\nYes, running the` with
`finish_reason: length` — the verdict survives the cut, which is the whole point. **The third
outcome is named** — `UnsureKind` (`could_not_decide`, `between_thresholds`, `unreadable`,
**`out_of_room`**) travels on the answer and becomes a column, `oracle_reading`, schema
**v11**, so *the rate of each* is a `GROUP BY` and not a regex over prose; the card already
carried the sentence, because R11 put the reply's basis on the wire. **And the knob the
sentence names is real** — `--oracle-max-tokens`, plumbed to `HttpOracle::with_max_tokens`.

`finish_reason` is the only thing that can tell a cut reply from one that stopped, and an
absent field reads as `false`: a server that does not report it must not be read as the
loudest case. The ceiling explains a *failure to parse* and never overrides a verdict.

Found on the way: `decisions_tool::counts_are_per_session_and_any_widens_to_everything` was
**already failing at HEAD** — the R11 fixture gave all three rows `consulted: Some(true)`, so
*"one oracle answered"* measured 3 — and the two rows a person and a boundary decided now say
`false`.

## N6 — a `harnessd` quit while a subagent is mid-turn dies of SIGSEGV at exit — **SETTLED 2026-10-10 — `3b7db2b`**

**Found 2026-10-10** while landing `agent/the-exit-that-crashes-2` (`382a8c6`), which fixed
the test binary's half and filed this one rather than smuggling it in. The measurement was a
core, not a theory: `cargo test -p letibot-harnessd --test message_between_turns` died of
SIGSEGV at exit on a minority of runs with **all five tests printing `ok` first** (2/60 on a
quiet box). `coredumpctl info <pid>` read it in one frame: the main thread in `exit()` →
`__run_exit_handlers` → `_dl_fini` → `__do_global_dtors_aux` (libggml-cuda.so.0) →
`libcudart.so.13`'s own destructor → `_int_free_chunk`, while the faulting thread was in
`unicode_byte_to_utf8` ← `unicode_regex_split` ← `llm_tokenizer_bpe_session::tokenize` ←
`HarnessTaskRunner::run_to_completion`; `si_code` 1 (SEGV_MAPERR), `si_addr` 0x3.

**The daemon had the same path and nothing had walked through the door.** `cli.rs` ran
`daemon.run(...)` until the registry closed, then `drop(merge_queue)`, `firecode::down_all()`,
`daemon.shutdown()` — which DOES close every hub, so a child parked in `serve_child` leaves at
once — `drop(sessions)`, `Ok(0)`, which `bin/harnessd.rs` and `letibot daemon` both turn into a
process exit. The child that survives a hub close is the one **mid-turn**: closing a hub does
not end a turn already in flight, so it was still tokenizing when the finalizers ran.
`382a8c6` had given the tree `Harness::wait_for_children` and `Harness::stop_children`; no
shutdown path called either.

**What landed.** `Sessions::drain_subagents(timeout)` — stop each session's children, then wait
for their threads — called from `cli.rs` on both exits, after `daemon.shutdown()` and before
`Ok(0)`. The order is the whole of the first half: a wait that ran first would spend its bound
on a child nobody had told to stop, and a child between turns would never be told at all. The
wait is `Harness::wait_for_children`'s, which already covers each session's whole tree.

**The bound is 5 s, and it is argued where it lives** (`SUBAGENT_EXIT_GRACE`). Three kinds of
child answer at once — a parked one leaves on the closed hub, one between rounds leaves at its
next round boundary (the round loop already polls its steering on 250 ms slices), and one in a
tool call leaves when that call returns — so several times the slice covers everything the
interrupt can reach. What is left is the child inside ONE model call or one long tool, which
cannot be interrupted at all and has been measured in minutes on this box; a bound large enough
for that would make every Ctrl-C wait minutes for a case a larger number cannot fix anyway.
Past the bound the exit gives up, names the session, and takes the crash risk rather than the
hang — the same ruling `drop(merge_queue)` already makes one line up.

**The log says which of the two it did**, which was this item's own done-when.
`drain_subagents` answers `SubagentExit::Waited { told }` or `GaveUp { sessions, waited }`, and
`sentence()` is the line to print: `None` when nothing was running, so the ordinary quit stays
silent, while a quit that HAD to wait and one that could not are told apart. A give-up names
the sessions rather than counting them, because the session still computing is the one somebody
can go and look at.

**The comment that was the bug's alibi is fixed in the same change.** `cli.rs` said
*"`std::process::exit` above runs no destructors"* — true of Rust destructors, false of the ELF
finalizers `exit()` runs, and it now says which half is which.

**Tests.** `the_daemons_exit_waits_for_a_child_and_gives_up_on_a_bound` builds a `Sessions`,
spawns a real child through the session's own runtime, holds its turn open with the stub's
gate, closes the registry the way the daemon does, and asserts the first drain spends its
300 ms bound and names the session — then, after the release, that the same call is a clean
`Waited` and returns at once. **What it does not stage is a real `harnessd` process exiting**:
that `exit()` is not something a unit test can watch and the crash it would take is a race, so
what is tested is the call and the bound, where both defects would live.
`the_daemons_exit_returns_promptly_when_nothing_is_running` is the one that catches a hang — an
empty tree must not cost the grace — and `the_subagent_exit_wait` (sessions.rs) pins the naming
and the silence.

---

## R97 — A command inside a measured boundary is admitted without a card (D16) — **SETTLED 2026-10-27, branch `boundary-admits`**

R98's first half came with it (the amendment of the same day), **behind a switch that is off by
default**: on an **unconfined** host, a command that runs the project's own code is not settled
by a grant — but only where `[permissions] ask_unconfined_code` says so in a `leticode.toml`.
The operator, on the first cut shipping it on: *"asking on every build/test on the Mac would
just push them to allow-all (which they already run, unsandboxed)"*. R98's second half — a
sandbox on macOS — is not this entry and not this slice.

**What was built.** The gate's `admit` gains one arm before the mode (`gate:boundary`): at
`writes-allowed`, an `Access::Exec` call is admitted with no card when `Boundary::encloses`
holds — every required namespace entered, **both seals `Held`**, egress `Denied` — and the call
carries no `network` intent and no region outside the session's own work. The row's basis begins
*"admitted: inside the measured boundary (D16)"* and names the mechanism that measured it; the
operator is also **told**, one transcript line per admission (`HubAdmissions` →
`Harness::flush_gate_admissions`, at the round boundary so it never lands between a call and its
result). Beside it, R98(1): a call that runs a test runner, a build that executes project code
or a script in the checkout, on a host with **no** boundary, offers no standing rung on its
card, is not settled by a session grant, the shape cache or a `permission` `allow`, and writes
none of them — with the switch on. With it off, nothing about an unconfined host changed. The
predicate is `crates/tools/src/project_code.rs`.

**Measured, and what the tests pin** (`adjudicate::tests::boundary`; `project_code`'s own):
`cargo test`, `bash scripts/check-fmt.sh` and `rm -rf target` are admitted inside a measured
boundary with the audit row saying why; `curl`, `cargo fetch`, `git push`, `gh`, `sudo`,
`shutdown`, a write outside the checkout, a missing namespace, an open seal, **an unverified
seal**, a boundary with egress to the host, and the whole set on an unconfined host all still
ask; `always-ask` still asks and `allow-all` admits by **its own** rule, not this one; a runner
on an unconfined host is asked about twice in a row however it was answered the first time —
with the switch on — while `cargo fmt` in the same fixture is still settled by its shape, so
neither half is vacuous; and with the switch off (the default) a seeded shape and an
`allow_session` answer still settle a runner, which is `the_default_leaves_an_unconfined_host_as_it_was`.

**The corpus numbers.** Exported from three hosts (`mac` 32,108 decisions, `lab2x1` 155,192,
`lubuntu1` 630) and swept by `cargo run --offline -p letibot-tools --example d16_corpus`, which
uses the gate's own `a_command_that_crosses_nothing` and `project_code::runs_project_code` rather
than a restatement: **D16** would have settled **80 of the 270** cards people answered at
`writes-allowed` (mac 69 of 251, lab2x1 11 of 18, lubuntu1 0 of 1). **R98(1)**, if switched on,
would take back **986** standing-answer settles over project code at points that would otherwise
ask (4,671 in total across the three hosts, most of them at `allow-all` where the mode admits
anyway): mac 171 of 834 — 137 of those the session's own programs, 24 `cargo test` — and lab2x1
815 of 3,837. The per-runner table is in `docs/boundary-and-adjudication.md` §6.

**Two facts found on the way, both fixed here.** `cargo fetch` carried no `network` intent —
the table named `install`/`publish`/`add`/`update` and not `fetch`, so a gate asking *does this
need the network?* answered no about the one command whose whole job it is. And the gate had no
channel for an admission at all: an admit left a row in the audit and nothing that scrolls,
which is why D16's *"so the operator can see it happened"* needed the sink above rather than
the row alone.

**The corpus replay was the last thing to land** and it is no longer a gap: the operator copied
`scratch/corpus/{mac,lab2x1,lubuntu1}.json` into the worktree, and the numbers above are what
`corpus_replay::unasked` reports over them. Nothing under `scratch/` is committed (it is
gitignored, and the corpus is the operator's).

**Still open?** `git log --oneline -S"gate:boundary" -- crates/tools/src/adjudicate.rs`.
---

# Settled 2026-10-30 — the twenty entries that said "BUILT on `<branch>`, not merged"

Every one of those branches is gone (`git rev-parse` answers nothing for all twenty), so none of
them was a queue anybody could pick from. Each entry was re-checked **in the tree**, not against
its own prose: the behaviour is here when a type, a function, a key or a warning code is here, and
the strongest evidence is a test that already exists — the ones marked **RUN** were run for this
pass (private `CARGO_TARGET_DIR`, `--offline`). Where an entry stated a `done when`, that is the
standard it is judged by; where it did not, its own "what landed" list is.

The entries as filed — every measurement, every argument, the whole record — are in `TODO.md` at
commit `4fb39324`, which is where to read them; this is their verdict, what main has, and what the
entry listed as left (kept so it is not lost, and so the next reader does not re-measure it).

Of the twenty: **eighteen settled, one settled in part (R70), one still open (R73 — the leticl
half of the compaction entry, which stayed in `TODO.md`), none could-not-establish.**

**Verified 2026-10-31**, independently of the pass above: every symbol these entries cite was
re-read in the tree, and the tests were run (`LETIBOT_REQUIRE_APPARATUS=1`, a private
`CARGO_TARGET_DIR`, `--offline`) — `restart_restore` 19/19, `session_lifecycle` 7/7,
`term_pane_live` 13/13, `child_nag` 4/4, `clear_context` 2/2, `context_claim` 4/4, `nag_pause` 3/3,
`delivery` 5/5, `metrics_instruments` 2/2, `turn_tail` 2/2, `worktree_build_cache` 3/3,
`input_loop` 3/3, `background` 17/17, `feedback` 1/1, `letibot-metrics` 5/5, `todo` 98/98,
the tui's `settings` 30/30 — plus the lib tests named in each entry (serve, gate, capacity, desk,
context, delivery, surfaces, hits, bindings, pane, registry) — all green. A few line numbers are
off by one or two (`REVIEW_SETTABLE`, the review card's two warning rows, `Screen::scrolled`);
every symbol is where the entry says it is. Four sentences were wrong and are corrected below:
**R68** (the release half is in for children, not for roots), **R70** (its own size rule is not met
— six inline blocks are over 800 lines), **R76** (one cited test has no `#[test]` and never runs)
and the count in this paragraph.

---

## Store busy — a session must never die of `database is locked` — **SETTLED 2026-10-30: every write outlasts a busy lock, and `persist` waits instead of dying**

*The entry claimed:* a child died on `sqlite: database is locked`; `store-lock-robust` fixed it by
retrying single-statement writes and `BEGIN IMMEDIATE` transactions with backoff, chunking
`append_items`, and making `Harness::persist` wait.

**Main has it.** `crates/tokencore/src/store/busy.rs` — `is_busy_sql`, `backoff` (50 ms doubling),
`retry_busy`/`retry_store`/`retry_sql`, `BUSY_RETRY_TOTAL` (exported at `store.rs:94`);
`Store::write_tx` (`store.rs:4468`); `Store::append_items` with `APPEND_CHUNK = 64` and one
`BEGIN IMMEDIATE` per chunk (`store.rs:3269`, `:3327`); `Harness::persist`'s wait, which says
`store_busy_wait` at most every two minutes (`harness.rs:16320`, `:16424`).

Two of the entry's four "still open" items are closed as well: the re-render copy now goes through
`append_items` (`harness.rs:5713-5726` — the comment there names the 2026-10-12 six-and-a-half
minute write lock), and `crates/turn/tests/restore.rs` no longer copies the live store (a
`LiveView`; the doc at `:69-86` records why the copy was wrong).

**Tests — RUN:** `store::busy::tests::an_append_outlasts_a_lock_held_past_the_busy_timeout`,
`store::busy::tests::a_long_copy_commits_in_chunks_and_lands_every_row`,
`store::subagents::tests::every_write_outlasts_a_lock_held_past_the_busy_timeout` (8 passed,
0 failed).

*Left:* `synchronous = FULL` is still the store's deliberate choice (`store.rs:63`, `:1930`) —
`NORMAL` was the operator's call and was not taken; and there is no harness-level test that drives
`persist` through a >90 s busy store (the store-level tests cover the retry).

---

## R61 — Review and merge are two queues — **SETTLED 2026-10-30: two tables, one lease per repository, a verdict bound to a head**

*The entry claimed:* review and merge were one row served by every daemon's worker; the branch
built two queues, a per-repository lease, conditional moves with history, a verdict bound to the
commit it judged, a deadline'd gate and a pane.

**Main has it.** `crates/tokencore/src/store/queues.rs`: `review_queue` (`:61`), `merge_queue` with
its `repository` column, `queue_transition` (append-only by trigger), `queue_lease`, and `Move`
restricted to `REVIEW_SETTABLE` (`:620`). `crates/harnessd/src/mergequeue/`: `serve.rs` (the
per-repository worker and its supervisor), `lease.rs`, `verbs.rs`, `desk.rs`, `gate.rs`,
`gate_host.rs`, `review.rs`, `notices.rs`, `capacity.rs`, `carry.rs`, `setup.rs`. The pane:
`crates/tui/src/app/tests/queue.rs` (two tabs, lease age, health, time in state).

**Tests:** `serve::tests::a_red_target_lands_nothing_but_its_fix_and_goes_first_in_review`
(`serve/tests.rs:914` — R61's own done-when),
`a_target_that_moves_during_the_gate_is_rebased_onto_and_gated_again` (`:533`),
`the_landing_pushes_then_installs_and_a_failed_push_installs_nothing` (`:819`),
`a_new_lease_holder_recovers_what_the_old_one_held_and_the_old_one_stops` (`:2595`).

*Left:* a refusal whose root session this daemon does not hold is raised
`refusal_undelivered` and nothing resumes it later; the pane's per-row history is the snapshot's
last 60 moves (the full history is in `queue_transition`); and the entry's three "confirm" items
are the tree's behaviour now and still unconfirmed by the operator — `/queue clean|rm` withdraw
rather than delete, `red_target` defaults to `fix`, a landed branch is deleted with `git branch
-D` when its head and worktree are clean.

---

## R86 — A reviewer with no free VM slot is not an attempt — **SETTLED 2026-10-30: a capacity refusal is a wait, and a block names its attempts**

*The entry claimed:* three reviewers at one head died on firecode's tap shortage and spent R61's
whole attempt budget in three minutes; `review-capacity-and-block-reason` made a capacity refusal
a wait that counts no attempt, and made `queue_blocked`/`queue_needs_human` say what each attempt
was.

**Main has it.** `crates/harnessd/src/mergequeue/capacity.rs` — `vm_capacity` (`:78`, the two
phrases and the zero-taps guard), `retry_after_ms` (`:117`, 1/2/5 then every 5 min), `streak`,
`branch_wait`, `gist`, `decisive_reason`, `undecided_heads`. The warnings:
`queue_waiting_for_vm` (Routine) and `queue_vm_slots_exhausted` (Failure) in
`crates/sessionlog/src/warning.rs:480-481`, and the head's card rows
(`crates/tui/src/ui/transcript/review_card.rs:207-208`).

**Tests:** `mergequeue/review/capacity_tests.rs` —
`a_reviewer_with_no_free_vm_slot_waits_uncounted_and_is_reviewed_when_a_slot_frees` (`:295`),
`a_wait_past_thirty_minutes_raises_once_with_firecodes_sudo_lines` (`:369`),
`a_real_placement_failure_still_counts_and_the_block_names_each_attempt` (`:539`),
`three_could_not_decide_verdicts_are_named_with_their_heads_and_the_decisive_reason` (`:586`).

*Left:* the timer is the only trigger — a VM the daemon knows has ended does not wake the row;
and `notices.rs`'s parked-row text for a model reads `failure`, which now carries the full blocked
sentence.

---

## R68 — One session lifecycle, one owner per session, one turn tail — **SETTLED 2026-10-30: one owner and one turn tail are in, and the release half is in for children — not for roots**

*The entry claimed:* a child's hub had two readers, nothing was ever released, the turn tails had
diverged and busy traffic starved the idle work; `r68-lifecycle` removed the classes.

**Main has it.** `letibot_sessionlog::hub::Owner` (`hub.rs:707`) and `Hub::try_command`, which
answers `None` for a hub it does not own (`:2220`); `Registry::remove` (`registry.rs:1865`);
`ChildSessionGuard` (`harness.rs:19140`); `Config::child_park_after` (`config.rs:838`, default
600 s) with `Sessions::park_idle_children`; `crates/harnessd/src/turn_tail.rs`, the one tail every
door and `serve_child` call, with `tests/turn_tail.rs` asserting one list against five daemon doors
and a child's prompt.

**Tests:** `registry::the_worker_never_takes_a_command_off_a_hub_owned_by_its_own_thread`
(`registry.rs:2772`), `session_lifecycle::a_prompt_to_a_live_subagent_is_served_by_its_own_thread_only`
(`tests/session_lifecycle.rs:411`), `fifty_finished_subagents_leave_no_thread_and_no_registry_entry`
(`:574`), `a_message_to_a_parked_child_reopens_it_from_the_store` (`:703`).

*Left:* the `report(session, code, detail)` sink was never built — stderr-only `eprintln!` and
`let _` sites remain; and R68.7's retry storm is quiet rather than gone: the gatekeeper's open is
still retried on every review pass, and the failure is said once per distinct reason
(`sessions.rs:3406-3431`, `gatekeeper_said`).

And two residues of the release class, from the entry's own "not done" list — **"nothing is ever
released" is gone for a CHILD, not for a root**: a root session is still never closed by the daemon
(its lifetime is the operator's), so a resumed root keeps its harness and its `monitor-wake-*`
thread until the daemon stops — the park rule walks children only
(`Sessions::park_idle_children` filters on `registry.parent_of(id).is_some()`) — and
`HostProcesses`' `reaps`, `promotions` and `Deadlines.fired` are still unbounded push-only vectors
(`exec/host.rs:921`, `:924`, `:734`, pushed at `:1513`, `:1518`, `:867`).

---

## R69 — One surface stack, hit-testing against what was drawn, per-surface key tables — **SETTLED 2026-10-30: the three are in, and `!term` mouse mode with them**

*The entry claimed:* draw order, key order and hint order were three hand-written lists that
disagreed, and each clickable surface did its own hit-testing; `r69-surfaces` replaced them with
one stack, one hit record per frame and one binding table per surface.

**Main has it.** `crates/tui/src/app/surface.rs` (the stack, `drawn()`, `owns_enter`),
`app/hits.rs` (the frame's `(Rect, Target, rows→stops)` and the one click router),
`app/keys/bindings.rs` (`Binding`, `Surface::bindings`, the bar and help generated from them),
`tests/input_loop.rs` (real bytes through the driver).

**Tests:** `app::tests::surfaces::{with_two_panes_open_the_keys_reach_the_one_drawn (surfaces.rs:30),
a_pane_under_the_editor_does_not_swallow_the_composers_enter (:94),
the_queue_pane_owns_enter_even_when_it_is_empty (:113)}`, plus `app/tests/hits.rs`,
`app/tests/bindings.rs` and `tests/input_loop.rs`.

The entry's one **not started** item — `!term` mouse mode — is in main:
`letibot_vt::Screen::mouse()` (`vt/src/screen.rs:413`) records `?9/1000/1002/1003` and `?1006`, and
`TermPane::mouse` (`tui/src/app/term_pane.rs:368-400`) decides each report against it.
**Test:** `pane::a_program_that_asked_for_the_mouse_is_sent_it_translated_to_the_pane`
(`app/tests/pane.rs:1149`).

*Left:* the modal cards (allow-all confirmation, new-todo, password, the `!term close` ask, the
prompt, the key ask) are still first in `App::key` and not on the stack — a second mechanism, and
by the entry's own note one the defect class does not reach.

---

## R70 — Split the largest inline test blocks into sibling files — **PARTLY SETTLED 2026-10-30: the large blocks moved, and six inline blocks over 800 lines remain**

*The entry claimed:* the biggest inline `mod tests` blocks moved to `<module>/tests.rs` or a topic
split, with the test count identical before and after.

**Main has it, in part.** `crates/tools/src/adjudicate/tests/` (mod + boundary, breaker, card, gate,
grants, history, memory, outcomes, supervised, unasked), `crates/tools/src/builtins/todo/tests/`,
`crates/harnessd/src/harness/tests/` (`mod.rs` and fifteen topic files: card_fixtures, children,
citations, context, delivery, fallback, intent, jobs, misc, operator_ask, prefix_on_open, questions,
restart_notice, store_writes, workspace), `crates/harnessd/src/sessions/idle_nag.rs`.

*Left, measured in this tree on 2026-10-31* — every inline `mod tests { … }` block under `crates/`
(source files only; block lines against the file's own): **54 blocks are still over 40 % of their
file, 13 are over 700 lines and 6 are over 800** — `crates/harnessd/src/plugins.rs` is the largest
(2,469 lines, 53 % of a 4,648-line file), then `mergequeue/gate.rs` (1,302), `tools/src/runtime.rs`
(1,281), `tools/src/firecode.rs` (975), `sessionlog/src/protocol.rs` (888) and `harnessd/src/cli.rs`
(863); ten of the over-40 % blocks sit in a file over 900 lines. So **the entry's own rule ("past
~800 lines or ~40 % moves") is not met, and this entry is settled only in part.** The small ones
are as the entry said: 34 of the 54 are 100–370-line blocks.

The figure this entry left behind was *29 blocks over 40 %, all under 700 lines, nothing past 800
inline*; all three of those are wrong in this tree, and `plugins.rs`'s 2,469-line block is the size
the rule was written for.

---

## R71 — A model that stops because "my context is full" when it is not gets told so — **SETTLED 2026-10-30: the claim is checked against the daemon's measurement and answered with the numbers**

*The entry claimed:* models stop with "my context is at the wall" on a guess; `r71-context-check`
detects the claim by similarity, checks it against the session's real fill, and slaps it once per
claim with the numbers.

**Main has it.** `crates/harnessd/src/context_claim.rs` (`FLOOR = 0.25` at `:150`, the calibration
table in the module doc, `LOG`/`REPEATED`), `Harness::context_fill` (`harness.rs:15838`) — the one
function that reads the fill, through R72's `measure_resident`, so the line quotes what the head
shows — the notice in `run_rounds` (no tool calls) and on the step-boundary queue, and
`<workspace>/.letibot/context-claims.jsonl`.

**Tests:** `crates/harnessd/tests/context_claim.rs` — the leticl 11:48 sentence at low fill gets
the line with the numbers and a second round runs; the same at the wall ends in one round with
nothing said; a repeat after the line is one `context_claim_repeated` warning, not a second line.

*Left:* the floor is in-sample and wants tuning from `context-claims.jsonl`; 16 of the 79 labelled
positives sit under it (four with no owner in the window, the rest long subordinate clauses) and
want a second tier, embeddings.

---

## R72 — The wall is judged in a number nobody sees — **SETTLED 2026-10-30: one count, the provider's, said once with its unit**

*The entry claimed:* the head said 740k, the wall said 937k and a restart said 3.45M for one
context; `r72-wall-units` made the resident size the provider's last measurement plus what was
appended since, and printed one unit on every line.

**Main has it.** `WallAnchor` (`harness.rs:17007`), `sent_since` (`:17127`), `roll_resident`
(`:17183`), `Harness::measure_resident` (`:8154`), `recovered_scale` returning the stored pair
whenever the stored ledger is not longer; `crates/harnessd/src/wall.rs` — `WallControl`
(Armed → Awaiting → Escalating → Stuck, and release on a measured count under window − 2×headroom)
with the ladder `Ordinary → SmallerTail → Sliced`.

**Tests:** `harness::tests::context::{a_reasoning_heavy_history_meets_the_wall_at_the_providers_count
(harness/tests/context.rs:81), a_restart_after_a_notice_recovers_the_providers_count_and_does_not_compact
(:137), a_fork_rebases_the_count_and_a_local_session_counts_the_ledger (:181),
the_compacted_line_is_one_unit_said_once (:269)}`; `wall.rs`'s own tests (the live pg-noop case
replayed); `tests/wall_refusal.rs`.

*Left:* no replay of `s-1791151788256943467`'s last rounds from the store (the tests are built from
the logged numbers); which reasoning a provider counts is assumed, not measured, and a per-provider
measurement would settle it; a restart after a compaction and before any round comes back unscaled;
no metered end-to-end test (a `ProviderConfig` cannot be pointed at the canned server); a single
tool result larger than the whole window is still appended whole.

---

## R76 — A red target nobody's model hears, a gate in the daemon's own memory, and no way out — **SETTLED 2026-10-30: items 1–5 and 6–7 are all in main**

*The entry claimed:* a `target_red` went out as a warning no model reads, the model had no queue
tool, nothing re-gated, a rewritten target left a review of a commit that no longer existed, a gate
that could not judge was called RED, and a gate step ran in the daemon's cgroup.

**Main has it.** `RepoQueue::ring_the_projects` (`serve.rs:650`) and
`Harness::queue_emergency_notices` (`harness.rs:10828`; the sentences it builds are
`mergequeue/notices.rs`, the failing step first); the `queue`
tool (`crates/tools/src/builtins/queue.rs`) over `mergequeue::desk::SessionQueue` (`desk.rs:78`)
with `status`/`regate`/`restart`/`withdraw`/`propose_target`; `verbs::regate` and
`verbs::regate_at_start` (`verbs.rs:638`); `mergequeue::setup::setup_failure` (`setup.rs:162`).
Items 6–7 as well: `exec::SiblingScope` (`tools/src/exec/scope.rs:751`), `[memory] gate`
(`mergequeue/gate.rs`), `crates/harnessd/tests/gate_memory.rs`, and `[memory] default`/`ceiling` in
a `leticode.toml` over the user's over the box's (`crates/leticode-config/src/lib.rs:597-663`).

**Tests:** `serve::tests::{a_red_target_is_told_to_a_session_not_only_rung (:1473 — item 1's
delivery), a_judged_tip_is_gated_again_on_request_and_when_a_daemon_takes_the_queue (:1398),
an_amended_target_supersedes_the_review_of_its_old_red_tip_and_gates_the_new_one (:1317),
a_gate_whose_own_setup_fails_after_the_build_is_unjudged_not_red_and_lands_nothing (:1207)}`,
`harness::queue_e2e::a_red_target_is_told_to_the_projects_root_session_once_with_the_failing_step_first`,
`desk::tests::the_model_reads_regates_withdraws_and_only_proposes_the_target` (`desk.rs:824`) — all
**RUN**, green.

**One correction to the evidence this entry cited:**
`serve::tests::a_red_target_and_a_blocked_review_ring_the_project_once_each` (`serve/tests.rs:1499`)
**is not a test.** Commit `7730d482` (2026-10-30, the delivery-of-the-sentence change) inserted a
second `#[test]` between the attribute above `:1499` and the function, so both attributes now belong
to `a_red_target_is_told_to_a_session_not_only_rung` — which is registered **twice** and runs twice
— and the ring test is compiled and never run. `cargo test -- --list` is the check: the name is
absent. The ring's behaviour is covered by the two tests named above; the test itself needs its
`#[test]` back.

*Left:* nobody held → nobody told (a ring finds only root sessions this daemon holds, and there is
no cheap "roots by workspace" read); "once" is per daemon, so a restart tells again; the setup
heuristic is words (a gate declaring a setup exit code was not done); `propose_target` is a warning
plus a history row, not an answerable card; the gate's cap is box-wide (no per-project
`[memory] gate`); and the learned per-command default was not built.

---

## R87 — A gate killed by its cap read as RED, and the alert reached a session that has nothing to do with letibot — **SETTLED 2026-10-30: unjudged is its own verdict, and an alert reaches its repository**

*The entry claimed:* a step that died at 96.8 % of its cap with no failing code was recorded RED
and raised at emergency severity on every hub, including a session about RSS usage.

**Main has it.** `red_cause` (`mergequeue/gate.rs:652`) decides before a step is called red
(`oom_kill > 0`, peak ≥ 95 % with no failing code, a leader ended by a signal, else red); both
kills are `GateVerdict::Infra` with `GATE_KILLED` rows naming the cap, the peak and the key, and
every row now ends `[step: exit code N]` / `[step: killed by signal N]`. The scope:
`mergequeue::repository_of_workspace` (`mergequeue.rs:754`), `QueueDoor::announce_to`
(`serve.rs:84`), `publish_for_repository` (`mergequeue.rs:863`).

**Tests:** `gate.rs::{the_measured_death_near_the_cap_is_killed_at_the_cap_not_red (:2284),
an_oom_kill_counted_in_the_cgroup_is_killed_at_the_cap_whatever_the_output_says (:2328),
a_step_killed_by_a_signal_is_unjudged_and_its_row_says_the_signal (:2501)}`;
`serve/tests.rs::{a_target_whose_gate_was_killed_at_its_cap_is_unjudged_not_red (:2830),
a_daemon_above_a_repository_neither_takes_its_lease_nor_shows_its_red (:3010),
a_repositorys_alert_reaches_its_sessions_and_the_board_and_not_the_home_session (:3162)}`.

*Left:* what signalled cargo at 00:51:10 is still unknown — the next occurrence carries the signal
on its row; and the cap default the entry recommended (*the larger of half of what is free and the
last green run's peak + 25 %*) was the operator's decision and was not taken.

---

## R88 — A worktree child finishes with its work outside its branch — **SETTLED 2026-10-30: the brief, the end-of-turn check and the settlement all name the branch**

*The entry claimed:* seven children in one night ended with their work uncommitted, and the parent
found out by looking; `child-commits-before-done` put the deliverable in the brief, checked
`git status --porcelain` at the end of a turn, and put the salvage line at the top of every
settlement.

**Main has it.** `letibot_tools::builtins::task::worktree_brief` (`task.rs:682`, appended to every
child with a fresh worktree), `harness::deliverable::EndOfWork` (`harness/deliverable.rs:128`) in
the round loop beside the intent nudge and the context-claim check, `Harness::set_deliverable`
(`harness.rs:14091`), and `AnswerOnce` carrying the deliverable so every ending has the line.
The build cache moved out of the tree entirely: `harness::worktree_build_env` sets
`CARGO_TARGET_DIR` through the session's own environment.

**Evidence in the hand:** this session's own brief is that text — *"[the deliverable] You work in
the worktree … commit your work to it as you go and before you finish."* **Tests:**
`crates/harnessd/src/harness/deliverable.rs`'s (clean → no nudge, dirty → one nudge, the two
printed commands leave the branch holding the work, every ending through `AnswerOnce`),
`crates/harnessd/tests/worktree_build_cache.rs` (an empty `git status --porcelain`, a tracked
`.cargo/config.toml` kept byte for byte, `CARGO_TARGET_DIR` in a `bash` call).

*Left:* not exercised end to end with a model (the loop's arm is three lines over `EndOfWork`,
which is tested); a CONFINED child does not get `CARGO_TARGET_DIR` — the main tree's `target/` is
outside its view — so it builds cold, as it effectively did before.

---

## R85 — A daemon restart brings back subagents and monitors, and reminds the model of jobs — **SETTLED 2026-10-30: schema 26's records and the restart door are in main**

*The entry claimed:* a restart lost every running child and every monitor; `subagents-survive-restart`
recorded each child, monitor and job context, and brought them back under `--restore-within`.

**Main has it.** `crates/harnessd/src/restart.rs`, `subagent_record.rs`,
`Config::restore_within` (`config.rs:851`, default `restart::RESTORE_WITHIN` = 3600 s), the
`task_resume` tool (`tools/src/builtins/task.rs:2168`), `restart_restored`/`restart_reminded` on
the hub (`harness.rs:11558`), and the store's `subagent_run`/`session_monitor`/`job_context`/
`daemon_life` (`crates/tokencore/src/store/subagents.rs`).

**Tests:** `crates/harnessd/tests/restart_restore.rs` —
`a_child_running_when_the_daemon_stopped_resumes_and_its_answer_reaches_the_parent` (`:450`),
`past_the_threshold_a_child_is_only_reminded_and_task_resume_brings_it_back` (`:1170`),
`a_grandchild_comes_back_when_its_parent_does` (`:1272`),
`monitors_are_rearmed_and_jobs_reminded_and_the_reminder_is_a_turn` (`:1642`),
`an_empty_restart_is_said_once_and_starts_no_turn` (`:1532`).

*Left, as the entry listed them (all limits, not defects):* a daemon of an older build left no
rows; a VM child that worked on a copy is reported, not resumed; a re-armed monitor's TTL starts
again; monitor re-arm runs without the permission gate; the queue's reviewers are not recorded.

---

## R77 — An operator's todo lands last and the nag never names it — **SETTLED 2026-10-30: the operator's row is the one the check names, and the first check says so**

*The entry claimed:* an operator row added at the end of a board was never the row a plan-check
named — the model was told "(N more open)"; `r77-operator-todos` ranked the operator's ready rows
first and said *"the operator added"* on the first check.

**Main has it.** `todo::the_row_to_name` (`crates/tools/src/builtins/todo.rs:1003`, `:1689` region:
operator's ready rows first, then the model's sticky `chosen`, then the head of the ready set);
`OPERATOR_ADD_NAG_AFTER` = 10 s (`sessions.rs:133`) with `TodoNagClock::operator_added`
(`sessions/idle_nag.rs:902`) and `sessions::the_operator_added` telling an add from a re-push; the
pure `nag_notice` read plus `Harness::nag_said_as`, so the ladder counts the PLAN and not the
wording.

**Tests:** `crates/harnessd/tests/child_nag.rs::a_childs_check_names_the_operators_row_first`
(`:614`), and the board/nag tests under `crates/tools/src/builtins/todo/tests/`.

*Left:* a later re-arm (a HELLO re-push, a turn ending) after an add sets the full window and so
pushes the 10 s deadline back; "told" is the sticky choice in memory, so after a restart the first
check of an already-named operator row says "the operator added" once more; placement (the board's
snapshot order) was not changed, by choice — ranking in the check is the fix.

---

## R79 — Job and subagent results arrive in the middle of work and pull the model off it — **SETTLED 2026-10-30: a routine completion is held and batched; only what the plan waits on interrupts**

*The entry claimed:* every completion was delivered the moment it landed, so a model working on X
was handed "job j975 ended" as the next thing to do; `r79-batched-delivery` decided delivery by
relevance and batched the rest into one `[meanwhile]` note.

**Main has it.** `crates/harnessd/src/delivery.rs` — `decide` (`:200`) over `Facts`,
`HELD_RESULTS_CAP` = 10 min (`:88`), `DeliveryLog` (`:389`); `JobWatchers::hold`/`take_held`/
`turn_ended` and `say_held` (`jobwatch.rs:1021-1035`); `SessionEvent::HeldResults`
(`sessionlog/src/event.rs:2316`); `bash::wake_sentence` and `wake: now|later` on `bash` and
`task`; `<workspace>/.letibot/delivery-log.jsonl`; the head's `[meanwhile]` fold and its counter on
the Responding row.

**Tests:** `delivery::tests::*` (the table, the board facts, a monitor's record),
`harness::tests::delivery::*` (`harness/tests/delivery.rs`: held-then-one-batch in a child's long
turn, `when: job` immediate to a root mid-turn, a needed job's failure, the starvation cap, the
fixture), `crates/harnessd/tests/delivery.rs` (a root's wake answers `Ok(None)` for a routine
completion and the next prompt carries it before the operator's words), and
`bash::tests::wake_is_now_or_later_and_the_result_says_which`.

*Left:* "what the model did next" is not recorded (the metrics design's); the before/after
measurement needs a deployment; `task_start` still has no `wake` argument; `held_results_cap` has
no providers.toml/CLI knob; emergencies are not in the record and still reach only a turn boundary.

---

## R81 — Metrics and operator feedback: an event table, the instruments, and a way to mark a moment — **SETTLED 2026-10-30 (phase 1): the file, the instruments, the mark and `letibot stats` are in main**

*The entry claimed:* per-turn metrics were computed and dropped, deliveries/wakes/nags were not
recorded at all, and the operator had no way to mark a moment; phase 1 built a sibling `metrics.db`,
ten instruments, a feedback frame and a reader.

**Main has it.** `crates/metrics` (`lib.rs` — `Event`, `emit`, the non-blocking channel;
`store.rs` — the `event`/`daily`/`meta` schema, `path_beside`, the writer, retention and the
rate/size caps), `crates/harnessd/src/instruments.rs` (one line per site), the hub tap,
`ClientFrame::Feedback` (`sessionlog/src/protocol.rs:683`), `crates/letibot/src/stats.rs`
(`stats_text` `:102`) and `letibot fb` (`main.rs:297`).

**Tests:** `crates/harnessd/tests/metrics_instruments.rs` (each instrument's row on a canned-server
turn through the production turn tail), `crates/sessionlog/tests/feedback.rs` (a mark round-trips
over a real socket into a temp `metrics.db` with its turn and round while no worker drains the
hub), and the emitter/writer/retention/cap tests in `crates/metrics`.

*Left:* phase 2/3 as designed — rollup percentiles, retiring the jsonl files, `gate.step` pointers,
the LIVE `letibot stats serve` page; leticl does not send `Feedback` yet; and a few §5 items (the
`notes-offers.jsonl` dual-write, `/fb list|rm`, a gutter mark, `delivery.followup`, the `perf`
< 1 % measurement). The "done when" still wants a day of real use to check the counts.

---

## R80 — `/clear`: a fresh conversation for the same session — **SETTLED 2026-10-30: the fork, the orientation and the boundary row are in main**

*The entry claimed:* `/compact`'s summary and `/reseat` both carry the drift forward, and the
operator wanted a fresh conversation without losing todos, jobs or subagents.

**Main has it.** `crates/harnessd/src/harness/clear.rs` (the `Clear` verb `:65`, `Harness::orientation`
`:170`), `Harness::fork_onto` (`harness.rs:13548`, which `fork_to_summary` also delegates to), the
`cleared` warning as the boundary row, `ChildCommand::Clear` answered by the child's own thread.

**Tests:** `crates/harnessd/tests/clear_context.rs::a_cleared_session_keeps_its_board_its_job_and_its_child`
(`:220`) — the transcript is `#t1` with exactly one item, the board identical, the old transcript
whole, the count unmeasured — and `a_childs_clear_is_answered_by_its_own_thread` (`:463`).

*Left:* `/clear <sentence>` quotes the sentence and runs no turn (whether it should be submitted as
the operator's prompt is the operator's to rule); the authorisation trail is not reset, so an
utterance from before the clear can still authorise an act the model can no longer see; leticl has
no `cleared` row code and the head does not fold it like `reseated`; not live-tested on a metered
provider.

---

## R82 — A row that is only waiting on a job is nagged every window — **SETTLED 2026-10-30: `wait_on` writes the edge, and the check then leaves the row alone**

*The entry claimed:* a `needs` edge silenced the check but only on a row the model restated, never
the operator's, and nothing told the model it existed; `todo-wait-on` gave `todo_write` a `wait_on`.

**Main has it.** `todo_write`'s `wait_on` — `{"wait_on": [{"content": …, "job"|"child": …}]}`
and `"detach": true` (`crates/tools/src/builtins/todo.rs:285`, `:896`), `TodoBoard::set_waits`
(`:323`), `jobs::resolve_job_in` (`jobs.rs:159`) and `ChildTodos::own_child` for the refusals by
name, `RunningJobs` carrying the session's jobs and subagents so the advice line can offer a real
handle. R79 reads the same `needs`, so a completion a row waits on is the plan's.

**Tests:** the needs cases under `crates/tools/src/builtins/todo/tests/`, and
`crates/tools/tests/background.rs::{a_wait_on_a_job_the_harness_is_delivering_returns_at_once
(:802), a_wait_on_a_job_nobody_is_delivering_still_blocks_for_its_deadline (:868)}`.

*Left:* restating one of the model's own rows in `todos` without the wait takes it off (the
upsert's contract, and the reply says so); the parent's and the queue's halves cannot take a wait.

---

## R83 — The operator can pause a session's plan-checks, loudly — **SETTLED 2026-10-30: `/nag`, the column, the rows and the resident line are in main**

*The entry claimed:* the only lever was per row; `nag-switch` added `/nag pause|resume` per
session, persisted on the session's row, with the pause said loudly in four places.

**Main has it.** `crates/harnessd/src/nag_pause.rs`, `/nag` in the slash table (`slash.rs:78`,
`:347`), `session.nags_paused_until` (schema 25 — `tokencore/src/store/floor.rs:137` records the
arm as Additive, `store.rs:361` describes the column), `Config::nags_paused`, the `nags_paused` /
`nags_resumed` rows on the hub, the `nags` settings row and the resident warning line.

**Tests:** `crates/harnessd/tests/nag_pause.rs`, `child_nag.rs`'s paused child, the clock cases in
`sessions/idle_nag.rs`, and three in `crates/tui/src/app/tests/settings.rs`.

*Left:* a child paused WITH an end does not park until the end (by design, so the resume is said on
time); the jobs pane does not list paused sessions (the picker does); the Sessions-side hand-back
of `/nag` to a child's reader has no test of its own.

---

## R60 — `!term`'s three missing things — **SETTLED 2026-10-30: scrollback, several panes, a confined pane — and the two R74 items beside them**

*The entry claimed:* the pane could not scroll back, could not hold a second program and ran
outside the session's boundary; `r60-term` built all three plus R74's mouse mode and split report.

**Main has it.** `letibot_vt::Screen`'s history (`vt/src/screen.rs:157` `HISTORY_ROWS = 2000`,
`:445` `view(back)`, `:424` `scrolled`), `letibot_ui::ansi::pane_rows_back` (`ui/src/ansi.rs:455`)
and `TermPane::back` (`tui/src/app/term_pane.rs:294`); panes numbered on the wire
(`ServerFrame::TermAttached`, `TermStatus { panes }`); `Registry::bound_panes`
(`sessionlog/src/registry.rs:2511`), `HostProcesses::terminal_wrap` (`exec/host.rs:1184`) and
`Confinement::wrap_terminal` (`exec/confine.rs:1501`, bwrap without `--new-session` so `/dev/tty`
survives), the `RELAY` under `script` for a confined pane (`exec/term.rs:216`); `Screen::mouse()`
(`vt/src/screen.rs:413`) with `TermPane::mouse` (`term_pane.rs:368-400`).

**Tests:** `vt … screen::tests::a_line_scrolled_off_the_top_is_kept_and_can_be_read_back`
(`screen.rs:1759`) — **RUN, ok**; `tui … pane::{a_wheel_over_a_pane_reads_back_what_scrolled_off_it
(:1101), a_program_that_asked_for_the_mouse_is_sent_it_translated_to_the_pane (:1149),
a_mouse_report_split_across_two_reads_is_put_back_together (:1189)}`;
`harnessd --test term_pane_live::{a_second_pane_runs_beside_the_first_and_each_is_named_by_its_number
(:585), a_pane_runs_inside_the_boundary_its_session_installed (:660)}`.

*Left:* no live `!term mc` / `!term less big.log` check was run on a real head — the one check that
is the operator's; a confined pane has no network and no `ttyname` of the host's (it runs under
`script`); two heads opening panes in the same session at the same instant can adopt each other's
number; a pane this head is not drawing that dies files no row here; and whether a confined
operator pane should get `--unshare-net` is still the operator's.


---

# Records as filed, moved out of `TODO.md`

These are entries `TODO.md` carried after their work closed. Each is verbatim as it was filed;
where `TODO.md` cited a verdict by number, that verdict is a section above and this is the entry it
settled.

## R112 - The harness reads the box's home for instructions and skills, so a test's prompt depends on the host - **CLOSED on branch `remote/02482000`, filed 2026-11-02**

`compact::a_child_session_compacts_on_a_tool_result_overrun_and_finishes` fails on lab2x1 on main (`3566df43`) and passes on lubuntu1 and xmg. The child's prompt there is 8341 tokens of a 32765 window, so the compaction cannot make room and the task ends in `ContextWall`. lab2x1 has `~/.claude/CLAUDE.md` (8.5 kB) and a user skill in `~/.claude/skills`; lubuntu1 has neither. The harness asked `BoxDirs::for_this_box()` (`$HOME`) at three places in `crates/harnessd/src/harness.rs`, and `crates/tools/src/builtins/skill.rs` reads `$HOME/.config/letibot/skills` and `$HOME/.claude/skills`, so an in-process test could not hand them a clean box.

**Landed (2026-11-02, branch `remote/02482000`): the box is data on the harness `Config`.** `Config::box_dirs` carries `leticode_config::BoxDirs` — the config dir, and the home `~/.claude/CLAUDE.md` hangs off — and `Config::box_skills` the box's skill roots. `Config::read_the_box()` is the one reader of `$HOME` and `$XDG_CONFIG_HOME`, called at a daemon's startup (`for_the_daemon`) and by the `m1` measurement, which is a live session and not a fixture. `Config::global_notes()` and `Config::skill_dirs()` are the two derivations, the project's skill roots taken from each session's own workspace so a child in a worktree reads that tree's; the five readers take them off the config — `Parts::load`, the prompt composed at `Harness::open`, `reseat_candidate`'s, the notes offer's, and the `notes` tool's scope. `Config::for_this_box`, which every fixture builds on, names an EMPTY box, and `standing_notes::global_dir()` is gone with its last reader. The same class of defect in the fixture git helpers — a box whose global config sets `commit.gpgsign` signed, and so refused, every fixture commit — was closed in the same pass: `letibot_tools::testing::git` and the fixtures that now use it.
**Done when**: the compact test passes on this host with an 8.5 kB `~/.claude/CLAUDE.md` and a `~/.claude/skills` skill in place — measured red before the change and green after, the same binary and the same home — and no harnessd code below the daemon's startup reads `$HOME` for instructions, box notes or skills.
**Still open?** `grep -rn 'BoxDirs::for_this_box\|SkillDirs::for_this_box' crates/harnessd/src` answers only `read_the_box`, where the call wraps onto a second line; `Config::for_this_box` is the fixture constructor and reads no home.

---

## R111 - Revisit read-before-edit: an edit should only touch what the model was shown - **CLOSED on branch `remote/04269000`, filed 2026-11-02, operator's ask**

The operator, 2026-11-02: *"add that file not read back to todo. i think we might want to revisit it"*.

`edit` had two gates and lost both in `4a2341a3` (2026-09-17), on the operator's reading:
- read-before-write was redundant where it worked (an edit that lands has already matched `old_string` exactly once in the current bytes) and broken where it was not (over the paste ceiling it showed an excerpt, marked the file seen and let the retry through);
- the staleness refusal compared a digest of the whole file, so a formatter touching line 900 refused an edit at line 12.

What was left open then (board item 01M2RFDXW33CPJHNDX0KMYPN92) is the version worth building: track the lines the model was actually shown (`read` ranges, grep and search hits, its own earlier edits), and refuse an edit whose target region it never saw or that changed since it saw it. Open question from that time: how near to a shown line counts as seen.

**Decision on "how near": a line counts as seen only when a tool displayed that line.** Adjacency is not display, and every showing tool already carries its own context — a `grep` hit can ask for lines around it, an edit's result shows three lines either side, a `read` window is explicit — so the boundary is where the display stopped. A line `read` clipped at its character cap, and a hit line `grep` clipped at 400 characters, are not shown: the model saw a prefix.

**Landed (2026-11-02, branch `remote/04269000`):** the ledger is `crates/tools/src/files.rs`, now one fingerprint per shown line instead of a whole-file digest, and `edit`'s gate runs after the match (the match locates the region the showing must cover; a miss or an ambiguity never reaches it). `read` marks the lines it displayed in full; a capped or clipped read marks only those; `grep` marks the rendered rung's hit and context lines; a landed edit reseats the table across its change (lines below shift, the changed region plus three either side is licensed) and a `write` of the session's own marks the whole file. A refusal names the range to read (`call read with offset=A and limit=B`), shows what has been shown, and marks nothing — the old rung's paste-and-record, which cleared its own flag, is deliberately not back. `replace_all` checks every occurrence, and a batch checks each step on a checkout of the ledger that is thrown away unless the whole batch writes. `write`'s whole-file staleness check is unchanged and now keys on `whole_of`, which a search-only path does not satisfy.
**Still open?** nothing.

---

## R110 - The gate's result depended on the host: nine tests that read the machine instead of their fixture - **CLOSED on branch `hermetic-tests`, filed 2026-11-02**

Measured 2026-11-02 by running the same gate on four hosts: these tests passed or failed by host state, not by code.

1. **The models catalogue.** `crates/provider`'s `every_fallback_names_a_real_model_and_its_shortest_form`, `crates/provider/tests/fake_provider.rs`'s unpriced-turn assertion and `crates/bridge/tests/end_to_end.rs`'s cost assertion read `$XDG_CACHE_HOME`/`~/.cache` (`opencode/models.json`, `letibot/models.json`), and four hosts carried four versions of it. `OpenAiProvider` now takes the catalogue it prices by, the fallback check asks the vendored snapshot, and the four bridge worlds point `HOME`, `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` at a fixture whose `models.json` is `{}`. (The ninth, filed with these eight after the first measurement.)
2. **`crates/room/src/tree.rs`'s project-key test** hashed `/home/dead/Projects/letibot/letibot`; where that checkout is absent the canonicalisation fails and the trailing-slash spelling hashes a different string, so the test was red here and green on the operator's Mac. It creates the folders it hashes.
3. **`crates/harnessd/tests/plugins_tool.rs`** needed rano's SDK at `$HOME/Projects/rano/rano/sdks/rust`, so the whole-loop test skipped on any other box. The SDK is a git dependency of this workspace: its rev is in `Cargo.lock` and its source in `$CARGO_HOME/git/checkouts`, which is now the last place `scaffold::sdk()` looks.
4. **`crates/letibot/src/launcher.rs`'s socket-table cross-check** compared whatever daemon the box happened to have running, and skipped where there was none. It binds a listener and writes the record the launcher would write, in a runtime directory of its own, and reads the table after the bind.
5. **`crates/tools/tests/toolchain_grant.rs`** granted `$HOME/.cargo` and `$HOME/.rustup` and ran `$HOME/.cargo/bin/cargo`, which is a rustup shim in a home directory and nothing at all on a box whose cargo is mise's or a distro's. It reads `CARGO`, `CARGO_HOME` and `RUSTUP_HOME`.
6. **`crates/harnessd/src/mergequeue/gate.rs`'s scratch test** asserted that `/var/tmp` is not a tmpfs, which is a fact about the machine and false in every placement sandbox. The short-path and removal properties are asserted on the production path; the filesystem half is measured on a root the test chooses and verifies, and the box's own tmpfs is printed rather than asserted.
7. **`crates/tokencore/src/apparatus.rs`'s `present_gguf`** answered PRESENT for a file that existed and could not load (a split shard without its siblings, an architecture this llama.cpp does not know), so 47 harnessd tests failed instead of skipping. Presence is now the load, measured once per path and cached, and the probe lives in `letibot_llama::apparatus`, the only crate that can answer it. The tokencore probe is gone, so the weak question cannot be asked again.
8. **`crates/harnessd/tests/stop_ends_runs.rs`** had two defects, and the window was the smaller one. Its sentence window was a fixed 5 s, the same 5 s `Cgroup2::end` may spend waiting for a killed tree to leave its cgroup, so on a loaded host the sentence was published into a loop that had given up; the loop now reads until the daemon's own `Bye`. And its fixture, `su -c true` on Linux, blocks only where `su` has to ask for a password: on a box whose session is already root inside its own namespace it ran `true` and was gone milliseconds later, so the stop arrived at nothing and the daemon named nothing (MEASURED on a 24-core host with cgroups: hubs closed 11.4 ms after the stop, no sentence). The run is now `/bin/cat` blocked on the terminal the daemon holds, which cannot end on its own anywhere, and the precondition is the card `Prompts` raises for a run of the operator's own rather than a warning about an earlier moment. **And the daemon had a defect of its own, which the fixed fixture then proved on xmg**: with the card up and the run blocked, the stop still named nothing, because the run ender is the process host's table while the operator's own run is `Prompts`' (`crates/sessionlog/src/registry.rs`). `Registry::close` now asks the session's `PromptDriver` when the ender named nothing, and the whole close is one caller's: a second caller used to close the hubs while the first was still ending runs, which is how a sentence could go into a hub that had already said `Bye`. **And a third round on xmg: the sentence cannot be built from the ender at all.** An ender answers only after a kill that takes seconds, so the announcement now happens *before* the kills and before the ack, from the two tables that can name a run without ending it (`Registry::runs_ending`: the host's `RunNames` reporter and `PromptDriver::in_flight`); the `Stop` frame says it, and `close` says it for the signal path. The test prints every frame between the stop and the `Bye` when it goes red, so the next report names the order rather than only the absence.

**Landed (2026-11-02, branch `hermetic-tests`):** one commit per item, and each host dependence measured rather than argued: the catalogue tests fail with `HOME` at a catalogue that prices the model, the project-key test fails where the checkout is absent, the whole-loop test skipped and now runs, the launcher test skipped and now compares, the gate test was red on `/var/tmp`, and `LETIBOT_VOCAB_GGUF=<a text file>` turned 20 llama tests from passes into panics before and into skips after. `stop_ends_runs` cannot run in a sandbox without a confined execution backend (no cgroup v2 delegation), so both halves of its fix are read here and measured on the gate's host; the gate decides.
**Still open?** nothing.

---

## R108 - sessionlog's placement test rests on inode reuse - **CLOSED on branch `inode-test`, filed 2026-10-27**

`placement::tests::a_record_of_a_socket_that_is_gone_is_not_an_answer` assumes a re-bound unix socket gets a new inode; ext4 on lab2x1 reuses it. The record's identity needs something a rebind cannot repeat, or the fixture's premise made true without sleeping. See `.letibot/notes/sessionlogs-placement-test-rests-on-inode-reuse.md`.
**Landed (2026-11-01, branch `inode-test`): both halves, and the identity is the one in code.** The
fixture's premise had already stopped depending on the allocator on `main` (`bae4ae44`): the successor
is bound beside the old socket and renamed onto the name, so *two files at one moment cannot share an
inode* is the fixture's own guarantee. The record now also carries what a rebind cannot repeat: a
`stamp <ctime>.<nsec>` line beside `socket <dev>:<ino>`, and `read` answers nothing unless BOTH halves
match the socket at the path, the ctime being set by the kernel at the bind and out of a rebind's
reach. MEASURED on this ext4 placement (wt-b3): 30 of 30 close-unlink-rebind rounds handed the same
inode back, and the ctime differed in every one. A record from a build that wrote no stamp gets the
same refusal (it cannot prove which binding it names), which costs the same edge as the record's own
introduction: a bridge from a newer build refuses an older daemon until it is restarted. Tests,
`crates/sessionlog/src/placement.rs`: `a_rebind_that_takes_the_same_inode_back_is_not_an_answer` (a
chmod stages the state a reusing rebind leaves, the inode staying and the ctime moving, so no bet on
the allocator) and `a_record_from_a_build_without_the_stamp_is_not_an_answer`; the gone-socket test
is green 20 of 20 here, and `cargo test --offline -p letibot-sessionlog` is 188 lib and 61
integration green with the four live-model targets unrun.
**Still open?** nothing.

---

## R109 - Skills are proposed, chosen by the operator, and handed to subagents - **LANDED on main (95058763, a68157ec, aa2dba41); live check and the offer are R113**

The operator's design (2026-11-01), in four parts plus six refinements, and what each became. **Status: built on a branch, not landed** — this entry is the record of what the tree now does and of what it deliberately does not.

1. **Propose.** The skills *index* — name, one line and where it came from — is injected into the prompt in the same block and under the same rules as the standing notes, and re-read at the same two moments (session open, every base rebuild). A body is never in the prompt. Sources are shown and derived: `this project` (`.letibot/skills`, `.claude/skills`), `the operator's` (`~/.config/letibot/skills`), `installed` or `installed from …` (`~/.claude/skills`, where an installer writes: the origin a `.origin` file or the frontmatter names, else `installed`, and never the operator's words), `this build` (compiled in), `the fabric` (the shelf), `written by a model` (`<data dir>/skills`). The index is capped three ways — `INDEX_MAX_ROWS` (60), `INDEX_MAX_BYTES` (4000) and `INDEX_BUDGET_TOKENS` (800, the session's own counter) — in source-priority order, and its tail says how many more `skill list` holds.
2. **`/skills` and `/skills NAME rest`.** The pane lists what the box can hand over (name, source, one line); Enter, or `/skills NAME`, hands that skill's body to the session as the operator's message, labelled (`skill_message`), and the rest of the line goes on as their prompt in the same turn. The DAEMON injects — it loads the body (a fabric one is a read only it may make), writes the row and runs the turn. `ClientFrame::ListSkills`/`LoadSkill` and `ServerFrame::Skills`, `PROTOCOL_VERSION` 55.
3. **Subagents.** `task` and `task_start` take `skills: [names]`; each name is checked against the same corpus `skill list` answers from, refused by name (with the names there are) before anything spawns, and the bodies are preloaded into the child's prompt. A child that names none inherits its parent's list, and the list rides the child's record so a resumed child boots with it too.
4. **Declared seats (C).** `Config::skills` is a session's declared list; the reviewer gets `[queue] reviewer_skills` from its project file (`Pool::reviewer_skills` → `TaskSpec::skills`), the one seat nobody types a brief for. The other places it applies: a `task` child's spec, the review queue's reviewer, and a child's inheritance. **Not wired**: a per-role table, a monitor's or a plugin condition's wake (a wake runs a turn in a session that already has the index and its own bodies).
5. **The query door (A).** `skill {query}` (or `action: "find"`) ranks the index with `letibot_tools::similarity` — `Corpus::ranked`, the notes' own arithmetic and not a new matcher — answers the nearest body and names the next two; below `QUERY_FLOOR` (0.15) it says so and loads nothing.
6. **`allowed-tools` may only narrow (D).** Parsed from the frontmatter (names, lowercased, `(...)` qualifiers dropped); for a session STARTED with declared skills, the union of their `allowed-tools` restricts the tools seated (`Registry::only_tools`), and the narrowing is said on the open notes. A skill loaded mid-conversation narrows nothing (message zero is frozen), and a skill with no `allowed-tools` restricts nothing.

**Where the tests are**: `builtins::skill` (sources, installed origin, the index's labels and cap, the query's floor, `tool_ceiling`), `builtins::task::tests::a_task_that_names_skills_carries_them_and_refuses_an_unknown_one`, `runtime::tests::only_tools_narrows_to_the_declared_names`, `standing_notes::tests` (the index and no bodies, a declared body whole, the handed-over message's label), `tui::app::tests::skills` (`/skills` opens and asks, the rows carry their sources, `NAME rest` carries the rest, Enter on a row).

**Left open, on purpose**: nothing writes `<data dir>/skills` yet (the model-written source is a seam with no writer); a reopened child inherits `cfg.skills` through the spawn's clone only, so a child whose parent named none and which was named in its record is the one case where a resume re-reads it from the record and nothing else; and the fabric's rows in the index are read on every base rebuild (one node call), which is the same shape the fabric block already has at open.

**Still open?** `/skills` on a box with skills in `.letibot/skills/`, and `task {skills: [...]}` on one of them.

---

## R101 — Three placements started together on one host: two failed the bundle hand-off, though R99 landed — **CLOSED on branch `agent/the-placement-and-bridge-cluster`, filed 2026-10-28**

MEASURED 2026-10-27 ~20:05: R2, WJ and TF were started on lab2x1 within a minute, by daemons that
already had R99 (one ssh master per daemon, streamed verified writes). All three failed with
`daemon_start_failed: placing this session on lab2x1: … handing the bundle to the remote
repository`; R2 restarted alone placed fine. So R99's own **Done when** (six at once) is not met:
whatever the hand-off shares between daemons — the remote mirror repository, a lock, the
`incoming.bundle` name — still collides. Find it with a forcing test (N concurrent placements of
the same and of different worktrees), fix it, and only then close R99.
**Landed (2026-10-31): two shared names in `SshBackend::place`.**
(1) The mirror was guarded by `[ ! -d <mirror> ]` — a DIRECTORY is not a repository. MEASURED:
`git init --bare` three at once on one path exits 128 for two of the three, in 30 of 30 rounds, and
a loser has already made the directory — so every later placement's `[ ! -d ]` skipped the init and
the NEXT step failed on a directory that is not a repository, which is the reported sentence. The
step now loops until `<mirror>/objects` exists (bounded, ~5 s) and answers the directory, not
`git init`'s exit status. (2) The bundle went over under the MIRROR's name —
`{mirror}/incoming.bundle`, one name per mirror — so two placements of one repository on one host
wrote and fetched through the same file. Nothing there fails loudly: `git fetch -f
incoming.bundle +HEAD:refs/heads/letibot/<mine>` creates MY ref from WHATEVER bundle is there, so a
placement that lost the race was handed a checkout of somebody else's commit and said nothing. It
is now `incoming-<pid>-<counter>-<short>.bundle`, removed by the same call, and the fetch's own
words reach the failure instead of *it said nothing*. Tests, both measured RED pre-fix:
`a_mirror_that_is_only_a_directory_is_made_a_repository` and
`two_placements_at_once_never_read_each_others_bundle` (the forcing test: the fake ssh holds one
call, HEAD moves while it is held, a second placement runs, then the first is released) —
`crates/tools/tests/remote_hosts.rs`.
**What was NOT established, said plainly.** The three packages in the report were three
WORKTREES of one repository, and `SshBackend::place` takes `git rev-parse --show-toplevel` of the
source — MEASURED: two worktrees resolve to two different toplevels, so they name two different
mirrors (`{root}/wt-a.git`, `{root}/wt-b.git`). Neither shared name above is reachable from that
shape: both need two placements of the SAME repository (two sessions of one worktree, a commit that
moves between two placements, or a mirror left half-made). So the fixes are real and measured, and
they are not proved to be the 20:05 failure. The likeliest cause of THAT one remains R99's mux
saturation — `ccf7b895` landed 12:08 the same day and a daemon started before noon runs the old
code until it is restarted — and nobody could check which build those daemons were running, because
`letibot --daemons` was blind to every daemon the bridge started (R102, fixed above).
**Still open?** R99's own *Done when* — six at once on a real host — is still unrun.

---

## R102 — `letibot --daemons` and `--stop` cannot see a daemon the bridge started — **CLOSED on branch `agent/the-placement-and-bridge-cluster`, filed 2026-10-28**

MEASURED 2026-10-27 on the Mac: about forty `harnessd` processes were running (every one started by
a `letibot-mcp` bridge, the operator's sessions' and the orchestrator's), and `letibot --daemons`
answered *"no daemons recorded"*: a bridge starts `harnessd` without writing the launcher's record.
So the operator's own tools are blind to most of what runs, and the launcher's liveness test
(`every_live_daemon_agrees_with_the_boxs_socket_table`) has nothing to compare against and fails
under `LETIBOT_REQUIRE_APPARATUS=1` — the one red of the 2026-10-27 Mac gate. The bridge should
write the same record the launcher does (one writer for it, not a copy).
**Landed (2026-10-31): the bridge writes the record.** `daemon::ensure` writes
`<rundir>/<key>.json` beside the socket after the socket answers, field for field the shell's
(`workspace`, `socket`, `pid` as a NUMBER, `role`, `model`, `answers`, `dialect`, `bash`,
`started`), through a temp name and a rename. Written after the daemon is up, so a failed start
leaves no record claiming one; a record that cannot be written is said on stderr and is not fatal.
**Why not a listing over sockets**: the socket is `<sha256(workspace)[..12]>.sock`, so a socket
with no record names a hash and nothing else — forty lines of hex is not a listing, and `--stop N`
needs the pid and the folder the record holds. Test: `crates/bridge/tests/daemon_record.rs` drives
`daemon::ensure` against a fake `harnessd` that binds the socket, then runs the operator's REAL
`scripts/letibot --daemons` and asserts the folder is listed `up (pid N)` — red pre-fix.
**Still open?** `every_live_daemon_agrees_with_the_boxs_socket_table` under
`LETIBOT_REQUIRE_APPARATUS=1` has not been re-run on the Mac since.

---

## R103 — `letibot-mcp wait` on a session whose daemon never started does not return — **CLOSED on branch `agent/the-placement-and-bridge-cluster`, filed 2026-10-28**

MEASURED 2026-10-27: WJ's and TF's daemons failed at placement (R101) and their sessions never ran
a round, and `wait --until attention` on each sat for an hour instead of exiting 20 (*the session
failed*). `leticode_status` said `idle`, zero rounds, and `leticode_events` held one
`daemon_start_failed` warning — the fact `wait` should exit on.
**Landed (2026-10-31): two facts `wait` did not read.** `daemon_start_failed` is a `Class::Failure`
warning the daemon publishes before its hubs close (`cli::Boot::failed`), and the bridge's head is
already attached when it lands (`ensure` returns as soon as the SOCKET answers, before the session
opens) — so it arrives live, and `State` kept it as a ring entry and nothing else. `State::boot_failed`
now holds it and `wait` answers `Attention::Failed` with the daemon's own sentence (exit 20); it is
deliberately NOT subject to the *a failure that was already there does not count* rule, which is
about a previous TURN's death. And a connection the daemon closed (`Bye`) ends the wait too — that
is what an hour of sitting on a dead socket was. `leticode_status` says `failed` instead of `idle`
for such a session. Test: `crates/bridge/tests/end_to_end.rs` pins both shapes (a session that had
turned, and the review queue's, which never did), each with the warning published 300 ms INTO the
wait — a fresh attach is handed a snapshot and no backlog, so the live path is the real one.
**Still open?** nothing measured here; the fixture publishes the warning rather than failing a real
boot, because a real boot failure takes the daemon down with it.

---

## R64 — A runaway process takes its whole window, daemon and session with it — **LANDED on main 2026-10-11 (073c1ea); see "Landed" below**

The operator: *"why my pg-noop session died and took byobu window with it?"* MEASURED in the
journal: four OOM kills in 7 hours on lab2x1 (185 GB, 512 MB swap), each `Failed with result
'oom-kill'` on a `tmux-spawn-*.scope`:

| time | killed | anon RSS |
|---|---|---|
| 10-10 19:49 | `letidb-branch` | 116 GB |
| 10-11 00:47 | `harnessd` (see R66) | 113 GB |
| 10-11 01:29 | `letidb-fixed-94` | 125 GB |
| 10-11 02:17 | `letidb-guard` (pg-noop; a subagent started it in the background at 02:16:07) | 77 GB |

**The mechanism.** The launcher's `setsid` makes a new session, not a new cgroup: the daemon
and every process its tools start stay in the systemd scope of the tmux window that started
them. `DefaultOOMPolicy=stop`, so when the kernel kills any one process in a scope, systemd stops
the **whole scope** — the window's shell, the TUI, the daemon, every session it served.

**The fix is two halves, and either alone is not enough:**

1. **The daemon gets its own scope** (`systemd-run --user --scope`, from `scripts/letibot`), so
   a kill in a window does not reach it. Alone this only moves the blast radius: the tools'
   processes would still be in the daemon's scope, and a runaway one would stop the daemon.
2. **Each tool call gets its own scope with a memory cap** — R65.

**Done when:** a tool call that allocates without bound dies alone, with a result that says so,
and the window, the TUI and the daemon are still there.

**Landed (branch `memory-caps`), MEASURED on lab2x1:** `scripts/letibot` starts the daemon — and
the one-shot — with `systemd-run --user --scope -p Delegate=yes -p OOMPolicy=continue
--expand-environment=no` (`scope_run`), falling back to the old way with a line saying tool calls
run uncapped when there is no user manager. A TUI-started scratch daemon came up in
`app.slice/letibot-harnessd-<key>-<pid>.scope/daemon`; `tmux kill-window` on its window left it
running; `letibot --stop` then stopped it and systemd collected the scope.

---

## R84 — The landing tree lives outside `.git`, a session can read it, and a gate gets its gitignored inputs — **MERGED on main 2026-10-12 (4ac0f18, d27ed56); the placement rule after it is PARKED, see R84a**

The operator, 2026-10-11: *"also, the merges and branches shoudlnt land into .git. look at firecode
sission - it fights exactly this"*. The firecode session's finding, in its own words: *"The queue runs
the gate in a landing worktree, `.git/letibot-landing` (detached at 60bf819). A git worktree doesn't
carry gitignored files, so that tree has no `images/` and no `vendor/` — no guest rootfs, no kernel,
no firecracker. Every check that boots a VM fails, which is precisely the failed list."* The gate
peaked at 1.6 GiB of 40.9 GiB because no VM booted, and `main` was declared RED for a reason that is
not in the code. And: *"`.git` is on the never-write list — I can't inspect the landing worktree, and
no grant reaches it. So I'll reproduce it instead: a fresh clone"*.

- **Where**: `git::landing_worktree` makes it at `$XDG_DATA_HOME/letibot/landing/<repo basename>-<12
  hex of sha256(canonical repo path)>` (`~/.local/share/letibot/landing/…`), still a `git worktree`
  of the repository (landing is still a CAS on `refs/heads/<target>`; git's own `.git/worktrees/<name>`
  bookkeeping is the one thing in `.git` it implies), reused with its `target/`, reset clean every time.
  The root is `QueueEnv::landing_root` (tests: beside their temp repo).
- **Migration**: the first time the new tree is made, an old `<git-common-dir>/letibot-landing` is
  removed by `git worktree remove --force` + `prune` and the daemon log says so; a remove git refuses
  is said and left. Nothing else in `.git` is touched.
- **Read-only to the project's sessions**: `harness::confined_grants` adds the repository's landing
  dir to a confined session's view as a `Grant::ReadOnly` beside `--grant-ro`'s (creating it empty so
  the bind has a target).
- **`[queue] gate_carry = ["images", "vendor"]`** (`mergequeue::carry`): paths relative to the repo
  root, copied from the operator's checkout into the landing tree before every gate run (target health
  and branch landing) — `rsync -a --delete` when rsync exists, else a fresh recursive copy; never a
  symlink. Absolute, `..`, `.git` and the root are refused by name when the file is read; a symlink out
  of the repository, a missing path or a TRACKED path when the gate runs (the row/health is blocked).
- **The notice**: a failed gate with no `gate_carry`, whose checkout has top-level gitignored dirs the
  landing tree lacks (`git ls-files --others --ignored --exclude-standard --directory`, less `target/`
  and whatever the landing tree has), ends its evidence — and the `EMERGENCY —` notice — with *"The
  landing tree has no `images/`, `vendor/` (gitignored, present in your checkout); if the gate needs
  them, set `[queue] gate_carry` in the project's `leticode.toml`."*
- Tests: `mergequeue::git::tests` (outside `.git` and reused; migration), `mergequeue::carry::tests`
  (copy with both copiers, refusals, the hint and its silence), `mergequeue::settings::tests::
  gate_carry_is_paths_inside_the_repository_and_one_outside_is_refused_by_name`, `serve::tests::
  a_gate_that_needs_an_ignored_input_is_told_its_name_and_passes_once_it_is_carried`,
  `harness::tests::misc::a_confined_session_reads_its_repositorys_landing_tree_and_cannot_write_it`.
- **Left / for the operator**: firecode needs `gate_carry = ["images", "vendor"]` in its
  `leticode.toml` (not written here). A carried tree is a full copy per repository (firecode's
  `images/` may be GiBs on disk a second time). Old landing trees of repositories that no longer exist
  are not swept from the data dir.

---

## R85 — A daemon restart brings back subagents and monitors, and reminds the model of jobs — **SETTLED 2026-10-30: schema 26's records and the restart door are in main — see `TODO-settled.md` R85 for the tests and what is left. The record below is the entry as filed.**

The operator, 2026-10-12: *"and here is one more thing about restart - we should bring subagents
back - i cant afford to wait till they finish. we should record them (session persist anyway) and
restart automatically"*. Then, widening it: *"on restoring agents - in fact to make restarts as much
painless as possible - by default we restore agents, monitors, and remind the model about jobs.
Now, if some threshold passed - say more than 1 hours after the stop, we start to simply remind the
model stating that time passed and this is what you wre running and how to restart that."* And the
scope: *"this is the case for when harnessd stopped of course"*.

**What was wrong.** A subagent was a thread of the daemon that spawned it and a slot in that
daemon's memory; the task journal is a dashboard file rewritten from memory. Its session was in the
store, but nothing said it was mid-task. Restarting the daemon to pick up a build lost every running
child (and every monitor; a background job's handle only came back as "gone").

**What is recorded** (schema 26, `tokencore::store::subagents`, all additive):
`subagent_run` (one row per child: parent, subtask verbatim, the spawn's spec — role, downgrade,
placement, model, worktree path/branch/base, VM source/check — state, owning daemon instance, crash
count, outcome, delivered), `session_monitor` (each `monitor` call's own arguments, live flag),
`job_context` (a background job's slug, cwd, owning instance; the `job` row now gets its command at
start too), `daemon_life` (each daemon's start, 30 s heartbeat, orderly stop).

**What happens at a start** (`harnessd::restart`, `harnessd::subagent_record`): when a session is
opened, the records written by a daemon that is GONE are read (records stamped with the running
daemon's instance are left alone — an attach after a long idle restores nothing). Down time = that
daemon's stop (exact for an orderly stop; its last heartbeat, said as an estimate, after a crash) to
this daemon's start. Under `--restore-within` (default 3600 s): mid-task children are resumed in
their own session/worktree with `[restart] the daemon restarted while you were working …` and their
completion reaches the parent as before; live monitors are re-declared with their own call (one
that cannot be is reported); jobs are never re-run, the model is reminded with the exact `bash` call
and the rows that waited on them (their wait is released). Over it: nothing is restored; one
reminder lists everything with the one call that brings it back (`task_resume`, new, for a child).
Either way the notice is a turn, and the hub gets `restart_restored` / `restart_reminded`.

**Edge cases, each tested** (`crates/harnessd/tests/restart_restore.rs`): worktree gone (reported,
not respawned), finished-but-undelivered (delivered, not respawned), nested (resumed by its parent's
own open), crash loop (3 resumes without an orderly stop → abandoned, parent told), operator-killed
(never resumed), a child whose daemon died while it was opening (asked its task again).

**Still open:**
1. Only children started on a build with this change are recorded; a daemon of an older build left
   no rows, so its children are not brought back by the first restart onto this build.
2. A firecode (VM) child that worked on a COPY is not resumed: the copy does not survive, and it
   is reported instead. One that worked in its mounted worktree (a Mac) is resumed in a fresh VM
   on the same worktree (`subagent_record::vm_work_survives`).
3. A re-armed monitor's TTL starts again from the restart (it is the same call made again).
4. Monitor re-arm runs without the permission gate (the declaration was gated when the model made
   it); the operator may want that the other way.
5. The review queue's reviewers are not recorded (the queue re-serves its own rows).

---

## R77 — An operator's todo lands last and the nag never names it — **SETTLED 2026-10-30: the operator's row is the one the check names, and the first check says so — see `TODO-settled.md` R77. The record below is the entry as filed.**

The operator: *"i added a todo to pg-noop session, but it was placed at the end, and didnt arrive
the next nag"*. Read in the code:

1. **Placed at the end** — the head appends an operator row to the board; nothing ranks the
   operator's rows above the model's, so it is simply the last open row.
2. **Not in the nag** — the add DOES re-arm the clock (`CommandKind::SetOperatorTodos` →
   `Sessions::rearm_todo_nag`, `crates/harnessd/src/sessions.rs` ~3170), but a plan-check names ONE
   row: `todo::unfinished_plan_for` (`crates/tools/src/builtins/todo.rs` ~1090) picks the model's
   `chosen` row or the FIRST ready one, and everything else is "(N more open)". A row at the end
   is never named, so the model was told "(N more open)" and nothing pointed at what the
   operator added.

**Proposal:** the operator's open, ready rows rank first when the check picks what to name (the
model's `chosen` row only after them); the first check after an operator add says so —
*"[todo check] the operator added: "…""*; optionally an operator add re-arms with a short window
rather than the full idle one. Subagent boards (`serve_child`'s clock) the same way.

**Done when:** an operator row added at the end of a board with model rows ahead of it is the row
the next check names, with "the operator added"; a test on `unfinished_plan_for` and one on the
daemon's re-arm.

**Built (`r77-operator-todos`):**

1. **Ranking** — `todo::the_row_to_name` (`crates/tools/src/builtins/todo.rs`): the operator's open,
   READY rows first (the one already named among them, then `open_priority`'s order), then the
   model's sticky `chosen`, then the head of the ready set. A blocked operator row is not named; it
   is said on the waiting line as any blocked row is. `open_priority` is unchanged — it is the
   board's order for every other reader; only which row the check NAMES moved.
2. **"the operator added"** — the first check that names an operator's PENDING row it has not named
   before says `[todo check] the operator added: "<row>" — this turn is finished and it is not
   done (N more open):`, then the same bullet/advice/waiting lines (the head's notice fold still
   reads it). The next check names it with the ordinary repeat wording, still first.
   **And a defect this exposed:** both delivery doors read `Harness::nag_notice` once to record
   it and `nag_turn` read it again, and the read wrote `nag_choice` — so the model's FIRST check of
   any row already said *"you were asked about this one already"*. The read is pure now and
   `nag_turn` commits the choice. The doors record `Harness::nag_said_as` (the plan read the way
   `nag_should_arm` reads it) instead of the wording, so the ladder counts the same PLAN and not
   the same words — the wording changes between the first check and its repeat, and recording it
   made every check after the second come due at the base window.
3. **The window** — `SetOperatorTodos` tells an ADD (a new open row on the operator's half, by its
   words: `sessions::the_operator_added`) from a head re-pushing its half; an add goes through
   `TodoNagClock::operator_added`: earned back (`prompt_arrived`) and due at
   `OPERATOR_ADD_NAG_AFTER` (10s, the reason on the constant) rather than `TODO_NAG_AFTER`.
4. **Subagent boards** — the same `unfinished_plan_for`, reached through the same `nag_notice`, on
   the child's own clock (`tests/child_nag.rs`, `a_childs_check_names_the_operators_row_first`).
   The child's door refuses a head's todo edits (`child_command` → `Leave`), so a child never
   takes the operator-add window; it has nothing to take it from.
5. **Placement — NOT done, by choice.** The board's snapshot order (model, parent, operator, queue)
   is what `open_priority` and the pane are written against; ranking in the check is the fix.

**Left:** a later re-arm (a HELLO re-push, a turn ending) after an add arms at the full window and
so pushes the 10s deadline back — `rearm` sets the deadline rather than keeping the earlier one.
And *told* is read from the sticky choice, which is in memory: after a daemon restart the first
check of an operator row that was already named says "the operator added" once more.

---

## R78 — Up no longer recalls prompt history — **CLOSED: already fixed in `08bbf9a7` (2026-10-07), four days before this entry was filed**

The operator: *"also up arrow is broken - it doesnt scroll prompt history"*. R69 moved every key
through the surface stack and per-surface binding tables the same day. The recall path is in
`crates/tui/src/app/keys/mod.rs`: `refresh_prompt_history` before a recall (~75–79), the queued-
prompt take-back on Up with an empty composer (~205), Up scrolling the transcript when the reader
is parked (`!self.following()`, ~272), and the editor's own `vertical(true, …)` (~386). Find
which arm now takes Up first in the conversation surface (likely the parked-scroll arm, or the
`Conversation` binding table's Up) and pin it with a test: an empty composer, prompts sent
earlier, Up → the previous prompt in the composer.

**CLOSED, and it was already closed when this was written: `08bbf9a7`, 2026-10-07** (*"Up recalls the
session's prompts, not only what this head typed"*). The arm this entry suspected is not taken by the
surface stack — `focused_surface()` answers `None` for `Drawn::Conversation`, so no binding table sees
Up before the composer does. The recall is `editor.vertical(true, cols)` at the bottom of `key()`, and
the history it walks is rebuilt from the session's own `User` rows on the press
(`composer.rs::refresh_prompt_history`, called at the top of `keys/mod.rs::key_dispatch`);
`Editor::set_history` is ignored mid-walk, so the list cannot be replaced under the walk's index.
Tests: `up_recalls_the_sessions_earlier_prompts_after_a_restart` and
`crates/harnessd/tests/prompt_history_resume.rs` (the real daemon twice over one store) — both green
2026-10-31, as is a scratch test of the plain case (two prompts sent from this head, both rows landed,
Up twice → newest first).

---

## R79 — Job and subagent results arrive in the middle of work and pull the model off it — **SETTLED 2026-10-30: a routine completion is held and batched, and only what the plan waits on interrupts — see `TODO-settled.md` R79. The record below is the entry as filed.**

The operator: *"can job and agent results come mid turn? i think you guys are getting distracted
when this happens"*. Read in the code:

- A **child** (subagent) session gets job and child completions MID-TURN, at round boundaries:
  `JobWatchers::take_mid_turn_completions` (`crates/harnessd/src/jobwatch.rs` ~495), drained at
  `harness.rs` ~1269 into the turn's steering.
- A **root** session gets each completion BETWEEN turns as a WAKE — a turn of its own
  (`Sessions::wake` → `Harness::wake` → the `[job]` notice) — so a model that was working on X
  ends its turn and is immediately handed "job j975 ended" as the next thing to do.
- Monitor firings reach a running turn through steering for every session.

Nothing asks whether the model is waiting on the result. **Proposal:** deliver by RELEVANCE — a
completion the plan depends on (a todo row with `when: job`, a `needs` on that job or child, or a
`job_wait` it is blocked in) is delivered at once as today; any other is HELD and folded into the
next natural break as ONE compact "meanwhile:" line per batch (end of the current turn's last
round, or the next turn the model takes anyway), and never starts a wake turn on its own.
Batch several completions into one note. Keep the head's compact job rows (39ee4c8) unchanged.
Measure the effect on a session like pg-noop (count wake turns caused by routine completions in
a day, before/after).

**The operator's decision, 2026-10-11** — they proposed *"nothing should come mid turn, lets try
to queue it and then send as a batch"*, heard why an absolute rule breaks (turns here run for
hours; a subagent's task is often one turn; a broken dependency should stop the work; monitors
exist to react; the operator's own messages must stay immediate), and agreed (*"yeah lets do it"*)
to this design:

1. **Queue and batch by default.** A routine job/subagent completion is HELD and delivered as ONE
   compact "meanwhile:" note at the next turn boundary — which also replaces a root session's
   one-wake-turn-per-completion.
2. **Immediate only for:** the operator's own messages (unchanged, they steer); a result the plan
   depends on — a todo row with `when: job`, a `needs` on that job or child, a `job_wait` / `task`
   collect in progress; the FAILURE of something the plan depends on; monitor firings; emergencies
   (R76's red target).
3. **Starvation cap:** a turn running longer than N minutes (10, named, configurable) with results
   held gets the batch at its next round boundary anyway, as one note.
4. **Optional** `wake: "now" | "later"` on `bash(background)` and `task`, default `later`, for what
   the plan's structure does not capture; the tool result says which applies.
5. **Measured** (see the metrics item): every delivery recorded with when it arrived, when it was
   delivered, by which rule, and what the model did next.

**Status, branch `r79-batched-delivery` (2026-10-11):**

- **The decision is one pure function** — `crate::delivery::decide` over `Facts` (kind, failed,
  emergency, an open row's `when: job`, an open row's `needs` job/child, and what the session
  said it wants: `WantedNow::Awaited` — a `job_wait`/`task_result` on a watched handle, or a
  foreground call promoted to the background — or `WantedNow::WakeNow`). Order: emergency,
  monitor, failure of a dependency (`when`/`needs`/awaited), `when_job`, `needed`, `awaited`,
  `wake_now`, else **held**. A failure nothing depends on is held. `facts_of` matches a row's
  handle by job id or slug, a child by its handle, open rows (pending/in progress) only.
- **Held results live on the session's `JobWatchers`** (`hold`/`take_held`), are re-decided at
  every boundary (`delivery::sort` — a row added or a `job_wait` since promotes a held one), and
  are handed up with the queue when a child stops.
- **(Superseded by the `idle-wake` change below for the between-turns half.)** **Delivered as ONE `[meanwhile]` note at the next turn the model takes anyway**: before the
  operator's prompt (a harness row, so the operator's words stay last), with a parent's message,
  inside a plan-check nag, a continuation after the wall, or a wake that has something immediate.
  `Harness::wake` with only held results answers `Ok(None)` — **no wake turn, root or child**.
  **If nothing ever starts a turn**, they wait: the plan-check carries them for a session with a
  plan; a session with no plan and no prompt keeps them held (the head's jobs pane already shows
  every settlement — `JobSettled`/the subagent row are published as before).
- **Immediate** goes in at the next round boundary of a running turn — **for a root too now**
  (before, a root heard nothing mid-turn and everything as a wake turn after) — or as a wake turn
  between turns. Monitors unchanged (both doors), now recorded. The operator's messages are the
  hub's commands, drained first, untouched. Emergencies are still `queue_emergency_notices` in the
  wake (unchanged, and they start a turn).
- **Starvation cap**: `Config::held_results_cap`, default `delivery::HELD_RESULTS_CAP` (10 min),
  measured from the longest-held result's ARRIVAL (a turn's age would degrade to per-completion
  delivery ten minutes into any long turn); past it the batch comes in at the next round boundary.
- **`wake: "now" | "later"`** on `bash` (background) and `task`, default `later`, refused by name
  if neither; the result ends with which rule applies (`bash::wake_sentence`). `job_wait` /
  `task_result` on a watched handle now also mark it wanted (`ToolRuntime::with_completion_wanted`,
  `InvokeCtx::want_completion`), and their "nothing to wait for" sentence says it comes the
  moment it ends.
- **The record**: `<workspace>/.letibot/delivery-log.jsonl` (`delivery::DeliveryLog`, the
  notes-offers shape; no store table, so no schema bump to collide with the metrics work): one line
  per delivered job/child/monitor — `session, handle, kind, arrived_at_ms, delivered_at_ms,
  waited_ms, rule (immediate:<why> | batch | starvation), batch`.
- **The head**: `[meanwhile] ` is a notice tag; the heading folds to `Meanwhile: N results arrived
  while you were busy`, and the `[job]` groups under it keep the compact two-line rows (fixture
  `crates/tui/src/app/tests/fixtures/job_notice_meanwhile.txt`, pinned by harnessd's
  `the_meanwhile_notice_is_the_heads_fixture`, bless with `LETIBOT_BLESS_JOB_NOTICE=1`). No
  protocol change (44).
- Tests (each seen failing against a mutation that restores the old behaviour — everything
  immediate, everything held, no cap, no record, no head fold): `delivery::tests::*` (the table,
  the board facts, a monitor's record), `harness::tests::delivery::*` (held-then-one-batch in a
  child's long turn, `when: job` immediate to a root mid-turn, a needed job's failure, `wake: now`
  and a later wait, the starvation cap with a 300 ms window, the fixture), `tests/delivery.rs`
  (canned server: a root's wake answers `Ok(None)` for a routine completion and the next prompt
  carries it before the operator's words; a `needs` completion still wakes a root and the held
  batch rides along), `bash::tests::wake_is_now_or_later_and_the_result_says_which`, and
  `a_watched_subagent_is_not_waited_on` now asserts the want.

**Changed 2026-10-11, and in main (`idle-wake` is gone) — nothing is held once a turn ends.** pg-noop:
*"[meanwhile] 1 result arrived while you were busy — …"* reached the model the moment the operator
sent a prompt, long after it had gone idle, because nothing started a turn for held results between
turns and the cap acts only inside a running turn. A 60-second "wake after quiet" was proposed and
replaced before it landed by the operator's ruling: *"jobs were ran for a reason and held for a
reason. once turn ends no reason to hold"*. So: holding is ONLY for a running turn (mid-turn and the
starvation cap unchanged); a turn that ends with results held rings its own session's wake
(`JobWatchers::turn_ended`) and `Harness::wake` delivers them as ONE `[meanwhile]` turn — heading
*"N results arrived while you were working — nothing in your plan was waiting on them, so they
were held until your turn ended instead of interrupting you."*, rule `turn_end`; a result that
arrives while idle is never held — delivered at once as an ordinary `[job]`/`[task]` notice, no
heading, rule `idle`; a prompt (commands drain before wakes) that gets there first still carries the
batch under the old *"while you were busy"* heading (`batch`). The split is one fact: arrived before
the session's last turn ended (`JobWatchers::last_turn_ended`, `Deliveries::between_turns`). No
config knob. `/nag pause` does not delay either delivery (it pauses the daemon asking, not the world
reporting). Applies to children too: a child whose turn ended with results held takes a wake turn
for them instead of keeping them until its next turn or handing them up when parked. The head folds
the new heading as before (fixture `job_notice_meanwhile_turn_end.txt`).

**And a held counter on the head (same branch).** The operator: *"what would be useful is a counter
of how much and what kind is held - somewhere close to Responding ...."*. The daemon publishes
`SessionEvent::HeldResults { jobs, subagents, failed }` whenever the held set's counts change
(`JobWatchers::say_held`, from `hold`/`take_held`; the re-sort's take does not publish, so a
boundary that changes nothing says nothing). Ephemeral, folded into no snapshot: a head that
attaches mid-turn draws nothing until the set next changes. The TUI draws it faint on the
Responding row — `held: 2 jobs · 1 subagent`, `held: 1 job (failed)`, `held: 3 jobs (1 failed)` —
and nothing when all are zero. **PROTOCOL_VERSION 45 → 46** (a new variant): heads and daemons must
be restarted together. No web head draws a Responding line (`httphead.rs` is an OpenAI-shaped
endpoint, not a UI), so nothing was added there.

**Left:**

- **"What the model did next"** (point 5) is not recorded — the record has arrival, delivery,
  rule and batch size; joining it to the next tool call is the metrics design's.
- **The before/after measurement** on a session like pg-noop (wake turns caused by routine
  completions per day) — needs this deployed; the record is what it reads.
- **A parent that ends its turn "waiting" for a child without saying so** (no `needs` row, no
  `task_result`, `wake: later`) is not told until its next turn — that is the decision as given;
  watch the record for `batch` rows of `kind: child` with long `waited_ms`.
- `task_start` has no `wake` argument (only `task` does); `held_results_cap` is a `Config` field
  with no providers.toml/CLI knob yet.
- Emergencies are not in the record (they are not completions) and still reach only a turn
  boundary, as R76 built them.

---

## R81 — Metrics and operator feedback: an event table, the instruments, and a way to mark a moment — **SETTLED 2026-10-30 (phase 1): the file, the instruments, the mark and `letibot stats` are in main — see `TODO-settled.md` R81. The record below is the entry as filed.**

The operator: *"we need metrics design, there is simply not enough data to make decisions like
this. i can't even live feedback so we can analyze sessions, and we need metrics - how often and
what delivered mid turn etc etc etc, with times and usual. and not only for this topic. MORE
METRICS"*.

**Why:** R61–R79 were each decided from a hand query, a log grep or one report. Today per-turn
tokens/latency/cost are computed (`TurnMetrics`) and dropped; tool durations exist nowhere;
deliveries, wakes, nags and worker queue waits are not recorded at all; `~/logs/harnessd.log` has
no timestamps; the hub's typed `SessionEvent` stream is lost on restart; and the operator has no
way to mark a moment of a session. Only `adjudication` and `queue_transition` are good records.
The inventory, with exact locations and what each source can and cannot answer, is
`docs/metrics.md` §1.

**Design (`docs/metrics.md` §2):** an append-only `event` table in a SIBLING file
`~/.local/share/letibot/metrics.db` (not the store: lock contention across 11 daemons, no schema
bump for mixed builds, cheap retention, no content); one row = ts, daemon, build, session, root,
transcript, turn, round, kind, name, outcome, dur_ms, three integer slots and a small JSON `attrs`;
~50–55k rows/day at 10-10's rate (~10–12 MB/day), raw kept 90 days, rollups and marked turns
forever. Emission is a non-blocking `try_send` to one writer thread per daemon that batches one
transaction per second — **no SQL on the worker** (the c12d044 lesson).

**Phase 1 scope:**

1. `crates/metrics`: `Event` enum, `emit`, the writer thread, `metrics.db` schema 1, drop counter,
   daily prune, `[metrics] enabled / keep_days / snippets`.
2. The hub tap: one line in `Hub::publish` (`crates/sessionlog/src/hub.rs:869`).
3. Ten instruments (`docs/metrics.md` §6, locations in §4): worker command queue wait + slow
   idle passes (`QueuedCommand` gains `enqueued_ms`; `daemon.rs:275`); turn start/end
   (`turn_tail::run_turn`); round with time-to-first-byte (`harness.rs:10057`,
   `engine.rs` first frame); tool call duration/outcome/bytes/memory peak (`harness.rs:10153`);
   deliveries with arrived/delivered/rule/mid-turn (`jobwatch.rs:694`, `harness.rs:1269`,
   `:7800`, and R79's hold code); wakes by cause (`Sessions::wake`); compaction + wall in both
   units; nags with the named row and whether the next turn touched it; context claims (R71,
   dual-written with the jsonl and carrying the real window); warnings, operator commands and
   subagent transitions through the tap. `mem.sample` from memwatch if it fits.
4. Feedback: `/fb`, `/fb+`, `/fb-` and two keys in the tui (and `f` on a selected row), a
   `ClientFrame::Feedback` (protocol 45) handled on the head-conn thread, not the worker; a
   routine `feedback` warning so both heads draw the mark; `letibot fb` from a shell.
5. `letibot stats` (read-only): today, turns, deliveries, waits, feedback, `session S`, `sql`.

**Done when:**

- A day of normal use produces `metrics.db` with every phase-1 kind present; `letibot stats`
  prints today's turns, p95 worker wait per build, deliveries by rule, and marks; tool-call counts
  agree with that day's `adjudication` rows within 1 %, turn counts with the turn-end log lines.
- A test runs a turn with the writer blocked and the channel full: the turn completes, no stall,
  `metrics.dropped` > 0. A `perf` sample over a busy hour puts < 1 % of the worker's samples in
  `metrics::`.
- `/fb- …` typed while the worker is pinned for 30 s is written within 1 s with the head's
  press time and the running turn's id and round.
- R79's question is one query: `docs/metrics.md` Q1 and Q2 return rows for the old and new
  delivery code.

**Open for the operator** (`docs/metrics.md` §7): sibling db vs store table; retention; the
feedback frame vs riding `Slash`; the feedback keys; snippets default; on by default; the page as
export + artifact or `letibot stats serve`.

---


**The operator's decisions, 2026-10-11:** (1) a separate `~/.local/share/letibot/metrics.db`, not a
table in the store; (2) raw rows kept 90 days, summaries and marked turns forever; (3) feedback on
its OWN protocol message (protocol 45), handled on the connection thread so a mark is not delayed
behind a busy worker; (4) alt-g / alt-b mark the current turn good / bad; (5) context-claim
sentences are kept as text by default while R71's floor is tuned; (6) not answered — built ON by
default with a config switch to turn it off; (7) a LIVE page served from the box (`letibot stats
serve`), not an export.

**Phase 1, built on `metrics-phase1` (2026-10-11).**

*The file.* `crates/metrics` (`letibot-metrics`): `Event` (typed constructors over the generic
row of §2.2; the kinds are `metrics::kind`), `emit` — a `try_send` on a bounded 8,192-slot
channel, a drop counter on a full one, never SQL, never a wait — and behind the `store` feature
the `metrics-writer` thread (one transaction a second or per 1,000 rows, WAL,
`synchronous=NORMAL`, a failed batch retried once then dropped and counted), schema 1 exactly as
§2.2 (with `daily`), the per-kind `TEMP` views the reader loads, and the prune: once at start and
daily, raw rows older than `keep_days` (90) rolled up into `daily` (n, sums, max; p50/p95 left
`NULL`) and deleted — except every `operator.feedback` row and every row of a turn a mark points
at. `metrics.db` lives beside the store (`~/.local/share/letibot/metrics.db`; `$LETIBOT_METRICS_DB`
overrides; 0600). `providers.toml` `[metrics]`: `enabled` (on by default), `keep_days`, `snippets`
(default `context.claim` — decision 5), and — `metrics-cap`, 2026-10-27, `docs/metrics.md` §2.9 —
`rate_per_sec` (20 identical events per key per second, the rest coalesced into one row carrying
`attrs.suppressed`, plus one `metrics.suppressed` report per key per minute and one log line) and
`max_mb` (2048: over it the oldest days are rolled up and deleted down to 80 % in bounded passes).
The caps exist because of a measured 89 GB `metrics.db` — 270M rows at 1,400 a second from one old
build's `interrupt` loop. A daemon with no `--store` records nothing.

*A turn, for metrics,* is a prompt — every round of one door — keyed by its FIRST round's engine
id. The hub tap derives the same key from `TurnStarted.began_ms`, so a mark and its turn join on
`turn_id`. `turn.start` is written with the turn's end (stamped when it began): the id only
exists after the first round. A crash mid-turn leaves no turn rows (the rounds are there).

*The instruments* (`crates/harnessd/src/instruments.rs`; each site one line):
1. `worker.cmd` — `QueuedCommand.enqueued_ms` (new), queue wait and dispatch time per command,
   in `Daemon::run`; `worker.pass` for the sweep, plan checks, parking and reviews over 250 ms.
2. `turn.start` / `turn.end` — `turn_tail::run_turn`: door, wake cause, fill and window, model,
   tokens, rounds, tool calls and errors, TTFB, cost, outcome (`eos`/`length`/`wall`/`stalled`/…).
3. `round` — `run_rounds`: tokens, `server_prompt_tokens` beside `ledger_tokens` (R72's two units),
   `ttfb_ms` (new `TurnMetrics.first_byte_ms`: local from the post to the first id, cloud from
   the request to the first delta), prompt/predicted ms, slot, cost.
4. `tool.call` — around `runtime.invoke`: duration, outcome (the wire's tag), inline/full bytes,
   memory peak and cap (bash leaves `MemoryUse` on its thread: `exec::scope::note_memory`).
5. `delivery` — in `DeliveryLog::record`, R79's one record site: what, how
   (`immediate`/`batched`/`starved`), rule, wait, batch, mid-turn (the session's turn has run a
   round) or at the turn's start. The jsonl is kept beside it until phase 2.
6. `wake` — `Harness::wake`: counts per cause (queue, monitor, result, due row), ran or empty.
7. `compaction` (automatic and `/compact`, both units, duration) and `wall` (a turn that ends
   at the wall: both units, continues used).
8. `nag` (the named row, its status, operator's or not) and `nag.followup` (`touched` /
   `untouched`: did the turn the check opened move that row).
9. `context.claim` beside the jsonl, with the sentence as its snippet — and **the `999999` fixed**:
   `context_fill` reported `shown_tokens(planning_window())`, a round trip through the ratio; it
   now reports the window as given (`shown_wall`). The jsonl gets the real window too.
10. The hub tap (`sessionlog::metrics_tap`, one line in `Inner::append_and_fan`): `warning` by
    code and class, `operator.action` (commands, attach/detach, mid-turn or not), `subagent`,
    `job`, `decision.answer`, `queue.move`. Plus `mem.sample` from memwatch every fifth tick and
    at every census step. The registry tells the writer each session's parent, so every row has
    its tree's root.

*Feedback.* `ClientFrame::Feedback {at_ms, polarity, note, item_id}`, **PROTOCOL_VERSION 45**,
answered on the connection thread (`metrics_tap::mark`): placed on the turn and round the hub says
is running (or the last), or on the turn a read-back row was written in; the head's press time
kept (a clock more than a minute off is replaced by the receipt time); `#tags` parsed; a routine
`feedback` warning confirms it to every head (new code; census 115). The tui: `/fb NOTE`, `/fb+`,
`/fb-`, alt-g / alt-b (a second press within 5 s fills the composer with `/fb± `). **alt-b was
readline's word-left** and stays it wherever there is a line — it marks only on an empty composer
with nothing in front (the `ctrl-e` rule); alt-g always marks. **No `f` on a selected row**: this
head has no row cursor; a mark made while the view is scrolled back names the anchored row
instead. `letibot fb --session S [--turn T] [+|-] NOTE` writes the same row from a shell (on the
session's last finished turn), no daemon needed.

*`letibot stats`* (read-only): today (the default), `waits` (Q3, per build), `deliveries`
(Q1 + wakes by cause + turns by door), `tools` (Q8), `models` (Q7: TTFB p50/p95, tok/s, cache
hit), `turns`, `feedback [--bad]` (Q15), `session ID`, `sql`. `--since 2d|12h|all`, `--db`.

*Tests:* the emitter never blocks (1,000 sends into a full channel return at once, 996 counted);
the writer batches and accounts for every event (written or counted dropped); retention prunes
and keeps marked turns and the rollup; each instrument's row on a canned-server turn through the
production turn tail (`harnessd/tests/metrics_instruments.rs`) and in unit tests (worker wait,
delivery mid-turn/at start, wake cause, nag follow-up, wall, an empty door writes nothing); the
mark round-trips over a real socket into a temp `metrics.db` with its turn and round while no
worker drains the hub (`sessionlog/tests/feedback.rs`); every `stats` verb against a temp db.

**Left after phase 1:**
- **leticl** needs: send `ClientFrame::Feedback` (`{"frame":"feedback","at_ms":…,"polarity":
  "good"|"bad"|"note","note":"…","item_id":null|"…"}`) from its own `/fb` and keys, accept
  protocol 45, and draw the routine `feedback` warning. Until then `letibot fb` is its door.
- Not built from §5's phase 1: the `notes-offers.jsonl` dual-write; `/fb list` / `/fb rm`
  (withdrawal as an `outcome = 'withdrawn'` row — the reader already skips those); a gutter mark
  at the marked row (the confirmation is the warning line); queue depth on `worker.cmd` (n2);
  `delivery.followup` (did the next round touch the delivered handle); the "writer blocked, turn
  completes" test at the harness level (the emitter's own test covers the property) and the
  `perf` < 1 % measurement, which needs a busy hour on the operator's box.
- Phase 2/3 as designed: rollup percentiles, the jsonl files retired, `adjudication` /
  `gate.step` pointers, `error` by kind, the degradation block, and the LIVE page the operator
  chose (`letibot stats serve`, decision 7) — the read side is `letibot_metrics::stats`, so the
  page is one more caller of the same queries.
- "Done when" still needs a day of real use: every phase-1 kind present, counts checked against
  the turn-end log lines and the day's `adjudication` rows.

---

## R80 — `/clear`: a fresh conversation for the same session — **SETTLED 2026-10-30: the fork, the orientation and the boundary row are in main — see `TODO-settled.md` R80. The record below is the entry as filed.**

The operator, verbatim: *"I need to be able to clear context - fresh reseat without loosing session todos and jobs"*, *"and subagents"*, *"this will allow me to fix model degradation"*. MEASURED the same day on pg-noop: 710k of a 1M window, reasoning full of `✓ ✗ ✓ ✓` glyph spam and repeated boilerplate. `/compact` does not cure that — its summary is written by the drifted model in its own voice and becomes the base every later turn imitates — and `/reseat` carries the drift across verbatim.

**What landed.** `/clear [what to do next]`, a daemon slash verb (no new frame, no protocol bump — it rides `ClientFrame::Slash`, and the head completes it from `DAEMON_VERBS_KEY`):

- A new `<session>#tN` transcript for the SAME session, forked through the one fork body (`Harness::fork_onto`, which `fork_to_summary` now delegates to) onto message zero as `reseat_target` composes it now (prompt, tools, seat, standing notes re-read). No history and no summary.
- Its only item is an orientation built from STATE, spoken as `Speaker::Agent` (`harness/clear.rs`, `Harness::orientation`): one line saying the operator cleared it and naming the old transcript id (read it with `transcript`), the operator's sentence if one was given, the todo board in `todo_write`'s own words (`todo::board_text`), backgrounded/running jobs (handle, state, age, command), live subagents of this session (handle, state, the brief from the registry title), and `queue status` for the workspace's repository.
- Everything else carries on because it was never the transcript's: board, process host and job watchers (keyed by session), the subagent runner and the children's registry parent, workspace/seat/mode/notes, queue rows. The stored context count is cleared and the R72 `WallAnchor` re-bases on the new transcript, so the first round measures.
- A `cleared` warning (Routine; census 112/37) is the boundary row; the head resolves its fork mark on it like `reseated`.
- A subagent's own `/clear` is answered by its serving thread (`ChildCommand::Clear`); the worker hands a `/clear` for an own-thread session back to that thread rather than opening a second harness.

Tests: `crates/harnessd/tests/clear_context.rs` — todos (both authors), a background job and a mid-turn child; after the clear the transcript is `#t1` with exactly one item, the board is identical, the old transcript loads whole, the count is unmeasured and equals the new base; then the job and the child settle and both completions are delivered by the next wake into `#t1`. And a child's `/clear` forks the child (`#t1`, one item) with no `child_door_refused`.

**Left.**
1. `/clear <sentence>` quotes the sentence in the orientation and runs no turn; the operator's next prompt starts the work. Whether the sentence should itself be submitted as the operator's prompt is for the operator to rule.
2. The authorisation trail is not reset: an operator utterance from before the clear can still authorise an act the model can no longer see. Probably it should be; not decided here.
3. leticl has no `cleared` row code yet (its `+compaction-row-codes+`), and the head does not fold the `cleared` note the way `COMPACTION_ROWS` folds `reseated`.
4. Not live-tested on a metered provider: the first round after a clear should replace the unmeasured anchor with the provider's count, as it does after a compaction.

---

## R82 — A row that is only waiting on a job is nagged every window, and nothing lets the model say so — **SETTLED 2026-10-30: `wait_on` writes the edge, and the check then leaves the row alone — see `TODO-settled.md` R82. The record below is the entry as filed.**

The operator: *"poor pg-noop gets hammered by this 'session · Todo CRITERION, NOT A TASK …' it
usually replies blabla job is running, waiting. the todo was created before conditioned todo. lets
give it a door to mark existing todos as dependent on an agent or a job"*. A `needs` job edge already
silenced the check, but only on a row the model restated, never on the operator's, and nothing told
the model it existed.

**Landed on the branch:**

1. `todo_write`'s `wait_on` — `{"wait_on": [{"content": "<row, quoted>", "job": "j123"}]}` (handle
   or name), `"child": "<session id>"` in place of `job`, or `"detach": true`. Reaches the model's
   rows AND the operator's; writes a `TodoNeed::Job`/`Child` onto `needs` and never touches words or
   status; detach takes off job/child edges and leaves row edges. The job must be this session's and
   running (`jobs::resolve_job_in`), the child this session's (`ChildTodos::own_child`); otherwise
   refused by name. A call of its own. Board door: `TodoBoard::set_waits`.
2. The plan-check's advice line offers it, with a real handle, when the named row is the model's or
   the operator's and this session has a running job or live subagent (`RunningJobs` now carries
   all of the session's running jobs and its own subagents — `Harness::running_jobs`). Same line,
   so the head's notice fold is unchanged.
3. R79 reads `needs`; an attached wait IS a `needs` entry, so a completion it waits on is the plan's.

**Left:** restating one of the model's own rows in `todos` without the wait takes it off (the
upsert's contract — the reply says so). The parent's and queue's halves cannot take a wait.

---

## R83 — The operator can pause a session's plan-checks, loudly — **SETTLED 2026-10-30: `/nag`, the column, the rows and the resident line are in main — see `TODO-settled.md` R83. The record below is the entry as filed.**

The operator: *"and i need a way to stop todo nags"*, then *"or rather - pause and the pause must
be loud"*. Before this the only lever was per row (an operator `Postponed`).

**What landed.** `/nag` (the state), `/nag pause [30m|2h|1h30m]`, `/nag resume`; `off`/`on` are
aliases. Per session, on the session's row (`session.nags_paused_until`, schema **v25**, a guarded
`ADD COLUMN` that is safe to renumber) and on `Config::nags_paused`, so it survives a restart and a
resume; a pause whose end passed while the session was closed is ended at open and said. While
paused, `TodoNagClock` arms only the pause's end (`NagHold`): no idle check, no repeat, and not
R77's sooner check; the board still takes every edit. The pass that reaches the end lifts the pause,
runs no turn, and re-arms. **Children**: `/nag` sent to a subagent is answered on the child's own
thread (its clock lives there; the worker hands the verb back); a parent's pause does NOT cover its
children — per session, said in the verb's reply. **Loud**: `nags_paused` / `nags_resumed` rows on
the hub (Routine — the register is pinned to refusals; the resident line is the loud half), a
`nags` settings row (`on` / `paused until 14:30`), a resident warning line above the composer on
every frame, a `/status` row, `· nags paused` on the picker row, and `n` in the todos pane (R69
table) toggling it. The model is not told — the notice text is untouched. **R79's held batch while
paused**: still held, never stranded — it rides the next turn the model takes anyway (a prompt, a
parent's message, an immediate wake, a continuation), or the first plan-check after the resume;
results the plan waits on are immediate and never held, so the pause cannot delay them. Code: `crates/harnessd/
src/nag_pause.rs`; tests `harnessd/tests/nag_pause.rs`, `child_nag.rs`'s paused child, the clock in
`sessions/idle_nag.rs`, three in `tui/src/app/tests/settings.rs`.

**Left:** a child paused WITH an end does not park until the end (its clock holds that deadline) —
by design, so the resume is said on time; one paused until resumed parks as usual and comes back
paused. The jobs pane does not list paused sessions (the picker does). The Sessions-side hand-back
of `/nag` to a child's reader (`Hub::give_back_to_its_own_reader` now takes a `Slash`) has no test
of its own; the child's own arm does.

---

## R60 — `!term` is built; the pane has three things it still does not do — **SETTLED 2026-10-30: scrollback, several panes and a confined pane are in main, and so are R74 items 22 and 30 — see `TODO-settled.md` R60. The record below is the entry as filed.** Filed against protocol 44; the fourth was closed afterwards, and the two acts a pane can end with were split out of it.

The operator: *"i mean i want it broooo"* — `! mc`, `! nano` running **in the pane**, the
conversation's rectangle given to the program with the composer keeping its rows. It runs:
`!term <command>` at the composer, a pty the daemon owns (`crates/tools/src/exec/term.rs`,
`setsid` + `TIOCSCTTY`), raw bytes both ways on protocol 31
(`ClientFrame::TermOpen`/`TermInput`/`TermResize`/`TermClose`,
`ServerFrame::TermOutput`/`TermEnded`), the head drawing them through `letibot_vt::Screen`
painted by `letibot_ui::ansi::pane_rows` in the conversation's rectangle, and `ctrl-\`
**leaving the pane without ending it** — intercepted on the raw byte stream, so the program
never receives it and cannot trap it. **A bare `!term` attaches** to the pane the session has
(protocol 32: `TermAttached`, and the daemon's replay of the screen it holds) — see item 4
below, which is done.

**And ending a pane is its own act, which asks** (protocol 34, `agent/term-detach`): `!term
close` raises a confirmation card naming the program and sends nothing until the operator
answers it with `y` — deliberately not Enter, which is the prompt card's key and the
composer's, and anything else cancels. The two questions a pane can put on the screen must not
be confusable, so they differ in words and in keys: the program's own `PromptRequested` card
takes the screen back if it arrives while the confirmation is up. `ctrl-\` sending *nothing*
is what makes the program keep running on the daemon's pty and a later `!term` an attach to the
**same** run, and it is why the daemon needed a read to answer *what is this session's pane
running* (`ClientFrame::TermStatus`, answered off `Terminals::status`) — a head that is not
drawing the pane draws that fact instead, and files no row, because a detach is not an event.

**What is not done, and none of it is a defect in what is:**

1. **No scrollback.** A program that scrolls *off* the pane's rectangle is gone. The decision
   it waits on is whether the ring is the screen's (a rendering question) or the daemon's (a
   storage one) — see `TermPane` and `exec/term.rs`'s header. `less` and `git log` are the
   ordinary commands that will want it.
2. **One pane per session, and no second one.** A second `TermOpen` while one is live is
   refused in a sentence rather than replacing the running program. Two panes on one screen is
   a layout question this head has no answer for.
3. **The pane is not confined.** It runs in the session's workspace with the session's
   terminal environment and **not** inside the namespace boundary a `bash` call gets.
   `exec::confine` is the seam; what a confined pane has to keep working (`/dev/tty`, the
   cgroup, the workspace mount) is the whole of the work.
4. **DONE (`agent/term-attach`): a head that switches away finds its pane again.** A bare
   `!term` attaches to the pane the session has. **The screen is the daemon's**: the driver keeps
   the last 256 KiB the program wrote (`harnessd/src/term.rs`, `Pane::log`), a bare `TermOpen`
   answers with `TermAttached` (what is running — only the daemon was ever told) and then
   replays that log as `TermOutput`, and the pty is resized to the *attaching* head's rectangle.
   The replay is what the attach is proved by, and the nudge is not: `TIOCSWINSZ` with an
   unchanged size raises no `SIGWINCH` at all (`exec/term.rs`'s
   `a_same_size_resize_is_not_a_nudge`), so a redraw would have to come from somewhere else.
   What is still not here: **a scrollback** (item 1 above), and the two things the log's cap
   costs — an attach to a pane that has drawn more than 256 KiB replays the tail, trimmed to the
   next `ESC` so a cut cannot be painted as text.

**And the sibling this waits on.** `shell.rs` names `ShellLine`/`ShellTurn`/`ShellResize`/
`ShellEnded` for a **line** typed at a shell the daemon keeps, and this branch deliberately did
**not** add them: a `Turn`'s status and cwd have no meaning for a screen program, so the pane
has a byte-stream pair of its own. That module's own TODOs are now narrowed to the *line* case
(streaming a line, a keystroke at a shell that is not a pane, the registry) and are still open.

**still open?** `cargo test -p letibot-sessionlog --test term_pane`,
`cargo test -p letibot-tools --lib exec::term`, `cargo test -p letibot-harnessd --test
term_pane_live` (which is where a real pty, the real driver and the socket meet) and
`cargo test -p letibot-tui --test term_way_out` (whose assertions are on **what the socket
carried** — the only place *did anything leave the head* can be asked) — and `!term mc` at a
live head, which is the one check that is not a test and is the operator's.

**Built on `r60-term` (2026-10-11), the three and the two R74 items beside them:**

1. **Scrollback — the ring is the screen's.** A scrollback row is a row a line feed pushed off
   the top of the main screen, and only the thing that applies the line feed knows that, so the
   ring is `letibot_vt::Screen`'s (`history`, `HISTORY_ROWS` = 2000, xterm's rules: main screen
   only, a region starting at the top only, `ED 3`/`RIS` clear it, rows trimmed of trailing
   blanks; `Screen::view(back)` is the rectangle `back` rows ago). The daemon's 256 KiB log is
   what *refills* it on an attach, so the cap is the scrollback's depth after one. The head: a
   wheel notch over a pane whose program asked for no mouse, on the main screen, moves the view
   3 rows (`TermPane::back`, `letibot_ui::ansi::pane_rows_back`), sends nothing, and the first
   byte that is the program's snaps back to live; rows printed while reading back are counted
   into `back` (`Screen::scrolled`) so the page stays put. Tests:
   `vt … a_line_scrolled_off_the_top_is_kept_and_can_be_read_back` (and two neighbours),
   `tui … pane::a_wheel_over_a_pane_reads_back_what_scrolled_off_it` (FAILS on main: the notch
   is stripped and line 40 is gone).
2. **Several panes per session (protocol 44).** A second `!term` runs beside the first; each
   pane has a number the session never reuses; `TermInput`/`TermResize`/`TermClose` carry it,
   `TermOutput`/`TermEnded` carry it back, `TermAttached { pane, command, opened }` also answers
   an open (the number before the first byte — keys typed before it are held and sent under
   it), and `TermStatus { panes }` lists them. A head draws one: `!term N` attaches to pane N,
   bare `!term` to the most recent, `!term close N` ends one (a bare close means the pane the
   head holds; with several and none held, it names them and asks nothing). Capped at 8 live
   per session, refused by name. Tests: `harnessd --test term_pane_live
   a_second_pane_runs_beside_the_first_and_each_is_named_by_its_number` (replaces
   `a_second_pane_is_refused_by_name_while_one_is_live`, which passes on main — the refusal),
   `tui … pane::a_head_draws_one_of_several_panes_and_closes_the_one_it_names`,
   `sessionlog --test term_pane` (the `!term N` / `!term close N` parse).
3. **The pane runs inside the session's boundary.** The harness installs
   `Registry::bound_panes(session, PaneBoundary)` beside its run ender — its process host's
   confinement, read when the pane opens (`HostProcesses::terminal_wrap`), so grants made since
   are in it — and `Terminals::open` puts that argv between the scope's join and the shell.
   `Confinement::wrap_terminal` is `wrap` without bwrap's `--new-session`, which is what keeps
   `/dev/tty` (measured: with it, `/dev/tty` is `ENXIO`); `HOME`/`SSH_AUTH_SOCK` are the view's
   (`exec::term::confined_env`). Tests: `tools … exec::term::tests::
   a_confined_pane_sees_only_its_view_and_keeps_its_terminal` (the control in the same test is
   the job's argv: no ctty), `harnessd --test term_pane_live
   a_pane_runs_inside_the_boundary_its_session_installed` (FAILS with the boundary not passed:
   the file beside the workspace is SEEN).
4. **R74 item 22, mouse mode.** `Screen::mouse()` records `?9/1000/1002/1003` and `?1006`
   (and `?1` DECCKM); `TermPane::route` decides each report: program asked → sent at the pane's
   coordinates in its encoding, filtered to its level; nothing asked + alternate screen → the
   wheel is three cursor keys (`less` scrolls); nothing asked + main screen → scrollback. Test:
   `tui … pane::a_program_that_asked_for_the_mouse_is_sent_it_translated_to_the_pane`.
5. **R74 item 30, a report split across reads**: held (`TermPane::held`) and finished by the next
   read, or sent as the bytes it was. Test: `tui …
   pane::a_mouse_report_split_across_two_reads_is_put_back_together` (FAILS on main: `l` and
   then `;5Ms` typed at the program).

**Left, and the operator's:**

- **`!term mc` at a live head**, and now `!term less big.log` (wheel), `!term bash` + `seq 500`
  (wheel back), two panes and `!term 1`/`!term 2`, and a pane in a confined (`coder`) session.
  No live tmux check was run for this branch.
- **A confined pane has no network and no `ttyname`** (the view's own `devpts`): `!term ssh` in
  a confined session fails as `! ssh` does, and `tty` prints *not a tty*. A session with **no
  process host** (read-only seat, VM placement) installs no boundary and its pane runs on the
  host as before — said in `harnessd/src/term.rs`; a ruling if that should be a refusal.
- **Two heads opening panes in the same session at the same instant** can adopt each other's
  number (the `TermAttached { opened }` fan-out); a per-connection reply would close it.
- **A pane this head is not drawing that dies** files no row on this head (it only leaves the
  status list); the head that held it files it.
- `ESC [` at the very end of a read is held for the next one (Alt-`[` waits a read); a lone
  `ESC` is never held.

---


**2026-10-11, after R60 landed — `tty` in a confined pane.** The operator: *"wtf is with tty?"* — a
confined pane printed *not a tty*. The boundary's fresh devpts does not hold the host's
`/dev/pts/N`, and glibc's `ttyname` refuses a pty from another devpts instance on purpose. Fixed
by running a confined pane's program under `script` (`term.rs` `RELAY`): the pty is opened inside
the boundary, so `tty` names `/dev/pts/0`, nested terminals still open, and the size follows.
The alternatives were measured and each broke something (see `RELAY`'s doc). A box without
`/usr/bin/script` runs the old line. Still open from the operator: whether the operator's OWN
pane in a confined session should get the session's no-network rule (`--unshare-net` when the
egress policy denies) — today it does.

---

## A pane's first resize is a real `SIGWINCH` to a program that has not drawn yet, and one test races it — **CLOSED 2026-10-10 by `401e61fe`: the size is set before the spawn**

`TermSession::start` spawns the child (`crates/tools/src/exec/term.rs`, `cmd.spawn()`) and
**then** sets the pty's size (`set_size(&master, cfg.cols, cfg.rows)`, four statements later).
The pty is born at the kernel's default `0×0`, so that ioctl is a real change to a tty the child
is already the foreground process group of, and the kernel raises `SIGWINCH` for it. A
full-screen program therefore gets a resize before it has drawn once — harmless in itself, and
the reason this is filed rather than treated as an emergency — and it makes `exec/term.rs`'s
`a_same_size_resize_is_not_a_nudge` **flaky**: that test starts a shell with
`trap 'echo WINCH' WINCH`, waits 300 ms, resizes to the size the pty already has, and asserts no
`WINCH` appears. The trap catches the *startup* signal whenever the shell reached its `trap`
statement before the parent's ioctl ran, which is a race the child wins on a loaded box.

**MEASURED 2026-10-06** while landing `agent/term-detach`:
`cargo test -p letibot-tools --lib a_same_size_resize_is_not_a_nudge` passed **8/8 alone**, and
the same test failed inside one full `cargo test -p letibot-tools` run with
`a TIOCSWINSZ that changes nothing must raise no SIGWINCH, and this one did: "WINCH\n"`. That
branch's diff does not touch the test at all (`git diff main -- crates/tools/src/exec/term.rs`
has no hunk inside it), so this is older than it and not caused by it.

**The fix, and it is the ORDER rather than a tolerance:** set the size **before** `cmd.spawn()`,
which is what the line's own comment already claims — *"The size before the first byte"*. It
needs a `Pty::set_size(&self, cols, rows)`: the winsize belongs to the tty, so an ioctl on the
master before the child opens the slave is enough, and `into_master`'s *call it after `spawn`*
rule is about the parent's own slave handle and not about this. Nothing else changes: the child
is born at the right rectangle instead of being resized into it.

**still open?** `cargo test -p letibot-tools --lib a_same_size_resize_is_not_a_nudge` — which
will pass alone, so the check that matters is the full `cargo test -p letibot-tools` on a loaded
box, or reading the order of those two statements in `TermSession::start`.

**CLOSED 2026-10-10 by `401e61fe`** (*"term: the pane is born at its size, so no resize is a
change"*): `TermSession::start` sets the winsize on the master **before** `cmd.spawn()`, so the child
is born at the pane's rectangle and the first `TIOCSWINSZ` changes nothing.
MEASURED 2026-10-31 on lab2x1: `a_same_size_resize_is_not_a_nudge` green alone **and** 48 of 48
under the load that used to find it — sixteen copies of its own test binary, three rounds,
`--test-threads=1` (the recipe in `the-load-a-racy-test-needs-is-sixteen-copies-of-itself`).

---

## A stop unlinks the socket without the daemon going, so the next start adds a SECOND daemon to one folder — **CLOSED 2026-10-11 by `f354b051`: the claim is an `flock` beside the socket**

**The symptom the operator met:** a session that could not be written to at all. Every append refused:

    s-1789462738453908838 · dead -> store: row 483 (s-1789462738453908838#t45.483):
        sqlite: transcript_item seq must be the next one

Not the context wall — the wall was long past. The head delivered, the daemon tried to record the row,
and sqlite refused, so no turn could start. The session was bricked: nothing could be said to it,
because saying anything is a write.

**The store is right and the trigger is doing its job.** `tokencore/src/store.rs:528`,
`transcript_item_append_only_insert`: *"A hole, an overlap, a reordering or a re-insert at an old
index is refused by the database."* MEASURED against the store: `#t45` held **510 rows, seq 0–509**,
so the next seq is 510 — and the daemon was offering **483**, twenty-seven behind. A re-insert at an
old index, refused exactly as designed. Without that trigger two daemons would have interleaved rows
into one transcript and the hash chain would have stopped meaning anything silently.

**THE CAUSE IS THREE DAEMONS ON ONE FOLDER.** Measured:

    2124394  21:27  ~/Projects/letibot
    2210747  22:26  ~/Projects/letibot
    2264222  23:09  ~/Projects/letibot     <- the head's

and two of them were **LISTENING ON THE SAME PATH WHILE THE FILE DID NOT EXIST**:

    ss -xlp:  /run/user/1000/letibot/42ce9f1aae08.sock  harnessd pid=2210747 fd=11  LISTEN
              /run/user/1000/letibot/42ce9f1aae08.sock  harnessd pid=2264222 fd=11  LISTEN
    ls:       cannot access that path: No such file or directory

**The guard is sound and is keyed on the wrong thing.** `server.rs:176` connects to the existing
socket first and refuses with *"already served by a live daemon"* when it answers, unlinking only
when it does not. Correct — **but `remove_file` also lives on `ServerHandle::drop` (`:106`, `:114`)**,
and `scripts/letibot`'s connect-or-start tests `[ -S "$SOCKET" ]`. So:

  1. the operator stops the daemon; the handle drops and the socket FILE is unlinked;
  2. the process does not actually go — the state R30 already names, *"the daemon was asked to stop
     and had not gone"* — and keeps listening on a now-nameless inode, holding the session's ledger;
  3. the next start finds no file, probes nothing, and legitimately starts a second daemon;
  4. the older one keeps appending. Its rows land; the newer one's ledger is stale; the trigger
     refuses every write the head makes.

Confirmed on the way out: `2210747` went on TERM, **`2124394` ignored TERM for fourteen seconds and
needed KILL** — the same not-going behaviour, from the other side.

**Liveness is being inferred from a filename, and an unlinked socket is indistinguishable from a dead
daemon.** That is the *absent means gone* assumption this tree has been bitten by repeatedly. The
answer is already on the fabric, in a note from 2026-08-29: *"Single-instance guards: pidfile is
defeatable, pgrep matches its own launcher, **use flock**."* A held lock dies with the process and
cannot be removed by a shutdown path, which is the property the socket file does not have.

**still open?** start a daemon for a folder, stop it so the file is unlinked, confirm the process
survives, then start again and count daemons for that folder: `ss -xlp | grep <workspace>.sock`.
Two LISTEN rows on one path is the defect.

**done when** a second daemon for a folder that already has a live one is refused by name, with the
refusal naming the live pid, and the refusal does not depend on the socket file existing.

**CLOSED 2026-10-11 by `f354b051`** (*"one daemon for one folder, because a filename cannot say so"*).
`sessionlog/src/server.rs` takes an `flock` on `$KEY.lock` beside the socket **before** it looks at
the socket file at all (`FolderClaim::take`), writes the holder's pid into it so the refusal can
name somebody, and never unlinks that file — its name keeps pointing at the inode that holds the
claim, which is the property `$KEY.sock` never had. The socket's fate follows the daemon's in both
directions: `unlink_if_ours` removes the file only when the path still names **our** (dev, ino), so a
stopping daemon cannot unlink its successor's socket, and the accept loop is non-blocking so a daemon
whose file is already gone can still be stopped politely. `scripts/letibot` asks the kernel instead of
the filename (`folder_lock_pid`, over `/proc/locks`) before it decides to start anything.
Tests: `crates/sessionlog/tests/one_daemon_one_folder.rs` — seven, and the entry's own *still open?*
check is one of them (`the_refusal_does_not_depend_on_the_socket_file_existing`). All seven green on
this box, 2026-10-31.

---

## A fold that SUCCEEDS is thrown away by the store — **CLOSED 2026-10-04 by `f0d9c9a1`: the fork IS a store row**

On `glm-coding/glm-5.3`, so the item above does not apply:

    · compact_half — compacting half 1 of 1: the conversation so far — 292 item(s), 568330 token(s) to read
    · compact_half — half 1 of 1 answered: 6146 chars written.
    ! auto_compact_failed — store: row 594 (s-1789462738453908838#t44.594):
        sqlite: transcript_item seq must be the next one.

**The model did the work and the write refused it.** 292 items and 568,330 tokens were read, a
6,146-character summary exists, and it is discarded — so every retry pays for the summary again.
This is the more expensive of the two failures: the provider one fails before spending anything.

**Not diagnosed, and deliberately not guessed at.** The question to start from is whether `#t44`'s
first row is numbered from the PARENT's sequence rather than the fork's — row 594 arriving where
the fork expects its own next is the shape a fork-numbering bug takes, and it is cheap to check
against the store.

**still open?** reproduce a fold on a session large enough to overrun, or read `#t44`'s rows out
of `sessions.db` and compare the first `seq` against the parent's last.

**done when** a successful fold's summary is persisted, and a sequence violation on the fork's
first row is a named refusal rather than a discarded answer.

**CLOSED 2026-10-04 by `f0d9c9a1`** (*"C1 wiring: /compact forks the transcript; the summary becomes
the base"*). The summary is written where the conversation continues: `fork_onto`
(`crates/harnessd/src/harness.rs`) opens the NEW transcript id (`s-…#t1`), appends the summary as its
first item, and `Store::put_fork_unsealed` writes the row that links the two — so the fork's first row
is numbered from the fork, which is the shape the paragraph above said to start from. The seal, the
fingerprint and the stale flag follow the last row (`Harness::seal_fork`, R91.2), so a crash mid-carry
is not a partial conversation that a resume lands on.
Test: `crates/harnessd/tests/compact.rs`'s
`a_compaction_fork_is_a_store_row_and_a_resume_lands_on_it` — two transcripts, the fork's parent
named, the summary verbatim in row 0, and a reopen landing on the fork. Green here 2026-10-31.

---

## R56 — the view can be held, so a selection survives a streaming turn — **BOTH HEADS HAVE IT (`c9634bc` leticl, `2b49304` here)**

The operator, on losing a text selection while a turn streams: *"leticl resets selection if screen
wasnt scrolled too. so both should not do it if anything selected. whether it means stopping render and
showing me 'new content' marker - likely."*

**Two measurements decide the design, and the first kills the obvious fix.** `tmux pipe-pane` on both
heads: leticl erased NOTHING in a twenty-two minute turn (no `ESC[2J`, no `ESC[K`) and still lost the
selection — **any write into a selected cell clears it**, so erasing is not the trigger and there is no
gentler way to paint. And the head **cannot detect the selection at all**: with mouse reporting on,
holding Shift tells the TERMINAL to select and not to forward the events, which is exactly why Shift is
the gesture. So *do not repaint while something is selected* is not implementable as written; the
reader is the only party who knows, so the reader holds the view.

**The contract, and it is all of it: while held, the head writes NOTHING.** Not a spinner, not a clock,
not a counter that ticks. One written cell is one lost selection. The events keep arriving and the head
keeps folding them — it simply stops drawing — so this needs no protocol change at all. Three pieces on
leticl's side, each in the one place that owns it: `paint-wanted-p` (the loop's only gate on painting —
a held view outranks both an event and the clock, and the loop still SLEEPS on a refusal, or a held
screen would spin a core), `*frozen-frame*` (the marker cannot be drawn without a write and cannot be
drawn while nothing writes, so the freeze owes exactly one frame, spent by the paint after the bytes
are out), and `frozen-lines` (the row itself).

**The chord is `ctrl-p`, and it is AGREED rather than picked** — the operator's first choice was
`ctrl-f`, which is taken by the composer's emacs motions (*ctrl-f goes right*, and the reference's
decoder binds it too); `ctrl-p` is free because the todos pane gave it up when that moved to `ctrl-t`,
and *pause* is the better mnemonic. An operator who learns it on one head will reach for it on the
other, so it must be the same key and the same words on both.

**The wording, verbatim, so the two heads cannot say it two ways:**

    ⏸ the view is held — ctrl-p follows again

and on the release, one note: **`the view follows again — N rows arrived while it was held`**, counted
ONCE at that moment, because a live count while held would be an animation and an animation is writes.

`c9634bc` is the whole of it on leticl's side, with the probe output in the commit message.

**THE CHORD DOES NOT TRANSFER, and this is the one thing to settle before any code.** `ctrl-p` is
free on leticl *because its todos pane gave the key up and moved to `ctrl-t`* — and **here `ctrl-p` IS
the todos pane** (`term.rs:781` decodes `0x10`, and the bar reads `ctrl-p todos`). `ctrl-t` cannot be
the destination here either: this head already spends it on the payload window (`ctrl-t newest
result · /t all tool rows`, which the operator reads off the bar and which R10 narrowed to that
meaning on purpose). So on this side the freeze can only have `ctrl-p` if the TODOS PANE MOVES, and
the decoder's own comment says what is left: *"`ctrl-v` (`0x16`) and `0x1c`-`0x1e` are the only bytes
left in this table with no arm"*. None of the four is mnemonic for *plan*, and none is mnemonic for
*pause* either — `ctrl-p` is the only letter that names the new act, which is why the conflict is on
the pane rather than on the hold.

Two ways out, both honest, and they are the operator's to pick:

  · **mirror leticl: todos → `ctrl-t`, and the payload window → `ctrl-v`** ("view"). Best parity —
    the two heads then agree on the freeze AND on the pane — and it moves a key in daily use, with the
    bar entry and its tests to match.
  · **todos → `ctrl-v`, keeping `ctrl-t` as the payload window.** Nothing the operator uses today
    changes, one pane key stays divergent (which it already is: jobs is `ctrl-q` here and `ctrl-j`
    there, and nobody has minded).

**The operator's answer, 2026-10-03: *"yes leticl reworked hotkeys"*** — read as the FIRST route,
because that is the only reading in which leticl's rework is the reason rather than the trivia: leticl
has already paid for the rework, so this head converges with it. todos → `ctrl-t`, the payload window →
`ctrl-v` ("view", and free — `term.rs`'s own comment lists `0x16` as unclaimed), and `ctrl-p` is the
hold's. The words and the marker sentence are unchanged from the agreement above. If that reading is
wrong, one word stops it — and it is worth the one word, because the cost of being wrong here is the
reader's muscle memory rather than a compile error.

**still open?** No — `2b49304` is the whole of it here: `App::toggle_hold` on `ctrl-p`,
`App::screen`'s one-frame freeze with the marker on it, `App::take_redraw`'s refusal, and
the panes moved to make room (`todos` → `ctrl-t`, the payload window → `ctrl-v`). The route
taken is the FIRST one the chord section above offers — the mirror of leticl — which is what
the operator's *"yes leticl reworked hotkeys"* was read as, and it is recorded as a reading
rather than a ruling because being wrong costs muscle memory and no compiler catches it.
`a_held_view_draws_the_same_frame_while_the_turn_streams` drives all three clauses.

**done when** a multi-round turn on their head writes nothing between the freeze and the release (the
byte-level assertion leticl's suite makes), the marker appears exactly once, and the release says how
much arrived.

---

## R58 — subagent TREES: delegation to a configurable depth, and no settlement without a worker — **LANDED 2026-10-03 (`9a9e8a1` + `51d5bd2` + `53e829c`); the design note below is kept for what it argued**

> **A feature, not a fix, and keeping that distinction is the first thing this row is for.** It exists
> because a commit message overstated a live defect that does not exist.

### The premise, verified rather than taken

**Delegation is one level today, and that is a fact about the SEAT TABLE rather than a property of the
design.** `crates/tools/src/runtime.rs`'s `roles::m2_coder()` — the seat every subagent is re-seated to
(`Seat::Coder`, `harnessd::config`, which re-seats for the tools and must not re-confine) — names
exactly

    read · write · edit · grep · glob · read_spill · todo · bash

and **`task` is not among them**, while `roles::orchestrator()` and `roles::leticode()` both name it.
So a grandchild is unreachable **in today's binary** — and the operator has ruled what the design is:
*“subagents are absolutely allowed to spawn subagents up to configured nesting level.”*

**CORRECTED 2026-10-03, twice, and the second correction is the one worth reading.** `660b585` filed
the absent `task` as a **safety property** — *“the guarantee is in the seat tables rather than in a
guard”* — concluded that there was **no live grandchild case**, and **retracted `fd4aaad`'s finding** as
an overstatement. All three are the same mistake, and it is not about subagents:

> **An inspection of what the code does today cannot correct a statement of what it is supposed to do.**
> The seat table lacking `task` is evidence about the **binary**. The ruling is evidence about the
> **requirement**. Where the two disagree, the binary is the thing that is wrong — which is what makes
> this a TODO item instead of a description.

The same shape as three defects fixed the same day — `plan_overrun`'s note describing a decision the
code no longer made, the three `rano` manifests whose comments still said *“the absolute path is
deliberate”* after the lines took a git tag, leticl's `install.sh` requiring eleven files while its
workflow packaged ten — with one difference that matters: in each of those the CODE was the authority.
Here the claim was the operator's own ruling, which is the one case where that ordering is never right.

**`fd4aaad` did not overstate it, and that retraction is WITHDRAWN.** The finding — *a nested child's
harness is built inside the runner's thread, outside `Sessions::open`, so nothing serves its wake* — was
correct when it was written and is correct now, and it is asserted in the code at `Sessions::wake`'s
early return and at the `registry.adopt` that creates the situation. Restoring it matters more than any
framing does: a reader who finds a retraction stops looking, and whoever writes the trees will need
exactly that finding. So the missing seat is the gap **this requirement exists to close**, and the
nested-wake problem below is live now rather than on the day trees land.

### The ruling

**Trees are allowed, to a configurable depth, default 3.**

### Two things settled while this is still a plan, because they change the diff

**1. The depth limit rides the CONFIG, not the seat table — and the idiom already exists.** *“Does the
seat name `task`”* can express exactly two depths, zero and unlimited, so it cannot carry a
*configured* limit at all. `base_role_for_seat` (`harness.rs:7508`) is already the shape that can:
`m2_coder` **lists** `bash`, and the config **strips** it when `!cfg.allow_bash` — the comment there
says why, and it is the same sentence this needs: *“listing it here is what makes the flag mean
something for this seat.”* So `task` is listed on the seat and stripped where the depth says no, with
the counter riding the config the child is built from — a `depth` field on the config, incremented once
at spawn (`harness.rs:7262`'s `sub_cfg`, built from `self.base`), and read where the tools are seated.

**And the refusal is by NAME, not by absence.** A seat that simply lacks `task` at the limit
manufactures the workaround, and that is this tree's own recorded lesson in the same file four hundred
lines away — `m2_coder`'s comment on `todo`: *“a capability that exists but is hidden manufactures the
workaround”*, measured at 13 tool calls and ~15k tokens of a model emulating the tool it had not been
given. A `task` call past the limit gets a refusal that names the knob.

**2. The wake fix is NOT depth-N-safe, and what is missing is the SERVANT rather than the routing.**
Verified in the code instead of argued:

  · `settled_here` (`jobwatch.rs:520`) rings `bell.ring_wake(&hub.session_id())` — and **which hub that
    is depends on whose watcher it is**: every harness builds its own, so a child that spawns a
    grandchild rings for the CHILD's session id;
  · `Sessions::wake` (`sessions.rs:1634`) is the only servant, and its second line is
    `let Some(harness) = self.open.get_mut(session_id) else { return Outcome::Ignored };`
  · a child harness is built inside the runner's thread (`harness.rs:7278`) and **adopted into the
    registry, not into `Sessions::open`** (`registry.adopt`, line 7288) — which is exactly why a head
    can peek at it and attach to it while the daemon still cannot drive it.

So depth 1 closes **because the watcher belongs to the PARENT, and the parent is in `open`**: the bell
is rung for a session the daemon owns, `wake` drains the queue, and the `[task]` row lands in the
parent's transcript. At depth 2 the watcher belongs to the CHILD, the daemon is woken for a session it
does not hold, and it **returns `Ignored` — the condition fires and is discarded**, which is the shape
of failure this row's own design note names. Routing a settlement through `completion_notice` therefore
buys nothing here: *a job is a job* is true of the **routing** and says nothing about the **servant**.

**Which is what “one slots list and one watcher set per TREE, rooted at the top session” is for** — and
it now has a reason rather than an assertion: **the bell must ring for a session the daemon can
drive**, and the only such session in a tree is its root.

### Four pieces, and the first three are mechanical

1. **Seat `task` where the depth allows it.** `task` is `Access::Session`, asserted and argued in
   `builtins/task.rs`: *"`task` runs no host command itself — it delegates to a child turn whose own
   gate governs its write/exec/network calls"*, and *"a subagent that could not even be spawned
   without a head would never run."* That is the same class as `todo`, which `m2_coder`'s own comment
   records as needing **no gate change** — so this piece is a seating decision and not a permission
   one. It does meet §8.4's ceiling: `m2_coder` is seven tools without `bash` and eight with it, so
   the daemon's `max_tools += 1` shape (`harness.rs`, the `bash` and `web_fetch` precedents) is what
   the addition follows.
2. **A depth counter on the spawn path**, so `task`'s seating is a function of where the child sits
   rather than of a flag somebody has to remember.
3. **The knob**, default 3, beside the other session settings.
4. **Orphan-proofing, which is the load-bearing piece and the reason a depth of 1 is not simply
   raised to 3.** See below.

### The piece that is not mechanical: a settlement must ring a bell that has a worker

The working design, and it is the whole of what makes depth safe: **one slots list and one watcher
set per TREE, rooted at the top session** — so a settlement always rings a bell that has a worker,
and **any ancestor can collect the handle**.

That is R57’s own finding with one more level under it. R57 is about a settlement nobody hears
(the bell and the worker are not paired, and the child's completion is a firing rather than a
wake); at depth > 1 the same defect gains a floor: a child's child settles into a tree whose root may
be the only worker left, and a handle held by the wrong level is a handle no key reaches. The shape
above is what makes "impossible at any depth" a property of the structure rather than a rule
re-checked at each level.

**Not to be confused with T24, and the word *orphan* is what conflates them.** T24 is about
**processes**: cgroups, worktrees, tmux sessions, what a child leaves running. This is about
**handles**: the slots list and the watcher set that turn a settlement into a wake. Two different
orphans, two different owners, and neither fix touches the other.

**LANDED 2026-10-03 (`9a9e8a1`, the handle-list half in `51d5bd2`, the flag in `53e829c`) — all four
pieces, with the two that mattered made testable.**

  · **1. The seat.** `m2_coder` names `task` and `task_result` (`runtime.rs`), seated at every
    depth on purpose: the cap is enforced at the CALL, not by the seat's absence, because a seat
    that simply lacked `task` at the limit manufactures the workaround this role already paid for
    once (its own `todo` note). The pair goes together for `orchestrator`'s reason.
  · **2. The depth.** `Config::depth` (0 for a root) is incremented once at `spawn_subagent`'s
    `sub_cfg`, and rides the config down the tree like `unconfined` does.
  · **3. The knob.** `Config::max_subagent_depth`, default 3, `--max-subagent-depth`. The refusal
    is `subagent_depth_refusal` (`harness.rs`), a pure function so it needs no harness to test:
    it names where the session is, the level it would have opened, and the knob that moves it.
  · **4. The worker's bell, and ONE handle list.** A child's watcher set is built from its ROOT's
    (`JobWatchers::shares_tree`): `completions`/`watching`/`settled`/`stop`/`bell` shared, and a
    new `wake_target` carrying the root's id so the ring goes **up**. The root's set rides `Parts`
    as `tree_watch` (`None` at `Parts::load`, the parent's set at spawn), and the target propagates
    unchanged, so a tree rings one bell with no parent walk. **And the same for the handles**
    (`51d5bd2`): `Parts::tree_slots` seeds the child's runner with the parent's `slots`, so the root
    can `task_result`/`job_kill`/list every handle in its tree — without it the root is *told* about
    a grandchild's settlement and answered *"no subagent … in this session"* when it asks.

Tests: `a_grandchilds_settlement_reaches_its_parent_and_rings_its_parent` (jobwatch — renamed
2026-10-06; the ring half of that sentence is the CORRECTION below) drives a depth-2 settlement
through the CHILD's set and asserts the completion lands on the tree's queue and the ring names the
root; `the_depth_cap_refuses_the_spawn_one_past_it_by_name` (harness) pins the boundary; 
`the_coder_seat_names_the_delegation_pair` (runtime) pins the seating; and the plan-mode fixture
registers the pair — the same loud `resolve_role` path its own comment records for `todo` and
`bash`.

**still open?** `grep -rn "max_subagent_depth" crates/` finds the field, the flag, the refusal and
the two tests; `grep -n '"task"' crates/tools/src/runtime.rs` now names it three times
(`orchestrator`, `leticode`, `m2_coder`); `grep -rn "wake_target\|tree_slots\|shares_tree"
crates/harnessd/src/` names the ring target and both shared channels — **and it names the ring
target no longer**: `wake_target` was renamed on 2026-10-06 and the grep finds `tree_slots` and
`shares_tree` alone. The CORRECTION below says what the ring names now. **Not yet exercised end to end**:
no test spawns a real depth-2 tree through the daemon, because that needs two live harnesses and a
model — the mechanism is pinned at the `JobWatchers` layer, which is where the defect was, and the
handle-list sharing rides the same untested path.

**One naming consequence, settled rather than left open.** `depth` in this tree already means the
*model's* depth (`config.rs:252`), so the SESSION's level is `Config::depth` and the KNOB is
`max_subagent_depth` — two names, not one word doing two jobs. That is the consequence this
paragraph asked for, applied.

**REFINED 2026-10-03 — and the first bullet below is WRONG, kept with its correction because the
correction is the one worth reading.** The claim was that the slots list is per-daemon and so
*"any ancestor can collect"* needed nothing. **It needed the second commit.** `Parts::load`'s
`Arc<TaskJournal>` is the dashboard's state file, not the handle list; the handles live on
`HarnessTaskRunner::slots`, which is per-SESSION — `collect`, `kill` and `started` all read it, and
`kill`'s own refusal says *"no subagent `{handle}` in this session"*. So the root could be told about a
grandchild and could not collect it. `tree_slots` (`51d5bd2`) is the fix, and the mistake is the same
shape as this row's own premise: a sound-sounding inspection of one structure used to conclude about
another.

  · **(WRONG — see the correction above.)** The slots list is per-DAEMON, not per-tree. `Parts::load` (`harness.rs:138`) builds ONE
    `Arc<TaskJournal>` for the process — the same one-0.6s-load argument as the vocab — and every child
    inherits that same Arc (`spawn_subagent`'s `Parts { tasks: self.tasks.clone() }`, `harness.rs:7285`).
    So `task_result` already reaches any handle from any ancestor. *"Every ancestor's handle can be
    collected from any level"* is satisfied **more widely than the tree**; nothing to build. (If a
    tightening to *exactly* the tree is wanted later — so a sibling root cannot collect another root's
    child — that is a separate, deliberate narrowing and not part of this.)
  · **So the whole of piece 4 is the watcher set, and it is one field.** `settled_here`
    (`jobwatch.rs:520`) rings `bell.ring_wake(&hub.session_id())` — the **child's** id — and queues the
    `JobCompletion` on the **child's** `completions`. Both are per-session because the child's set is
    built fresh in `open_with_registry` (`harness.rs:2283`). At depth 2 that queues onto, and rings for,
    a session `Sessions::open` does not hold — the `Ignored` above, from one field rather than a missing
    structure.
  · **The shape the fix takes.** The tree's watcher set is the ROOT's and a child's is built from it:
    `completions`, `watching`, `settled`, `stop` and `bell` shared (Arcs — `with_tasks` already shares
    three of them), a `wake_target: String` carrying the root id so the ring goes **up** rather than out,
    and only `hub`/`host`/`tasks` new per child. The root's set rides `Parts` as a
    `tree_watch: Option<Arc<JobWatchers>>` — `None` from `Parts::load` for a root, `Some(self.job_watch)`
    at `spawn_subagent` — so `open_with_registry` finds it and shares instead of building. No new root
    walk: the set it is built from already knows its root.
  · **And piece 4 cannot land alone.** At depth 1 the watcher that rings belongs to the parent, which IS
    the root, so the change is **unobservable and untestable today** — a grandchild cannot exist until
    `task` is seated past level 1. Pieces 1 and 4 land together or the diff is scaffolding nothing can
    exercise.

**done when** a session at depth 3 spawns a child that spawns a child, each child's settlement wakes
the tree exactly once, **every ancestor's handle can be collected from any level**, and a depth one
past the knob is refused by name rather than by a stack that quietly runs out — with the correction
above carried wherever `fd4aaad` is read.

**CORRECTED 2026-10-06 (`agent/supervision`) — the ring does not go to the root, and the reason is the
operator's own design.** *"think about it like it is an erlang supervision tree. we talk to parents and
they own lifecycle."* Two things in the note above were the tree read as a **flat** structure and they
are both wrong at depth > 1:

  · **Ringing the ROOT put the ring and the notice on two different sessions.** The settlement is queued
    on the session that STARTED the work (that half is right), and the ring named the root — so the root
    woke, drained its own queue, found nothing and ran no turn, while the notice sat unread below it.
    **The ring now names the session that owns the settlement**: its parent. `JobWatchers::wake_target`
    became `me` (the owner) plus `parent` (the level above, whose queue a handed-up settlement goes to
    and whose bell the hand-up rings), and `tree_root()` reads a separate `root` field, because a card
    needs a session with a HEAD while an exit needs the session that STARTED it.
  · **"the only session a worker can drive is the root" was a fact about the daemon's `open` set, not
    about what can be served.** A child's harness lives on its parent's spawn thread, so `Sessions::wake`
    cannot run its turn — and that is exactly why it must HAND THE WAKE ON rather than discard it:
    `Hub::wake_its_own_reader` wakes the child's own condvar, the thread parked in `harness::serve_child`
    answers with `Harness::wake`, and the notice becomes a turn of the child's own. `Sessions::wake` is
    three arms now (`wake_route`): drive it, hand it to its own reader, or — a session with no hub at
    all — nothing, which is safe because a closing session hands its undrained settlements up a level
    first (`JobWatchers::stop`).

**And the downward edge, which the "one watcher set per tree" note never had.** A stop is a stop for the
subtree: `TaskRunner::stop_all` stops the children a session OWNS (`TaskSlot::owner`, `stoppable_children`)
and only those that have not exited (`TaskSlot::exited` — "answered" is not "gone", a host child parks
and can be asked more), it is called from the steering source's interrupt arm and from a child's serving
loop, and each child then stops its own. One level per session, so the walk is finite and every session is
answerable for exactly its own children. Measured before this landed: an interrupt reached exactly one
session and left a stalled grandchild computing for nobody.

**Still open, and it is the second half (marked `TODO(supervision-2)` in `harness.rs`): the restart
policy** — what a child's exit MEANS is not decided anywhere; a parent that wants a child restarted starts
one itself, and there is no `one_for_one`/`one_for_all`, no restarts-in-a-window, no strategy on the
parent. The seam is the parent's own drain of its child's settlement. The same half adds a runtime
`max_depth` (per-call and per-child); the bound in force tonight is `--max-subagent-depth`, and it is left
exactly as it is rather than given a second number.

---

## The `Subagent` event's `prompt` is a title, and on the finish it is the child's answer — **LANDED 2026-10-03 (`83be154`) on letibot's side; the wire now carries the task**

**(No R-number: the series is yours to number.)**

The operator, reading the subagents pane: *"the first prompt is truncated too early"* and *"I want to
be able to easily see it in full"*. MEASURED, and it is not a truncation the head can undo:

```rust
let title = derive_title(prompt);   // harness.rs:7177 — "the title is the subtask's first line"
publish("opening", &title);
publish("running", &title);
publish("done",    &first_line);   // harness.rs:7322 — the CHILD'S ANSWER's first line
```

So the one field a head has for *what this child was asked to do* is the task's first line from the
start, and on the finishing event it is replaced by what the child said. Read out of the running head:
the row for a child spawned from a session carried **122 characters which were its answer**, while the
task was two lines and ~250 characters — the head had no copy of the task anywhere, and the pane's
first line is where the operator saw it.

**The ask, and it is two small things:** carry the task itself on the event (the head can truncate for
a row — it already truncates everything else it draws), and if the picker wants the child's answer as
a subtitle then that is a second field, not this one rewritten: a field named `prompt` that holds a
title on two states and an answer on the third cannot be read by either party without knowing which
state it is.

leticl's half is already in: the child's task is drawn in full in the peek, from the transcript read
(`821d218`… `the-subagent-pane-draws-the-task-in-full-and-follows-the-rung`). The pane's *row* cannot
be fixed until this is.

**LANDED, 2026-10-03 (`83be154`).** Both halves of the ask, and the wire is letibot's so leticl's
pane row can be fixed now:

  · **`task: String`** on `SessionEvent::Subagent` — the subtask in full, the same string on every
    state, `#[serde(default)]`, and **never truncated by the daemon**: the head truncates for a row as
    it does for everything else it draws.
  · **`answer: Option<String>`** — the child's answer's first line, `Some` only on the finish. The
    same string `prompt` carries there, given its own name so a reader does not have to know the
    state.
  · **`prompt` is left exactly as it was**, two meanings and all, which is what keeps a head older
    than these fields byte-identical; a new head reads the row from `task` and falls back to `prompt`
    when `task` is empty — which is the pre-field behaviour, not a blank row. letibot's row does that
    (`subagents_lines`), flattens newlines because the row is one line, and draws `answer` as the
    subtitle; Enter still opens the whole thing in the peek.

`a_subagent_row_shows_the_task_and_the_answer_beside_it` pins both halves; the pane's older fixtures
carry `task: ""` and keep exercising the fallback.

**leticl:** the fields are on the wire with no protocol bump (a volunteered frame may grow a field,
not a variant), so its row can read `task` where it reads `prompt` today and its subtitle `answer`.

**still open?** `grep -n "task: task.clone()" crates/harnessd/src/harness.rs` finds the publication;
`grep -n "s.task.is_empty()" crates/tui/src/app.rs` finds the fallback. Nothing open on this side.

**done when** a long, multi-line task reads whole on the pane's row (or an unfold of it does) without
opening the child.

---

## The notice rows stopped on the daemon rebuilt at ~14:2x — **CLOSED 2026-10-04: they never stopped; two heads render one row two ways**

**(No R-number: the series is yours to number.)**

The operator, watching for the two things this file asked for: *"interesting - the job finished but i dont
see notification about it"*, and then *"subagent finished too - no notification. i think we had that
before restart"* — and the second sentence is the diagnosis.

**Measured, on the daemon rebuilt at ~14:2x (leticl head 1700841 attached to it):**

  · **the EVENTS are fine**: the head's jobs list holds `(("j19" "exited 0"))` — the `job_settled` event
    arrived and updated the row — and the `Subagent` events arrive too, so the subagents pane and the
    completion cards' inputs are all there;
  · **the NOTICE ROW is absent**: no `User { speaker: Agent }` row for either settlement — not on the
    glass, and not delivered to the model either. Both were present for every job and child before the
    restart (`Job j83 exited 0 …`, `Agent … · done: …`).

**CORRECTED THE SAME HOUR — THE PUBLISH PATH IS FINE, AND THE STORE PROVES IT.** Querying the daemon's own
store (`~/.local/share/letibot/sessions.db`, `transcript_item.item_json`):

    120138 | 2026-10-04 14:44:58 | {"type":"user","parts":[{"kind":"text","text":"[job] a job you backgrounded has ended:
    120012 | 2026-10-04 14:40:48 | {"type":"user","parts":[{"kind":"text","text":"[job] a job you backgrounded has ended:

Both written AFTER the ~14:2x restart. So `completion_notice` publishes, the row is in the model's
transcript, and **the operator's other head receives it** — *"yet a newly restarted letibot gets job
completion event"*. What does NOT have it is **leticl's attached head**: its newest agent-speaker row is
`j111`, from before the restart, and no notice row since appears in its items at all.

**So this is per-head DELIVERY, not the publish path** — one connection gets those rows and the other
does not — and the question is now narrow: what differs between the two heads' attaches (identity, hub
membership, or the seq they are served from) such that a `User { speaker: Agent }` row written to the
store reaches one and not the other. leticl's folding half is known good: given such a row its parser
turns it into the right card (measured on the pre-restart rows, `Job …, result …`).

**RESOLVED THE SAME HOUR, AND THERE WAS NO DEFECT.** A child was spawned to land a notice while the
operator watched, and leticl's own screen, read seconds later:

    session · Agent Report the number of lines in /home/dead/Projects/leticl/src/head.lisp, as one number, with no other words.
                  result  done: 2201 · /t opens it

The notice is delivered to leticl, folded and drawn. What the operator had been seeing was **letibot's
rendering of the same row**: `letibot_transcript::Speaker::Agent => (RowClass::Other,
session_block(&text, its.ts, cfg))` — the raw paragraph, unchanged, which is exactly the text they
pasted. leticl folds that row into a card; letibot wraps it whole. **Two heads, one row, two
renderings — and the parity question is whether letibot wants the card too**, not whether anything
is broken. Everything in this entry above is superseded: the publish path was fine, the delivery was
fine, and the one real finding was mine looking at the wrong pane.

**AND THE ONE FIELD THAT DIFFERS BETWEEN THE TWO CONNECTIONS IS THE IDENTITY.** leticl attaches with
`:identity "leticl"` (`src/head.lisp`, `%try-reconnect`'s `make-attach`), and the letibot head the
operator watches attaches with `--identity dead`. Everything else about them is the same daemon, the same
store, the same session, the same moment — and one of them is served a `User { speaker: Agent }` row and
the other is not. That is worth a look before anything else: **what the hub does differently for a
connection whose identity is not the one the row is addressed to** is the whole question now, and it is
one word to test from either side.

**still open?** start a background job or a `task` child on this box and watch for the `[job]`/`[task]`
row in the parent's transcript — absent means it is still open.

**done when** a settled job and a finished child each produce one notice row on the parent's hub, on a
daemon built from the current tip.

---

## A session's JOB notices STALL — 29 s late, or never — while its CHILD notices keep coming — **CORRECTED AND CLOSED 2026-10-04 15:40: it was never per-KIND; two settlements were swept by a transcript FORK**

**(This supersedes the CLOSED heading on the notice-rows entry above; that closure was too broad.)**

Two background jobs on this box, twenty minutes apart, both started by the model through the same path:

    j19  45009 ms  exited 0  -> its `[job]` notice IS in the store (14:44, and leticl drew it)
    j56   5012 ms  exited 0  -> NO `[job]` row anywhere in the store, minutes later

**And leticl's head saw BOTH settlements** — its jobs list reads
`(("j19" "exited 0" 45009) ("j56" "exited 0" 5012))` — so the `JobSettled` hop is fine for both. What
differs is the WAKE: the notice row reaches the transcript for one and not the other.

**The shape to look at is the queue-take**, and your own words point at it: *`wake()` is left with the
part that is not a decision: taking the queue and submitting what this returns* — and *an empty queue
returns nothing* rather than running a turn with nothing to say. A settlement that lands at, or after,
the moment the queue is taken has no second chance: nothing re-arms. Same family as D26 (*a wake spends a
generation*).

**What is NOT broken, so nobody re-tests it:**

  · **child completions publish**: one landed minutes ago and leticl drew the card off it, verified on
    the glass (`session · Agent Report the number of lines in head.lisp…` over `result done: 2201`);
  · **the publish path writes the store** (j19's row is there);
  · **leticl's fold is correct** on every row it receives, and the head is served them.

**still open?** start two background jobs a few minutes apart and query the store for both notices:
`select ... from transcript_item where item_json like '%<job id>%'`. One missing is this entry.

**MEASURED FURTHER THE SAME HOUR — THE SPLIT IS PER SESSION, NOT PER KIND.** Job notices are being
published on this daemon all day: 12 today, 21 yesterday, 25 the day before; the newest is 14:54:43.
**8 went to `s-1789462738453908838#t41` (newest 14:54:43) and 4 to `s-1791017230755743833`, whose newest
is 14:09:14** — that is the session which started `j19` and `j56`, and which has received no job notice
since 14:09. Its `[task]` child notices kept arriving throughout the same window (newest 14:56:50), so
this is not one session going deaf: **in one session one kind stops and the other kind keeps coming.**
Three jobs settled after 14:09: `j19` (~14:44), `j56` (~14:57), `j2` (`sleep 8; echo job-demo-two-settled`,
launched 15:08:30). **`j2`'s notice did arrive — 29 seconds late**, written 15:09:07 and handed to the model
right after; a store query one second before that read 6 notices, newest 14:09:14, which is why it first
looked lost. So the shape is a **stall, not a hole**: 29 s for `j2`, while `j19` and `j56` are still absent
12 and 25 minutes on. Latency against settlement, measured: `j2` 29 s; `j19`, `j56` unbounded so far.

**And the head was restarted at ~15:0x** — the operator's doing. The restarted head's job registry holds
**only `j2`**: `j19` and `j56` are not in it at all. So the restart is a concrete drop point: a settlement
whose notice is still queued when the head reattaches never gets submitted, and its job leaves the client's
registry with it. `j2`, started and settled after the restart, got its notice 29 s late — when the session
next went idle, not at the settlement.

**Prediction to test:** start a job and restart the head before the session next goes idle — the notice is
lost. A job that settles while the session stays continuously busy is delivered at the next idle turn,
however long that is. **Take every reading twice**: this defect has been misread four times here, each time
from one query treated as final.
**Take the reading twice before calling anything missing** — this defect has been misread four times here,
every time from a single query treated as final. So the wake is not dropping settlements at random — it
**keeps publishing job notices for one session and stops for another**, while children keep getting
through in the same session. Look at what makes a session eligible at the moment of the take, rather
than at the settlement itself.

**done when** every settlement that `JobSettled` reports is also submitted as a wake item, once, however
the timing falls.

**CORRECTED AND CLOSED — THE FIFTH READING, AND IT IS THE ONE THE ENTRY ASKED FOR TWICE.** The mistake
was the dimension the query grouped by: **`transcript_id` is not stable, and every reading above crossed a
fork without saying so.** Read per transcript, out of the store (`transcript_item`, `"type":"user"`):

    s-1791017230755743833#t2   14 job notices, newest 2026-10-04 14:09:14   tasks newest 14:56:50
    s-1791017230755743833#t3    3 job notices, newest 2026-10-04 15:34:36   tasks newest 15:35:05

`#t3` begins at **14:58:48**, and its first rows are a CARRY of the last few rows of `#t2` (`what child`,
a `[task]` notice, `ok now do job`) all stamped 14:58:48 by the copy — so the session forked 79 seconds
before that stamp, which is the context wall firing (`Your previous turn was stopped at the context wall`
is the first row of every fork on this box, mine included: `s-1789462738453908838#t42` at 15:18:17).

**So the two missing settlements were not a channel going deaf: they were swept by the fork.** `j19`
settled ~14:44 and `j56` ~14:57, both while `#t2` was current; the fork at ~14:57–14:58 replaced the
conversation, and **a wake that was queued for a turn that had not run yet does not survive its
transcript being replaced** — the same family as `7fc4cec` (a fill's bar ends when the snapshot replaces
the stream) and R16's echoes, which is why those two were fixed the same way and this was not noticed.
Everything the entry read as *jobs stop, tasks continue* is that fork: **`#t3` receives BOTH kinds, and
has all afternoon** — 3 job notices (15:09:07 `j2`, 15:12:32 and 15:34:36, both `3 jobs … have ended`)
and 4 task notices, the newest 15:35:05.

**And the entry's own prediction is FALSIFIED, which is what closes it.** *"Start a job and restart the
head before the session next goes idle — the notice is lost."* The operator restarted twice in that
window (`yeah I irestartred you`, 15:09:29, and `restarted, lets tet 3 jobs and 3 agents again`,
15:33:46) and **every job notice after each restart arrived**, `j2`'s 29 s later and the 3-job group 5 s
after the second. The restart is not the drop point; the fork is. The one thing that would falsify THAT
is a settlement whose notice is absent while its transcript was never replaced — one job, one query, no
fork in between, **and the `transcript_id` read the second time as well as the first**.

**What is left is narrow and it is still a defect**: a settlement that lands while a turn is queued and
whose transcript is then replaced loses its notice to nobody's decision. Filed below as the wake's own
half of R16 rather than as the stall this entry spent four hours naming wrongly.

---

## Subagents launched in ONE ROUND share a single id — **CLOSED 2026-10-04: `79d1165` mints a unique id at creation**

Three `task` launches in a single round (counting lines of `panes.lisp`, `cards.lisp`, `chrome.lisp`) came
back with the **same** subagent id — `s-1791017230755743833-sub-1791119445423`, three times over. What that
costs, all measured on leticl's head at 15:10:

  · the head held **seven `Subagent` events, every one under that one `:subagent-id`** (states mixed
    `running` / `done` / `failed`), so any fold keyed on the id collapses three children into one row whose
    state is whichever event arrived last — leticl's fold does this, and so does letibot's own
    (`tui/src/app.rs:1846`), so the reference is not better off;
  · the running count drawn on the box's top edge therefore read **no subagents at all while three were
    live** (`composer-title` counts `(subagent-rows head)` filtered to `running`);
  · `task_result` listing this session's subagents prints three identical lines — so they cannot be
    addressed individually there either, and a `[task]` notice naming that id names three children at once.

**The fix belongs at creation**: a per-subagent id (counter or random suffix) rather than something derived
from the round's timestamp. A smarter fold downstream is not the fix — leticl keys on the id because the id
is what the wire offers, and guessing a better key here would paper over the wire.

**And it costs ANSWERS, not just rows.** Measured at 15:12, when the round's notices were released: three
children produced a single `[task]` line — `- `s-1791017230755743833-sub-1791119445423` done: 3529` — so
the model is told that one child finished and receives one answer. **The other two children's answers are
never delivered at all**, to the model or to any head. Per-subagent ids fix the pane and the notices
together, which is one more reason not to patch the fold instead.

**VERIFIED ON THE GLASS 2026-10-04, after `79d1165`** (leticl head `1776144`): a second round of three children
came back with three ids (`…146225190`, `…146494815`, `…146744031`); the head held eleven `Subagent` events
across them rather than one collapsed row; the box's top edge drew **`1 subagent running · 2 jobs running`**
while one child and two jobs were live; and the completion notice named **each child with its own id and its
own answer** (`done: 2633`, `done: 2201`, `done: 2904`), drawn as a card with each answer on its own row.
`2201` is `src/head.lisp`'s line count — the same number a child asked that question returned before the fix
— so the separated answers are the right ones, not merely distinct.

**Closed by `79d1165`**, and the fix is the one this entry asked for, one prefix along: the id is minted
from `crate::config::now_ns()` — the entropy this daemon already names its own sessions with — followed by
`server.rs::mint_session_id`'s counter loop, so the same-shape collision is answered where every other id
in the tree already answers it. **Two checks, because the check this entry first implies is the wrong
one**: a registry lookup alone still lets three children in a round share an id, since the mint returns
while `adopt` is still inside the child's thread opening a whole harness. So the caller also carries what
it has handed out, and the two together are what `three_children_minted_at_one_instant_get_three_ids`
drives at one nanosecond. The timestamp stays in front because it is what stops a post-restart child from
colliding with a persisted one — a bare counter resets and `adopt` would RESUME the old child rather than
spawning a new one. And a duplicate that ever does happen is no longer reported as the child failing: it
names the id, says the mint is what is wrong, and says what it costs.

---

## R18 — every hand-rolled lexer replaced by rano + tree-sitter — **given 2026-09-20**

> lets extend todo with this task - completely replace handrolled code with rano and
> treesitter. I really want us to rely on rano as much as possible, you are free to go
> and patch/extend it too

Rano is the hub: `~/Projects/rano/rano`, a path dependency of `crates/ui` and
`crates/tui`. It owns the tree-sitter engine — 28 languages, one capture walk, the
palette left to the caller. The rule stated in `crates/ui/Cargo.toml:27-34` is that a
**second tree-sitter integration in this process is the thing rano exists to prevent**.
There are three. Surveyed 2026-09-20:

| hand-rolled | where | size | what rano has instead |
|---|---|---|---|
| `StreamingCode` | `crates/ui/src/highlight.rs` | 699 lines, **10 languages** | `Stream` + `captures`, 28 languages |
| its own grammar set | `crates/code/Cargo.toml:20-26` | tree-sitter 0.27 + **6 grammars** | the same tree-sitter, 28 grammars |
| `scan_script` | `crates/tools/src/intent.rs:3390` | a hand-written shell scan | `crates/code::shell::shape` |

Everything below `crates/tui/src/markdown.rs`'s projection is already rano's (the block
pass, the inline pass, the `Node` tree) — `docs/streaming-markdown-plan.md` is the
record. What that file keeps by hand is **not** in scope: `fences_in`, `mask`,
`cut_point`, `stable_boundary_with`, `list_kind`, `unescape`. Each exists because the
grammar gets something wrong (the closing fence is not line-anchored; a loose list
cannot be settled by the tree) and finding it in the text *is* the fix. Deleting those
would reintroduce the bugs of 2026-09-20.

### R18.1 — `StreamingCode` goes, and the conversation gets 28 languages — **DONE 2026-09-20 (`50b005c` + rano `e58600c`)**

The visible win: a code fence is coloured by a 10-language hand-written lexer today, so a
fence tagged `tsx`, `lua`, `ruby`, `diff` and eighteen others renders plain. Rano knows
them all.

Done: `crates/ui/src/highlight.rs` 699 → 118 lines, holding `role_for_capture` and
nothing else; `sidediff` shares that table instead of keeping its own; `CodePaint` is a
rano `Stream` + `Stream::spans`; `BlockCache`'s instrument is `parses` rather than
`bytes_highlighted`. Three rano additions landed with it — `Lang::from_token`,
`Lang::name`, `Stream::spans` — and the walk got 1.5–2.5× cheaper on the way
(`for_each_capture` no longer builds a `Vec<char>` and a char→byte map per line).
Measured: ~470 µs per push at the end of a 5.4 KB Rust fence, a frame's budget. What is
still open is in rano's `TODO.md` §9: the walk is O(text), which is a fence's budget and
not an editor's whole-file repaint.

**The trade, and measure it before deleting anything.** `StreamingCode`'s guarantee is
that *a complete line is highlighted exactly once, ever* — rano cannot promise that, and
rano measured why: markdown's push re-lexes the whole document (~106 ns/byte) and rust's
reuses ~97% (~3.5 ns/byte). At the last figure a 10 KB fence costs ~35 µs per push, which
is a frame's worth of nothing; at markdown's it would not be. So the number to take is
rano's `bytes_reparsed`-equivalent for a rust/go/ts fence grown one token at a time, and
the decision follows it. If it is not flat enough, the fence gets the same window
discipline the conversation has.

**Where.** `CodePaint` (`crates/tui/src/render.rs:176`) holds the `StreamingCode` and the
byte offset it has been fed; `render_block_with`'s `Block::Code` arm draws what it
returns. It becomes a rano `Stream` plus a capture walk over the fence's text. Delete
`highlight.rs`'s lexer (`Syntax`, `State`, `StreamingCode`, the 10-language table) once
nothing calls it, and its `bytes_highlighted` instrument with it — the number it exists
for has a replacement in rano.

### R18.2 — `crates/code` stops carrying its own tree-sitter — **DONE 2026-09-20**

`crates/code` was a second integration: its own `tree-sitter = "0.27"` and six grammars
(rust, python, go, c, bash, json), used for `outline` (`crates/tools`) and the shell shape
(`crates/harnessd`'s etalon map). All six are rano's already.

Done: `Cargo.toml` drops the seven dependencies for one `rano` path dep; `outline` and
`shell::normalise` drive `Stream` and walk rano's `Node`; `examples/dump.rs` — the tool
that prints a grammar's node skeleton, and how every kind string in `classify` was chosen
— reads the same `Node`, so what it prints is what the outline sees.

**What it is not**: a merge of the two crates. `crates/code` exists because the grammars
are C and pull a `cc` build, and `crates/tools` deliberately has no `build.rs` — that
reason stands, and this crate is still where it lives. What went was the second engine
underneath it.

**Rano grew four things to make it possible**, all of them "the hub's node should carry
what its consumers need": `Node::field` (which field a child sits in — 17
`child_by_field_name` call sites here), `Node::start_point`/`end_point` as rano's own
`Point`, `Node::id` (two nodes can share a byte range, so identity is not a coordinate),
and rano's `Point` no longer being tree-sitter's in its public API.

**One bug this found, in this crate and not in the port.** `shell.rs`'s `named_children`
was misnamed — it collected *every* child, anonymous tokens included. A reader taking the
name at its word filtered on `Node::named` and lost the `&&`, `|` and `;` the module
decides *by*: `test -f x && rm x` came back `Certainty::Always` for `rm` instead of
`Conditional`. Renamed `kids` with the misnomer written down, because an operator token is
exactly the kind of child a grammar leaves anonymous.

### R18.3 — the intent scanner's shell scan — **CLOSED 2026-09-20: it stays, and not as a duplicate**

`crates/tools/src/intent.rs::scan_script` is read at the call sites rather than guessed at,
and it does not read shell at all: `ScriptLang::Shell` bodies already go through
`shell::normalise` and the grammar. This handles `ScriptLang::Other` — a **script in
another language**, named by its interpreter (`python`, `node`, `perl`, `ruby`, `php`),
which a bash grammar would read wrongly and no single rano grammar covers.

Its question is also a different one: not what the shell will *do*, but which
**capabilities** the text names — a network import, a shell-out, a host, a secret path —
deliberately **body-wide rather than adjacency-based**, because `host = "192.0.2.10"` three
lines above `urlopen(f"http://{host}/")` is a real shape. A parse does not change that.

What a parse *would* improve is precision (`import urllib.request` names its module in a
node, where this matches a name list against a whole line) — noted in the function's own
doc rather than done, because it changes what a gate decides and that is not a refactor.
So: no grammar here, and the reason is recorded where the next reader asks.

### Rano patches this needed — **all three landed** (`e58600c`)

- **`Lang::from_token(&str)`** — done, with its own alias table rather than `detect`'s
  (`mk` is Make as an extension and not a token; `sh` is bash as a token and `/bin/sh` as
  a path).
- **A route from `Lang` to its highlight query** — done as `Stream::spans`, which is
  better than making `query()` public: the embedder hands over *nothing* and gets capture
  names back, so no query text crosses the boundary at all. `classes()` was written first
  and removed — see rano's commit.
- **`Stream` over a growing text** — it already existed; the fence feeds it the delta.

**Still open?** Nothing — all three subtasks are closed. What *was* the check, for the
record, is now the answer:

    grep -c 'name: "' crates/ui/src/highlight.rs        # 0  (was 10)
    grep -c '^tree-sitter-' crates/code/Cargo.toml      # 0  (was 6)
    ls crates/tools/build.rs                             # absent, as it must stay

**Done when.** A fence tagged `tsx` (or `lua`, `ruby`, `diff`) renders coloured in a live
answer — the rendering is pinned by tests in `crates/tui/src/render.rs`, and the one thing
a test cannot assert is a live answer; `crates/ui/src/highlight.rs` holds no lexer;
`crates/code` declares no `tree-sitter-*` of its own and `crates/tools` still has no
`build.rs`; the conversation's per-push cost is measured against the old one and the number
is in rano's `TODO.md` §9; 2026-09-20's markdown fixtures pass, since they are the record of
what the grammar gets wrong.

### R18.4 — attaching to a big session takes seconds, and it is the markdown parse

**Reported 2026-09-20**, after the migration shipped: *"startup time skyrocketed.
literally seconds"*, and then *"I do `leticode --continue` and it just does nothing, then
chrome appears with empty conversation history and then after a while it renders
history"*.

**Measured**, release, on this workspace's own stored transcripts:

| | |
|---|---|
| markdown block grammar | 133 ns/byte (555 KB → 74 ms) |
| markdown inline grammar | 250 ns/byte (450 KB → 133 ms) |
| Rust, for scale | ~3 ns/byte |
| a stored session | **4.1 MB and 5.9 MB** (`s-…838#t18`, `s-…813#t11`) |
| so one attach | **1.6-2.7 s** of lexing, which is the "after a while" |

Both markdown grammars run external scanners and that is the constant; tree-sitter is
not the problem. A row is lexed once (the walk is monotonic — see
`rendering_the_history_does_not_grow_with_the_session_either`), so this is one linear
pass, not a repeat.

**Fixed (`5884ddf`, rano)**: the query cache. `Query::new` is 8.7-10.2 ms and was paid
per *code fence* and per *diff excerpt*; it is compiled once per process per language
now. This session alone holds 132 fenced messages, so that was ~1.2 s of pure query
compilation per attach — our bug, and it is gone.

**Chrome first (done 2026-09-20).** The head used to draw its first frame *after*
the whole attach — and `HeadClient::attach` blocks on the daemon's `Hello`, which
**carries the snapshot**. So on a big session the operator's previous screen stayed
up for the entire round trip and then the transcript appeared at once, which is
exactly the reported *"does nothing, then chrome appears with empty conversation
history and then after a while it renders history"*.

Measured on this daemon: 0.01 s of process start, 0.17 s of attach (the `Hello`,
snapshot included), 0.08 s of first frame. Only the first of those was ever visible
as *letibot*.

Now the terminal is taken over and a frame is drawn **before** the attach, so the
composer and the hint bar are on screen in the time the process takes to start, and
the history fills in when the daemon answers. The frame drawn in the meantime must
not lie: the empty-transcript banner says *"this session has said nothing yet"*,
which is false when the truth is *"nobody has told this head yet"*, so `App` carries
an `attaching` flag that suppresses it — `the_frame_before_the_attach_claims_nothing_about_the_session`.

`Terminal`'s `Drop` restores the termios and leaves the alternate screen, and it does
so before printing its own reports, so entering the screen before a refused attach
still lands the error on a restored terminal.

**The design, and it is the operator's rather than mine.** Stated twice, and I said
yes to it both times and then planned as though I had not:

> so in a way we are looking back right, instead of looking in the future, interesting
> challenge for treesitter  — 2026-09-20, on the tail problem

> render chrome asap, then render the very current frame of text, then some scroll up
> buffer if needed — mind the bounds of blocks, etc  — earlier the same day

So: **stream backwards.** The document is finished and the head wants its *end* — the
current frame, then scrollback above it, fetched as it is looked at. That is a viewport
over a buffer the daemon already owns, which is what an editor has, and it is *not* the
three-patch list a first pass at this item produced (lex less, cache the render, fill
over frames). Those were written after `tail_cut` was built from the operator's own
insight and then not wired up — the mechanism for this design already exists in
`crates/tui/src/markdown.rs` and every one of those three would have worked around it.

**Stage 1 — the window is a window (no protocol change).**
`tail_cut(src, min_bytes)` returns the offset near the end from which the suffix parses
standalone. Committed and tested (`815200e`, `ad0a10a`): `lex(tail)[1..]` is the
document's blocks, the first may be ragged and goes off the top, and a 4 KB tail of
152 KB measures **3.0%** of the whole's cost. What is missing is only the caller: the
walk in `body_window` still lexes each row whole.

**Stage 2 — a row is a logical string, and the head holds a viewport.**
The operator's own framing, and it is the right one: a tool result of 418 KB is a
*logical* string that wraps to thousands of *display* lines, of which 40 are on screen.
Wrapping exists; paging does not, so an unfolded row is unreachable past the fold. The
fix is the editor's, not a cap: the daemon owns the buffer, the head holds the window,
and a row fetched on demand when it is unfolded. `Peek` already fetches a stored
conversation for the session picker, so the mechanism is built — it needs a row-shaped
form rather than a new protocol.

**Stage 3 — bound the snapshot in bytes (`ViewBounds`).**
`items: 2_000` bounds *count*, and the operator has sessions of **160 MB and thousands
of turns** where a row may be 418 KB — so the snapshot can be most of a gigabyte. This
is the first half of the viewport rather than an optimisation: without it the window has
nothing to be a window *over*. Do it first only in the sense that it is small and needs
no protocol change; on its own it just drops old rows and still ships the fat one.

**Not a fix**: making the grammars cheaper. Both external scanners are upstream, and the
markdown half costing ~20x the Rust half per byte is a fact about those scanners rather
than about rano. Nor is capping a row's payload at the source: that cuts the logical
string to fix a display problem, which the operator named as the editor mistake.

**Not a fix**: making the grammars cheaper. Both external scanners are upstream, and the
markdown half costing ~20x the Rust half per byte is a fact about those scanners rather
than about rano.

**The one piece left, and it is rano's.** The capture walk is O(text) per repaint. A code
fence is a frame's budget at fence sizes — measured, and why this shipped — but an editor
repainting a 200 KB file per keystroke is not, and that is exactly the consumer rano's
README says it is for. rano's `TODO.md` §9 has it as open with the shape of the fix
(a range-limited `QueryCursor` widened back to any overlapping node, which is where the
correctness lives). It is not filed here as well, because it is one item and it belongs to
the engine.

---

## R21 — the jobs pane cannot show a job's output, and the reply path is why — **DONE 2026-09-20 (`3aabe4f`)**

The operator, 2026-09-20: *"when i press enter on jobs pane im not shown the job output im brought
back to the main conversation with /job <id> posted - this is not what i want - when i press enter
i want to see job output"*. Narrowed by them a moment later: *"entering the running job works fine
- but finished does /job <id>"*.

**One path serves both, and nothing about running versus finished differs in the code** — so the
"works fine" on a running job is the reply being one *sentence* (`j12 is still running and has
written nothing yet`), which reads acceptably as a line in the chat, while a finished job's reply
is a 16 KB dump that does not. The pane closes in both cases, deliberately.

### What the reply is, and why the pane has to close

`Action::Slash { line: "job j12" }` → `ClientFrame::Slash` → `CommandKind::Slash` (the command
queue) → `Sessions::slash_parsed` → `Harness::job_output(job, offset, 16 KiB)`, and the answer is
published as

    SessionEvent::Warning { code: "slash", detail: "…/job j12\n<the output>" }

— `crates/harnessd/src/sessions.rs:1546`. So a slash reply is a **warning on the session log**,
which is the design (*"a frame per verb would have every head learn every verb"*), and the head
closes the pane so the operator can read it there.

### The two shortcuts that do not work, both checked

1. **Match the warning in the head and draw it in the pane.** The head does know it asked for
   `/job j12`, and the reply's first line is that exact string — but the event is still published
   to the session, so the 16 KB would appear *both* in the pane and in the conversation. Hiding it
   in this head would make the head's screen disagree with the log every other head sees.
2. **`ClientFrame::ReadJobOutput` answered on the asking connection.** This is the shape
   `Peek`/`Settings`/`Jobs` use, and it is what a pane wants — but those all read something the
   **server** can reach (the hub's view, a registry mailbox), and job output lives in the **exec
   host**, which is the daemon worker's. The server would have to register a reply channel, queue a
   command, and block its own read loop on the answer — which stalls that connection's live events
   for the duration. The `secrets` map is that pattern and it is the only one: a reply channel the
   worker fills.

### The design that fits, and it needs no protocol change at all

**Publish the window as its own `SessionEvent`**, and let the pane be a view of it:

    SessionEvent::JobOutput { job, from, to, produced, dropped, state, lines, next }

- It is a **slash reply**, so the log is exactly where it belongs by the existing rule — no lie,
  no suppression, every head sees it.
- It carries the **offsets beside the text**, which the `Warning` prose cannot: today
  `Harness::job_output` builds a footer (`[exited 0 — bytes 0..16384 of 40000 produced]`, `more:
  /job j12 --offset 16384`) that a pane would have to parse. The window type and that method's
  structured twin were written and are small.
- The head needs **no new frame and no reply channel**: `CommandKind` + the fan-out already carry
  it, and the head's `ServerEvent` arm fills a `job_out` pane that mirrors `sub_out`.
- Paging is the same event again: the answer carries `next`, the offset to ask at when the ring
  still holds more, and the head asks a `ReadJobOutput` at it. The head keeps a stack of the
  offsets it was given rather than the page size, because the page size is the daemon's.
- **No `PROTOCOL_VERSION` bump**: `SessionEvent` is an internally-tagged enum and this is an
  additive variant, the same way `TranscriptContent` and `JobSettled` were added.

**Where.** `crates/sessionlog/src/event.rs` (the variant), `crates/sessionlog/src/hub.rs`
(`CommandKind::ReadJobOutput`), `crates/harnessd/src/harness.rs` (`job_output_window`, structured),
`crates/harnessd/src/sessions.rs` (the dispatch arm publishing it), `crates/tui/src/app.rs`
(`job_out`, the Enter arm, the render, the event arm), and tests on both sides.

**Done when.** Enter on a job row — running or finished — draws its output in the pane, esc returns
to the list, `↓` pages when there is more and says so when there is not, and the conversation gains
nothing the operator did not ask to put there.

**Done.** Implemented in `3aabe4f`, and one thing is different from the design above: paging is
**not** a slash reply. Once the window is a `ReadJobOutput` there is no reason to route "the next
page" back through the verb, so the event carries `next` and the head asks a second
`ReadJobOutput` at it — the head keeps the offsets (`←` walks back the way `→` came) rather than
recomputing a window it does not size. One thing was also **added** that the design did not name:
the window is ephemeral, so `scrub::is_interactive` returns true and `StoredProjection::keep`
strips it and counts it — a window from four minutes ago is a lie about now.

---

## R12 — the firecode backend for subagents, and the cookbook — **SETTLED 2026-09-14 (claude-lab2x1)**

Done: `crates/tools/src/firecode.rs` + the harness placement; live test
`crates/tools/tests/firecode_live.rs` (up 6.9 s, read/write/list/stat/run/job,
down landing the guest's writes); an end-to-end run where a leticode session
spawned `task(role: researcher, access: read-only, where: firecode)` and the child
listed the workspace inside the VM and had no shell; `docs/cookbook.md`, filed as
skill `01M2FRW9D3A8K8V9660BJQX5PZ`. Left open, in `docs/subagents.md` §4:
checkpoints, `--cwd` made after boot, the child's own voice.

**Given by the operator 2026-09-14**, leaving for the day: *"1. firecode + cookbook"*.
Design and measured numbers in `docs/subagents.md` §3. The `task` tool's `where:
firecode` is a declared seam that refuses by name (`4f3d1d7`); this fills it.

**Where.** `crates/tools/src/firecode.rs` (`FirecodeBackend: ExecBackend`, a
`FirecodeConfinement` wrapping every job as `firecode in`), and `harness.rs`
placement: `Placement::Firecode` opens that backend for the child, `backend_confined
= true`, mode allow-all inside. The child works on a **copy** of the parent's
workspace under `~/.cache/letibot/firecode/` (firecode's shared-tree guard refuses a
main checkout with worktrees and uncommitted tracked changes — exactly when a parent
spawns; and `/tmp` scratch evaporates). Cold boots only: checkpoint/restore is not
dependable on the host yet (claude-host-lab's note `01M2FR5A1VAJJ13M7S2XRRZK33`). The
operator's direction for startup: **hierarchical image caches**, so a boot is
milliseconds — firecode's side of the seam; recorded in the cookbook, not built here.

**Done when.** A `task(where: "firecode")` from a leticode session boots a VM, runs
its tools inside it (read/write/list/run over vsock), ends with `down`, and returns
its answer naming the sibling directory where its writes landed; a live test behind
`FIRECODE_LIVE=1` holds it; `docs/cookbook.md` exists, is filed on the fabric as
`kind=skill`, and covers firecode, flowy, subagents.

---

## R17 — Brave behind `web_search` — **SETTLED 2026-09-14 (claude-lab2x1), not yet run live**

*"I bought brave search api key, I want web search tool"* (2026-09-14). The seam
was already there: `web_search` has shipped as a refusing tool since the external
module was written, precisely so the schema could be fixed before anything cached
against it. This fills it.

- New crate `letibot-websearch` — `Brave` implements
  `letibot_tools::…::web::SearchProvider` over ureq+rustls. Its own crate because
  `letibot-tools` is deliberately *a function of a call and a filesystem*; the
  trait is the seam that keeps TLS out of it.
- Key: `--brave-key` → `$BRAVE_API_KEY` / `$BRAVE_SEARCH_API_KEY` → `[brave] key`
  in `~/.config/letibot/providers.toml`. **Refuses at attach**, naming all three.
  The key is held in a `OnceLock` in the websearch crate, never in `Config`,
  which derives `Debug`.
- Seated **only when attached** (`role_for_seat`, like `flowy`'s door), so a
  session without `--web-search` is byte-identical to yesterday's and nothing
  re-prefills. `max_tools` rises by one when it is.
- Only `web.results` is read; `<strong>` stripped; a URL-less hit dropped;
  `considered` carries the denominator; Brave's `query.altered` is surfaced as
  `rewritten_query` because a silently rewritten query is a rewritten query.
- Tests: 3 unit (key order and refusal text, the `[brave]` section read, markup),
  4 against a stand-in Brave (the key rides in `X-Subscription-Token`, `site:`
  goes out as Brave's operator, 401/429/transport each say which they are, an
  empty result is empty rather than invented).

**Verified live 2026-09-15.** The operator's key landed and
`crates/websearch/tests/brave_live.rs` (BRAVE_LIVE=1) ran a real query:
`Brave Search, key from ~/.config/letibot/providers.toml`, 3 hits of 3
considered, titles/URLs/snippets all parsed, no `<strong>` reaching the model.
The field names written from the documented shape are the ones Brave sends, so
the remaining unknown named here is closed.

Two notes from doing it. The key was pasted onto the commented placeholder line
and stayed a comment, which reads as "no key" — correctly, and the refusal named
all three places, but the operator's belief was that it was set; the placeholder
being *inside* the section is what made that easy. And the end-to-end through a
local-model session could not be run: `ggml-cuda.cu:108` on model load, the
standing CUDA fragility, not anything to do with search. `--provider deepseek
--web-search brave` is the path that avoids the local GPU.

---

## R16 — the preapproved list, Always allow, `ps`, a shell by default, and sudo-to-the-head — **SETTLED 2026-09-14 (claude-lab2x1)**

The operator, 2026-09-14: *"we badly need a list of preapproved globs, like all
read only git and gh commands, cargo, go, and other tests. and good old Allow
Always from opencode and Claude Code"*; *"when i do leticode --bash must be a
default"*; and, from the two transcript scans on this box (`PS_USE.md`: 294
`ps` pipelines in 22 days; `SUDO_USE.md`: 40 `sudo` attempts, 0 succeeded):
*"we need tools/skills for ps - i dont want models to reinvent the same commands
and i want a solution for sudo"*.

Done:

- **The preapproved list** — `permission::DEFAULT_ALLOW`, 142 prefix rules for
  `bash`: read-only git and gh, cargo/go/npm/pytest build-and-test verbs, the
  shell's read-only utilities. Deliberately absent: `find` (`-delete`), `awk`
  (`system()`), `sed` without `-n`, `env`, `gh api`, `cargo run`, `git branch
  -d`. A compound command is tested **one simple command at a time** — every
  segment must match — and a substitution, a redirection to a file, a group, a
  here-doc or a leading assignment is never matched (`bash_segments`). That is
  a parser for the purpose of NOT admitting, the one job `docs/tool-survey.md`
  allows a shell parser in a gate. Precedence: shipped < `~/.config/letibot/
  permission.json` < `$LETIBOT_PERMISSION`, last match wins; a `deny` row
  outranks everything. The `preapproved` disclosure counts them.
- **Always allow** — offered on every prompt whose tier is not always-ask, exec
  included (the 2026-09-11 rule *"exec asks every time"* is revised: a session
  GRANT is still never offered for exec; a durable RULE, in a file the operator
  reads and edits, is the operator's own preapproval). For `bash` the rule is
  the program and its verb (`cargo run --bin x` → `cargo run*`); for a file
  tool, the path. Written to `permission.json` through a sink; the row says
  when it could not be.
- **`ps`** — read-only; `pattern` / `pid` / `children_of` / `top cpu|mem`;
  pid, ppid, age, state, CPU%, RSS, command line; never lists this process;
  `PROTECTED` and `job` marks as `pkill`. Seated with the shell.
- **`leticode` seats the shell by default** (`--no-bash` to refuse it), the
  way opencode's coder has bash. What it runs unasked is the list above.

**Sudo — SETTLED 2026-09-14, shape 1.** The operator's ask: *"if model wants a
sudo i must be able to enter password safely and let it run … some other shim …
i honestly dont care now"*. Built as `SUDO_ASKPASS` routed to the head:

- Every command of a host session runs with `SUDO_ASKPASS` pointing at
  `letibot-askpass` (a fourth binary in the harnessd crate) and a `sudo` **shim**
  ahead of `PATH` (`crate::sudo`, written once to `$XDG_RUNTIME_DIR/letibot/
  shims`) that execs the real sudo with `-A` — except for `sudo -n`, left alone,
  so a probe still answers *no* honestly. The daemon also puts `LETIBOT_SOCKET`,
  `LETIBOT_SESSION` and `LETIBOT_COMMAND` in every command's env (a new
  `HostProcesses::set_standing_env` / `set_path_prefix`).
- The command itself passed the gate first — `sudo …` is privilege escalation,
  always-ask — so the operator saw and admitted it before any password.
- The helper attaches as an `askpass` head and sends one `Askpass` frame
  (PROTOCOL_VERSION 12). The daemon raises `SecretRequested` to every head; the
  TUI shows a card naming the command and sudo's prompt with a **masked field
  that owns the keyboard** (dots on screen, never the composer, never its
  history); the head answers with a `Secret` frame; the password goes head →
  daemon → helper → sudo's stdin and **nowhere else** — no log, no view, no
  transcript, no `CommandIssued`. The log gets `SecretSettled { given, by }`,
  the record without the secret. Deadline two minutes; Esc refuses; a late or
  duplicate answer is a `secret_late` warning.
- Tests: `letibot-sessionlog`'s `askpass` socket test (the secret reaches only
  the helper; refusal and late answer are honest; the hub state and log never
  hold it); the TUI `password_field` test (dots, keyboard ownership, no history);
  `letibot-tools`… the `sudo::install` shim test (adds `-A`, leaves `-n`); and
  the live `sudo_live` (SUDO_LIVE=1) that runs the box's real sudo through the
  shim and gets its wrong-password refusal, not a tty error.

Shape 2 (`Defaults timestamp_type=global`) and shape 3 (a detached privileged
shell) were the alternatives; shape 1 is the one that keeps the secret off the
log and scopes to the session. Not yet done: the remote/ACP heads have no
password card (only the TUI does), so a session driven only by one of those
still cannot answer a sudo — the frames are there, the UI is not.

The behaviour when no head answers is the one the scan shows Claude Code
already has: probe `sudo -n true`, announce the refusal, write the root half as
a script for the operator, and stop — never `script -q` or `echo '' | sudo -S`.

---

## R15 — a leticode session started at `/`, and searched it — **SETTLED 2026-09-14 (claude-lab2x1)**

Measured twice, driving local GLM as a one-shot coder on this repository: `read
crates/flowy/src/context.rs` → `no file`, because the whole-host backend joined the
relative path to its root and the root is `/`. The model then did the reasonable
thing — `glob` and `grep` from `/home` down for the file — and the daemon read a
27 GB model shard into memory and spent twelve minutes in the kernel with no head
able to reach it. Four fixes, all with tests:

- `HostBackend` has a `cwd`; relative paths and a command's `cwd` start there
  (`with_cwd`, `ExecBackend::workdir`); the harness sets it to the workspace for
  the `/`-rooted backend; the disclosure says `relative paths start at DIR`.
- `grep` opens no file over 16 MiB, stops a rung after 512 MiB read, and the
  result counts what it did not open. `read` refuses a file over 256 MiB by size
  before opening it. A walk never enters `/proc`, `/sys`, `/dev`, `/run`.
- The VM copy leaves out `.git/worktrees`: firecode's shared-tree guard counted
  the source's 28 and refused the copy.
- A NEW session's open-time notes reach the banner and the head (`open_note`):
  the project store choosing `writes allowed` over `--mode allow-all` used to go
  into a report only a resumed session printed.

After the fix the same GLM one-shot read the file, made the edit, added the two
tests (5/5 pass) and, having no shell, said it could not run them rather than
inventing a result line. DeepSeek, live, did the same on `grep.rs` for ≈ $0.02
(`docs/providers.md` "Verified").

**Then, the same afternoon: both models in a VM each.** `--where firecode` /
`letibot --vm` place a ROOT session in a VM (the placement a subagent already
had), `--vm-arg` hands `firecode up` its options, one-shots get their own
socket. DeepSeek and local GLM each wrote the `read` ceiling test and ran
`cargo test` inside — 10 min / $0.075 and 35 min respectively — after both
discovered the guest lacks llama.cpp and the sqlite dev symlink and stubbed them
outside the tree. `docs/cookbook.md` §4a. Then, the operator's rule — *"allow-all should be the
true allow-all"* — `Mode` gained a boundary axis: `allow-all` is `Structural`
and admits the always-ask list (`sudo -n id -u` in a VM: `0`, no ask); the
flow rule (a secret across the boundary) still refuses. Layers follow the copy
(`firecode layer inherit`, in the firecode tree, uncommitted). **Open:** the
layer itself for this project (toolchain, llama.cpp fork, `libsqlite3-dev`) —
needs docker, which lab2x1 lacks.

**Open, small.** `Mode::ALLOW_ALL` on a bare host is refused by its confinement
prerequisite and the project store overrides `--mode` — both by design — but a
one-shot with no adjudicator then has a `bash` that fails closed on every call.
A one-shot wanting a shell needs `--bash` and an adjudicator, or a VM. Whether
`--mode` on the command line should beat the store row is the operator's call.

---

## R14 — inject the fabric's skills and memories into the session — **SETTLED 2026-09-14 (claude-lab2x1)**

Done: `letibot_flowy::context` + `Sessions::with_fabric` / `refresh_fabric`, the
`fabric` disclosure, live test (8 skills, 42 memories; cache served with the node
away). Not run through a `--flowy` daemon end to end on this box: the seat's reader
is held by another session's listener and `Seat::open` refuses, as it should.
`docs/flowy-monitor.md` §5c.

*"3. I want flowy skills, memories etc to be injected. how? skills are summaries of
full pages, memories are titles."* At session open (and after a compaction), when
the daemon holds a seat: the shelf's skills as one line each (title + first
paragraph as the summary), memories as titles, into a `fabric` block of the system
prompt, so the model knows what exists and loads a body through `skill` /
`flowy get` when it needs it. **Offline mode** (`docs/tool-design-brief.md` §3b,
`docs/closed-loop.md` §5): a node that is away is a declared state, the block is
served from the last cached copy on disk and says its age; no seat, no block, said
so in the disclosure.

**Done when.** A session's system prompt carries the block; a compaction refreshes
it; a stalled seat serves the cached block labelled with its age; a session with no
seat has no block and the disclosure says `fabric: OFF`.

---

## R7 — A turn that ends inside its own reasoning is reported as success — **SETTLED 2026-09-13, see TODO-settled.md (R7) — a6b970e**

---

## R8 — `edit`'s near-miss recovery is defeated by exactly the error this model makes — **SETTLED 2026-09-12, see TODO-settled.md (R8) — ad23f39**

**Measured 2026-09-11**, in the live letibot session, twice in one turn.

The model sent `old_string` beginning `                if!text.is_empty() {`. The file
holds `                if !text.is_empty() {` at line 2543 — **one missing space** after
`if`. That is a reproduction artefact of this model (` !` versus `!`), not a typo, and
it is the same family as R7: GLM emitting something that is not quite the bytes it read.

The edit failed, which is correct. What is wrong is that **both** recovery paths in
`builtins/edit.rs::no_match` are defeated by that single space:

- `probe()` normalises whitespace, indentation and case — but normalising cannot
  **re-insert a deleted** character, so it finds nothing.
- `anchor_lines()` anchors on *"the longest whitespace-delimited token"* of the first
  line. Deleting the space **merges two tokens into one** — `if!text.is_empty()` —
  producing a token that occurs nowhere in the file.

So the tool honestly reports *"nothing in `…` matches that text, and no part of your
first line occurs anywhere in it either"* and then prints the file's **first 20 lines**,
which for `crates/tui/src/app.rs` is its module doc comment: pure waste, and no closer
to the answer. Two rounds burned, and the operator watching.

`text.is_empty() {` **does** occur, at 2543. One shorter anchor would have landed on it
and the model would have seen its own missing space.

**Still open?** `grep -n -A20 'fn no_match' crates/tools/src/builtins/edit.rs` — the
`_ =>` arm that dumps `file.lf.lines().take(20)`.

**Where.** `anchor_lines`, in the same file. When the longest token does not occur, fall
back to progressively shorter ones — next-longest, then any token over a few characters —
before giving up. Print the candidate lines, not the top of the file.

**Done when.** An `old_string` differing from a real line by one inserted or deleted
whitespace character comes back with that line and its number, and a test in this file
asserts it on the `if !text` case verbatim. §2.1's rule is the bar: the miss is
self-correcting **in the same call**.

---

## R9 — a refusal says `host_other` about a path inside the workspace — **SETTLED 2026-09-12, see TODO-settled.md (R9) — 545d9d7**

**Seen 2026-09-10** in the live session, on an `edit` of `crates/tui/src/app.rs`:

    reading: ask — intents [write_file] over [host_other]

The path is plainly inside the workspace, and the classifier that **decides** agrees:
`GateCall::path_is_inside` returns true for a relative path with no `..`, so
`ActionClass::host(access, inside, creates)` yields `EffectScope::HostProject`. The
grant key is right.

What is wrong is the **reporting**. The `host_other` in that line comes from
`crate::intent`'s `Region`, a different classifier written for shell-argument analysis,
whose `region_of` falls back to `Region::HostOther` for anything it cannot place. It
reaches the operator through the baseline string.

**Why it is worth fixing rather than tolerating.** It cost real time: the wrong scope in
that message was the first hypothesis for why `allow_session` did not stick, and it was
wrong — the cause was the mode's grant scope. A misleading fact in a refusal is worse
than no fact, because a refusal is what somebody reads when they are already confused.

**Still open?** `grep -rn 'region_of' crates/tools/src/intent.rs` and read the
`Region::HostOther` fallback at the end.

**Where.** Either give `region_of` the workspace so a file path resolves the way
`path_is_inside` does, or keep the baseline silent about region for a call whose class
was decided from a path. The second is smaller and loses nothing.

**Done when.** A gated `edit` inside the workspace reports a project-scoped region, or
none, and never `host_other`. A test on the rendered refusal payload, not on the class.

---

## R10 — layer 1's two env-hygiene lines, which `bash` waits on — **SETTLED 2026-09-12, see TODO-settled.md (R10) — 61afc57**

`docs/boundary-and-adjudication.md` §5 states these as **requirements rather than
assumptions**, *"because a requirement crosses a merge where an assumption does not"* —
and they are what stands between the exec substrate and
`Surroundings::with_pinned_shell` being honestly callable:

1. **`env_clear()` before the explicit `env` pairs**, so `PATH` is *pinned* rather than
   inherited.
2. **Unset `BASH_ENV`, `ENV`, `SHELLOPTS`, `BASHOPTS`.** `BASH_ENV` **is** sourced by bash
   for non-interactive shells, so a distribution where `/bin/sh` is bash has a real
   injection point this box does not — which is exactly the kind of thing that is true
   here and false one machine over.

**Why this is the prerequisite and not caution.** *"A resolved parse is not a resolved
meaning."* A grammar reads text; a shell resolves a bare command name through aliases,
functions and `PATH`, none of which are **in** the text — which is how the survey's best
parser is defeated. So `intent::ShellTrust` defaults to `Unknown`: a bare name is
unresolved and the command is `not_run`, while an absolute path is not shadowable and
passes. Layer A can only mean something if layer 1 pins what the words resolve to.

The gap is small because `exec/host.rs` already spawns `/bin/sh -c`, non-interactive and
non-login, so no rc file is read, and `/bin/sh` here is `dash`, which reads `$ENV` only
when interactive.

**Still open?** `grep -rn 'env_clear\|BASH_ENV' crates/tools/src/exec/host.rs`

**Where.** The spawn path in `crates/tools/src/exec/host.rs`. `Prereq::Confinement`
already exists as the seam to assert it against.

**Done when.** The spawn clears the environment before setting its own pairs, the four
variables are unset, a test asserts a `BASH_ENV` planted in the parent does not reach the
child, and `Surroundings::with_pinned_shell` is called where it was previously only
described. Note what this does **not** do: it does not seat `bash` — see N4 and the
transcript-edge choke point, which is the other half.

---

## R11 — the audit rows are written and never read — **SETTLED 2026-09-13, see TODO-settled.md (R11) — 74abade**

---

## N6 — a `harnessd` quit while a subagent is mid-turn dies of SIGSEGV at exit — **SETTLED 2026-10-10, see TODO-settled.md (N6) — 3b7db2b**

---

## T11 — §18.1-I1's observable form is not checkable on a hybrid model — **SETTLED 2026-09-11, plan text corrected, see TODO-settled.md (R5)**

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

## T4 — Verify what opencode actually sends — **SETTLED 2026-09-11, mechanism 2 refuted, see TODO-settled.md (R6)**

`docs/chat-templates.md` §3 lists two ways a client causes prompt-cache divergence.
The second — sending reasoning as its own message rather than fused onto the
assistant message — applies only if opencode does that. Unverified.

The W1 recording proxy answers it directly. Decides whether the residual
divergence after the `interleaved` fix is one mechanism or two.

---

## R57 — a child's completion is not a firing, so the wake never happens — **measured from leticl 2026-10-03; explained below, and the last hop is STILL UNTESTED** — **SETTLED 2026-11-02, audited: every item ✓ fixed and tested**

The operator restarted the session that spawns `task` children so that this could be tested rather
than assumed, and the measurement is:

  · the child ran and answered — two facts off a fixture, no decisions in it;
  · **the head's agent counter flashed 1 → 0**, so the child's `opening`/`running`/`done` events DID
    reach a head and were folded. That half works;
  · **no notice row appeared** — not in the head, not in the model's own conversation;
  · `job_list` in the session that spawned it: **0 monitors, 0 fired, 0 jobs**. Nothing was armed, so
    nothing could fire;
  · the model learned the child had finished **only by polling** (`task_result`) — the sentence in the
    daemon's own banner: *"a fired monitor reaches the model only when something calls job_list. That
    is a poll, not a wake."*

The mechanism is not missing. `completion_notice` (`harness.rs:664`) publishes *"[job] a job you
backgrounded has ended"*, those notices arrive unprompted, and one arrived in leticl's transcript
today. Its signature is `fn completion_notice(done: &[JobCompletion])` — **jobs only, no children.**
D26 says the wake exists (*"T24's wake turns a firing into a user item and runs a turn"*), so the gap
is narrower than *no wake*: **a child's finish is not a firing.**

**done when** a `task` child's completion reaches the model unprompted — the same
`User { speaker: Agent }` row a job's ending produces, in a session that armed no monitor of its own.
— ✓ fixed in `fd4aaad4`, test `a_childs_answer_wakes_its_parent_with_nobody_asking`

**What letibot found reading the tree, and it changes what to do about the measurement.** The chain is
**closed on HEAD and was open in the build that was measured**:

  · **The daemon under test was a binary from the day before, and that is the whole explanation.**
    Measured on this box 2026-10-03: `target/release/harnessd` is dated **2026-10-02 20:17:36**
    (34724024 bytes), and `fd4aaad` (*"a subagent settles the same way a backgrounded job does"*) was
    committed **2026-10-03 21:16:24** — twenty-five hours later. The daemon serving leticl's workspace
    (pid 3840826) had been restarted minutes before the check and was running **that same file**, so a
    fresh PROCESS was running a stale BINARY: **a session restart does not rebuild a daemon.** The dated
    build cannot have contained the child path at all, and `0 monitors, 0 fired, 0 jobs` is exactly what
    it should do. leticl's reading of `completion_notice` as *"jobs only, no children"* is a reading of
    that build rather than of the tree.
    *(The pin is NOT the reason, and it was checked the wrong way first: leticl runs LOCALLY, so
    `LETIBOT_REF` describes its release packaging and says nothing about the daemon on this box.)*
  · On HEAD there is **one queue and one bell**, not two mechanisms: `JobWatchers::with_tasks` gives
    the watcher the session's `TaskRunner`, `watch_task` (`jobwatch.rs:432`) queues through
    `settled_here` — *"the push and the bell"* — and `Harness::wake` (`harness.rs:4442`) takes both
    kinds off `take_completions()` and partitions them **for wording only** (`job_output` for a job,
    `task_result` for a child), submitting one `User { speaker: Speaker::Agent }` item — the same row
    the ruling asked for. And the monitor waiter is **not** needed for this: the watcher rings the
    bell itself, so a session with no monitors is woken by a child.
  · **So the first thing to do is not to build anything: it is to re-measure on a daemon that has been
    REBUILT** (`cargo build --release`, then restart that daemon — the rebuild is the half that was
    missed), because the measurement above cannot distinguish *the code is wrong* from *the code was
    not in the binary* — and it is the second.

**The gap that remains, and it is mine — measured precisely, since the first version of this paragraph
overclaimed it.** The chain decomposes, and all but one piece is asserted:

| piece | asserted by |
|---|---|
| the completion is queued, with `kind: Subagent` | `a_subagent_settles_through_the_jobs_own_channel` (`jobwatch.rs:967`) |
| **the bell RINGS** for a child (`Work::Woken`) | the same test, `jobwatch.rs:1016` |
| a completion is taken once, not left for the next wake | the same test |
| a job's sentence | `a_completion_notice_names_the_job_and_says_do_not_wait` (`harness.rs:7764`) |
| **a child's sentence** | **`8533d84`** — landed, and it did not exist before |
| the partition: which noun for which kind, and none for nothing | **`a_settlement_is_two_nouns_out_of_one_channel`** — the partition is now `completion_notices`, a pure function, and it is asserted on a job, a child, both, two children, and an empty queue |
| **`wake()`'s remaining body**: taking the queue and submitting one `Speaker::Agent` item | **nothing, on either kind** |

**Audited 2026-11-02 against HEAD: the last hop is no longer unasserted, and every piece above is now
pinned.** One mark per row, in the table's own order:

  · the completion is queued, with `kind: Subagent` — ✓ fixed in `fd4aaad4`, test `a_subagent_settles_through_the_jobs_own_channel`
  · **the bell RINGS** for a child (`Work::Woken`) — ✓ fixed in `fd4aaad4`, test `a_subagent_settles_through_the_jobs_own_channel`
  · a completion is taken once, not left for the next wake — ✓ fixed in `fd4aaad4`, test `a_subagent_settles_through_the_jobs_own_channel`
  · a job's sentence — ✓ fixed in `114e8d75`, test `a_completion_notice_names_the_job_and_says_do_not_wait`
  · **a child's sentence** — ✓ fixed in `8533d84`, test `a_childs_completion_names_the_child_and_points_at_task_result`
  · the partition: which noun for which kind, and none for nothing — ✓ fixed in `85f890bc`, test `a_settlement_is_two_nouns_out_of_one_channel`
  · **`wake()`'s remaining body**: taking the queue and submitting one `Speaker::Agent` item — ✓ fixed in `fd4aaad4`, test `a_childs_answer_wakes_its_parent_with_nobody_asking` (`crates/harnessd/tests/child_completion_wakes.rs` — a real turn whose stub model emits a `task` call, a real child, `Work::Woken` with nobody calling `task_result`, then `Harness::wake` and one `User { speaker: Agent }` row naming the child and the first line of its answer)

**Narrowed again 2026-10-03 (the paragraph this replaces said the whole body was unasserted).** `wake()`
had the partition inline, where only a live `Harness` could reach it — and this crate has **no fixture
that builds a `Harness` at all** (every harnessd test is a pure-function unit test or a registry-level
integration test), which is *why* the decision had none. So the decision was lifted out into
[`completion_notices`] (`harness.rs`), which is pure and now tested; `wake()` keeps only the part that
is not a decision — take the queue, call the function, submit what it returns. What remains unexecuted
is therefore those few lines, and the reading in the bullet above is still the evidence for them:
reading rather than measurement, stated here rather than left to look stronger than it is. A `Harness`
fixture is the thing to build if even that is to be asserted.

---

## Completion notices are one-liners in leticl and full prose here — **CLOSED on this side 2026-10-04 by `1f0146a`; two pieces named below are not in it** — **SETTLED 2026-11-02, audited: every item ✓ fixed and tested**

**(No R-number: the series is yours to number.)**

The operator, reading a settled job and a finished subagent: *"too much, for example i dont want to see
that message to you 'This is the completion…' I also dont care about 'sabagent you started…' it must be
something like Job <id> <command summary or wrap> finished <result result summary or wrap> same for
agents."*

**leticl folds them, and the whole of it is existing machinery you already have on your side**: the
notice arrives as a `User` row with `speaker: agent`, the fact lines are the `- ` lines under the
daemon's own heading, and **the closing paragraph is a promise TO THE MODEL** — which is exactly what the
operator does not want to read. leticl drops it, keeps the daemon's words verbatim otherwise, and the
full message stays one verb away (`/t` on the row that has one). Its row, read off the operator's live
screen:

    session · Job j83 exited 0 after 3.0s, wrote 5 bytes: sleep 3; echo done
    session · Agent …-sub-1791065633114 · Answer with one word: ready. · done: ready.

Three details worth copying rather than rediscovering:

  · **the vocabulary is TWO openings, and it was one.** `[job] ` was folded and `[task] ` was not, so an
    agent's completion — the same shape, the same speaker, the same paragraph — drew as full prose while
    a job's was a line. It is a list now (`[job] `, `[task] `), and the agent's row is named
    `Agent <id> · <task> · done: <answer>`;
  · **the agent's own task is LOOKED UP, not parsed.** The notice carries only what the child answered;
    what it was asked is on the subagent row (the title — its task's first line), so the row and the
    subagents pane cannot disagree;
  · **counts are per GROUP, not per opening.** `[job] 3 jobs … have ended:` is ONE heading with three
    settlements under it — counting headings read `1 job ended (j12, j15, j19)` — and a coalesced row
    can hold a job's and an agent's, where "2 jobs ended" calls a subagent a job.

leticl's commit: the one-liner is `9b22de3`'s shape and the coalesced-count fix is in the commit after it.

**CLOSED on letibot's side: `1f0146a`.** `folded_notice` (`app.rs`) folds a `[job]`/`[task]` notice
at DRAW time, in the `Speaker::Agent` arm, and the paragraph is simply not drawn. It keeps every
fact, one per line — `Job j57 exited 0 after 7m06s, wrote 508 bytes: sleep 3; echo done`,
`Agent s-…-sub-… · done: 3529`. **The refusal is the half worth keeping**: the opening must be one
of the two known ones, a settlement line must be a bullet with its handle in backticks, and the
only text that may follow is the promise, by name — anything else returns `None` and the row draws
raw, so a notice whose shape changes fails visibly (*"the notices got long again"*) rather than
silently (*"a notice lost a line"*). The `[monitor]` notice is the live case: same voice, same
bullets, deliberately not folded, because leticl does not fold it either.

**still open?** `grep -n "you do not need to wait for it" crates/tui/src/app.rs` — the phrase now
appears only in `folded_notice`'s refusal check and its tests, so a *drawing* reference is what this
is looking for.

**TWO PIECES ARE NOT IN `1f0146a`**, and they are the remainder of this row:

  · **the child's own task in the `Agent` line.** leticl draws `Agent <id> · <task> · done: <answer>`
    and the task is LOOKED UP — the notice carries only what the child answered, and the title lives
    on the subagent row. This head draws the handle and the answer, no task. Threading it means one
    more field on `ItemCtx` (`subagents: &'a [SubagentState]`, four construction sites) — not done,
    and **not to be parsed out of the notice**: a second source for the same fact is how the row and
    the pane come to disagree;
    — ✓ fixed in be5f74a2, test the_folded_agent_line_names_the_task_it_was_asked
  · **opening the whole message on demand.** leticl's folded line says `· /t opens it` and its `/t`
    reaches a `User` row. letibot's `/t` is *all tool rows*, so this needs a verb or nothing. Ruled:
    nothing for now — the transcript and the model's own copy are untouched, which is what makes the
    fold a rendering rather than a loss.
    — ✓ fixed in c7435d80, test a_folded_task_row_opens_its_notice

**THE PLAN AS MEASURED, 2026-10-04** (kept because it is what the two pieces above are against). The render site is
`app.rs:18197`, the `Speaker::Agent` arm of `item_lines` — `(RowClass::Other,
session_block(&text, its.ts, cfg))`, the raw paragraph. The two shapes on the wire, read
out of the store (`transcript_item.item_json`, `"type":"user"`, `"speaker":"agent"`):

    [job] a job you backgrounded has ended:
      - `j57` exited 0 after 7m06s, wrote 508 bytes: <the command>
    <the paragraph, from `completion_notice`, harness.rs:691 — `job_output` verb>

    [task] a subagent you started has finished:
      - `s-…-sub-…` done: 3529
    <the paragraph, from `subagent_notice`, harness.rs:732 — `task_result` verb>

and **one row can hold BOTH groups**, joined by a blank line (measured: a `[job] 3 jobs …`
heading with three settlements, then a `[task]` heading under it) — which is why the group
counts have to be per GROUP and not per row.

**Three things make it more than a pure function here, and they are the whole cost:**

  · the TASK line wants the child's own task in it (`Agent <id> · <task> · done: <answer>`),
    and leticl LOOKS IT UP — the notice carries only what the child answered, and the title
    (the task's first line) is on the subagent row. `item_lines` is given `ItemCtx` and not
    the subagents pane, so the title has to be threaded in or the row resigned to the id
    alone. Do not parse it out of the notice: a second source for the same fact is how the
    row and the pane come to disagree;
  · **leticl's row keeps the whole message one verb away** (`/t` on the row, and its folded
    line says `· /t opens it`). letibot has no such verb for a `User` row — `/t` is *all tool
    rows* — so either the paragraph is simply not drawn (the text stays the row's in the
    transcript and the model still reads it) or a verb is added. **Ruled: do not add a key
    for this yet.** Draw the fold, and say in the row's own doc that the record is intact;
  · **the fold must not touch what the MODEL gets.** The paragraph is the half of R7 that
    tells the model not to `job_wait` — it is a promise to the model, which is exactly why
    the operator does not want to read it. Display only, and a test that asserts the raw
    text is unchanged by the fold.

**done when** both heads draw a settled job or a finished subagent as one row that names it, drops the
model-facing paragraph, and opens the whole message on demand. **leticl's half is landed, `/t` and all;
letibot's is the plan above, with that last clause deliberately not in this change — and the raw text
staying in the transcript, and in the model's context, is what makes that honest rather than a loss.**

---

## T5 — Operator decisions still open — **SETTLED 2026-11-02, audited: every item settled by decision — D3, D5, D6, D7 and D8 are each answered in `DECISIONS.md`**

Carried from `DECISIONS.md`; see there for the full statement of each.

- **D3** — firecode's two asks (parent cgroup, persistent shell). A written "no" is a
  complete answer and unblocks the work; it selects which of two tool runtimes gets
  built. — settled: `DECISIONS.md` §D3 (ANSWERED 2026-09-09 — byobu replaces the
  persistent shell, and cgroup membership is use-dependent)
- **D5** — Falsifier B scoring rubric. — settled: `DECISIONS.md` §D5 (SETTLED 2026-09-09, and the experiment is run)
- **D6** — `max_inline_bytes` (deliberately has no default). — settled: `DECISIONS.md` §D6 (ANSWERED 2026-09-09: a decider, not a number, and the default stays unset)
- **D7** — the 27B preset. — settled: `DECISIONS.md` §D7 (ANSWERED 2026-09-09: derive it by measurement)
- **D8** — the harness licence. The workspace currently declares Apache-2.0 and there
  is no GitHub remote yet; the repo is intended to be public, so this should not stay
  open. — settled: `DECISIONS.md` §D8 (SETTLED 2026-09-09: Apache-2.0, which the
  workspace already declared)

---

## T22 — `f_keep` names two different quantities, and C4 applies one's threshold to the other — **SETTLED 2026-11-02, audited: every item ✓ fixed and tested**

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
   claims. Needs the server's `lcp`, which is S1 or `-lv 4`. — ✓ fixed in cca619f8, test
   f_keep_is_indifferent_to_growth_and_f_sim_is_not: C4 is `lcp / cached_entry` in the plan
   (`docs/implementation-plan.md:296`) and `f_keep` is `cached / what the previous turn left`
   (`crates/turn/src/metrics.rs:66`), with no server change and no `lcp` needed.
2. **Keep `cached_tokens / prompt_tokens`** and set a bar reachable for tool-using
   sessions. The harness-only version of this is T17's C4b, which measured p10 1.0000.
   — not taken: the decision was option 1 (D11), and `f_sim` is now printed beside `f_keep`
   rather than carrying its threshold (`crates/turn/src/metrics.rs:98`).

Until it is decided, **M1 remains formally unexited** and the run stands as recorded.

— decided: `DECISIONS.md` §D11 (SETTLED 2026-09-09). The re-measurement this asks for is T17's
and has not been done there either.

This is the seventh instance in one day of a number whose meaning was not checked
before use, and unlike the others it is **ours** — it is in the plan, not in something
we inherited.

---

## R75 — A model's command can drive the operator's panes and doors through the daemon's socket — **DECIDED AND BUILT 2026-11-03: a tool call's connection gets the askpass door and no other**

Found answering the operator's *"who owns !term process?"* / *"so it means say web head can have
access to that?"*. The web head (`crates/harnessd/src/httphead.rs`) cannot — it is stateless,
seated with no tools, no store and no pane frames. The hole is the daemon's unix socket:

- The socket is `srw-------`, the operator's user only (`/run/user/1000/letibot/<key>.sock`).
- The server answers `!term` attach / run / input / close, `!` lines, `/mode` and the other
  operator doors (`crates/sessionlog/src/server.rs` ~1300) for **whoever connected** — a "head"
  is anything that speaks the protocol; there is no identity on the connection.
- Every model tool call in an UNCONFINED session is handed the way in: `LETIBOT_SOCKET` and
  `LETIBOT_SESSION` are on every command's environment (`crates/harnessd/src/harness.rs` ~2435),
  for `letibot-askpass` to put a sudo prompt in front of the head.

So in `leticode` (unconfined, the launcher's default) a model's `bash` call can connect and type
into the operator's open `!term` pane, or start its own `!term` / `!` line — which runs as the
OPERATOR's own act and is therefore ungated: the gate judged only the outer command that opened a
socket. Confined sessions are not exposed (`/run` is a fresh tmpfs inside the boundary).

**The decision (the operator's, 2026-11-03): close it, by asking the kernel who connected.** The
operator's own words: *"read the connecting process from the kernel ... and classify the
connection: from inside a session's own tool tree ..., or not"*. So a connection from inside this
daemon's own tree is a tool call and not a head, and it gets only the askpass door.

**The rule, as built** (`crates/sessionlog/src/peer.rs`, `Peer::of`):

- The pid comes from the kernel at `connect` — `SO_PEERCRED` on Linux, `LOCAL_PEERPID` on macOS —
  read in `serve_conn` before the stream is handed to the reader. Not from a frame: a head's own
  `kind` and `identity` are claims a command can type.
- `inside` is the parent chain up to this daemon (`/proc/<pid>/stat`, `PROC_PIDTBSDINFO`) **or**, on
  Linux, `/proc/<pid>/cgroup` naming `letibot.<daemon pid>.<n>` — the tree every tool call and job
  runs in. The cgroup arm is the one a double-forked child cannot leave; a process is not its own
  descendant, which is what keeps every head that attaches to a daemon it started.
- A peer the kernel will not name is `inside`: an unsupported platform, or a process that exited
  under the question, falls on the side that refuses, never the other way.
- The doors a tool tree keeps are `Attach`, `Ack`, `Resync`, `Detach` and `Askpass`. Every other
  frame is answered `Rejected` **by name** — `!term opening a pane`, `!term keys`, `!term closing a
  pane`, `a ! line`, `/mode`, `a / verb`, `the settings rows`, `the session list`, `typing a prompt
  to the model`, `a decision`, `stopping the daemon`, and the rest — with the sentence naming the
  door and the pid, so a person reading it knows what was refused and why.
- Nothing else was needed: no per-head credential, no second socket, and no token. The `!`-line and
  `!term` paths are unchanged for a head.

**What the operator said is NOT closed by this** — carried here so the next reader does not think
the socket was the whole story: *"when the operator's terminal runs inside tmux or screen, an
unconfined command can still type into it through the multiplexer (`tmux send-keys`, `screen -X
stuff`); that is outside letibot's socket and is not closed by this change. Confined sessions do not
reach the operator's tmux socket."*

**And two limits of the rule itself**, measured while building it: on macOS there is no cgroup, so a
child that double-forks (`setsid`) leaves the parent chain and is read as a head there; and a
program the daemon started outside a tool call — a `!term` pane's program, a background job — is
also `inside` by this rule, so it is not a head. Nothing in the tree attached as a head from either.

**Done when:** a test connects to the socket from a process placed in the tool tree and is
refused `!term` input and `!` lines while askpass's ask still works; a head connection is
unaffected. — **DONE**, three ways:

1. `crates/sessionlog/tests/peer_doors.rs` re-executes itself as a child of the process that serves
the socket and asks for eleven operator doors, each refused with a reason that names it, then asks
for the password and gets the head's answer; the same file attaches a head from the serving process
and checks the `!` line is accepted and the session list answered.
2. `crates/harnessd/tests/askpass_from_tool_tree.rs` runs the production `letibot-askpass` binary as
a child of the serving process and asserts its stdout is the password, byte for byte.
3. `crates/sessionlog/src/peer.rs`'s own tests: a spawned child is inside, this process is not, and
an unread peer is on the refusing side.

**The other half of this entry — pane ownership — is filed as R75a.**

---

## R75a — A pane outlived its session, and a dead daemon's panes were never swept — **SETTLED AND BUILT 2026-11-03: all three, so a pane now has an owner of record**

Split out of R75, which answered *who may type into a pane* and left *whose pane it is*. The
measurements stood: `Terminals` is keyed by session and pane, each pane is its own
`session.term-<session>-<N>` scope under the daemon's tree, and the only things that ended one were
`!term close`, its program's exit and a clean daemon stop. `Sessions::close` named no driver, so a
session's close and its park left its panes running in a session nobody could reopen, and a daemon
killed hard left them in a tree whose owner pid was gone.

**The three answers, and each is what the tree now does:**

1. **A pane ends with its session.** `TerminalDriver::end_session` is the new seam - a default of
   `0`, so a driver that holds no pane is unchanged - and `Terminals` implements it: every pane the
   session holds is closed with the reason `the session ended`, its scope ended, and the map entry
   dropped. `Sessions::close` calls it FIRST and puts the count in its own answer
   (`2 pane(s) ended with it`), which is the daemon log's report for a close and the only report a
   PARK has, because `park_idle_children` closes through the same function.
2. **A daemon's start sweeps what a killed predecessor left.** `Terminals::begin` runs before any
   head can open a pane: it reads `<data dir>/panes/<workspace key>.scopes` - the tree roots this
   workspace's daemons recorded, one path per line - and for each root whose `letibot.<pid>.<n>`
   names a DEAD pid, ends every `session.term-*` child and forgets the line. The two facts that
   licence it are the cgroup path and the dead owner pid, and nothing else: a LIVE daemon's root is
   kept, a job or turn scope in the same tree is not a pane and is left alone, and a directory this
   format did not write is never touched. A root whose panes survive the kill stays recorded, so the
   next start tries again. Then it records its own root (`ScopeTree::root`, new and defaulted), so
   the next daemon can find it.
3. **A pane is capped.** `PaneMemory` resolves the same policy a `bash` call in that session gets
   (the box's `[memory]` under the user's `leticode.toml` and the session's own project's, against
   what is free) and `Terminals::open` puts it on the pane's scope BEFORE the program starts. A tree
   that will not take the cap says so once (`panes run UNCAPPED: ...`) rather than per pane, and a
   box with no `/proc/meminfo` has no controller to hold one, which is `bash`'s answer there too.
   The daemon wires both at `crates/harnessd/src/cli.rs`, where the terminal driver is installed.

**Done when** a test says which - **DONE**, and none of them needs a cgroup: they run the real
driver against a `ScopeTree` whose scopes are directories, so the daemon's DECISION is measured
instead of the kernel's.

- `crates/harnessd/src/term.rs`'s `closing_a_session_ends_its_panes_and_counts_them`: two real
  programs on real ptys; `end_session` answers 2, the session holds no pane after, the tree was told
  to end exactly those two scopes, and the two pids are GONE (waited for), which is the half a
  scope's name cannot prove.
- `the_close_report_says_what_a_session_took_with_it`: the sentence, in the four shapes it has
  (`None` with no pane is the one that says nothing).
- `a_pane_is_capped_by_the_policy_its_session_gets`: the pane's own scope is the one capped, at the
  policy's default, and `memory.max` holds that number; skipped through `apparatus::present` on a box
  with no `/proc/meminfo`.
- `a_start_ends_the_panes_a_dead_daemon_of_the_same_workspace_left`: the dead root's two pane scopes
  are ended and gone, while the LIVE root's pane, the job scope beside them and a directory with no
  owner pid in its name are all untouched.
- `a_start_records_this_tree_after_sweeping_a_dead_one`: the record it writes is its own root, and
  the swept one is not in it.

The `TerminalDriver` default is what keeps every other implementor - a fake in sessionlog's own
tests - compiling without an `end_session`.

**Not done, so nobody reads it as covered:** a pane is capped by the DAEMON's memory policy resolved
against the session's workspace, so a session whose own project sets `[memory] default` differently
from the daemon's workspace gets the daemon's number for its pane. The record is per workspace, so a
pane scope whose record was lost (a wiped data dir) is debris nobody claims.

---

# Settled 2026-11-03 — the entries a packaging pass closed against main

Each entry below was filed in `TODO.md`, and its work is on main: the entry names the commits that
landed it and the test that proves it. Every entry is verbatim as it was filed except its status
line, which now says DONE and names the commits. Where an entry had something left in it, it stayed
in `TODO.md` and says what is left.

<!-- range A -->

<!-- range B -->

<!-- range C -->

<!-- range D -->

<!-- range E -->

<!-- range F -->

<!-- range G -->

<!-- range H -->

<!-- range I -->

<!-- range J -->

<!-- range K -->
<!-- range F -->

## R73 — A compaction is eight lines of notes and two numbers that read as 1.95M tokens — **DONE 2026-11-03: one updating line and one honest compaction row (leticl's half is its own repo's), a33655d7, 272a96a9; a_half_is_progress_from_its_first_event_to_its_last_and_never_a_warning and a_compaction_draws_one_row_with_its_unit_and_what_it_kept_and_dropped**

The operator, pasting what leticl's head showed for ONE compaction: *"this is how much text i get
for a compaction in leticl"*:

```
· compact_half — compacting half 1 of 2: the most recent exchanges — 1256 item(s), 976900 token(s)
  to read. Nothing of this half reaches the transcript; it is summarised in a scratch session of its own.
  → nothing to fix — the conversation is being summarised. The bar above the composer is that fold; …
· compact_half — half 1 of 2 answered: 6189 chars written.
  → nothing to fix — …
· compact_half — compacting half 2 of 2: the older part — 43 item(s), 976930 token(s) to read. …
  → nothing to fix — …
· compact_half — half 2 of 2 answered: 7357 chars written.
  → nothing to fix — …
```

**Three defects, two of them here:**

1. **Progress is sent as warnings.** `summarise_one` (`crates/turn/src/compaction.rs:1745`) emits
   `TurnEvent::Warning { code: "compact_half" }` at the start and at the end of each half — four
   warning rows for one operation, each a permanent row in the log and the view. The fold already
   has a progress bar (`CompactionSink`); `compact_half` was classed `Routine`
   (`crates/sessionlog/src/warning.rs:148`) to stop it reading as a fault, which kept it a row.
   It should be progress that updates one line in place — *"compacting: half 1 of 2 · 41%"* — and
   one closing line: *"compacted: 937k → 75k tokens (deepseek's count), summary 13.5k chars"*.
   — ✓ fixed in a33655d7, test a_half_is_progress_from_its_first_event_to_its_last_and_never_a_warning
2. **The token numbers mislead.** Each half says ~977k "to read", and a reader adds them to 1.95M
   for a session the head showed at 740k. Two reasons: they are LEDGER tokens
   (`scratch.ledger.len()`, `:1742`, reasoning included — the unit R72 is about), and the halves
   OVERLAP by design (`summarise_overrun`, `:~1560`: the tail runs from `tail_from`, the old part
   up to `cut`, and `cut > tail_from`), each filled to what fits. Say the unit; say the overlap or
   print the halves' share of one total.
   — ✓ fixed in a33655d7, test the_compacted_line_is_one_unit_said_once
3. **leticl adds a remedy line under every one** (`leticl/src/cards/note-card.lisp:182`,
   `"compact_half" . "nothing to fix — …"`; `src/session/warnings.lisp:289`). That is leticl's to
   fix — a remedy line belongs to a fault, and a routine event has none — and moot once (1) sends
   progress instead of warnings. Filed here so it is not lost; leticl's own TODO is the place for
   its half.
   — OPEN: leticl's repo, not this one — the remedy line under a routine code (`leticl/src/cards/note-card.lisp:182`, `src/session/warnings.lisp:289`) is not in this tree, so nothing here can change or check it.

**Done when:** one compaction draws one updating line and one closing line in both heads, the
closing line's numbers are in the provider's unit (R72) and add up, and no `compact_half` row
remains in the log.

**What landed (`r72-wall-units`):** `summarise_one` (`crates/turn/src/compaction.rs`) emits no
`compact_half` warning any more. Each half runs through `run_half`, which publishes the half's
`CompactionProgress` as it begins (so the line names *half 1 of 2* before the server says
anything), lets the existing `CompactionSink` update it while the server reads and writes, and
publishes once more with the final count — nothing else reaches the sink (test:
`a_half_is_progress_from_its_first_event_to_its_last_and_never_a_warning`, which failed with the
old warning emits put back). The per-half prompt size stays only as the progress bar's
denominator and its comment says why it is not a sum (ledger units, halves overlap). The closing
account is the `compacted` report, now in the provider's unit (R72's `Scales::unit`). The Rust
head already draws `CompactionProgress` as one walking line (`crates/tui/src/ui/loading.rs`,
`compacting_line`) and clears it on `compacted`/`auto_compact_failed`, so it needed no change;
`compact_half` stays classed `Routine` in `crates/sessionlog/src/warning.rs` and in the notes pane's
`COMPACTION_ROWS` for logs written before this.

**What landed (`wt-r73`, the honest line):** an automatic compaction now leaves ONE durable row,
and that row carries the four facts a reader needs.

- **The sentence** (`crates/harnessd/src/sessions.rs::compaction_said`) is `compacted: 958397 →
  100005 deepseek tokens, kept 12 item(s) verbatim plus a 13532-character summary, 2 item(s)
  dropped at the seam, on transcript s#t25.` — the size before and after in the daemon's own unit
  for that count, what the summary stands in for, what was kept verbatim, and what was dropped. A
  count said once: the two sentences that repeated the tail's numbers are gone. The `compacted`
  report also carries *the summary was CUT OFF* now (`sessions::SUMMARY_CUT_OFF`, shared with the
  daemon's log line).
- **The third row is gone.** The wall door published a second `auto_compact` row after the report
  with the same numbers and that cut-off sentence; it is deleted, so nothing named `auto_compact`
  names a size any more (asserted in `crates/harnessd/tests/compact.rs` and `wall_refusal.rs`).
- **The announcement is not a row.** A fold that is about to run — the wall, a refusal for
  length, which fold was chosen — is published as `compacting` (new, `Class::Routine`), and the
  head takes it as STATE: it is the progress line's click (`App::compacting_why`), never a note.
  An older head draws it as the routine row it used to be, which is the same text.
- **The drawn line** (`crates/tui/src/ui/transcript/notice_card.rs::compaction_line`) is
  `session · compacted · 958k → 100k deepseek tokens · kept 12 items + a 13.5k-char summary ·
  2 dropped · click for details`, with `≈` kept in front of an estimated after-count, `nothing
  dropped` when the sentence says so, and `summary CUT OFF` when it does. It parses the whole unit
  (`deepseek tokens`, not the bare `tokens` the old line printed beside a provider's number), and
  a detail it cannot read falls back to the words it has rather than guessing. Pinned by
  `a_compaction_draws_the_size_the_unit_and_what_it_kept_and_dropped` (the card, five shapes) and
  `a_compaction_draws_one_row_with_its_unit_and_what_it_kept_and_dropped` (the frame).

**Still open:**

1. **leticl** (its repo, not this one): stop adding a remedy line under routine codes
   (`src/cards/note-card.lisp:182`, `src/session/warnings.lisp:289`), draw `CompactionProgress`
   as one line updated in place (half N of M, read/written count) cleared by the `compacted`
   warning, and read the `compacted` detail the way this tree's head does — that first sentence is
   the contract, and `compaction_of` is its other end. **And it has to know one new code:** a fold
   about to run is announced as `compacting`, so a head that takes it as state keeps one row per
   compaction; one that draws every warning keeps drawing it, which is what it did before.

---
