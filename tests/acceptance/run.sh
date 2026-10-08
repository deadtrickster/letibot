#!/usr/bin/env bash
# **Every acceptance spec, against the real head in tmux.** Exits non-zero when any fails.
#
#   cargo build -p letibot-tui --bin letibot-tui && tests/acceptance/run.sh
#   LETIBOT_TUI=target/release/letibot-tui tests/acceptance/run.sh editor-pane
#   tests/acceptance/run.sh --local             # and local/: a VM, firecode — never CI
#
# A spec is `tests/acceptance/*.sh` other than `lib.sh` and this file; naming some runs
# only those. See `lib.sh` for the driver and `docs/tui-testing.md` for the method.
set -u
DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
command -v tmux >/dev/null || { echo "acceptance: tmux is not installed"; exit 2; }

# `--local` adds `local/`: specs that need this box (a VM, firecode) and never run in CI.
local_too=0
[ "${1:-}" = "--local" ] && { local_too=1; shift; }

specs=()
if [ $# -gt 0 ]; then
  for n in "$@"; do
    if [ -f "$DIR/${n%.sh}.sh" ]; then specs+=("$DIR/${n%.sh}.sh"); else specs+=("$DIR/local/${n%.sh}.sh"); fi
  done
else
  for f in "$DIR"/*.sh; do
    case "$(basename "$f")" in lib.sh | run.sh) ;; *) specs+=("$f") ;; esac
  done
  if [ "$local_too" = 1 ]; then
    for f in "$DIR"/local/*.sh; do specs+=("$f"); done
  fi
fi

failed=0
for s in "${specs[@]}"; do
  echo "== $(basename "$s" .sh)"
  bash "$s" || failed=$((failed + 1))
done
[ "$failed" -eq 0 ] && echo "acceptance: ${#specs[@]} specs pass" || { echo "acceptance: $failed of ${#specs[@]} specs failed"; exit 1; }
