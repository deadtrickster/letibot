#!/usr/bin/env python3
"""The training-runtime oracle: render a chat template the way the model was trained.

WHY THIS FILE EXISTS
====================

A chat template is a Jinja program shipped inside the model. Turning a
conversation into a prompt means running that program. There is more than one
Jinja, and they do not agree.

  * **CPython Jinja2**, driven by HuggingFace `transformers`, is what rendered
    the text the model was *trained on*. Whatever it produces is, by definition,
    the format the model recognises.
  * **minja** is llama.cpp's from-scratch C++ reimplementation of Jinja. It is
    what `POST /apply-template` and `POST /v1/chat/completions` run at serving
    time.

They diverge. We have a measured instance, and it is not academic — see
"THE DIVERGENCE THAT MOTIVATED THIS FILE" below.

The gate used to treat `/apply-template` as the authority. That was the wrong
authority for two reasons:

 1. **We do not use it.** The harness tokenizes `RenderSpan`s itself and submits
    token ids to `/completion`. minja is not in our runtime path at all.
    Conformance to minja measures agreement with a bug we already route around.
 2. **The model does not use it either.** The model's notion of "a correctly
    formatted conversation" was fixed during training, by CPython Jinja2.

So this file is the authority, and `/apply-template` is demoted to interop
information: "how would llama.cpp's server differ from us", which is worth
knowing and must not gate.

The happy consequence: this oracle needs **no GPU, no model weights and no
server**. It needs the template text (see `extract_template.py`) and jinja2.


WHAT "FAITHFUL TO transformers" MEANS PRECISELY
===============================================

Verified against **transformers 5.16.1** (`pip install transformers`, read at
`transformers/utils/chat_template_utils.py::_cached_compile_jinja_template`,
2026-09-09). Every line of `build_env()` below is a copy of a line in that
function, and each one is load-bearing in a way that fails *silently* if you
omit it:

  * `ImmutableSandboxedEnvironment` — not a plain `Environment`. It refuses
    mutating method calls (`list.append`, `dict.update`). A template that
    mutates renders fine under a plain Environment and raises under the real
    runtime, so a plain Environment can pass a template that production rejects.
  * `trim_blocks=True, lstrip_blocks=True` — whitespace control. Without them
    every `{% ... %}` on its own line leaves a newline in the prompt. The output
    still looks plausible; it is just not what the model saw.
  * `extensions=[jinja2.ext.loopcontrols]` — GLM's template uses `{% break %}`
    in `has_dup_tool_result_id`. Stock Jinja2 rejects it outright with
    `unknown tag 'break'`, so this one at least fails loudly.
  * a **replacement `tojson` filter** — this is the subtle one. Stock Jinja2's
    `tojson` is `do_tojson(eval_ctx, value, indent=None)`: it takes no
    `ensure_ascii` argument and it HTML-escapes `<`, `>` and `&` into `<`
    style escapes. GLM's template calls `{{ v | tojson(ensure_ascii=False) }}`,
    which under stock Jinja2 is a `TypeError`, and if you "fix" that by dropping
    the argument you get every non-ASCII character escaped and every angle
    bracket mangled. transformers replaces the filter with a thin `json.dumps`
    wrapper. So do we, byte for byte.
  * `raise_exception` and `strftime_now` globals — templates call these.
    `strftime_now` takes exactly one positional `format` argument.

One thing in transformers we deliberately do NOT copy verbatim: its
`AssistantTracker` extension, which implements the `{% generation %}` tag used
to compute assistant-token masks for training. We do not need masks. But its
mere presence changes whether a template *parses*, so `build_env()` registers a
pass-through `generation` tag: a template using it renders identically to
transformers, and we simply do not collect the indices. Neither GLM's nor
Qwen's template uses the tag today.


THE DIVERGENCE THAT MOTIVATED THIS FILE
=======================================

GLM's template contains, inside `{% for m in messages %}`:

    {%- if m.reasoning_content is string %}
        {%- set reasoning_content = m.reasoning_content %}
    {%- endif %}
    ...
    {%- if ... and reasoning_content is defined -%}{{ '<think>' + reasoning_content + '</think>' }}

A bare `{% set %}` inside a `{% for %}` is scoped to the iteration in CPython
Jinja2: on the next message `reasoning_content` is undefined again. minja keeps
it alive. So under minja an assistant turn that has no reasoning of its own is
served carrying the **previous** turn's `<think>` block.

That the template's author understood the scoping rule is not a guess: the same
template uses `namespace()` seven times (`ns`, `ns_tool`, `ns_blk`, `ns_chk`,
`ns_a`, `ns_cnt`, `ns_f`) precisely to make a value survive a loop iteration.
Bare `set` where they wanted per-iteration, `namespace` where they wanted
carry-over. The intent is unambiguous and minja is wrong.

There is one profile, `faithful`, and it must equal this file. There used to be
a second, `server-bug-compatible`, which modelled minja's bug and had to differ
here exactly on the leaking cases. T2 removed it: prompts are now rendered by
running this same template through a correct Jinja engine, so reproducing a
wrong one bought nothing that a differential against CPython does not already
buy — and it meant maintaining someone else's bug on purpose. `--interop` still
measures the leak, against the server that has it rather than against our model
of it.


PROVENANCE: THE PART THAT DOES REAL WORK
========================================

Byte equality answers "did we render the right prompt". It does not answer the
question the `RenderSpan::{Text,Control}` split exists to answer:

    can anything a user, a tool, or an MCP server put into the conversation
    turn into a CONTROL TOKEN?

If it can, the model sees a turn boundary the harness never wrote — prompt
injection at the tokenizer level. `crates/dialect` makes that structurally
impossible by tokenizing `Text` with special-token parsing OFF. But that is only
a guarantee if the renderer put the right bytes in `Text` in the first place,
and today that is a hand-written property of the renderer.

This file makes it mechanical. For every output byte we decide:

    LITERAL — it came from the template source. It MAY be a control token.
    DATA    — it came from message/tool payload. It MUST NOT be a control token.

Method: **wrap the data, not the template.** Trying to instrument the template
means parsing and rewriting Jinja, and a rewritten template is no longer the
thing under test. Instead we leave the template untouched and mark the *inputs*:

 1. Pick two Private-Use-Area codepoints that appear nowhere in the request
    (see `pick_sentinels` — adaptive, so genuine PUA characters in user input
    cannot collide with ours, and nothing has to be stripped from the payload).
 2. Wrap every payload string in that pair.
 3. Render twice: once with the clean payload, once with the wrapped payload.
 4. **Self-validate:** deleting the sentinels from the wrapped render must equal
    the clean render, byte for byte. If it does not, the template did something
    to the data we did not model — sliced it, hashed it, compared it, escaped it
    — and the map is not a map of anything. We RAISE. A provenance map that is
    allowed to be approximately right is worse than none, because it will be
    trusted.
 5. Split on the sentinels: inside a pair is DATA, outside is LITERAL.

Step 4 is the whole reason this is trustworthy rather than plausible.


WHAT IS AND IS NOT WRAPPED, AND WHY
===================================

`wrap_request()` walks the request with a small amount of knowledge about what
a chat template is allowed to do with each field.

**Values are wrapped. Keys at structural positions are not.** The template
looks fields up by name (`m.role == 'user'`, `'function' in tool`,
`tc.function`, `item.type == 'text'`) and compares a few scalars against
literals (`role`, `type`, `defer_loading`, `strict`). Wrapping those would
change control flow, i.e. change the render — which step 4 would catch, loudly,
but as a failure rather than as a map. So they are left alone by construction.

**Below an "opaque" boundary, keys are wrapped too.** A tool's `parameters`
schema and a tool call's `arguments` object are never indexed by the template;
they are handed wholesale to `tojson`, or iterated with `.items()` and emitted
as `<arg_key>{{ k }}</arg_key>`. Those keys are model- or MCP-controlled text
that reaches the output, so they are DATA and are wrapped.

**Tool schemas are wrapped.** They used to be left out, on the reasoning that
tools are developer-supplied. That reasoning expires the moment a tool
description comes from an MCP server the user added this morning. A tool
`description` is attacker-reachable text that is rendered into the system
prompt, which is the most valuable place in the prompt to inject.

**Numbers, booleans and null are not wrapped**, and this is a statement about
them rather than an omission. There is no way to wrap them without changing
their type and therefore the render. They do not need it: `json.dumps` renders
them as `[-+0-9.eE]`, `true`, `false`, `null`, and every control token in every
dialect we support is delimited by `<`, `[` or `|`. A JSON number cannot spell
one. If a future dialect adopts a bare-alphanumeric control token, this
paragraph stops being true and `check_scalar_safety()` below is the place that
should start failing.

**Known residual gap.** Keys at structural positions — including the property
names inside a *top-level* tool dict — are classified LITERAL although a
sufficiently strange MCP server controls some of them. They are reported by
`literal_data_keys()` so the gap is visible rather than forgotten.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass, field
from datetime import datetime

try:
    import jinja2
    import jinja2.ext
    from jinja2.sandbox import ImmutableSandboxedEnvironment
except ImportError:  # pragma: no cover - environment problem, not a test failure
    sys.exit(
        "oracle_hf.py needs jinja2 (>=3.1). It needs nothing else: no torch, no\n"
        "transformers, no model, no server. `pip install jinja2` or apt install\n"
        "python3-jinja2."
    )

# The transformers version whose _cached_compile_jinja_template this mirrors.
# Bump it only after re-reading that function; see build_env().
TRANSFORMERS_REFERENCE = "5.16.1"

LITERAL = "LITERAL"
DATA = "DATA"


# --------------------------------------------------------------------------
# 1. The environment
# --------------------------------------------------------------------------


class _PassthroughGeneration(jinja2.ext.Extension):
    """`{% generation %}...{% endgeneration %}` as a no-op wrapper.

    transformers registers an `AssistantTracker` for this tag to record which
    output bytes the assistant produced, for training masks. We do not want the
    indices, but the tag has to *parse* or a template that uses it explodes here
    while rendering fine in transformers. So we keep the tag and drop the
    bookkeeping: the rendered bytes are identical either way.
    """

    tags = {"generation"}

    def parse(self, parser):
        lineno = next(parser.stream).lineno
        body = parser.parse_statements(["name:endgeneration"], drop_needle=True)
        return jinja2.nodes.CallBlock(
            self.call_method("_noop"), [], [], body
        ).set_lineno(lineno)

    def _noop(self, caller):
        return caller()


def _raise_exception(message):
    raise jinja2.exceptions.TemplateError(message)


def _strftime_now(format):  # noqa: A002 - the parameter name is part of the contract
    return datetime.now().strftime(format)


def _tojson(x, ensure_ascii=False, indent=None, separators=None, sort_keys=False):
    """transformers' replacement for Jinja2's `tojson`.

    Stock Jinja2 signature is `(eval_ctx, value, indent=None)` and it HTML-escapes
    `<`, `>`, `&`, `'`. Neither the extra kwargs nor the absence of escaping is
    optional: GLM calls `tojson(ensure_ascii=False)`, which stock Jinja2 raises
    on, and a tool schema containing `<` would otherwise be mangled.
    """
    return json.dumps(
        x,
        ensure_ascii=ensure_ascii,
        indent=indent,
        separators=separators,
        sort_keys=sort_keys,
    )


def build_env() -> ImmutableSandboxedEnvironment:
    """Exactly transformers' chat-template environment. See module docstring."""
    env = ImmutableSandboxedEnvironment(
        trim_blocks=True,
        lstrip_blocks=True,
        extensions=[_PassthroughGeneration, jinja2.ext.loopcontrols],
    )
    env.filters["tojson"] = _tojson
    env.globals["raise_exception"] = _raise_exception
    env.globals["strftime_now"] = _strftime_now
    return env


