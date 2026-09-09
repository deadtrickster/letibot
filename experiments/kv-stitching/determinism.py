"""Noise floor. Two runs that differ in nothing must agree in everything.

Exact token agreement is only a usable metric if the pipeline is deterministic
when nothing is perturbed. Three floors are measured:

  A  same construction twice (prime P[:n], restore, ask P ++ Q) - if this is
     not 200/200 the exact-agreement metric is worthless.
  B  the same prompt prefilled cold in one batch vs. continued from a restored
     state. Different ubatch segmentation over the same tokens, so this is the
     float noise that a stitch measurement has to beat.
  C  a relabelled state whose relabelling is a no-op (P -> P). Isolates the
     save/patch/restore path itself.
"""
import argparse, json, os, sys, random

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import srv, corpus
from stitch import build, first_divergence, patch_tokens, SAVE_DIR, SAVE_NAME
from stitch2 import prime

ap = argparse.ArgumentParser()
ap.add_argument("--slot", type=int, default=4)
ap.add_argument("--out", type=str, required=True)
a = ap.parse_args()

p = build(11, 1, 60, 60)
P = p["B"] + p["C"]
Q = p["Q_C"]
n = len(P) - 64
print("|P|=%d n_cached=%d |Q|=%d" % (len(P), n, len(Q)))

srv.slot_erase(a.slot)
cold = srv.complete(P + Q, 200, a.slot, cache_prompt=False, ignore_eos=True)

prime(P[:n], None, a.slot)
r1 = srv.complete(P + Q, 200, a.slot, cache_prompt=True, ignore_eos=True)
prime(P[:n], None, a.slot)
r2 = srv.complete(P + Q, 200, a.slot, cache_prompt=True, ignore_eos=True)
prime(P[:n], P[:n], a.slot)  # relabel to itself
r3 = srv.complete(P + Q, 200, a.slot, cache_prompt=True, ignore_eos=True)

out = {
    "n_pred_cold": len(cold["tokens"]),
    "A_repeat_same_construction": first_divergence(r1["tokens"], r2["tokens"]),
    "B_restored_vs_cold": first_divergence(cold["tokens"], r1["tokens"]),
    "C_noop_relabel_vs_plain": first_divergence(r1["tokens"], r3["tokens"]),
    "n_pred": [len(r1["tokens"]), len(r2["tokens"]), len(r3["tokens"])],
}
print(json.dumps(out, indent=1))
with open(a.out, "w") as f:
    json.dump(out, f, indent=1)
