# Chat templates, from zero

Written 2026-09-09 because this became load-bearing for letibot in a single morning
and the operator had not met the artifact before. Everything here was measured on
this box; the commands are runnable.

---

## 1. What the thing is

A base language model completes text. A *chat* model has been fine-tuned on text in
one exact shape — special tokens marking turn boundaries, a specific place for tool
schemas, a specific wrapper around reasoning. Feed it a different shape and nothing
errors: it just gets quietly worse, because it is seeing something it was not
trained on.

The **chat template** is the program that produces that exact shape. It is a
[Jinja](https://jinja.palletsprojects.com/) program, and it ships **inside the
GGUF** as metadata under `tokenizer.chat_template`. It is written by whoever
published the model, not by llama.cpp and not by us.

```bash
python3 tests/fidelity/extract_template.py \
    ~/models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf > glm.jinja
wc -c glm.jinja      # 10648
```

Input is a list of `{role, content}` messages plus optional tools. Output is one
string. For GLM, roughly:

```
[gMASK]<sop><|system|>Reasoning Effort: Max<|user|>what is 2+2<|assistant|><think>...
```

Those `<|user|>` and `<think>` pieces are **single tokens** in the vocabulary, not
punctuation. That matters later.

### It is per template, not per model

Measured here:

| model | template sha256 (first 16) | bytes |
|---|---|---|
| Qwen3.8-Flash-Next | `12827f24b742ea4e` | 9,993 |
| Qwen3.8-27B | `12827f24b742ea4e` | 9,993 |
| GLM-5.3-Flash | `a4fddbbf0b432101` | 10,648 |

The two Qwen models ship a **byte-identical** template, so they are one dialect.
Three target models, two templates. `Dialect::template_sha()` exists so this is
answered by hashing rather than by assumption, and so a model update that changes
the template invalidates the dialect automatically.

---

## 2. The problem: one program, two interpreters

`transformers` (HuggingFace) renders the template with **CPython Jinja2**. That is
the code path that produced the training data, so it is the definition of correct.

llama.cpp cannot embed Python, so it renders with **minja** — a from-scratch C++
reimplementation of Jinja by the llama.cpp authors.

Same program, two interpreters, and they do not agree everywhere.

### The instance we hit

GLM's template contains, inside the loop over messages:

```jinja
{%- if m.reasoning_content is string %}
    {%- set reasoning_content = m.reasoning_content %}
{%- endif %}
{%- if (...) and reasoning_content is defined -%}
{{ '<think>' + reasoning_content + '</think>' }}
```

A bare `{% set %}` inside a `{% for %}` is **scoped to the iteration** in Jinja2.
minja keeps it across iterations. So an assistant turn with no reasoning of its own
inherits the **previous** turn's thinking:

```
CPython Jinja2 :  <|assistant|><think></think>second answer          ← correct
minja          :  <|assistant|><think>LEAK-CANARY…</think>second answer
```

The author of the template knew the scoping rule — they used `namespace()`
elsewhere in the same file precisely to *escape* per-iteration scoping. So the
intent is unambiguous and minja is wrong.

It is invisible in normal use because llama.cpp drops `reasoning_content` when it is
the empty string, which makes "empty think block" and "absent think block"
indistinguishable downstream.

**Consequence for anything talking to llama.cpp with `messages`:** the model is
conditioned on reasoning it never produced, on every assistant turn that did not
itself reason — which in an agentic tool loop is most of them.

---

## 3. Why this connects to the prompt cache

llama.cpp caches `prompt ++ generated` tokens and reuses an entry only when the new
prompt is a **strict prefix extension** of it. Anything that makes turn N's replay
differ from what was cached at turn N kills the cache from that point on.

The template gives you two ways to do that without noticing:

1. **The leak above.** The cache holds what the model generated; the replay holds
   the previous turn's thoughts injected by minja. They differ.
2. **Replaying reasoning.** Verified 2026-09-11 against the client itself
   (`docs/evidence/opencode-reasoning-2026-09-11.md`): opencode stores reasoning
   as separate parts but the OpenAI-compatible converter
   (`OpenAIChat.lowerAssistantMessage` in the binary) fuses every part of an
   assistant turn into one `reasoning_content` field on that same assistant
   message. Reasoning is never sent as its own message, so the feared extra turn
   marker cannot arise from message structure. This mechanism is REFUTED for
   opencode.

This is the residual behind the divergence we chased with `interleaved`. That fix
was real and addressed the primary cause (reasoning missing from the replay
entirely, `f_keep` p10 0.000 -> 0.999). With mechanism 2 refuted (above), the
residual — p99 re-prefill settling at 5,294 tokens rather than at zero — is
mechanism 1 or a third, unidentified cause.

---

## 4. The injection problem, and why it decides the design

After rendering you have one flat string. Now you must turn it into tokens. The
tokenizer has a flag, `parse_special`:

- **on** — `<|assistant|>` in the text becomes the assistant *control token*
- **off** — it becomes ordinary text characters

Neither is right for a whole string, and that is the trap:

- With it **on**, a user who types `<|assistant|>` in their message forges a turn
  boundary the harness never wrote.
- With it **off**, the template's own control tokens arrive as literal text and the
  model sees no turn structure at all.

A string cannot tell you which is which, because the information — *where did these
bytes come from* — was thrown away when the template produced a string.

This is why `RenderSpan` in `crates/dialect` is `Text | Control` rather than a
string, and why `Text` is tokenized with `parse_special` **off** while `Control`
resolves to an exact id. The distinction is structural: neither path can produce the
other's output.

### Recovering provenance without hand-writing a renderer

The information is not really lost — it is in the template's *execution*. Control
tokens appear as **literal text in the template source**; user content only ever
arrives through `{{ substitution }}`. Verified on GLM: every control token is a
literal, and user content reaches the output only via `{{ visible_text(m.content) }}`
and `{{ content.strip() }}`.

So: **wrap the data, not the template.** Put a sentinel character around every
string that comes from message data, render, and split the output on the sentinels.
Inside a pair is data; outside is template literal.

It self-validates, which is what makes it trustworthy rather than clever: stripping
the sentinels must reproduce the un-instrumented render byte-for-byte. If it does
not, the provenance map is void and the render must be rejected. That is one string
comparison per turn, and it runs in production, not only in tests.

Measured on GLM with tools, `tojson`, tool calls and adversarial content — 7
sentinel pairs, stripped back exactly, and a user-supplied
`has "quotes" and <|user|>` inside a tool argument correctly classified as data.

---

## 5. Reproducing the training runtime is not free

Every one of these is required to render GLM's template the way `transformers`
does, and each omission is silent or fatal rather than obvious:

| needed | what happens without it |
|---|---|
| `jinja2.ext.loopcontrols` | `unknown tag 'break'` — GLM's template uses `{% break %}` |
| `trim_blocks=True`, `lstrip_blocks=True` | whitespace differs throughout |
| a custom `tojson` accepting `ensure_ascii` | `TypeError` — stock Jinja2's `tojson` has no such kwarg; transformers replaces the filter |
| `ImmutableSandboxedEnvironment` | works, but is not what transformers runs |
| `raise_exception`, `strftime_now` globals | templates that call them fail |

"Just use the training runtime" is the right instinct and it is still a
configuration exercise where a wrong guess produces a wrong oracle, quietly.

---

## 6. Where letibot stands

- The harness renders the prompt itself and submits **token ids** to `/completion`.
  So minja is **not in our runtime path**, and agreement with `/apply-template`
  measures conformance to a bug we already routed around.
- The authority is therefore **CPython Jinja2 with provenance**, which needs no GPU,
  no model and no running server — only the template out of the GGUF.
- `crates/dialect-glm` is a hand-written renderer, verified byte-exact against that
  authority. It renders two profiles: `faithful` (the training format) and
  `server-bug-compatible` (reproduces minja's leak). Both must pass — one models the
  bug, one the training format, and they must disagree exactly where the bug is.
- Two independent implementations agreeing is stronger evidence than either alone.
  That is the current position, and `TODO.md` records what would replace it.

## 7. Commands

```bash
# pull a template out of any GGUF
python3 tests/fidelity/extract_template.py <gguf> > t.jinja

# list a model's control tokens and their attributes
python3 tests/fidelity/extract_template.py --tokens <gguf>

# the authoritative gate (no server needed)
python3 tests/fidelity/run_gate.py

# the secondary, interop-only diff against llama.cpp's minja
bash tests/fidelity/serve_oracle.sh start && python3 tests/fidelity/run_gate.py --minja
bash tests/fidelity/serve_oracle.sh stop
```
