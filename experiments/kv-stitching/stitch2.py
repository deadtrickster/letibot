"""UNVERIFIED-16, run 2: paired design.

Run 1 compared a stitched state against a COLD full prefill of the reference.
That comparison mixes two effects: the staleness under test, and the ordinary
float noise of prefilling the same tokens in a different number of ubatches.
Greedy decoding amplifies the second one, so it has to be measured, not assumed
away.

Run 2 pairs every stitched condition with a control that is byte-identical in
construction except that the state it restores was computed over the TRUE
history:

    control_k : prefill P[:n],     save, restore,             ask P ++ Q
    stitch_k  : prefill P_alt[:n], save, RELABEL to P[:n],
                restore,                                      ask P ++ Q

    n = |P| - k

Both prefill exactly k + |Q| tokens on top of a restored state, so the ubatch
segmentation, the save/restore round trip and the decode path are the same.
Everything that differs between the two is the staleness of the KV, which is
what UNVERIFIED-16 asks about.

Metrics per k:
  div_vs_ctrl  first position where the 200 greedy tokens disagree with the
               paired control. This is the number that has to rise with k if a
               partial recompute converges.
  div_ctrl_ref divergence of the control against the cold reference. This is
               the noise floor at that k, and it bounds what div_vs_ctrl can
               mean.
  kl_first     KL(control || stitch) over the top-20 next-token distribution at
               the join. Degrades smoothly, so it localises a failure that the
               exact-agreement metric only reports as yes/no.
"""
import argparse, json, math, os, random, struct, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import srv
import corpus
from stitch import build, patch_tokens, first_divergence, top_probs, kl, SAVE_DIR, SAVE_NAME


