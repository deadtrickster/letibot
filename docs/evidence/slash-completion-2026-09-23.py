#!/usr/bin/env python3
"""**Three lists, one key: what this head completes from, and what it will act on.**

R32's first clause had a cause and it was not "completion is missing". It is that
*completion is answered from a different list than the one that dispatches* — a registry
that exists for one purpose silently becoming the answer to a different question, and
nobody notices because it is nearly right.

Measured on this tree, 2026-09-23, after two wrong guesses about the numbers (a first
count said 12 entries, a second said 21, and both were artefacts of a regex that missed
the multi-line tuple form). The lists are read out of the source, so this is reproducible
rather than remembered:

  A. **what Tab offers** — `SLASH_COMMANDS`, in `crates/tui/src/app.rs`.
  B. **what the head ACTS on** — every arm of `App::command`, the dispatcher, plus the
     verbs `notes_command` and `toggle_todos`/`promote` reach through it.
  C. **what the DAEMON acts on** — the first word of every arm of `Slash::parse`, in
     `crates/harnessd/src/slash.rs`. Every verb the head does not recognise is FORWARDED,
     so the daemon's list is the authority for its half and the head must not enumerate it.

    python3 slash-completion-2026-09-23.py [repo-root]
"""

import os
import re
import sys

ROOT = sys.argv[1] if len(sys.argv) > 1 else "/home/dead/Projects/letibot/letibot"


def read(rel):
    with open(os.path.join(ROOT, rel), encoding="utf-8") as f:
        return f.read()


def braced(s, start, open_c="{", close_c="}"):
    """The text from `start` to its matching close, for slicing one function out."""
    d = 0
    j = start
    while j < len(s):
        if s[j] == open_c:
            d += 1
        elif s[j] == close_c:
            d -= 1
            if d == 0:
                return s[start:j]
        j += 1
    raise SystemExit("unbalanced braces — the source moved")


def offered():
    """A: what Tab offers."""
    s = read("crates/tui/src/app.rs")
    i = s.index("const SLASH_COMMANDS: &[(&str, &str)] = &[")
    j = s.index("\n];", i)
    return sorted(set(re.findall(r'\(\s*"([^"]+)"\s*,', s[i:j])))


def acted_on():
    """B: what the head acts on, from the dispatcher itself."""
    s = read("crates/tui/src/app.rs")
    body = braced(s, s.index("    fn command(&mut self, cmd: &str) -> Option<Action> {"))
    verbs = set(re.findall(r'verb_arg\(cmd, "([^"]+)"\)', body))
    for m in re.finditer(r"matches!\(cmd, ([^)]*)\)", body):
        verbs |= set(re.findall(r'"([^"]+)"', m.group(1)))
    for m in re.finditer(r'cmd\.strip_prefix\("([^ ]+) ', body):
        verbs.add(m.group(1))
    mb = body[body.index("        match cmd {") :]
    for m in re.finditer(r'\n            ((?:"[^"]+"\s*\|\s*)*"[^"]+")\s*(?:=>|\n)', mb):
        verbs |= set(re.findall(r'"([^"]+)"', m.group(1)))
    verbs |= set(re.findall(r'"(reseat[^"]*)"', mb))
    # **`notes_command`'s own arms are NOT verbs.** `back` and `restore` are what
    # follows `/notes`, not what follows `/`, and a first run of this script counted
    # them because it read the wrong function — which is the same defect one layer down:
    # a regex that is nearly right reads like a measurement.
    return sorted(v for v in verbs if v)


def daemon_verbs():
    """C: the first word of every arm of `Slash::parse`."""
    s = read("crates/harnessd/src/slash.rs")
    body = braced(s, s.index("    pub fn parse(line: &str) -> Slash {"))
    # Depth 1 inside `match words.first()`: the arms that DECIDE the verb, not the
    # sub-words of an arm that has already matched.
    out = set()
    depth = 0
    for line in body.split("\n"):
        stripped = line.strip()
        if depth == 2:
            m = re.match(r'Some\("([a-z_-]+)"\)', stripped)
            if m:
                out.add(m.group(1))
        depth += line.count("{") - line.count("}")
    # Aliases of one verb, collapsed to the spelling a person would type.
    for alias, canonical in (
        ("default_model", "default-model"),
        ("default", "default-model"),
        ("model", "models"),
        ("supervised", "supervise"),
    ):
        if alias in out and canonical in out:
            out.discard(alias)
    return sorted(out)


def main():
    a, b, c = set(offered()), set(acted_on()), set(daemon_verbs())
    # The one-letter and short spellings the table deliberately does not carry: the
    # registry's own comment says "aliases are deliberately absent — offering both
    # spellings doubles the list to teach the same actions".
    aliases = {"?", "h", "q", "r", "s", "t", "v", "i"}
    # `reseat keep|verbatim|summarise|summarize` are one verb with a modifier.
    reseat = {v for v in b if v.startswith("reseat")}

    print(f"A. what Tab offers      ({len(a):2d}): {' '.join(sorted(a))}")
    print(f"B. what the head acts on({len(b):2d}): {' '.join(sorted(b))}")
    print(f"C. what the daemon acts ({len(c):2d}): {' '.join(sorted(c))}")
    print()
    print("**The head's own verbs Tab does not offer** (aliases excluded):")
    for v in sorted(b - a - aliases - reseat):
        print(f"  /{v}")
    print()
    print("**The daemon's verbs Tab does not offer** — these WORK; they are forwarded:")
    for v in sorted(c - a):
        print(f"  /{v}")
    print()
    print("**Offered, and not a verb either half acts on:**")
    for v in sorted(a - b - c):
        print(f"  /{v}")
    print()
    print(f"aliases deliberately absent: {' '.join(sorted(aliases))}")
    print(f"reseat modifiers:            {' '.join(sorted(reseat))}")


if __name__ == "__main__":
    main()
