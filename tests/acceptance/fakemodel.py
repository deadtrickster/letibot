#!/usr/bin/env python3
"""**A scripted model on the provider seam**, for the acceptance suite.

It speaks the OpenAI chat-completions shape DeepSeek streams (`crates/provider`'s
`MessagesBackend`): a POST of the transcript as `messages`, answered with SSE deltas
— text, or a tool call, then `usage` and `[DONE]`. A `providers.toml` whose
`[deepseek] url` names this server makes the whole daemon run on it, parent and
subagents alike, with no network, no key and no cost.

The script is keyed by what the conversation says, not by call count, so a retry or
an extra round cannot desynchronise it. Each scenario is a brief the spec types: the
first user message carries a marker, and the model plays its part for that marker.

    PARENT-FIRECODE   call `task` with `where: firecode` and the CHILD-UNAME brief;
                      after the tool result, say it was dispatched; when the child's
                      answer arrives, repeat it (`parent: the subagent reported …`).
    PARENT-HOST       call `task` in the parent's own boundary with the CHILD-SAY brief.
    CHILD-SAY         answer `child-done: said hello from the subagent`, no tools.
    PARENT-SLOW       call `task` with the CHILD-SLOW brief.
    CHILD-SLOW        hold the answer $FAKEMODEL_SLOW_SECONDS (40), so the child is mid-turn.
    PARENT-GATE       call `merge_gate`, then say what it listed.
    GATE-NOTICE       a turn carrying the queue's "waiting for a merge gate" notice: call
                      `merge_gate` for the repository it names, then say the choices are offered.
    GATEKEEPER        the merge queue's reviewer (its brief begins "You are the gatekeeper."):
                      answer a verdict block — `verdict: accept` — with no tools.
    CHILD-UNAME       call `bash` with `uname -s; echo from-the-vm`; after the
                      result, answer `child-done:` and what the shell said.

Anything else is answered `fake: nothing scripted for this`, so a missing scenario is
a sentence on screen rather than a hang. Every request is appended to $FAKEMODEL_LOG
(one JSON object per line) for a failed spec to read.

    fakemodel.py PORT_FILE     # binds 127.0.0.1:0, writes the port to PORT_FILE
"""

import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

CHILD_BRIEF = "CHILD-UNAME: run `uname -s` in a shell and report what it printed."


def text_of(message):
    c = message.get("content")
    if isinstance(c, str):
        return c
    if isinstance(c, list):
        return " ".join(p.get("text", "") for p in c if isinstance(p, dict))
    return ""


def first_user(messages):
    for m in messages:
        if m.get("role") == "user":
            return text_of(m)
    return ""


def tool_results(messages):
    return [text_of(m) for m in messages if m.get("role") == "tool"]


def play(messages):
    """What the model says next: ("text", str) or ("call", name, args)."""
    brief = first_user(messages)
    results = tool_results(messages)
    if "PARENT-FIRECODE" in brief:
        # The child's answer reaches the parent on its own, later; say what it was.
        for m in messages[1:]:
            t = text_of(m)
            if "child-done:" in t and m.get("role") != "assistant":
                said = t[t.index("child-done:"):].split("\n")[0][:200]
                return ("text", f"parent: the subagent reported {said}")
        if not results:
            return ("call", "task", {
                "prompt": CHILD_BRIEF,
                "role": "runner",
                "where": "firecode",
            })
        return ("text", "parent: dispatched the subagent to a firecode VM.")
    if "PARENT-HOST" in brief:
        # A subagent in the parent's own boundary, whose work is a sentence: the session
        # tree without a VM or a shell, for the specs that walk it.
        if not results:
            return ("call", "task", {"prompt": "CHILD-SAY: answer in one line.", "role": "researcher"})
        return ("text", "parent: dispatched the subagent.")
    if "PARENT-SLOW" in brief:
        if not results:
            call = {"prompt": "CHILD-SLOW: think for a while.", "role": "researcher"}
            if "IN-VM" in brief:
                call["where"] = "firecode"
            return ("call", "task", call)
        return ("text", "parent: dispatched the slow subagent.")
    if "CHILD-SLOW" in brief:
        # A turn that is still running when a spec walks into its session: the answer is held
        # for SLOW_SECONDS (40 by default) before it streams.
        import time
        time.sleep(float(os.environ.get("FAKEMODEL_SLOW_SECONDS", "40")))
        return ("text", "child-done: slow and steady")
    if "PARENT-GATE" in brief:
        if not results:
            return ("call", "merge_gate", {})
        return ("text", "parent: the gate choices are in.")
    if brief.startswith("You are the gatekeeper."):
        branch = brief.split("- branch:", 1)[1].split("\n", 1)[0].strip() if "- branch:" in brief else "?"
        return ("text", "Read the change.\n\nverdict: accept\n"
                        f"reasons: - the fake reviewer read {branch}\n"
                        "files: none\ncommands: none")
    said = [text_of(m) for m in messages if m.get("role") != "assistant"]
    notice = next((t for t in said if "The merge queue is holding" in t), None)
    if notice is not None:
        if not results:
            repo = notice.split("the repository `", 1)[1].split("`", 1)[0] if "the repository `" in notice else "."
            return ("call", "merge_gate", {"repo": repo})
        return ("text", "parent: offered the operator the gate choices.")
    if "CHILD-SAY" in brief:
        return ("text", "child-done: said hello from the subagent")
    if "CHILD-UNAME" in brief:
        if not results:
            return ("call", "bash", {"command": "uname -s; echo from-the-vm"})
        said = " | ".join(r.strip().replace("\n", " / ") for r in results)[:300]
        return ("text", f"child-done: {said}")
    return ("text", "fake: nothing scripted for this")


def sse(obj):
    return ("data: " + json.dumps(obj) + "\n\n").encode()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(n) or b"{}")
        messages = body.get("messages", [])
        move = play(messages)
        log = os.environ.get("FAKEMODEL_LOG")
        if log:
            with open(log, "a") as f:
                f.write(json.dumps({"brief": first_user(messages)[:80],
                                    "tools": len(tool_results(messages)),
                                    "last_result": (tool_results(messages) or [""])[-1][:2000],
                                    "move": move[:2]}) + "\n")
        chunks = []
        if move[0] == "text":
            chunks.append(sse({"choices": [{"delta": {"content": move[1]}}]}))
            chunks.append(sse({"choices": [{"delta": {}, "finish_reason": "stop"}]}))
        else:
            _, name, args = move
            call_id = f"call_{len(tool_results(messages))}"
            chunks.append(sse({"choices": [{"delta": {"tool_calls": [{
                "index": 0, "id": call_id,
                "function": {"name": name, "arguments": json.dumps(args)},
            }]}}]}))
            chunks.append(sse({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}))
        chunks.append(sse({"choices": [], "usage": {
            "prompt_tokens": 100, "completion_tokens": 10, "prompt_cache_hit_tokens": 0}}))
        chunks.append(b"data: [DONE]\n\n")
        payload = b"".join(chunks)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    with open(sys.argv[1], "w") as f:
        f.write(str(server.server_address[1]))
    server.serve_forever()


if __name__ == "__main__":
    main()
