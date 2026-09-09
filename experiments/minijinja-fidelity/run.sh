#!/usr/bin/env bash
# Reproduce every number in RESULTS.md. No GPU, no model, no server, no port.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --quiet --bin probe --bin render --bin tests_probe

# The micro-probes run minijinja UNSHIMMED on purpose: they answer "what does
# minijinja do as shipped", which is what T1 asked. Differences here are the
# input to the shim in src/bin/render.rs, not defects in it. Expect exactly:
# stock tojson (rejects ensure_ascii on both engines, escapes differently),
# engine error prose, and `none is iterable`.
echo "### semantics micro-probes (minijinja | CPython) ###"
diff <(./target/debug/probe) <(python3 probe_cpython.py) && echo "  identical" || true
cargo build --quiet --bin none_probe
diff <(./target/debug/none_probe) <(python3 none_probe.py) && echo "  none/undefined: identical" || true

echo
echo "### is-test matrix (minijinja | CPython) ###"
./target/debug/tests_probe > /tmp/mj_tests.txt
python3 tests_probe.py > /tmp/py_tests.txt
paste /tmp/mj_tests.txt /tmp/py_tests.txt \
  | awk -F'\t' '$3!=$6 {printf "  %-10s is %-10s  minijinja=%-10s cpython=%s\n",$1,$2,$3,$6}' \
  || echo "  identical"

# corpora
python3 gen_cases.py --qwen --out cases-qwen.json
python3 gen_cases.py --edge --out cases-edge.json
python3 gen_cases.py --fuzz 800 --seed 1234 --out cases-fuzz.json
python3 gen_cases.py --fuzz 700 --seed 777  --out cases-fuzz2.json
python3 gen_cases.py --fuzz 700 --seed 99   --out cases-fuzz3.json
# cases-faithful.json is the fixture corpus, rendered by the hand-written GLM
# renderer purely to obtain the /apply-template-shaped request bodies and its
# RenderSpan list. Regenerate with:
#   CARGO_TARGET_DIR=.target-shared cargo run -q -p letibot-dialect-glm \
#     --bin letibot-render -- --dialect glm-5.3-flash --profile faithful \
#     ../../tests/fidelity/fixtures/*.json > cases-faithful.json

for f in cases-faithful.json cases-qwen.json cases-edge.json \
         cases-fuzz.json cases-fuzz2.json cases-fuzz3.json; do
  echo; echo "### $f ###"
  # cases-edge.json is EXPECTED to report DIVERGENCE -- it is the bigint case in
  # RESULTS.md §5, kept in the harness so the gap stays measured rather than
  # remembered. Every other corpus must say AGREEMENT.
  python3 compare.py --cases "$f" || true
done

echo; echo "### is the provenance map non-vacuous? ###"
python3 injection_check.py cases-faithful.json cases-qwen.json cases-fuzz.json

echo; echo "### cost ###"
cargo build --release --quiet --bin bench
python3 -c "
import json,sys; sys.path[:0]=['.','../../tests/fidelity']
import compare as C
json.dump(C.build_jobs(json.load(open('cases-faithful.json')))[0], open('bench-jobs.json','w'))"
./target/release/bench ../../crates/dialect-glm/template/glm-5.3-flash.jinja bench-jobs.json 300
./target/release/bench templates/qwen3.8-flash-next.jinja bench-jobs.json 300
