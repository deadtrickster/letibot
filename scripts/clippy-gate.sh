#!/bin/sh
# Clippy for a gate, on sources made stale on purpose.
#
# cargo's fingerprint for a unit does not include the arguments after `--`, so a
# unit built by a plain `cargo clippy` or a `cargo check` in this target dir is
# still fresh for `-- -D warnings`, and the step can exit green having linted
# nothing. Touching every workspace member's own root first makes each member's
# units stale, so the lints run every time. Dependencies are left alone: their
# lints are capped by cargo and their fingerprints are not the hole.
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
