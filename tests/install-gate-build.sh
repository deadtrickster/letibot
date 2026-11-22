#!/usr/bin/env bash
# shellcheck disable=SC2015 # note and fail both answer 0, so `COND && note || fail` arms are exclusive
# **install-gate-build.sh against a real main tree, worktree and build directory**: the build it is
# told about goes in, a by-hand build in a worktree goes in, and the two ways the step can install
# nothing are refused by name.
#
#   tests/install-gate-build.sh      # from the repo root; exits non-zero on a failure
set -u
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

fails=0
note() { echo "ok   $1"; }
fail() {
  echo "FAIL $1"
  [ -n "${2:-}" ] && printf '%s\n' "$2" | sed 's/^/     /'
  fails=$((fails + 1))
}

# The fixture's own git and home: nothing here reads the operator's config or signs a commit.
export HOME="$T/home"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
mkdir -p "$HOME"
git() { command git -c user.name=tester -c user.email=t@example.com -c commit.gpgsign=false "$@"; }

# A compiled name, as the build leaves it: executable, and answering `--version` with the
# directory it was built in, so a case can say WHICH build was installed.
built_bin() { # built_bin DIR NAME
  printf '#!/bin/sh\necho "%s: built in %s"\n' "$2" "$1" >"$1/$2"
  chmod 755 "$1/$2"
}
built_all() { # built_all DIR
  mkdir -p "$1"
  for b in harnessd letibot-tui letibot-multi letibot-mcp letibot-acp; do built_bin "$1" "$b"; done
}
answers() { # answers FILE -- the build directory the installed binary names
  [ -x "$1" ] && "$1" --version 2>/dev/null | grep -qF "$2"
}

# ---------------------------------------------------------------------------------------
# The trees: a main checkout, a linked worktree cut from it (what the gate builds in), and the
# build directory `CARGO_TARGET_DIR` names.
# ---------------------------------------------------------------------------------------
main=$T/main
mkdir -p "$main/scripts"
cp "$ROOT/scripts/install-gate-build.sh" "$main/scripts/"
# The three lists the script reads. `letibot` and `leticode` are launchers, not build products.
cat >"$main/install.sh" <<'INSTALL'
BINARIES="harnessd letibot-tui letibot-multi letibot letibot-mcp letibot-acp"
LAUNCHERS="letibot leticode"
ROLE_LINKS="letibot-askpass"
INSTALL
git -C "$main" init -q -b main
git -C "$main" add -A
git -C "$main" commit -qm "the fixture"
gate=$T/gate
git -C "$main" worktree add -q "$gate" -b gate

build=$T/build/target-gnj
built_all "$build/release"

# ---------------------------------------------------------------------------------------
# The build the step is told about is the one that lands.
# ---------------------------------------------------------------------------------------
st=0
out="$(cd "$gate" && CARGO_TARGET_DIR="$build" sh scripts/install-gate-build.sh 2>&1)" || st=$?
dest=$main/target/release
if [ "$st" = 0 ] && answers "$dest/harnessd" "$build/release"; then
  note "the build CARGO_TARGET_DIR names is what lands in the main tree"
else
  fail "the build CARGO_TARGET_DIR names is what lands in the main tree" "$out"
fi
if [ -x "$dest/letibot-askpass" ]; then
  note "the role names install.sh lists are made beside it"
else
  fail "the role names install.sh lists are made beside it" "$(ls -l "$dest" 2>&1)"
fi
if [ "$st" = 0 ] && printf '%s' "$out" | grep -q 'letibot is a repository script'; then
  note "a launcher in BINARIES is said to be a script, not a missing build product"
else
  fail "a launcher in BINARIES is said to be a script, not a missing build product" "$out"
fi

# ---------------------------------------------------------------------------------------
# By hand in a worktree, with no CARGO_TARGET_DIR: the worktree's own target is the build, which
# is the legitimate run the two refusals below must not catch.
# ---------------------------------------------------------------------------------------
hand=$T/gate-hand
git -C "$main" worktree add -q "$hand" -b hand
built_all "$hand/target/release"
# The installed one is older on the clock, so "newest" has to pick the hand build.
touch -t 202101010000 "$dest/harnessd"
st=0
out="$(cd "$hand" && env -u CARGO_TARGET_DIR sh scripts/install-gate-build.sh 2>&1)" || st=$?
if [ "$st" = 0 ] && answers "$dest/harnessd" "$hand/target/release"; then
  note "by hand in a worktree, the worktree's own target follows its HEAD's build"
else
  fail "by hand in a worktree, the worktree's own target follows its HEAD's build" "$out"
fi

# ---------------------------------------------------------------------------------------
# Source and destination are one directory: the copy is a binary onto itself and the last line
# still says installed. Refused, naming the variable that says where the build went.
# ---------------------------------------------------------------------------------------
st=0
out="$(cd "$main" && env -u CARGO_TARGET_DIR sh scripts/install-gate-build.sh 2>&1)" || st=$?
if [ "$st" != 0 ] && printf '%s' "$out" | grep -q 'one directory' &&
  printf '%s' "$out" | grep -q 'CARGO_TARGET_DIR'; then
  note "refused when the build and the install directory are one"
else
  fail "refused when the build and the install directory are one" "$out"
fi
# And the same refusal when CARGO_TARGET_DIR is set but names that one directory, which is what a
# gate step that lost its own target dir would say.
st=0
out="$(cd "$main" && CARGO_TARGET_DIR="$main/target" sh scripts/install-gate-build.sh 2>&1)" || st=$?
if [ "$st" != 0 ] && printf '%s' "$out" | grep -q 'one directory'; then
  note "refused when CARGO_TARGET_DIR names the install directory"
else
  fail "refused when CARGO_TARGET_DIR names the install directory" "$out"
fi

# ---------------------------------------------------------------------------------------
# A build older than the one already installed: refused, and nothing is moved over the newer one.
# ---------------------------------------------------------------------------------------
older=$T/build/older
built_all "$older/release"
touch -t 202001010000 "$older/release/harnessd"
st=0
out="$(cd "$gate" && CARGO_TARGET_DIR="$older" sh scripts/install-gate-build.sh 2>&1)" || st=$?
if [ "$st" != 0 ] && printf '%s' "$out" | grep -q 'older'; then
  note "refused when the build is older than the newest harnessd it can see"
else
  fail "refused when the build is older than the newest harnessd it can see" "$out"
fi
if answers "$dest/harnessd" "$hand/target/release"; then
  note "and the newer installed daemon is still the one that runs"
else
  fail "and the newer installed daemon is still the one that runs" "$("$dest/harnessd" --version 2>&1)"
fi

# ---------------------------------------------------------------------------------------
[ "$fails" -eq 0 ] || {
  echo "$fails check(s) failed"
  exit 1
}
echo "install-gate-build: the build it is told about lands, and the two ways of installing nothing are refused"