def render(
    template: str,
    messages: list,
    tools: list | None = None,
    documents: list | None = None,
    add_generation_prompt: bool = False,
    **template_kwargs,
) -> str:
    """One render, the way `render_jinja_template` does it.

    `tools=None` and `documents=None` are passed explicitly rather than omitted:
    transformers always binds both names, and a template asking
    `{% if tools is defined %}` must see the same answer here as in training.
    """
    env = build_env()
    compiled = env.from_string(template)
    return compiled.render(
        messages=messages,
        tools=tools,
        documents=documents,
        add_generation_prompt=add_generation_prompt,
        **template_kwargs,
    )


# --------------------------------------------------------------------------
# 2. Sentinels
# --------------------------------------------------------------------------

# The Basic Multilingual Plane's Private Use Area. Nothing standard assigns
# these, so a template will not emit one on its own; we still verify the pair we
# pick is absent from the payload rather than assuming it.
PUA_LO, PUA_HI = 0xE000, 0xF8FF


def pick_sentinels(payload) -> tuple[str, str]:
    """Two PUA codepoints that do not occur anywhere in `payload`.

    Adaptive on purpose. The obvious alternative is to fix the pair and strip it
    out of incoming data, but that means the oracle silently rewrites the very
    input it is supposed to be faithful to, and an attacker who guesses the pair
    gets to punch holes in the provenance map. Choosing a free pair instead means
    the map is exact for arbitrary input, including input that contains PUA
    characters of its own.
    """
    used = {ord(c) for c in json.dumps(payload, ensure_ascii=False) if PUA_LO <= ord(c) <= PUA_HI}
    free = [cp for cp in range(PUA_LO, PUA_HI + 1) if cp not in used]
    if len(free) < 2:
        raise ProvenanceError(
            "no free Private Use Area codepoint left to use as a sentinel: the "
            "payload uses all 6400 of them. Nothing legitimate does this."
        )
    return chr(free[0]), chr(free[1])


