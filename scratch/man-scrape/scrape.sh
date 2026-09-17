#!/usr/bin/env bash
# Extract destructive flags from every man page that mentions destruction, via the
# local GLM. Writes files only; prints one progress line per program to its log.
#
# RESUMABLE: a program with a row file already present is skipped, so killing this
# and restarting loses at most one call.
#
# The grep pre-filter is not an optimisation, it is the design: a page that never
# says delete/overwrite/truncate has no destructive flag to find, and 2557 of 3664
# pages are in that set. The model is asked only about the 1107 that might.
set -u
OUT="$(cd "$(dirname "$0")" && pwd)"
EP=127.0.0.1:8080
ask() { # $1 program, $2 context
  jq -nc --arg p "For the program \`$1\`, list ONLY the flags or subcommands that DESTROY data — that delete, overwrite or truncate something that existed before the command ran. Writing a NEW file is not destruction. Reporting or dry-running is not destruction.

Answer as JSON lines, one object per flag, no prose and no fence:
{\"flag\":\"--x\",\"subcommand\":null,\"why\":\"one sentence somebody who does not know this flag can read\"}

If nothing here describes destruction, output nothing at all.

--- man $1 ---
$2
--- end ---" '{model:"glm", messages:[{role:"user",content:$p}], max_tokens:3000, temperature:0}' \
  | curl -s --max-time 300 -H 'Content-Type: application/json' -d @- "http://$EP/v1/chat/completions"
}

run_one() {
  local prog="$1" page="$2"
  [ -s "$OUT/rows/$prog.jsonl" ] && return 0
  [ -f "$OUT/rows/$prog.none" ] && return 0
  local ctx
  ctx=$(zcat -f "$page" 2>/dev/null | col -b 2>/dev/null \
        | grep -iE -B1 -A3 'delete|remove|destroy|overwrit|truncat|erase|unlink|purge' \
        | head -c 3000)
  [ -z "$ctx" ] && { : > "$OUT/rows/$prog.none"; return 0; }
  local t0 r content
  t0=$(date +%s)
  r=$(ask "$prog" "$ctx")
  printf '%s' "$r" > "$OUT/raw/$prog.json"
  content=$(jq -r '.choices[0].message.content // ""' <<<"$r" 2>/dev/null)
  local fin; fin=$(jq -r '.choices[0].finish_reason // "?"' <<<"$r" 2>/dev/null)
  # Keep only well-formed objects. A line the model wrapped in prose is dropped
  # rather than half-parsed -- a malformed row in a security table is worse than
  # a missing one.
  printf '%s\n' "$content" | grep -E '^\s*\{.*"flag".*\}\s*$' \
    | jq -c --arg p "$prog" 'select((.flag|type)=="string" and (.flag|length)>0) | . + {program:$p}' 2>/dev/null > "$OUT/rows/$prog.jsonl.tmp"
  if [ -s "$OUT/rows/$prog.jsonl.tmp" ]; then
    mv "$OUT/rows/$prog.jsonl.tmp" "$OUT/rows/$prog.jsonl"
  else
    rm -f "$OUT/rows/$prog.jsonl.tmp"; : > "$OUT/rows/$prog.none"
  fi
  printf '%s\t%s\t%ss\t%s\t%s\n' "$prog" "$fin" "$(( $(date +%s) - t0 ))" \
    "$(wc -l < "$OUT/rows/$prog.jsonl" 2>/dev/null || echo 0)" "$page" >> "$OUT/progress.tsv"
}

# Build the work list: priority names first, then everything else.
: > "$OUT/worklist.tsv"
declare -A seen
while read -r p; do
  [ -z "$p" ] && continue
  f=$(man -w "$p" 2>/dev/null | head -1)
  [ -n "$f" ] && [ -z "${seen[$p]:-}" ] && { echo -e "$p\t$f" >> "$OUT/worklist.tsv"; seen[$p]=1; }
done < "$OUT/priority.txt"
for f in $(find /usr/share/man/man1 /usr/share/man/man8 -type f 2>/dev/null | sort); do
  p=$(basename "$f"); p=${p%.gz}; p=${p%.*}
  [ -n "${seen[$p]:-}" ] && continue
  seen[$p]=1
  echo -e "$p\t$f" >> "$OUT/worklist.tsv"
done

total=$(wc -l < "$OUT/worklist.tsv")
i=0
while IFS=$'\t' read -r prog page; do
  i=$((i+1))
  run_one "$prog" "$page"
  [ $((i % 25)) -eq 0 ] && echo "  $i/$total done, $(ls "$OUT"/rows/*.jsonl 2>/dev/null | wc -l) programs with rows"
done < "$OUT/worklist.tsv"
echo "FINISHED $i/$total"
