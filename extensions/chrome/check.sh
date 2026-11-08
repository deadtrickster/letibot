#!/usr/bin/env bash
# Syntax-gate the extension the way Chrome parses it, from oracle's chrome-capture check.sh.
# Module files are checked as .mjs and injected files as .cjs: plain `node --check` reads a .js
# as a classic script and passes module-only mistakes. Needs node: `firecode exec bash check.sh`.
set -euo pipefail

cd "$(dirname "$0")"

MODULES=(background.js tab.js sse.js popup.js options.js)
CLASSIC=(overlay.js regionselect.js)

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fail=0

for f in "${MODULES[@]}"; do
	cp "$f" "$tmp/${f%.js}.mjs"
	if node --check "$tmp/${f%.js}.mjs"; then
		printf '  ok    %-20s (module)\n' "$f"
	else
		printf '  FAIL  %-20s (module)\n' "$f"
		fail=1
	fi
done

for f in "${CLASSIC[@]}"; do
	cp "$f" "$tmp/${f%.js}.cjs"
	if node --check "$tmp/${f%.js}.cjs"; then
		printf '  ok    %-20s (classic)\n' "$f"
	else
		printf '  FAIL  %-20s (classic)\n' "$f"
		fail=1
	fi
done

if node -e 'JSON.parse(require("fs").readFileSync("manifest.json", "utf8"))'; then
	printf '  ok    %-20s (json)\n' manifest.json
else
	printf '  FAIL  %-20s (json)\n' manifest.json
	fail=1
fi

# An injected function runs in the page, so a reference to this worker's module scope is a
# ReferenceError there that no syntax check can see. tab.js is checked the same way.
if node check-injected.mjs; then
	printf '  ok    %-20s (injected scope)\n' "background.js tab.js"
else
	printf '  FAIL  %-20s (injected scope)\n' "background.js tab.js"
	fail=1
fi

exit "$fail"
