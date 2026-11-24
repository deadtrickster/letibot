#!/bin/sh
# quick-tests.sh [--print]: the quick gate's test step. It reads the paths this run changed out of
# $LETIBOT_GATE_CHANGED (one per line, written by the gate), maps them to the crates they belong to,
# and runs nextest over those crates AND everything that depends on them - so a change to a core
# crate still runs its callers' tests, while a change to one leaf crate does not pay for the suite.
#
#     sh scripts/quick-tests.sh            # run it
#     sh scripts/quick-tests.sh --print    # say what would run, and run nothing
set -eu
print=no
[ "${1:-}" = --print ] && print=yes
changed=${LETIBOT_GATE_CHANGED:-}
if [ -z "$changed" ] || [ ! -f "$changed" ]; then
  # Not "nothing changed": a run that cannot read the list says so rather than gating nothing.
  echo "quick-tests: \$LETIBOT_GATE_CHANGED names no file, so what this run changed is unknown" >&2
  exit 2
fi
# A crate directory's package name, read from its own manifest: the queue's rows name the
# `crates/<dir>` a path is in, and nextest wants the package name (`letibot-tools`).
pkg_of() {
  awk -F'"' '/^\[package\]/ { p = 1; next } /^\[/ { p = 0 } p && $1 ~ /^name *= *$/ { print $2; exit }' \
    "$1/Cargo.toml" 2>/dev/null
}
# What a path can break. Everything is a crate unless it is one of the few places whose reach is
# known: `scripts/` and the root `tests/` are the shell suites, which the tools crate carries; the
# built-in guests are compiled by the harnessd build script; and a manifest or the nextest profile
# reaches every crate, so a change to one runs everything.
pkgs=""
all=no
nothing=yes
while IFS= read -r p; do
  [ -n "$p" ] || continue
  nothing=no
  case $p in
    crates/*/*)
      dir=${p#crates/}
      dir=crates/${dir%%/*}
      name=$(pkg_of "$dir")
      if [ -n "$name" ]; then
        pkgs="$pkgs $name"
      else
        all=yes
      fi
      ;;
    crates/*) all=yes ;;
    scripts/* | tests/*)
      name=$(pkg_of crates/tools)
      [ -n "$name" ] && pkgs="$pkgs $name" || all=yes
      ;;
    plugins/builtin/*)
      name=$(pkg_of crates/harnessd)
      [ -n "$name" ] && pkgs="$pkgs $name" || all=yes
      ;;
    Cargo.toml | Cargo.lock | .config/* | build.rs) all=yes ;;
    # A change that cannot compile anything: prose, the workflows, the licence.
    docs/* | .github/* | LICENSE | *.md) ;;
    *) all=yes ;;
  esac
done <"$changed"
# One package once, in the order it was first named, so the expression a person reads is stable.
# shellcheck disable=SC2086 # a word-split list by design: these are package names, one per word
pkgs=$(printf '%s\n' $pkgs | awk '!seen[$0]++' | tr '\n' ' ')
if [ -n "$pkgs" ] && [ "$all" = yes ]; then
  all=yes
fi
if [ "$all" = yes ]; then
  expr=all\(\)
elif [ -n "${pkgs% }" ]; then
  expr=""
  for n in $pkgs; do
    expr="${expr:+$expr + }rdeps($n)"
  done
elif [ "$nothing" = yes ]; then
  echo "quick-tests: the changed-path list is empty, so no test can be aimed at it" >&2
  exit 2
else
  # Every path was prose: there is nothing in this change for a Rust test to see.
  echo "quick-tests: nothing in this change is Rust (docs, workflows, prose); nothing to run"
  exit 0
fi
if [ "$print" = yes ]; then
  printf '%s\n' "$expr"
  exit 0
fi
# A box without nextest cannot gate: say so by name rather than letting cargo answer
# `no such command`, which reads like the branch's own fault.
if ! command -v cargo-nextest >/dev/null 2>&1; then
  echo "quick-tests: this step runs 'cargo nextest', and cargo-nextest is not on PATH" >&2
  echo "quick-tests: install it with: cargo install cargo-nextest --locked" >&2
  exit 2
fi
echo "quick-tests: $expr"
exec cargo nextest run --profile gate -E "$expr"
