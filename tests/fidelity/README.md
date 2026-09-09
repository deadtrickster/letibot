# The renderer-fidelity gate

This directory answers one question, and it is the question that makes "we own the
renderer" a safe thing to say:

> When our Rust code turns a conversation into a prompt, does the model see the
> format it was trained on — and can anything a user or a tool wrote turn into a
> control token on the way?

Two questions really. The first is byte equality. The second is *provenance*, and it
is the one that keeps you up at night.

---

## 1. Background: a chat template is a program, and there are two interpreters

A conversation is a list of messages. A model does not consume a list; it consumes a
flat string of tokens. The thing that converts one into the other is the **chat
template** — a [Jinja](https://jinja.palletsprojects.com/) program shipped *inside*
the model file, under the GGUF key `tokenizer.chat_template`. GLM's is 10,648 bytes
of it; you can read ours at `crates/dialect-glm/template/glm-5.3-flash.jinja`, and
pull a fresh one out of any GGUF with `extract_template.py`.

So "render a conversation" means "run this program". And here is the thing that this
whole directory grew out of:

**There is more than one Jinja, and they do not agree.**

| | what it is | where it runs |
|---|---|---|
| **CPython Jinja2** | the original, in Python, driven by HuggingFace `transformers` | the machine that produced the model's **training data** |
| **minja** | llama.cpp's from-scratch C++ reimplementation | llama.cpp's `/apply-template` and `/v1/chat/completions`, at **serving** time |

A model does not "understand English messages". It learned that a conversation looks
like a very specific byte sequence, and it learned that from text rendered by CPython
Jinja2. Feed it a byte sequence that differs — even slightly, even sensibly — and you
are off the distribution it was trained on. Sometimes that costs nothing. Sometimes it
costs a tool call.

### The divergence we measured

GLM's template contains this, **inside** `{% for m in messages %}`:

```jinja
{%- if m.reasoning_content is string %}
    {%- set reasoning_content = m.reasoning_content %}
{%- endif %}
...
{%- if ... and reasoning_content is defined -%}{{ '<think>' + reasoning_content + '</think>' }}
```

A bare `{% set %}` inside a `{% for %}` is **scoped to the iteration** in CPython
Jinja2: when the loop moves to the next message, `reasoning_content` is undefined
again. minja keeps it alive across iterations. The consequence, on the same input:

```
minja  : …<think>R1</think>one<|user|>b<|assistant|><think>R1</think>two…
Jinja2 : …<think>R1</think>one<|user|>b<|assistant|><think></think>two…
                                              the previous turn's reasoning ↑
```

An assistant turn with no reasoning of its own is served carrying **the previous
turn's `<think>` block**. It is not a corner case: llama.cpp drops `reasoning_content`
when it is the empty string, so a genuinely empty think block is indistinguishable
from an absent one and inherits the older text.

Which one is right? The template's own author tells you. That same template uses
`namespace()` **seven times** (`ns`, `ns_tool`, `ns_blk`, `ns_chk`, `ns_a`, `ns_cnt`,
`ns_f`) — and the *only* reason `namespace()` exists in Jinja is to make a value
survive a loop iteration. Bare `set` where they wanted per-iteration; `namespace`
where they wanted carry-over. The intent is unambiguous. minja is wrong.

### Why that made `/apply-template` the wrong authority

This gate used to diff our renderer against llama.cpp's `POST /apply-template`. It was
free, it was one HTTP call, and it was measuring the wrong thing:

1. **minja is not in our runtime path.** The harness tokenizes `RenderSpan`s itself and
   submits **token ids** to `/completion`. llama.cpp's renderer never runs on our
   traffic. Conformance to it measures agreement with a bug we already route around.
2. **minja is not in the model's history either.** The model's notion of a correctly
   formatted conversation was fixed during training, by CPython Jinja2.

So the authority is now `oracle_hf.py`: CPython Jinja2, configured exactly the way
`transformers` configures it. The `/apply-template` diff survives, **demoted**: it
answers "how would a stock llama.cpp server render this", which is genuine interop
information and is not a statement about correctness. It reports; it does not gate.

The happy consequence is that the authoritative gate needs **no GPU, no model weights,
no server and no port**. It needs the template text and `jinja2`.

---

## 2. What is in here

```
tests/fidelity/
  README.md              this
  oracle_hf.py           THE AUTHORITY. transformers' Jinja2 environment + provenance
  test_oracle_hf.py      tests for the oracle. run_gate runs them first
  run_gate.py            the gate: four phases, three of which can fail it
  fixtures/*.json        the corpus
  extract_template.py    tokenizer.chat_template out of a GGUF; --tokens for the vocab
  serve_oracle.sh        brings up llama.cpp for the (optional, non-gating) interop diff
```

### Running it

```bash
tests/fidelity/run_gate.py              # the whole authoritative gate. ~10s. Nothing else needs to be alive.
tests/fidelity/run_gate.py -v           # every case
tests/fidelity/run_gate.py --only image # one fixture
```

That is the complete command. **No server, no model, no GPU, no free port.** It is safe
to run on a box that is busy serving something else, which was not true of the old
gate. Only dependency is `jinja2` >= 3.1 (`python3-jinja2` on Debian).

For the demoted interop diff, and only then, you need llama.cpp:

```bash
tests/fidelity/serve_oracle.sh start    # its own port (8137), CPU, substitute model
tests/fidelity/run_gate.py --interop
tests/fidelity/serve_oracle.sh stop     # do not leave it running
```

`serve_oracle.sh` exists because `/apply-template` is **proxied in router mode**
(`server.cpp:239`), so pointing this at the router tests the router. Its header
documents exactly what the substitute model does and does not change.

---

## 3. The four phases

`run_gate.py` runs four things. The first three gate; the fourth reports.

### Phase 0 — the oracle's own self-test

`test_oracle_hf.py`, 19 tests, run in a subprocess **before anything else**. The oracle
judges the renderer, and nothing judges the oracle, so this does. Running it first
means a broken oracle reports itself instead of blaming the renderer and sending you
to fix the wrong file.

### Phase 1 — AUTHORITY: our render == the training runtime

```
for each fixture F, for each prefix P of F, with and without the generation prompt:
    assert spans_to_string(faithful.render(P)) == oracle_hf.render(P)
```

Byte-for-byte, no exceptions, no normalisation. **139 cases, all exact.** A difference
here is ours, always: what CPython Jinja2 produces *is* the format the model was
trained to recognise, so there is no version of "the oracle is wrong" available.

Every prefix, not just the whole conversation — §7.2's "truncated at every prefix
boundary". A renderer that is right about a whole conversation and wrong about its
third prefix is a renderer whose KV cache never hits.

### Phase 2 — PROVENANCE: no control token may come from data

Explained in full in §4 below, because it is the part that does real work.

### The PROFILES phase, and why it is gone

There used to be a third gating phase. `--profile server-bug-compatible` rendered the
corpus again with `GlmQuirks::reasoning_leak` on — a deliberate reproduction of minja's
bug — and the gate required it to differ from the oracle exactly on the fixtures that
declare a divergence.

T2 removed it. Its purpose was to prove we understood the shipped template well enough
to reproduce llama.cpp's renderer exactly, which is what earned `faithful` the standing
to call a difference a divergence. Under T1 prompts are rendered by running the real
template through a correct Jinja engine, and that is stronger evidence than reproducing
a wrong one — while keeping the quirk would mean carrying a reimplementation of someone
else's bug forever. `--profile` survives with one value, and rejects the removed one
with a message rather than treating it as a filename.

The `divergences` declarations did not become decoration with it: they moved into
INTEROP, where they are checked against the server itself instead of against our model
of it.

### Phase 3 — INTEROP: how llama.cpp would differ. Reports only.

`--interop`, needs the server, **cannot fail the gate**. Currently 134/139 identical,
differing on exactly the 5 `reasoning-leak` cases — the prefixes short enough to have
no second assistant turn cannot leak, which is why a divergence is declared per fixture
and not per case.

This phase also reports a fixture that declares a divergence the server no longer
produces, and one that declares none and diverges anyway. That is the two-sided check
the PROFILES phase used to run, one hop closer to the thing being described. It does
not gate, because it needs a server, and a check that only runs when somebody
remembered to start one must not be what says a render is correct.

Demoting this is not discarding it. The day someone points this harness at a stock
llama.cpp chat-completions endpoint instead of submitting token ids, it becomes
load-bearing again.

---

## 4. Provenance: the part that does real work

### The problem

`crates/dialect` splits a rendered prompt into `RenderSpan::Text` and
`RenderSpan::Control` rather than producing one flat string. The reason is a security
one, and the crate docs put it bluntly:

> **A user message containing the literal text `<|assistant|>` must never become the
> assistant control token.**

If the renderer produced one flat string and it were later tokenized with
special-token parsing enabled, it would — and the model would see a turn boundary the
harness never wrote. Someone pasting a log, a tool returning a document, an MCP server
describing its own tools: any of them can spell `<|assistant|>`. The split makes the
promotion *structurally impossible*: `Text` is tokenized with special-token parsing
**off**, `Control` is resolved to exact ids by the token core, and neither path can
produce the other's output.

But that is only a guarantee if the renderer puts the right bytes in `Text` in the
first place. Until now that was a hand-written property: a human read the renderer and
believed it. Byte equality cannot check it — **the two possibilities render to
identical strings.** That is the whole difficulty in one sentence.

### The solution: classify every output byte by origin

For each byte of the rendered prompt, decide:

* **LITERAL** — it came from the template source. It *may* be a control token.
* **DATA** — it was substituted in from message or tool payload. It must *never* be.

Then the check is mechanical: **no `RenderSpan::Control` may cover a DATA byte.**

Here is the `adversarial-control-literals` fixture, rendered and classified. The bytes
`<|assistant|>` occur four times and are the same four characters every time:

```
byte  64  <|assistant|>   DATA      the user typed it
byte 170  <|assistant|>   LITERAL   the template emitted it
byte 255  <|assistant|>   DATA      the assistant echoed it back
byte 307  <|assistant|>   LITERAL   the generation prompt
```

A renderer that promoted byte 64 to a control token would produce a **byte-identical**
prompt and pass phase 1. Provenance is what notices. Currently: 2,033 control spans
checked across the corpus, zero originating in DATA.

### How the map is built: wrap the data, not the template

The obvious approach is to instrument the template — parse the Jinja, wrap every
`{{ }}` that emits payload. Do not do this. A rewritten template is no longer the
thing under test, and Jinja is a real language with macros, includes and filters.

Instead the template is left **completely untouched** and the *inputs* are marked:

1. **Pick two sentinel characters** from the Unicode Private Use Area that appear
   nowhere in this request. Adaptive, not fixed — see "adversarial input" below.
2. **Wrap every payload string** in that pair.
3. **Render twice**: once with the clean payload, once with the wrapped payload.
4. **Self-validate.** Deleting the sentinels from the wrapped render must equal the
   clean render, byte for byte.
5. **Split on the sentinels.** Inside a pair is DATA; outside is LITERAL.

**Step 4 is the entire reason to believe any of this.** If the stripped render does not
reproduce the clean one, the template did something to the payload the map does not
model — sliced it, measured it, compared it, escaped it — and the sentinels did not
merely ride along. In that case there is no map, and `oracle_hf.py` **raises**. It does
not warn and it does not fall back to "assume LITERAL". A provenance map that is
allowed to be approximately right is worse than none, because it will be trusted.

Two negative controls in `test_oracle_hf.py` prove step 4 can still say no: a template
that slices the payload (`content[:4]` — a sentinel is cut off) and one that measures
it (`content | length` — both sentinels survive, but the output is *derived from*
rather than *containing* the payload, so only the byte-for-byte comparison catches it).

### What gets wrapped, and what deliberately does not

| input | classified | why |
|---|---|---|
| message `content`, `reasoning_content` | DATA | obviously payload |
| multi-part content: `text`, `image_url.url` | DATA | payload |
| tool call `id`, `name` | DATA | the model chooses these |
| tool call **argument keys** | DATA | GLM prints them into `<arg_key>{{ k }}</arg_key>` |
| tool call argument values, nested | DATA | wrapped recursively through `tojson` |
| **tool schema `name`, `description`, `parameters`** | DATA | **see below** |
| `role`, `type`, `defer_loading`, `strict` | LITERAL | discriminators, not payload |
| dict keys at structural positions | LITERAL | see the residual gap |
| numbers, booleans, null | LITERAL | see below |

**Tool schemas.** The first cut of this left them unwrapped, on the reasoning that a
tool schema is written by the developer. That reasoning expires the moment a tool comes
from an MCP server the user added this morning: its `description` is remote text
rendered into the **system prompt**, which is the single most valuable place in a
prompt to land an injected control token. They are wrapped now.

**Discriminators are not wrapped, and this is structural rather than a judgement
call.** The template looks fields up by name (`m.role == 'user'`, `'function' in tool`,
`item.type == 'text'`). Wrapping those would change *control flow* — it would change
the render rather than annotate it — and step 4 would then fail, loudly, giving you a
failure instead of a map. So they are left alone by construction.

**Numbers and booleans are not wrapped**, and that is a statement rather than an
omission. They cannot be wrapped without changing their type and therefore the render.
They do not need to be: `json.dumps` renders them from the alphabet
`[-+0-9.eE]`/`true`/`false`/`null`, and every control token in every dialect we support
is delimited by `<`, `[` or `|`. A JSON number cannot spell one.
`oracle_hf.check_scalar_safety()` encodes that argument as a test, so if a future
dialect ever adopts a bare-alphanumeric control token, this stops being a paragraph of
prose and starts being a red test.

**Adversarial input: a user who sends our sentinels.** The naive design fixes the
sentinel pair and strips it out of incoming text — which means the oracle silently
rewriting the very input it is supposed to be faithful to, and an attacker who guesses
the pair gets to punch holes in the map. Instead `pick_sentinels()` scans the request
and chooses a pair the payload does not use (the Private Use Area has 6,400 codepoints
in the BMP alone). PUA characters in user text survive verbatim **and** are correctly
classified as DATA. There is a test.

**One refinement worth knowing about, because it looks like a hack and is not.** GLM
calls `.strip()` on assistant content. A sentinel is not whitespace, so
`(SENT + "  hi  " + SENT).strip()` is unchanged while `"  hi  ".strip()` is `"hi"` —
the two renders diverge and step 4 correctly voids the map. So `_wrap_str` puts the
sentinels *inside* the padding: `"  " + SENT + "hi" + SENT + "  "`. The wrap then
survives `.strip()` on both sides. The cost is that leading and trailing whitespace of
a payload string is classified LITERAL, which is a real and harmless loss of precision:
a run of whitespace cannot spell a control token. Measured: without this, the
`whitespace` fixture loses its map in 3 of its cases. There is a test that fails if the
template stops calling `.strip()`, so the refinement cannot outlive its reason.

### Known residual gap

Dictionary **keys at structural positions** — including the top-level keys of a tool
dict — are classified LITERAL, because the template looks them up by name. Almost all
of them are fixed vocabulary (`role`, `content`, `name`, `parameters`), but a
sufficiently strange MCP server controls some. `oracle_hf.literal_data_keys()` returns
the set actually present so the gap is visible rather than forgotten. Below an "opaque"
boundary — a tool's `parameters` schema, a tool call's `arguments` — keys *are* wrapped,
because the template never indexes into those and does print them.

---

## 5. Matching `transformers` exactly

`oracle_hf.build_env()` is a line-for-line copy of
`transformers/utils/chat_template_utils.py::_cached_compile_jinja_template`, verified
against **transformers 5.16.1** by installing it in a throwaway venv and reading the
function (2026-09-09). The oracle itself does **not** import transformers — it needs
only `jinja2`, which is the point. `oracle_hf.TRANSFORMERS_REFERENCE` records the
version; bump it only after re-reading that function.

Every element is load-bearing, and all but one fail *silently* if omitted:

| element | what breaks without it |
|---|---|
| `ImmutableSandboxedEnvironment` | a plain `Environment` accepts mutating templates that the real runtime rejects — you can pass a template production refuses |
| `trim_blocks=True, lstrip_blocks=True` | every `{% %}` on its own line leaves a stray newline. The prompt still looks plausible; it is just not what the model saw |
| `extensions=[jinja2.ext.loopcontrols]` | GLM's `{% break %}` is rejected as `unknown tag 'break'`. The one loud failure |
| a **replacement `tojson`** | the subtle one, see below |
| `raise_exception`, `strftime_now` globals | templates call them; `strftime_now` takes one positional `format` |

The `tojson` trap deserves its own paragraph. Stock Jinja2's filter is
`do_tojson(eval_ctx, value, indent=None)`: it takes **no `ensure_ascii` argument** and
it **HTML-escapes** `<`, `>` and `&`. GLM calls `{{ v | tojson(ensure_ascii=False) }}`,
which under stock Jinja2 is a `TypeError` — and the natural "fix" of deleting the
argument lands you in the escaping, where every tool schema containing an angle bracket
is quietly mangled and the render still looks like a prompt. `transformers` replaces
the filter with a thin `json.dumps` wrapper. So do we. There is a test that fails if
stock Jinja2 ever grows the kwarg.

One thing we deliberately do **not** copy: transformers' `AssistantTracker` extension,
which implements `{% generation %}` for training-time token masks. We do not want the
masks — but the tag's presence changes whether a template *parses*, so `build_env()`
registers a pass-through `generation` tag that renders identically and collects
nothing. Neither GLM's nor Qwen's template uses it today.

---

## 6. The fixture format

One JSON file per fixture. It stores **the transcript**, not the prompt and not the
request body — those are derived, so there is nothing to keep in sync by hand.

```jsonc
{
  "name": "reasoning-two-tool-calls",
  "why": "Why this shape is in the corpus, and what breaks if it is not.",
  "dialect": "glm-5.3-flash",
  "reasoning_effort": "low",           // optional; omit for the default (Max)

  "prefix": {                          // the StablePrefix: what the KV cache is keyed on
    "system": "You are a coding assistant.",
    "tools":  [ { "type": "function", "function": { … } } ]   // OpenAI-shaped
  },

  "items": [ … ],                      // TranscriptItem, exactly as serde serialises it

  "divergences": [                     // where llama.cpp's renderer differs from the
    { "id": "reasoning-leak", "why": "…" }   // training runtime. Checked by INTEROP.
  ],
  "normalise": [                       // where byte equality is not AVAILABLE against
    { "id": "media-marker", "why": "…" }     // llama.cpp. Interop phase only.
  ]
}
```

`items` is the transcript crate's own serde form, so a fixture is something you can
paste out of a real session:

```jsonc
{"type":"user","parts":[{"kind":"text","text":"Compare a.txt and b.txt."}]}
{"type":"reasoning","text":"Read both, then diff.","field":"reasoning_content"}
{"type":"assistant","text":"","tool_calls":[{"id":"c1","name":"read","arguments":"{\"path\":\"a.txt\"}"}]}
{"type":"tool_result","call_id":"c1","name":"read","outcome":"ok","payload":"alpha\n"}
{"type":"segment_mark","segment_id":"s1","label":"opening","kind":"task","edge":"open"}
```

**Tools are stored OpenAI-shaped, not GLM-shaped.** `StablePrefix.tools_json` holds
finished text, and producing that text is itself a thing that can be wrong. The
conversion lives in `letibot_dialect_glm::glm_tool_json` and the fixture goes through
it, so the gate covers the conversion as well as the render. Whatever fills
`tools_json` in production must call the same function.

### `divergences` and `normalise` after the demotion

Both now describe **llama.cpp**, not us. Our renderer has no declared divergence from
the authority — that is the point of the move, and phase 1 admits no exceptions at all.

* **`divergences`** — the two engines genuinely disagree. Currently one:
  `reasoning-leak`. Two-sided checked by the interop phase: a declared divergence the
  server stops producing is reported, and so is an undeclared one that appears.
* **`normalise`** — byte equality against llama.cpp is not *available*. Currently one:
  `media-marker`, where llama.cpp substitutes `<__media_NONCE__>` with a nonce
  regenerated per server process, so no renderer could reproduce it. **The HF oracle
  needs no normalisation at all** — it runs the template's own `emit_image()` macro and
  produces the real tokens — so this rule exists only in the interop phase, and it is
  still two-sided there: the raw comparison is tried first, and a rule that never fires
  anywhere is reported, because an unnecessary rewrite is a place a real difference
  could hide.

---

## 7. How a dialect is invoked from Python

It is not. Python never sees a `TranscriptItem`.

```
letibot-render --dialect glm-5.3-flash --profile faithful FIXTURE.json [FIXTURE.json …]
```

prints a JSON array of **cases** to stdout. Everything that needs to understand the
transcript — expanding a fixture into its prefix truncations, mapping items onto
OpenAI messages, deciding whether a generation prompt belongs on the end — happens
there, in Rust. The runner reads cases and compares.

```jsonc
{
  "fixture": "reasoning-two-tool-calls",
  "prefix_len": 4,                      // items[..4] were rendered
  "add_generation_prompt": false,
  "rendered": "[gMASK]<sop><|system|>Reasoning Effort: Max…",
  "spans": [ {"control":"[gMASK]"}, {"text":"…"} ],   // phase 2 checks these
  "request": { "messages": [...], "tools": [...], "add_generation_prompt": false },
  "divergences": ["reasoning-leak"],
  "normalise": []
}
```

Note that `spans` is no longer "for eyeballing". Phase 2 checks it against the
provenance map, so the span list must be an exact partition of `rendered` — the gate
says so if it is not.

One shape conversion happens inside `oracle_hf._adapt_request`: on the wire a tool
call's `arguments` is a **JSON string**, but a chat template expects an **object**
(GLM iterates it with `.items()`, and a `str` has no `.items()`). llama.cpp performs the
same parse before handing messages to minja, which is why the old string-form gate
worked. We do it explicitly so the conversion is visible rather than buried in a server.

### What one fixture expands into

Every prefix, and each prefix twice:

* `items[..k]` for every `k` in `0..=len`.
* with and without the generation prompt, except where the prefix ends mid-assistant
  turn and there is no generation prompt to add. `<|assistant|><think>` is the string
  every turn starts with, so it is checked on its own rather than assumed.

14 fixtures currently expand to 139 cases.

---

## 8. What the corpus covers

| fixture | shape |
|---|---|
| `plain-turn` | the floor: one user message, one reply |
| `assistant-reasoning` | reasoning replayed as `reasoning_content` |
| `reasoning-two-tool-calls` | §7.2's headline: reasoning, two calls, two results, a second turn |
| `tool-outcomes` | `Abstained` and `Timeout` — abstention is not success (§8.2) |
| `empty-assistant-turn` | the `content: ""` case |
| `system-mid-history` | a system message at index > 0 → `SystemUpdateMode::InHistory` |
| `adversarial-control-literals` | user text spelling `<|assistant|>`, `<think>`, `<arg_key>` — **the provenance case** |
| `argument-types` | every JSON value kind through `<arg_value>`, incl. unicode |
| `segment-marks` | `SegmentMark` renders to nothing, and splits no observation block |
| `whitespace` | assistant content is stripped, user content is not — **the `_wrap_str` case** |
| `reasoning-effort-low` | the effort line at position 3, which re-prefills everything |
| `long-turn` | a turn long enough to cross any internal limit |
| `reasoning-leak` | the declared engine divergence |
| `image` | an image part; needs no normalisation against the HF oracle |

### Adding a fixture

1. Write the transcript into `fixtures/<name>.json`, and write `why`. A fixture
   without a reason to exist is a fixture nobody will dare delete.
2. `tests/fidelity/run_gate.py --only <name> -v`.
3. If phase 1 fails, **the renderer is wrong.** There is no "declare a divergence"
   escape from the authority, and there should not be.
4. If phase 2 fails, a control token is being built out of conversation data. Stop and
   read it carefully; that is the failure this whole directory exists to catch.

---

## 9. Adding a dialect

`TEMPLATES` in `run_gate.py` maps a dialect name to its shipped jinja. Two dialects
cover our three target models:

| template sha256 (prefix) | bytes | models |
|---|---|---|
| `a4fddbbf0b432101` | 10,648 | GLM-5.3-Flash |
| `12827f24b742ea4e` | 9,993 | Qwen3.8-Flash-Next **and** Qwen3.8-27B — byte-identical |

The oracle has been checked against the Qwen template as well as GLM's: it renders,
the provenance map self-validates, tool descriptions land in DATA, and a user-typed
`<|im_start|>` is DATA while all six of the template's own are LITERAL. The Rust side
of that dialect does not exist yet; when it lands, `--dialect` is already the switch.

`letibot-render` lives in `dialect-glm` today only because that is the crate this
strand owns, and should move out when Qwen lands.

## 10. When the model ships a template change

`template_sha` stops matching (`letibot_dialect_glm::template_sha` is computed from the
embedded jinja, not pasted beside it), the spec is stale, and this gate reports which
fixtures moved. Adopting the change forks every live transcript on that model (§5.5),
because the render changed — which is correct, and which should be visible.

```bash
tests/fidelity/extract_template.py <gguf> > crates/dialect-glm/template/glm-5.3-flash.jinja
cargo test -p letibot-dialect-glm     # template_sha_is_the_hash_of_the_shipped_jinja fails first
tests/fidelity/run_gate.py            # then this says what actually moved
```
