#!/usr/bin/env python3
"""The census behind R26: is the stated goal in the brief the models are given?

R26 (2026-09-23) refuted the reading that `AdjudicationRequest::brief` is the model's
brief and therefore that the goal and the decision history never reach a model. The
evidence is two artefacts that already existed — this script only counts them, so the
table is reproduced rather than trusted.

  basharena-rows-2026-09-22.jsonl   the 339 BashArena rows, with the `trail` field that
                                    `etalon_oracle.rs` turns into an `AuthorisationTrail`
                                    and `ModelBrief::render` prints as the numbered block.
  basharena-oracle-run1.txt         the incumbent's own answers, which cite `trail entry 0`.

The etalon corpus is the same question on the operator's own 138,540 rows, and it is where
the goal is *genuinely* absent (25 rows have no utterance at all). It is read only if it is
on this box, so the script is honest about which half it could run.
"""

import collections
import json
import os
import statistics
import sys

ROWS = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                    "basharena-rows-2026-09-22.jsonl")
ETALON = os.path.expanduser("~/.local/share/letibot/etalon.jsonl")


def census(path):
    counts, speakers, turns, secs, lens, heads, sessions = (
        collections.Counter(), collections.Counter(), collections.Counter(),
        collections.Counter(), [], collections.Counter(), collections.Counter(),
    )
    rows = 0
    for line in open(path, encoding="utf-8"):
        r = json.loads(line)
        rows += 1
        trail = r.get("trail") or []
        counts[len(trail)] += 1
        sessions[r.get("session")] += 1
        for u in trail:
            speakers[u.get("speaker")] += 1
            turns[u.get("turns_ago")] += 1
            secs[u.get("seconds_ago")] += 1
            lens.append(len(u.get("text") or ""))
            heads[(u.get("text") or "")[:24]] += 1
    per_session = list(sessions.values())
    return {
        "rows": rows,
        "trail_entries_per_row": counts,
        "speakers": speakers,
        "turns_ago": turns,
        "seconds_ago": secs,
        "text_len_min": min(lens), "text_len_max": max(lens),
        "text_heads": heads,
        "sessions": len(sessions),
        "rows_per_session": (min(per_session), statistics.median(per_session),
                             max(per_session)),
    }


def main():
    print("== basharena-rows-2026-09-22.jsonl ==")
    for k, v in census(ROWS).items():
        print(f"  {k}: {v}")

    if not os.path.exists(ETALON):
        print("\netalon corpus not on this box — the goal-absent case not counted here")
        return
    print(f"\n== {ETALON} ==")
    rows = zero = 0
    for line in open(ETALON, encoding="utf-8"):
        rows += 1
        if not (json.loads(line).get("trail") or []):
            zero += 1
    print(f"  rows: {rows}")
    print(f"  rows with 0 trail entries: {zero}"
          f"  ({100.0 * zero / rows:.3f}%) — the only case where the goal is absent,"
          " and the brief says `0 operator message(s) of 0 scanned`")


if __name__ == "__main__":
    sys.exit(main())
