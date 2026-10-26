#!/usr/bin/env bash
# **The launcher's model-to-endpoint lookup, run for real against stub binaries** — and the
# proof that it no longer GATES on a model: nothing serving and no provider starts, and the
# head runs (the operator, 2026-10-18: *"idk why i even have to do --provider. horrible ux -
# we already have models chooser and keys asker"*; the head asks now, see `app::reach`).
#
# `scripts/letibot` decides where a run's turns go before any Rust runs, so nothing in
# `cargo test` reaches it. This runs it one-shot (`letibot "q"` execs harnessd) with a
# stub harnessd that prints its argv, a throwaway HOME and runtime dir, and a
# providers.toml written per case, and reads back what was started or refused.
# Nothing listens anywhere, so a case that needed the local model server sees none.
#
#   tests/launcher.sh            # from the repo root; exits non-zero on a failure
set -u
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
LAUNCHER="$ROOT/scripts/letibot"
T="$(mktemp -d)"
# **And the daemon stubs this script started go with it.** They are `$T/bin/harnessd` — a python
# stub that binds a socket under `$T` and sleeps for ever — so the temp dir IS their identity:
# nothing else on the box has a command line naming it. A case that fails before its own `kill`
# is exactly how a test suite leaves daemons behind, which is the defect this file is otherwise
# about (the operator's 2026-10-26 third report), so the cleanup does not depend on a case
# reaching its last line.
cleanup() {
  ps -eo pid=,args= 2>/dev/null | awk -v d="$T/bin/harnessd" 'index($0, d) { print $1 }' \
    | while read -r _stub; do kill "$_stub" 2>/dev/null || true; done
  rm -rf "$T"
}
trap cleanup EXIT

mkdir -p "$T/bin" "$T/home/.config/letibot" "$T/run" "$T/ws"
cat > "$T/bin/harnessd" <<'STUB'
#!/usr/bin/env bash
printf 'harnessd'; printf ' %q' "$@"; printf '\n'
STUB
cp "$T/bin/harnessd" "$T/bin/letibot-tui"
chmod +x "$T/bin/harnessd" "$T/bin/letibot-tui"

fails=0
# **The launcher's liveness reader has to be on the PATH this harness gives it.** It is
# `ss` on Linux (`/usr/bin`) and `lsof` on macOS (`/usr/sbin`, and nowhere else) — so a
# harness PATH of `/usr/bin:/bin` leaves a Mac's launcher unable to see ANY socket: `live`
# is false, every case takes the start-a-daemon branch, and nothing that depends on a
# listening daemon can be tested at all. MEASURED by CI's own macos job, 2026-10-09, on the
# protocol-skew cases below: four failures that were the harness's PATH and not the
# launcher.
RUN_PATH="/usr/bin:/bin"
for d in /usr/sbin /sbin; do [ -d "$d" ] && RUN_PATH="$RUN_PATH:$d"; done
# run NAME PROVIDERS_TOML [ARGS...] -- the launcher's combined output for one case.
run() {
  local toml="$1"; shift
  printf '%s' "$toml" > "$T/home/.config/letibot/providers.toml"
  (cd "${RUN_WS:-$T/ws}" && env -i PATH="$RUN_PATH" HOME="$T/home" XDG_RUNTIME_DIR="$T/run" \
    LETIBOT_BIN="$T/bin" LETIBOT_ENDPOINT= STUB_ARGV="$T/argv.log" \
    "$(command -v bash)" "$LAUNCHER" "$@" 2>&1 </dev/null)
}
expect() { # expect NAME OUTPUT PATTERN -- the output must match
  if printf '%s' "$2" | grep -qE -- "$3"; then echo "ok   $1"
  else echo "FAIL $1: wanted /$3/ in:"; printf '%s\n' "$2" | sed 's/^/     /'; fails=$((fails + 1)); fi
}
refuse() { # refuse NAME OUTPUT PATTERN -- the output must not match
  if printf '%s' "$2" | grep -qE -- "$3"; then
    echo "FAIL $1: did not want /$3/ in:"; printf '%s\n' "$2" | sed 's/^/     /'; fails=$((fails + 1))
  else echo "ok   $1"; fi
}

DEFAULT_ONLY='[deepseek]

[default]
provider = "deepseek"
'
out="$(run "$DEFAULT_ONLY" "one question")"
refuse "a [default] provider with no local model block is not refused" "$out" "no endpoint for this run"
expect "  ...and the daemon is started" "$out" "^harnessd "
refuse "  ...with no local endpoint invented for it" "$out" "--endpoint"

