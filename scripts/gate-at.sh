#!/bin/sh
# gate-at.sh WORKTREE TARGET_DIR BASE: check-fmt against BASE, then the full workspace test run.
# The local landing gate the lead runs on a stack of branches; docs/gate.md has the rest.
set -u
[ $# -eq 3 ] || {
  echo "usage: gate-at.sh WORKTREE TARGET_DIR BASE" >&2
  exit 2
}
W=$(cd "$1" && pwd) || exit 2
TD=$2
B=$3
LOCK=${LETIBOT_GATE_LOCK:-/build/gate.lock}
L=${LETIBOT_GATE_LOGS:-${TMPDIR:-/tmp}/letibot-gate}
N=$(basename "$W")
mkdir -p "$L" || exit 2
cd "$W" || exit 2
sh scripts/check-fmt.sh "$B" >"$L/$N.fmt.log" 2>&1 || {
  cat "$L/$N.fmt.log"
  echo "FMT FAILED"
  exit 4
}
tail -1 "$L/$N.fmt.log"
# The built-in guests' build script watches absolute paths; in a target shared across worktrees
# it would keep another tree's wasm. Drop any whose watch list is not this tree.
for o in "$TD"/debug/build/letibot-builtins-*/output; do
  if [ -f "$o" ] && ! grep -q "rerun-if-changed=$W/" "$o"; then
    rm -rf "$(dirname "$o")"
  fi
done
# A test that forgets to isolate its data dir must not reach the operator's store, and a head or
# bridge a test starts must not open the operator's sockets: scripts/gate-env.sh builds the env.
G="$L/$N.xdg"
rm -rf "$G"
# Socket paths have a 108-byte limit, so the runtime dir is short.
R=$(mktemp -d "${TMPDIR:-/tmp}/g.XXXX") || exit 2
chmod 700 "$R"
trap 'rm -rf "$R"' EXIT
# One full workspace run at a time on this box: parallel gates choke it. Later callers wait here.
exec 9>"$LOCK"
echo "gate: waiting for $LOCK" >&2
flock 9
echo "gate: lock held" >&2
sh scripts/gate-env.sh "$G" XDG_RUNTIME_DIR="$R" CARGO_TARGET_DIR="$TD" \
  cargo test --workspace --no-fail-fast >"$L/$N.test.log" 2>&1
find "$G/home" "$R" \( -name 'sessions.db' -o -name 'shared.db' \) 2>/dev/null | sed 's/^/ISOLATED: /' | head -3
find "$G" -name 'sessions.db' | sed 's/^/LEAKED: /'
ok=$(grep -cE '^test result: ok\.' "$L/$N.test.log" || true)
bad=$(grep -cE '^test result: FAILED|^error(\[|:)' "$L/$N.test.log" || true)
echo "suites ok=$ok bad=$bad"
if [ "$bad" != 0 ] || [ "$ok" -lt 140 ]; then
  grep -E '^test result: FAILED|^error|FAILED$|panicked' "$L/$N.test.log" | head -20
  exit 3
fi
echo GREEN
