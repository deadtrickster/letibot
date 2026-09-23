#!/usr/bin/env python3
"""**R35's numbers: how many python heredoc edits gain a write, and how many gain a prompt.**

The second number is the one that decides affordability, and it is the number the first
version of the capability scan got wrong: **1,854 new prompts and 893 new blocks**, from
counting every host and every secret path mentioned in a body.

This runs the operator's own corpus through layer A twice — before and after — by building
the two binaries from git and classifying every `python3 - <<'PY'` command in the store.

    python3 heredoc-writes-2026-09-23.py [sessions-store-jsonl]

**Before and after are GIT REVISIONS, not a flag.** `--before 51977f6` checks out the
parent commit, builds `classify`, sweeps, restores, builds again and sweeps — because the
only honest "before" is the classifier as it was, and a flag that turned the scan off would
be a second implementation of it.
"""

import json
import os
import re
import subprocess
import sys

REPO = "/home/dead/Projects/letibot/letibot"
SRC = os.path.expanduser("~/.local/share/letibot/etalon.jsonl")
BEFORE = sys.argv[2] if len(sys.argv) > 2 else "51977f6"
CMD = sys.argv[1] if len(sys.argv) > 1 else None


def commands():
    """Every bash command in the store that hands a program to an interpreter on stdin."""
    out = []
    for line in open(SRC, encoding="utf-8"):
        d = json.loads(line)
        if d.get("tool") != "bash":
            continue
        c = (d.get("arguments") or {}).get("command") or ""
        if re.search(r"python3?\s+-\s*<<|python3?\s*<<", c):
            out.append(c)
    return out


def classify(cmds, rev=None):
    """Layer A's reading of each command, at `rev`."""
    if rev:
        subprocess.run(["git", "checkout", "-q", rev], cwd=REPO, check=True)
    subprocess.run(
        ["cargo", "build", "-q", "-p", "letibot-tools", "--example", "classify"],
        cwd=REPO, check=True,
    )
    blob = "\0".join(cmds).encode()
    p = subprocess.run(
        [f"{REPO}/target/debug/examples/classify"], input=blob, capture_output=True, check=True
    )
    out = []
    for line in p.stdout.decode().split("\n"):
        if not line.startswith("reads="):
            continue
        out.append(
            (
                re.search(r"tier=(\S+)\s", line).group(1),
                "write_file" in line.split("intents=")[1].split("]")[0],
                "secret" in line.split("regions=")[1].split("]")[0],
            )
        )
    return out


def main():
    cmds = commands()
    print(f"python heredoc commands in the store: {len(cmds)}")
    after = classify(cmds)
    if not os.environ.get("SKIP_BEFORE"):
        before = classify(cmds, rev=BEFORE)
        subprocess.run(["git", "checkout", "-q", "-"], cwd=REPO, check=True)
        subprocess.run(
            ["cargo", "build", "-q", "-p", "letibot-tools", "--example", "classify"],
            cwd=REPO, check=True,
        )
    else:
        before = after

    def n(rows, i):
        return sum(1 for r in rows if r[i])

    print()
    print(f"  write intent     before {n(before,1):6}  after {n(after,1):6}")
    print(f"  GAIN a write     {sum(1 for x, y in zip(before, after) if not x[1] and y[1])}")
    print(f"  secret region    before {n(before,2):6}  after {n(after,2):6}")
    print()
    for label, rows in (("before", before), ("after ", after)):
        from collections import Counter
        print(f"  tiers {label} {dict(Counter(r[0] for r in rows))}")
    gp = [i for i, (x, y) in enumerate(zip(before, after))
          if x[0] not in ("always_ask", "blocked") and y[0] in ("always_ask", "blocked")]
    gb = [i for i, (x, y) in enumerate(zip(before, after)) if x[0] != "blocked" and y[0] == "blocked"]
    lo = [i for i, (x, y) in enumerate(zip(before, after))
          if x[0] in ("always_ask", "blocked") and y[0] not in ("always_ask", "blocked")]
    print()
    print(f"  GAIN a PROMPT    {len(gp)}   (of which a BLOCK: {len(gb)})")
    print(f"  LOSE a prompt    {len(lo)}")
    print(f"  rows whose tier changed at all: {sum(1 for x, y in zip(before, after) if x[0] != y[0])}")
    print()
    print("  the rows that gained a prompt, and they are one cause:")
    for i in gp:
        print(f"    {cmds[i].replace(chr(10), ' ')[:110]}")


if __name__ == "__main__":
    main()
