#!/bin/sh
# gate-at.sh WORKTREE TARGET_DIR BASE: the merge gate BASE's AGENTS.md declares, run on WORKTREE.
# Every step but the install (that one runs after the push), each in gate-env.sh's env, judged by
# its exit status; `check-fmt.sh main` compares against BASE. docs/gate.md has the rest.
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
# Read from BASE, as the queue reads it from the target: a stack cannot change its own gate.
steps=$(git show "$B:AGENTS.md" | sh scripts/gate-steps.sh) || {
  echo "gate: $B:AGENTS.md declares no merge gate" >&2
  exit 2
}
# A test step that runs nextest on a box without it fails as `cargo: no such command`, which reads
# like the branch's fault. Name the install instead, before the lock is taken.
if printf '%s\n' "$steps" | grep -q 'cargo nextest' && ! command -v cargo-nextest >/dev/null 2>&1; then
  echo "gate: a step runs 'cargo nextest', and cargo-nextest is not on PATH" >&2
  echo "gate: install it with: cargo install cargo-nextest --locked" >&2
  exit 2
fi
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
# Steps run with fd 9 closed, so a process a test leaks cannot hold the lock after the gate.
exec 9>"$LOCK"
echo "gate: waiting for $LOCK" >&2
flock 9
echo "gate: lock held" >&2
i=0
while IFS= read -r step <&3; do
  i=$((i + 1))
  case $step in
    *install-gate-build*)
      echo "gate: not run before the push: $step"
      continue
      ;;
    *"check-fmt.sh main")
      step="${step%main}$B"
      ;;
  esac
  log="$L/$N.step$i.log"
  refused="$L/$N.step$i.refusals"
  rm -f "$refused"
  echo "gate: step $i: $step"
  # A skip is not a pass: a test whose apparatus is missing fails instead of skipping.
  sh scripts/gate-env.sh "$G" XDG_RUNTIME_DIR="$R" CARGO_TARGET_DIR="$TD" LETIBOT_REQUIRE_APPARATUS=1 \
    LETIBOT_APPARATUS_REFUSALS="$refused" sh -c "$step" >"$log" 2>&1 </dev/null 9>&-
  rc=$?
  # What passed: libtest's line per test binary, or nextest's own one-line summary for the step.
  ok=$(grep -cE '^test result: ok\.' "$log")
  ran=$(sed -n 's/^ *Summary \[[^]]*\] \([0-9][0-9]*\) tests* run: \([0-9][0-9]*\) passed.*/\2/p' "$log")
  [ -z "$ran" ] || ok=$ran
  [ "$ok" = 0 ] || echo "gate: passed=$ok; skip lines: $(grep -c 'THIS IS NOT A PASS' "$log")"
  if [ "$rc" != 0 ]; then
    grep -E '^test result: FAILED|^ *FAIL \[|^error|FAILED$|panicked' "$log" | head -20
    tail -5 "$log"
    [ -s "$refused" ] && sed 's/^/REFUSED: /' "$refused"
    echo "gate: step $i exited $rc: $step (log: $log)"
    exit 3
  fi
done 3<<EOF
$steps
EOF
# Any store under the scratch is a test that wrote where the operator's store lives.
leaks=$(find "$G" "$R" \( -name 'sessions.db' -o -name 'shared.db' \) 2>/dev/null)
if [ -n "$leaks" ]; then
  printf '%s\n' "$leaks" | sed 's/^/LEAKED: /'
  exit 3
fi
echo GREEN