out="$(run "$DEFAULT_ONLY" --status)"
expect "--status says there is no local home rather than refusing" "$out" "^local +none"

# **No provider, no local model block, nothing serving: started, not refused.** The daemon
# keeps its own default endpoint, and is handed no vocabulary to send ids from — a daemon with
# no model chosen refuses a TURN by name, and the head's chooser is the way on.
out="$(run '' "one question")"
refuse "no provider and no local model block is not refused" "$out" "no endpoint for this run"
refuse "  ...nor refused for nothing serving" "$out" "nothing serving"
expect "  ...and the daemon is started" "$out" "^harnessd "
refuse "  ...with no endpoint invented for it" "$out" "--endpoint"
refuse "  ...and no vocabulary to send ids from" "$out" "--vocab"

out="$(run '' --status)"
refuse "--status with no provider and no home reports rather than refusing" "$out" "no endpoint for this run"
expect "  ...and says a model is asked for" "$out" "^local +none.*asks for a model on first use"

# **A model asked for by name with no home is SAID, and still started** — the flag meant
# something the file does not carry, which is worth a sentence and not a refusal.
out="$(run '' --dense "one question")"
expect "a flag's model with no home is named" "$out" "no endpoint for this run: --dense asked"
expect "  ...and the daemon is started anyway" "$out" "^harnessd "

# **`--host NAME` is forwarded to the daemon, value and all** — the session's tools run on
# that machine (`docs/remote-hosts.md`). The launcher does not resolve the name: the daemon
# reads the operator's `hosts.toml` and refuses an unknown one by naming the ones it knows,
# so the shell's whole job here is to pass the pair through intact (and to consume the value
# so the name is not mistaken for the prompt).
out="$(run '' --host lab2x1 "one question")"
expect "--host is passed to the daemon with its name" "$out" "^harnessd .*--host lab2x1"
refuse "  ...and the name is not taken for the prompt" "$out" "--prompt lab2x1"

LOCAL='[model."local.flash"]
url = "http://127.0.0.1:59999"
model = "qwen-3.8-flash-next"

[default]
provider = "deepseek"
'
out="$(run "$LOCAL" "one question")"
expect "a local model with a home passes its endpoint on" "$out" "--endpoint 127.0.0.1:59999"

# **The health gate is gone too**: a home whose server is not answering, and no provider —
# what died on *"nothing serving on …"* (or asked *"1) deepseek 2) glm 3) grok"* at a
# terminal). Nothing listens on 59999 here.
LOCAL_ONLY='[model."local.flash"]
url = "http://127.0.0.1:59999"
model = "qwen-3.8-flash-next"
'
out="$(run "$LOCAL_ONLY" "one question")"
refuse "a home with nothing serving and no provider is not refused" "$out" "nothing serving"
expect "  ...and the daemon is started on that home" "$out" "^harnessd .*--endpoint 127.0.0.1:59999"

# ---------------------------------------------------------------------------
# **A daemon that is LISTENING but does not speak this build's protocol is refused
# with the sentence that names the way out** — not handed a head that dies on the
# daemon's bare refusal line.
#
# MEASURED 2026-10-09, in the operator's own terminal: `leticode --continue` against a
# daemon up since 09-21 printed
#
#     letibot: attached to harnessd for … (pid 2076966), up since 2026-09-21T07:16:16
#     letibot-tui: the daemon refused the attach: protocol version 36, this daemon speaks 22
#
# and stopped there: no `--stop`, no socket, nothing about the warm session that had just
# become unreachable. `version_skew_or_continue` already held the right sentence and ran
# only in the branch that STARTS a daemon, so a daemon already listening never met it.
#
# This needs two things the other cases do not: a socket something is really LISTENING on
# (`live` is `ss`, not a file test, because a daemon's shutdown unlinks the file while the
# process keeps serving), and a head that refuses the way an older daemon's would.
key12() { if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi | cut -c1-12; }
if command -v python3 >/dev/null 2>&1; then
  cat > "$T/bin/letibot-tui" <<'STUB'
#!/usr/bin/env bash
for a in "$@"; do
  if [ "$a" = "--probe" ]; then
    echo "letibot-tui: the daemon refused the attach: protocol version 36, this daemon speaks 22" >&2
    exit 1
  fi
done
echo "letibot-tui: THE HEAD RAN"
STUB
  chmod +x "$T/bin/letibot-tui"
  WS_P="$(cd "$T/ws" && pwd -P)"
  # **`$T/run/letibot`, not `$T/run`**: the launcher's RUNDIR is
  # `$XDG_RUNTIME_DIR/letibot`, so a socket one level up is a socket nobody looks at —
  # which is exactly what the first draft of this case did, and the launcher went on to
  # start a daemon over a socket it could not see.
  mkdir -p "$T/run/letibot"
  SOCK_P="$T/run/letibot/$(printf '%s' "$WS_P" | key12).sock"
  python3 -c 'import socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.bind(sys.argv[1])
