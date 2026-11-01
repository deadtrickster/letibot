#!/bin/sh
# Every #[test] attribute must sit on its own function.
#
# Two test attributes separated only by comments and other attributes attach
# both to one function, and the test the first one was meant for silently stops
# running while the suite stays green. Reject that shape before it lands.
#
#     sh scripts/check-test-attrs.sh [file ...]
#
# With no arguments every tracked .rs file is checked; the explicit form is for
# trying a planted case by hand.
set -eu

if [ "$#" -eq 0 ]; then
  # shellcheck disable=SC2046 # a word-split path list; this tree has no spaces in .rs paths
  set -- $(git ls-files -- '*.rs')
fi
[ "$#" -gt 0 ] || {
  echo "check-test-attrs: no Rust files to check" >&2
  exit 1
}

# The report inside the first rule is the whole check: a test attribute arriving
# while another is still waiting for its fn. Between a test attribute and its fn
# only comments, blank lines and other attributes may appear; anything else ends
# the run, and a fn ends it.
if awk '
  FNR == 1 { armed = 0 }
  /^[[:space:]]*#\[[[:space:]]*(tokio::)?test[][:space:](]/ {
    if (armed) {
      printf "%s:%d: a second test attribute before any fn, so both attach to one function\n", FILENAME, FNR > "/dev/stderr"
      bad = 1
    }
    # a fn on the same line as its attribute ends the run at once
    rest = $0
    sub(/^[[:space:]]*#\[[^]]*\][[:space:]]*/, "", rest)
    armed = (rest !~ /^(async[[:space:]]+|const[[:space:]]+|unsafe[[:space:]]+)*fn[][:space:](]/)
    next
  }
  !armed { next }
  /^[[:space:]]*(async[[:space:]]+|const[[:space:]]+|unsafe[[:space:]]+)*fn[][:space:](]/ { armed = 0; next }
  /^[[:space:]]*(\/\/.*)?$/ { next }
  /^[[:space:]]*#\[/ { next }
  { armed = 0 }
  END { exit bad ? 1 : 0 }
' "$@"; then
  echo "check-test-attrs: $# file(s) checked, every test attribute on a function of its own"
else
  echo "check-test-attrs: a test attribute does not sit on a function of its own" >&2
  exit 1
fi
