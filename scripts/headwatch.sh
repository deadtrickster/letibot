#!/usr/bin/env bash
# Capture the two heads' panes every second and keep every frame, to see what a head draws.
#   letibot  tmux 1:2  - this session's head
#   leticl   tmux 1:4  - the reference head
# One file per sample, named by the second, so a restart shows as the frame where the pane changes.
set -u

OUT="${OUT:-/tmp/headwatch}"
SECONDS_TO_RUN="${SECONDS_TO_RUN:-5400}" # 90 minutes; the job outlives most conversations
INTERVAL="${INTERVAL:-1}"

mkdir -p "$OUT/letibot" "$OUT/leticl"
: >"$OUT/index.tsv"

end=$(($(date +%s) + SECONDS_TO_RUN))
n=0
while [ "$(date +%s)" -lt "$end" ]; do
  ts=$(date +%s)
  n=$((n + 1))

  # The todo pane is opened by the head's own key, so a plain capture only sees it while it is up.
  # What is captured every second is the WHOLE pane, which is what a reader sees; the pane's
  # presence is then a fact about the sample rather than something this script arranges.
  lb=$(tmux capture-pane -p -t 1:2 2>/dev/null) || lb=""
  lc=$(tmux capture-pane -p -t 1:4 2>/dev/null) || lc=""

  printf '%s\n' "$lb" >"$OUT/letibot/$ts.txt"
  printf '%s\n' "$lc" >"$OUT/leticl/$ts.txt"
  printf '%s\t%s\t%s\t%s\n' "$n" "$ts" "$(printf '%s' "$lb" | wc -c)" "$(printf '%s' "$lc" | wc -c)" >>"$OUT/index.tsv"

  sleep "$INTERVAL"
done
echo "sampled $n frames into $OUT"
