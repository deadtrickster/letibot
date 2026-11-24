#!/bin/sh
# gate-at.sh WORKTREE TARGET_DIR BASE [--quick]: the gate BASE's AGENTS.md declares, run on
# WORKTREE. `--quick` (or `QUICK=1` in the environment) runs the quick tier instead of the whole
# gate - what a branch must pass to land, where the project declares two tiers. Every step but the
# install (that one runs after the push), each in gate-env.sh's env, judged by its exit status;
# `check-fmt.sh main` compares against BASE. docs/gate.md has the rest.
set -u
quick=${QUICK:-0}
case ${4:-} in
  '' | --quick) ;;
  *)
    echo "usage: gate-at.sh WORKTREE TARGET_DIR BASE [--quick]" >&2
    exit 2
    ;;
esac
[ "${4:-}" = --quick ] && quick=1
[ $# -eq 3 ] || [ $# -eq 4 ] || {
  echo "usage: gate-at.sh WORKTREE TARGET_DIR BASE [--quick]" >&2
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
# The record a hand gate leaves while it runs - the same `key=value` shape a queue gate's run
# directory holds, so a reader on this box lists the hand gate beside the queue's: who (`hand`),
# which tip, which step. It sits under the box's gate lock, where hand and queue gates already
# agree to take turns.
RD=${LETIBOT_GATE_RUN_DIR:-$(dirname "$LOCK")/gate-runs}/$N
mkdir -p "$RD" || exit 2
# Set here, and replaced below once the scratch dir exists: a refusal between the two must not leave a
# record saying a hand gate is running.
trap 'rm -rf "$RD"' EXIT
SHA=$(git rev-parse HEAD 2>/dev/null) || SHA=
START=$(($(date +%s) * 1000))
# $1 is the step, $2 its text: rewritten as each step starts, so a pane's `step N` is true.
note() {
  {
    echo 'daemon=hand'
    echo "run_id=hand-$SHA"
    echo "repository=$N"
    echo "target=$B"
    echo "sha=$SHA"
    echo "step=$1"
    echo 'of_step='
    echo "command=$2"
    echo "started_ms=$START"
    echo "beat_ms=$(($(date +%s) * 1000))"
  } >"$RD/owner"
}
note 0 ''
# Read from BASE, as the queue reads it from the target: a stack cannot change its own gate. The
# quick tier is the branch's own bar; the whole gate is what main's health is judged by.
tier=--full
[ "$quick" = 1 ] && tier=--quick
steps=$(git show "$B:AGENTS.md" | sh scripts/gate-steps.sh "$tier") || {
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
trap 'rm -rf "$R" "$RD"' EXIT
# What every step is told it is compared against, the same two facts the queue hands its steps:
# the sha BASE names, and the paths this worktree changes since it - so a step can aim itself.
BASE_SHA=$(git rev-parse "$B" 2>/dev/null) || BASE_SHA=
CHANGED="$R/changed.txt"
git diff --name-only "$BASE_SHA" "$SHA" >"$CHANGED" 2>/dev/null || : >"$CHANGED"
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
  note "$i" "$step"
  # A skip is not a pass: a test whose apparatus is missing fails instead of skipping.
  sh scripts/gate-env.sh "$G" XDG_RUNTIME_DIR="$R" CARGO_TARGET_DIR="$TD" LETIBOT_REQUIRE_APPARATUS=1 \
    LETIBOT_APPARATUS_REFUSALS="$refused" LETIBOT_GATE_BASE="$BASE_SHA" \
    LETIBOT_GATE_CHANGED="$CHANGED" sh -c "$step" >"$log" 2>&1 </dev/null 9>&-
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
