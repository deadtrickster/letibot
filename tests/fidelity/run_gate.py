#!/usr/bin/env python3
"""The W3 renderer-fidelity gate.

    ours = spans_to_string(D.render(F))          via `letibot-render`
    theirs = POST /apply-template(F).prompt
    assert ours == theirs                        exact string equality

Run it:

    tests/fidelity/serve_oracle.sh start          # or point --addr at a real server
    tests/fidelity/run_gate.py --addr http://127.0.0.1:8137

It runs the whole corpus **twice**, and that is the interesting part.

  * profile `server-bug-compatible` — our renderer with every known oracle quirk
    switched on. This must match byte-for-byte on every fixture and every prefix,
    with no exceptions at all. Passing it is the claim "we have a complete model of
    what the server's renderer does".

  * profile `faithful` — what the harness actually emits. Here a fixture may declare
    a divergence, and the check is **two-sided**: an undeclared mismatch fails, and a
    declared divergence that no longer reproduces fails just as hard, because a
    stale exception is how a gate rots into decoration.

Exit status is 0 only if both profiles come out as expected.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))

# llama.cpp swaps media parts for <__media_NONCE__> before the template runs, with a
# nonce regenerated per server process. Both sides are folded onto the same token
# triplet so the rest of an image fixture is still compared exactly.
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


def apply_template(addr: str, body: dict) -> str:
    req = urllib.request.Request(
        addr.rstrip("/") + "/apply-template",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return json.load(r)["prompt"]
    except urllib.error.HTTPError as e:
        detail = e.read().decode("utf-8", "replace")[:400]
        raise SystemExit(
            f"/apply-template returned {e.code}: {detail}\n"
            "  A 4xx here is usually the request body, not the renderer."
        )
    except urllib.error.URLError as e:
        raise SystemExit(
            f"cannot reach {addr}: {e.reason}\n"
            "  The gate needs a DIRECTLY LAUNCHED single-model server, not the router:\n"
            "  /apply-template is proxied in router mode (server.cpp:239).\n"
            "  tests/fidelity/serve_oracle.sh start"
        )


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


def run_profile(addr: str, cases: list[dict], profile: str, verbose: bool) -> tuple[int, int, list[str], set[str]]:
    ok = 0
    failures: list[str] = []
    fired: set[str] = set()
    for c in cases:
        divergences = set(c["divergences"])
        rules = set(c.get("normalise", []))
        ours_raw = c["rendered"]
        theirs_raw = apply_template(addr, c["request"])
        label = (
            f'{c["fixture"]}[{c["prefix_len"]}]'
            f'{"+gen" if c["add_generation_prompt"] else ""}'
        )

        if ours_raw == theirs_raw:
            # Exact, with no rewriting of either side. This is the only outcome that
            # needs no explanation, which is why it is tested before the rules apply.
            ok += 1
            if verbose:
                print(f"  ok   {label}")
            continue

        ours = normalise(ours_raw, rules)
        theirs = normalise(theirs_raw, rules)
        if rules and ours == theirs:
            fired |= rules
            ok += 1
            print(f"  NORMALISED ({', '.join(sorted(rules))}) {label}")
            continue

        if profile == "server-bug-compatible":
            failures.append(
                f"  FAIL {label}\n"
                f"    the quirked profile must match the oracle exactly, with no exceptions:\n"
                f"    it is the evidence that our renderer models the oracle completely,\n"
                f"    and it is what earns the faithful profile the right to declare a difference.\n"
                f"{diff_head(ours, theirs)}"
            )
        elif divergences:
            fired |= divergences
            print(f"  DIVERGES (declared: {', '.join(sorted(divergences))}) {label}")
            if verbose:
                print(diff_head(ours, theirs))
        else:
            failures.append(
                f"  FAIL {label}\n"
                f"    undeclared divergence from the shipped template.\n"
                f"{diff_head(ours, theirs)}"
            )
    return ok, len(cases), failures, fired


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--addr", default=os.environ.get("LETIBOT_ORACLE", "http://127.0.0.1:8137"))
    ap.add_argument("--dialect", default="glm-5.3-flash")
    ap.add_argument("--fixtures", default=os.path.join(HERE, "fixtures"))
    ap.add_argument("--only", default=None, help="substring match on fixture name")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    files = sorted(
        os.path.join(args.fixtures, f)
        for f in os.listdir(args.fixtures)
        if f.endswith(".json") and (args.only is None or args.only in f)
    )
    if not files:
        sys.exit(f"no fixtures in {args.fixtures}")

    declared: set[str] = set()
    for f in files:
        with open(f) as fh:
            fx = json.load(fh)
            for d in fx.get("divergences", []) + fx.get("normalise", []):
                declared.add(d["id"])

    print(f"corpus: {len(files)} fixtures, oracle: {args.addr}")
    rc = 0
    all_fired: set[str] = set()
    for profile in ("server-bug-compatible", "faithful"):
        cases = render_cases(files, profile, args.dialect)
        print(f"\n=== profile {profile}: {len(cases)} cases ===")
        ok, total, failures, fired = run_profile(args.addr, cases, profile, args.verbose)
        all_fired |= fired
        for f in failures:
            print(f)
        print(f"  {ok}/{total} exact" + (f", {len(failures)} FAILED" if failures else ""))
        if failures:
            rc = 1

    # The other half of the two-sided check. A declared divergence that stopped
    # reproducing means either the server was fixed or the fixture no longer reaches
    # the code path; either way the exception is now a lie and must be removed.
    stale = declared - all_fired
    if stale:
        print(f"\nSTALE EXCEPTIONS (declared but no longer reproducing): {sorted(stale)}")
        print("  Remove them from the fixture, or find out why the fixture stopped reaching them.")
        rc = 1

    print("\nGATE PASS" if rc == 0 else "\nGATE FAIL")
    return rc


if __name__ == "__main__":
    sys.exit(main())
