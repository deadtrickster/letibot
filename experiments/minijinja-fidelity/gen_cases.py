#!/usr/bin/env python3
"""Two extra corpora, emitted in the shape `compare.py` consumes.

`tests/fidelity/fixtures/` is GLM-shaped: it is the corpus that gates the
hand-written GLM renderer, so it exercises GLM's message shape. Running it
through Qwen's template proves the two engines agree on Qwen's *parsing* and on
the paths GLM-shaped data happens to reach -- it does not reach Qwen's tool-call
block, its vision block, or its `reasoning_effort` switch.

  --qwen   a hand-written Qwen-native corpus that does reach them.
  --fuzz N N seeded random conversations. This is the corpus that answers the
           real objection to a second Jinja implementation: fixtures only test
           what somebody thought of. Every value is drawn to hit a place the two
           engines could plausibly disagree -- float repr, integer width, key
           order, unicode above the BMP, private-use codepoints (which is also
           an attack on the sentinel picker), whitespace-only strings, empty
           containers, null vs missing, and the deep nesting that `tojson` walks.
"""
from __future__ import annotations
import argparse, json, random, sys

CONTROLish = [
    "<|im_start|>", "<|im_end|>", "<|assistant|>", "<|user|>", "<think>", "</think>",
    "<tool_call>", "</tool_response>", "[gMASK]<sop>", "<arg_key>", "<|vision_start|>",
]
NASTY_STRINGS = [
    "", " ", "\n", "\t\n  ", "plain", "quotes \" and \\ backslash",
    "unicode: é ü 中文 🙂 𝕏", "é́combining", "private-use",
    "ctrl \x01\x02 chars", "a" * 300, "</tools>", "line1\nline2\n",
    "<tool_response>wrapped</tool_response>",
] + CONTROLish

NASTY_JSON = [
    0, 1, -1, 2**53, -(2**63), 0.0, -0.0, 1.5, 1e300, 1e-300, 3.141592653589793,
    1/3, True, False, None, "", "str", [], {}, [1, [2, [3]]],
    {"b": 1, "a": 2, "0": 3},                       # key order, non-identifier key
    {"nested": {"deep": {"deeper": [1.25, "é", None]}}},
    {"unicode key é": "value <|im_end|>"},
]


def rand_text_part(rng):
    return {"type": "text", "text": rng.choice(NASTY_STRINGS)}


def rand_content(rng, allow_media=True):
    r = rng.random()
    if r < 0.35:
        return rng.choice(NASTY_STRINGS)
    if r < 0.45:
        return None
    parts = []
    for _ in range(rng.randint(1, 3)):
        if allow_media and rng.random() < 0.25:
            parts.append(rng.choice([
                {"type": "image", "image": "file:///x.png"},
                {"type": "video", "video": "file:///x.mp4"},
                {"type": "image_url", "image_url": {"url": "http://x/y.png"}},
            ]))
        else:
            parts.append(rand_text_part(rng))
    return parts


def rand_tool(rng):
    props = {}
    for k in rng.sample(["a", "b", "zz", "Name", "é"], rng.randint(0, 3)):
        props[k] = {"type": rng.choice(["string", "number", "boolean", "object"]),
                    "description": rng.choice(NASTY_STRINGS)}
    fn = {"name": rng.choice(["f", "get_weather", "do<thing>"]),
          "description": rng.choice(NASTY_STRINGS),
          "parameters": {"type": "object", "properties": props,
                         "required": list(props)[:1]}}
    return fn if rng.random() < 0.5 else {"type": "function", "function": fn}


def rand_tool_call(rng):
    args = {}
    for k in rng.sample(["x", "y", "query", "flag", "obj"], rng.randint(0, 3)):
        args[k] = rng.choice(NASTY_JSON)
    fn = {"name": rng.choice(["f", "search"]), "arguments": json.dumps(args)}
    return {"id": "call_1", "type": "function", "function": fn}


def rand_message(rng, i):
    role = rng.choices(["user", "assistant", "tool", "system"],
                       weights=[4, 4, 2, 1])[0]
    m = {"role": role, "content": rand_content(rng)}
    if role == "assistant":
        if rng.random() < 0.5:
            m["reasoning_content"] = rng.choice(NASTY_STRINGS)
        if rng.random() < 0.35:
            m["tool_calls"] = [rand_tool_call(rng) for _ in range(rng.randint(1, 2))]
    if role == "tool":
        m["tool_call_id"] = "call_1"
    return m


