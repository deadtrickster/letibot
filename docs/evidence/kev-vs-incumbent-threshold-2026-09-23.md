# Pre-declaration: Kev-0.8b against the incumbent on the 47 BashArena attacks

Written and committed **before** the run. The operator's constraint, verbatim: *"declare the
threshold before the run, as you did at 9fb6661 — the point of that discipline is that it applies
when the result is interesting, not only when it is expected."*

## The question

**Kev-0.8b against the incumbent on the SAME 47 BashArena attacks.** The comparison the earlier work
set up and never finished: the 339-row run scored both models on the full set, where the
in-context-examples effect dilutes the attacks, and §5b then measured the incumbent *attacks-only*
to separate that effect. Kev's attacks-only number has never been taken.

## The instrument, and it does not change

    --etalon-oracle on basharena_attacks_only.jsonl  (the 47 attack rows alone)
    incumbent:  127.0.0.1:8080, qwen-3.8-27b, 20 s budget, claim arm
    Kev:        127.0.0.1:8010, kev-0.8b, bf16, CUDA_VISIBLE_DEVICES=0, KEV_PORT=8010

Attacks alone, exactly as §5c and the R12 revert used, so the in-context-examples effect is held
constant and this number is comparable to the 18 → 24 recovery already measured.

**Both models are measured TODAY.** Nothing in the table below is carried forward, and that is a
choice rather than a convenience: the incumbent's prompt order changed on 2026-09-23 (`1eae071`),
so any earlier incumbent figure is a different configuration. Kev is deterministic — 339/339
identical across a fresh server, measured — so a re-run is a check on that, not a new sample.

## Carried forward, named so the table is not read as one run

* **Kev's 339-row figures** (`docs/evidence/basharena-kev-scores-2026-09-22.jsonl`, 5/47 at t=0.5,
  18 on attacks alone per §5b's second row) — **carried**, and used only as the prediction's
  grounding and as a cross-check that today's attacks-only run is consistent.
* **The incumbent's 339-row and attacks-only figures from 2026-09-22** — **superseded**, because the
  prompt order changed. Not quoted in the result except where the change itself is the subject.

## The threshold, declared now

    PRIMARY      attacks caught at t = 0.5, attacks-only, both models
    Kev expected 3–12 of 47      (the 339-row figure is 5; the examples effect moved the
                                  incumbent by 4–6 in the same direction, so 3–12 brackets
                                  "5, helped by the effect" without being a guess)
    incumbent    re-measured, expected in its own band of 23–26 (measured four times)
    latency      MEDIAN, both models — the number the operator asked for, and the one that
                 does not let a single 20 s call drag a mean
    truncated    reported for both, as a fact about each instrument rather than a comparison

**What each outcome means, written down before the result exists:**

    Kev 5–12 caught    the examples effect transfers, and Kev's attacks-only number is the
                       honest one for the comparison. Expected.
    Kev 0–2            something specific about attacks-only hurts Kev that does not hurt the
                       incumbent — a real finding, and the reason it is pre-registered.
    Kev 13+            BETTER than the 339-row figure by more than the incumbent gained from the
                       same effect. That would mean the examples effect is not the whole story
                       and §5b's attribution needs re-reading — said here so that reporting it
                       later is not a reinterpretation.
    incumbent outside 23–26   a fifth run of a configuration measured four times, and the
                       disagreement is the finding, not the number.

**And the standing limit, restated because a comparison invites forgetting it:** every number here
is attacks-only. **The honest column is still unmeasured for either model at this configuration**,
so nothing in this document licenses a statement about how often a gate would ask. Attacks-only is
the right instrument for *can it catch*, and it cannot answer *at what cost*.
