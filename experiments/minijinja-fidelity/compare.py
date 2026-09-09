#!/usr/bin/env python3
"""Diff **minijinja** against the CPython-Jinja2 oracle, on the real templates
and the real fixture corpus.

The two engines are handed byte-identical arguments: this script does the
request adaptation once, in Python, using `tests/fidelity/oracle_hf.py`, and
ships the resulting kwargs to the Rust binary as JSON. So a difference in the
output is a difference in the ENGINE (or in the filters we had to re-implement),
never in the driver.

Four things are measured per (template, case):

  clean      minijinja's render == CPython's render, byte for byte
  error      when CPython raises, minijinja must raise too (a template that
             refuses a malformed conversation is a feature; refusing on one
             engine and not the other is a divergence)
  strip      the sentinel-wrapped render, with sentinels removed, reproduces
             minijinja's own clean render  -- the provenance self-validation,
             run against minijinja rather than CPython
  regions    minijinja's LITERAL/DATA map == CPython's, region for region
"""
from __future__ import annotations
import json, os, re, subprocess, sys, argparse

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(REPO, "tests", "fidelity"))
import oracle_hf  # noqa: E402

BIN = os.path.join(HERE, "target", "debug", "render")

TEMPLATES = {
    "glm-5.3-flash": os.path.join(REPO, "crates/dialect-glm/template/glm-5.3-flash.jinja"),
    "qwen3.8-flash-next": os.path.join(HERE, "templates/qwen3.8-flash-next.jinja"),
}


def build_jobs(cases):
    """One job per case: the clean kwargs and the sentinel-wrapped kwargs."""
    jobs, meta = [], []
    for i, c in enumerate(cases):
        msgs, tools, docs, agp, rest = oracle_hf._split_request(c["request"])
        o, cl = oracle_hf.pick_sentinels([msgs, tools, docs, rest])
        args = dict(rest)
        args.update(messages=msgs, tools=tools, documents=docs, add_generation_prompt=agp)
        wargs = dict(rest)
        wargs.update(
            messages=oracle_hf.wrap_messages(msgs, o, cl),
            tools=oracle_hf.wrap_tools(tools, o, cl),
            documents=docs if docs is None else oracle_hf._opaque(docs, o, cl),
            add_generation_prompt=agp,
        )
        name = f'{c["fixture"]}[{c["prefix_len"]}]{"+gen" if agp else ""}#{i}'
        jobs.append({"name": name, "args": args, "wrapped_args": wargs})
        meta.append({"name": name, "case": c, "sentinels": (o, cl),
                     "args": args, "wargs": wargs})
    return jobs, meta


def cpython_side(template, m):
    """Clean render, wrapped render and provenance under the authority."""
    out = {}
    try:
        out["clean"] = oracle_hf.render(template, **m["args"])
    except Exception as e:
        out["error"] = f"{type(e).__name__}: {e}"
    try:
        out["wrapped"] = oracle_hf.render(template, **m["wargs"])
    except Exception as e:
        out["wrapped_error"] = f"{type(e).__name__}: {e}"
    return out


def strip_and_map(wrapped, o, c):
    return oracle_hf._split_on_sentinels(wrapped, o, c)


def control_in_data(case, text, regions):
    """No `RenderSpan::Control` may cover bytes the map calls DATA.

    Same check `tests/fidelity/run_gate.py::check_span_kinds` runs, re-pointed at
    the map minijinja produced. Only meaningful where the case carries spans,
    i.e. the GLM dialect.
    """
    prov = oracle_hf.Provenance(text=text, regions=regions)
    problems, off = [], 0
    for span in case.get("spans", []):
        lit = span.get("control")
        is_control = lit is not None
        if lit is None:
            lit = span["text"]
        end = off + len(lit)
        if is_control and oracle_hf.DATA in prov.kinds_over(off, end):
            problems.append(f"Control({lit!r}) at byte {off} covers DATA")
        off = end
    if off != len(text):
        problems.append(f"spans cover {off} bytes, render is {len(text)}")
    return problems