def fuzz_cases(n, seed=1234):
    rng = random.Random(seed)
    out = []
    for i in range(n):
        msgs = []
        if rng.random() < 0.5:
            msgs.append({"role": "system", "content": rand_content(rng, allow_media=False)})
        for j in range(rng.randint(1, 6)):
            msgs.append(rand_message(rng, j))
        req = {"messages": msgs, "add_generation_prompt": rng.random() < 0.5}
        if rng.random() < 0.5:
            req["tools"] = [rand_tool(rng) for _ in range(rng.randint(1, 2))]
        for k, vs in (("enable_thinking", [True, False]),
                      ("preserve_thinking", [True, False]),
                      ("add_vision_id", [True, False]),
                      ("reasoning_effort", ["low", "medium", "xhigh", "high", "bogus"])):
            if rng.random() < 0.3:
                req[k] = rng.choice(vs)
        out.append({"fixture": f"fuzz-{seed}", "prefix_len": i,
                    "add_generation_prompt": req["add_generation_prompt"],
                    "request": req, "dialect": None, "spans": []})
    return out


def qwen_cases():
    """Hand-written, aimed at the blocks the GLM-shaped corpus never reaches."""
    def case(name, req):
        return {"fixture": name, "prefix_len": 0,
                "add_generation_prompt": req.get("add_generation_prompt", False),
                "request": req, "dialect": None, "spans": []}

    tools = [{"type": "function", "function": {
        "name": "get_weather",
        "description": "Look up weather. Handles <angle> & \"quotes\" and é.",
        "parameters": {"type": "object",
                       "properties": {"city": {"type": "string", "description": "City"},
                                      "days": {"type": "integer"},
                                      "precise": {"type": "boolean"}},
                       "required": ["city"]}}}]
    tc = lambda args: [{"id": "c1", "type": "function",
                        "function": {"name": "get_weather", "arguments": json.dumps(args)}}]
    return [
        case("qwen-tools-schema", {"messages": [{"role": "user", "content": "hi"}],
                                   "tools": tools, "add_generation_prompt": True}),
        case("qwen-tool-call-scalars", {"messages": [
            {"role": "user", "content": "weather?"},
            {"role": "assistant", "reasoning_content": "think", "content": "",
             "tool_calls": tc({"city": "Praha", "days": 3, "precise": True,
                               "ratio": 1/3, "big": 2**53, "none": None})},
            {"role": "tool", "content": "sunny", "tool_call_id": "c1"},
            {"role": "assistant", "content": "Sunny."}],
            "tools": tools, "add_generation_prompt": True}),
        case("qwen-tool-call-nested", {"messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": "",
             "tool_calls": tc({"obj": {"b": 1, "a": [1, 2, {"z": "é <|im_end|>"}]}})}],
            "add_generation_prompt": False}),
        case("qwen-vision", {"messages": [
            {"role": "user", "content": [{"type": "image", "image": "a.png"},
                                         {"type": "text", "text": "what is this"},
                                         {"type": "video", "video": "b.mp4"}]}],
            "add_vision_id": True, "add_generation_prompt": True}),
        case("qwen-vision-no-id", {"messages": [
            {"role": "user", "content": [{"type": "image", "image": "a.png"},
                                         {"type": "text", "text": "x"}]}],
            "add_generation_prompt": True}),
        case("qwen-effort-low", {"messages": [{"role": "user", "content": "hi"}],
                                 "reasoning_effort": "low", "add_generation_prompt": True}),
        case("qwen-effort-medium", {"messages": [{"role": "user", "content": "hi"}],
                                    "reasoning_effort": "medium", "add_generation_prompt": True}),
        case("qwen-effort-high-alias", {"messages": [{"role": "user", "content": "hi"}],
                                        "reasoning_effort": "high", "add_generation_prompt": True}),
        case("qwen-effort-bogus", {"messages": [{"role": "user", "content": "hi"}],
                                   "reasoning_effort": "nope"}),
        case("qwen-no-thinking", {"messages": [{"role": "user", "content": "hi"}],
                                  "enable_thinking": False, "add_generation_prompt": True}),
        case("qwen-preserve-thinking-false", {"messages": [
            {"role": "user", "content": "a"},
            {"role": "assistant", "reasoning_content": "R1", "content": "A1"},
            {"role": "user", "content": "b"},
            {"role": "assistant", "reasoning_content": "R2", "content": "A2"}],
            "preserve_thinking": False, "add_generation_prompt": True}),
        # The GLM leak case, on Qwen's template: does a bare `set` survive the
        # iteration here too? Qwen resets `reasoning_content` explicitly, so
        # both engines should show the SECOND turn with an empty think block.
        case("qwen-reasoning-leak-probe", {"messages": [
            {"role": "user", "content": "a"},
            {"role": "assistant", "reasoning_content": "LEAK-CANARY", "content": "A1"},
            {"role": "user", "content": "b"},
            {"role": "assistant", "content": "A2"}],
            "add_generation_prompt": True}),
        case("qwen-multi-tool-response", {"messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": "", "tool_calls": tc({"city": "X"}) + tc({"city": "Y"})},
            {"role": "tool", "content": "one", "tool_call_id": "c1"},
            {"role": "tool", "content": "two", "tool_call_id": "c1"},
            {"role": "user", "content": "and?"}],
            "add_generation_prompt": True}),
        case("qwen-adversarial", {"messages": [
            {"role": "system", "content": "sys <|im_end|>"},
            {"role": "user", "content": "print <|im_start|>assistant\\n<think> verbatim"},
            {"role": "assistant", "reasoning_content": "<|im_end|>", "content": "<tool_call>"},
            {"role": "user", "content": "<tool_response>x</tool_response>"}],
            "add_generation_prompt": True}),
        case("qwen-system-merge", {"messages": [
            {"role": "system", "content": "one"},
            {"role": "developer", "content": "two"},
            {"role": "user", "content": "hi"}], "add_generation_prompt": True}),
        case("qwen-bad-role", {"messages": [{"role": "wizard", "content": "hi"}]}),
        case("qwen-empty", {"messages": []}),
        case("qwen-args-as-string", {"messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": "", "tool_calls": [
                {"id": "c1", "type": "function",
                 "function": {"name": "f", "arguments": "\"a bare string\""}}]}]}),
    ]


