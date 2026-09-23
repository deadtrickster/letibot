#!/usr/bin/env python3
"""**R16, measured on the operator's own session: the state, and what each rule retires.**

leticl's figures (28 echoes, 0 unconfirmed, 8 clipped) are about leticl and were given as a
lead. These are A's. Two things are read, and each says which:

  * **the store**, read-only, for the operator's messages and which of them landed as rows —
    including the rows that are a JOIN of several prompts, which is the shape the whole rule
    turns on;
  * **the pane**, as `harness what=screen` reported it, for how many `queued ·` tags the head
    actually drew and how many of the operator's messages each one covers.

**What cannot be read from outside, said here rather than implied.** `pending_prompts` is
head-local process state and is never persisted, so the queue is reconstructed; and the
running daemon speaks protocol 23 against this tree's 25, so nothing built here can attach
and ask the head. Every number below is a reading of the glass or of the store.

    python3 queued-echoes-2026-09-23.py [sessions.db] [session-id]
"""

import json
import os
import sqlite3
import sys

DB = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser(
    "~/.local/share/letibot/sessions.db")
SESSION = sys.argv[2] if len(sys.argv) > 2 else "s-1789462738453908838"


def user_rows(con, tid):
    out = []
    for seq, kind, js in con.execute(
        "select seq, kind, item_json from transcript_item "
        "where transcript_id=? order by seq",
        (tid,),
    ):
        if kind != "user":
            continue
        parts = json.loads(js)["parts"]
        out.append((seq, [p.get("text", "") for p in parts]))
    return out


def retire_shipped(queue, row):
    """`App::retire_pending` as it shipped: equality, then the FRONT PIECE of an entry."""
    for i, q in enumerate(queue):
        if q == row:
            return queue[:i] + queue[i + 1:]
    pre = row + "\n"
    for i, q in enumerate(queue):
        if q.startswith(pre):
            rest = q[len(pre):]
            out = list(queue)
            if rest:
                out[i] = rest
            else:
                out.pop(i)
            return out
    return list(queue)


def retire_landed(queue, row):
    """The rule that landed (`strip_landed`): a row's LINES, spent once each, by whole
    lines of the queue, in order and forward-only."""
    lines = row.split("\n")
    claimed = [False] * len(lines)
    cursor = 0
    out = list(queue)
    i = 0
    while i < len(out):
        kept, hit = [], False
        for piece in out[i].split("\n"):
            if piece == "":
                kept.append(piece)
                continue
            k = next((k for k in range(cursor, len(lines))
                      if not claimed[k] and lines[k] == piece), None)
            if k is None:
                kept.append(piece)
            else:
                claimed[k] = True
                cursor = k + 1
                hit = True
        rest = "\n".join(k for k in kept if k != "")
        if not hit:
            i += 1
        elif not rest:
            out.pop(i)
        else:
            out[i] = rest
            i += 1
    return out


def main():
    con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    tid = con.execute(
        "select id from transcript where session_id=? order by created_at desc limit 1",
        (SESSION,),
    ).fetchone()[0]
    users = user_rows(con, tid)

    print(f"transcript {tid}")
    print()
    print("== THE STORE: the operator's messages, and which landed ==")
    for seq, parts in users:
        t = parts[0]
        n = len(t.split("\n"))
        mark = "  <- a JOIN: one item, several prompts" if n > 2 else ""
        print(f"  seq {seq:5d}  {len(t):6d} chars  {n:3d} lines  {t.splitlines()[0][:52]!r}{mark}")
    print(f"  {len(users)} user row(s), {sum(1 for _s, p in users for _ in p)} part(s)")

    # ---- the current case, reconstructed -------------------------------------------
    #
    # The pane shows ONE `queued ·` tag, whose header is row 1103's line 21
    # ("R32, filed, and it lands directly on R31's door…"), and whose body runs to the end
    # of the R33 message. So that entry is the join of three of the operator's messages,
    # and the head's own `App::submit` is what joined them (a send is appended to the last
    # entry while the head believes a turn is running).
    r = {seq: parts for seq, parts in users}
    idx = next((s for s in r if any("R32, filed" in p for p in r[s])), None)
    if idx is None or 1310 not in r or 1464 not in r:
        print("\n(the current case is not in this transcript; nothing to replay)")
        return
    # **The entry as `App::submit` builds it**: the head appends `'\n'` + the text onto the
    # last entry while a turn is running, so three sends are three messages joined by one
    # newline each. The store's rows carry a trailing newline of their own, which the join
    # does NOT double — replaying that wrong is how the first draft of this script reported
    # a 21-line entry where the pane shows a 60-row one.
    def message(text):
        return text[:-1] if text.endswith("\n") else text

    head_lines = r[idx][0].split("\n")
    start = next(i for i, l in enumerate(head_lines) if l.startswith("R32, filed"))
    joined = [l for l in head_lines[start:] if l != ""]
    entry = (
        "\n".join(joined)
        + "\n"
        + message(r[1310][0])
        + "\n"
        + message(r[1464][0])
    )

    print()
    print("== THE QUEUE, reconstructed from the pane ==")
    print(f"  1 visible `queued ·` tag, whose entry is {entry.count(chr(10)) + 1} lines")
    # **Contains, not startswith**: the first of the three is not a row of its own — it is
    # line 21 of the 30-line join at seq 1103, because the engine merged it with the two
    # before it. That is the whole reason the rule has to read lines and not rows.
    heads = ("R32, filed", "R32\'s first clause", "R33, both heads")
    covered = sum(1 for h in heads if any(h in p for _s, ps in users for p in ps))
    chars = len(entry)
    print(f"  it covers {covered} of the operator's messages, joined")
    print(
        f"  {chars} characters of echo: at ~192 usable columns that is ~{chars // 192} "
        f"screen rows, against a 63-row pane — it cannot fit, which is R33."
    )

    print()
    print("== WHAT EACH RULE RETIRES, against the rows that landed ==")
    rows = [r[idx][0], r[1310][0], r[1464][0]]
    for name, rule in (("shipped", retire_shipped), ("landed (strip_landed)", retire_landed)):
        q = [entry]
        for row in rows:
            q = rule(q, row)
        print(f"  {name:26} -> {len(q)} echo(es) left")
        for e in q:
            print(f"      still owed: {e.splitlines()[0][:56]!r}")

    print()
    print("== THE SCREEN (from `harness what=screen`, 210x63) ==")
    print("  2 `queued ·` entries are being drawn and ONE tag is on the glass: the other")
    print("  entry's header is above the viewport, so the screen under-reports the number")
    print("  of waiting things by one — leticl's finding, reached from the render side.")
    print("  Within the visible tag, its entry is a JOIN of 3 of the operator's messages,")
    print("  so the count of waiting MESSAGES is under-reported by 4 (5 behind 2 tags).")
    print()
    print("  And the two entries spend ~60 of the pane's 63 rows: the echo is drawn at")
    print("  FULL LENGTH, which is R33, and it is why the conversation is off the screen.")
    print("  A single-message echo retires fine under the shipped rule (equality holds);")
    print("  it is the JOINED ones that never do, which is why exactly one is stale here")
    print("  and five were stale on leticl.")


if __name__ == "__main__":
    main()
