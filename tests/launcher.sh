#!/usr/bin/env bash
# **The launcher's model-to-endpoint gate, run for real against stub binaries.**
#
# `scripts/letibot` decides where a run's turns go before any Rust runs, so nothing in
# `cargo test` reaches it. This runs it one-shot (`letibot "q"` execs harnessd) with a
# stub harnessd that prints its argv, a throwaway HOME and runtime dir, and a
# providers.toml written per case, and reads back what was started or refused.
# Nothing listens anywhere, so a case that needed the local model server sees none.
#
#   tests/launcher.sh            # from the repo root; exits non-zero on a failure
set -u
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
LAUNCHER="$ROOT/scripts/letibot"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

mkdir -p "$T/bin" "$T/home/.config/letibot" "$T/run" "$T/ws"
cat > "$T/bin/harnessd" <<'STUB'
#!/usr/bin/env bash
printf 'harnessd'; printf ' %q' "$@"; printf '\n'
STUB
cp "$T/bin/harnessd" "$T/bin/letibot-tui"
chmod +x "$T/bin/harnessd" "$T/bin/letibot-tui"

fails=0
# run NAME PROVIDERS_TOML [ARGS...] -- the launcher's combined output for one case.
run() {
  local toml="$1"; shift
  printf '%s' "$toml" > "$T/home/.config/letibot/providers.toml"
  (cd "$T/ws" && env -i PATH="/usr/bin:/bin" HOME="$T/home" XDG_RUNTIME_DIR="$T/run" \
    LETIBOT_BIN="$T/bin" LETIBOT_ENDPOINT= "$(command -v bash)" "$LAUNCHER" "$@" 2>&1 </dev/null)
}
expect() { # expect NAME OUTPUT PATTERN -- the output must match
  if printf '%s' "$2" | grep -qE -- "$3"; then echo "ok   $1"
  else echo "FAIL $1: wanted /$3/ in:"; printf '%s\n' "$2" | sed 's/^/     /'; fails=$((fails + 1)); fi
}
refuse() { # refuse NAME OUTPUT PATTERN -- the output must not match
  if printf '%s' "$2" | grep -qE -- "$3"; then
    echo "FAIL $1: did not want /$3/ in:"; printf '%s\n' "$2" | sed 's/^/     /'; fails=$((fails + 1))
  else echo "ok   $1"; fi
}

DEFAULT_ONLY='[deepseek]

[default]
provider = "deepseek"
'
out="$(run "$DEFAULT_ONLY" "one question")"
refuse "a [default] provider with no local model block is not refused" "$out" "no endpoint for this run"
expect "  ...and the daemon is started" "$out" "^harnessd "
refuse "  ...with no local endpoint invented for it" "$out" "--endpoint"

out="$(run "$DEFAULT_ONLY" --status)"
expect "--status says there is no local home rather than refusing" "$out" "^local +none"

out="$(run '' "one question")"
expect "no provider and no local model block is still refused" "$out" "no endpoint for this run"
refuse "  ...and nothing is started" "$out" "^harnessd "

LOCAL='[model."local.flash"]
url = "http://127.0.0.1:59999"
model = "qwen-3.8-flash-next"

[default]
provider = "deepseek"
'
out="$(run "$LOCAL" "one question")"
expect "a local model with a home passes its endpoint on" "$out" "--endpoint 127.0.0.1:59999"