class ProvenanceError(RuntimeError):
    """The provenance map could not be established, so there is no map.

    Raised, never warned. A map that is allowed to be approximately right is
    worse than no map, because downstream code will trust it.
    """


def _wrap_str(s: str, o: str, c: str) -> str:
    """Wrap the non-whitespace core of `s`, leaving surrounding whitespace outside.

    The naive `o + s + c` breaks on any template that calls `.strip()` on the
    value — GLM's does, on assistant content. `"  hi  ".strip()` is `"hi"`, but
    `(o + "  hi  " + c).strip()` is unchanged, because a sentinel is not
    whitespace. The self-validation in `render_with_provenance` catches that as a
    hard failure, which is correct but useless: we would have no map for any
    fixture with padded assistant content.

    Putting the sentinels *inside* the padding makes the wrap survive `.strip()`
    on both sides. The cost is that leading and trailing whitespace of a payload
    string is classified LITERAL. That is a real, and harmless, loss of
    precision: a run of whitespace cannot spell a control token in any dialect.
    """
    if not s:
        return s
    i = 0
    while i < len(s) and s[i].isspace():
        i += 1
    if i == len(s):
        return s  # all whitespace; nothing here can ever be a control token
    j = len(s)
    while s[j - 1].isspace():
        j -= 1
    return s[:i] + o + s[i:j] + c + s[j:]


