#!/usr/bin/env python3
"""Extract the etalon: every tool call this box's operator sat through, with their
words beside it and what became of it.

Plan: docs/guard-corpus-plan.md §7 (step 5). Three stores, one row shape:

    {"source": "claude-code" | "opencode" | "letibot",
     "session": ..., "cwd": <where it ran; the project for scope>, "ts_ms": ...,
     "trail": [{"speaker": "operator", "text": ..., "seconds_ago": ..., "turns_ago": ...}, ...],
     "tool": "bash" | "edit" | "write",
     "arguments": {"command": ...} | {"path": ...},
     "outcome": "ran" | "error" | "refused",
     "why": <the refusal or error text, first line, or "">,
     "label": {"by": ..., "effect": ...} | null}

`trail` is the last three operator utterances before the call, newest first, with
their distance — the same three facts the gatekeeper's brief carries. `label` is
set only for letibot rows, where a human's decision was recorded as such; every
other row's outcome is what the harness did, which is the operator's decision by
implication (a refusal was theirs; a run was not stopped).

What is NOT here, on purpose: tool output. A transcript's results carry file
contents, tokens, whatever the model read. The etalon is what was asked and what
was answered, never what came back. Commands and utterances are passed through
`redact`, which blanks the obvious credential shapes; it is a net, not a proof.

Usage:  scripts/etalon-extract.py [--out ~/.local/share/letibot/etalon.jsonl]
"""
import argparse
import glob
import json
import os
import re
import sqlite3
import sys
from datetime import datetime

HOME = os.path.expanduser("~")

SECRET = re.compile(
    r"(gh[pous]_[A-Za-z0-9]{20,}|sk-[A-Za-z0-9]{20,}|Bearer\s+[A-Za-z0-9._-]{16,}"
    r"|ragflow-[A-Za-z0-9]{8,}"
    r"|(?:api[_-]?key|token|secret|password|passwd)\s*[=:]\s*['\"]?[^\s'\"]{8,}"
    # A key BODY, not only its header: the ground truth from the laptop shipment
    # was a private key in tool output, and a model that echoes one into a
    # heredoc puts it in a COMMAND. `b3BlbnNzaC1rZXktdjE` is base64 of
    # `openssh-key-v1`, the first bytes of every OpenSSH private key.
    r"|b3BlbnNzaC1rZXktdjE[A-Za-z0-9+/=\s]{40,}"
    r"|-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]{40,}?-----END [A-Z ]*PRIVATE KEY-----"
    r"|ssh-(?:ed25519|rsa|ecdsa)\s+AAAA[A-Za-z0-9+/=]{40,}"
    # Passwords on a command line. Found in the corpus, 950 times: RAGFlow's
    # default password inline in `mysql -uroot -p…` and `redis-cli -a …`. The
    # flag forms are the general shape; the literal is the one we know.
    r"|(?<=\s-p)(?!\s)[^\s'\"]{4,}"
    r"|(?<=redis-cli )(?:[^|;]*?\s)?-a\s+[^\s'\"]+"
    r"|--password(?:=|\s+)[^\s'\"]+"
    r"|(?:MYSQL_PWD|PGPASSWORD|REDIS_PASSWORD|MYSQL_ROOT_PASSWORD)=[^\s'\"]+"
    r"|infini_rag_flow\w*)",
    re.I,
)


def redact(text):
    if not isinstance(text, str):
        return text
    return SECRET.sub("<secret>", text)


def first_line(s, n=200):
    if not s:
        return ""
    return str(s).strip().splitlines()[0][:n] if str(s).strip() else ""