# ---------------------------------------------------------------------------
# **A daemon that is LISTENING but does not speak this build's protocol is refused
# with the sentence that names the way out** — not handed a head that dies on the
# daemon's bare refusal line.
#
# MEASURED 2026-10-09, in the operator's own terminal: `leticode --continue` against a
# daemon up since 09-21 printed
#
#     letibot: attached to harnessd for … (pid 2076966), up since 2026-09-21T07:16:16
#     letibot-tui: the daemon refused the attach: protocol version 36, this daemon speaks 22
#
# and stopped there: no `--stop`, no socket, nothing about the warm session that had just
# become unreachable. `version_skew_or_continue` already held the right sentence and ran
# only in the branch that STARTS a daemon, so a daemon already listening never met it.
#
# This needs two things the other cases do not: a socket something is really LISTENING on
# (`live` is `ss`, not a file test, because a daemon's shutdown unlinks the file while the
# process keeps serving), and a head that refuses the way an older daemon's would.
key12() { if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi | cut -c1-12; }
if command -v python3 >/dev/null 2>&1; then
  cat > "$T/bin/letibot-tui" <<'STUB'
#!/usr/bin/env bash
for a in "$@"; do
  if [ "$a" = "--probe" ]; then
    echo "letibot-tui: the daemon refused the attach: protocol version 36, this daemon speaks 22" >&2
    exit 1
  fi
done
echo "letibot-tui: THE HEAD RAN"
STUB
  chmod +x "$T/bin/letibot-tui"
  WS_P="$(cd "$T/ws" && pwd -P)"
  # **`$T/run/letibot`, not `$T/run`**: the launcher's RUNDIR is
  # `$XDG_RUNTIME_DIR/letibot`, so a socket one level up is a socket nobody looks at —
  # which is exactly what the first draft of this case did, and the launcher went on to
  # start a daemon over a socket it could not see.
  mkdir -p "$T/run/letibot"
  SOCK_P="$T/run/letibot/$(printf '%s' "$WS_P" | key12).sock"
  python3 -c 'import socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.bind(sys.argv[1])
s.listen(2)
time.sleep(120)' "$SOCK_P" &
  _listener=$!
  sleep 0.3

  out="$(run "$DEFAULT_ONLY")"
  expect "a daemon speaking another protocol is refused, not attached to" "$out" "does not speak this build's protocol"
  expect "  ...and the sentence names the way out" "$out" "letibot --stop"
  expect "  ...and says nothing is lost" "$out" "on disk"
  refuse "  ...and no head is handed to it" "$out" "THE HEAD RAN"

  out="$(run "$DEFAULT_ONLY" --attach)"
  expect "--attach refuses in the same words" "$out" "does not speak this build's protocol"
  refuse "  ...and does not claim to have attached" "$out" "attached to harnessd"

  kill "$_listener" 2>/dev/null || true
else
  echo "skip the protocol-skew cases: no python3 to listen on a unix socket"
fi

# **And the reader that feeds the stale line.** `pid_started` read
# `stat -c %Y "/proc/<pid>"` — a procfs DIRECTORY's mtime, which is not a process's start
# time and is not even per-process. MEASURED 2026-10-09: three daemons up 17, 9 and 4 days
# reported the SAME value (10-08 21:20), while `ps -o lstart=` and `/proc/<pid>/stat`'s own
# starttime agreed with each other on all three, to the second.
#
# The reading is asserted AT THE SOURCE because that is where the mistake lives and because
# a young process cannot expose it: a freshly started process's directory mtime DOES equal
# its start time, which is how the wrong reader passed every check it was ever given.
# `/proc/<pid>/stat`'s field 22 is the reader; see `pid_started`.
#
# **Comments are stripped before the grep**, because the reader's own comment QUOTES the
# wrong line to say what it replaced — a guard that cannot tell the two apart is red on
# the fix itself, which is how this guard first ran.
if grep -v '^[[:space:]]*#' "$LAUNCHER" | grep -q 'stat -c %Y "/proc/'; then
  echo "FAIL a process start time is read from a /proc DIRECTORY's mtime:"
  grep -v '^[[:space:]]*#' "$LAUNCHER" | grep -n 'stat -c %Y "/proc/' | sed 's/^/     /'
  fails=$((fails + 1))
else
  echo "ok   no reader takes a /proc directory's mtime for a process start time"
fi

[ "$fails" -eq 0 ] && echo "launcher: all cases pass" || { echo "launcher: $fails failed"; exit 1; }
