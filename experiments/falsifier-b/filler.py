"""Build the depth filler: real technical prose in conversation shape.

Sources are planning / engineering documents from this box, in a fixed order.
None of them mentions any of the five tasks (verified: no occurrence of
merge_intervals, parse_size, LruCache, longest_common_prefix or
parse_range_list anywhere in the corpus).

Depth is measured with the server's own tokenizer, never estimated: the message
list is rendered through /apply-template and the rendering is tokenized with
parse_special=True (passed explicitly -- the default is not what we want to
rely on).
"""
import json, urllib.request

BASE = "http://127.0.0.1:8080"

CORPUS = [
    "/home/dead/Projects/letibot/docs/implementation-plan.md",
    "/home/dead/Projects/letibot/docs/design-brief.md",
    "/home/dead/Projects/letibot/docs/workstreams.md",
    "/home/dead/Projects/letibot/docs/survey.md",
    "/home/dead/Projects/letibot/docs/chat-templates.md",
    "/home/dead/GLM-TODO.md",
    "/home/dead/GLM-STATE.md",
    "/home/dead/Projects/llama.cpp-notes/T4-test-plan.md",
    "/home/dead/Projects/llama.cpp/HANDOVER-llama-upstream-pr.md",
    "/home/dead/Projects/rano/PLAN.md",
]

CHUNK_CHARS = 4000   # ~1.1k tokens, a plausible turn size

USER_LEADS = [
    "Next section of the notes. Read it and hold on to it, I will ask about it later.",
    "Continuing the walkthrough. Here is the next part.",
    "More of the same document. Keep going.",
    "Here is the following section, verbatim.",
    "Next block. Same document, carry on.",
]
ASST_LEADS = [
    "Noted. Reading on from where we were:",
    "Understood. The next part of the document reads:",
    "Got it. Continuing:",
    "Taken in. The section that follows says:",
    "Right. Next up in the document:",
]


def _post(path, obj, timeout=600):
    req = urllib.request.Request(BASE + path, data=json.dumps(obj).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=timeout))


def render(messages):
    return _post("/apply-template", {"messages": messages})["prompt"]


def ntokens(text):
    return len(_post("/tokenize", {"content": text, "parse_special": True})["tokens"])


def prompt_tokens(messages):
    """Exact token count of the rendered conversation prefix."""
    if not messages:
        return ntokens(render([{"role": "user", "content": ""}])) - ntokens(
            render([{"role": "user", "content": ""}]))  # unused; 0k has no filler
    return ntokens(render(messages))


def chunks():
    out = []
    for path in CORPUS:
        text = open(path, encoding="utf-8").read()
        paras = text.split("\n\n")
        buf = ""
        for p in paras:
            if buf and len(buf) + len(p) + 2 > CHUNK_CHARS:
                out.append(buf.strip())
                buf = ""
            buf += p + "\n\n"
        if buf.strip():
            out.append(buf.strip())
    return out


def pairs():
    """Alternating user/assistant turns, both sides carrying real prose."""
    cs = chunks()
    out = []
    for i in range(0, len(cs) - 1, 2):
        out.append([
            {"role": "user",
             "content": USER_LEADS[(i // 2) % len(USER_LEADS)] + "\n\n" + cs[i]},
            {"role": "assistant",
             "content": ASST_LEADS[(i // 2) % len(ASST_LEADS)] + "\n\n" + cs[i + 1]},
        ])
    return out


def build(target_tokens, tol=0.01, verbose=True):
    """Return (messages, exact_token_count) for a filler prefix near target."""
    if target_tokens == 0:
        return [], 0
    ps = pairs()
    # first estimate from a small sample, then walk to the target
    sample = [m for p in ps[:4] for m in p]
    per_pair = ntokens(render(sample)) / 4.0
    n = max(1, min(len(ps), int(target_tokens / per_pair)))
    seen = {}

    def count(k):
        if k not in seen:
            seen[k] = ntokens(render([m for p in ps[:k] for m in p]))
        return seen[k]

    lo, hi = 1, len(ps)
    while lo < hi:
        c = count(n)
        if abs(c - target_tokens) <= tol * target_tokens:
            break
        if c < target_tokens:
            lo = n + 1
        else:
            hi = n
        if lo >= hi:
            n = lo
            break
        n = (lo + hi) // 2
    # pick the neighbour that lands closest without overshooting the pool
    best = min([k for k in (n - 1, n, n + 1) if 1 <= k <= len(ps)],
               key=lambda k: abs(count(k) - target_tokens))
    msgs = [m for p in ps[:best] for m in p]
    exact = count(best)
    if verbose:
        print(f"  depth target {target_tokens}: {best} pairs "
              f"({best*2} turns) = {exact} tokens "
              f"({100*(exact-target_tokens)/target_tokens:+.2f}%)")
    return msgs, exact


if __name__ == "__main__":
    cs = chunks()
    print(f"corpus: {len(CORPUS)} files, {len(cs)} chunks, {len(pairs())} pairs available")
    for t in (20000, 60000, 150000):
        build(t)
