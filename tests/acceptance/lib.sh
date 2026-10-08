#!/usr/bin/env bash
# **The acceptance suite's driver: the real head in a real terminal, read back as a screen.**
#
# Playwright's shape for a terminal: start the binary in a private tmux server, act on it
# with keys and mouse reports a terminal would send, locate things by the text on screen,
# and wait for the screen rather than sleeping a guessed number of seconds. A spec sources
# this file and reads as steps:
#
#   start --replay "$FIXTURES/edit.jsonl"
#   wait_for "Edited notes.txt"
#   press C-]
#   wait_for "Review notes.txt"
#   click "Edited notes.txt"
#   expect_not "content not loaded"
#
# Why tmux and not a pty in a test: tmux IS a terminal emulator, so the bytes the head
# writes are interpreted the way Ghostty interprets them, and `capture-pane` is the screen a
# person sees. `docs/tui-testing.md` is the method; this is it as a library.
#
# Every server is private (`-L`), named per run, and killed on exit, so a spec never attaches
# to a session it did not start and never leaves one behind.
set -u

ACCEPTANCE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(cd -- "$ACCEPTANCE/../.." && pwd -P)"
FIXTURES="$ACCEPTANCE/fixtures"
# The binary under test: LETIBOT_TUI names one, else the newer of the workspace's debug
# and release builds — a stale debug binary beside a fresh release one fails every spec
# that tests anything new.
if [ -n "${LETIBOT_TUI:-}" ]; then
  TUI="$LETIBOT_TUI"
elif [ "$ROOT/target/release/letibot-tui" -nt "$ROOT/target/debug/letibot-tui" ]; then
  TUI="$ROOT/target/release/letibot-tui"
else
  TUI="$ROOT/target/debug/letibot-tui"
fi
COLS="${ACCEPTANCE_COLS:-120}"
ROWS="${ACCEPTANCE_ROWS:-36}"
TIMEOUT="${ACCEPTANCE_TIMEOUT:-10}"

SERVER="letibot-acc-$$"
WORK="$(mktemp -d)"
FAILS=0
STEPS=0
FAKE_PID=""
SESSION_UP=0
teardown() {
  tmux -L "$SERVER" kill-server 2>/dev/null
  # The run's own daemon, never anyone else's: same HOME, same runtime dir, same folder.
  ((SESSION_UP)) && (cd "$WORK/ws" && session_env "$ROOT/scripts/letibot" --stop >/dev/null 2>&1)
  [ -n "$FAKE_PID" ] && kill "$FAKE_PID" 2>/dev/null
  rm -rf "$WORK"
}
trap teardown EXIT

_t() { tmux -L "$SERVER" "$@"; }

# screen [-e] -- the pane as a person sees it; -e keeps the SGR (colour, reverse, cursor).
screen() { _t capture-pane -p -t acc "$@" 2>/dev/null; }

# The run's own HOME: the head's prefs and rano's config are read from here, never from the
# person running the suite, so a spec sees the same screen on a laptop and in CI. A spec that
# needs a setting writes it into $CONFIG first (rano's is $CONFIG/rano/config.toml).
CONFIG="$WORK/home/.config"
mkdir -p "$CONFIG"

# start ARGS... -- the head, in $WORK, at COLS x ROWS. A spec writes its files into $WORK
# first: a replay has no session workspace, so the pane opens paths relative to here.
start() {
  [ -x "$TUI" ] || { echo "acceptance: no binary at $TUI (cargo build -p letibot-tui)"; exit 2; }
  local cmd
  cmd="cd $(printf '%q' "$WORK") && HOME=$(printf '%q' "$WORK/home") XDG_CONFIG_HOME=$(printf '%q' "$CONFIG") exec $(printf '%q' "$TUI")"
  for a in "$@"; do cmd="$cmd $(printf '%q' "$a")"; done
  _t new-session -d -s acc -x "$COLS" -y "$ROWS" "$cmd"
}

