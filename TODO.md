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

## T7 — Falsifier B: what we are actually asking — **needs the operator, question restated**

The compressed version was unanswerable. Plainly:

**The assumption on trial.** The plan assumes a long conversation stays usable right
up to the context window, so compaction can be lazy and mostly-soft. If that is
false — if quality collapses at, say, 60k when the window is 262k — then compaction
must be aggressive and early *whatever it costs in prefill*, and a whole section of
the plan is wrong. Nobody has measured it. It is milestone M3.5, not a footnote.

**The experiment.** Run the same task three times, with 20k / 60k / 150k of unrelated
preceding conversation already in context. Same task, same model, same everything
else. Then see whether the answers get worse as the preamble grows.

**The open part is only: what does "worse" mean.** A human rating is subjective and
does not survive being re-run months later. My proposal:

> Use this repo's own work as the task — "implement this small, fully specified
> function and its tests" — and score it **objectively**: does it compile, do the
> tests pass, on the first attempt. Three depths, several tasks each, one number per
> depth.

That is reproducible by someone who was not there, and it is the workload we actually
care about rather than a proxy for it. The alternative is a rubric someone scores by
hand, which is more sensitive but not repeatable.

**What a decision changes:** nothing until M3.5, and then it decides whether the
compaction design in §10 survives contact with measurement.

---

## T8 — Cloud-hosted models — **new requirement 2026-09-09, and it cuts across the central bet**

Stated by the operator: letibot, flowy and firecode must keep working with
cloud-hosted models. With the sharp observation that there are **two different
clouds**, not one:

> "if it my own cloud compute the tokens still do not matter much since i pay for
> hardware time. but if it is a token-metered setting - then the usuals."

So there are three modes, and the middle one is nearly free:

| mode | who renders | submit shape | what a token costs |
|---|---|---|---|
| **1. local** | us | token ids to `/completion` | wall clock |
| **2. own cloud compute** | us | token ids to `/completion` | wall clock (hardware time) |
| **3. token-metered API** | **the provider** | `messages` | **money** |

**Mode 2 is architecturally mode 1.** We still run the server, so self-rendering,
the token ledger and the structural prefix invariant all survive. What changes is
latency, and possibly the engine (vLLM rather than llama.cpp) — that is a backend
detail, not a design change.

**Mode 3 breaks §3.1 and §7, and it is worth being blunt about which parts die.**

- **We cannot render.** The provider owns the template. Everything in
  `docs/chat-templates.md` — provenance, the Text/Control split, fidelity gates —
  is inapplicable, because we never produce tokens.
- **The prefix invariant stops being structural.** §4.3's whole claim is that a
  violation is *inexpressible* because request N+1 is the same memfd region read to
  a longer length. Against a `messages` API we can only *assert* prefix stability
  after the fact, which is what every other harness does and what the plan set out
  to improve on.
- **`parse` survives.** Providers return structured content and tool calls, so the
  transcript model (§4.2) is unaffected. `TranscriptItem` was the right shape either
  way.
- **Cost accounting becomes a first-class metric.** In modes 1 and 2 the opencode
  config's comment holds — "input tokens are free, wall clock is not". In mode 3 it
  inverts, and compaction stops being a latency optimisation and becomes a spend
  control. Two different policies, same mechanism.
- **`EXPLAIN` goes shallow.** Per-stage cache/compute attribution needs the server's
  own counters. A provider gives us `cached_tokens` at best.

**The decision this forces, and it should be made before W6 not after.** The turn
engine needs a backend seam:

```
trait Backend {
    fn submit(&self, ...) -> TurnStream;   // token ids OR messages
    fn caps(&self) -> BackendCaps;         // renders_locally, accepts_token_ids,
}                                          // reports_cache_stats, meters_tokens
```

Named now, mode 3 is an implementation. Retrofitted, it means touching the turn
engine, compaction, EXPLAIN and every metric — this is exactly what D6 taught about
`max_inline_bytes`, one week earlier and one level larger.

**DECIDED 2026-09-09 (D10): mode 3 later, seam reserved now.** `crates/backend`
exists with `BackendCaps`, `PrefixGuarantee`, `Meter` and `TurnCost`, and no
implementation behind it. Suites must skip loudly rather than pass vacuously;
`skip_reason()` returns a message, not a bool, so the silent skip is the harder one
to write.

**Still open, deferred with the milestone:**

1. Which provider to build against first. "Apple ecosystem, I don't care about other
   providers" was the local stance; mode 3 needs one concrete API.
2. Whether `EXPLAIN` renders `BackendCaps` inline on every plan or only on a
   capability change. Inline is honest and noisy.
3. Whether compaction reads `Meter` directly or is handed a policy — the same
   interface-versus-constant question D6 settled for spill.

---

## T9 — Compaction economics invert with the meter — **analysis, feeds T8 and T7**

The open question left by D10 ("does compaction read `Meter` directly or take a
policy") has a sharper answer than it looked: **the tradeoff does not merely change
size between the two modes, it changes sign.**

### Local / own compute — compaction COSTS time and saves nothing in the steady state

With a warm prompt cache, turn N+1 carrying the full history is a **prefix hit**:
the server prefills only the new tokens. Keeping everything is close to free.

Compacting replaces that history with a summary, which is a **different prefix**, so
the cache entry no longer applies and the next turn is a cold prefill. Measured on
this box with opencode: 144,436 tokens, cache hit 0, ~13 minutes to first token.

So locally, compaction has no cost benefit at all. Its only justifications are:

1. the context window is a hard wall,
2. KV memory pressure (`--cache-ram`, and the ladder that evicts under it),
3. **quality degrading with depth — which is exactly what T7 measures and nobody
   has measured yet.**

If T7 comes back saying quality holds to the window, then the local trigger policy
is "compact when you must, as late as possible", and §10's leveled design is doing
work that only pays off at the wall.

### Metered API — compaction saves money on every subsequent turn

Every input token is billed on every request. Cached input is discounted rather than
free, and writing a cache entry typically costs *more* than a plain input token, so
there is an optimum rather than a monotone answer — the exact multipliers are
provider-specific and must be read from the provider's own pricing, not assumed.

The shape, though, is unambiguous: history you carry is rent, paid per turn, forever.
Compaction is a one-off cost that lowers the rent. **Compact early and often** is
correct here and wrong locally.

### The other asymmetry: what evicts the cache

| | local | metered API |
|---|---|---|
| cache bounded by | **memory** (`--cache-ram`, entry count, the eviction ladder) | **time** (a TTL measured in minutes) |
| an idle conversation | keeps its entry until something else needs the RAM | loses it on a timer and pays full price on return |
| we control eviction | yes — it is our ladder | no |

This one has a consequence the plan does not currently carry: on a metered backend,
**wall-clock idleness is itself expensive**, so a conversation resumed after a pause
should expect a cold price. A "keep the session warm" heartbeat is a rational move
there and a pointless one locally — the reverse of what the local design assumes.

### What follows

- The **mechanism** is shared: segments, levels, the warm summarizer.
- The **trigger policy** is per-meter and must not be a constant. It reads
  `BackendCaps.meter` plus the window and the memory budget.
- `TurnCost` already carries both units so a policy can be written against either
  without the caller conflating them.
- This is the third instance of the same lesson (D6 spill threshold, D10 backend
  seam, this): **the decision is an interface, not a number.** Worth stating once in
  §10 rather than rediscovering a fourth time.

Depends on nothing; blocks nothing today. It should be settled before W13 is written,
because a compaction scheduler built around the local assumption will need reworking
rather than configuring.

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
