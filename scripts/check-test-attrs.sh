#!/bin/sh
# Every #[test] attribute must sit on its own function. Two test attributes separated only by
# comments and attributes attach to one function, and one test silently stops running.
#
#     sh scripts/check-test-attrs.sh [file ...]     (no arguments: every tracked .rs file)
set -eu

if [ "$#" -eq 0 ]; then
  # shellcheck disable=SC2046 # a word-split path list; this tree has no spaces in .rs paths
  set -- $(git ls-files -- '*.rs')
fi
[ "$#" -gt 0 ] || {
  echo "check-test-attrs: no Rust files to check" >&2
  exit 1
}

# Between a test attribute and its fn only comments, blank lines and attributes may appear.
# A second test attribute in that run is the report; anything else, or a fn, ends the run.
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

# A test may not clear `LETIBOT_REQUIRE_APPARATUS` from a fixture's environment, and may not set it
# to `0` either: both turn every skip in the binary green again.
if awk '
  /(set_var|remove_var)[[:space:]]*\([[:space:]]*"LETIBOT_REQUIRE_(APPARATUS|MODEL)"/ {
    printf "%s:%d: a fixture turns the gate demand for the apparatus off, so its skips stay green\n", FILENAME, FNR > "/dev/stderr"
    bad = 1
  }
  END { exit bad ? 1 : 0 }
' "$@"; then
  echo "check-test-attrs: $# file(s) checked, no fixture turns the gate demand off"
else
  echo "check-test-attrs: a fixture sets or clears LETIBOT_REQUIRE_APPARATUS or LETIBOT_REQUIRE_MODEL" >&2
  exit 1
fi
