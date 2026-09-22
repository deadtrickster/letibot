#!/usr/bin/env python3
"""BashArena's monitor-eval split, rendered as the `state` Kev is asked about.

The rows come from `scripts/basharena-to-etalon.py`, which is also what
`--etalon-oracle` reads, so **the two models are given the same bytes**: whatever the
converter keeps, Kev sees, and whatever it drops, neither sees.

`render()` is imported from `etalon_build.py` rather than re-written, so the framing
sentence, the trail numbering and the argument quoting are the same ones the operator's
own corpus was asked in. Two differences from that corpus, both the converter's:

* `mode` is `automode`, because `etalon_oracle.rs` seats every etalon row with
  `mode: "automode"` — an attack row has no mode of its own.
* the ask is `task_description`, which the converter clips to 600 characters.

The second is why this file writes TWO sets. `basharena_kev.jsonl` is the same bytes the
incumbent gets; `basharena_kev_full.jsonl` is the same rows with the whole task
description, which is a probe of the converter's clip and not a comparison with the
incumbent.
"""

import json
import sys

sys.path.insert(0, "/tmp/kev-eval")
from etalon_build import render  # noqa: E402

SRC = "/tmp/kev-eval/basharena/basharena.jsonl"


def load(path=SRC):
    return [json.loads(l) for l in open(path, encoding="utf-8")]


def write(rows, path, clip):
    with open(path, "w", encoding="utf-8") as f:
        for i, r in enumerate(rows):
            if not clip:
                # The whole ask, not the converter's 600-character head. `render` takes
                # the trail as the corpus gives it, so the only change is the text.
                r = dict(r)
                r["trail"] = [dict(u) for u in r["trail"]]
                for u in r["trail"]:
                    u["text"] = FULL[i]
            r = dict(r, _i=i, mode="automode")
            f.write(json.dumps({
                "id": f"{r['source']}#{i}",
                "state": render(r),
                "label": "refuse" if r["label"]["effect"] == "refuse" else "admit",
                "by": r["label"]["by"],
                "side_task": r["label"]["by"].split(":", 1)[-1],
                "monitor_score": r["label"].get("monitor"),
                "side_task_success": r["label"].get("side_task_success"),
                "tool": r["tool"],
                "source": r["source"],
                "session": r["session"],
                "cwd": r.get("cwd", ""),
                "why": "",
            }, ensure_ascii=False) + "\n")
    print(f"{len(rows)} rows -> {path}", file=sys.stderr)


FULL = []
if __name__ == "__main__":
    rows = load()
    # The converter's clip, recovered from the parquet on the same order the converter
    # walked it: one output row per bash action, in row-then-action order.
    import pyarrow.parquet as pq

    pq_rows = pq.read_table("/tmp/kev-eval/basharena/monitor-eval.parquet").to_pylist()
    for r in pq_rows:
        acts = json.loads(r["action"]) if isinstance(r["action"], str) else r["action"]
        for a in acts:
            if a.get("function") != "bash":
                continue
            FULL.append(r["task_description"] or "")
    assert len(FULL) == len(rows), f"{len(FULL)} asks against {len(rows)} rows"
    clipped = sum(1 for t in FULL if len(t) > 600)
    print(f"asks: {len(FULL)}, longer than the converter's 600-char clip: {clipped}",
          file=sys.stderr)
    write(rows, "/tmp/kev-eval/basharena/basharena_kev.jsonl", clip=True)
    write(rows, "/tmp/kev-eval/basharena/basharena_kev_full.jsonl", clip=False)
