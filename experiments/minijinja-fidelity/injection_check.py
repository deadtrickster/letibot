#!/usr/bin/env python3
"""Does minijinja's provenance map actually SEPARATE forged control tokens from
real ones?

`regions_equal` says minijinja's map matches the authority's. This says the map
is not vacuous: across the corpus, control-token literals appear in the rendered
prompt from BOTH origins, and the map classifies each occurrence by where it
came from rather than by how it is spelled. If every occurrence were LITERAL the
check would pass while guaranteeing nothing.
"""
from __future__ import annotations
import json, os, subprocess, sys, collections

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(REPO, "tests", "fidelity"))
sys.path.insert(0, HERE)
import oracle_hf, compare as C  # noqa: E402

LITERALS = {
    "glm-5.3-flash": ["[gMASK]", "<sop>", "<|system|>", "<|user|>", "<|assistant|>",
                      "<|observation|>", "<think>", "</think>", "<tool_call>",
                      "</tool_call>", "<arg_key>", "<arg_value>", "<|begin_of_image|>"],
    "qwen3.8-flash-next": ["<|im_start|>", "<|im_end|>", "<think>", "</think>",
                           "<tool_call>", "</tool_call>", "<tool_response>",
                           "</tool_response>", "<|vision_start|>", "<|image_pad|>"],
}


def main(paths):
    cases = []
    for p in paths:
        cases += json.load(open(p, encoding="utf-8"))
    for dialect, lits in LITERALS.items():
        template = open(C.TEMPLATES[dialect], encoding="utf-8").read()
        jobs, meta = C.build_jobs(cases)
        proc = subprocess.run([C.BIN],
                              input=json.dumps({"template": template, "jobs": jobs}, ensure_ascii=False),
                              capture_output=True, text=True)
        rust = {r["name"]: r for r in json.loads(proc.stdout)}
        counts = collections.Counter()
        per_lit = collections.defaultdict(lambda: [0, 0])  # [literal_origin, data_origin]
        mixed = 0
        for m in meta:
            rs = rust[m["name"]]
            if "clean" not in rs or "wrapped" not in rs:
                continue
            o, c = m["sentinels"]
            try:
                stripped, regions, _ = C.strip_and_map(rs["wrapped"], o, c)
            except oracle_hf.ProvenanceError:
                continue
            if stripped != rs["clean"]:
                continue
            prov = oracle_hf.Provenance(text=rs["clean"], regions=regions)
            for lit in lits:
                start = 0
                while True:
                    i = rs["clean"].find(lit, start)
                    if i < 0:
                        break
                    start = i + 1
                    kinds = prov.kinds_over(i, i + len(lit))
                    if kinds == {oracle_hf.LITERAL}:
                        per_lit[lit][0] += 1
                        counts["template-emitted"] += 1
                    elif kinds == {oracle_hf.DATA}:
                        per_lit[lit][1] += 1
                        counts["payload-supplied"] += 1
                    else:
                        mixed += 1
                        counts["straddles-a-boundary"] += 1
        print(f"\n=== {dialect} ===")
        for k, v in counts.items():
            print(f"  {k:24s} {v}")
        both = [l for l, (a, b) in per_lit.items() if a and b]
        only_lit = [l for l, (a, b) in per_lit.items() if a and not b]
        print(f"  literals seen from BOTH origins ({len(both)}): {both}")
        print(f"  literals seen only template-emitted: {only_lit}")
        if mixed:
            print(f"  WARNING: {mixed} occurrences straddle a LITERAL/DATA boundary")


if __name__ == "__main__":
    main(sys.argv[1:])
