#!/usr/bin/env bash
# **A merge-queue entry is reviewed by a gatekeeper subagent, and the verdict lands on its row.**
#
# The operator, 2026-10-09: entries had waited ten hours and more, because the queue asked a
# reviewer nobody could start. Now the review runs as a hidden child of the session the entry
# came from. Driven end to end on a real daemon: the queue's ring, the host's spawn, the child's
# turn on the scripted model (`GATEKEEPER` in fakemodel.py), and the verdict written back.
# Local only: it runs the release daemon with a model server of its own.
. "$(dirname -- "${BASH_SOURCE[0]}")/../lib.sh"

[ -x "$ROOT/target/release/harnessd" ] || { echo "acceptance: cargo build --release first"; exit 2; }

fake_model
start_session --new

spec "the session is up"
TIMEOUT=30 wait_for "Type a question"
settle

STORE="$WORK/home/.local/share/letibot/sessions.db"
spec "a branch to review, and its entry in the queue"
(cd "$WORK/ws" && git branch -m main 2>/dev/null; git checkout -q -b agent/gk-e2e &&
  echo change >change.txt && git add -A &&
  git -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -qm change &&
  git checkout -q main)
BASE="$(cd "$WORK/ws" && git rev-parse main)"
ROOT_SESSION="$(sqlite3 "$STORE" "select id from session where parent_session_id is null order by created_at desc limit 1")"
[ -n "$ROOT_SESSION" ] && pass "the root session is $ROOT_SESSION" || fail "no root session in the store"
NOW=$(($(date +%s) * 1000))
sqlite3 "$STORE" "insert into merge_queue (id, session_id, branch, base_sha, priority, needs_json, state, brief, evidence, created_ms, updated_ms) values ('gk-e2e', '$ROOT_SESSION', 'agent/gk-e2e', '$BASE', 'subagent', '[]', 'waiting', 'add change.txt', '', $NOW, $NOW)"

spec "the gatekeeper answers, and the verdict is on the row"
verdict=""
for _ in $(seq 1 60); do
  verdict="$(sqlite3 "$STORE" "select decision from merge_review where entry_id = 'gk-e2e' and answered_ms is not null")"
  [ -n "$verdict" ] && break
  sleep 0.5
done
[ "$verdict" = "accept" ] && pass "the verdict is accept" || fail "verdict: '${verdict}' (row: $(sqlite3 "$STORE" "select * from merge_review"))"
reasons="$(sqlite3 "$STORE" "select reasons_json from merge_review where entry_id = 'gk-e2e'")"
case "$reasons" in *"read agent/gk-e2e"*) pass "the reviewer named the branch" ;; *) fail "reasons: $reasons" ;; esac
host="$(sqlite3 "$STORE" "select session_id from merge_review where entry_id = 'gk-e2e'")"
[ "$host" = "$ROOT_SESSION" ] && pass "hosted by the root session" || fail "hosted by '$host'"
grep -q '"brief": "You are the gatekeeper.' "$WORK/model.log" && pass "the model was asked as the gatekeeper" || fail "the model never saw the gatekeeper's brief"

spec "the reviewer is not one of the session's subagents"
expect_not "subagent running"
expect_not "subagents running"
expect_not "You are the gatekeeper"

done_spec
