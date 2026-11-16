#!/bin/sh
# Clippy for a gate, on sources made stale on purpose.
#
# cargo's fingerprint ignores the arguments after `--`, so a unit already built by plain clippy
# or check is fresh for `-D warnings` and nothing is linted. Touching each member's root makes
# its units stale. Dependencies are left alone: cargo caps their lints.
#
#     sh scripts/clippy-gate.sh <clippy arguments...>
set -eu

members=$(cargo metadata --no-deps --format-version 1 |
  grep -o '"manifest_path": *"[^"]*"' | cut -d'"' -f4)

[ -n "$members" ] || {
  echo "clippy-gate: cargo metadata named no workspace member" >&2
  exit 1
}

want=0
bare=""
for m in $members; do
  want=$((want + 1))
  crate=${m%/*}
  if [ -f "$crate/src/lib.rs" ]; then
    touch "$crate/src/lib.rs"
  elif [ -f "$crate/src/main.rs" ]; then
    touch "$crate/src/main.rs"
  else
    bare="$bare $crate"
  fi
done

# A member whose root this script does not know is a member whose units may stay
# fresh, so a partial touch is a silent hole rather than a smaller check.
if [ -n "$bare" ]; then
  echo "clippy-gate: these workspace members have neither src/lib.rs nor src/main.rs," >&2
  echo "  so their units cannot be made stale:$bare" >&2
  exit 1
fi

echo "clippy-gate: made $want workspace crate root(s) stale, so clippy cannot reuse a fresh fingerprint"
exec cargo clippy "$@"