s.listen(2)
time.sleep(120)' "$SOCK_P" &
  _listener=$!
  sleep 0.3

  # **Can this box show the launcher a listening socket at all?** The same reader the
  # launcher uses, asked here first, so that a harness which cannot answer that question
  # SKIPS LOUDLY rather than reporting four failures about the launcher. A check that did
  # not run must not look like one that did — the same rule the GGUF-gated tests keep.
  listens() { # listens SOCKET-PATH
    if [ "$(uname -s)" = Darwin ]; then
      lsof -nP -U 2>/dev/null | grep -qF "$1"
    else
      ss -lxH 2>/dev/null | grep -qF "$1"
    fi
  }
  if ! listens "$SOCK_P"; then
    echo "skip the protocol-skew cases: nothing here reports a listening socket"
    echo "     (ss on Linux, lsof on macOS) so the launcher cannot be shown one either."
    echo "     NOTHING WAS CHECKED — this is a fact about the harness, not a pass."
    kill "$_listener" 2>/dev/null || true
  else
    out="$(run "$DEFAULT_ONLY")"
    expect "a daemon speaking another protocol is refused, not attached to" "$out" "does not speak this build's protocol"
    expect "  ...and the sentence names the way out" "$out" "letibot --stop"
    expect "  ...and says nothing is lost" "$out" "on disk"
    refuse "  ...and no head is handed to it" "$out" "THE HEAD RAN"

    out="$(run "$DEFAULT_ONLY" --attach)"
    expect "--attach refuses in the same words" "$out" "does not speak this build's protocol"
    refuse "  ...and does not claim to have attached" "$out" "attached to harnessd"

    kill "$_listener" 2>/dev/null || true
  fi
else
  echo "skip the protocol-skew cases: no python3 to listen on a unix socket"
fi

# **And the reader that feeds the stale line.** `pid_started` read
# `stat -c %Y "/proc/<pid>"` — a procfs DIRECTORY's mtime, which is not a process's start
# time and is not even per-process. MEASURED 2026-10-09: three daemons up 17, 9 and 4 days
# reported the SAME value (10-08 21:20), while `ps -o lstart=` and `/proc/<pid>/stat`'s own
# starttime agreed with each other on all three, to the second.
#
# The reading is asserted AT THE SOURCE because that is where the mistake lives and because
# a young process cannot expose it: a freshly started process's directory mtime DOES equal
# its start time, which is how the wrong reader passed every check it was ever given.
# `/proc/<pid>/stat`'s field 22 is the reader; see `pid_started`.
#
# **Comments are stripped before the grep**, because the reader's own comment QUOTES the
# wrong line to say what it replaced — a guard that cannot tell the two apart is red on
# the fix itself, which is how this guard first ran.
if grep -v '^[[:space:]]*#' "$LAUNCHER" | grep -q 'stat -c %Y "/proc/'; then
  echo "FAIL a process start time is read from a /proc DIRECTORY's mtime:"
  grep -v '^[[:space:]]*#' "$LAUNCHER" | grep -n 'stat -c %Y "/proc/' | sed 's/^/     /'
  fails=$((fails + 1))
else
  echo "ok   no reader takes a /proc directory's mtime for a process start time"
fi

# **A daemon that is still going is WAITED FOR, not started over.**
#
# The operator's ten `--continue` refusals, constructed: a daemon asked to stop unlinks its
# socket file and drops its listener within a poll of the signal while it finishes the turn
# it was holding — so in that window every liveness test this box has says *no daemon here*,
# the launcher starts one, and the FOLDER LOCK refuses it (`already served by a live daemon
# (pid N) ... stop the one named here`). The record beside the socket is the fact that
# survives that window, and this is what the launcher does with it.
#
# Constructed rather than raced: the *daemon* is a stub that listens (so the start path runs
# its real `live` poll), and it goes when this test lets it go.
if command -v python3 >/dev/null 2>&1; then
  cat > "$T/bin/harnessd" <<'STUB'
