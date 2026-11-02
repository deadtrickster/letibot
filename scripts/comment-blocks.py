#!/usr/bin/env python3
"""Rewrite a file's long comment blocks in one batch.

    scripts/comment-blocks.py list FILE...        blocks over 3 lines, as JSON on stdout
    scripts/comment-blocks.py apply FILE MAP.json  replace blocks by start line
    scripts/comment-blocks.py count FILE...        blocks over 3 lines, per file and total

A block is a run of consecutive `//` lines. `list` skips blocks holding a doctest
fence or a SAFETY: note. MAP.json is {"<start line>": "<new lines>"}; every new
line must itself be a `//` comment, and an empty string deletes the block.
"""
import json
import re
import sys

COMMENT = re.compile(r"^\s*//")


def blocks(lines):
    out, start = [], None
    for i, line in enumerate(lines + ["x"]):
        if COMMENT.match(line):
            if start is None:
                start = i
        elif start is not None:
            out.append((start, i))
            start = None
    return out


def keep(text):
    return "```" in text or "SAFETY:" in text


def long_blocks(path):
    lines = open(path, encoding="utf-8").read().split("\n")
    for s, e in blocks(lines):
        text = "\n".join(lines[s:e])
        if e - s > 3 and not keep(text):
            yield s + 1, text


def main(argv):
    if len(argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    verb, rest = argv[1], argv[2:]
    if verb == "list":
        print(json.dumps({p: {str(n): t for n, t in long_blocks(p)} for p in rest}, indent=1))
        return 0
    if verb == "count":
        total = 0
        for p in rest:
            n = sum(1 for _ in long_blocks(p))
            total += n
            if n:
                print(f"{n:5} {p}")
        print(f"{total:5} total")
        return 0
    if verb == "apply":
        path, mapping = rest[0], json.load(open(rest[1], encoding="utf-8"))
        lines = open(path, encoding="utf-8").read().split("\n")
        spans = {s + 1: e for s, e in blocks(lines)}
        for key in sorted(mapping, key=int, reverse=True):
            start = int(key)
            if start not in spans:
                print(f"line {start} does not start a comment block", file=sys.stderr)
                return 1
            new = mapping[key].split("\n") if mapping[key] else []
            bad = [x for x in new if not COMMENT.match(x)]
            if bad:
                print(f"line {start}: not a comment: {bad[0]!r}", file=sys.stderr)
                return 1
            lines[start - 1 : spans[start]] = new
        open(path, "w", encoding="utf-8").write("\n".join(lines))
        print(f"applied {len(mapping)} blocks to {path}")
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