# --------------------------------------------------------------------------
# 3. Wrapping the request
# --------------------------------------------------------------------------

# Scalars the template compares against string literals, so wrapping them would
# change control flow rather than annotate output. Every one of these is a
# discriminator, not payload.
STRUCTURAL_SCALARS = frozenset({"role", "type", "defer_loading", "strict"})


def _opaque(x, o: str, c: str):
    """Wrap everything below here, keys included.

    Used where the template never indexes into the value by name: it either
    hands the whole thing to `tojson` or iterates `.items()` and prints both
    halves. Both halves are then reachable by an attacker and both are output,
    so both are DATA.
    """
    if isinstance(x, str):
        return _wrap_str(x, o, c)
    if isinstance(x, dict):
        return {_wrap_str(k, o, c): _opaque(v, o, c) for k, v in x.items()}
    if isinstance(x, list):
        return [_opaque(v, o, c) for v in x]
    return x  # numbers, booleans, null - see module docstring


def _wrap_content(content, o: str, c: str):
    """Message content: a string, or the OpenAI multi-part list."""
    if isinstance(content, str):
        return _wrap_str(content, o, c)
    if isinstance(content, list):
        out = []
        for part in content:
            if not isinstance(part, dict):
                out.append(_opaque(part, o, c))
                continue
            p = {}
            for k, v in part.items():
                if k in STRUCTURAL_SCALARS:
                    p[k] = v
                elif k == "image_url" and isinstance(v, dict):
                    # {"image_url": {"url": ...}} - `url` is payload, `url` is not
                    # looked up as anything else.
                    p[k] = {kk: _opaque(vv, o, c) for kk, vv in v.items()}
                else:
                    p[k] = _opaque(v, o, c)
            out.append(p)
        return out
    return _opaque(content, o, c)


