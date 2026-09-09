"""UNVERIFIED-16: does a bounded recompute repair a KV state that was computed
over a different history, on a hybrid (attention + recurrent) model?

Construction, per docs/implementation-plan.md 3.10-C:

    P     = A ++ B ++ C          the prompt we want to answer
    P_alt = A ++ B_alt ++ C      the same block C, same positions, computed
                                 behind a DIFFERENT intervening block

  |B| == |B_alt| in tokens, so C occupies identical positions in both. RoPE is
  therefore not a confound: the only thing that differs about C's cached KV is
  the CONTEXT it was computed in. That is obstacle 2 in isolation.

  The stitched state for recompute width k is built by prefilling the first
  (|P| - k) tokens of P_alt and then relabelling the saved token list to the
  first (|P| - k) tokens of P. The server then believes it holds a prefix of P
  and prefills only the trailing k tokens (plus the question) on top of it.

  k = 0            no reconciliation at all
  0 < k < |C|      the trailing k tokens of C recomputed at their true position
  k = |C|          all of C recomputed, but on top of the A++B_alt state
  k = |C| + |B|    the recompute starts at the divergence -> exact by
                   construction; this is the determinism control.

  Recomputing a SUFFIX rather than CacheBlend's prefix-of-the-block is
  deliberate and is the stronger test: on a recurrent layer the state is a
  left-to-right fold, so a recomputed prefix is immediately overwritten by the
  stale later state, while a recomputed suffix is the only placement that can
  carry corrected state to the generation point. If the suffix form fails, the
  prefix form fails a fortiori.
"""
import argparse, json, os, random, struct, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import srv
import corpus

SAVE_DIR = "/data/kvcache"
SAVE_NAME = "unv16-stitch.bin"


# ---------------------------------------------------------------- prompt build

