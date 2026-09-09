"""How far back can a hybrid slot re-enter its own history without re-folding?

This is the recompute-fraction question stripped of everything else. No
doctoring, no relabelling, no stitch: just a slot holding the KV for a prompt P
of N tokens, asked for the honest prefix P[:N-d]. Removing the last d tokens is
the cheapest possible edit, and the tokens that remain were computed in exactly
the right context at exactly the right positions.

For an attention-only model the answer is "free at any d": drop the cells, keep
the rest.

For a hybrid model llama_memory_recurrent::seq_rm refuses a partial erase unless
the rollback lands inside the per-token snapshot window n_rs_seq, and
llama_memory_hybrid::seq_rm propagates that refusal to the attention cache as
well. So the measured prompt_n is the number of tokens the server had to run
again, and the smallest d at which it stops being ~0 is the width of the only
window in which a partial recompute exists at all.

Reported: prompt_n against d. cheap (prompt_n ~ 0) vs re-folded.
"""
import argparse, json, os, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import srv
import corpus
import random


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--slot", type=int, default=4)
    ap.add_argument("--recs", type=int, default=200)
    ap.add_argument("--ds", type=str, default="1,2,3,4,5,6,7,8,16,64,256,1024,4096")
    ap.add_argument("--out", type=str, required=True)
    args = ap.parse_args()

    rows = []
    for i, d in enumerate([int(x) for x in args.ds.split(",")]):
        # a fresh prompt for every d, so nothing an earlier iteration left in
        # the server's prompt cache can serve this one
        recs = corpus.records(random.Random(700 + i), args.recs, "TT")
        P = srv.tokenize(corpus.render(recs, "MANIFEST"))
        N = len(P)
        if d >= N:
            continue
        srv.guard()
        srv.slot_erase(args.slot)
        r0 = srv.complete(P, 0, args.slot, cache_prompt=True)
        # now ask for the honest prefix: the last d tokens must leave the cache
        r1 = srv.complete(P[:N - d], 0, args.slot, cache_prompt=True)
        pn = r1["timings"]["prompt_n"]
        rows.append({"d": d, "N": N, "n_prefix": N - d, "prompt_n": pn,
                     "prime_prompt_n": r0["timings"]["prompt_n"],
                     "fraction_refolded": pn / float(N - d)})
        print("  d=%-6d prefix=%-7d prompt_n=%-7d  refolded=%.1f%% of the prefix"
              % (d, N - d, pn, 100.0 * pn / (N - d)), flush=True)

    with open(args.out, "w") as f:
        json.dump({"rows": rows}, f, indent=1)
    print("wrote", args.out)


if __name__ == "__main__":
    main()
