# T1 — can a Rust Jinja engine replace hand-written per-model renderers?

Measured 2026-09-09 on `dead-XMG-NEO-E25`. No GPU, no model weights, no server,
no port — only the two templates extracted from the GGUFs. `./run.sh`
reproduces every number below.

---

## Verdict: **adopt the template-driven design, on `minijinja` 2.24 plus a named compatibility shim, and keep the CPython differential as a gate in CI.**

`minijinja` renders both shipped templates **byte-exact against the CPython
oracle on 4,132 renders** across five corpora, including 2,200 randomly
generated conversations. It scopes `{% set %}` per loop iteration correctly —
it does **not** have minja's bug — it supports `{% break %}`, and it takes a
custom `tojson`. The sentinel provenance technique works unchanged under it:
the strip-back is exact, and the LITERAL/DATA map it produces is **identical to
the authority's, region for region, in every case where both engines produced
one**.

It is not a drop-in. Getting from "renders the template" to "renders it
byte-exact" took **eight** deliberate configuration decisions. **Four of them
fail silently**, and **three of those four were found only by running the two
engines side by side and diffing** — not by reading either engine's
documentation. That is the real result, and it is why the recommendation is
*template-driven with a differential gate* rather than *template-driven*.

One divergence remains unfixed, and it is bounded and characterised: §5.

---

## 1. The questions T1 asks

| # | question | answer |
|---|---|---|
| 1 | `{% break %}` / `loopcontrols`? | **Yes** — cargo feature `loop_controls`. `{% break %}` and `{% continue %}` both match CPython. |
| 2 | is a bare `{% set %}` scoped per loop iteration? | **Yes, correctly.** minijinja does **not** have minja's bug. `namespace()` carries across iterations, exactly as in CPython. |
| 3 | custom `tojson` with `ensure_ascii`? | **Yes** — `env.add_filter` overrides the builtin, and `Kwargs` gives it named arguments. Note minijinja's *stock* `tojson` is as unusable as Jinja2's: it rejects `ensure_ascii` **and** escapes `<`, `>`, `&` as `<`-style. Replacing it is mandatory on both engines, for the same reason. |
| 4 | byte-exact on GLM's and Qwen's real templates? | **Yes**, 4,132/4,134 renders. The two exceptions are one edge case, §5. |
| 5 | does the sentinel technique work under it? | **Yes.** Strip-back exact, region maps identical to CPython's, no `Control` span over DATA. |

Question 2 was the one that could have been decisive, so it was run first and
in isolation (`src/bin/probe.rs` against `probe_cpython.py`):

```
                    minijinja        CPython Jinja2
set_scope           "A- B:R2 C- "    "A- B:R2 C- "     ← per-iteration, correct
namespace_carry     "A:None B:R2 C:R2 "  (same)        ← carries, correct
break               "12"             "12"
continue            "124"            "124"
```

The `set_scope` probe is the GLM leak in miniature: message C has no reasoning
of its own and must print `-`, not `R2`. minja prints `R2`. minijinja prints
`-`.

---

## 2. What was measured

Both engines are handed **byte-identical arguments**. `compare.py` does the
request adaptation once, in Python, through `tests/fidelity/oracle_hf.py`, then
ships the resulting kwargs to the Rust binary as JSON. A difference in the
output is therefore a difference in the engine (or in a filter that had to be
re-implemented), never in the driver.

| corpus | cases | GLM template | Qwen template |
|---|---:|---|---|
| `tests/fidelity/fixtures/` via `letibot-render` | 139 | 139 exact | 115 exact, 24 both refuse |
| hand-written Qwen-native (`--qwen`) | 18 | 17 exact, 1 both refuse | 14 exact, 4 both refuse |
| random, seed 1234 | 800 | 800 exact | 591 exact, 209 both refuse |
| random, seed 777 | 700 | 700 exact | 532 exact, 168 both refuse |
| random, seed 99 | 700 | 700 exact | 518 exact, 182 both refuse |
| numeric edge cases (`--edge`) | 4 | 3 exact, **1 differs** | 3 exact, **1 differs** |
| **total** | **2,361** | **2,359 exact** | **1,773 exact** |

