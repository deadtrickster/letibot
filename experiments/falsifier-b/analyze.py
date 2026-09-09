"""Aggregate raw/samples.jsonl into results.json and a printable table."""
import json, math, pathlib, statistics, sys

HERE = pathlib.Path(__file__).resolve().parent
RAW = HERE / "raw"


def wilson(k, n, z=1.96):
    """95% Wilson score interval for a binomial proportion."""
    if n == 0:
        return (0.0, 0.0)
    p = k / n
    d = 1 + z * z / n
    c = p + z * z / (2 * n)
    r = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n))
    return ((c - r) / d, (c + r) / d)


def load():
    return [json.loads(l) for l in open(RAW / "samples.jsonl") if l.strip()]


def main():
    rows = load()
    manifest = json.loads((RAW / "manifest.json").read_text())
    depths = sorted({r["depth_target"] for r in rows})
    tasks = manifest["tasks"]

    out = {"manifest": manifest, "by_depth": {}, "by_depth_task": {}, "cache": {}}

    conc = manifest.get("concurrency", 1)
    print(f"\n{len(rows)} samples, temp {manifest['server_defaults']['temperature']}, "
          f"top_p {manifest['server_defaults']['top_p']}, "
          f"top_k {manifest['server_defaults']['top_k']}, "
          f"max_tokens {manifest['max_tokens']}, concurrency {conc} "
          f"(one worker per depth)\n")

    # ---- headline: pass rate by depth --------------------------------------
    print("PASS RATE BY DEPTH (tests_pass, first attempt, no retries)")
    print(f"{'depth':>8} {'tokens':>8} {'n':>4} {'trunc':>6} {'nocode':>7} "
          f"{'compiles':>9} {'pass':>6} {'rate':>7} {'95% CI':>16} {'per-rep spread':>18}")
    for d in depths:
        rs = [r for r in rows if r["depth_target"] == d]
        trunc = [r for r in rs if r.get("outcome") == "truncated"]
        terr = [r for r in rs if r.get("outcome") == "transport_error"]
        sc = [r for r in rs if r.get("outcome") == "scored"]
        n = len(sc)
        nocode = sum(1 for r in sc if not r.get("has_code"))
        comp = sum(1 for r in sc if r.get("compiles"))
        pas = sum(1 for r in sc if r.get("tests_pass"))
        lo, hi = wilson(pas, n)
        # spread: pass rate computed within each rep (one full task sweep)
        reps = sorted({r["rep"] for r in sc})
        per_rep = []
        for rp in reps:
            g = [r for r in sc if r["rep"] == rp]
            if g:
                per_rep.append(sum(1 for r in g if r["tests_pass"]) / len(g))
        spread = (f"{min(per_rep):.2f}-{max(per_rep):.2f} "
                  f"sd={statistics.pstdev(per_rep):.3f}") if per_rep else "-"
        tok = manifest["depths"][str(d)]["actual_tokens"]
        ci = f"[{lo:.2f},{hi:.2f}]"
        print(f"{d:>8} {tok:>8} {n:>4} {len(trunc):>6} {nocode:>7} {comp:>9} "
              f"{pas:>6} {pas/n if n else 0:>7.3f} {ci:>16} {spread:>18}")
        out["by_depth"][str(d)] = {
            "depth_tokens": tok, "n_scored": n, "n_truncated": len(trunc),
            "n_transport_error": len(terr), "n_no_code_block": nocode,
            "compiles": comp, "tests_pass": pas,
            "compile_rate": comp / n if n else None,
            "pass_rate": pas / n if n else None,
            "pass_rate_ci95": [lo, hi],
            "per_rep_pass_rates": per_rep,
            "per_rep_sd": statistics.pstdev(per_rep) if per_rep else None,
        }

    # ---- per task x depth ---------------------------------------------------
    print("\nPASS RATE BY TASK x DEPTH  (pass/n)")
    hdr = f"{'task':24s}" + "".join(f"{('d='+str(d)):>14}" for d in depths)
    print(hdr)
    for t in tasks:
        line = f"{t:24s}"
        for d in depths:
            sc = [r for r in rows
                  if r["depth_target"] == d and r["task"] == t
                  and r.get("outcome") == "scored"]
            p = sum(1 for r in sc if r["tests_pass"])
            line += f"{p}/{len(sc)}".rjust(14)
            out["by_depth_task"].setdefault(t, {})[str(d)] = {
                "pass": p, "n": len(sc),
                "compiles": sum(1 for r in sc if r["compiles"])}
        print(line)

    # ---- cache evidence -----------------------------------------------------
    print("\nPROMPT CACHE (cache_prompt=true, identical filler prefix per depth)")
    print(f"{'depth':>8} {'first cache_n':>14} {'first prompt_n':>15} "
          f"{'median cache_n':>15} {'median prompt_n':>16} {'median prompt_ms':>17}")
    for d in depths:
        rs = [r for r in rows if r["depth_target"] == d and r.get("cache_n") is not None]
        if not rs:
            continue
        rs.sort(key=lambda r: r["idx"])
        rest = rs[1:] or rs
        row = {
            "first_cache_n": rs[0]["cache_n"], "first_prompt_n": rs[0]["prompt_n"],
            "first_prompt_ms": rs[0]["prompt_ms"],
            "median_cache_n": statistics.median(r["cache_n"] for r in rest),
            "median_prompt_n": statistics.median(r["prompt_n"] for r in rest),
            "median_prompt_ms": statistics.median(r["prompt_ms"] for r in rest),
            "median_predicted_per_second": statistics.median(
                r["predicted_per_second"] for r in rest if r.get("predicted_per_second")),
            "median_wall_s": statistics.median(r["wall_s"] for r in rest),
        }
        out["cache"][str(d)] = row
        print(f"{d:>8} {row['first_cache_n']:>14} {row['first_prompt_n']:>15} "
              f"{row['median_cache_n']:>15.0f} {row['median_prompt_n']:>16.0f} "
              f"{row['median_prompt_ms']:>17.1f}")

    # ---- run losses ---------------------------------------------------------
    tr = [r for r in rows if r.get("outcome") == "truncated"]
    te = [r for r in rows if r.get("outcome") == "transport_error"]
    print(f"\nRun losses: {len(tr)} truncated (finish_reason=length), "
          f"{len(te)} transport errors, out of {len(rows)}")
    print(f"{'depth':>8} {'truncated':>10} {'issued':>8} {'trunc rate':>11}")
    for d in depths:
        rs = [r for r in rows if r["depth_target"] == d]
        t = sum(1 for r in rs if r.get("outcome") == "truncated")
        print(f"{d:>8} {t:>10} {len(rs):>8} {t/len(rs) if rs else 0:>11.3f}")
    out["run_losses"] = {
        "truncated": len(tr), "transport_error": len(te), "total_samples": len(rows),
        "truncated_by_depth": {str(d): sum(1 for r in tr if r["depth_target"] == d)
                               for d in depths},
        "truncated_by_task": {t: sum(1 for r in tr if r["task"] == t) for t in tasks},
    }

    # ---- timing (concurrency 1) --------------------------------------------
    print(f"\nTIMING -- read with care. Concurrency was {conc} (one worker per")
    print("depth) for most of the run, but workers finished at different times, so")
    print("the 0k worker ran largely alone at the end. These are NOT comparable")
    print("per-request throughputs and say nothing about depth.")
    print(f"{'depth':>8} {'median decode tok/s':>21} {'median wall s':>15}")
    for d in depths:
        c = out["cache"].get(str(d))
        if c:
            print(f"{d:>8} {c['median_predicted_per_second']:>21.1f} "
                  f"{c['median_wall_s']:>15.1f}")

    # ---- output length: a real secondary effect ----------------------------
    print("\nOUTPUT LENGTH (reasoning + content tokens generated, all samples)")
    print(f"{'depth':>8} {'median':>8} {'mean':>8} {'p90':>8} {'max':>8}")
    out["output_length"] = {}
    for d in depths:
        v = sorted(r["predicted_n"] for r in rows
                   if r["depth_target"] == d and r.get("predicted_n"))
        if not v:
            continue
        p90 = v[min(len(v) - 1, int(0.9 * len(v)))]
        row = {"median": statistics.median(v), "mean": statistics.mean(v),
               "p90": p90, "max": max(v), "n": len(v)}
        out["output_length"][str(d)] = row
        print(f"{d:>8} {row['median']:>8.0f} {row['mean']:>8.0f} "
              f"{p90:>8} {max(v):>8}")

    # ---- significance ------------------------------------------------------
    def fisher(a, b, c, d):
        """two-sided Fisher exact p for [[a,b],[c,d]]"""
        n = a + b + c + d
        r1, c1 = a + b, a + c
        def pr(x):
            return (math.comb(r1, x) * math.comb(n - r1, c1 - x)) / math.comb(n, c1)
        p0 = pr(a)
        lo = max(0, c1 - (n - r1))
        hi = min(r1, c1)
        return min(1.0, sum(pr(x) for x in range(lo, hi + 1) if pr(x) <= p0 + 1e-12))

    print("\nSIGNIFICANCE (Fisher exact, two-sided, on tests_pass)")
    out["significance"] = {}
    base = out["by_depth"][str(depths[0])]
    pairs = [(depths[0], d) for d in depths[1:]] + [(20000, 150000)]
    for x, y in pairs:
        if str(x) not in out["by_depth"] or str(y) not in out["by_depth"]:
            continue
        X, Y = out["by_depth"][str(x)], out["by_depth"][str(y)]
        p = fisher(X["tests_pass"], X["n_scored"] - X["tests_pass"],
                   Y["tests_pass"], Y["n_scored"] - Y["tests_pass"])
        out["significance"][f"{x}_vs_{y}"] = p
        print(f"  {x:>6} ({X['tests_pass']}/{X['n_scored']}) vs "
              f"{y:>6} ({Y['tests_pass']}/{Y['n_scored']}):  p = {p:.3f}")
    # everything with a preamble, pooled, against the 0k control
    deep = [r for r in rows if r["depth_target"] > 0 and r.get("outcome") == "scored"]
    dp = sum(1 for r in deep if r["tests_pass"])
    p = fisher(base["tests_pass"], base["n_scored"] - base["tests_pass"],
               dp, len(deep) - dp)
    out["significance"]["0_vs_all_depths_pooled"] = p
    lo, hi = wilson(dp, len(deep))
    print(f"  {0:>6} ({base['tests_pass']}/{base['n_scored']}) vs "
          f"pooled >0 ({dp}/{len(deep)}, [{lo:.2f},{hi:.2f}]):  p = {p:.3f}")

    (HERE / "results.json").write_text(json.dumps(out, indent=2))
    print(f"\nwrote {HERE/'results.json'}")


if __name__ == "__main__":
    main()