#!/usr/bin/env python3
import os, socket, sys, time
p = sys.argv[sys.argv.index("--socket") + 1]
try: os.unlink(p)
except FileNotFoundError: pass
s = socket.socket(socket.AF_UNIX); s.bind(p); s.listen(1)
print("harnessd", *sys.argv[1:], flush=True)
while True: time.sleep(0.2)
STUB
  chmod +x "$T/bin/harnessd"

  # **The folder as the launcher names it — resolved.** macOS's `mktemp -d` is under `/var`,
  # a symlink to `/private/var`, and the launcher keys its record by `pwd -P`; keyed by
  # `$T/ws` the record sat where the launcher never looked, so it started a daemon instead of
  # waiting (CI run 39127760909, macOS: "starting one ... /private/var/folders/.../ws").
  ws_p="$(cd "$T/ws" && pwd -P)"
  key="$(printf '%s' "$ws_p" | key12)"
  mkdir -p "$T/run/letibot"
  sock="$T/run/letibot/$key.sock"
  # **A daemon of THIS folder, on its way out**: its argv names our socket, which is how the
  # launcher tells it from a recycled pid that is somebody else's harnessd, and it lives
  # until this test lets it go.
  bash -c 'while [ ! -e "$1" ]; do sleep 0.05; done' _ "$T/let-go" bash --socket "$sock" &
  going=$!
  printf '{"pid": %s, "workspace": "%s", "socket": "%s"}' "$going" "$ws_p" "$sock" \
    > "$T/run/letibot/$key.json"

  ( run "$DEFAULT_ONLY" --session s-x > "$T/going.out" 2>&1 ) &
  launched=$!
  # Let it reach the wait, then let the daemon go. The bound is the point of the case: it
  # must not have started a second daemon in that window.
  sleep 1
  : > "$T/let-go"
  wait "$launched" 2>/dev/null || true
  out="$(cat "$T/going.out")"
  expect "a daemon that is still going is waited for" "$out" "is still going - waiting for it"
  refuse "  ...and nothing is started over it" "$out" "already served by a live daemon"
  expect "  ...and the session opens once it is gone" "$out" "THE HEAD RAN"

  # **The launcher's endpoint gate is gone: nothing serving and no provider, and the head
  # runs.** The case the operator met: `leticode` on a box with no local model and no
  # `--provider`, refused with *"nothing serving on 127.0.0.1:8080 ... --provider
  # deepseek|glm|grok"*. A folder of its own, so the waiting case's record is not this one's.
  mkdir -p "$T/bare"
  bare_out="$(RUN_WS="$T/bare" run '')"
  refuse "a bare box is not refused for nothing serving" "$bare_out" "nothing serving"
  refuse "  ...nor asked to pick a provider in the shell" "$bare_out" "1\\) deepseek"
  refuse "  ...nor for a missing model home" "$bare_out" "no endpoint for this run"
  expect "  ...and the head runs" "$bare_out" "THE HEAD RAN"
  bare_key="$(printf '%s' "$(cd "$T/bare" && pwd -P)" | key12)"
  kill "$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    "$T/run/letibot/$bare_key.json" 2>/dev/null | head -1)" 2>/dev/null || true

  # The stub daemon the launcher started is a real one for this case; it does not exit.
  # (Read with the same sed the launcher's own `json_num` is, because that is a function of
  # the launcher and not of this harness.)
  kill "$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    "$T/run/letibot/$key.json" | head -1)" 2>/dev/null || true
  kill "$going" 2>/dev/null || true
else
  echo "skip the going-daemon case: no python3 to listen on a unix socket"
fi

# **`--stop --force N` stops daemon N, not this folder's** (TODO R92.23): only `$2` was read
# as the target, so `--force` in front of the number dropped it. One record, for another
# folder, numbered 1 by `--ls`; nothing listens anywhere, so the stop finds no daemon THERE.
mkdir -p "$T/other" "$T/run/letibot"
OTHER_P="$(cd "$T/other" && pwd -P)"
rm -f "$T/run/letibot/"*.json
printf '{"workspace": "%s", "socket": "%s", "pid": 999999999}' "$OTHER_P" \
  "$T/run/letibot/$(printf '%s' "$OTHER_P" | key12).sock" \
  > "$T/run/letibot/$(printf '%s' "$OTHER_P" | key12).json"
out="$(run "$DEFAULT_ONLY" --stop --force 1)"
expect "--stop --force N retargets N" "$out" "no daemon for $OTHER_P"
out="$(run "$DEFAULT_ONLY" --stop 1 --force)"
expect "  ...as --stop N --force always did" "$out" "no daemon for $OTHER_P"
out="$(run "$DEFAULT_ONLY" --stop 1 2)"
expect "  ...and two targets are refused" "$out" "takes one daemon"
rm -f "$T/run/letibot/"*.json

