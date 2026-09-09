import sys, json, os, struct, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import srv

SLOT = 4
t = srv.tokenize("The quick brown fox jumps over the lazy dog. " * 10)
print("tokenize ok:", len(t), t[:10])
toks = t[:64]
srv.slot_erase(SLOT)
r = srv.complete(toks, 0, SLOT)
print("n_predict=0 ->", {k: r.get(k) for k in ("stop_type", "tokens_predicted", "tokens_evaluated")},
      "prompt_n=", r.get("timings", {}).get("prompt_n"), "content=", repr(r.get("content"))[:60])
s = srv.slot_save(SLOT, "unv16-probe.bin")
print("save:", s)
p = "/data/kvcache/unv16-probe.bin"
print("size", os.path.getsize(p))
with open(p, "rb") as f:
    head = f.read(12 + 4 * 80)
magic, ver, ntok = struct.unpack("<III", head[:12])
print("magic=%08x ver=%d ntok=%d" % (magic, ver, ntok))
n = min(ntok, 80)
filetoks = list(struct.unpack("<%di" % n, head[12:12 + 4 * n]))
print("file tokens[:10]", filetoks[:10])
print("match prompt:", filetoks[:len(toks)] == toks)