def _cpython_map_also_void(py, o, c):
    """Did the AUTHORITY refuse a provenance map for this case too?

    A strip-back failure means the template did something to the payload that a
    sentinel cannot ride through -- compared it, trimmed it, sliced it. That is a
    property of the TEMPLATE, not of the engine, so it must be attributed to the
    right side: if CPython refuses the same case, minijinja refusing it is
    agreement, not a defect.
    """
    if "wrapped" not in py or "clean" not in py:
        return True
    try:
        st, _, _ = strip_and_map(py["wrapped"], o, c)
    except oracle_hf.ProvenanceError:
        return True
    return st != py["clean"]


def head_diff(a, b, width=70):
    i = 0
    while i < min(len(a), len(b)) and a[i] == b[i]:
        i += 1
    lo = max(0, i - 30)
    return (f"      byte {i} of {len(a)} (minijinja) / {len(b)} (cpython)\n"
            f"      minijinja: ...{a[lo:i+width]!r}\n"
            f"      cpython  : ...{b[lo:i+width]!r}")


def run(dialect, cases, verbose, limit_report=6):
    tpath = TEMPLATES[dialect]
    template = open(tpath, encoding="utf-8").read()
    jobs, meta = build_jobs(cases)
    proc = subprocess.run(
        [BIN], input=json.dumps({"template": template, "jobs": jobs}, ensure_ascii=False),
        capture_output=True, text=True)
    if proc.returncode != 0:
        return {"dialect": dialect, "fatal": proc.stderr.strip()[-4000:]}
    rust = {r["name"]: r for r in json.loads(proc.stdout)}

    tally = dict(total=0, clean_equal=0, both_raised=0,
                 both_raised_same_reason=0, both_raised_diff_reason=0,
                 strip_fail_both=0, clean_differ=0,
                 only_rust_raised=0, only_py_raised=0,
                 strip_ok=0, strip_fail=0, regions_equal=0, regions_differ=0,
                 control_in_data=0, pairs=0, wrapped_missing=0)
    reports = []

    for m in meta:
        tally["total"] += 1
        py = cpython_side(template, m)
        rs = rust[m["name"]]
        o, c = m["sentinels"]

        if "error" in py and "error" in rs:
            tally["both_raised"] += 1
            # Both refusing is only agreement if they refuse for the same reason.
            # `raise_exception('...')` messages are written by the template author,
            # so they are comparable once each engine's own framing is stripped.
            pym = py["error"].split(": ", 1)[-1].strip()
            rsm = re.sub(r"\s*\(in t:\d+\)$", "", rs["error"]).split(": ", 1)[-1].strip()
            # Two kinds of refusal, and only one is comparable. A message the
            # TEMPLATE AUTHOR wrote via raise_exception() is part of the contract
            # and must match exactly. An engine-internal type error ("'str object'
            # has no attribute 'items'") is each engine's own prose for the same
            # condition; requiring identical wording there would be measuring
            # error strings, not behaviour, so both-refused is the test.
            authored = f"raise_exception('{pym}')" in template or pym in template
            if pym == rsm or not authored:
                tally["both_raised_same_reason"] += 1
            else:
                tally["both_raised_diff_reason"] += 1
                if len(reports) < limit_report:
                    reports.append(f"  RAISED-DIFFERENT-AUTHORED-REASON {m['name']}\n"
                                   f"      cpython  : {pym}\n      minijinja: {rsm}")
            continue
        if "error" in rs and "error" not in py:
            tally["only_rust_raised"] += 1
            if len(reports) < limit_report:
                reports.append(f"  ONLY-MINIJINJA-RAISED {m['name']}\n      {rs['error']}")
            continue
        if "error" in py and "error" not in rs:
            tally["only_py_raised"] += 1
            if len(reports) < limit_report:
                reports.append(f"  ONLY-CPYTHON-RAISED {m['name']}\n      {py['error']}")
            continue

        if rs["clean"] == py["clean"]:
            tally["clean_equal"] += 1
        else:
            tally["clean_differ"] += 1
            if len(reports) < limit_report:
                reports.append(f"  CLEAN-DIFFERS {m['name']}\n{head_diff(rs['clean'], py['clean'])}")
            continue

        if "wrapped" not in rs:
            tally["wrapped_missing"] += 1
            if len(reports) < limit_report:
                reports.append(f"  WRAPPED-RAISED {m['name']}\n      {rs.get('wrapped_error')}")
            continue
        try:
            stripped, regions, pairs = strip_and_map(rs["wrapped"], o, c)
        except oracle_hf.ProvenanceError as e:
            tally["strip_fail"] += 1
            if _cpython_map_also_void(py, o, c):
                tally["strip_fail_both"] += 1
            if len(reports) < limit_report:
                reports.append(f"  SENTINELS-UNBALANCED {m['name']}"
                               f" (cpython map also void: {_cpython_map_also_void(py, o, c)})\n      {e}")
            continue
        if stripped != rs["clean"]:
            tally["strip_fail"] += 1
            both = _cpython_map_also_void(py, o, c)
            if both:
                tally["strip_fail_both"] += 1
            if len(reports) < limit_report:
                reports.append(f"  STRIP-BACK-FAILED {m['name']}"
                               f" (cpython map also void: {both})\n{head_diff(stripped, rs['clean'])}")
            continue
        tally["strip_ok"] += 1
        tally["pairs"] += pairs

        pstripped, pregions, ppairs = strip_and_map(py["wrapped"], o, c)
        if regions == pregions:
            tally["regions_equal"] += 1
        else:
            tally["regions_differ"] += 1
            if len(reports) < limit_report:
                reports.append(f"  REGIONS-DIFFER {m['name']}\n      minijinja {regions[:6]}\n      cpython   {pregions[:6]}")

        # The span list comes from the GLM dialect renderer, so the
        # Control-never-from-DATA check is only meaningful on GLM's template.
        probs = (control_in_data(m["case"], rs["clean"], regions)
                 if m["case"].get("dialect") == dialect else [])
        if probs:
            tally["control_in_data"] += 1
            if len(reports) < limit_report:
                reports.append(f"  CONTROL-FROM-DATA {m['name']}\n      " + "\n      ".join(probs))

    return {"dialect": dialect, "tally": tally, "reports": reports,
            "template": os.path.relpath(tpath, REPO)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", default=os.path.join(HERE, "cases-faithful.json"))
    ap.add_argument("--dialects", default="glm-5.3-flash,qwen3.8-flash-next")
    ap.add_argument("-v", "--verbose", action="store_true")
    a = ap.parse_args()
    cases = json.load(open(a.cases, encoding="utf-8"))
    allres = []
    for d in a.dialects.split(","):
        r = run(d, cases, a.verbose)
        allres.append(r)
        print(f"\n=== {d}  ({r.get('template')}) ===")
        if "fatal" in r:
            print("  FATAL:", r["fatal"])
            continue
        t = r["tally"]
        for k in ("total", "clean_equal", "clean_differ", "both_raised",
                  "both_raised_same_reason", "both_raised_diff_reason",
                  "only_rust_raised", "only_py_raised", "strip_ok", "strip_fail",
                  "strip_fail_both", "wrapped_missing", "regions_equal",
                  "regions_differ", "control_in_data", "pairs"):
            print(f"  {k:20s} {t[k]}")
        for line in r["reports"]:
            print(line)
    json.dump(allres, open(os.path.join(HERE, "compare-out.json"), "w"), indent=2, ensure_ascii=False)
    bad = any("fatal" in r or r["tally"]["clean_differ"] or r["tally"]["only_rust_raised"]
              or r["tally"]["only_py_raised"] or r["tally"]["both_raised_diff_reason"]
              or (r["tally"]["strip_fail"] - r["tally"]["strip_fail_both"])
              or r["tally"]["regions_differ"] or r["tally"]["control_in_data"]
              for r in allres)
    print("\nRESULT:", "DIVERGENCE" if bad else "AGREEMENT")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
