#!/usr/bin/env bash
# **The runner's first act: a checkout that waited for the lock is never older than the artifacts.**
# A run queued behind another was cloned before that build finished, and cargo calls a source that
# is not NEWER than its artifact fresh, so the run built the other branch's code. Two real crates,
# one shared target dir, and each run must build its own.
#
# Nothing here reaches a host: the runner is run directly, on local paths only.
#
#   tests/rrun.sh            # from the repo root; exits non-zero on a failure
set -u
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
RUNNER="$ROOT/scripts/rrun-runner.py"
T="$(mktemp -d)"
# The runner processes this file starts, so a case that fails before its own wait does not leave one
# building in a temp dir for ever: the trap stops them by pid, and by nothing else.
runners=()
cleanup() {
  for p in ${runners[@]+"${runners[@]}"}; do kill "$p" 2> /dev/null || true; done
  rm -rf "$T"
}
trap cleanup EXIT

mkdir -p "$T/home"
fails=0
note() { echo "ok   $1"; }
fail() {
  echo "FAIL $1"
  [ $# -lt 2 ] || printf '%s\n' "$2" | sed 's/^/     /'
  fails=$((fails + 1))
}
json_steps() { python3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' "$@"; }

new_run() {
  mkdir -p "$1/tree"
  python3 - "$1/status.json" << 'PY'
import json, sys
json.dump({"name": "t", "host": "h", "branch": "b", "sha": "s", "state": "shipping",
           "started": 0, "updated": 0, "steps": []}, open(sys.argv[1], "w"))
PY
}
state_of() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["state"])' "$1/status.json" 2> /dev/null; }
# wait_state DIR STATE -- the run reaches that state within ten seconds, or this is false.
wait_state() {
  i=0
  while [ "$i" -lt 200 ]; do
    [ "$(state_of "$1")" = "$2" ] && return 0
    sleep 0.05
    i=$((i + 1))
  done
  return 1
}
# reaches DIR STATE NAME -- the run gets there, or the case says where it stopped instead.
reaches() {
  if wait_state "$1" "$2"; then note "$3"; else fail "$3" "$(cat "$1/runner.out" 2> /dev/null)"; fi
}
# A checkout the launcher's clone would produce: the files, and git TRACKING them, which is what the
# runner reads to know which mtimes are its own to move.
a_checkout() {
  mkdir -p "$1/tree"
  cp -R "$2/." "$1/tree/"
  git -C "$1/tree" init -q
  git -C "$1/tree" config user.name tester
  git -C "$1/tree" config user.email tester@example.invalid
  git -C "$1/tree" config commit.gpgsign false
  git -C "$1/tree" add .
  git -C "$1/tree" commit -qm one
}
# ---------------------------------------------------------------------------------------
# **A run that waited on the lock has an OLD checkout**, cloned before the build it queued behind,
# and cargo calls a source that is not NEWER than its artifact fresh: the run then built against
# the other branch's code. Two real crates, one shared target dir, each run its own binary.
if command -v cargo > /dev/null 2>&1 && command -v rustc > /dev/null 2>&1; then
  cargo_bin="$(command -v cargo)"
  mkdir -p "$T/crate-a/src" "$T/crate-b/src"
  for t in a b; do
    printf '[package]\nname = "which"\nversion = "0.1.0"\nedition = "2021"\n' > "$T/crate-$t/Cargo.toml"
    printf 'fn main() { println!("%s"); }\n' "$t" > "$T/crate-$t/src/main.rs"
  done
  # Both checkouts are made before either builds, which is how a queued run gets an old tree.
  new_run "$T/r-old-a"
  a_checkout "$T/r-old-a" "$T/crate-a"
  new_run "$T/r-old-b"
  a_checkout "$T/r-old-b" "$T/crate-b"
  touch -t 200101010101 "$T/r-old-b/tree/Cargo.toml" "$T/r-old-b/tree/src/main.rs"
  # The toolchain as absolute paths: the step runs under `bash -lc`, whose profile has opinions
  # about PATH, and the case is about mtimes rather than about which shim was found.
  cargo_run() {
    env -i HOME="$T/home" USER=tester PATH="$(dirname "$cargo_bin"):/usr/bin:/bin" \
      CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
      RUSTC="$(command -v rustc)" CARGO_TARGET_DIR="$T/shared-target" \
      python3 "$RUNNER" "$1" "$(json_steps "$cargo_bin build --offline -q && \"\$CARGO_TARGET_DIR/debug/which\" > $T/built-$2")" \
      > "$1/runner.out" 2>&1
  }
  cargo_run "$T/r-old-a" a
  reaches "$T/r-old-a" passed "a run builds its own code into a shared target dir"
  if [ "$(cat "$T/built-a" 2> /dev/null)" = a ]; then note "  ...and runs it"; else fail "  ...and runs it" "$(cat "$T/built-a" 2> /dev/null)"; fi
  cargo_run "$T/r-old-b" b
  reaches "$T/r-old-b" passed "a run whose checkout waited for that build builds its own code"
  if [ "$(cat "$T/built-b" 2> /dev/null)" = b ]; then
    note "  ...not the binary the first run left in the shared dir"
  else
    fail "  ...not the binary the first run left in the shared dir" "it ran $(cat "$T/built-b" 2> /dev/null)"
  fi
else
  echo "skip the mtime case: no cargo or rustc to build a crate with"
fi

if [ "$fails" -ne 0 ]; then
  echo "rrun: $fails failed"
  exit 1
fi
echo "rrun: all cases pass"
