#!/usr/bin/env python3
"""The renderer-fidelity gate.

    AUTHORITY (gates)       our `faithful` render  ==  CPython Jinja2 + transformers
    PROVENANCE (gates)      no RenderSpan::Control originates in substituted DATA
    INTEROP (reports only)  how llama.cpp's own renderer would differ, and whether
                            each declared divergence still reproduces

WHAT HAPPENED TO THE PROFILES PHASE
===================================

There used to be a third gating phase. `letibot-render --profile
server-bug-compatible` rendered the corpus a second time with a quirk that
reproduced minja's cross-iteration `{% set %}` leak, and the gate required the
two profiles to differ exactly on the fixtures that declared a divergence.

T2 removed it, and the reason is worth keeping. Its purpose was to show we
understood the shipped template well enough to reproduce llama.cpp's renderer
exactly, which is what earned the `faithful` profile the standing to call a
difference a divergence. Under T1 prompts are rendered by running the real
template through a correct Jinja engine, and *that* is stronger evidence than
reproducing a wrong one — while keeping the quirk would mean carrying a
deliberate reimplementation of someone else's bug forever.

The declarations did not become decoration with it. A fixture's `divergences`
list is now checked in INTEROP, against the server itself rather than against
our model of it: a declared divergence that the server no longer produces is
reported there, which is the same two-sided check one hop closer to the thing
being described. It does not gate, because it needs a server.

WHY THE AUTHORITY MOVED
=======================

This gate used to diff our renderer against llama.cpp's `POST /apply-template`.
That endpoint runs **minja**, a from-scratch C++ reimplementation of Jinja. The
model was trained on text produced by **CPython Jinja2** driven by HuggingFace
`transformers`. The two disagree, and we have a measured case where the
disagreement changes what the model is told (`oracle_hf.py`, "THE DIVERGENCE
THAT MOTIVATED THIS FILE").

Two facts decide which one is the authority:

  * The harness tokenizes `RenderSpan`s itself and submits **token ids** to
    `/completion`. minja is not in our runtime path at any point. Conformance to
    it measures agreement with a bug we already route around.
  * The model's idea of a correctly formatted conversation was fixed during
    training, by CPython Jinja2.

So `oracle_hf.py` gates and `/apply-template` reports. Demoting the server diff
is not discarding it: "how would llama.cpp render this" is real interop
information, and the day someone points this harness at a stock llama.cpp
chat-completions endpoint it becomes load-bearing again. It just is not
correctness.

WHAT IT COSTS TO RUN
====================

Nothing. The authority needs the template text and jinja2: no GPU, no model
weights, no server, no port. Run it on a laptop, in CI, next to a busy box
without touching it.

    tests/fidelity/run_gate.py                # the whole gate, nothing else alive
    tests/fidelity/run_gate.py --interop      # adds the (non-gating) server diff
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)

import oracle_hf  # noqa: E402  (must follow the sys.path fix-up)

# Which shipped jinja each dialect is rendered from. Qwen3.8-Flash-Next and
# Qwen3.8-27B ship a byte-identical template, so a dialect name maps to a
# template file and several models map to one dialect.
TEMPLATES = {
    "glm-5.3-flash": "crates/dialect-glm/template/glm-5.3-flash.jinja",
}

# llama.cpp swaps media parts for <__media_NONCE__> before minja runs, with a
# nonce regenerated per server process. Only the INTEROP phase ever sees this:
# the HF oracle renders the template's own `emit_image()` and needs no rewriting.
MEDIA_MARKER = re.compile(r"<__media_[A-Za-z0-9]+__>")
MEDIA_CANON = "<|begin_of_image|><|image|><|end_of_image|>"


def normalise(s: str, rules: set[str]) -> str:
    if "media-marker" in rules:
        return MEDIA_MARKER.sub(MEDIA_CANON, s)
    return s


def render_cases(fixtures: list[str], profile: str, dialect: str) -> list[dict]:
    cmd = [
        "cargo", "run", "-q", "-p", "letibot-dialect-glm", "--bin", "letibot-render", "--",
        "--dialect", dialect, "--profile", profile, *fixtures,
    ]
    out = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True)
    if out.returncode != 0:
        sys.exit(f"letibot-render failed:\n{out.stderr}")
    return json.loads(out.stdout)


def label_of(c: dict) -> str:
    return f'{c["fixture"]}[{c["prefix_len"]}]{"+gen" if c["add_generation_prompt"] else ""}'


def diff_head(a: str, b: str, width: int = 90) -> str:
    i = 0
    while i < min(len(a), len(b)) and a[i] == b[i]:
        i += 1
    lo = max(0, i - width // 2)
    return (
        f"    first difference at byte {i} of {len(a)} (ours) / {len(b)} (theirs)\n"
        f"    ours  : ...{a[lo:i + width]!r}\n"
        f"    theirs: ...{b[lo:i + width]!r}"
    )


# --------------------------------------------------------------------------
# Phase 1 - the authority, and the provenance check that rides along with it
# --------------------------------------------------------------------------


def check_span_kinds(case: dict, prov: oracle_hf.Provenance) -> list[str]:
    """No `RenderSpan::Control` may originate in a DATA region.

    This is the injection property, made checkable end to end. `crates/dialect`
    tokenizes `Text` with special-token parsing OFF, so a control token can only
    enter the prompt through a `Control` span. If a `Control` span covers bytes
    the oracle says came from message or tool payload, then something a user, a
    tool result or an MCP server wrote has been promoted into a turn boundary.

    The check has teeth precisely because the rendered *string* cannot tell you
    this. In the `adversarial-control-literals` fixture the bytes `<|assistant|>`
    appear four times: twice because the user typed them and twice because the
    template emitted them. Byte equality is blind to the difference; provenance
    is not.

    Only `Control` spans are checked. A `Text` span may hold template literals -
    "# Tools" is template text that is correctly text - and that direction is
    harmless: text is never promoted to a special token.
    """
    problems: list[str] = []
    off = 0
    for span in case["spans"]:
        literal = span.get("control")
        is_control = literal is not None
        if literal is None:
            literal = span["text"]
        end = off + len(literal)
        if is_control:
            kinds = prov.kinds_over(off, end)
            if oracle_hf.DATA in kinds:
                problems.append(
                    f"    RenderSpan::Control({literal!r}) at byte {off} covers bytes the\n"
                    f"    oracle attributes to SUBSTITUTED DATA. Something from the\n"
                    f"    conversation has been promoted into a control token."
                )
        off = end
    if off != len(prov.text):
        problems.append(
            f"    spans concatenate to {off} bytes but the render is {len(prov.text)}: "
            f"the span list is not a partition of the prompt."
        )
    return problems


def phase_authority(
    cases: list[dict], template: str, verbose: bool
) -> tuple[list[str], int, int, set[str]]:
    """`faithful` must equal the training runtime, and its spans must agree with it."""
    failures: list[str] = []
    exact = 0
    checked_spans = 0
    structural_keys: set[str] = set()
    for c in cases:
        structural_keys.update(
            oracle_hf.literal_data_keys(
                c["request"].get("messages", []), c["request"].get("tools")
            )
        )
        lab = label_of(c)
        try:
            prov = oracle_hf.render_request(template, c["request"])
        except oracle_hf.ProvenanceError as e:
            failures.append(
                f"  FAIL {lab}\n"
                f"    the provenance map could not be established, so neither the\n"
                f"    origin check nor its guarantee is available for this case.\n"
                + "\n".join("    " + line for line in str(e).splitlines())
            )
            continue
        except Exception as e:  # a template that raises is a fixture we cannot serve
            failures.append(f"  FAIL {lab}\n    the template raised: {e!r}")
            continue

        if c["rendered"] != prov.text:
            failures.append(
                f"  FAIL {lab}\n"
                f"    our `faithful` render differs from the training runtime.\n"
                f"    This is the authority: what CPython Jinja2 produces is the format\n"
                f"    the model was trained to recognise. A difference here is ours.\n"
                f"{diff_head(c['rendered'], prov.text)}"
            )
            continue
        exact += 1

        problems = check_span_kinds(c, prov)
        checked_spans += sum(1 for s in c["spans"] if "control" in s)
        if problems:
            failures.append(f"  FAIL {lab}\n" + "\n".join(problems))
        elif verbose:
            print(f"  ok   {lab}  ({prov.pairs} data regions)")
    return failures, exact, checked_spans, structural_keys


# --------------------------------------------------------------------------
# Phase 2 - interop. Reports, never gates.
# --------------------------------------------------------------------------


def apply_template(addr: str, body: dict) -> str:
    req = urllib.request.Request(
        addr.rstrip("/") + "/apply-template",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)["prompt"]


def phase_interop(addr: str, cases: list[dict], template: str, verbose: bool) -> None:
    """What llama.cpp's own renderer would have produced.

    Nothing here can fail the gate. It answers "if we sent this conversation to a
    stock llama.cpp chat-completions endpoint, where would it land differently",
    which is worth knowing and is not a statement about whether our renderer is
    right.

    The comparison is oracle-vs-server, not ours-vs-server, because our render
    has already been proved equal to the oracle in phase 1 and because that
    framing puts the finding where it belongs: the difference is between two
    Jinja engines, not between us and anybody.

    It also carries what the removed PROFILES phase used to check. A fixture that
    DECLARES a divergence and no longer produces one against the real server means
    either llama.cpp fixed the bug or the fixture stopped reaching it, and either
    way the declaration has become decoration. That is reported, not gated: it
    needs a server, and a check that only runs when somebody remembered to start
    one must not be allowed to be the thing that says a render is correct.
    """
    agree = 0
    diffs: collections.Counter = collections.Counter()
    declared_div: dict[str, set[str]] = {}
    fired_div: collections.Counter = collections.Counter()
    declared_norm: set[str] = set()
    fired_norm: set[str] = set()
    for c in cases:
        declared_div.setdefault(c["fixture"], set()).update(c["divergences"])
        rules = set(c.get("normalise", []))
        declared_norm |= rules
        try:
            theirs = apply_template(addr, c["request"])
        except urllib.error.HTTPError as e:
            print(f"  interop unavailable: /apply-template returned {e.code}: "
                  f"{e.read().decode('utf-8', 'replace')[:200]}")
            return
        except urllib.error.URLError as e:
            print(
                f"  interop unavailable: cannot reach {addr}: {e.reason}\n"
                "    It needs a DIRECTLY LAUNCHED single-model server, not the router:\n"
                "    /apply-template is proxied in router mode (server.cpp:239).\n"
                "    tests/fidelity/serve_oracle.sh start   (and stop it when done)"
            )
            return
        ours = c["rendered"]
        if ours == theirs:
            # Tried raw first, deliberately: a normalisation that was never needed
            # is a place a real difference could hide, and the only way to notice
            # is to see whether the rule ever fires.
            agree += 1
            continue
        if rules and normalise(ours, rules) == normalise(theirs, rules):
            fired_norm |= rules
            agree += 1
            continue
        diffs[c["fixture"]] += 1
        fired_div[c["fixture"]] += 1
        if verbose:
            print(f"  minja differs: {label_of(c)}\n{diff_head(ours, theirs)}")

    print(f"  {agree}/{len(cases)} identical to llama.cpp's renderer")
    if diffs:
        print("  minja differs on: " + ", ".join(f"{k} ({v})" for k, v in sorted(diffs.items())))
        print("  This is interop information. See oracle_hf.py for why it does not gate.")
    for fx, ids in sorted(declared_div.items()):
        if ids and not fired_div[fx]:
            print(f"  note: {fx} declares {sorted(ids)} but renders identically to the server.")
            print("  Either llama.cpp fixed it or the fixture stopped reaching it. A stale")
            print("  exception is how a gate rots into decoration - remove it or find out why.")
        elif fired_div[fx] and not ids:
            print(f"  note: {fx} declares no divergence but the server differs on it "
                  f"({fired_div[fx]} case(s)).")
    stale = declared_norm - fired_norm
    if stale:
        print(f"  note: normalisation rules declared but never needed: {sorted(stale)}")
        print("  An unnecessary rewrite is a place a real difference could hide. Either the")
        print("  server stopped emitting the unreproducible thing, or no fixture reaches it.")


# --------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--dialect", default="glm-5.3-flash")
    ap.add_argument("--template", default=None, help="override the shipped jinja path")
    ap.add_argument("--fixtures", default=os.path.join(HERE, "fixtures"))
    ap.add_argument("--only", default=None, help="substring match on fixture name")
    ap.add_argument("--interop", action="store_true",
                    help="also diff against llama.cpp /apply-template (never gates)")
    ap.add_argument("--addr", default=os.environ.get("LETIBOT_ORACLE", "http://127.0.0.1:8137"))
    ap.add_argument("--skip-self-test", action="store_true")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    tpath = args.template or TEMPLATES.get(args.dialect)
    if tpath is None:
        sys.exit(f"no template registered for dialect {args.dialect!r}; pass --template")
    if not os.path.isabs(tpath):
        tpath = os.path.join(REPO, tpath)
    with open(tpath, encoding="utf-8") as fh:
        template = fh.read()

    files = sorted(
        os.path.join(args.fixtures, f)
        for f in os.listdir(args.fixtures)
        if f.endswith(".json") and (args.only is None or args.only in f)
    )
    if not files:
        sys.exit(f"no fixtures in {args.fixtures}")

    rc = 0

    if not args.skip_self_test:
        # The oracle checks the renderer; this checks the oracle. Running it first
        # means a broken oracle reports itself instead of blaming the renderer.
        st = subprocess.run([sys.executable, os.path.join(HERE, "test_oracle_hf.py")],
                            capture_output=True, text=True)
        print(f"=== oracle self-test ===\n{st.stdout.strip()}")
        if st.returncode != 0:
            print(st.stderr.strip())
            print("\nGATE FAIL (the oracle is broken; nothing it said about the renderer counts)")
            return 1

    print(f"\ncorpus: {len(files)} fixtures, template: {os.path.relpath(tpath, REPO)}")
    print(f"authority: CPython Jinja2 as transformers {oracle_hf.TRANSFORMERS_REFERENCE} drives it "
          f"(no server, no model, no GPU)")

    faithful = render_cases(files, "faithful", args.dialect)
    print(f"\n=== AUTHORITY + PROVENANCE: {len(faithful)} cases ===")
    failures, exact, spans, keys = phase_authority(faithful, template, args.verbose)
    for f in failures:
        print(f)
    print(f"  {exact}/{len(faithful)} byte-identical to the training runtime; "
          f"{spans} control spans checked against the provenance map")
    # The one place the map is deliberately incomplete: dictionary keys the template
    # looks up by name cannot be wrapped without changing control flow, so they stay
    # LITERAL. Printed rather than assumed, so an unexpected one is noticed.
    print(f"  classified LITERAL by construction (structural keys): {sorted(keys)}")
    if failures:
        rc = 1

    if args.interop:
        print(f"\n=== INTEROP (reports only): {args.addr} ===")
        phase_interop(args.addr, faithful, template, args.verbose)

    print("\nGATE PASS" if rc == 0 else "\nGATE FAIL")
    return rc


if __name__ == "__main__":
    sys.exit(main())