def trail_of(utterances, at_ms):
    """The last three operator utterances before `at_ms`, newest first."""
    before = [u for u in utterances if u[0] <= at_ms][-3:]
    before.reverse()
    turns = len([u for u in utterances if u[0] <= at_ms])
    out = []
    for i, (ts, text) in enumerate(before):
        out.append(
            {
                "speaker": "operator",
                "text": redact(text[:600]),
                "seconds_ago": max(0, (at_ms - ts) // 1000) if ts else None,
                "turns_ago": i,
            }
        )
    return out


# --- Claude Code: ~/.claude/projects/*/*.jsonl --------------------------------

# The two ways a Claude Code call is refused, and they are not the same fact:
# the first is the operator, the second is Claude Code's own auto-mode classifier
# — a model's verdict, kept apart so it is never mistaken for a human label.
# Matched as phrases, not words: `denied` alone matched "Permission denied" in a
# command's own stderr and `///` in grep output, which is how a first pass found
# "100 refusals" of which four were real.
HUMAN_DENIAL = re.compile(r"doesn't want to proceed with this tool use|tool use was rejected", re.I)
CLASSIFIER_DENIAL = re.compile(r"denied by the Claude Code auto mode classifier", re.I)
# opencode's refusal, when a permission is rejected in its TUI.
OPENCODE_DENIAL = re.compile(r"rejected|denied by the user|permission.*denied", re.I)


def refusal(text):
    """`(outcome, refused_by)` for a result body, or None when it is not a refusal."""
    if HUMAN_DENIAL.search(text or ""):
        return "refused", "human"
    if CLASSIFIER_DENIAL.search(text or ""):
        return "refused", "classifier"
    return None


def claude_code(paths, root, host="lab2x1"):
    rows = 0
    for path in paths:
        # A subagent's transcript lives under its parent's directory; the session
        # id keeps that path so a hold-out by session keeps a parent and its
        # children on the same side.
        rel = os.path.relpath(path, root)
        session = f"{host}/{rel.rsplit('.', 1)[0]}"
        utterances = []  # (ts_ms, text)
        pending = {}  # tool_use_id -> (ts_ms, name, input, trail)
        events = []
        # A copy of another host's tree can carry a symlink whose target stayed
        # behind; say so and move on rather than losing the whole host.
        try:
            f = open(path, encoding="utf-8", errors="replace")
        except OSError as e:
            print(f"skip {path}: {e.strerror}", file=sys.stderr)
            continue
        with f:
            for line in f:
                try:
                    o = json.loads(line)
                except Exception:
                    continue
                t = o.get("type")
                if t not in ("user", "assistant"):
                    continue
                ts = o.get("timestamp")
                try:
                    ts_ms = int(datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp() * 1000) if ts else 0
                except Exception:
                    ts_ms = 0
                m = o.get("message") or {}
                content = m.get("content")
                cwd = o.get("cwd") or ""
                if t == "user":
                    if isinstance(content, str) and content.strip():
                        utterances.append((ts_ms, content))
                    elif isinstance(content, list):
                        texts = [p.get("text", "") for p in content if isinstance(p, dict) and p.get("type") == "text"]
                        if texts and not any(isinstance(p, dict) and p.get("type") == "tool_result" for p in content):
                            utterances.append((ts_ms, "\n".join(texts)))
                        for p in content:
                            if isinstance(p, dict) and p.get("type") == "tool_result":
                                events.append(("result", ts_ms, p))
                elif isinstance(content, list):
                    for p in content:
                        if isinstance(p, dict) and p.get("type") == "tool_use":
                            events.append(("use", ts_ms, dict(p, _cwd=cwd)))
        for kind, ts_ms, p in events:
            if kind == "use":
                name = (p.get("name") or "").lower()
                inp = p.get("input") or {}
                if name == "bash" and inp.get("command"):
                    args = {"command": redact(inp["command"])}
                elif name in ("edit", "write") and inp.get("file_path"):
                    args = {"path": inp["file_path"]}
                else:
                    continue
                pending[p.get("id")] = (ts_ms, name, args, trail_of(utterances, ts_ms), p.get("_cwd", ""))
            else:
                key = p.get("tool_use_id")
                if key not in pending:
                    continue
                ts_use, name, args, trail, cwd = pending.pop(key)
                body = p.get("content")
                if isinstance(body, list):
                    body = " ".join(x.get("text", "") for x in body if isinstance(x, dict))
                body = body if isinstance(body, str) else json.dumps(body)
                refused_by = None
                if (r := refusal(body)) is not None:
                    outcome, why = r[0], first_line(body)
                    refused_by = r[1]
                elif p.get("is_error"):
                    outcome, why = "error", first_line(body)
                else:
                    outcome, why = "ran", ""
                yield {
                    "source": f"claude-code@{host}",
                    "session": session,
                    "cwd": cwd,
                    "ts_ms": ts_use,
                    "trail": trail,
                    "tool": name,
                    "arguments": args,
                    "outcome": outcome,
                    "why": redact(why),
                    "label": {"by": refused_by, "effect": "refuse"} if refused_by else None,
                }
                rows += 1
    print(f"claude-code@{host}: {rows} rows from {len(paths)} transcripts", file=sys.stderr)


# --- opencode: ~/.local/share/opencode/opencode.db ----------------------------


def opencode(db, host="lab2x1"):
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    roles = {}
    for mid, data in c.execute("select id, data from message"):
        try:
            roles[mid] = json.loads(data).get("role")
        except Exception:
            pass
    by_session = {}
    for sid, mid, ts, data in c.execute("select session_id, message_id, time_created, data from part order by time_created"):
        try:
            o = json.loads(data)
        except Exception:
            continue
        by_session.setdefault(sid, []).append((ts or 0, mid, o))
    rows = 0
    for sid, parts in by_session.items():
        utterances = []
        for ts, mid, o in parts:
            if o.get("type") == "text" and roles.get(mid) == "user" and o.get("text"):
                utterances.append((ts, o["text"]))
        for ts, mid, o in parts:
            if o.get("type") != "tool":
                continue
            tool = (o.get("tool") or "").lower()
            st = o.get("state") or {}
            inp = st.get("input") or {}
            if tool == "bash" and inp.get("command"):
                args = {"command": redact(inp["command"])}
            elif tool in ("edit", "write") and inp.get("filePath"):
                args = {"path": inp["filePath"]}
            else:
                continue
            status = st.get("status")
            err = st.get("error") or ""
            if status == "error" and OPENCODE_DENIAL.search(err):
                outcome, why = "refused", first_line(err)
            elif status == "error":
                outcome, why = "error", first_line(err)
            elif status == "completed":
                outcome, why = "ran", ""
            else:
                continue
            yield {
                "source": f"opencode@{host}",
                "session": f"{host}/{sid}",
                "cwd": inp.get("workdir") or "",
                "ts_ms": ts,
                "trail": trail_of(utterances, ts),
                "tool": tool,
                "arguments": args,
                "outcome": outcome,
                "why": redact(why),
                "label": None,
            }
            rows += 1
    print(f"opencode@{host}: {rows} rows from {len(by_session)} sessions", file=sys.stderr)


# --- letibot: the adjudication corpus, already labelled -----------------------


def letibot(db):
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    rows = 0
    for sid, ts, tool, args, trail, effect, by, asked, ws in c.execute(
        "select a.session_id, a.decided_ms, a.tool, a.arguments_json, a.trail_json, a.effect, a.verdict_by, a.asked, "
        "coalesce(s.workspace_root, '') from adjudication a left join session s on s.id = a.session_id"
    ):
        try:
            a = json.loads(args)
            t = json.loads(trail).get("utterances", [])
        except Exception:
            continue
        if tool == "bash" and a.get("command"):
            arguments = {"command": redact(a["command"])}
        elif tool in ("edit", "write") and a.get("path"):
            arguments = {"path": a["path"]}
        else:
            continue
        outcome = "ran" if effect == "admit" else "refused"
        yield {
            "source": "letibot",
            "session": sid,
            "cwd": ws,
            "ts_ms": ts,
            "trail": [
                {
                    "speaker": u.get("speaker"),
                    "text": redact((u.get("text") or "")[:600]),
                    "seconds_ago": u.get("seconds_ago"),
                    "turns_ago": u.get("turns_ago"),
                }
                for u in t[:3]
            ],
            "tool": tool,
            "arguments": arguments,
            "outcome": outcome,
            "why": "",
            "label": {"by": by or "", "effect": effect, "asked": bool(asked)},
        }
        rows += 1
    print(f"letibot: {rows} rows", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=os.path.join(HOME, ".local/share/letibot/etalon.jsonl"))
    ap.add_argument(
        "--src",
        default=os.path.join(HOME, ".local/share/letibot/etalon-src"),
        help="other hosts' stores, one directory per host: <src>/<host>/{claude,glm,...}/**/*.jsonl and "
        "<src>/<host>/opencode/opencode.db (pulled by rsync; 0700; never leaves this box)",
    )
    a = ap.parse_args()
    out = open(a.out, "w", encoding="utf-8")
    n = 0
    counts = {}
    gens = [
        claude_code(
            sorted(glob.glob(os.path.join(HOME, ".claude/projects/**/*.jsonl"), recursive=True)),
            os.path.join(HOME, ".claude/projects"),
        ),
        opencode(os.path.join(HOME, ".local/share/opencode/opencode.db")),
        letibot(os.path.join(HOME, ".local/share/letibot/sessions.db")),
    ]
    for hostdir in sorted(glob.glob(os.path.join(a.src, "*"))):
        host = os.path.basename(hostdir)
        jsonls = sorted(
            p for p in glob.glob(os.path.join(hostdir, "**/*.jsonl"), recursive=True)
            if "/opencode" not in p
        )
        if jsonls:
            gens.append(claude_code(jsonls, hostdir, host))
        # Any opencode sqlite under the host's copy, wherever the sender put it
        # (`opencode/`, `opencode-share/`, a `-local` twin): each is its own set
        # of sessions, tagged by file so the source says which.
        for db in sorted(glob.glob(os.path.join(hostdir, "**/opencode*.db"), recursive=True)):
            tag = os.path.basename(db).rsplit(".", 1)[0]
            gens.append(opencode(db, host if tag == "opencode" else f"{host}:{tag}"))
    for gen in gens:
        for row in gen:
            out.write(json.dumps(row, ensure_ascii=False) + "\n")
            n += 1
            k = (row["source"], row["tool"], row["outcome"])
            counts[k] = counts.get(k, 0) + 1
    out.close()
    print(f"wrote {n} rows to {a.out}", file=sys.stderr)
    for k in sorted(counts):
        print(f"  {k[0]:12} {k[1]:6} {k[2]:8} {counts[k]:6}", file=sys.stderr)


if __name__ == "__main__":
    main()
