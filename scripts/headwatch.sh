#!/usr/bin/env bash
# **Read the two heads every second and keep every frame.**
#
# The operator's instrument: a job that samples the terminal rather than the logs, because what is
# being investigated is what a HEAD DRAWS. A log can say a todo row exists; only the screen says
# whether it is on the pane after a restart.
#
# Two panes, so the comparison is like-for-like at the same instant:
#   letibot  tmux 1:2  — this session's own head
#   leticl   tmux 1:4  — the other head, which is the reference for what persistent todos look like
#
# Each sample is one file, named by the second it was taken, so a restart can be located by looking
# for the frame where the pane changes. `tmux capture-pane -p` gives the pane's TEXT without the
# escapes — enough to see a todo pane, and cheap enough to run for an hour.
set -u

OUT="${OUT:-/tmp/headwatch}"
SECONDS_TO_RUN="${SECONDS_TO_RUN:-5400}"   # 90 minutes; the job outlives most conversations
INTERVAL="${INTERVAL:-1}"

mkdir -p "$OUT/letibot" "$OUT/leticl"
: > "$OUT/index.tsv"

end=$(( $(date +%s) + SECONDS_TO_RUN ))
n=0
while [ "$(date +%s)" -lt "$end" ]; do
    ts=$(date +%s)
    n=$((n + 1))

    # The todo pane is opened by the head's own key, so a plain capture only sees it while it is up.
    # What is captured every second is the WHOLE pane, which is what a reader sees; the pane's
    # presence is then a fact about the sample rather than something this script arranges.
    lb=$(tmux capture-pane -p -t 1:2 2>/dev/null) || lb=""
    lc=$(tmux capture-pane -p -t 1:4 2>/dev/null) || lc=""

    printf '%s\n' "$lb" > "$OUT/letibot/$ts.txt"
    printf '%s\n' "$lc" > "$OUT/leticl/$ts.txt"
    printf '%s\t%s\t%s\t%s\n' "$n" "$ts" "$(printf '%s' "$lb" | wc -c)" "$(printf '%s' "$lc" | wc -c)" >> "$OUT/index.tsv"

    sleep "$INTERVAL"
done
echo "sampled $n frames into $OUT"