# fake_model -- the scripted model (`fakemodel.py`) on a port of its own, and the run's
# providers.toml pointing deepseek at it as the default, so a whole session — parent and
# subagents — runs on it with no network, no key and no cost. Its requests are logged to
# $WORK/model.log.
fake_model() {
  FAKEMODEL_LOG="$WORK/model.log" python3 "$ACCEPTANCE/fakemodel.py" "$WORK/model.port" \
    >"$WORK/model.out" 2>&1 &
  FAKE_PID=$!
  local i=0
  while [ ! -s "$WORK/model.port" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
  mkdir -p "$CONFIG/letibot"
  printf '[deepseek]\nkey = "sk-acceptance"\nurl = "http://127.0.0.1:%s/chat/completions"\n\n[default]\nprovider = "deepseek"\n' \
    "$(cat "$WORK/model.port")" >"$CONFIG/letibot/providers.toml"
}

# session_env CMD... -- CMD with the run's environment and nothing else: its HOME, its
# runtime dir, the repository's launcher scripts first on PATH, and firecode's directory
# when there is one. The daemon this starts is the run's alone.
session_env() {
  local path="$ROOT/scripts:/usr/bin:/bin:/usr/sbin:/sbin"
  command -v firecode >/dev/null 2>&1 && path="$(dirname "$(command -v firecode)"):$path"
  mkdir -p "$WORK/run"
  env -i PATH="$path" HOME="$WORK/home" XDG_CONFIG_HOME="$CONFIG" XDG_RUNTIME_DIR="$WORK/run" \
    TERM=xterm-256color USER="$(id -un)" LOGNAME="$(id -un)" LETIBOT_LOG="$WORK/harnessd.log" "$@"
}

# start_session ARGS... -- the real launcher, `leticode ARGS`, in a fresh git workspace at
# $WORK/ws: launcher, daemon and head, the way a person starts one. Uses the release build
# beside the scripts (`cargo build --release`).
start_session() {
  mkdir -p "$WORK/ws"
  (cd "$WORK/ws" && git init -q && echo "# acceptance" >README.md && git add -A &&
    git -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -qm init)
  SESSION_UP=1
  # A script, not a command line: tmux runs its command under the person's login shell,
  # and the environment is bash's to build.
  {
    echo '#!/usr/bin/env bash'
    declare -p ROOT WORK CONFIG
    declare -f session_env
    printf 'cd %q && session_env leticode' "$WORK/ws"
    printf ' %q' "$@"
    echo
  } >"$WORK/start.sh"
  _t new-session -d -s acc -x "$COLS" -y "$ROWS" "bash $(printf '%q' "$WORK/start.sh")"
}

# press KEY... -- tmux key names: C-], M-s, Escape, Enter, s.
press() { _t send-keys -t acc "$@"; }

# type TEXT -- literal text, as typed.
type_text() { _t send-keys -t acc -l "$1"; }

# locate TEXT -- "ROW COL" (1-based, a terminal's) of TEXT's first occurrence, or nothing.
locate() {
  screen | awk -v want="$1" '{ i = index($0, want); if (i) { print NR, i; exit } }'
}

# click TEXT -- a left press and release on TEXT, as the SGR mouse report a terminal sends.
click() {
  local at row col
  at="$(locate "$1")"
  if [ -z "$at" ]; then fail "click: no \"$1\" on screen"; return; fi
  row="${at% *}"; col="${at#* }"
  _t send-keys -t acc -l $'\e[<0;'"$col;$row"'M'
  _t send-keys -t acc -l $'\e[<0;'"$col;$row"'m'
}

# wait_for TEXT -- until TEXT is on screen, or fail after TIMEOUT seconds.
wait_for() {
  local i=0 limit=$((TIMEOUT * 10))
  while [ "$i" -lt "$limit" ]; do
    screen | grep -qF -- "$1" && { pass "shows \"$1\""; return 0; }
    sleep 0.1; i=$((i + 1))
  done
  fail "waited ${TIMEOUT}s for \"$1\""
}

# wait_gone TEXT -- until TEXT has left the screen.
wait_gone() {
  local i=0 limit=$((TIMEOUT * 10))
  while [ "$i" -lt "$limit" ]; do
    screen | grep -qF -- "$1" || { pass "no longer shows \"$1\""; return 0; }
    sleep 0.1; i=$((i + 1))
  done
  fail "waited ${TIMEOUT}s for \"$1\" to go"
}

# settle -- until two captures 0.5 s apart are identical (a replay paces itself).
settle() {
  local a b i=0
  a="$(screen)"
  while [ "$i" -lt $((TIMEOUT * 2)) ]; do
    sleep 0.5; b="$(screen)"
    [ "$a" = "$b" ] && [ -n "$b" ] && return 0
    a="$b"; i=$((i + 1))
  done
}

# expect TEXT / expect_not TEXT -- the screen, now.
expect() { if screen | grep -qF -- "$1"; then pass "shows \"$1\""; else fail "does not show \"$1\""; fi; }
expect_not() { if screen | grep -qF -- "$1"; then fail "shows \"$1\""; else pass "does not show \"$1\""; fi; }

# expect_line PATTERN TEXT -- the first line matching PATTERN also contains TEXT.
expect_line() {
  local line
  line="$(screen | grep -m1 -E -- "$1")"
  case "$line" in
    *"$2"*) pass "the \"$1\" line has \"$2\"" ;;
    *) fail "the \"$1\" line lacks \"$2\": ${line:-<no such line>}" ;;
  esac
}

pass() { STEPS=$((STEPS + 1)); printf '  ok    %s\n' "$1"; }
fail() {
  STEPS=$((STEPS + 1)); FAILS=$((FAILS + 1))
  printf '  FAIL  %s\n' "$1"
  screen | sed 's/^/        | /'
}

# spec NAME -- a heading for the steps that follow.
spec() { printf '%s\n' "$1"; }

# done_spec -- the exit status a runner reads.
done_spec() {
  if [ "$FAILS" -eq 0 ]; then printf '  %d steps, all pass\n' "$STEPS"; exit 0; fi
  printf '  %d of %d steps failed\n' "$FAILS" "$STEPS"; exit 1
}
