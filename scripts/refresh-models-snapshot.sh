#!/bin/sh
# Refresh the vendored model snapshot from models.dev, and say what to change.
#
# The snapshot is the FLOOR of the catalogue chain (crates/provider/src/catalogue.rs):
#   LETIBOT_MODELS_JSON -> ~/.cache/letibot/models.json -> opencode's -> the snapshot.
# It carries every provider a preset can switch to, and nothing else — it is not a
# browsing catalogue, it is the answer to "a box with nothing else still plans against
# the right window".
#
# After running this, set SNAPSHOT_FETCHED in crates/provider/src/catalogue.rs to the
# date the script prints. The two are one fact in two places, and this script is what
# keeps them honest with each other.
#
# With --cache, the same trimmed file is ALSO written to ~/.cache/letibot/models.json,
# which is the first place the loader looks after $LETIBOT_MODELS_JSON — so the box's
# own cache is refreshed rather than only the committed floor.
set -eu
dir=$(cd "$(dirname "$0")/.." && pwd)
out="$dir/crates/provider/data/models-snapshot.json"

curl -s --max-time 60 https://models.dev/api.json -o /tmp/models.dev.json

python3 - "$out" "${1:-}" <<'PY'
import json, sys, os, datetime
from pathlib import Path

out, cache_arg = sys.argv[1], sys.argv[2]
d = json.load(open("/tmp/models.dev.json"))

# Every provider a crate::presets Preset names as its catalogue_id. Adding a preset
# means adding its id here — the script fails loudly below if it forgets.
WANT = ["deepseek", "zhipuai", "zai-coding-plan", "zhipuai-coding-plan", "xai"]

trimmed = {}
for pid in WANT:
    p = d.get(pid)
    if not p or not p.get("models"):
        sys.exit(f"models.dev has no models for {pid!r} — is the id still right?")
    ms = {}
    for name, m in p["models"].items():
        e = {}
        lim = m.get("limit") or {}
        if lim.get("context") or lim.get("output"):
            e["limit"] = {"context": lim.get("context", 0), "output": lim.get("output", 0)}
        cost = m.get("cost")
        if cost:
            e["cost"] = {k: cost[k] for k in ("input", "output", "cache_read") if k in cost}
        if e:
            ms[name] = e
    trimmed[pid] = {"models": ms}

text = json.dumps(trimmed, indent=1, sort_keys=True) + "\n"
Path(out).write_text(text)

today = datetime.date.today().isoformat()
if cache_arg == "--cache":
    base = os.environ.get("XDG_CACHE_HOME") or os.path.join(
        os.environ.get("HOME", ""), ".cache")
    target = Path(base) / "letibot"
    target.mkdir(parents=True, exist_ok=True)
    (target / "models.json").write_text(text)
    print(f"also wrote {target / 'models.json'}")
print(f"snapshot: {len(trimmed)} providers, {len(text)} bytes")
print(f"set SNAPSHOT_FETCHED = \"{today}\" in crates/provider/src/catalogue.rs")
PY
