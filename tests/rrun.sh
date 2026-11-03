#!/usr/bin/env bash
# shellcheck disable=SC2016 # a literal a remote command line carries, never a local expansion
# **rrun, from its argv to the build, with no host in between.** The launcher's flags become one
# remote command line a stub ssh records, and the runner builds what it was handed; the case behind
# both is a run that waited on the lock and must still build its own code rather than another's.
#
# `ssh` on the PATH is a file that records what it was sent, so nothing here reaches a host.
#
#   tests/rrun.sh            # from the repo root; exits non-zero on a failure
set -u
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
RRUN="$ROOT/scripts/rrun"
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

mkdir -p "$T/bin" "$T/home/.config/letibot" "$T/files"
# **The stub ssh.** A `cat > path` is a payload, so its bytes are kept under their own name and a
# case can read back what rrun shipped; a `cat .../status.json` is the watcher's read, and the run
# it asks about has ended. Everything else only has to exit 0.
cat > "$T/bin/ssh" << 'STUB'
#!/usr/bin/env python3
import os
import re
import shutil
import sys

cmd = sys.argv[-1] if len(sys.argv) > 1 else ""
with open(os.environ["RRUN_SSH_LOG"], "a") as log:
    log.write(cmd + "\n")
payload = re.match(r"cat > (\S+)\s*$", cmd)
if payload:
    dest = os.path.join(os.environ["RRUN_SSH_FILES"], os.path.basename(payload.group(1)))
    with open(dest, "wb") as f:
        shutil.copyfileobj(sys.stdin.buffer, f)
elif re.match(r"cat \S+/status\.json\s*$", cmd):
    sys.stdout.write('{"state": "passed"}')
sys.exit(0)
STUB
# The launcher's watcher sleeps 15 s between reads of the run's status; every other duration
# belongs to a step or to this file and gets the real thing.
cat > "$T/bin/sleep" << 'STUB'
#!/usr/bin/env bash
[ "${1:-}" = "15" ] && exit 0
exec /bin/sleep "$@"
STUB
chmod +x "$T/bin/ssh" "$T/bin/sleep"
# The address a host's name resolves to comes from hosts.toml, which awk reads before any ssh.
printf '[host.lab2x1]\nhostname = "192.0.2.10"\n' > "$T/home/.config/letibot/hosts.toml"
# A worktree rrun can bundle: HEAD and main, which is what the shipped bundle asks for.
git init -q -b main "$T/repo"
git -C "$T/repo" config user.name tester
git -C "$T/repo" config user.email tester@example.invalid
git -C "$T/repo" config commit.gpgsign false
echo one > "$T/repo/README"
git -C "$T/repo" add README
git -C "$T/repo" commit -qm first

fails=0
note() { echo "ok   $1"; }
fail() {
  echo "FAIL $1"
  [ $# -lt 2 ] || printf '%s\n' "$2" | sed 's/^/     /'
  fails=$((fails + 1))
}
json_steps() { python3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' "$@"; }
# expect NAME PATTERN -- the stub was sent a command line holding this text.
expect() {
  if grep -qF -- "$2" "$T/ssh.log" 2> /dev/null; then note "$1"; else fail "$1" "$(cat "$T/ssh.log" 2> /dev/null)"; fi
}
# refuse NAME PATTERN -- and none held it.
refuse() {
  if grep -qF -- "$2" "$T/ssh.log" 2> /dev/null; then fail "$1" "$(cat "$T/ssh.log" 2> /dev/null)"; else note "$1"; fi
}

# run_rrun ARGS... -- the launcher's combined output, with the stubs on the PATH and nothing of
# the operator's environment: the throwaway HOME holds the status file it writes.
run_rrun() {
  (cd "$ROOT" && env -i PATH="$T/bin:/usr/bin:/bin" HOME="$T/home" USER=tester \
    RRUN_SSH_LOG="$T/ssh.log" RRUN_SSH_FILES="$T/files" \
    "$(command -v bash)" "$RRUN" "$@" 2>&1 < /dev/null)
}

# ---------------------------------------------------------------------------------------
# The launcher: the target dir a run is handed.
: > "$T/ssh.log"
out="$(run_rrun one "$T/repo" -- true)"
st=$?
[ "$st" -eq 0 ] || fail "a run with no flags starts" "$out"
expect "the default target is still the host's one dir" 'CARGO_TARGET_DIR=$HOME/remote-runs/target'

# ---------------------------------------------------------------------------------------
# The runner: a checkout that waited for the lock is never older than the artifacts it shares.
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