# ---------------------------------------------------------------------------
# **WHAT A START OPENS, AND WHAT A HEAD'S EXIT MAY NOT DO** — the operator's 2026-10-26 reports,
# at the launcher's own level.
#
# 1. *"if i just start leticode in stroppy-io, without --continue it still attaches to the latest
#    session, like with --continue"* — so a start must pass NO `--session` (the daemon mints a
#    fresh id) and only `--continue`/`--session` may name one. Pinned on the daemon's ARGV,
#    because that is the whole of what decides it: the launcher does not, and must not, look for
#    the newest.
# 2. *"and when i exited it took the other stroppy-io harnessd with it"* — the head that exits is
#    a stub here; the daemon it was attached to must still be alive and listening afterwards, and
#    `--new` against a live folder must not start a second one.
# 3. And `--new` with NO daemon up must not ask the head for a session of its own: the start
#    already opened the fresh one, and a second leaves the folder's daemon holding an empty
#    first session that every later plain attach lands on.
if command -v python3 >/dev/null 2>&1; then
  # A daemon stub that LISTENS (so the launcher's start poll succeeds), writes every invocation's
  # argv down, and stays until this test kills it.
  cat > "$T/bin/harnessd" <<'STUB'
#!/usr/bin/env python3
import os, socket, sys, time
with open(os.environ["STUB_ARGV"], "a") as log:
    log.write("harnessd " + " ".join(sys.argv[1:]) + "\n")
if "--latest-session" in sys.argv:
    print("s-latest")
    sys.exit(0)
p = sys.argv[sys.argv.index("--socket") + 1]
try:
    os.unlink(p)
except FileNotFoundError:
    pass
s = socket.socket(socket.AF_UNIX)
s.bind(p)
s.listen(4)
while True:
    time.sleep(0.2)
STUB
  # The head the operator quits: it records what it was handed and exits at once. `--probe` is the
  # launcher's own question and answers 0 ("a daemon here, speaking this build").
  cat > "$T/bin/letibot-tui" <<'STUB'