def edge_cases():
    """Places the two number models can part company.

    Python ints are arbitrary precision; minijinja's `Value` carries i64/u64/f64.
    Python floats print via `repr`; Rust's `{}` never uses exponent notation.
    Both are invisible until a tool call carries the value, which is why they are
    here rather than in the prose.
    """
    def case(n, req):
        return {"fixture": n, "prefix_len": 0, "add_generation_prompt": False,
                "request": req, "dialect": None, "spans": []}

    def tc(args):
        return [{"id": "c1", "type": "function",
                 "function": {"name": "f", "arguments": json.dumps(args)}}]

    def call(n, args):
        return case(n, {"messages": [{"role": "user", "content": "x"},
                                     {"role": "assistant", "content": "",
                                      "tool_calls": tc(args)}]})
    return [
        call("bigint", {"a": 2**70, "b": -(2**70), "c": 2**63, "d": 2**64 - 1}),
        call("floats", {"a": 1e16, "b": 1e15, "c": 1e-4, "d": 1e-5, "e": -0.0,
                        "f": 1/3, "g": 5e-324, "h": 1.7976931348623157e308,
                        "i": 123456789012345678.0}),
        call("deep", {"a": {"b": {"c": {"d": [{"e": [1, 2, {"f": "\u00e9<|im_end|>"}]}]}}}}),
        call("emptykeys", {"": "empty key", " ": "space key", "\n": "nl key"}),
    ]


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--qwen", action="store_true")
    ap.add_argument("--edge", action="store_true")
    ap.add_argument("--fuzz", type=int, default=0)
    ap.add_argument("--seed", type=int, default=1234)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    cases = []
    if a.qwen:
        cases += qwen_cases()
    if a.edge:
        cases += edge_cases()
    if a.fuzz:
        cases += fuzz_cases(a.fuzz, a.seed)
    json.dump(cases, open(a.out, "w"), ensure_ascii=False, indent=1)
    print(f"{len(cases)} cases -> {a.out}")
