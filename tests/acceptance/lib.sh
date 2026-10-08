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
# The binary under test: LETIBOT_TUI names one, else the workspace's debug build.
TUI="${LETIBOT_TUI:-$ROOT/target/debug/letibot-tui}"
COLS="${ACCEPTANCE_COLS:-120}"
ROWS="${ACCEPTANCE_ROWS:-36}"
TIMEOUT="${ACCEPTANCE_TIMEOUT:-10}"

SERVER="letibot-acc-$$"
WORK="$(mktemp -d)"
FAILS=0
STEPS=0
trap 'tmux -L "$SERVER" kill-server 2>/dev/null; rm -rf "$WORK"' EXIT

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
