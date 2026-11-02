#!/usr/bin/env python3
"""Prove a comment-only change: every .rs file's tokens, comments stripped, match BASE.

    scripts/same-code.py BASE [PATH...]

Doctest blocks live inside doc comments, so their code lines are compared separately.
Exits 1 and names each file that differs. `--selftest` checks the tokenizer.
"""
import subprocess
import sys


def strip(src):
    """Tokens of Rust source without comments, plus the code lines of doc-comment fences."""
    out, docs = [], []
    i, n = 0, len(src)
    fence = False
    while i < n:
        c = src[i]
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            line = src[i:j]
            if line.startswith(("///", "//!")):
                body = line[3:].strip()
                if body.startswith("```"):
                    fence = not fence
                    docs.append(body)
                elif fence:
                    docs.append(body)
            i = j
            continue
        if src.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if src.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif src.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            out.append(" ")
            continue
        # Raw strings: r"..", r#".."#, br#".."#. Matched before identifiers swallow the `r`.
        k = i + 1 if c == "b" else i
        if k < n and src[k] == "r" and (k + 1 < n and src[k + 1] in '"#') and not _ident_before(src, i):
            h = k + 1
            while h < n and src[h] == "#":
                h += 1
            if h < n and src[h] == '"':
                end = '"' + "#" * (h - k - 1)
                j = src.find(end, h + 1)
                j = n if j < 0 else j + len(end)
                out.append(src[i:j])
                i = j
                continue
        if c == '"' or (c == "b" and src.startswith('b"', i) and not _ident_before(src, i)):
            j = i + (2 if c == "b" else 1)
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            out.append(src[i : j + 1])
            i = j + 1
            continue
        if c == "'":
            # A char literal closes within a few chars; a lifetime or label does not.
            if src.startswith("'\\", i):
                j = src.find("'", i + 2)
                if src[i + 2] == "'":
                    j = i + 3
                out.append(src[i : j + 1])
                i = j + 1
                continue
            if i + 2 < n and src[i + 2] == "'":
                out.append(src[i : i + 3])
                i += 3
                continue
        if c.isspace():
            if out and out[-1] != " ":
                out.append(" ")
            i += 1
            continue
        out.append(c)
        i += 1
    return "".join(out).replace(" ", ""), docs


def _ident_before(src, i):
    return i > 0 and (src[i - 1].isalnum() or src[i - 1] == "_")


def git(*args):
    return subprocess.run(["git", *args], capture_output=True, text=True, check=True).stdout


def selftest():
    a = 'fn f<\'a>(x: &\'a str) -> char { /* c /* n */ */ let s = r#"//x"#; \'"\' } // t\n'
    b = 'fn f<\'a>(x: &\'a str) -> char {  let s = r#"//x"#; \'"\' }\n'
    assert strip(a)[0] == strip(b)[0], "comments must not count"
    assert strip(a)[0] != strip(b.replace("//x", "//y"))[0], "string contents must count"
    assert strip("let c = '\\'';")[0] != strip("let c = '\\\\';")[0], "escaped chars must count"
    d1 = "/// ```\n/// assert!(f());\n/// ```\nfn f() {}\n"
    assert strip(d1)[1] != strip(d1.replace("f()", "g()", 1))[1], "doctest code must count"
    print("same-code selftest ok")


def main(argv):
    if argv[1:] == ["--selftest"]:
        selftest()
        return 0
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    base, paths = argv[1], argv[2:]
    now = {p for p in git("ls-files", "--", *paths).split() if p.endswith(".rs")}
    then = {p for p in git("ls-tree", "-r", "--name-only", base, "--", *paths).split() if p.endswith(".rs")}
    bad = sorted(now ^ then)
    for p in sorted(now & then):
        old = strip(git("show", f"{base}:{p}"))
        with open(p, encoding="utf-8") as f:
            new = strip(f.read())
        if old != new:
            bad.append(p)
    for p in bad:
        print(f"differs: {p}")
    print(f"same-code: {len(now | then)} files compared, {len(bad)} differ")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