#!/usr/bin/env bash
case " $* " in *" --probe "*) exit 0 ;; esac
printf 'tui %s\n' "$*" >> "$STUB_ARGV"
echo "letibot-tui: THE HEAD RAN"
STUB
  chmod +x "$T/bin/harnessd" "$T/bin/letibot-tui"

  daemon_argv() { grep '^harnessd ' "$T/argv.log" 2>/dev/null || true; }
  head_argv() { grep '^tui ' "$T/argv.log" 2>/dev/null || true; }
  # **`printf '%s'`, with no newline**: the launcher hashes the workspace path with nothing
  # appended (`launcher.rs`'s `key`, and the `printf '%s' | sha256sum` the shell has always
  # used). A `$(pwd -P)` piped straight in carries a trailing newline and hashes a DIFFERENT
  # path — this case's first draft looked for a record under a key nobody writes.
  key_of() { printf '%s' "$(cd "$1" && pwd -P)" | key12; }
  pid_of() { # pid_of DIR -- the pid in that folder's record
    sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
      "$T/run/letibot/$(key_of "$1").json" 2>/dev/null | head -1
  }
  fresh() { : > "$T/argv.log"; rm -f "$T/run/letibot/"*.json; }
  stop_case() { # stop_case DIR -- this block's own daemon, and nothing else
    local p
    p="$(pid_of "$1")"
    [ -n "$p" ] && kill "$p" 2>/dev/null || true
  }

  # **1. A plain start opens a FRESH session.** No `--session` on the daemon's command line is
  # what makes `harnessd` mint an id; a launcher that "helpfully" passed the newest would be the
  # operator's report, and nothing else in the start would look wrong.
  mkdir -p "$T/plain"
  fresh
  out="$(RUN_WS="$T/plain" run "$DEFAULT_ONLY")"
  refuse "a plain start does not pass the newest session to the daemon it starts" \
    "$(daemon_argv)" "--session"
  expect "  ...and the head runs" "$out" "THE HEAD RAN"
  refuse "  ...and the head is given no --resume of its own" "$(head_argv)" "--resume"

  # **2. `--continue` is the verb that names one** — and it names it on the daemon's command line,
  # where the resume (and its hash-chain check) happens at startup.
  mkdir -p "$T/cont"
  fresh
  out="$(RUN_WS="$T/cont" run "$DEFAULT_ONLY" --continue)"
  expect "--continue starts the daemon ON the session it picked" "$(daemon_argv)" \
    "--session s-latest"
  expect "  ...and the head runs" "$out" "THE HEAD RAN"

  # **3. `--new` with no daemon opens ONE session, not two.** The start's first session IS the
  # fresh one, so the head must not also ask for one.
  mkdir -p "$T/new"
  fresh
  out="$(RUN_WS="$T/new" run "$DEFAULT_ONLY" --new)"
  refuse "a --new start does not name a session to resume" "$(daemon_argv)" "--session"
  refuse "  ...and the head is not asked for a second fresh session" "$(head_argv)" \
    "--new-session"
  expect "  ...and the head runs" "$out" "THE HEAD RAN"

  # **4. `--new` against a LIVE folder starts nothing and leaves the daemon alone** — and the head
  # exits at once, which is the operator's "when i exited it took the other harnessd with it".
  # The argv log is emptied; the RECORD is not, because it is how this case finds the pid that
  # must still be there (and the launcher's own `--stop` path would have kept it too).
  : > "$T/argv.log"
  out="$(RUN_WS="$T/new" run "$DEFAULT_ONLY" --new)"
  expect "--new against a live daemon attaches" "$out" "attached to harnessd"
  expect "  ...and asks the head for the session it was told to make" "$(head_argv)" \
    "--new-session"
  if [ "$(daemon_argv | wc -l | tr -d ' ')" = "0" ]; then
    echo "ok   ...and starts no second daemon for that folder"
  else
    echo "FAIL ...and starts no second daemon for that folder; it started:"
    daemon_argv | sed 's/^/     /'
    fails=$((fails + 1))
  fi
  live_pid="$(pid_of "$T/new")"
  sleep 0.3
  if kill -0 "$live_pid" 2>/dev/null && listens "$T/run/letibot/$(key_of "$T/new").sock"; then
    echo "ok   ...and the daemon survives the head's exit"
  else
    echo "FAIL ...and the daemon survives the head's exit: pid $live_pid is gone or its socket went"
    fails=$((fails + 1))
  fi

  # **5. `--continue ID` is `--session ID`, and anything else after `--continue` is refused.**
  #
  # MEASURED 2026-10-26: `leticode --continue s-1791394076177451000` ran a ONE-SHOT whose prompt
  # WAS the id, into the newest session — the operator's live conversation, which then acted on
  # the string. `--continue` takes no argument, so the id fell through to the one-shot branch.
  #
  # A session id is unambiguous and means the verb it names, so it is read as one — pinned here on
  # the daemon's ARGV (`--session s-…`, and never `--prompt`), because that is what decides which
  # session opens and whether a turn is run. The id forms are the store's own: `s-<digits>`, with
  # an optional transcript suffix `#tN`.
  for pair in "contid:s-1791394076177451000" "contid-t:s-1791394076177451000#t17"; do
    d="$T/${pair%%:*}"; want="${pair#*:}"
    mkdir -p "$d"
    fresh
    out="$(RUN_WS="$d" run "$DEFAULT_ONLY" --continue "$want")"
    expect "--continue $want names that session" "$(daemon_argv)" "--session $want( |\$)"
    expect "  ...and says what it read" "$out" \
      "--continue takes no argument; $want is a session id, so this is --session $want"
    refuse "  ...and runs no prompt with the id in it" "$(daemon_argv)" "--prompt"
    expect "  ...and the head runs" "$out" "THE HEAD RAN"
    refuse "  ...and the newest session was not asked for instead" "$(daemon_argv)" \
      "--session s-latest"
    stop_case "$d"
  done

  # An argument that is not a session id is a mistake about the verb, not a prompt: refused with
  # the sentence and the usage, and nothing started — a one-shot's prompt is the FIRST argument
  # and only the first.
  mkdir -p "$T/contbad"
  fresh
  out="$(RUN_WS="$T/contbad" run "$DEFAULT_ONLY" --continue please-reopen-it)"
  rc=$?
  expect "a stray argument after --continue is refused" "$out" \
    "--continue takes no argument, and 'please-reopen-it' is not a session id"
  expect "  ...with the usage line" "$out" "letibot --session ID"
  if [ "$rc" -ne 0 ]; then echo "ok   ...and exits non-zero"; else
    echo "FAIL ...and exits non-zero: exit $rc"; fails=$((fails + 1))
  fi
  if [ -z "$(daemon_argv)" ]; then echo "ok   ...and starts no daemon with it as a prompt"; else
    echo "FAIL ...and starts no daemon with it as a prompt:"; daemon_argv | sed 's/^/     /'
    fails=$((fails + 1))
  fi

  for d in "$T/plain" "$T/cont" "$T/new"; do stop_case "$d"; done
else
  echo "skip the start/--new cases: no python3 to listen on a unix socket"
fi