* **4,132** byte-exact renders.
* **588** cases where both engines refused. Every one refused *for the same
  reason*: where the template author wrote `raise_exception('...')`, the
  messages are identical strings. (Engine-internal type errors — `'str object'
  has no attribute 'items'` vs `string has no method named items` — are each
  engine's own prose for the same condition and are counted as agreement;
  requiring identical wording there would be measuring error strings, not
  behaviour.)
* **0** cases where one engine rendered and the other refused, in either
  direction. That is the asymmetry that would matter most in production, and it
  did not occur once.
* **2** divergences, both the same `bigint` case. §5.

The GLM fixture corpus is the one that gates the hand-written renderer, so
transitively: **`crates/dialect-glm` and `minijinja` agree** on all 139 cases,
because each independently equals the CPython oracle on them.

### Why a fuzz corpus, and not just the fixtures

The standing objection to a second Jinja implementation is the one that produced
the minja bug: fixtures only test what somebody thought of. `gen_cases.py
--fuzz` draws every value from a set chosen to hit places two engines could
plausibly part company — float repr, integer width, map key order, unicode above
the BMP, real Private-Use-Area codepoints (which is also an attack on the
sentinel picker), whitespace-only strings, empty containers, `null` vs absent,
and nesting deep enough that `tojson` has to walk it.

It earned its place immediately: **three of the eight shim items in §3 were
found by the fuzzer and by nothing else.**

---

## 3. The shim — what "minijinja can render these templates" actually costs

`transformers` does not run Jinja. It runs Jinja **on top of the Python object
model**, and chat templates lean on that. Eight things had to be decided; the
column that matters is how each one failed.

| # | what | how it failed without it |
|---|---|---|
| 1 | feature `loop_controls` | **Loud.** `unknown tag 'break'` at parse. |
| 2 | `minijinja_contrib::pycompat` unknown-method callback | **Loud.** `string has no method named strip` / `startswith`; `map has no method named items`. 97 of 139 GLM cases and 103 of 139 Qwen cases died on it. `.strip()`, `.startswith()`, `.endswith()`, `.items()` are *Python*, not Jinja. |
| 3 | replacement `tojson` (separators `", "` / `": "`) | **Silent.** `serde_json::to_string` emits `{"a":1}`; `json.dumps` emits `{"a": 1}`. Every tool schema differs. |
| 4 | minijinja feature `preserve_order` | **Silent.** Without it minijinja's value map is a `BTreeMap` and **sorts object keys**, so `{"name":…,"description":…}` renders as `{"description":…,"name":…}`. 42 GLM cases and 40 Qwen cases, found by the fixture corpus. |
| 5 | CPython `repr(float)` in `tojson` | **Silent.** Rust's `{}` for `f64` never uses exponent notation: `1e300` printed as a 301-digit integer against CPython's `1e+300`. Found only by the fuzzer. |
| 6 | override `is iterable`, `is sequence`, `is number` | **Silent, and reachable.** Jinja2's answers are Python's: `none is iterable` is **False** (minijinja says True), `str`/`dict` `is sequence` is **True** (minijinja says False), `True is number` is **True** (minijinja says False). GLM's `visible_text` and Qwen's `render_content` both branch on `is iterable`, so a message with `content: null` rendered differently. Found only by the fuzzer. |
| 7 | `raise_exception` global | **Loud**, and both templates lean on it hard — Qwen calls it in ten places. |
| 8 | `trim_blocks`, `lstrip_blocks`, lenient undefined | **Silent** whitespace differences throughout. |

Items 3–6 are the finding. Four silent failure modes, all in the seam between
"Jinja the language" (which minijinja implements correctly) and "Python the
runtime" (which it does not, and does not claim to). The shim is 60 lines of
Rust and two cargo features. It is small, and it is **not discoverable by
reading either engine's documentation** — items 4, 5 and 6 were found by running
the two engines side by side and diffing.

