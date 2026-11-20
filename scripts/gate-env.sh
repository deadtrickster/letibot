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
# The step's runtime dir: the caller's XDG_RUNTIME_DIR= pair, else the scratch one.
run=$S/run
for a in "$@"; do
  case $a in
    XDG_RUNTIME_DIR=*) run=${a#XDG_RUNTIME_DIR=} ;;
    *=*) ;;
    *) break ;;
  esac
done
# `systemd-run --user` dials the user manager at $XDG_RUNTIME_DIR/systemd/private. That one socket
# is linked into the step's runtime dir, so letibot's sockets in the real one stay out of reach.
# No session bus: it carries the keyring.
m=${XDG_RUNTIME_DIR:-}/systemd/private
if [ -S "$m" ]; then
  mkdir -p "$run/systemd" && ln -sf "$m" "$run/systemd/private" || exit 2
fi
# The command line is rebuilt in front of the caller's NAME=VALUE pairs, so the caller's win. A
# gate a test starts takes a lock of its own, never the box's one the outer gate holds.
# TMUX_TMPDIR is the step's own runtime dir: tmux's default socket directory is /tmp/tmux-<uid>,
# which is the OPERATOR's server - a test's `tmux kill-server` there killed their byobu on
# 2026-11-19. TMUX itself is dropped with the rest of the environment (`env -i`), so a step
# cannot believe it is inside a session either.
set -- HOME="$S/home" XDG_DATA_HOME="$S/data" XDG_STATE_HOME="$S/state" XDG_RUNTIME_DIR="$S/run" \
  LETIBOT_GATE_LOCK="$S/gate.lock" TMUX_TMPDIR="$S/run" \
  CARGO_HOME="${CARGO_HOME:-$real/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$real/.rustup}" "$@"
# The read-only apparatus the tests measure against, by the variables they read: the caller's
# value, else the box's file under the real HOME, and only when it is there.
for a in \
  "LETIBOT_VOCAB_GGUF=$real/models/qwen3.8-flash-next/Qwen3.8-Flash-Next-UD-Q6_K_XL-00001-of-00006.gguf" \
  "LETIBOT_GLM_GGUF=$real/models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf" \
  "LETIBOT_CALIBRATION=$real/.local/share/letibot/calibration"; do
  n=${a%%=*}
  eval "v=\${$n:-}"
  [ -n "$v" ] || v=${a#*=}
  [ -e "$v" ] && set -- "$n=$v" "$@"
done
# Kept from the caller by name: the gate's own refusals file and, above, the apparatus paths, and
# nothing else of LETIBOT_*. No keys, sockets, XDG_CONFIG_HOME or XDG_CACHE_HOME.
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
