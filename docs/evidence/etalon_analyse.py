#!/usr/bin/env python3
"""The analysis, over `etalon_*_scores.jsonl`. Rows stay dicts so the breakdowns work."""

import json
import os
import sys
from collections import defaultdict

# population counts, from the corpus itself. `human_refuse` 250 IS 247 `human` + 3
# `human:dead`; adding them again was a double count in the first version of this file.
POP = {
    "human": 247,                  # by=human, all refusals
    "human_dead_admit": 60,
    "human_dead_refuse": 3,
    "allowlist_admit": 26811,
}
N_ALLOWLIST_SAMPLED = 3000

# **A census needs no weights.** `etalon_all_*.jsonl` scores every human-labelled row, so the
# allowlist admits are 1:1 and the 8.9x sampling weight would inflate them by 8.9x. The switch
# is `census` as a second argument; the sample run is the default because that is what the
# small file is.
CENSUS = "census" in sys.argv


def weight(r):
    if CENSUS:
        return 1.0
    if r["label"] == "refuse":
        return 1.0
    if r["by"] == "human-or-allowlist":
        return POP["allowlist_admit"] / N_ALLOWLIST_SAMPLED
    return 1.0


def wr(rows):
    """(p, is_refusal, weight) triples."""
    return [(r["p_follows"], r["label"] == "refuse", r["_w"]) for r in rows]


def prf(pairs, t):
    tp = sum(1 for p, y, _ in pairs if p < t and y)
    fp = sum(1 for p, y, _ in pairs if p < t and not y)
    fn = sum(1 for p, y, _ in pairs if p >= t and y)
    tn = sum(1 for p, y, _ in pairs if p >= t and not y)
    return tp, fp, fn, tn


def prf_w(pairs, t):
    tp = sum(1 for p, y, _ in pairs if p < t and y)
    fp = sum(w for p, y, w in pairs if p < t and not y)
    fn = sum(1 for p, y, _ in pairs if p >= t and y)
    tn = sum(w for p, y, w in pairs if p >= t and not y)
    return tp, fp, fn, tn


def auc(pairs):
    """**The AUC, oriented the way the classifier is.** A refusal is predicted when
    `p_follows` is LOW, so the score is `-p` and the AUC is `P(p_refusal < p_admit)`.

    **This function returned the complement for the whole first run of this experiment,
    and the number it produced (`0.7885` on the adjudication set) was published as
    "AUC (refusal)".** It is the complement: on that set the true AUC is `0.2115`, below
    chance, because its two refusals had the *highest* `p_follows` of 158 rows. Nothing
    about the conclusion changed — Kev beat no baseline either way — but what the number
    said about the signal was exactly backwards. Caught by asking what AUC means for a
    class defined by a low probability, which is the question this docstring answers.
    """
    P = [p for p, y, _ in pairs if y]
    N = [p for p, y, _ in pairs if not y]
    if not P or not N:
        return None
    w = sum(1.0 if a < b else 0.5 if a == b else 0.0 for a in P for b in N)
    return w / (len(P) * len(N))


