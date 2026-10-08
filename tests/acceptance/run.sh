#!/usr/bin/env bash
# **Every acceptance spec, against the real head in tmux.** Exits non-zero when any fails.
#
#   cargo build -p letibot-tui --bin letibot-tui && tests/acceptance/run.sh
#   LETIBOT_TUI=target/release/letibot-tui tests/acceptance/run.sh editor-pane
#
# A spec is `tests/acceptance/*.sh` other than `lib.sh` and this file; naming some runs
# only those. See `lib.sh` for the driver and `docs/tui-testing.md` for the method.
set -u
DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
command -v tmux >/dev/null || { echo "acceptance: tmux is not installed"; exit 2; }

specs=()
if [ $# -gt 0 ]; then
  for n in "$@"; do specs+=("$DIR/${n%.sh}.sh"); done
else
  for f in "$DIR"/*.sh; do
    case "$(basename "$f")" in lib.sh | run.sh) ;; *) specs+=("$f") ;; esac
  done
fi

failed=0
for s in "${specs[@]}"; do
  echo "== $(basename "$s" .sh)"
  bash "$s" || failed=$((failed + 1))
done
[ "$failed" -eq 0 ] && echo "acceptance: ${#specs[@]} specs pass" || { echo "acceptance: $failed of ${#specs[@]} specs failed"; exit 1; }
