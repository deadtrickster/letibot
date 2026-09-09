#!/usr/bin/env python3
"""The renderer-fidelity check for `letibot-dialect-qwen`.

    crates/dialect-qwen/fidelity.py            # the corpus, byte equality + provenance
    crates/dialect-qwen/fidelity.py -v         # print the first difference per failure

Same authority, same two phases and the *same code* as `tests/fidelity/run_gate.py`:
`oracle_hf.render_request` renders the shipped jinja through CPython Jinja2 as
`transformers` drives it, and `run_gate.phase_authority` compares. This file is a
thirty-line driver, not a second gate.

WHY IT IS A SEPARATE FILE
=========================

`run_gate.py` hardcodes `cargo run -p letibot-dialect-glm --bin letibot-render`, and
that file may not be modified by this strand. Teaching that binary about Qwen would
make `dialect-glm` depend on `dialect-qwen`, so building GLM would pull in a model it
has nothing to do with. Everything else — the oracle, the provenance map, the span
check, the failure formatting — is imported rather than reimplemented, so there is
one authority and it is the one that already gates GLM.

WHAT THIS IS NOT
================

It is not the M0 gate and it does not replace it. `tests/fidelity/run_gate.py` is
139/139 on GLM and is untouched by this strand. This is a second corpus, on a second
dialect, reported as its own number — and one of its fixtures is EXPECTED TO FAIL:
`two-tool-calls` declares a divergence the renderer really has (see
`crates/dialect-qwen/src/render.rs`). A declared divergence that stops reproducing is
reported too, because a divergence that quietly went away is one somebody may have
"fixed" by changing the wrong thing.

    exit 0   every non-declared case is byte-identical to the training runtime
    exit 1   something diverged that was not declared
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
GATE = os.path.join(REPO, "tests", "fidelity")
sys.path.insert(0, GATE)

import oracle_hf  # noqa: E402
import run_gate  # noqa: E402

TEMPLATE = os.path.join(HERE, "template", "qwen3.8-flash-next.jinja")
FIXTURES = os.path.join(HERE, "fixtures")


def render_cases(fixtures: list[str]) -> list[dict]:
    cmd = [
        "cargo", "run", "-q", "-p", "letibot-dialect-qwen",
        "--bin", "letibot-render-qwen", "--", *fixtures,
    ]
    out = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True)
    if out.returncode != 0:
        sys.exit(f"letibot-render-qwen failed:\n{out.stderr}")
    return json.loads(out.stdout)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--fixtures", default=FIXTURES)
    ap.add_argument("--template", default=TEMPLATE)
    ap.add_argument("--only", default=None)
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    with open(args.template, encoding="utf-8") as fh:
        template = fh.read()

    files = sorted(
        os.path.join(args.fixtures, f)
        for f in os.listdir(args.fixtures)
        if f.endswith(".json") and (args.only is None or args.only in f)
    )
    if not files:
        sys.exit(f"no fixtures in {args.fixtures}")

    all_cases = render_cases(files)
    # A prefix ending inside an assistant turn has no oracle: a message list cannot
    # say "not finished". Counted and printed, never silently dropped.
    cases = [c for c in all_cases if not c.get("no_oracle")]
    reasons = sorted({c["no_oracle"] for c in all_cases if c.get("no_oracle")})
    no_oracle = len(all_cases) - len(cases)
    declared = {c["fixture"] for c in cases if c.get("divergences")}

    print(f"corpus: {len(files)} fixtures, template: "
          f"{os.path.relpath(args.template, REPO)}")
    print(f"authority: CPython Jinja2 as transformers "
          f"{oracle_hf.TRANSFORMERS_REFERENCE} drives it")
    if no_oracle:
        print(f"no oracle available for {no_oracle} of {len(all_cases)} cases "
              f"({', '.join(reasons)}): apply_chat_template closes every message it "
              f"renders, so it cannot express a prefix that stops inside an assistant "
              f"turn; and it refuses an empty message list outright")
    if declared:
        print(f"declared divergences: {sorted(declared)} — failures there are expected "
              f"and do not gate; their ABSENCE is reported")

    # Split so a declared divergence cannot hide an undeclared one in the same run.
    gating = [c for c in cases if c["fixture"] not in declared]
    waived = [c for c in cases if c["fixture"] in declared]

    print(f"\n=== AUTHORITY + PROVENANCE: {len(gating)} gating cases ===")
    failures, exact, spans, keys = run_gate.phase_authority(gating, template, args.verbose)
    for f in failures:
        print(f)
    print(f"  {exact}/{len(gating)} byte-identical to the training runtime; "
          f"{spans} control spans checked against the provenance map")
    print(f"  classified LITERAL by construction (structural keys): {sorted(keys)}")
    rc = 1 if failures else 0

    if waived:
        print(f"\n=== DECLARED DIVERGENCE: {len(waived)} cases (reports only) ===")
        wf, wexact, _, _ = run_gate.phase_authority(waived, template, args.verbose)
        print(f"  {wexact}/{len(waived)} byte-identical; {len(wf)} differ as declared")
        if not wf:
            print("  NOTE: the declared divergence did not reproduce on any case. "
                  "Either the corpus no longer reaches it or the renderer changed; "
                  "check before deleting the declaration.")

    print("\nQWEN FIDELITY PASS" if rc == 0 else "\nQWEN FIDELITY FAIL")
    return rc


if __name__ == "__main__":
    sys.exit(main())
