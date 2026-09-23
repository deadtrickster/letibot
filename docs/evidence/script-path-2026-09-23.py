#!/usr/bin/env python3
"""**R39's numbers: what a script named as a PATH costs, and what it gains.**

The operator's card was `python3 r38-1.py`: layer A reported `[inspect execute_code]` and
nothing about what the file does, while the oracle's brief was already carrying the file
with *"judge THIS, not the filename"*. The deterministic half was blind to exactly the
case the brief was built for.

Measured the way R35 was measured, and the same numbers are reported:

  * how much of the corpus NAMES a script path at all — the reach of the change,
  * how the three cases the requirement names divide it (read / unreadable / not on disk
    yet because an earlier stage of the same command writes it),
  * how many rows gain a WRITE intent,
  * and **how many gain a PROMPT**, which is the number that decides affordability.

**Before and after are the same binary, with and without the bodies.** That is not the
`git checkout` dance `heredoc-writes-2026-09-23.py` does, and the difference is worth
stating: `of_command(cmd, env)` IS `of_command_with(cmd, env, &[])`, so the counterfactual
is the same function with nothing to read rather than a second implementation of the scan.
A flag that reimplemented the decision is exactly what R35's harness refused.

    python3 script-path-2026-09-23.py [--limit N]

Reads `~/.local/share/letibot/etalon.jsonl` and resolves each row's script path against
that row's own `cwd`, which is what the session it came from would have done.
"""
import json
import os
import subprocess
import sys
from collections import Counter

REPO = "/home/dead/Projects/letibot/letibot"
SRC = os.path.expanduser("~/.local/share/letibot/etalon.jsonl")
BIN = f"{REPO}/target/debug/examples/classify"

# The interpreters whose first positional is a file to run — the list `script_argument`
# holds. Used ONLY to pick which rows are worth asking about; every reading below is the
# real parser's, and `script_argument` decides what it decides.
RUNS_A_FILE = set(
    "python python2 python3 perl ruby node deno bun php lua luajit Rscript julia "
    "bash sh zsh ksh dash tclsh".split()
)


def rows():
    """Every `bash` row in the etalon, with the two facts the sweep needs."""
    for line in open(SRC, encoding="utf-8"):
        try:
            d = json.loads(line)
        except Exception:
            continue
        if d.get("tool") != "bash":
            continue
        c = (d.get("arguments") or {}).get("command") or ""
        if not c.strip():
            continue
        yield c, d.get("cwd") or "/home/dead"


def maybe_names_one(cmd):
    """A cheap pre-filter: does any word of this command follow an interpreter name?

    Deliberately generous. It is not the decision — `script_argument` is — and a row it
    lets through that names no script costs nothing but a line of output.
    """
    toks = cmd.replace("<<", " ").split()
    return any(t.rpartition("/")[2] in RUNS_A_FILE for t in toks)


def sweep(cmds, cwds, read):
    """Layer A's reading of each command, through the real example, one process."""
    blob = b""
    for c, w in zip(cmds, cwds):
        blob += w.encode() + b"\0" + c.encode() + b"\0"
    env = dict(os.environ)
    env["READ_SCRIPTS"] = "1" if read else "0"
    env["PAIRS"] = "1"
    p = subprocess.run([BIN], input=blob, capture_output=True, env=env)
    return p.stdout.decode(errors="replace")


def parse_blocks(text):
    """One (tier, intents, regions, findings-text) per command, in order.

    **Split on the `reads=` line and not on a blank line.** The example prints a blank
    line between records, and a corpus command contains blank lines of its own — the first
    version of this split on `\n\n` and produced 30,659 blocks for 30,526 commands, which
    is the kind of off-by-133 that reads as a rendering detail and is a broken join.
    """
    out = []
    cur = None
    for line in text.split("\n"):
        if line.startswith("reads="):
            if cur is not None:
                out.append(cur)
            cur = [line]
        elif cur is not None:
            cur.append(line)
    if cur is not None:
        out.append(cur)

    rows = []
    for block in out:
        tier = ""
        intents = []
        regions = []
        findings = []
        for line in block:
            if line.startswith("reads="):
                tier = line.split("tier=")[1].split()[0]
                # **Strip the quotes and commas.** `{:?}` on the intent list renders it
                # with them, and `"write_file" in intents` is then False for every row —
                # which is how the first version of this sweep reported "GAIN a write 0"
                # for 305 rows that gained one. A measurement that cannot see the field
                # it is counting reports the absence of a finding, not a finding.
                intents = [t.strip('",') for t in line.split("intents=[")[1].split("]")[0].split()]
                regions = [t.strip('",') for t in line.split("regions=[")[1].split("]")[0].split()]
            elif line.startswith("  ! "):
                findings.append(line[4:])
        rows.append((tier, intents, regions, "\n".join(findings)))
    return rows


