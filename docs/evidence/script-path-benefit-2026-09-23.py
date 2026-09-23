#!/usr/bin/env python3
"""**R39's benefit, measured where the files ARE** — because the corpus's are gone.

`script-path-2026-09-23.py` answers the requirement's own question on the corpus, and its
answer is that the corpus can barely be asked: of 7,644 rows that name a script path, **9**
still have their file on this box. 6,659 do not, and 831 name one an earlier stage of the
same command writes. So the COST is measured there (0 new prompts) and it is close to
vacuous, and saying so is more useful than dressing up 9 rows.

This is the other half, and it is honest about what it is: **not the corpus, the tree.**
Every real script file under the operator's own projects is driven through the same two
readings — `python3 <path>` / `bash <path>`, with and without the body — and what is counted
is what the card would have said and could not have said before.

    python3 script-path-benefit-2026-09-23.py
"""
import os
import subprocess

REPO = "/home/dead/Projects/letibot/letibot"
BIN = f"{REPO}/target/debug/examples/classify"
ROOTS = [
    "/home/dead/Projects/letibot/letibot",
    "/home/dead/Projects/leticl",
]
# What a shell would run it with, by extension. A file no interpreter is handed is not a
# script for this purpose, however it is named.
BY_EXT = {
    ".py": "python3",
    ".sh": "bash",
    ".bash": "bash",
    ".rb": "ruby",
    ".pl": "perl",
    ".js": "node",
}
SKIP_DIRS = {".git", "target", "node_modules", ".venv", "venv", "__pycache__"}


def scripts():
    out = []
    for root in ROOTS:
        if not os.path.isdir(root):
            continue
        for dirpath, dirnames, filenames in os.walk(root):
            dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
            for f in filenames:
                ext = os.path.splitext(f)[1]
                if ext in BY_EXT:
                    out.append((os.path.join(dirpath, f), BY_EXT[ext], root))
    return out


def main():
    found = scripts()
    print(f"real script files under the two trees: {len(found)}")

    blob = b""
    for path, interp, root in found:
        cwd = os.path.dirname(path)
        blob += cwd.encode() + b"\0" + f"{interp} {path}".encode() + b"\0"

    def sweep(read):
        env = dict(os.environ)
        env["READ_SCRIPTS"] = "1" if read else "0"
        env["PAIRS"] = "1"
        p = subprocess.run([BIN], input=blob, capture_output=True, env=env)
        blocks = []
        cur = None
        for line in p.stdout.decode(errors="replace").split("\n"):
            if line.startswith("reads="):
                cur = [line]
                blocks.append(cur)
            elif cur is not None:
                cur.append(line)
        rows = []
        for b in blocks:
            tier = b[0].split("tier=")[1].split()[0]
            # **Strip the quotes and commas.** `{:?}` on the intent list renders it with
            # them, and `"write_file" in intents` is then False for every row — which is
            # how the first version of this sweep reported "GAIN a write 0" for 305 files
            # that gained one. A measurement that cannot see the field it is counting
            # reports the absence of a finding, not a finding.
            intents = [t.strip('",') for t in b[0].split("intents=[")[1].split("]")[0].split()]
            regions = [t.strip('",') for t in b[0].split("regions=[")[1].split("]")[0].split()]
            findings = [l[4:] for l in b if l.startswith("  ! ")]
            rows.append((tier, intents, regions, findings))
        return rows

    before = sweep(False)
    after = sweep(True)
    assert len(before) == len(found) == len(after), (len(before), len(found), len(after))

    rank = {"auto": 0, "may_approve": 1, "always_ask": 2, "blocked": 3, "not_run": 1}
    read = [i for i, r in enumerate(after) if any("read from disk as the program" in f for f in r[3])]
    writes = [
        i
        for i, (x, y) in enumerate(zip(before, after))
        if "write_file" not in x[1] and "write_file" in y[1]
    ]
    prompts = [
        i
        for i, (x, y) in enumerate(zip(before, after))
        if rank.get(x[0], 1) < 2 and rank.get(y[0], 1) >= 2
    ]
    blocks = [i for i in prompts if after[i][0] == "blocked"]
    network = [
        i
        for i, (x, y) in enumerate(zip(before, after))
        if "network" not in x[1] and "network" in y[1]
    ]

    print(f"  whose body was judged:            {len(read)}")
    print(f"  GAIN a write intent:              {len(writes)}")
    print(f"  GAIN a network intent:            {len(network)}")
    print()
    print(f"  GAIN a PROMPT:                    {len(prompts)}   (BLOCKED: {len(blocks)})")
    print(f"  lose a prompt:                    "
          f"{sum(1 for x, y in zip(before, after) if rank.get(x[0], 1) >= 2 and rank.get(y[0], 1) < 2)}")
    print()
    print("  the ones that gained a prompt, and what moved:")
    for i in prompts[:20]:
        print(f"    [{before[i][0]} -> {after[i][0]}] {found[i][0].replace('/home/dead/', '~/')}")
        for f in after[i][3]:
            if "read from disk" in f or "always-ask" in f:
                print(f"        {f[:160]}")


if __name__ == "__main__":
    main()