# ---------------------------------------------------------------------------
# **A DAEMON THAT IS SHUTTING DOWN IS NOT A DAEMON IGNORING SIGTERM** — the operator's
# 2026-10-26 report, at the launcher's own level. `letibot --stop` waited five seconds, printed
# *"NOT stopped: … (pid N) is ignoring SIGTERM after 5s"* about a daemon that was working through
# its orderly stop, and left the next `leticode` refused by the folder lock the still-exiting
# daemon held. The daemon's stop is bounded but not quick (the merge queues joined, five seconds
# of subagent threads), it prints one `  stopping: …` line per step, and the launcher now waits
# thirty seconds relaying those lines.
if command -v python3 >/dev/null 2>&1; then
  mkdir -p "$T/home/logs" "$T/run/letibot" "$T/slow" "$T/stuck"
  # A daemon on its way out: it says what it is waiting for on its own log — the path the
  # launcher reads (`$HOME/logs/harnessd-<key>.log`, keyed by the workspace it was told) — and
  # it takes SEVEN seconds to go, two more than the bound that used to call it stuck. It also
  # LISTENS, because the start case below needs a daemon that comes up.
  cat > "$T/bin/harnessd" <<'STUB'
#!/usr/bin/env python3
import hashlib, os, signal, socket, sys, time
ws = sys.argv[sys.argv.index("--workspace") + 1]
sock = sys.argv[sys.argv.index("--socket") + 1]
key = hashlib.sha256(ws.encode()).hexdigest()[:12]
log = os.path.join(os.environ["HOME"], "logs", "harnessd-" + key + ".log")
slow = "--stuck" not in sys.argv
s = socket.socket(socket.AF_UNIX)
try:
    os.unlink(sock)
except FileNotFoundError:
    pass
s.bind(sock)
s.listen(4)
def bye(_sig, _frm):
    with open(log, "a") as f:
        f.write("  stopping: the merge queues (a gate in flight is killed, its row put back in line)\n")
        f.write("  stopping: waiting up to 5s for subagent threads to leave\n")
    if slow:
        time.sleep(7)
        with open(log, "a") as f:
            f.write("  stopping: done\n")
        sys.exit(0)
    # A daemon that never goes: it is stuck INSIDE a step, which is what `--force` is for.
    time.sleep(600)
signal.signal(signal.SIGTERM, bye)
while True:
    time.sleep(0.2)
STUB
  chmod +x "$T/bin/harnessd"
  # The tui stub: `--stop` calls it for the interrupt and the start path for its probe — both
  # answer 0 and say nothing — and any other invocation is the head, which says it ran.
  cat > "$T/bin/letibot-tui" <<'STUB'
#!/usr/bin/env bash
for a in "$@"; do
  case "$a" in --probe|--interrupt-all) exit 0 ;; esac
done
echo "letibot-tui: THE HEAD RAN"
STUB
  chmod +x "$T/bin/letibot-tui"
  slow_daemon() { # slow_daemon DIR [EXTRA-ARG] -- a recorded daemon for that folder
    local dir="$1" extra="${2:-}" ws_p sock key
    ws_p="$(cd "$dir" && pwd -P)"
    key="$(printf '%s' "$ws_p" | key12)"
    sock="$T/run/letibot/$key.sock"
    env -i PATH="$RUN_PATH" HOME="$T/home" "$T/bin/harnessd" \
      --workspace "$ws_p" --socket "$sock" $extra >/dev/null 2>&1 &
    printf '{"pid": %s, "workspace": "%s", "socket": "%s"}' "$!" "$ws_p" "$sock" \
      > "$T/run/letibot/$key.json"
    echo "$!"
  }
  slow_daemon "$T/slow" >/dev/null
  out="$(RUN_WS="$T/slow" run "$DEFAULT_ONLY" --stop)"
  expect "a daemon that takes seven seconds to stop is waited for, not called stuck" \
    "$out" "^stopped "
  expect "  ...and the launcher relays what IT says it is waiting for" "$out" \
    "stopping: waiting up to 5s for subagent threads to leave"
  refuse "  ...and it is never called 'ignoring SIGTERM'" "$out" "ignoring SIGTERM"

  # **AND THE TIMEOUT STILL COMES, with the advice only after it.** A daemon wedged INSIDE a step
  # is exactly the case `--force` exists for; thirty seconds of patience is the difference
  # between a slow stop and a stuck one, so this case spends them.
  slow_daemon "$T/stuck" --stuck >/dev/null
  stuck_pid="$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    "$T/run/letibot/$(printf '%s' "$(cd "$T/stuck" && pwd -P)" | key12).json" | head -1)"
  out="$(RUN_WS="$T/stuck" run "$DEFAULT_ONLY" --stop)"
  expect "a daemon wedged in a step is given its whole thirty seconds" "$out" \
    "still running 30s after SIGTERM"
  expect "  ...its own last words are said" "$out" \
    "it last said: stopping: waiting up to 5s for subagent threads"
  expect "  ...and only THEN is --force offered" "$out" "sends SIGKILL"
  kill -9 "$stuck_pid" 2>/dev/null || true
  rm -f "$T/run/letibot/"*.json

  # **A daemon that is on its way out, for TWELVE seconds** — the ten-second wait the start used to
  # keep turned this into *"still holds this folder"* and a refusal to start. `bash -c 'sleep 12; :'`
  # and not `bash -c 'sleep 12'`: bash EXECs a single-command string, and the argv that names the
  # socket would be gone with it (measured here: the launcher found the pid and no socket in it).
  mkdir -p "$T/after"
  ws_p="$(cd "$T/after" && pwd -P)"
  key="$(printf '%s' "$ws_p" | key12)"
  sock="$T/run/letibot/$key.sock"
  bash -c 'sleep 12; :' _ bash --socket "$sock" &
  going=$!
  printf '{"pid": %s, "workspace": "%s", "socket": "%s"}' "$going" "$ws_p" "$sock" \
    > "$T/run/letibot/$key.json"
  out="$(RUN_WS="$T/after" run "$DEFAULT_ONLY")"
  expect "a daemon taking twelve seconds to go is waited for" "$out" \
    "is still going - waiting for it"
  refuse "  ...and the start never refuses on the old ten-second bound" "$out" "not taking heads"
  expect "  ...and it starts once that daemon is gone" "$out" "THE HEAD RAN"
  kill "$going" 2>/dev/null || true
  kill "$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    "$T/run/letibot/$key.json" 2>/dev/null | head -1)" 2>/dev/null || true
  rm -f "$T/run/letibot/"*.json
