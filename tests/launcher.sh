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

[ "$fails" -eq 0 ] && echo "launcher: all cases pass" || { echo "launcher: $fails failed"; exit 1; }