def build(seed, a_recs, b_recs, c_recs):
    rng = random.Random(seed)
    A_recs = corpus.records(rng, a_recs, "AA")
    B_recs = corpus.records(rng, b_recs, "BB")
    Balt_recs = corpus.records(rng, b_recs, "BX")
    C_recs = corpus.records(rng, c_recs, "CC")

    A_txt = corpus.render(A_recs, "LIST ONE - archive manifest")
    B_txt = corpus.render(B_recs, "LIST TWO - transfer manifest")
    Balt_txt = corpus.render(Balt_recs, "LIST TWO - transfer manifest")
    C_txt = corpus.render(C_recs, "LIST THREE - settlement manifest")

    A = srv.tokenize(A_txt)
    B = srv.tokenize(B_txt)
    Balt = srv.tokenize(Balt_txt)
    C = srv.tokenize(C_txt)

    # |B| must equal |B_alt| exactly so that C keeps its positions
    b = min(len(B), len(Balt))
    B, Balt = B[:b], Balt[:b]

    # the questioned records must survive the truncation and sit away from the
    # very end of their block
    q_c_rec = C_recs[len(C_recs) // 2]
    q_b_rec = B_recs[len(B_recs) // 3]

    Q_C = srv.tokenize(corpus.question(q_c_rec, "LIST THREE - settlement manifest"))
    Q_B = srv.tokenize(corpus.question(q_b_rec, "LIST TWO - transfer manifest"))

    return {
        "seed": seed,
        "A": A, "B": B, "Balt": Balt, "C": C,
        "Q_C": Q_C, "Q_B": Q_B,
        "ans_C": "%d" % q_c_rec["value"], "own_C": q_c_rec["owner"],
        "ans_B": "%d" % q_b_rec["value"], "own_B": q_b_rec["owner"],
        "alt_ans_C": None,
    }


# ---------------------------------------------------------------- state doctor

def patch_tokens(path, new_tokens):
    """Rewrite the token list inside a llama_state_seq save file in place.

    Layout: u32 magic, u32 version, u32 n_packed, n_packed * i32 packed, state.
    The packed array is server_tokens::serialize():
        [ -1 marker, version, count, tokens..., n_media(=0) ]
    Only the tokens are rewritten; the count and everything else is unchanged,
    so the file stays exactly the same size and the KV blob is untouched.
    """
    with open(path, "r+b") as f:
        magic, ver, n_packed = struct.unpack("<III", f.read(12))
        assert magic == 0x67677371, "unexpected magic %08x" % magic
        marker, tver, count = struct.unpack("<iII", f.read(12))
        assert marker == -1, "unexpected packed marker %d" % marker
        assert count == len(new_tokens), "token count %d != %d" % (count, len(new_tokens))
        f.write(struct.pack("<%di" % count, *new_tokens))
    return count


# ---------------------------------------------------------------- one condition

def first_divergence(a, b):
    n = min(len(a), len(b))
    for i in range(n):
        if a[i] != b[i]:
            return i
    return n if len(a) == len(b) else n


def top_probs(res, idx=0):
    cp = res.get("completion_probabilities") or []
    if idx >= len(cp):
        return {}
    out = {}
    for e in cp[idx].get("top_logprobs", cp[idx].get("probs", [])):
        tok = e.get("id", e.get("tok_str"))
        lp = e.get("logprob")
        if lp is None and "prob" in e:
            import math
            lp = math.log(max(e["prob"], 1e-30))
        out[tok] = lp
    return out


def kl(p_lp, q_lp):
    """KL(P||Q) over the tokens P puts mass on; Q gets a floor where absent."""
    import math
    if not p_lp or not q_lp:
        return None
    floor = min(q_lp.values()) - 5.0
    tot = 0.0
    for t, lp in p_lp.items():
        p = math.exp(lp)
        q = math.exp(q_lp.get(t, floor))
        tot += p * (lp - math.log(max(q, 1e-30)))
    return tot


def run_condition(P_alt_pref, P_pref, full, Q, n_predict, slot, n_probs):
    """Prefill the alt prefix, relabel it as the true prefix, restore, ask."""
    srv.slot_erase(slot)
    r0 = srv.complete(P_alt_pref, 0, slot, cache_prompt=True)
    got = r0["timings"]["prompt_n"]
    srv.slot_save(slot, SAVE_NAME)
    patch_tokens(os.path.join(SAVE_DIR, SAVE_NAME), P_pref)
    srv.slot_erase(slot)
    rr = srv.slot_restore(slot, SAVE_NAME)
    res = srv.complete(full + Q, n_predict, slot, cache_prompt=True, n_probs=n_probs)
    return res, {"prefill_alt_n": got, "restored_n": rr["n_restored"] if "n_restored" in rr else rr.get("n_tokens")}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--slot", type=int, default=4)
    ap.add_argument("--seeds", type=int, nargs="+", default=[11])
    ap.add_argument("--a", type=int, default=40)
    ap.add_argument("--b", type=int, default=10)
    ap.add_argument("--c", type=int, default=80)
    ap.add_argument("--npred", type=int, default=200)
    ap.add_argument("--ks", type=str, default="")
    ap.add_argument("--out", type=str, required=True)
    args = ap.parse_args()

    results = []
    for seed in args.seeds:
        t0 = time.time()
        p = build(seed, args.a, args.b, args.c)
        A, B, Balt, C = p["A"], p["B"], p["Balt"], p["C"]
        a, b, c = len(A), len(B), len(C)
        P = A + B + C
        P_alt = A + Balt + C
        assert len(P) == len(P_alt)

        if args.ks:
            ks = [int(x) for x in args.ks.split(",")]
        else:
            ks = [0, 1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024]
            ks = [k for k in ks if k < c]
            ks += [c, c + b // 2, c + b]

        print("[seed %d] a=%d b=%d c=%d |P|=%d ks=%s" % (seed, a, b, c, len(P), ks), flush=True)

        # reference: cold full prefill of P ++ Q
        srv.slot_erase(args.slot)
        ref_C = srv.complete(P + p["Q_C"], args.npred, args.slot, cache_prompt=False, n_probs=20)
        srv.slot_erase(args.slot)
        ref_B = srv.complete(P + p["Q_B"], 32, args.slot, cache_prompt=False, n_probs=0)
        ref_C_toks = ref_C["tokens"]
        ref_B_txt = ref_B["content"]
        ref_lp = top_probs(ref_C)
        print("  ref prompt_n=%d (expect %d) answer_C=%r truth=%s/%s" %
              (ref_C["timings"]["prompt_n"], len(P) + len(p["Q_C"]),
               ref_C["content"][:60], p["ans_C"], p["own_C"]), flush=True)
        print("  ref answer_B=%r truth=%s/%s" % (ref_B_txt[:60], p["ans_B"], p["own_B"]), flush=True)

        # control R: save/restore round trip with NO doctoring at all. Any
        # divergence here is the noise floor of the pipeline, not the stitch.
        rt, _ = run_condition(P, P, P, p["Q_C"], args.npred, args.slot, 20)
        rt_div = first_divergence(ref_C_toks, rt["tokens"])
        print("  [control roundtrip] prompt_n=%d div=%d/%d" %
              (rt["timings"]["prompt_n"], rt_div, len(ref_C_toks)), flush=True)

        rows = []
        for k in ks:
            n = len(P) - k
            P_alt_pref = P_alt[:n]
            P_pref = P[:n]
            resC, metaC = run_condition(P_alt_pref, P_pref, P, p["Q_C"], args.npred,
                                        args.slot, 20)
            div = first_divergence(ref_C_toks, resC["tokens"])
            row = {
                "seed": seed, "k": k, "n_cached": n,
                "prompt_n": resC["timings"]["prompt_n"],
                "prompt_n_expect": k + len(p["Q_C"]),
                "divergence": div,
                "n_pred": len(resC["tokens"]),
                "exact": div >= min(len(ref_C_toks), len(resC["tokens"])) and len(ref_C_toks) == len(resC["tokens"]),
                "kl_first": kl(ref_lp, top_probs(resC)),
                "answer_C": resC["content"][:80],
                "ref_answer_C": ref_C["content"][:80],
                "truth_C": p["ans_C"] + " / " + p["own_C"],
            }
            # positive control: a question about B, which the stitched state saw
            # only as B_alt. if this comes back right, the doctoring did not take.
            resB, _ = run_condition(P_alt_pref, P_pref, P, p["Q_B"], 32, args.slot, 0)
            row["answer_B"] = resB["content"][:80]
            row["truth_B"] = p["ans_B"] + " / " + p["own_B"]
            row["answer_B_correct"] = p["ans_B"] in resB["content"]
            row["prompt_n_B"] = resB["timings"]["prompt_n"]
            rows.append(row)
            print("  k=%-6d cached=%-7d prompt_n=%-6d (exp %-6d) div=%-4d exact=%-5s KL=%s  C=%r  B_ok=%s"
                  % (k, n, row["prompt_n"], row["prompt_n_expect"], div, row["exact"],
                     ("%.4f" % row["kl_first"]) if row["kl_first"] is not None else "n/a",
                     row["answer_C"][:34], row["answer_B_correct"]), flush=True)

        results.append({
            "seed": seed, "a": a, "b": b, "c": c, "n_P": len(P),
            "ref_prompt_n": ref_C["timings"]["prompt_n"],
            "ref_answer_C": ref_C["content"], "ref_answer_B": ref_B_txt,
            "truth_C": p["ans_C"] + " / " + p["own_C"],
            "truth_B": p["ans_B"] + " / " + p["own_B"],
            "ref_tokens": ref_C_toks,
            "control_roundtrip_div": rt_div,
            "control_roundtrip_prompt_n": rt["timings"]["prompt_n"],
            "control_roundtrip_answer": rt["content"][:80],
            "rows": rows,
            "wall_s": time.time() - t0,
        })
        with open(args.out, "w") as f:
            json.dump(results, f, indent=1)
    print("wrote", args.out)


if __name__ == "__main__":
    main()