def main():
    limit = None
    if "--limit" in sys.argv:
        limit = int(sys.argv[sys.argv.index("--limit") + 1])

    all_rows = list(rows())
    picked = [(c, w) for c, w in all_rows if maybe_names_one(c)]
    if limit:
        picked = picked[:limit]
    print(f"bash commands in the etalon:            {len(all_rows)}")
    print(f"  pre-filtered as possibly naming one:  {len(picked)}")

    cmds = [c for c, _ in picked]
    cwds = [w for _, w in picked]

    after = parse_blocks(sweep(cmds, cwds, read=True))
    before = parse_blocks(sweep(cmds, cwds, read=False))
    assert len(after) == len(cmds), f"{len(after)} blocks for {len(cmds)} commands"

    named = sum(1 for _, _, _, f in after if "read from disk as the program" in f)
    unread = sum(1 for _, _, _, f in after if "could not be read" in f)
    future = sum(
        1 for _, _, _, f in after if "not on disk when this was classified" in f
    )
    secret = sum(1 for _, _, _, f in after if "not opened to find out what it contains" in f)
    truncated = sum(1 for _, _, _, f in after if "NOT READ" in f)
    notread = sum(1 for _, _, _, f in after if "was not read" in f)
    # **A row NAMES a script path when one of the four sentences fired.** The pre-filter
    # cannot answer this — it lets 22,882 rows through that name no script — so the
    # reading itself is asked, which is the only count worth printing.
    prefiltered = len(cmds)
    names_one = sum(
        1
        for _, _, _, f in after
        if "read from disk as the program" in f
        or "could not be read" in f
        or "not on disk when this was classified" in f
        or "was not read" in f
    )

    print()
    print("  --- the cases the requirement names, as the corpus divides ---")
    print(f"  NAMING a script path:                  {names_one}")
    print(f"  READ, so the body was judged:          {named}")
    print(f"  of those, truncated at 16 KiB:         {truncated}")
    print(f"  a secret-store path, refused unopened: {secret}")
    print(f"  could not be read (missing/other):     {unread}")
    print(f"  not on disk yet: an earlier stage:     {future}")
    print(f"  no body offered at all:                {notread}")
    print(f"  (pre-filter let through {prefiltered}, of which {prefiltered - names_one} name none)")
    print()

    def w(rows_):
        return sum(1 for r in rows_ if "write_file" in r[1])

    print(f"  write intent   before {w(before):6}   after {w(after):6}")
    print(
        f"  GAIN a write   {sum(1 for x, y in zip(before, after) if 'write_file' not in x[1] and 'write_file' in y[1])}"
    )
    print()
    print(
        f"  any region     before {sum(1 for r in before if r[2]):6}   after "
        f"{sum(1 for r in after if r[2]):6}"
    )
    print()
    for label, r in (("before", before), ("after ", after)):
        print(f"  tiers {label} {dict(Counter(t for t, _, _, _ in r))}")

    rank = {"auto": 0, "may_approve": 1, "always_ask": 2, "blocked": 3, "not_run": 1}
    gp = [
        i
        for i, (x, y) in enumerate(zip(before, after))
        if rank.get(x[0], 1) < 2 and rank.get(y[0], 1) >= 2
    ]
    gb = [
        i
        for i, (x, y) in enumerate(zip(before, after))
        if x[0] != "blocked" and y[0] == "blocked"
    ]
    lo = [
        i
        for i, (x, y) in enumerate(zip(before, after))
        if rank.get(x[0], 1) >= 2 and rank.get(y[0], 1) < 2
    ]
    print()
    print(f"  GAIN a PROMPT  {len(gp)}   (of which a BLOCK: {len(gb)})")
    print(f"  LOSE a prompt  {len(lo)}")
    print(
        f"  rows whose tier moved at all: {sum(1 for x, y in zip(before, after) if x[0] != y[0])}"
    )
    print()
    if gp:
        print("  the rows that gained a prompt, and their cause:")
        for i in gp[:25]:
            cause = [l for l in after[i][3].split("\n") if "read from disk" in l or "always-ask" in l]
            print(f"    [{before[i][0]} -> {after[i][0]}] {cmds[i].replace(chr(10), ' ')[:90]}")
            for c in cause[:2]:
                print(f"        {c[:150]}")
    if os.environ.get("SHOW_WRITES"):
        print()
        print("  the rows that gained a write:")
        for i, (x, y) in enumerate(zip(before, after)):
            if "write_file" not in x[1] and "write_file" in y[1]:
                print(f"    {cmds[i].replace(chr(10), ' ')[:110]}")
                print(f"        {after[i][3].splitlines()[:1]}")


if __name__ == "__main__":
    main()