def prime(prefix_tokens, relabel_to, slot):
    """Prefill `prefix_tokens`, save, optionally relabel the saved token list to
    `relabel_to`, restore. Returns tokens actually prefilled."""
    srv.slot_erase(slot)
    r0 = srv.complete(prefix_tokens, 0, slot, cache_prompt=True)
    srv.slot_save(slot, SAVE_NAME)
    if relabel_to is not None:
        patch_tokens(os.path.join(SAVE_DIR, SAVE_NAME), relabel_to)
    srv.slot_erase(slot)
    srv.slot_restore(slot, SAVE_NAME)
    return r0["timings"]["prompt_n"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--slot", type=int, default=4)
    ap.add_argument("--seeds", type=int, nargs="+", default=[11])
    ap.add_argument("--a", type=int, default=0, help="records in the shared prefix A")
    ap.add_argument("--b", type=int, default=60, help="records in the diverging block B")
    ap.add_argument("--c", type=int, default=200, help="records in the reused block C")
    ap.add_argument("--npred", type=int, default=200)
    ap.add_argument("--ks", type=str, default="")
    ap.add_argument("--out", type=str, required=True)
    args = ap.parse_args()

    results = []
    for seed in args.seeds:
        t0 = time.time()
        p = build(seed, max(args.a, 1), args.b, args.c)
        A = p["A"] if args.a > 0 else []
        B, Balt, C = p["B"], p["Balt"], p["C"]
        a, b, c = len(A), len(B), len(C)
        P = A + B + C
        P_alt = A + Balt + C
        Q = p["Q_C"]

        if args.ks:
            # "c" = recompute all of C; "cb" = recompute from the divergence,
            # which is exact by construction and is the determinism control.
            sym = {"c": c, "cb": c + b}
            ks = [sym[x] if x in sym else int(x) for x in args.ks.split(",")]
            # keep at least one cached token so the "prime" prefill is legal
            ks = sorted(set(min(k, len(P) - 1) for k in ks))
        else:
            ks = [0, 4, 16, 64, 256, 1024, 2048]
            ks = [k for k in ks if k < c]
            ks += [c, c + b]

        print("[seed %d] a=%d b=%d c=%d |P|=%d |Q|=%d ks=%s" %
              (seed, a, b, c, len(P), len(Q), ks), flush=True)

        srv.slot_erase(args.slot)
        ref = srv.complete(P + Q, args.npred, args.slot, cache_prompt=False, n_probs=20, ignore_eos=True)
        ref_toks = ref["tokens"]
        print("  ref prompt_n=%d/%d  answer=%r  truth=%s / %s" %
              (ref["timings"]["prompt_n"], len(P) + len(Q), ref["content"][:50],
               p["ans_C"], p["own_C"]), flush=True)

        # validity: does the stitched state actually hold B_alt rather than B?
        prime(P_alt[:len(P)], P, args.slot)
        vb = srv.complete(P + p["Q_B"], 32, args.slot, cache_prompt=True)
        srv.slot_erase(args.slot)
        ref_b = srv.complete(P + p["Q_B"], 32, args.slot, cache_prompt=False)
        print("  validity: ref_B=%r stitched_B=%r truth=%s (stitched must be wrong)" %
              (ref_b["content"][:40], vb["content"][:40], p["ans_B"]), flush=True)
        validity_ok = (p["ans_B"] in ref_b["content"]) and (p["ans_B"] not in vb["content"])

        rows = []
        for k in ks:
            srv.guard()
            n = len(P) - k

            pn_c = prime(P[:n], None, args.slot)
            rc = srv.complete(P + Q, args.npred, args.slot, cache_prompt=True, n_probs=20, ignore_eos=True)

            pn_s = prime(P_alt[:n], P[:n], args.slot)
            rs_ = srv.complete(P + Q, args.npred, args.slot, cache_prompt=True, n_probs=20, ignore_eos=True)

            row = {
                "seed": seed, "k": k, "n_cached": n,
                "prompt_n_ctrl": rc["timings"]["prompt_n"],
                "prompt_n_stitch": rs_["timings"]["prompt_n"],
                "prompt_n_expect": k + len(Q),
                "prefill_n_ctrl": pn_c, "prefill_n_stitch": pn_s,
                "div_vs_ctrl": first_divergence(rc["tokens"], rs_["tokens"]),
                "div_ctrl_ref": first_divergence(ref_toks, rc["tokens"]),
                "div_stitch_ref": first_divergence(ref_toks, rs_["tokens"]),
                "n_ctrl": len(rc["tokens"]), "n_stitch": len(rs_["tokens"]),
                "kl_first": kl(top_probs(rc), top_probs(rs_)),
                "kl_ctrl_ref": kl(top_probs(ref), top_probs(rc)),
                "answer_ctrl": rc["content"][:60],
                "answer_stitch": rs_["content"][:60],
                "correct_ctrl": p["ans_C"] in rc["content"],
                "correct_stitch": p["ans_C"] in rs_["content"],
            }
            rows.append(row)
            print("  k=%-6d n=%-7d pn=%-5d/%-5d(exp %-5d) div_stitch_vs_ctrl=%-4d "
                  "div_ctrl_vs_ref=%-4d KL(c||s)=%-8s KL(r||c)=%-8s ansC=%s/%s"
                  % (k, n, row["prompt_n_ctrl"], row["prompt_n_stitch"], row["prompt_n_expect"],
                     row["div_vs_ctrl"], row["div_ctrl_ref"],
                     ("%.5f" % row["kl_first"]) if row["kl_first"] is not None else "n/a",
                     ("%.5f" % row["kl_ctrl_ref"]) if row["kl_ctrl_ref"] is not None else "n/a",
                     row["correct_ctrl"], row["correct_stitch"]), flush=True)

        results.append({
            "seed": seed, "a": a, "b": b, "c": c, "n_P": len(P), "n_Q": len(Q),
            "npred": args.npred,
            "validity_ok": validity_ok,
            "ref_answer": ref["content"], "ref_answer_B": ref_b["content"],
            "stitched_answer_B": vb["content"],
            "truth_C": p["ans_C"] + " / " + p["own_C"],
            "truth_B": p["ans_B"] + " / " + p["own_B"],
            "ref_tokens": ref_toks,
            "rows": rows, "wall_s": time.time() - t0,
        })
        with open(args.out, "w") as f:
            json.dump(results, f, indent=1)
        print("  [seed %d done in %.0fs]" % (seed, time.time() - t0), flush=True)
    print("wrote", args.out)


if __name__ == "__main__":
    main()
