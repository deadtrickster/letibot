#!/usr/bin/env bash
# **A session above its repositories: the queue serves them, holds an entry for its gate, and the
# project's agent is told on its own.**
#
# The operator, 2026-10-09: *"agent can be running inside repo or level above it"*, *"let main
# project agent manage it"*. The session's root is a plain directory with a repository `app` in
# it; an entry from `app` is reviewed (the scripted gatekeeper accepts), held because `app` has no
# gate on main, and the session's model is woken with the notice and asks `merge_gate` about
# `app` without being prompted. Then the gate lands on `app`'s main and the entry lands too.
. "$(dirname -- "${BASH_SOURCE[0]}")/../lib.sh"
[ -x "$ROOT/target/release/harnessd" ] || { echo "acceptance: cargo build --release first"; exit 2; }
gitq() { git -c user.name=t -c user.email=t@t -c commit.gpgsign=false "$@"; }

fake_model
# The session root: a directory that is NOT a repository, with one inside it.
mkdir -p "$WORK/ws/app"
(cd "$WORK/ws/app" && git init -q -b main && printf 'test:\n\techo ok\n' >Makefile &&
  git add -A && gitq commit -qm init)
SESSION_UP=1
{
  echo '#!/usr/bin/env bash'
  declare -p ROOT WORK CONFIG
  declare -f session_env
  printf 'cd %q && session_env leticode --new\n' "$WORK/ws"
} >"$WORK/start.sh"
_t new-session -d -s acc -x "$COLS" -y "$ROWS" "bash $(printf '%q' "$WORK/start.sh")"

spec "the session is up, a level above its repository"
TIMEOUT=30 wait_for "Type a question"
settle

STORE="$WORK/home/.local/share/letibot/sessions.db"
spec "an entry from app, on a branch in a worktree"
WT="$WORK/ws/app/.claude/worktrees/agent-above"
(cd "$WORK/ws/app" && gitq worktree add -q "$WT" -b agent/above && cd "$WT" &&
  echo change >change.txt && git add -A && gitq commit -qm change)
BASE="$(git -C "$WORK/ws/app" rev-parse main)"
ROOT_SESSION="$(sqlite3 "$STORE" "select id from session where parent_session_id is null order by created_at desc limit 1")"
NOW=$(($(date +%s) * 1000))
sqlite3 "$STORE" "insert into merge_queue (id, session_id, branch, base_sha, priority, needs_json, state, brief, evidence, created_ms, updated_ms, worktree) values ('above', '$ROOT_SESSION', 'agent/above', '$BASE', 'subagent', '[]', 'waiting', 'add change.txt', '', $NOW, $NOW, '$WT')"
pass "queued from $ROOT_SESSION"

spec "reviewed, then held for app's gate"
ev=""
for _ in $(seq 1 60); do
  ev="$(sqlite3 "$STORE" "select evidence from merge_queue where id = 'above'")"
  case "$ev" in "waiting for a merge gate"*) break ;; esac
  sleep 0.5
done
case "$ev" in "waiting for a merge gate"*"/app"*) pass "held: $ev" ;; *) fail "evidence: $ev" ;; esac
[ "$(sqlite3 "$STORE" "select decision from merge_review where entry_id = 'above'")" = accept ] &&
  pass "the gatekeeper accepted it" || fail "no verdict"

spec "the session's model was told, and asked merge_gate about app"
TIMEOUT=30 wait_for "parent: offered the operator the gate choices."
grep -q '"move": \["call", "merge_gate"\]' "$WORK/model.log" && pass "merge_gate was called" || fail "merge_gate was not called"
case "$(cat "$WORK/model.log")" in *"merge gate for \`app\`"*"make test"*) pass "it listed app's choices" ;; *) fail "model.log: $(tail -2 "$WORK/model.log")" ;; esac

spec "the gate lands on app's main, and so does the entry"
(cd "$WORK/ws/app" && printf '## Merge gate\n\n```sh\ntest -f change.txt\n```\n' >AGENTS.md &&
  git add AGENTS.md && gitq commit -qm gate)
st=""
for _ in $(seq 1 60); do
  st="$(sqlite3 "$STORE" "select state from merge_queue where id = 'above'")"
  [ "$st" = landed ] && break
  sleep 0.5
done
[ "$st" = landed ] && pass "landed" || fail "state: $st — $(sqlite3 "$STORE" "select evidence from merge_queue where id = 'above'")"
git -C "$WORK/ws/app" log --oneline -1 main | grep -q change && pass "app's main has the change" || fail "main: $(git -C "$WORK/ws/app" log --oneline -3 main)"

done_spec
