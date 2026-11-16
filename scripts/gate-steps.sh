#!/bin/sh
# gate-steps.sh [FILE]: print the merge gate's steps out of an AGENTS.md (FILE, or stdin), one per
# line; exit 1 when it declares none. The reading of `parse_merge_gate` (tools/src/gatekeeper.rs),
# held to it by `gate_steps_sh_reads_what_the_queue_reads`.
set -u
[ $# -le 1 ] || {
  echo "usage: gate-steps.sh [FILE]" >&2
  exit 2
}
f=${1:--}
awk '
function trim(s) {
  sub(/^[ \t\r]+/, "", s)
  sub(/[ \t\r]+$/, "", s)
  return s
}
function level(line, t) {
  t = line
  sub(/^[ \t]+/, "", t)
  if (!match(t, /^#+/)) return 0
  if (substr(t, RLENGTH + 1, 1) != " ") return 0
  return RLENGTH
}
function title(line, t, n) {
  t = line
  sub(/^[ \t]+/, "", t)
  n = level(line)
  t = trim(substr(t, n + 1))
  sub(/#+$/, "", t)
  return tolower(trim(t))
}
BEGIN { at = 0; fence = ""; n = 0 }
at == 0 {
  l = level($0)
  if (l > 0 && title($0) == "merge gate") at = l
  next
}
fence == "" {
  l = level($0)
  if (l > 0 && l <= at) exit
  t = trim($0)
  if (substr(t, 1, 3) == "```") fence = "```"
  else if (substr(t, 1, 3) == "~~~") fence = "~~~"
  next
}
{
  t = trim($0)
  if (substr(t, 1, 3) == fence) exit
  if (t != "" && substr(t, 1, 1) != "#") { steps[n++] = t }
}
END {
  if (n == 0) exit 1
  for (i = 0; i < n; i++) print steps[i]
}
' "$f"
