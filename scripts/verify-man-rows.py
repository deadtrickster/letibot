#!/usr/bin/env python3
"""Does each extracted row name a flag that is actually in that program's man page?

The check `docs/` describes and nobody had written down. It exists because a
27B run produced rows describing a DIFFERENT program than the one asked about,
at roughly one in ten: the harness did `o["program"] = prog` after parsing,
overwriting the model's own `program` field with the asked-for one, so the model
was reporting its drift and the code erased the report before writing it. An
invented flag then looks exactly as authoritative as a real one.

This catches that class without a model and without trusting the extractor: a
flag that is not in the page is not a flag, whatever wrote it down.

Two ways it UNDER-reports, and neither is a reason to skip it:

  1. mdoc pages write flags as macros (`.Fl D`), so the literal never appears
     and a real flag is rejected. `man(1)` is used to render when available,
     which fixes most of it.
  2. It checks EXISTENCE, not MEANING. A flag that is in the page and destroys
     nothing passes here. That is the answer key's job, not this one's.

It also cannot see a destructive flag the model MISSED -- a check that only
looks at rows that exist says nothing about recall.

    verify-man-rows.py ROWS.jsonl [--quarantine OUT.jsonl] [--verified OUT.jsonl]
"""
import argparse
import json
import re
import subprocess
import sys


def page_text(program: str) -> str | None:
    """The program's man page as plain text, or None when it has none."""
    for argv in (["man", "-P", "cat", program], ["man", "-P", "cat", "1", program]):
        try:
            r = subprocess.run(argv, capture_output=True, text=True, timeout=20,
                               env={"MANWIDTH": "200", "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"})
        except (OSError, subprocess.TimeoutExpired):
            continue
        if r.returncode == 0 and r.stdout.strip():
            # Overstrike bolding: `-\b-\bd` is one flag to a reader and three
            # characters to a matcher.
            return re.sub(r".\x08", "", r.stdout)
    return None


def flags_of(row: dict) -> list[str]:
    """Both corpus shapes: `flag` (a string) and `flags` (a list)."""
    if row.get("flags"):
        return list(row["flags"])
    if row.get("flag"):
        return [row["flag"]]
    # A subcommand-scoped row cites the subcommand itself.
    return [row["subcommand"]] if row.get("subcommand") else []


# A row whose "flag" says the destruction is what the program does with no flag
# at all: `(no flag) FILE...`, `(default, unless -k/--keep)`. There is nothing to
# find in the page, so the existence check does not apply to it -- and calling
# that a failure would be the check lying about its own reach. These become the
# `unless` half of a `FlagRule` rather than the `flags` half.
DEFAULT_BEHAVIOUR = re.compile(r"^\s*\((?:no flag|default\b)", re.I)


def candidates(flag: str) -> list[str]:
    """Spellings of one flag that all mean the same thing in a page.

    `-f, --force` is one row naming two; `-ao{a|s|t|u}` is a template whose stem
    is what the page prints; `--remove[=HOW]` keeps its suffix in some pages and
    not others; `rm (interactive command)` is one flag and one gloss, and the
    gloss is never in the page.
    """
    # The gloss a model adds to say WHERE the word applies. Dropped before
    # splitting, so `-r (capability string)` does not become the candidate
    # `capability string`.
    flag = re.sub(r"\s*\([^)]*\)\s*$", "", flag).strip()
    out = []
    for part in re.split(r"[,/|]", flag):
        part = part.strip()
        if not part:
            continue
        out.append(part)
        stem = re.split(r"[\[{=<]", part)[0].strip()
        if stem and stem != part:
            out.append(stem)
    return out


def verify(row: dict, program: str, text: str) -> bool:
    """True when any spelling of any of the row's flags is in the page."""
    # roff escapes a leading hyphen as `\-`; a reader never sees the backslash.
    hay = text.replace("\\-", "-")
    for flag in flags_of(row):
        for c in candidates(flag):
            if len(c) < 2 and not c.isalnum():
                continue
            # A subcommand is printed as `git-rm(1)` in git's own page, so the
            # left boundary that is right for a flag is wrong for a subcommand.
            spellings = [c]
            if re.fullmatch(r"[a-z][a-z0-9-]*", c):
                spellings.append(f"{program}-{c}")
            for sp in spellings:
                if re.search(r"(?<![\w-])" + re.escape(sp) + r"(?![\w-])", hay):
                    return True
    return False


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("rows")
    ap.add_argument("--quarantine")
    ap.add_argument("--verified")
    a = ap.parse_args()

    rows = []
    for line in open(a.rows):
        line = line.strip()
        if line:
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                pass

    pages: dict[str, str | None] = {}
    ok, bad, nopage, bydefault = [], [], [], []
    for r in rows:
        prog = r.get("program", "")
        if prog not in pages:
            pages[prog] = page_text(prog)
        text = pages[prog]
        # Two spellings of the same claim: GLM says `(no flag) FILE...` in the
        # flag field, the 27B leaves both `flags` and `subcommand` empty. Either
        # way the row says the program destroys with no flag asked for, and there
        # is nothing in the page for a literal search to find.
        if not flags_of(r) or any(DEFAULT_BEHAVIOUR.match(f) for f in flags_of(r)):
            bydefault.append(r)
        elif text is None:
            # A page that cannot be read is not a row that failed. Kept apart,
            # because collapsing the two would let "never checked" read as
            # "checked and clean".
            nopage.append(r)
        elif verify(r, prog, text):
            ok.append(r)
        else:
            bad.append(r)

    print(f"rows {len(rows)}   verified {len(ok)}   quarantined {len(bad)}   "
          f"no page {len(nopage)}   destroys by default {len(bydefault)}")
    progs = {r.get("program") for r in rows}
    print(f"programs {len(progs)}   with a page on this box {len([p for p in progs if pages.get(p)])}")
    if a.verified:
        with open(a.verified, "w") as f:
            for r in ok + bydefault:
                f.write(json.dumps(r) + "\n")
    if a.quarantine:
        with open(a.quarantine, "w") as f:
            for r in bad + nopage:
                r = dict(r, _why="flag not in page" if r in bad else "no man page on this box")
                f.write(json.dumps(r) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
