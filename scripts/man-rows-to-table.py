#!/usr/bin/env python3
"""Turn verified man-page rows into the `FlagRule` table layer A consults.

Input is the output of `verify-man-rows.py --verified`: rows whose flag was
found in the program's own man page. Output is a Rust file of `FlagRule`
literals, included by `intent.rs`.

Three things this does that the corpus does not:

  1. **Resolves the program name.** The corpus keys on the MAN PAGE name, and
     `git-diff` is a page while `git diff` is a command. The box decides which
     is which: a name that is an executable on PATH is a program, and one that
     is not, whose prefix is, is a subcommand. `update-alternatives` and
     `ssh-add` survive as programs by this rule; `gh-issue-edit` becomes
     `gh` + `issue edit`.

  2. **Keeps git's hyphens.** git's own subcommands contain them
     (`diff-files`, `update-ref`), where docker's and gh's do not
     (`docker service update`). So git splits once and everything else splits
     throughout.

  3. **Drops what it cannot check.** A row naming no flag and no subcommand
     says the program destroys by default. There is nothing in the page for a
     literal search to confirm, and it is the shape the drift bug produced
     most often, so it is not written into the table by this script.

Every row carries `Provenance::Documented` naming the page and the extractor,
because a row that cannot cite the text it came from is a row nobody can check.
"""
import argparse
import json
import re
import shutil

# git's subcommands contain hyphens; these multiplexers' do not.
SPLIT_THROUGHOUT = {"gh", "docker", "podman", "ip", "tc", "btrfs", "dcb", "devlink",
                    "bluetoothctl", "ostree", "zfs", "zpool", "virsh", "pkgctl"}

_which: dict[str, bool] = {}


def is_program(name: str) -> bool:
    if name not in _which:
        _which[name] = shutil.which(name) is not None
    return _which[name]


def resolve(page_name: str) -> tuple[str, str | None] | None:
    """(program, subcommand) for a man-page name, or None when neither exists."""
    if is_program(page_name):
        return page_name, None
    if "-" not in page_name:
        return None
    head, rest = page_name.split("-", 1)
    if not is_program(head):
        return None
    sub = rest.replace("-", " ") if head in SPLIT_THROUGHOUT else rest
    return head, sub


# `why` is the sentence a person reads in the brief, so it has to stand alone.
# The table's own hygiene test rejects anything under 30 characters as "not an
# explanation", and a handful of extracted sentences are accurate but bare --
# "Remove all ACLs." never says WHICH program removes them, which is exactly the
# thing a reader of a brief does not already know. Naming the call fixes the
# sentence and clears the bar for the same reason.
def explanation(program: str, flags: list[str], why: str) -> str:
    why = re.sub(r"\s+", " ", why).strip()
    if len(why) > 30:
        return why
    return f"`{program} {flags[0]}` \u2014 {why}"


def rs(s: str) -> str:
    """One Rust string literal, on one line."""
    s = re.sub(r"\s+", " ", s).strip()
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def flags_of(row: dict) -> list[str]:
    if row.get("flags"):
        return list(row["flags"])
    if row.get("flag"):
        # GLM writes `-f, --force` as one field naming two spellings.
        return [f.strip() for f in re.split(r"[,/]", re.sub(r"\s*\([^)]*\)\s*$", "", row["flag"])) if f.strip()]
    return []


