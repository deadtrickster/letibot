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
| `model_endpoint_retry` | the server did not answer and the round is being taken again |
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
