#!/usr/bin/env python3
"""**The six stale echoes, measured on A's head against the operator's own session.**

Reported 2026-09-23: the operator's pane carried six `queued ·` echoes, one per paragraph
of the R27 instruction, every one of them answered. This script is the measurement behind
A's answer to that report, in the same shape leticl used for its own 28
(`head-parity-2026-09-21.md` R16): the state, then what each rule retires.

**It reads the store, read-only, and nothing else.** The head's `pending_prompts` is
head-local and never persisted, so the queue is *reconstructed* — and the reconstruction is
checkable rather than asserted, because the queue's six entries are the six LINES of a user
row the store does hold (seq 172), and the seventh send is the second row (seq 583):

  * the six paragraphs arrived as SIX prompts, which is what the operator's `send-keys`
    does with newlines — so the head held six entries;
  * the daemon merged them into ONE user item, one part, newline-joined
    (`crates/turn/src/steering.rs:197-210`, `Pending::absorb`);
  * the head's next send happened while a turn was running, so `App::submit`
    (`crates/tui/src/app.rs:5230-5237`) appended it to the LAST entry — which is why one
    entry is a superset of one row and a prefix of another.

Both rules are implemented here exactly as the Rust has them, and the difference between
them is the whole finding.

    python3 queued-echoes-2026-09-23.py [sessions.db] [session-id] [row-with-six] [row-with-seven]
"""

import json
import os
import sqlite3
import sys

DB = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser(
    "~/.local/share/letibot/sessions.db")
SESSION = sys.argv[2] if len(sys.argv) > 2 else "s-1789462738453908838"
SIX = int(sys.argv[3]) if len(sys.argv) > 3 else 172
SEVEN = int(sys.argv[4]) if len(sys.argv) > 4 else 583


def user_text(con, transcript, seq):
    row = con.execute(
        "select item_json from transcript_item where transcript_id=? and seq=?",
        (transcript, seq),
    ).fetchone()
    if row is None:
        raise SystemExit(f"{transcript} seq {seq}: no such row")
    return json.loads(row[0])["parts"][0]["text"]


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


def retire_fixed(queue, row):
    """The fix: the row's LINES are spent once each, by whole lines of the queue.

    `claimed` is the row's lines, spent across the whole queue in one call and only in
    the order the queue holds them. A blank piece claims nothing.
    """
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
        if not hit:
            i += 1
        elif not "\n".join(kept):
            out.pop(i)
        else:
            out[i] = "\n".join(kept)
            i += 1
    return out


def main():
    con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    tid = con.execute(
        "select id from transcript where session_id=? order by created_at desc limit 1",
        (SESSION,),
    ).fetchone()[0]
    six = user_text(con, tid, SIX)
    seven = user_text(con, tid, SEVEN)

    paragraphs = six.split("\n")
    queue = list(paragraphs)
    queue[5] = queue[5] + "\n" + seven

    print(f"transcript {tid}")
    print(f"  row {SIX}:   {len(six):5d} chars, {six.count(chr(10)) + 1} line(s)  "
          f"<- the six paragraphs, ONE user item")
    print(f"  row {SEVEN}: {len(seven):5d} chars, {seven.count(chr(10)) + 1} line(s)  "
          f"<- the next message, also ONE item")
    print()
    print(f"the head's queue, reconstructed: {len(queue)} entr(ies)")
    for i, q in enumerate(queue):
        print(f"  [{i}] {len(q):5d} chars, {q.count(chr(10)) + 1} line(s)  "
              f"{q.split(chr(10))[0][:56]!r}")
    print()

    for name, rule in (("shipped", retire_shipped), ("fixed", retire_fixed)):
        q = list(queue)
        mid = rule(q, six)
        end = rule(mid, seven)
        print(f"{name:8} after row {SIX}: {len(mid)} left   "
              f"after row {SEVEN}: {len(end)} left")
        for e in end:
            print(f"           still owed: {e.split(chr(10))[0][:56]!r}")
    print()
    print("the shipped rule retires nothing: it asks whether the row IS the echo, or")
    print("begins with it, and BOTH directions are wrong here — the row is the echo's")
    print("join. The fix retires all six, and leaves nothing owed once both rows land.")


if __name__ == "__main__":
    main()
