# W3 — the renderer-fidelity gate

The plan's §7.2 check, and the thing that makes "we own the renderer" safe to say:

```
for each dialect D, for each fixture F in the corpus:
    assert spans_to_string(D.render(F)) == POST /apply-template(F).prompt
```

`/apply-template` runs the *same* `oaicompat_chat_params_parse` as
`/v1/chat/completions` (`server-context.cpp:7745`) and touches no slot, no sequence
and no GPU. It is a free oracle, so this runs on every commit.

`docs/workstreams.md` lists the corpus format as **partially specified**: §7.2 says
which shapes to cover and nothing about how a fixture is stored or how a dialect is
invoked from Python. That is what this file settles.

```
tests/fidelity/
  README.md              this
  fixtures/*.json        the corpus
  run_gate.py            the runner: renders, POSTs, diffs, exits non-zero
  serve_oracle.sh        brings up a directly-launched single-model server
  extract_template.py    tokenizer.chat_template out of a GGUF; --tokens for the vocab table
```

## Running it

```bash
tests/fidelity/serve_oracle.sh start        # substitute-model oracle, CPU, port 8137
tests/fidelity/run_gate.py                  # --addr to point elsewhere, -v for every case
tests/fidelity/serve_oracle.sh stop
```

**Never point it at the router.** `/apply-template` is proxied in router mode
(`server.cpp:239`), so the router tests the router. `serve_oracle.sh` launches a
single-model server on a port of its own; the header of that script documents exactly
what a substitute model does and does not change, and `MODE=real` uses the real GLM
server when the box has room for it.