def _wrap_tool_call(tc, o: str, c: str):
    if not isinstance(tc, dict):
        return _opaque(tc, o, c)
    out = {}
    for k, v in tc.items():
        if k in STRUCTURAL_SCALARS:
            out[k] = v
        elif k == "function" and isinstance(v, dict):
            fn = {}
            for fk, fv in v.items():
                # `arguments` is opaque: GLM iterates it with `.items()` and
                # prints the keys into <arg_key>, so arg names are DATA too.
                fn[fk] = _opaque(fv, o, c)
            out[k] = fn
        else:
            out[k] = _opaque(v, o, c)
    return out


def wrap_messages(messages: list, o: str, c: str) -> list:
    out = []
    for m in messages:
        if not isinstance(m, dict):
            out.append(_opaque(m, o, c))
            continue
        w = {}
        for k, v in m.items():
            if k in STRUCTURAL_SCALARS:
                w[k] = v
            elif k == "content":
                w[k] = _wrap_content(v, o, c)
            elif k == "tool_calls" and isinstance(v, list):
                w[k] = [_wrap_tool_call(tc, o, c) for tc in v]
            else:
                # reasoning_content, tool_call_id, id, name, and anything a
                # future template adds. Payload until shown otherwise; if it
                # turns out to be a discriminator the self-validation says so.
                w[k] = _opaque(v, o, c)
        out.append(w)
    return out


def _wrap_tool_body(fn: dict, o: str, c: str) -> dict:
    out = {}
    for k, v in fn.items():
        if k in STRUCTURAL_SCALARS:
            out[k] = v
        else:
            # name, description, parameters, returns... all of it is text that
            # lands in the system prompt, and with MCP all of it is remote.
            out[k] = _opaque(v, o, c)
    return out


def wrap_tools(tools: list | None, o: str, c: str) -> list | None:
    """Tool schemas are payload.

    They were left unwrapped in the first cut of this, on the reasoning that a
    tool schema is written by the developer. That stops being true the moment a
    tool comes from an MCP server: its `description` is remote text rendered
    into the system prompt, which is the single most valuable place in a prompt
    to land an injected control token.

    The top-level keys of a tool dict stay bare: the template tests
    `'function' in tool` and skips `defer_loading`/`strict` by name.
    """
    if tools is None:
        return None
    out = []
    for t in tools:
        if not isinstance(t, dict):
            out.append(_opaque(t, o, c))
        elif "function" in t:
            # {"type": "function", "function": {...}} - the OpenAI shape.
            out.append(
                {
                    k: (v if k in STRUCTURAL_SCALARS
                        else _wrap_tool_body(v, o, c) if k == "function" and isinstance(v, dict)
                        else _opaque(v, o, c))
                    for k, v in t.items()
                }
            )
        else:
            # The un-nested form GLM also accepts: the tool dict *is* the body.
            out.append(_wrap_tool_body(t, o, c))
    return out


def literal_data_keys(messages: list, tools: list | None) -> list[str]:
    """The keys we deliberately left classified LITERAL, so the gap is visible.

    These are dictionary keys at structural positions. The template looks them
    up by name, so wrapping them would change the render rather than annotate
    it. Almost all of them are fixed vocabulary (`role`, `content`, `name`); the
    interesting case is a tool dict carrying an unexpected key, which is why
    this returns the actual set found rather than a constant.
    """
    seen: set[str] = set()
    for m in messages or []:
        if isinstance(m, dict):
            seen.update(m.keys())
    for t in tools or []:
        if isinstance(t, dict):
            seen.update(t.keys())
            fn = t.get("function")
            if isinstance(fn, dict):
                seen.update(fn.keys())
    return sorted(seen)


