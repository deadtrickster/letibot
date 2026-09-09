#!/bin/bash
# Paired k-sweep at three distances between the divergence and the tip.
# A is empty in every configuration: the whole context preceding the reused
# block C differs, which is the cross-session block-reuse case.
set -e
cd "$(dirname "$0")"
mkdir -p raw

# distance = |C|, the number of tokens between the divergence and the point
# where generation starts.
python3 stitch2.py --slot 4 --seeds 11 12 --a 0 --b 60 --c 2   --npred 200 \
    --ks 0,4,16,32,c,cb --out raw/near80.json

python3 stitch2.py --slot 4 --seeds 11 12 --a 0 --b 60 --c 12  --npred 200 \
    --ks 0,4,16,64,128,256,c,cb --out raw/mid460.json

python3 stitch2.py --slot 4 --seeds 11 12 --a 0 --b 60 --c 200 --npred 200 \
    --ks 0,16,64,256,1024,4096,c,cb --out raw/far7700.json