That is the argument for keeping the CPython differential as a permanent CI
gate rather than a one-off validation, and it is what converts "another
reimplementation, same class of risk as minja" into a priced, bounded risk: the
risk is not that minijinja is wrong, it is that a *new template* reaches a
corner of the shim nobody has exercised. A gate that renders every fixture
through both engines catches exactly that, costs no GPU and no server, and
already exists in `tests/fidelity/`.

---

## 4. Provenance under minijinja

The technique from `docs/chat-templates.md` §4 — wrap the data, not the template
— transfers without modification.

* **Strip-back exact:** 2,358 of 2,361 (GLM) and 1,772 of 1,773 (Qwen). The
  three failures are cases where the **template compared or trimmed the payload**
  (Qwen's `content.startswith('<tool_response>')` and `|trim`), so the sentinels
  changed the control flow. **CPython refuses a map on those same three cases.**
  This is the self-validation working as designed, on both engines, and it is a
  property of the template rather than of the engine.
* **Region maps identical:** in every case where both engines produced a map,
  minijinja's `(start, end, LITERAL|DATA)` list equals CPython's exactly. 0
  differences in 4,132 comparisons.
* **No `Control` span over DATA:** 0, on the 139 fixture cases that carry a
  `RenderSpan` list from `letibot-render`. Same check as
  `run_gate.py::check_span_kinds`, re-pointed at the map minijinja produced.
* **The map is not vacuous.** Across the fixture, Qwen-native and seed-1234
  corpora, control-token literals appear in the rendered prompt from *both*
  origins and the map separates them by origin rather than by spelling:

  | template | template-emitted | payload-supplied | straddling a boundary |
  |---|---:|---:|---:|
  | GLM | 16,451 | 2,020 | 0 |
  | Qwen | 11,944 | 1,597 | 0 |

  Nine of GLM's thirteen control literals and eight of Qwen's ten were observed
  from *both* origins — `<|assistant|>`, `<think>`, `<|im_start|>`, `[gMASK]`,
  `<arg_key>` among them. Every payload-supplied occurrence classified DATA.
  This is the injection property, measured rather than argued.

---

## 5. The one unfixed divergence

**Integers outside `i64`/`u64` render differently.**

```
tool call argument  {"a": 2**70}
CPython             <arg_value>1180591620717411303424</arg_value>
minijinja           <arg_value>1.1805916207174113e+21</arg_value>
```

Python integers are arbitrary precision. `minijinja::Value` carries `i64`, `u64`
and `f64` and has no bignum, so the value is already an `f64` before `tojson`
sees it. `serde_json`'s `arbitrary_precision` feature does **not** fix it — the
precision is lost in the conversion into `minijinja::Value`, downstream of
serde. Tried and measured, not assumed.

Bounded and detectable: it needs a tool-call argument or tool-schema constant
with |n| > 2⁶⁴−1. Three ways to close it, none of them blocking:

1. Reject such values at the JSON boundary in `crates/dialect` (they cannot
   survive a round trip through most JSON consumers anyway).
2. Carry them as strings, which is what any wire protocol that cares does.
3. Leave it, and let the CI differential gate report it if a template ever meets
   one.

Everything else in the numeric model agreed exactly, including `1e16`, `1e-5`,
`-0.0`, `5e-324`, `1.7976931348623157e308`, `1/3`, and `2**63`/`2**64-1` at the
boundary.

---

## 6. Cost

51 KB prompt (the `long-turn` fixture), release build, 300 iterations:

| | template compile | render | render ×2 + compare (provenance) |
|---|---:|---:|---:|
| minijinja, GLM | 220 µs | **7.1 µs** | **14.3 µs** |
| minijinja, Qwen | 150 µs | **15.0 µs** | **30.2 µs** |
| CPython, GLM | 17.1 ms | 26.1 µs | 57.0 µs |
| CPython, Qwen | 11.2 ms | 41.6 µs | 93.0 µs |

Rendering twice — which is what provenance costs — is **tens of microseconds**
against a prefill measured in hundreds of milliseconds. It is not on the
critical path in any meaningful sense, and the compile is done once at startup.

Dependency footprint: `minijinja` + `minijinja-contrib` (default features off,
`pycompat` only) pull **18 crates**, of which `serde` and `serde_json` are
already workspace dependencies. No C, no Python, no build scripts beyond serde's
proc macros. Compare with the alternative T1 names: pyo3 + a CPython runtime
inside the daemon.

---

## 7. What this means for `crates/dialect` and for T2

A `Dialect` stops being an implementation and becomes **data plus a small amount
of declared behaviour**:

```rust
pub struct Dialect {
    template: String,              // out of the GGUF, hashed
    template_sha: [u8; 32],
    control_tokens: Vec<ControlToken>,   // literals, from tokenizer metadata
    stop_tokens: Vec<StopToken>,
    system_update_mode: SystemUpdateMode,
    guards: Vec<Guard>,
    quirks: Quirks,                // e.g. the minja-compatible profile
}
```

`render` becomes one function, shared by every dialect: render the template
twice (clean, sentinel-wrapped), self-validate the strip-back, and turn the
LITERAL/DATA map into `Vec<RenderSpan>` by matching known control literals
inside LITERAL regions only. A new model costs a fixture corpus and a
control-token list. It does **not** cost a renderer. `crates/dialect-glm` is 1,319 hand-written
lines today (1,211 excluding its vendored sha256), not counting its tests.

`parse` is untouched by this. Templates render one direction; there is no
inverse. Whatever `parse` needed before, it still needs.

### The eight T2 defects, re-scored

| # | defect | under a template-driven dialect |
|---|---|---|
| 1 | `render_incremental(prev_end, new_items)` cannot be pure | **Disappears.** Jinja has no incremental mode: you render the whole conversation, every turn, for 7–15 µs. So there is no incremental renderer to carry boundary state through, and `GlmDialect::for_conversation` and the panicking `new()` both go away. The prefix relationship moves to the turn engine as a diff against the previously-submitted token stream — which is where the §4.3 prefix invariant already wants it, and which has to exist for cache safety regardless. |
| 2 | `parse(&[u32])` unimplementable as specified | **Survives unchanged.** Not a rendering question. Still needs a vocab; still needs the caller-supplied `TokenDecoder` or an equivalent. |
| 3 | no home for the generation prompt | **Disappears.** `add_generation_prompt` is a template argument, and the template itself owns whether and what to append. It becomes a parameter of `render`, not a separate method with an invariant to state. |
| 4 | `ControlRole` closed and too small | **Shrinks to a `parse`-only concern.** `render` no longer needs roles at all: the provenance map yields literals, and literals resolve to ids. `<arg_key>`, `<arg_value>`, `<sop>` and the image triplet stop needing a role, because nothing on the render path asks for one. The "no role" bucket under `TurnEnd` goes away. |
| 5 | resolution must key on literal, not role | **Disappears on the render path**, for the same reason. `ControlTokens::get(role)` is only reachable from `parse`, where one-role-many-literals is a much smaller problem, and the `GLM_TOKENS` ordering convention stops being load-bearing. |
| 6 | `stop_tokens()` returns bare `&'static str` with no role | **Survives.** The correctness argument — a stop token that is silently a *sequence* never fires, and the turn runs to `n_ctx` — is unaffected. The `&'static` half is fixed by #7. |
| 7 | `ControlTokens` as `&'static [ControlToken]` | **Becomes mandatory, not optional.** A template-driven dialect is loaded from a GGUF at runtime; `&'static str` is then impossible, not merely inconvenient. `Cow<'static, str>` (or `String`) is forced by the design rather than argued for. |
| 8 | `ControlRole` has no `Ord` | **Survives, smaller.** Still want deterministic listings; the enum it applies to is now much smaller. |

Three disappear (1, 3, 5), one shrinks to a different crate's problem (4), one
is forced (7), three survive (2, 6, 8). T2 is roughly halved, and — more to the
point — the half that survives is entirely on the `parse` side, which T1 does
not touch. **T2 can be unblocked now.**

### The one thing that gets harder

`crates/dialect-glm` today renders two profiles: `faithful` and
`server-bug-compatible`, and the gate requires them to disagree exactly where
minja's bug is. A template-driven renderer can produce `faithful` for free; it
**cannot** produce `server-bug-compatible` without reintroducing the bug
deliberately, and minijinja has no knob for "scope `set` wrongly". If that
profile is still wanted — it exists to model llama.cpp's behaviour for interop —
it has to become either a template rewrite (which changes the thing under test)
or a post-hoc transformation of the render. It is worth deciding whether it is
still wanted before it is rebuilt: the harness submits token ids and does not
route through minja at all.

---

## 8. What was **not** tested

Stated plainly, because it is the part a future reader needs.

* **Only two templates.** GLM-5.3-Flash and Qwen3.8-Flash-Next — which is all
  three target models, since Qwen3.8-27B ships a byte-identical template. Any
  further model can use constructs neither of these reaches.
* **`{% generation %}` was not exercised.** `transformers` registers an
  `AssistantTracker` for it and `oracle_hf.py` registers a pass-through, so a
  template using it renders under the oracle. **minijinja 2.24 has no public
  custom-tag API**, so such a template would fail to *parse* under it. Neither
  target template uses the tag. This is the most likely way a third model breaks
  the approach, and there is no shim for it.
* **`strftime_now` is deliberately unimplemented** in the harness (a clock in a
  fidelity harness is non-determinism). Neither template calls it. A template
  that does would need it, and would then need the two engines' `strftime` to
  agree on format specifiers — untested.
* **The sandbox was not tested.** `ImmutableSandboxedEnvironment` refuses
  mutating method calls. minijinja has no mutating methods to refuse, so the
  dangerous direction (renders under minijinja, raises under transformers) seems
  closed by construction — but that is an argument, not a measurement.
* **No tokenization.** Everything here compares strings and provenance maps.
  The downstream claim — `Text` tokenized with `parse_special` off, `Control`
  resolved to exact ids — is untested by this experiment. It is the existing
  `crates/tokencore` contract and is unchanged by T1.
* **No model, no server, no generation.** Nothing here says the prompts produce
  good output; it says they are the same bytes the training runtime produces.
* **The fuzzer fuzzes the data, not the template.** A mutated *template* would
  be a stronger probe of engine agreement and would likely find more shim items.
  If T1 is adopted, that is the natural next thing to build, and it is cheap.
* **Only one minijinja version** (2.24.0) and one CPython Jinja2 (3.1.6) /
  `transformers` reference (5.16.1). The shim is a claim about a pair of
  versions. Pinning both, and re-running the differential on any bump, is part
  of the price.

---

## 9. Files

| | |
|---|---|
| `src/bin/probe.rs`, `probe_cpython.py` | the three T1 semantics questions, side by side, on an **unshimmed** minijinja — they measure what it does as shipped |
| `src/bin/tests_probe.rs`, `tests_probe.py` | every `is` test × every value kind; found shim item 6 |
| `src/bin/none_probe.rs`, `none_probe.py` | how each engine renders `none` and `undefined` — they agree; the divergence was in `is iterable`, not in rendering |
| `src/bin/render.rs` | the minijinja environment and the shim, commented per item |
| `src/bin/shared_env.rs` | that environment, extracted so `bench.rs` uses the same one |
| `compare.py` | the differential: same arguments to both engines, four checks |
| `gen_cases.py` | the Qwen-native, numeric-edge and random corpora |
| `injection_check.py` | proves the provenance map is not vacuous |
| `src/bin/bench.rs` | §6 |
| `run.sh` | reproduces everything |

Corpora are generated, not committed; they are deterministic from their seeds.
