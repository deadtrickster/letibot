#!/bin/sh
# gate-env.sh SCRATCH [NAME=VALUE...] COMMAND...: run COMMAND in the gate's clean environment.
# The one builder both gates use: scripts/gate-at.sh runs it, the merge queue embeds it
# (`mergequeue/gate.rs`), so a test sees the same HOME, XDG dirs and doors under either.
set -u
[ $# -ge 2 ] || {
  echo "usage: gate-env.sh SCRATCH [NAME=VALUE...] COMMAND..." >&2
  exit 2
}
S=$1
shift
mkdir -p "$S/home/.cargo" "$S/home/.rustup" "$S/data" "$S/state" "$S/run" || exit 2
chmod 700 "$S/run" || exit 2
real=${HOME:-/nonexistent}
[ -f "$real/.gitconfig" ] && cp "$real/.gitconfig" "$S/home/.gitconfig"
# Remote-host tests need the hosts and their keys. Copies, read-only, of the files hosts.toml
# names: a linked keys directory would let a test write the operator's keys.
c=$real/.config/letibot
if [ -f "$c/hosts.toml" ]; then
  mkdir -p "$S/home/.config/letibot/keys" || exit 2
  for f in hosts.toml known_hosts; do
    [ -f "$c/$f" ] && cp "$c/$f" "$S/home/.config/letibot/$f" && chmod 444 "$S/home/.config/letibot/$f"
  done
  sed -n 's|^[[:space:]]*key[[:space:]]*=[[:space:]]*"~/\.config/letibot/keys/\([^"/]*\)".*|\1|p' "$c/hosts.toml" |
    while IFS= read -r k; do
      for f in "$k" "$k.pub"; do
        [ -f "$c/keys/$f" ] && cp "$c/keys/$f" "$S/home/.config/letibot/keys/$f" && chmod 400 "$S/home/.config/letibot/keys/$f"
      done
    done
fi
# The command line is rebuilt in front of the caller's NAME=VALUE pairs, so the caller's win.
set -- HOME="$S/home" XDG_DATA_HOME="$S/data" XDG_STATE_HOME="$S/state" XDG_RUNTIME_DIR="$S/run" \
  CARGO_HOME="${CARGO_HOME:-$real/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$real/.rustup}" "$@"
# Kept from the caller by name: the gate's own refusals file and nothing else of LETIBOT_*.
# No keys, sockets, XDG_CONFIG_HOME or XDG_CACHE_HOME.
for n in PATH USER LOGNAME LANG LC_ALL TZ TMPDIR CARGO_TARGET_DIR CARGO_BUILD_JOBS LETIBOT_APPARATUS_REFUSALS; do
  eval "has=\${$n+x}"
  # shellcheck disable=SC2154 # assigned by the eval above
  if [ -n "$has" ]; then
    eval "v=\$$n"
    # shellcheck disable=SC2154 # assigned by the eval above
    set -- "$n=$v" "$@"
  fi
done
exec env -i "$@"