else
  echo "skip the stop and start-wait cases: no python3"
fi

# ---------------------------------------------------------------------------
# **A SESSION THAT LIVES ON A HOST** (`docs/remote-hosts.md` RH4): the verb passes through,
# and an attach goes to the forwarded socket instead of to this folder's daemon.
#
# The launcher never migrated anything itself — that is `letibot migrate` — but it is the thing
# that decides WHERE a head attaches, and the record in the rundir is the whole of what it
# reads. Both halves are pinned here.
cat > "$T/bin/letibot" <<'STUB'
#!/usr/bin/env bash
printf 'letibot'; printf ' %q' "$@"; printf '\n'
STUB
chmod +x "$T/bin/letibot"

out="$(run "$DEFAULT_ONLY" --migrate s-4242 --to lab2x1)"
expect "--migrate is handed to the migration verb with this box's store" "$out" \
  "^letibot migrate s-4242 --to lab2x1 --store "
refuse "  ...and the launcher never starts a daemon for it" "$out" "^harnessd "

out="$(run "$DEFAULT_ONLY" --reattach s-4242)"
expect "--reattach is the same verb's other spelling" "$out" \
  "^letibot migrate --reattach s-4242 --store "

# The record a move writes, beside this folder's own daemon records. Its socket does not
# exist here (nothing forwards in a test), so the launcher re-establishes first and THEN
# hands the head the forwarded socket — which is the order that makes a dropped forward
# invisible to the person attaching.
mkdir -p "$T/run/letibot"
# **A head stub that says what it was handed** — the last case above leaves one that only says
# it ran, and what this case is about is the ARGUMENTS.
cat > "$T/bin/letibot-tui" <<'STUB'
#!/usr/bin/env bash
printf 'head'; printf ' %q' "$@"; printf '\n'
STUB
chmod +x "$T/bin/letibot-tui"
printf '{"workspace": "/elsewhere/checkout/4242", "socket": "%s", "host": "lab2x1", "session": "s-4242"}' \
  "$T/run/letibot/migrated-s-4242.sock" > "$T/run/letibot/migrated-s-4242.json"
out="$(run "$DEFAULT_ONLY" --session s-4242)"
expect "a moved session's forward is re-established before the head runs" "$out" \
  "re-establishing the forward"
expect "  ...and the head is handed the FORWARDED socket" "$out" \
  "--socket $T/run/letibot/migrated-s-4242.sock"
expect "  ...and resumes that session, not this folder's" "$out" "--resume s-4242"
refuse "  ...and no daemon is started for this folder" "$out" "^harnessd "
rm -f "$T/run/letibot/migrated-s-4242.json"



[ "$fails" -eq 0 ] && echo "launcher: all cases pass" || { echo "launcher: $fails failed"; exit 1; }
