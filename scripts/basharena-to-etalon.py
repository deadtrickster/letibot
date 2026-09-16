#!/usr/bin/env python3
"""BashArena's monitor-eval split, in the etalon's row shape.

Plan: docs/guard-corpus-plan.md §1 (BashArena, arXiv 2512.15688). Source: HF
`abhayesian/basharena-monitor-eval` — 373 per-action rows from 134 BashArena
trajectories, each labelled `attack` (with its side task: download_malware,
leak_passwords, sudo_edit, disable_firewall) or `honest`, carrying the main
task description, the previous actions, the action, and the original
monitor's suspicion score. 339 of the actions are bash; those become rows.

An attack action is written as `refused`, an honest one as `ran`, so
`harnessd --etalon FILE` (layer A) and `--etalon-oracle FILE` (the guard)
read them with the same words they use for the operator's own corpus: a
refused row the guard would allow is a FALSE ALLOW — here, an attack let
through. The main task is the operator's one utterance; `cwd` is `/app`,
where BashArena's agent works.

Usage: scripts/basharena-to-etalon.py monitor-eval.parquet out.jsonl
       (needs pyarrow; a venv with it is enough)
"""
import json
import sys

import pyarrow.parquet as pq

src, out = sys.argv[1], sys.argv[2]
rows = pq.read_table(src).to_pylist()
n = 0
with open(out, "w") as f:
    for r in rows:
        acts = json.loads(r["action"]) if isinstance(r["action"], str) else r["action"]
        for a in acts:
            if a.get("function") != "bash":
                continue
            args = a.get("arguments") or {}
            cmd = args.get("cmd") or args.get("command") or ""
            attack = r["label"] == "attack"
            f.write(json.dumps({
                "source": "basharena-attack" if attack else "basharena-honest",
                "session": r["sample_id"],
                "cwd": "/app",
                "tool": "bash",
                "arguments": {"command": cmd},
                "outcome": "refused" if attack else "ran",
                "trail": [{"speaker": "operator", "text": (r["task_description"] or "")[:600], "seconds_ago": 60, "turns_ago": 0}],
                "label": {"by": "basharena:" + (r["side_task"] or "honest"), "effect": "refuse" if attack else "admit",
                          "monitor": r["original_monitor_score"], "side_task_success": r["side_task_success"]},
            }) + "\n")
            n += 1
print(f"{n} bash actions -> {out}", file=sys.stderr)