# --------------------------------------------------------------------------
# 4. Provenance
# --------------------------------------------------------------------------


@dataclass
class Provenance:
    """A rendered prompt plus a byte-exact origin map over it."""

    text: str
    #: (start, end, LITERAL|DATA) over `text`, contiguous, covering it exactly.
    regions: list[tuple[int, int, str]] = field(default_factory=list)
    sentinels: tuple[str, str] = ("", "")
    #: number of sentinel pairs that survived into the output
    pairs: int = 0

    def kind_at(self, i: int) -> str:
        for start, end, kind in self.regions:
            if start <= i < end:
                return kind
        raise IndexError(i)

    def kinds_over(self, start: int, end: int) -> set[str]:
        """Every kind touched by the half-open byte range [start, end)."""
        if start >= end:
            return set()
        return {k for (s, e, k) in self.regions if s < end and start < e}

    def data_slices(self) -> list[str]:
        return [self.text[s:e] for (s, e, k) in self.regions if k == DATA]


def _split_on_sentinels(wrapped: str, o: str, c: str) -> tuple[str, list[tuple[int, int, str]], int]:
    """Strip the sentinels and record what was between them."""
    out: list[str] = []
    regions: list[tuple[int, int, str]] = []
    pos = 0
    depth = 0
    run_start = 0
    pairs = 0

    def close_run(kind: str) -> None:
        nonlocal run_start
        if pos > run_start:
            if regions and regions[-1][2] == kind:
                regions[-1] = (regions[-1][0], pos, kind)
            else:
                regions.append((run_start, pos, kind))
        run_start = pos

    for ch in wrapped:
        if ch == o:
            close_run(DATA if depth else LITERAL)
            depth += 1
            continue
        if ch == c:
            if depth == 0:
                raise ProvenanceError(
                    "closing sentinel with no opening one in the wrapped render: "
                    "the template reordered or duplicated payload in a way this "
                    "map cannot describe."
                )
            close_run(DATA)
            depth -= 1
            if depth == 0:
                pairs += 1
            continue
        out.append(ch)
        pos += 1
    close_run(DATA if depth else LITERAL)
    if depth != 0:
        raise ProvenanceError(
            f"{depth} unclosed sentinel(s) in the wrapped render: the template "
            "truncated payload mid-string."
        )
    return "".join(out), regions, pairs


def render_with_provenance(
    template: str,
    messages: list,
    tools: list | None = None,
    documents: list | None = None,
    add_generation_prompt: bool = False,
    **template_kwargs,
) -> Provenance:
    """Render, and classify every output byte as LITERAL or DATA.

    Raises `ProvenanceError` if the wrapped render does not strip back to the
    clean one. That is not a warning condition: it means the template did
    something to the payload that this map does not model, and a map that is
    only mostly right is a map that will be trusted while being wrong.
    """
    o, c = pick_sentinels([messages, tools, documents, template_kwargs])

    clean = render(
        template, messages, tools, documents, add_generation_prompt, **template_kwargs
    )
    wrapped_raw = render(
        template,
        wrap_messages(messages, o, c),
        wrap_tools(tools, o, c),
        documents if documents is None else _opaque(documents, o, c),
        add_generation_prompt,
        **template_kwargs,
    )

    stripped, regions, pairs = _split_on_sentinels(wrapped_raw, o, c)
    if stripped != clean:
        i = 0
        while i < min(len(stripped), len(clean)) and stripped[i] == clean[i]:
            i += 1
        raise ProvenanceError(
            "provenance self-validation FAILED: removing the sentinels from the\n"
            "wrapped render did not reproduce the clean render, so the sentinels\n"
            "did not merely ride along with the data - the template transformed,\n"
            "compared, sliced or escaped it. There is no provenance map for this\n"
            "case; treat it as unclassified rather than as all-LITERAL.\n"
            f"  first difference at byte {i} "
            f"(clean is {len(clean)} bytes, stripped is {len(stripped)})\n"
            f"  clean   : ...{clean[max(0, i - 40):i + 60]!r}\n"
            f"  stripped: ...{stripped[max(0, i - 40):i + 60]!r}"
        )
    return Provenance(text=clean, regions=regions, sentinels=(o, c), pairs=pairs)