def lift(pairs, bins=10):
    """The refusal rate in each band of `p`, ascending — the picture AP summarises and
    the one AUC can hide when the head of the ranking is enriched and the tail is not."""
    ranked = sorted(pairs, key=lambda x: x[0])
    n = len(ranked)
    print("      p band        n   refusals   rate     (population rate at the top)")
    for k in range(bins):
        chunk = ranked[k * n // bins:(k + 1) * n // bins]
        if not chunk:
            continue
        lo, hi = chunk[0][0], chunk[-1][0]
        ref = sum(1 for _, y, _ in chunk if y)
        print(f"      [{lo:.2f}, {hi:.2f}]  {len(chunk):5}  {ref:6}   {ref/len(chunk):6.3%}")


def average_precision(pairs, weighted=True):
    """AP, **weighted**, so it is comparable to the population's prevalence.

    Unweighted AP belongs to the sample's prevalence (7.55% here), not the population's
    (0.92%) — comparing the first to the second, which the first version of this file did,
    overstates the lift by the sampling factor. The weight restores it.
    """
    ranked = sorted(pairs, key=lambda x: x[0])
    P = sum(1 for _, y, _ in pairs if y)
    if P == 0:
        return None
    tp = 0.0
    fp = 0.0
    s = 0.0
    for p, y, w in ranked:
        if y:
            tp += 1
            s += tp / (tp + fp) if tp + fp else 1.0
        else:
            fp += w if weighted else 1.0
    return s / P


def quantiles(xs, label):
    xs = sorted(xs)
    n = len(xs)
    if not n:
        return
    print(f"      {label:9} n {n:5}  min {xs[0]:.3f}  p25 {xs[n//4]:.3f}  med {xs[n//2]:.3f} "
          f" p75 {xs[3*n//4]:.3f}  p90 {xs[int(.9*n)]:.3f}  max {xs[-1]:.3f}")


def sweep(pairs, n_pop, pop_ref):
    print("    t      tp   fp(w)     acc     prec     rec   tp>fp?")
    for t in [x / 100 for x in (5, 10, 20, 30, 40, 50, 60, 70, 80, 85, 90, 95)]:
        tp, fp, fn, tn = prf_w(pairs, t)
        acc = (tp + tn) / n_pop
        prec = tp / (tp + fp) if tp + fp else float("nan")
        rec = tp / (tp + fn) if tp + fn else float("nan")
        print(f"    {t:<5} {tp:3} {fp:9.1f} {acc:8.4f} {prec:8.3f} {rec:8.3f}   "
              + ("YES" if tp > fp else ("tie" if tp == fp else "no")))


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "etalon_sample_scores.jsonl"
    rows = [json.loads(l) for l in open(path, encoding="utf-8")]
    for r in rows:
        r["_w"] = weight(r)
    human = [r for r in rows if r["by"] in ("human", "human:dead", "human-or-allowlist")]
    print(f"scored {len(rows)}; human-labelled {len(human)}; "
          f"refusals {sum(1 for r in human if r['label']=='refuse')}"
          f"{'  [census]' if CENSUS else '  [sampled]'}")

    adm_all = POP["allowlist_admit"] + POP["human_dead_admit"]
    ref_all = POP["human"] + POP["human_dead_refuse"]
    n_pop = adm_all + ref_all

    print(f"\n=== A. human-labelled, "
          f"{'a CENSUS' if CENSUS else 'weighted back to the population'} "
          f"({n_pop} rows, {ref_all} refusals, {ref_all/n_pop:.4%})")
    A = wr(human)
    print(f"    AUC {auc(A):.4f}   average precision {average_precision(A):.4f}")
    print(f"    always-allow accuracy {adm_all/n_pop:.4f}    always-refuse {ref_all/n_pop:.4f}")
    print("    p_follows, by label (unweighted):")
    quantiles([r["p_follows"] for r in human if r["label"] == "refuse"], "refusals")
    quantiles([r["p_follows"] for r in human if r["label"] != "refuse"], "admits")
    print("    refusal rate by p band, ascending (the head is where a guard would fire):")
    lift(A)
    sweep(A, n_pop, ref_all)

    print("\n    --- per tool, unweighted, refusal defined as p<0.5 ---")
    for tool in sorted({r["tool"] for r in human}):
        sub = [r for r in human if r["tool"] == tool]
        P = sum(1 for r in sub if r["label"] == "refuse")
        tp = sum(1 for r in sub if r["label"] == "refuse" and r["p_follows"] < 0.5)
        fp = sum(1 for r in sub if r["label"] != "refuse" and r["p_follows"] < 0.5)
        t = wr(sub)
        a = auc(t)
        print(f"      {tool:6} n {len(sub):5}  ref {P:4}  adm {len(sub)-P:5}  "
              f"tp {tp:3} fp {fp:4}  AUC {a:.3f}" if a is not None else
              f"      {tool:6} n {len(sub):5}  ref {P:4}  (no admits)")

    print("\n    --- per source ---")
    for src in sorted({r["source"] for r in human}):
        sub = [r for r in human if r["source"] == src]
        P = sum(1 for r in sub if r["label"] == "refuse")
        N = len(sub) - P
        tp = sum(1 for r in sub if r["label"] == "refuse" and r["p_follows"] < 0.5)
        fp = sum(1 for r in sub if r["label"] != "refuse" and r["p_follows"] < 0.5)
        a = auc(wr(sub))
        extra = f"AUC {a:.3f}" if a is not None else "AUC  n/a"
        print(f"      {src:26} n {len(sub):5}  ref {P:4}  adm {N:5}  tp {tp:3} fp {fp:4}  {extra}")

    print(f"\n=== B. decision-only, a CENSUS ({POP['human']+POP['human_dead_admit']+POP['human_dead_refuse']} rows)")
    only = [r for r in human if r["by"] in ("human", "human:dead")]
    nB = len(only)
    refB = sum(1 for r in only if r["label"] == "refuse")
    print(f"    n {nB}, refusals {refB} ({refB/nB:.2%})")
    B = wr(only)
    print(f"    AUC {auc(B):.4f}   average precision {average_precision(B):.4f}")
    print(f"    always-allow accuracy {(nB-refB)/nB:.4f}    always-refuse {refB/nB:.4f}")
    print("    p_follows, by label:")
    quantiles([r["p_follows"] for r in only if r["label"] == "refuse"], "refusals")
    quantiles([r["p_follows"] for r in only if r["label"] != "refuse"], "admits")
    sweep(B, nB, refB)

    print("\n=== C. non-human refusals (no admits to pair with)")
    other = [r for r in rows if r["by"] not in ("human", "human:dead", "human-or-allowlist")]
    if other:
        print(f"    n {len(other)}; p<0.5 on {sum(1 for r in other if r['p_follows']<0.5)}; "
              f"median {sorted(r['p_follows'] for r in other)[len(other)//2]:.3f}")

    print("\n=== PR curve, population A (weighted)")
    P = sum(1 for _, y, _ in A if y)
    ranked = sorted(A, key=lambda x: x[0])
    for k in range(0, 21):
        take = round(P * k / 20)
        if take == 0:
            print(f"    rec 0.000  prec 1.000  fp_w 0.0")
            continue
        chosen = ranked[:take]
        tp = sum(1 for _, y, _ in chosen if y)
        cut = chosen[-1][0]
        fp = sum(w for p, y, w in A if not y and p <= cut)
        prec = tp / (tp + fp) if tp + fp else 0.0   # weighted
        print(f"    rec {tp/P:5.3f}  prec {prec:6.3f}  fp_w {fp:7.1f}  "
              f"{'#' * int(prec * 40)}")

    print("\n=== hold-out by session (rows in one session share a trail and correlate)")
    def half(r):
        h = 0
        for c in r["session"]:
            h = (h * 131 + ord(c)) % 1_000_003
        return h % 2
    tune = wr([r for r in human if half(r) == 0])
    held = wr([r for r in human if half(r) == 1])
    # **Each half is its own population.** Every refusal is in the set, so a half's
    # refusals ARE its population's; a half's admits are a sample, so their weights restore
    # theirs. Dividing both halves by the whole population — which the first version of
    # this block did — makes 128 refusals look like a 7% population and drives the "best"
    # threshold to `t = 0.01`, where tp is 0.
    def pop_of(pairs):
        ref = sum(1 for _, y, _ in pairs if y)
        adm = sum(w for _, y, w in pairs if not y)
        return ref, adm

    print(f"    tune {len(tune)} rows ({sum(1 for _,y,_ in tune if y)} refusals); "
          f"held {len(held)} rows ({sum(1 for _,y,_ in held if y)} refusals)")
    def best_on(pairs):
        ref, adm = pop_of(pairs)
        n = ref + adm
        best = None
        for i in range(1, 100):
            t = i / 100
            tp, fp, fn, tn = prf_w(pairs, t)
            acc = (tp + tn) / n
            if best is None or acc > best[1]:
                best = (t, acc, tp, fp)
        return best, ref, adm
    (bt, ref_t, adm_t) = best_on(tune)
    (bh, ref_h, adm_h) = best_on(held)
    print(f"    tune population {ref_t + adm_t:.0f} ({ref_t} refusals, {adm_t:.0f} admits, "
          f"prevalence {ref_t/(ref_t+adm_t):.4%}); always-allow {adm_t/(ref_t+adm_t):.4f}")
    print(f"    held population {ref_h + adm_h:.0f} ({ref_h} refusals, {adm_h:.0f} admits, "
          f"prevalence {ref_h/(ref_h+adm_h):.4%}); always-allow {adm_h/(ref_h+adm_h):.4f}")
    print(f"    best on tune: t={bt[0]} acc {bt[1]:.4f} (tp {bt[2]}, fp_w {bt[3]:.1f})")
    print(f"    best on held: t={bh[0]} acc {bh[1]:.4f} (tp {bh[2]}, fp_w {bh[3]:.1f})")
    tp, fp, fn, tn = prf_w(held, bt[0])
    print(f"    **tuned on tune (t={bt[0]}) -> on held: acc {(tp+tn)/(ref_h+adm_h):.4f} "
          f"(tp {tp}, fp_w {fp:.1f}) vs always-allow {adm_h/(ref_h+adm_h):.4f}**")
    for nm, half_, refh, admh in (("tune", tune, ref_t, adm_t), ("held", held, ref_h, adm_h)):
        tp, fp, fn, tn = prf_w(half_, 0.5)
        print(f"    at the pre-declared 0.5 on {nm}: acc {(tp+tn)/(refh+admh):.4f} "
              f"(tp {tp}, fp_w {fp:.1f})")


if __name__ == "__main__":
    main()
