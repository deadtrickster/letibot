"""UNVERIFIED-16, run 3: the same k-sweep, with the noise floor measured
alongside every point instead of assumed.

Runs 1 and 2 established that exact 200-token greedy agreement is not a metric
on this server: two runs with identical inputs, identical construction,
temperature 0, top_k 1 and a fixed seed diverge at token 12-43 of 200. Turning
MTP drafting off per request did not fix it, so the source is the shared decode
batch - four other slots are serving other agents, and what else is in the batch
changes the reduction order. That is a property of the box, not of the stitch.

So every k is measured three times:

    ctrl   prime P[:n] (true history), restore, ask
    ctrl2  the same thing again - this pair IS the noise floor
    stitch prime P_alt[:n], relabel to P[:n], restore, ask

and the stitch is only claimed to differ from the control where it differs by
more than ctrl2 does. Reported per k: first-divergence and the top-20 KL at the
join for both the stitch and the repeat.
"""
import argparse, json, os, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import srv
from stitch import build, first_divergence, top_probs, kl
from stitch2 import prime


def argmax_tok(res):
    cp = res.get("completion_probabilities") or []
    return res["tokens"][0] if res.get("tokens") else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--slot", type=int, default=4)
    ap.add_argument("--seeds", type=int, nargs="+", default=[21, 22, 23])
    ap.add_argument("--b", type=int, default=30)
    ap.add_argument("--c", type=int, default=8)
    ap.add_argument("--npred", type=int, default=200)
    ap.add_argument("--ks", type=str, default="0,4,16,64,c,cb")
    ap.add_argument("--out", type=str, required=True)
    args = ap.parse_args()

    out = []
    for seed in args.seeds:
        p = build(seed, 1, args.b, args.c)
        B, Balt, C, Q = p["B"], p["Balt"], p["C"], p["Q_C"]
        b, c = len(B), len(C)
        P, P_alt = B + C, Balt + C
        sym = {"c": c, "cb": c + b}
        ks = sorted(set(min(sym.get(x, None) or int(x), len(P) - 1)
                        for x in args.ks.split(",")))
        print("[seed %d] b=%d c=%d |P|=%d |Q|=%d ks=%s" % (seed, b, c, len(P), len(Q), ks),
              flush=True)

        rows = []
        for k in ks:
            srv.guard()
            n = len(P) - k

            prime(P[:n], None, args.slot)
            rc = srv.complete(P + Q, args.npred, args.slot, True, 20, True)
            prime(P[:n], None, args.slot)
            rc2 = srv.complete(P + Q, args.npred, args.slot, True, 20, True)
            prime(P_alt[:n], P[:n], args.slot)
            rs = srv.complete(P + Q, args.npred, args.slot, True, 20, True)

            lc, lc2, ls = top_probs(rc), top_probs(rc2), top_probs(rs)
            row = {
                "seed": seed, "k": k, "n_cached": n,
                "prompt_n": rs["timings"]["prompt_n"], "prompt_n_expect": k + len(Q),
                "div_floor": first_divergence(rc["tokens"], rc2["tokens"]),
                "div_stitch": first_divergence(rc["tokens"], rs["tokens"]),
                "kl_floor": kl(lc, lc2),
                "kl_stitch": kl(lc, ls),
                "tok1_floor_same": rc["tokens"][0] == rc2["tokens"][0],
                "tok1_stitch_same": rc["tokens"][0] == rs["tokens"][0],
                "correct_ctrl": p["ans_C"] in rc["content"],
                "correct_stitch": p["ans_C"] in rs["content"],
            }
            rows.append(row)
            print("  k=%-5d n=%-6d pn=%-5d(exp %-5d) div: floor=%-4d stitch=%-4d | "
                  "KL: floor=%-9s stitch=%-9s | ansC ctrl=%s stitch=%s"
                  % (k, n, row["prompt_n"], row["prompt_n_expect"],
                     row["div_floor"], row["div_stitch"],
                     "%.6f" % row["kl_floor"] if row["kl_floor"] is not None else "n/a",
                     "%.6f" % row["kl_stitch"] if row["kl_stitch"] is not None else "n/a",
                     row["correct_ctrl"], row["correct_stitch"]), flush=True)
        out.append({"seed": seed, "b": b, "c": c, "n_P": len(P), "rows": rows})
        with open(args.out, "w") as f:
            json.dump(out, f, indent=1)
    print("wrote", args.out)


if __name__ == "__main__":
    main()
