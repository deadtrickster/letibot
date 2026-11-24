#!/bin/sh
# gate-steps.sh [--quick|--full|--merge] [FILE]: print one tier of the gate's steps out of an
# AGENTS.md (FILE, or stdin), one per line; exit 1 when it declares none. The reading of
# `gate_section`/`parse_gates` (tools/src/gatekeeper.rs), held to it by
# `gate_steps_sh_reads_what_the_queue_reads`. `--merge` (the default) is what a project with one
# section declares; `--quick` and `--full` resolve their fallbacks the way the queue does.
set -u
tier=merge
case ${1:-} in
  --quick | --full | --merge)
    tier=${1#--}
    shift
    ;;
esac
[ $# -le 1 ] || {
  echo "usage: gate-steps.sh [--quick|--full|--merge] [FILE]" >&2
  exit 2
}
# Read once, so stdin works: the three sections are three readings of the same text.
doc=$(cat -- "${1:--}") || exit 2
# One heading's first fenced block, with the queue's rules: any level, any case; a block whose
# info string names something other than a shell is a sample and is skipped; the section ends at
# the next heading of the same level or higher, and a `#` line inside the block is a comment.
section() {
  printf '%s\n' "$doc" | awk -v want="$1" '
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
function shellinfo(info) {
  info = tolower(trim(info))
  sub(/[ \t,].*$/, "", info)
  if (info == "" || info == "sh" || info == "shell" || info == "bash" || info == "zsh" ||
      info == "console" || info == "shell-session") return 1
  return 0
}
function title(line, t, n) {
  n = level(line)
  t = line
  sub(/^[ \t]+/, "", t)
  t = trim(substr(t, n + 1))
  sub(/#+$/, "", t)
  return tolower(trim(t))
}
BEGIN { at = 0; fence = ""; take = 1; n = 0 }
at == 0 {
  l = level($0)
  if (l > 0 && title($0) == want) at = l
  next
}
fence == "" {
  l = level($0)
  if (l > 0 && l <= at) exit
  t = trim($0)
  if (substr(t, 1, 3) == "```") { fence = "```"; take = shellinfo(substr(t, 4)) }
  else if (substr(t, 1, 3) == "~~~") { fence = "~~~"; take = shellinfo(substr(t, 4)) }
  next
}
{
  t = trim($0)
  if (substr(t, 1, 3) == fence) exit
  if (take && t != "" && substr(t, 1, 1) != "#") { steps[n++] = t }
}
END {
  if (n == 0) exit 1
  for (i = 0; i < n; i++) print steps[i]
}
'
}
merge=$(section "merge gate")
quick=$(section "quick gate")
full=$(section "full gate")
# A tier nobody wrote is not a tier nobody judges: it is the merge gate, else the one that is
# there. The queue's order, so a hand gate and a queue gate run the same steps.
case $tier in
  quick)
    [ -n "$quick" ] || quick=$merge
    [ -n "$quick" ] || quick=$full
    steps=$quick
    ;;
  full)
    [ -n "$full" ] || full=$merge
    [ -n "$full" ] || full=$quick
    steps=$full
    ;;
  *) steps=$merge ;;
esac
[ -n "$steps" ] || exit 1
printf '%s\n' "$steps"
