"""Thin client for the running llama-server. Read-only with respect to server config."""
import json, urllib.request, urllib.error

BASE = "http://127.0.0.1:8080"

def post(path, body, timeout=1800):
    req = urllib.request.Request(BASE + path, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        raise RuntimeError("HTTP %d on %s: %s" % (e.code, path, e.read().decode()[:800]))

def get(path, timeout=60):
    with urllib.request.urlopen(BASE + path, timeout=timeout) as r:
        return json.load(r)

def mem_available_gib():
    with open("/proc/meminfo") as f:
        for line in f:
            if line.startswith("MemAvailable:"):
                return int(line.split()[1]) / (1024.0 * 1024.0)
    return 0.0


def guard(floor_gib=40.0):
    """Every distinct prompt this harness prefills becomes an entry in the
    server's 80 GiB host-RAM prompt cache (~120 KiB per token, because 111 MiB
    of it is the per-sequence recurrent state). A sweep of large prompts fills
    that faster than the spill can drain it. On 2026-09-09 this drove the
    kernel OOM killer to kill qwen-flash-next.service at a 158.1 GiB peak.
    Refuse to add more pressure when the box is already short."""
    avail = mem_available_gib()
    if avail < floor_gib:
        raise RuntimeError("MemAvailable %.1f GiB is below the %.1f GiB floor - "
                           "refusing to add prompt-cache pressure" % (avail, floor_gib))
    return avail


def tokenize(text):
    return post("/tokenize", {"content": text})["tokens"]

def detokenize(toks):
    return post("/detokenize", {"tokens": toks})["content"]

def slot_save(slot, filename):
    return post("/slots/%d?action=save" % slot, {"filename": filename})

def slot_restore(slot, filename):
    return post("/slots/%d?action=restore" % slot, {"filename": filename})

def slot_erase(slot):
    return post("/slots/%d?action=erase" % slot, {})

def complete(tokens, n_predict, slot, cache_prompt=True, n_probs=0, ignore_eos=False):
    body = {
        "ignore_eos": ignore_eos,
        # MTP drafting makes the decode batch composition depend on the
        # acceptance rate, which varies run to run. Two identical runs then
        # diverge at token 43 of 200 (raw/determinism.json). Exact token
        # agreement is only a metric once this is off.
        "speculative.n_max": 0,
        "prompt": tokens,
        "n_predict": n_predict,
        "temperature": 0.0,
        "top_k": 1,
        "top_p": 1.0,
        "min_p": 0.0,
        "seed": 1234,
        "cache_prompt": cache_prompt,
        "id_slot": slot,
        "return_tokens": True,
        "samplers": [],
    }
    if n_probs:
        body["n_probs"] = n_probs
    return post("/completion", body)
