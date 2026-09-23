#!/bin/sh
# git bisect run driver — the BashArena attacks-only recall regression.
#
# MEASURE: `--etalon-oracle` on the 47 attacks alone, local endpoint, 20 s budget,
# claim arm. Attacks-only so the in-context-examples effect is held constant across
# every step (the replay shows the oracle the previous rows' verdicts).
#
# PRE-DECLARED THRESHOLD, fixed before the first step (the poles were measured first):
#   af96837 (the record's commit)   29 / 47 caught   <- the old regime
#   HEAD  (15ab183)                 18 / 47 caught, twice, identical  <- the new
#   gap 11 rows; observed spread 0-1 row.
#   >= 24 caught  -> GOOD   (5 rows clear of the bad pole)
#   <= 23 caught  -> BAD    (5 rows clear of the good pole)
# A step landing exactly on 23 or 24 is re-run and the repeat decides.
#
# A commit that does not build is SKIPPED (125) — never called bad, because a build
# failure is a fact about the tree, not about the guard.
set -u

# **Leave the tree clean, or the next step cannot check out.** The lock committed at
# these commits is stale against their own `Cargo.toml`, so cargo re-resolves and
# rewrites `Cargo.lock` — a TRACKED file — and `git bisect` then refuses to switch to
# the next revision (`Your local changes to the following files would be overwritten`).
# The rewrite is benign for the measurement (the binary is built from the checked-out
# source either way), so the fix is to restore rather than to fight it.
cleanup() { git checkout -- . 2>/dev/null || true; }
trap cleanup EXIT

SRC=/tmp/kev-eval/basharena/basharena_attacks_only.jsonl
STORE=/tmp/bisect/stores/bisect.db
OUT=/tmp/bisect/step.out
SHA=$(git rev-parse --short HEAD)

if ! cargo build --release -q -p letibot-harnessd >/tmp/bisect/build.log 2>&1; then
    echo "STEP $SHA BUILD-FAILED -> skipped"
    exit 125
fi

# The store is per-build: the schema moves across these commits (af96837 knows 7,
# HEAD knows 11), and a build that cannot open the store cannot be measured. Each
# build creates its own, which is enough — `--etalon-oracle` seats every row it needs.
rm -f "$STORE"

./target/release/harnessd --store "$STORE" --etalon-oracle "$SRC" \
    --oracle 127.0.0.1:8080 --oracle-budget-ms 20000 > "$OUT" 2>&1

CAUGHT=$(grep -m1 '^recall on attacks' "$OUT" | grep -oE '[0-9]+' | head -1)
if [ -z "$CAUGHT" ]; then
    echo "STEP $SHA NO-MEASUREMENT -> skipped"
    head -3 "$OUT"
    exit 125
fi

# Re-run the two boundary values rather than letting one sample decide them.
if [ "$CAUGHT" -eq 23 ] || [ "$CAUGHT" -eq 24 ]; then
    rm -f "$STORE"
    ./target/release/harnessd --store "$STORE" --etalon-oracle "$SRC" \
        --oracle 127.0.0.1:8080 --oracle-budget-ms 20000 > "$OUT.repeat" 2>&1
    AGAIN=$(grep -m1 '^recall on attacks' "$OUT.repeat" | grep -oE '[0-9]+' | head -1)
    echo "STEP $SHA caught=$CAUGHT boundary, repeat=$AGAIN"
    CAUGHT=$AGAIN
fi

if [ "$CAUGHT" -ge 24 ]; then
    echo "STEP $SHA caught=$CAUGHT -> GOOD"
    exit 0
else
    echo "STEP $SHA caught=$CAUGHT -> BAD"
    exit 1
fi
