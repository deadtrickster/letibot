#!/bin/bash
# wire-skew.sh [OLD_REF...]: two builds on one wire. This build's daemon captures the corpus for
# each older build's protocol, and that build's own decoder must read it with the same meaning.
# Default refs: the last commit before the current PROTOCOL_VERSION, and the newest release tag.
set -euo pipefail

ROOT=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
PROTO=crates/sessionlog/src/protocol.rs
TD=${CARGO_TARGET_DIR:-$ROOT/target}/wire-skew
WORK=$(mktemp -d "${TMPDIR:-/tmp}/wire-skew.XXXX")
trees=()
cleanup() {
  for t in "${trees[@]}"; do
    git -C "$ROOT" worktree remove --force "$t" >/dev/null 2>&1 || true
  done
  rm -rf "$WORK"
}
trap cleanup EXIT

version_at() {
  git -C "$ROOT" show "$1:$PROTO" | sed -n 's/^pub const PROTOCOL_VERSION: u32 = \([0-9]*\);/\1/p'
}

# A crate outside the workspace, so only the log's normal dependencies build, never its dev ones.
bin_crate() {
  local tree=$1 name=$2 main=$3
  local dir=$WORK/crate-$name
  mkdir -p "$dir"
  cat >"$dir/Cargo.toml" <<EOF
[package]
name = "wire-skew-$name"
version = "0.0.0"
edition = "2024"
publish = false

[[bin]]
name = "run"
path = "$main"

[dependencies]
letibot-sessionlog = { path = "$tree/crates/sessionlog", default-features = false }
serde_json = "1"

[workspace]
EOF
  cp "$tree/Cargo.lock" "$dir/Cargo.lock"
  echo "$dir/Cargo.toml"
}

run_bin() {
  local manifest=$1 name=$2
  shift 2
  cargo run -q --manifest-path "$manifest" --target-dir "$TD/$name" -- "$@"
}

if [ $# -eq 0 ]; then
  bump=$(git -C "$ROOT" log -1 --format=%H -G'^pub const PROTOCOL_VERSION: u32 = ' -- "$PROTO")
  tag=$(git -C "$ROOT" describe --tags --abbrev=0 --match 'v*' HEAD)
  set -- "$bump^" "$tag"
fi

new=$(version_at HEAD)
capture=$(bin_crate "$ROOT" new-capture "$ROOT/crates/sessionlog/examples/wire_capture.rs")
newdec=$(bin_crate "$ROOT" new-decode "$ROOT/crates/sessionlog/examples/wire_decode.rs")

failed=0
echo "== this build ($new) reads every kept corpus"
if ! run_bin "$newdec" new-decode "$ROOT"/crates/sessionlog/tests/wire_golden/v*/*.jsonl; then
  failed=$((failed + 1))
fi
for ref in "$@"; do
  old=$(version_at "$ref")
  sha=$(git -C "$ROOT" rev-parse --short "$ref^{commit}")
  tree=$WORK/tree-$sha
  git -C "$ROOT" worktree add --detach "$tree" "$sha" >/dev/null 2>&1
  trees+=("$tree")
  out=$WORK/corpus-$sha
  run_bin "$capture" new-capture "$out" "$old"
  dec=$(bin_crate "$tree" "old-$sha" "$ROOT/crates/sessionlog/examples/wire_decode.rs")
  echo "== $ref ($sha, protocol $old) reads this build's ($new) frames"
  if ! run_bin "$dec" "old-$sha" "$out/server.jsonl" "$out/client.jsonl"; then
    failed=$((failed + 1))
  fi
done
[ "$failed" -eq 0 ] || {
  echo "wire-skew: $failed build(s) misread another build's frames" >&2
  exit 1
}
echo "wire-skew: every older build read this build's frames"
