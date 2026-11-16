#!/usr/bin/env bash
# shellcheck disable=SC2015 # note and fail both answer 0, so `COND && note || fail` arms are exclusive
# **install.sh against a stub cargo and a stub curl**: what lands under which name.
#
# The launcher and the multicall are separate files, and no path through the installer may put a
# compiled binary under a launcher's name. Each case runs the real install.sh in a throwaway tree.
#
#   tests/install.sh      # from the repo root; exits non-zero on a failure
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

ELF=$(type -P true)
# The installer's refusal, matched as text: the backticks are literal.
# shellcheck disable=SC2016
REFUSED='is not a script, and `letibot` must be one'
if [ -z "$ELF" ] || head -c 2 "$ELF" | grep -q '^#!'; then
  echo "install: no compiled \`true\` on PATH to stand in for a binary"
  exit 1
fi
BUILT="harnessd letibot-tui letibot-multi letibot-mcp letibot-acp"
LIBS="libllama.so.0 libggml.so.0 libggml-cpu.so.0 libggml-base.so.0"

# A launcher stand-in: a script that answers --help.
launcher() {
  printf '#!/bin/sh\necho "%s: the launcher"\n' "$2" >"$1"
  chmod +x "$1"
}

mkdir -p "$T/stubs"
# `cargo build` writes every compiled name, and the multicall under no other.
cat >"$T/stubs/cargo" <<'STUB'
#!/bin/sh
manifest=""
while [ $# -gt 0 ]; do
  [ "$1" = --manifest-path ] && manifest="$2"
  shift
done
out="$(dirname "$manifest")/target/release"
mkdir -p "$out"
for b in $STUB_BUILT; do cp "$STUB_ELF" "$out/$b"; done
STUB
# `curl -o FILE URL` hands over the archive the case staged; anything else fails, as offline.
cat >"$T/stubs/curl" <<'STUB'
#!/bin/sh
out=""
while [ $# -gt 0 ]; do
  [ "$1" = -o ] && out="$2"
  [ "$1" = -I ] || [ "$1" = -fsSLI ] && exit 22
  shift
done
[ -n "$out" ] && [ -f "$STUB_ARCHIVE" ] || exit 22
cp "$STUB_ARCHIVE" "$out"
STUB
chmod +x "$T/stubs/cargo" "$T/stubs/curl"

# install CASE SCRIPT -- run SCRIPT into $T/CASE/bin and print what it said.
install() {
  mkdir -p "$T/$1/home"
  env -i PATH="$T/stubs:$PATH" HOME="$T/$1/home" LETIBOT_INSTALL_DIR="$T/$1/bin" \
    STUB_ELF="$ELF" STUB_BUILT="$BUILT" STUB_ARCHIVE="${STUB_ARCHIVE:-}" \
    sh "$2" 2>&1
}

is_script() { head -c 2 "$1" 2>/dev/null | grep -q '^#!'; }

# A checkout as `local_checkout` recognises one.
checkout() {
  mkdir -p "$1/scripts"
  printf '[workspace]\nmembers = ["crates/harnessd"]\n' >"$1/Cargo.toml"
  cp "$ROOT/install.sh" "$1/install.sh"
  launcher "$1/scripts/letibot" letibot
  launcher "$1/scripts/leticode" leticode
}

layout_ok() { # layout_ok CASE DIR -- the launcher a script, the multicall and its role one file
  d="$2"
  if is_script "$d/letibot" && is_script "$d/leticode"; then
    note "$1: letibot and leticode are the scripts"
  else
    fail "$1: letibot and leticode are the scripts" "$(ls -la "$d")"
  fi
  if [ -f "$d/letibot-multi" ] && ! is_script "$d/letibot-multi"; then
    note "$1: the multicall is letibot-multi"
  else
    fail "$1: the multicall is letibot-multi" "$(ls -la "$d")"
  fi
  if [ "$d/letibot-askpass" -ef "$d/letibot-multi" ]; then
    note "$1: letibot-askpass is letibot-multi under the role's name"
  else
    fail "$1: letibot-askpass is letibot-multi under the role's name" "$(ls -lai "$d")"
  fi
}

# --- a source install --------------------------------------------------------------------------
checkout "$T/src"
out="$(install src "$T/src/install.sh")"
st=$?
[ "$st" -eq 0 ] && note "source: install.sh exits 0" || fail "source: install.sh exits 0" "$out"
layout_ok source "$T/src/bin"

# The checkout's launcher is an ELF: refused, and the installed script is left as it was.
checkout "$T/srcelf"
mkdir -p "$T/srcelf/bin"
launcher "$T/srcelf/bin/letibot" letibot
cp "$ELF" "$T/srcelf/scripts/letibot"
out="$(install srcelf "$T/srcelf/install.sh")"
st=$?
if [ "$st" -ne 0 ] && printf '%s' "$out" | grep -qF "$REFUSED"; then
  note "source: an ELF at scripts/letibot is refused by name"
else
  fail "source: an ELF at scripts/letibot is refused by name" "exit $st: $out"
fi
is_script "$T/srcelf/bin/letibot" && note "source: the installed launcher is untouched" ||
  fail "source: the installed launcher is untouched" "$(ls -la "$T/srcelf/bin")"

# --- a prebuilt archive ------------------------------------------------------------------------
# archive DIR [NAMES] -- an archive of stub binaries, the launchers as scripts, the libraries.
archive() {
  mkdir -p "$1/stage"
  for b in ${2:-$BUILT}; do cp "$ELF" "$1/stage/$b"; done
  launcher "$1/stage/letibot" letibot
  launcher "$1/stage/leticode" leticode
  for l in $LIBS; do : >"$1/stage/$l"; done
  tar -czf "$1/a.tar.gz" -C "$1/stage" .
}
mkdir -p "$T/bare"
cp "$ROOT/install.sh" "$T/bare/install.sh"

# The prebuilt path checks the host's runtime libraries first; a box without them cannot reach
# the copy at all, which is not this suite's question.
prebuilt_reached() {
  case "$1" in
    *"missing libraries the binaries need"* | *"no glibc dynamic loader"*)
      echo "skip $2: this host lacks the prebuilt runtime"
      return 1
      ;;
  esac
  return 0
}

archive "$T/arc"
out="$(STUB_ARCHIVE="$T/arc/a.tar.gz" install pre "$T/bare/install.sh")"
st=$?
if prebuilt_reached "$out" prebuilt; then
  [ "$st" -eq 0 ] && note "prebuilt: install.sh exits 0" || fail "prebuilt: install.sh exits 0" "$out"
  layout_ok prebuilt "$T/pre/bin"
fi

# An archive whose `letibot` is the multicall: refused, nothing written under that name.
archive "$T/arcelf"
cp "$ELF" "$T/arcelf/stage/letibot"
tar -czf "$T/arcelf/a.tar.gz" -C "$T/arcelf/stage" .
out="$(STUB_ARCHIVE="$T/arcelf/a.tar.gz" install preelf "$T/bare/install.sh")"
st=$?
if prebuilt_reached "$out" "prebuilt ELF"; then
  if [ "$st" -ne 0 ] && printf '%s' "$out" | grep -qF "$REFUSED"; then
    note "prebuilt: an ELF named letibot in the archive is refused"
  else
    fail "prebuilt: an ELF named letibot in the archive is refused" "exit $st: $out"
  fi
  [ -e "$T/preelf/bin/letibot" ] && fail "prebuilt: nothing lands at letibot" "$(ls -la "$T/preelf/bin")" ||
    note "prebuilt: nothing lands at letibot"
fi

# An archive from before the rename: no multicall, its own askpass binary installed instead.
archive "$T/arcold" "harnessd letibot-tui letibot-mcp letibot-acp letibot-askpass"
out="$(STUB_ARCHIVE="$T/arcold/a.tar.gz" install old "$T/bare/install.sh")"
st=$?
if prebuilt_reached "$out" "old archive"; then
  if [ "$st" -eq 0 ] && printf '%s' "$out" | grep -q 'this release carries no letibot-multi'; then
    note "old archive: installs and names the missing multicall"
  else
    fail "old archive: installs and names the missing multicall" "exit $st: $out"
  fi
  [ -f "$T/old/bin/letibot-askpass" ] && ! is_script "$T/old/bin/letibot-askpass" &&
    note "old archive: its letibot-askpass is installed" ||
    fail "old archive: its letibot-askpass is installed" "$(ls -la "$T/old/bin")"
fi

# --- the gate's check on a build's output -------------------------------------------------------
# built CASE -- run check-dist-names.sh --built on $T/CASE and print its exit code and output.
built() {
  out="$(sh "$ROOT/scripts/check-dist-names.sh" --built "$T/$1" 2>&1)"
  printf '%s\n%s' "$?" "$out"
}
mkdir -p "$T/rel"
cp "$ELF" "$T/rel/letibot-multi"
out="$(built rel)"
[ "${out%%$'\n'*}" = 0 ] && note "check: a build with no letibot passes" || fail "check: a build with no letibot passes" "$out"
cp "$ELF" "$T/rel/letibot"
touch -t 202001010000 "$T/rel/letibot"
out="$(built rel)"
if [ "${out%%$'\n'*}" != 0 ] && printf '%s' "$out" | grep -q 'stale build product'; then
  note "check: an ELF letibot older than letibot-multi fails as stale"
else
  fail "check: an ELF letibot older than letibot-multi fails as stale" "$out"
fi
touch "$T/rel/letibot"
touch -t 202001010000 "$T/rel/letibot-multi"
out="$(built rel)"
if [ "${out%%$'\n'*}" != 0 ] && printf '%s' "$out" | grep -q 'compiled binary under the launcher'; then
  note "check: a freshly built ELF named letibot fails the check"
else
  fail "check: a freshly built ELF named letibot fails the check" "$out"
fi

# An interpolated matrix triple cannot be compared with install.sh, so it is refused.
mkdir -p "$T/dist/scripts" "$T/dist/.github/workflows"
cp "$ROOT/install.sh" "$T/dist/"
cp "$ROOT/scripts/check-dist-names.sh" "$ROOT/scripts/make-dist.sh" "$T/dist/scripts/"
cp "$ROOT"/.github/workflows/*.yml "$T/dist/.github/workflows/"
st=0
out="$(sh "$T/dist/scripts/check-dist-names.sh" 2>&1)" || st=$?
[ "$st" = 0 ] && note "check: the repo's own workflows pass" || fail "check: the repo's own workflows pass" "$out"
# The value is literal YAML text.
# shellcheck disable=SC2016
printf 'jobs:\n  b:\n    with:\n      triple: ${{ inputs.triple }}\n' >"$T/dist/.github/workflows/extra.yml"
st=0
out="$(sh "$T/dist/scripts/check-dist-names.sh" 2>&1)" || st=$?
if [ "$st" != 0 ] && printf '%s' "$out" | grep -q 'cannot read'; then
  note "check: an interpolated matrix triple is refused, not dropped"
else
  fail "check: an interpolated matrix triple is refused, not dropped" "$out"
fi

# ---------------------------------------------------------------------------------------
[ "$fails" -eq 0 ] || {
  echo "$fails check(s) failed"
  exit 1
}
echo "install: the launchers are scripts and the multicall is letibot-multi on every path"