def hand_written_keys(path: str):
    """The (program, subcommand, flags) of every row already in `FLAG_RULES`."""
    src = open(path).read()
    i = src.index("const FLAG_RULES: &[FlagRule] = &[")
    block = src[i:src.index("\n];", i)]
    for blk in block.split("FlagRule {")[1:]:
        prog = re.search(r'program: "([^"]*)"', blk)
        sub = re.search(r'subcommand: (None|Some\("([^"]*)"\))', blk)
        fl = re.search(r"flags: &\[([^\]]*)\]", blk)
        if not (prog and sub and fl):
            continue
        yield (
            prog.group(1),
            sub.group(2) if sub.group(1) != "None" else None,
            tuple(sorted(re.findall(r'"([^"]*)"', fl.group(1)))),
        )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rows", action="append", required=True, metavar="EXTRACTOR=FILE")
    ap.add_argument("--out", required=True)
    ap.add_argument("--hand-table", help="intent.rs, so rows already written by hand are not written again")
    a = ap.parse_args()

    # **The hand-written rows win.** A generated row that duplicates one adds
    # nothing and replaces a sentence somebody argued about with a sentence a model
    # wrote. `git reset --hard` was in the table before any page was scraped; the
    # scrape found it again, which is a small vote of confidence in the extractor
    # and not a reason to write it twice.
    seen: set[tuple] = set(hand_written_keys(a.hand_table)) if a.hand_table else set()
    out, kept, dropped_default, dropped_name = [], 0, 0, 0
    for spec in a.rows:
        extractor, _, path = spec.partition("=")
        for line in open(path):
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            page = row.get("program", "")
            flags = flags_of(row)
            sub_in_corpus = row.get("subcommand")
            # **A token that is not a flag is a subcommand claim, and a subcommand
            # claim is too coarse to be a rule.**
            #
            # The extraction prompt asked for "flags or subcommands that DESTROY",
            # and the models answered with verbs that CAN: `git reset`, `git
            # branch`, `git tag` all reached the flag field as bare words. A rule
            # built from one fires on every use of the verb, so `git reset HEAD~5`
            # — which moves a tip and leaves every commit reachable — came back
            # marked destructive. The hand-written answer key caught it as the only
            # false positive in 13 negatives, which is exactly what its negative
            # twins are for.
            #
            # The flag rows do not have this problem: `--force`, `--delete` and
            # `-sdel` mean one thing wherever they appear. So the line is drawn at
            # the leading dash, and what falls the other side of it is counted, not
            # quietly dropped.
            flags = [f for f in flags if f.startswith("-")]
            if not flags:
                dropped_default += 1
                continue
            r = resolve(page)
            if r is None:
                dropped_name += 1
                continue
            program, sub = r
            # A row scoped to a subcommand in the corpus, under a program that is
            # itself a page name: `7z` + subcommand `d`.
            key = (program, sub, tuple(sorted(flags)))
            if key in seen:
                continue
            seen.add(key)
            kept += 1
            out.append(
                "    FlagRule {\n"
                f"        program: {rs(program)},\n"
                f"        subcommand: {f'Some({rs(sub)})' if sub else 'None'},\n"
                "        unless: &[],\n"
                "        flags: &[" + ", ".join(rs(f) for f in flags) + "],\n"
                "        intent: Intent::Destroy,\n"
                f"        why: {rs(explanation(program, flags, row.get('why', '')))},\n"
                f"        provenance: Provenance::Documented {{ source: {rs(f'man {page}, via {extractor}, flag found in the page')} }},\n"
                "    },"
            )

    with open(a.out, "w") as f:
        f.write(
            "// @generated by scripts/man-rows-to-table.py -- do not edit by hand.\n"
            "//\n"
            "// Rows extracted from programs' own man pages by a local model, then checked\n"
            "// by scripts/verify-man-rows.py: every flag below was found in the page its\n"
            "// `source` names. The check catches the extractor describing a DIFFERENT\n"
            "// program than the one asked about, which a 27B run did at roughly one row in\n"
            "// ten; it does NOT check that the flag means what the row says it means.\n"
            "//\n"
            "// Additive only, like every other rule in this table: a row here can say an\n"
            "// action also destroys, never that it does less. So a wrong row costs a\n"
            "// question and cannot cost a file.\n"
            "//\n"
            f"// {kept} rows. Held back: {dropped_default} that name a subcommand rather\n"
            "// than a flag -- 'this verb can destroy' fires on every use of the verb, and\n"
            "// the answer key caught exactly that as its only false positive -- and\n"
            f"// {dropped_name} whose program is not installed on the box that generated this.\n"
            "#[rustfmt::skip]\n"
            "const DOCUMENTED_FLAG_RULES: &[FlagRule] = &[\n"
            + "\n".join(out)
            + "\n];\n"
        )
    print(f"wrote {kept} rows to {a.out}")
    print(f"held back: {dropped_default} destroy-by-default, {dropped_name} program not on this box")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