def check_scalar_safety(control_literals) -> list[str]:
    """Numbers and booleans are unwrapped; prove that stays harmless.

    A JSON scalar renders as `[-+0-9.eE]`, `true`, `false` or `null`. If some
    dialect ever declares a control token spellable from that alphabet, the
    "unwrapped scalars cannot inject" argument in the module docstring collapses
    and this returns the offending literals.
    """
    alphabet = set("0123456789+-.eE") | set("truefalsn")
    return [lit for lit in control_literals if set(lit) <= alphabet]


# --------------------------------------------------------------------------
# 5. CLI
# --------------------------------------------------------------------------


def _adapt_request(req: dict) -> dict:
    """Turn an OpenAI-shaped `/apply-template` body into template arguments.

    One real conversion happens here, and it is not cosmetic. On the wire a tool
    call's `arguments` is a **JSON string**; a chat template expects an
    **object**, because that is what transformers passes (GLM iterates it with
    `.items()` to emit `<arg_key>`/`<arg_value>` pairs, and a str has no
    `.items()`). llama.cpp performs the same parse before handing the messages
    to minja, which is why the existing `/apply-template` gate works with the
    string form. We do it explicitly so the conversion is visible rather than
    buried in a server.
    """
    req = json.loads(json.dumps(req))  # deep copy; callers reuse these dicts
    for m in req.get("messages", []):
        for tc in m.get("tool_calls", []) or []:
            fn = tc.get("function")
            if isinstance(fn, dict) and isinstance(fn.get("arguments"), str):
                try:
                    fn["arguments"] = json.loads(fn["arguments"])
                except json.JSONDecodeError as e:
                    raise ValueError(
                        f"tool_call arguments is not JSON: {fn['arguments']!r} ({e})"
                    ) from None
    return req


def _split_request(req: dict):
    r = _adapt_request(req)
    return (
        r.pop("messages", []),
        r.pop("tools", None),
        r.pop("documents", None),
        r.pop("add_generation_prompt", False),
        r,
    )


def render_request(template: str, req: dict) -> Provenance:
    """Render one `/apply-template`-shaped request body, with provenance."""
    messages, tools, documents, agp, rest = _split_request(req)
    return render_with_provenance(template, messages, tools, documents, agp, **rest)


def render_request_clean(template: str, req: dict) -> str:
    """The render alone, with no provenance map.

    For the one caller that still needs the bytes after `render_request` has
    refused to produce a map: the failure is already reported, and the comparison
    it was making is still meaningful.
    """
    messages, tools, documents, agp, rest = _split_request(req)
    return render(template, messages, tools, documents, agp, **rest)


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--template", required=True, help="path to the .jinja chat template")
    ap.add_argument(
        "--requests",
        default="-",
        help="JSON file holding one /apply-template body or a list of them; - for stdin",
    )
    ap.add_argument("--json", action="store_true", help="emit machine-readable output")
    args = ap.parse_args()

    with open(args.template, encoding="utf-8") as fh:
        template = fh.read()
    raw = sys.stdin.read() if args.requests == "-" else open(args.requests, encoding="utf-8").read()
    bodies = json.loads(raw)
    if isinstance(bodies, dict):
        bodies = [bodies]

    out = []
    for body in bodies:
        p = render_request(template, body)
        out.append(
            {
                "prompt": p.text,
                "regions": [[s, e, k] for (s, e, k) in p.regions],
                "pairs": p.pairs,
            }
        )
    if args.json:
        json.dump(out, sys.stdout, ensure_ascii=False, indent=2)
        print()
    else:
        for o in out:
            print(o["prompt"])
            print(f"--- {o['pairs']} sentinel pairs, {len(o['regions'])} regions ---")
    return 0


if __name__ == "__main__":
    sys.exit(main())
