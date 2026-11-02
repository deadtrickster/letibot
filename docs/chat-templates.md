# Chat templates, from zero

A conversation is a list of messages; a model consumes a flat string of tokens. The **chat
template** is the program that turns one into the other. `tests/fidelity/README.md` is the
reference for the gate that checks our renderers.

## 1. What the thing is

A chat model was fine-tuned on text in one exact shape, so the program that produces that shape
is part of the model: a [Jinja](https://jinja.palletsprojects.com/) program shipped **inside the
GGUF** as `tokenizer.chat_template`, written by whoever published the model. For GLM, roughly:

```
[gMASK]<sop><|system|>Reasoning Effort: Max<|user|>what is 2+2<|assistant|><think>...
```

`<|user|>` and `<think>` are **single tokens** in the vocabulary, not punctuation.

```bash
python3 tests/fidelity/extract_template.py \
    ~/models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf > glm.jinja
wc -c glm.jinja      # 10648
```

### It is per template, not per model

| model | template sha256 (first 16) | bytes | in this tree |
|---|---|---|---|
| Qwen3.8-Flash-Next | `12827f24b742ea4e` | 9,993 | `crates/dialect-qwen/template/qwen3.8-flash-next.jinja` |
| Qwen3.8-27B | `12827f24b742ea4e` | 9,993 | the same file |
| GLM-5.3-Flash | `a4fddbbf0b432101` | 10,648 | `crates/dialect-glm/template/glm-5.3-flash.jinja` |

The two Qwen models ship a **byte-identical** template, so they are one dialect: three target
models, two templates. `DialectSpec::template_sha` is hashed from the embedded file, so a
template change invalidates the dialect.

## 2. One program, two interpreters

`transformers` renders the template with **CPython Jinja2** - the code path that produced the
training data, so it defines what correct means. llama.cpp cannot embed Python and renders with
**minja**, a from-scratch C++ reimplementation.

### The divergence we measured

GLM's template contains this, inside the loop over messages:

```jinja
{%- if m.reasoning_content is string %}
    {%- set reasoning_content = m.reasoning_content %}
{%- endif %}
{%- if (...) and reasoning_content is defined -%}
{{ '<think>' + reasoning_content + '</think>' }}
```

A bare `{% set %}` inside a `{% for %}` is scoped to the iteration in Jinja2; minja keeps it
across iterations, so an assistant turn with no reasoning of its own inherits the **previous**
turn's thinking:

```
CPython Jinja2 :  <|assistant|><think></think>second answer
minja          :  <|assistant|><think>LEAK-CANARY…</think>second answer
```

The template's author settles which is right: the same file uses `namespace()` seven times, which
exists in Jinja to make a value survive an iteration. minja is wrong, and the consequence for
anything talking to llama.cpp with `messages` is that the model is conditioned on reasoning it
never produced - on every assistant turn that did not itself reason.

Our authority is `oracle_hf.py` (CPython Jinja2 as `transformers` configures it), and
`/apply-template` is a reporting interop diff.

## 3. Why this connects to the prompt cache

llama.cpp reuses a cached prompt only when the new prompt is a **strict prefix extension** of
it, so anything that makes turn N's replay differ from what was cached kills the cache from that
point. The leak above is one such thing and cannot arise here, because we never hand llama.cpp a
`messages` array. Replaying reasoning as its own message is refuted for opencode
(`docs/evidence/opencode-reasoning-2026-09-11.md`).

The gate checks the prefix property at **every prefix boundary**: a renderer that is right about
a whole conversation and wrong about its third prefix has a cache that never hits.

## 4. The injection problem, and why it decides the shape of the code

The tokenizer has a flag, `parse_special`: **on** makes `<|assistant|>` in the text the assistant
control token, **off** makes it ordinary characters. Neither is right for a whole string - with
it on, a user who types `<|assistant|>` forges a turn boundary the harness never wrote; with it
off, the template's own control tokens arrive as literal text and the model sees no turn
structure.

So `RenderSpan` in `crates/dialect` is `Text | Control` rather than a string: `Text` is
tokenized with `parse_special` **off**, `Control` resolves to an exact id, and neither path can
produce the other's output.

**Byte equality cannot check it.** In the `adversarial-control-literals` fixture the bytes
`<|assistant|>` appear four times - twice from the user, twice from the template - so a renderer
that promoted the user's copy would produce an identical prompt.
Provenance notices: the gate classifies every output byte as LITERAL (template source) or DATA
(substituted payload) and fails if any `Control` span covers a DATA byte - 2,033 control spans
across the corpus, none from data. The map comes from wrapping the **data**, not the template:
sentinel characters, stripped back and compared byte for byte, so a template that slices or
measures the payload voids it and the oracle raises. `tests/fidelity/README.md` §4 has the
mechanics.

## 5. Reproducing the training runtime is not free

| needed | what happens without it |
|---|---|
| `jinja2.ext.loopcontrols` | `unknown tag 'break'` - GLM's template uses `{% break %}` |
| `trim_blocks=True`, `lstrip_blocks=True` | whitespace differs throughout |
| a custom `tojson` accepting `ensure_ascii` | `TypeError` - stock Jinja2's `tojson` has no such kwarg |
| `ImmutableSandboxedEnvironment` | works, but is not what transformers runs |
| `raise_exception`, `strftime_now` globals | templates that call them fail |

## 6. Where letibot stands

- The harness renders and submits **token ids** to `/completion`, so minja is not in our runtime
  path at all.
- Two dialects are wired in `crates/harnessd/src/dialect.rs`, each a hand-written renderer: GLM
  (`GlmRenderer`, gated at **139/139** cases against the training runtime) and Qwen3.8
  (`QwenRenderer`, **56/56** on its own corpus with one declared divergence,
  `crates/dialect-qwen/fidelity.py`). The template is the **authority**, not the runtime.
- There is **one profile**, `faithful`. The `server-bug-compatible` profile, which reproduced
  minja's leak on purpose, is gone: running the real template through a correct engine is
  stronger evidence than reproducing a wrong one. A declared `divergence` is still checked in the
  interop phase, against the server itself.

## 7. Commands

```bash
# pull a template out of any GGUF
python3 tests/fidelity/extract_template.py <gguf> > t.jinja

# list a model's control tokens and their attributes
python3 tests/fidelity/extract_template.py --tokens <gguf>

# the authoritative gate: no server, no model, no GPU
python3 tests/fidelity/run_gate.py

# the Qwen corpus, same authority, its own driver
python3 crates/dialect-qwen/fidelity.py

# the secondary, interop-only diff against llama.cpp's minja
bash tests/fidelity/serve_oracle.sh start && python3 tests/fidelity/run_gate.py --interop
bash tests/fidelity/serve_oracle.sh stop
```