## The fixture format

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

  "divergences": [                     // optional, see below
    { "id": "reasoning-leak", "why": "…" }
  ],
  "normalise": [                       // optional, see below
    { "id": "media-marker", "why": "…" }
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
finished text, and producing that text is itself a thing that can be wrong — the
server normalises a schema before the template sees it (it fills in
`description: ""`, drops the `function` wrapper, and fixes the key order). The
conversion lives in `letibot_dialect_glm::glm_tool_json` and the fixture goes through
it, so the gate covers the conversion as well as the render. Whatever fills
`tools_json` in production must call the same function.

## How a dialect is invoked from Python

It is not. Python never sees a `TranscriptItem`.

```
letibot-render --dialect glm-5.3-flash --profile faithful FIXTURE.json [FIXTURE.json …]
```

prints a JSON array of **cases** to stdout. Everything that needs to understand the
transcript — expanding a fixture into its prefix truncations, mapping items onto
OpenAI messages, deciding whether a generation prompt belongs on the end — happens
there, in Rust. The runner reads cases, POSTs `request`, and compares strings.

```jsonc
{
  "fixture": "reasoning-two-tool-calls",
  "prefix_len": 4,                      // items[..4] were rendered
  "add_generation_prompt": false,
  "rendered": "[gMASK]<sop><|system|>Reasoning Effort: Max…",
  "spans": [ {"control":"[gMASK]"}, {"text":"…"} ],   // for eyeballing, not compared
  "request": { "messages": [...], "tools": [...], "add_generation_prompt": false },
  "divergences": ["reasoning-leak"],
  "normalise": [],
  "divergence_why": { "reasoning-leak": "…" }
}
```

Adding a second dialect is `--dialect <name>`; the binary lives in `dialect-glm`
today only because that is the crate this strand owns, and should move out when Qwen
lands.

### What one fixture expands into

Every prefix, and each prefix twice:

* `items[..k]` for every `k` in `0..=len` — §7.2's "the same fixture truncated at
  every prefix boundary", and the most valuable thing the corpus does. A renderer
  that is right about a whole conversation and wrong about its third prefix is a
  renderer whose cache never hits.
* with and without the generation prompt, except where the prefix ends mid-assistant
  turn and there is no generation prompt to add. `<|assistant|><think>` is the string
  every turn starts with, so it is checked on its own rather than assumed.

14 fixtures currently expand to 139 cases, run twice.

## Two profiles, and why the gate runs the corpus twice

`--profile server-bug-compatible` renders with every known oracle quirk switched on
and must match **byte-for-byte everywhere, with no exceptions**. Passing it is the
claim "our renderer is a complete model of what the server's renderer does".

`--profile faithful` is what the harness actually emits. Here a fixture may declare a
divergence — and only here, and only having earned it.

The order matters. An exception mechanism that is not backed by a
no-exceptions profile is just a way to make a red gate green.

## `divergences` vs `normalise`

Two different things, kept apart on purpose.

* **`divergences`** — we and the oracle genuinely disagree, and we think we are
  right. Currently one: `reasoning-leak`. Checked **two-sided**: an undeclared
  mismatch fails, *and* a declared divergence that stops reproducing fails, because a
  stale exception is how a gate rots into decoration.
* **`normalise`** — byte equality is not *available*. Currently one: `media-marker`,
  where llama.cpp substitutes `<__media_NONCE__>` with a nonce regenerated per server
  process, so no renderer could reproduce it. Both sides are folded onto the same
  canonical form and everything around the marker is still compared exactly. Also
  two-sided: the raw comparison is tried first, and a normalisation that turns out to
  be unnecessary is reported, because an unnecessary rewrite is a place a real diff
  could hide.

### The one declared divergence, in full

`reasoning-leak`. GLM's shipped template does

```jinja
{%- if m.reasoning_content is string %}{%- set reasoning_content = m.reasoning_content %}{%- endif %}
```

inside `{% for m in messages %}`. CPython Jinja2 scopes that to the iteration.
llama.cpp's jinja keeps it alive into the next one, so an assistant turn with no
reasoning of its own is rendered by the server carrying the **previous** turn's
`<think>` block. Measured 2026-09-09, same inputs, both engines:

```
server: …<think>R1</think>one<|user|>b<|assistant|><think>R1</think>two…
jinja2: …<think>R1</think>one<|user|>b<|assistant|><think></think>two…
```

It is not a corner case: llama.cpp drops `reasoning_content` when it is the empty
string, so a genuinely empty think block is indistinguishable from an absent one and
inherits the older text.

We render the Jinja2 reading, because Hugging Face is what rendered the training data
and because our production path submits token ids and never touches llama.cpp's
renderer. `GlmQuirks::reasoning_leak` reproduces the server exactly, which is what
makes the `server-bug-compatible` profile green and the divergence a measurement
rather than an opinion. Flipping the default is one line in `GlmDialect`.

## What the corpus covers

§7.2's list, plus what probing the template turned up.

| fixture | shape |
|---|---|
| `plain-turn` | the floor: one user message, one reply |
| `assistant-reasoning` | reasoning replayed as `reasoning_content` |
| `reasoning-two-tool-calls` | §7.2's headline: reasoning, two calls, two results, a second turn |
| `tool-outcomes` | `Abstained` and `Timeout` — abstention is not success (§8.2) |
| `empty-assistant-turn` | the `content: ""` case |
| `system-mid-history` | a system message at index > 0 → `SystemUpdateMode::InHistory` |
| `adversarial-control-literals` | user text spelling `<|assistant|>`, `<think>`, `<arg_key>` |
| `argument-types` | every JSON value kind through `<arg_value>`, incl. unicode |
| `segment-marks` | `SegmentMark` renders to nothing, and splits no observation block |
| `whitespace` | assistant content is stripped, user content is not |
| `reasoning-effort-low` | the effort line at position 3, which re-prefills everything |
| `long-turn` | a turn long enough to cross any internal limit |
| `reasoning-leak` | the declared divergence |
| `image` | an image part; the marker normalisation |

## Adding a fixture

1. Write the transcript into `fixtures/<name>.json`, and write `why`. A fixture
   without a reason to exist is a fixture nobody will dare delete.
2. `run_gate.py --only <name> -v`.
3. If it diverges, the honest options are to fix the renderer or to declare the
   divergence with its reason — not to trim the fixture until it passes.

## When the model ships a template change

`template_sha` stops matching (`GlmDialect::template_sha` is computed from the
embedded jinja, not pasted beside it), the dialect is stale, and this gate reports
exactly which fixtures moved. Adopting the change forks every live transcript on that
model (§5.5), because the render changed — which is correct, and which should be
visible.

```bash
tests/fidelity/extract_template.py <gguf> > crates/dialect-glm/template/glm-5.3-flash.jinja
cargo test -p letibot-dialect-glm     # template_sha_is_the_hash_of_the_shipped_jinja fails first
```
