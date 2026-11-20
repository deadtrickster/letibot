#!/usr/bin/env bash
# The launcher's model-to-endpoint lookup, run against stub binaries: `letibot "q"` execs a
# harnessd stub that prints its argv, with a throwaway HOME and a providers.toml per case.
# Nothing listens, and the launcher must still start rather than gate on a model.
#   tests/launcher.sh            # from the repo root; exits non-zero on a failure
set -u
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
LAUNCHER="$ROOT/scripts/letibot"
T="$(mktemp -d)"
# The daemon stubs are named by `$T` and nothing else on the box is, so cleanup kills by it and
# does not depend on a case reaching its own `kill`.
cleanup() {
  ps -eo pid=,args= 2> /dev/null | awk -v d="$T/bin/harnessd" 'index($0, d) { print $1 }' \
    | while read -r _stub; do kill "$_stub" 2> /dev/null || true; done
  rm -rf "$T"
}
trap cleanup EXIT
# The python stubs poll this and leave once it is gone, so a run killed past its trap (SIGKILL, a
# runner's timeout) does not leave them listening for weeks.
export LBT_OWNER=$$

mkdir -p "$T/bin" "$T/home/.config/letibot" "$T/run" "$T/ws"
# A coordinator stub that binds `$LETIBOT_ROOM_SOCKET` and writes the lease, so every later
# case's `room_up` answers yes. `exec -a harnessd`: `--stop --all` judges liveness by argv's
# first word, and `room_up` reads the argv for `--room`.
cat > "$T/bin/harnessd-room.py" << 'STUB'
import os, socket, sys, time
p = os.environ["LETIBOT_ROOM_SOCKET"]
with open(os.environ["ROOM_ARGV"], "a") as f:
    f.write("room " + " ".join(sys.argv[1:]) + "\n")
with open(os.path.join(os.path.dirname(p), "pid"), "w") as f:
    f.write(str(os.getpid()))
try: os.unlink(p)
except FileNotFoundError: pass
s = socket.socket(socket.AF_UNIX); s.bind(p); s.listen(4)
now = int(time.time() * 1000)
with open(p[:-5] + ".lease", "w") as f:
    f.write("box/%d/1\t%d\t%d\t%d\n" % (os.getpid(), os.getpid(), now, now))
def owner_alive():
    o = os.environ.get("LBT_OWNER")
    if not o:
        return True
    try:
        os.kill(int(o), 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass
    return True
while owner_alive():
    time.sleep(0.2)
STUB
cat > "$T/bin/harnessd" << 'STUB'
#!/usr/bin/env bash
if [ "${1:-}" = "--room" ]; then
  exec -a harnessd python3 "$(dirname "$0")/harnessd-room.py" --room
fi
printf 'harnessd'; printf ' %q' "$@"; printf '\n'
# **What the launcher gave this child**, for the open-file cases below: a child's limits are
# its parent's, so this is the only honest way to see what the launcher raised.
printf 'openfiles %s\n' "$(ulimit -Sn)"
printf 'openfiles-hard %s\n' "$(ulimit -Hn)"
STUB
cp "$T/bin/harnessd" "$T/bin/letibot-tui"
chmod +x "$T/bin/harnessd" "$T/bin/letibot-tui"
# The resolver's answer, as `bridge::daemon::resolve` gives it: the launcher asks
# `letibot-multi where DIR --json` for a folder's key and socket. This is the stub's copy of the
# rule; `where.rs` holds the launcher to the real binary.
cat > "$T/bin/where-json" << 'STUB'
#!/usr/bin/env bash
d="$(cd "$1" && pwd -P)" || exit 1
top="$(git -C "$d" rev-parse --show-toplevel 2> /dev/null)" && d="$(cd "$top" && pwd -P)"
if command -v sha256sum > /dev/null 2>&1; then sum=(sha256sum); else sum=(shasum -a 256); fi
key="$(printf '%s' "$d" | "${sum[@]}" | cut -c1-12)"
proto=null
[ -f "$(dirname "$0")/../stub-protocol" ] && proto="$(cat "$(dirname "$0")/../stub-protocol")"
printf '{"folder":"%s","live":false,"project":"%s","protocol":%s,"socket":"%s","this_build":36}\n' \
  "$d" "$key" "$proto" "${LETIBOT_SOCKET:-$XDG_RUNTIME_DIR/letibot/$key.sock}"
STUB
# The multicall: answers the launcher's own `where DIR --json`, prints the argv of anything else.
cat > "$T/bin/letibot-multi.default" << 'STUB'
#!/usr/bin/env bash
if [ "$#" -eq 3 ] && [ "$1" = where ] && [ "$3" = --json ]; then exec "$(dirname "$0")/where-json" "$2"; fi
printf 'letibot-multi'; printf ' %q' "$@"; printf '\n'
[ -z "${LETIBOT_STORE:-}" ] || printf 'store=%s\n' "$LETIBOT_STORE"
STUB
chmod +x "$T/bin/where-json" "$T/bin/letibot-multi.default"
cp "$T/bin/letibot-multi.default" "$T/bin/letibot-multi"

fails=0
# The liveness reader must be on this PATH: `ss` on Linux (`/usr/bin`), `lsof` on macOS
# (`/usr/sbin`). Without it a Mac's launcher sees no socket and every case takes the start branch.
RUN_PATH="/usr/bin:/bin"
for d in /usr/sbin /sbin; do [ -d "$d" ] && RUN_PATH="$RUN_PATH:$d"; done
# The box's room goes somewhere this harness owns. `harnessd`'s `room::socket_path` and the
# launcher's `room_paths` both honour `LETIBOT_ROOM_SOCKET`, so every case here shares one room —
# which is also what a real box has — and nothing touches the operator's data directory.
ROOM_SOCK="$T/room/room.sock"
mkdir -p "$T/room"
# run NAME PROVIDERS_TOML [ARGS...] -- the launcher's combined output for one case.
run() {
  local toml="$1"
                   shift
  printf '%s' "$toml" > "$T/home/.config/letibot/providers.toml"
  (cd "${RUN_WS:-$T/ws}" && env -i PATH="${RUN_PATH_EXTRA:-}$RUN_PATH" HOME="$T/home" XDG_RUNTIME_DIR="$T/run" \
    LETIBOT_BIN="$T/bin" LETIBOT_ENDPOINT= STUB_ARGV="$T/argv.log" \
    XDG_DATA_HOME="${RUN_XDG_DATA_HOME:-}" XDG_CONFIG_HOME="${RUN_XDG_CONFIG_HOME:-}" \
    LETIBOT_PROVIDERS="${RUN_LETIBOT_PROVIDERS:-}" \
    LETIBOT_ROOM_SOCKET="${ROOM_SOCK_OVERRIDE-$ROOM_SOCK}" ROOM_ARGV="$T/room-argv.log" \
    LETIBOT_SCOPE_RUNTIME_MAX="${LETIBOT_SCOPE_RUNTIME_MAX:-}" \
    "$(command -v bash)" "$LAUNCHER" "$@" 2>&1 < /dev/null)
}
expect() { # expect NAME OUTPUT PATTERN -- the output must match
  if printf '%s' "$2" | grep -qE -- "$3"; then
                                               echo "ok   $1"
  else
       echo "FAIL $1: wanted /$3/ in:"
                                        printf '%s\n' "$2" | sed 's/^/     /'
                                                                               fails=$((fails + 1))
  fi
}
in_order() { # in_order NAME OUTPUT PATTERN... -- each pattern matches a line after the previous one's
  local name="$1" out="$2" at=0 n
  shift 2
  for p in "$@"; do
    n="$(printf '%s\n' "$out" | tail -n +"$((at + 1))" | grep -nE -m1 -- "$p" | cut -d: -f1)"
    if [ -z "$n" ]; then
      echo "FAIL $name: wanted /$p/ after line $at in:"
      printf '%s\n' "$out" | sed 's/^/     /'
      fails=$((fails + 1))
      return
    fi
    at=$((at + n))
  done
  echo "ok   $name"
}
refuse() { # refuse NAME OUTPUT PATTERN -- the output must not match
  if printf '%s' "$2" | grep -qE -- "$3"; then
    echo "FAIL $1: did not want /$3/ in:"
                                           printf '%s\n' "$2" | sed 's/^/     /'
                                                                                  fails=$((fails + 1))
  else echo "ok   $1"; fi
}

# ---------------------------------------------------------------------------------------
# The shell scripts under a stock Mac's bash 3.2. A parse is not enough: `declare -A` is a
# runtime error. BASH32 files are parsed with `/bin/bash -n` and grepped for bash-4 spellings;
# POSIX_SH (`#!/bin/sh`) files are grepped only. A new bash script under `scripts/` goes in BASH32.
# Out of scope: `tests/acceptance/*.sh` (Linux bash 5), `tests/fidelity/serve_oracle.sh`,
# `firecode.layer`. `+=` on an array is bash 3.1 and allowed; an empty-array expansion under
# `set -u` no grep can see, so the `--help` run below covers it.
BASH32="scripts/letibot scripts/leticode scripts/etalon-snapshot scripts/headwatch.sh scripts/migrate-notes.sh scripts/rrun scripts/wire-skew.sh tests/launcher.sh tests/rrun.sh tests/claude-plugin.sh"
POSIX_SH="scripts/check-dist-names.sh scripts/check-fmt.sh scripts/check-host-runtime.sh scripts/check-provider-models.sh scripts/check-tag.sh scripts/check-test-attrs.sh scripts/clippy-gate.sh scripts/gate-at.sh scripts/gate-env.sh scripts/gate-steps.sh scripts/install-gate-build.sh scripts/make-dist.sh scripts/refresh-models-snapshot.sh install.sh"
# The bash-4 spellings as one ERE: `-A`/`-n`/`-g` declarations, `mapfile`, `readarray`,
# `coproc`, `wait -n`, `[[ -v ]]`, `;;&`, `;&`, `&>>`, `|&`, `${x,,}`, `${x^^}`. Literals are
# written with a character class (`map[f]ile`) because this file greps itself.
BASH4_SPELLINGS='declare +-[[:alpha:]]*[An]|local +-[[:alpha:]]*[An]|typeset +-[[:alpha:]]*[An]|declare +-[gn]|local +-[gn]|map[f]ile|readarra[y]|copro[c]|wai[t] -n|\[\[ +-v |\$\{[[:alpha:]_][[:alnum:]_]*(\[[^]]*\])?(,,|\^\^|,|\^)\}|[&]>>|[|]&|;;[&]|;[&][[:space:]]*$'

for f in $BASH32; do
  if /bin/bash -n "$ROOT/$f" 2> "$T/parse.err"; then
    echo "ok   bash-3.2: $f parses under /bin/bash"
  else
    echo "FAIL bash-3.2: $f does not parse under /bin/bash ($(/bin/bash --version | head -1)):"
    sed 's/^/     /' "$T/parse.err"
                                     fails=$((fails + 1))
  fi
done
for f in $BASH32 $POSIX_SH; do
  # A COMMENT may name a spelling (`# we used to declare -A here`); a command may not. The
  # `-v` drops the comment lines, which is also what lets the note above say the words.
  hits="$(grep -nE -- "$BASH4_SPELLINGS" "$ROOT/$f" | grep -vE '^[0-9]+:[[:space:]]*#' || true)"
  if [ -n "$hits" ]; then
    echo "FAIL bash-3.2: $f uses a bash-4 spelling the Mac's /bin/bash does not have:"
    printf '%s\n' "$hits" | sed 's/^/     /'
    fails=$((fails + 1))
  else
    echo "ok   bash-3.2: $f has no bash-4 spelling"
  fi
done
# A script under `scripts/` is in scope by existing: a bash one not in BASH32, or an sh one not
# in POSIX_SH, fails by name, so the lists cannot drift from the directory.
for f in "$ROOT"/scripts/*; do
  [ -f "$f" ] || continue
  rel="scripts/${f##*/}"
  case "$(head -n 1 "$f" 2> /dev/null)" in
    *bash*)
      case " $BASH32 " in
        *" $rel "*) ;;
        *)
           echo "FAIL bash-3.2: $rel names bash and is not in BASH32 — add it to the guard"
                                                                                               fails=$((fails + 1))
                                                                                                                    ;;
      esac
           ;;
    # `*bin/sh*` and not `*sh`: a `#!/usr/bin/env zsh` is a shell script of a third kind,
    # and this guard is about the two the launchers and the installer are written in.
    *bin/sh*)
      case " $POSIX_SH " in
        *" $rel "*) ;;
        *)
           echo "FAIL bash-3.2: $rel is a /bin/sh script and is not in POSIX_SH — add it to the guard"
                                                                                                          fails=$((fails + 1))
                                                                                                                               ;;
      esac
           ;;
  esac
done
# A script nobody can run is not a check. The lists above are each file's documented
# invocation, and a mode that drifts to 644 makes `tests/launcher.sh` fail with Permission
# denied before a case runs - which is exactly how this file's own bit went missing.
for f in $BASH32 $POSIX_SH; do
  if [ -x "$ROOT/$f" ]; then
    echo "ok   modes: $f is executable"
  else
    echo "FAIL modes: $f is not executable - a script nobody can run is not a check"
    fails=$((fails + 1))
  fi
done
# A run, not only a parse: `--help` reaches argument handling and the array expansions without
# a daemon, in the same scrubbed environment as the cases above.
help_out="$( (cd "$T/ws" && env -i PATH="$RUN_PATH" HOME="$T/home" XDG_RUNTIME_DIR="$T/run" \
  LETIBOT_BIN="$T/bin" /bin/bash "$LAUNCHER" --help) 2>&1 )"
if printf '%s' "$help_out" | grep -q "one command to start chatting"; then
  echo "ok   bash-3.2: /bin/bash scripts/letibot --help runs on the Mac's bash"
else
  echo "FAIL bash-3.2: /bin/bash scripts/letibot --help did not run:"
  printf '%s\n' "$help_out" | sed 's/^/     /'
                                                fails=$((fails + 1))
fi

LOCAL_HOME_ONLY='[model."local.flash"]
url = "http://127.0.0.1:59997"
model = "qwen-3.8-flash-next"
'
DEFAULT_ONLY='[deepseek]

[default]
provider = "deepseek"
'
out="$(run "$DEFAULT_ONLY" "one question")"
refuse "a [default] provider with no local model block is not refused" "$out" "no endpoint for this run"
expect "  ...and the daemon is started" "$out" "^harnessd "
refuse "  ...with no local endpoint invented for it" "$out" "--endpoint"
# A MODE IS THE STORE'S, NOT THE LAUNCHER'S: `--mode writes-allowed` was passed on every start and
# overrode every project's `automode` row, sending each bash call to a head that is often not
# attached (and failing closed when it is not). Nothing is passed unless the person asks.
refuse "  ...and no --mode, so the project's stored mode (automode) is the one in force" "$out" \
  "^harnessd .*[ ]--mode([ ]|$)"
out="$(run "$DEFAULT_ONLY" --mode automode "one question")"
expect "  ...and an explicit --mode still reaches the daemon" "$out" "--mode automode( |$)"

# a_one_shot_with_model_runs_that_model_or_refuses: harnessd reads a bare `--model` as the
# local binding, so a provider's model must be passed as the provider's, not dropped.
ONESHOT_MODELS='[deepseek]

[glm]

[model."local-x"]
url = "http://127.0.0.1:9/v1"

[default]
provider = "deepseek"
'
out="$(run "$ONESHOT_MODELS" --model glm/glm-5.1 "one question")"
expect "a_one_shot_with_model_runs_that_model_or_refuses: provider/model names both" "$out" "^harnessd .*--provider glm --model glm-5\.1( |$)"
out="$(run "$ONESHOT_MODELS" --model glm "one question")"
expect "  ...a provider's name is that provider" "$out" "^harnessd .*--provider glm( |$)"
refuse "  ...and not a local binding" "$out" "--model glm( |$)"
out="$(run "$ONESHOT_MODELS" --model deepseek-chat "one question")"
expect "  ...a bare model is the default provider's" "$out" "^harnessd .*--provider deepseek --model deepseek-chat( |$)"
out="$(run "$ONESHOT_MODELS" --model local-x "one question")"
expect "  ...a local model under a default provider refuses with the key" "$out" "local-x is a local model.*\[default\] provider"
refuse "  ...and nothing runs" "$out" "^harnessd "

out="$(run "$DEFAULT_ONLY" --status)"
expect "--status says there is no local home rather than refusing" "$out" "^local +none"

# The providers file is the daemon's (`letibot_paths::config_dir`): XDG_CONFIG_HOME when absolute,
# and no LETIBOT_PROVIDERS, which no Rust reader honours.
mkdir -p "$T/xdgconfig/letibot"
printf '%s' "$LOCAL_HOME_ONLY" > "$T/xdgconfig/letibot/providers.toml"
out="$(RUN_XDG_CONFIG_HOME="$T/xdgconfig" run '' "one question")"
expect "the launcher reads XDG_CONFIG_HOME's providers.toml, as the daemon does" "$out" "--endpoint 127.0.0.1:59997( |$)"
out="$(RUN_LETIBOT_PROVIDERS="$T/xdgconfig/letibot/providers.toml" run '' --status)"
expect "  ...and LETIBOT_PROVIDERS is not a second file" "$out" "in $T/home/\.config/letibot/providers\.toml"
out="$(RUN_XDG_CONFIG_HOME="relative" run '' --status)"
expect "  ...and a relative XDG_CONFIG_HOME is unset" "$out" "in $T/home/\.config/letibot/providers\.toml"

# The readers follow `keys::parse_file`: a comment after a header or a value, and a block that
# is split in two, are read as the daemon reads them.
out="$(run '[deepseek]

[default] # where turns go
provider = "deepseek" # mine
' --model deepseek-chat "one question")"
expect "a commented [default] is still the default" "$out" "^harnessd .*--provider deepseek --model deepseek-chat( |$)"
out="$(run '[model."local.flash"]
url = "http://127.0.0.1:59996"

[deepseek]

[model."local.flash"]
model = "qwen-3.8-flash-next"
' "one question")"
expect "a block split in two is one block" "$out" "--endpoint 127.0.0.1:59996( |$)"

# ---------------------------------------------------------------------------------------
# HELP AND MISTAKES, WHICH NEED NO DAEMON.
# `--help` after a verb is that verb's page, and a missing argument is named. Asserted as shape:
# non-zero exit, the mistake named, the verb's page named, no wall of text.
refusal() { # refusal NAME RC OUTPUT FIRST SECOND -- a mistake, in two lines and no wall
  local name="$1" rc="$2" out="$3" first="$4" second="$5" why=""
  if [ "$rc" -eq 0 ]; then
                           why="exited 0"
  elif ! printf '%s\n' "$out" | grep -qE -- "$first"; then
                                                           why="wanted /$first/ as the first line"
  elif ! printf '%s\n' "$out" | grep -qE -- "$second"; then
                                                            why="wanted /$second/ after it"
  elif printf '%s\n' "$out" | grep -q "one command to start chatting"; then
                                                                            why="printed the usage wall"
  fi
  if [ -z "$why" ]; then
                         echo "ok   $name"
  else
    echo "FAIL $name: $why in:"
                                 printf '%s\n' "$out" | sed 's/^/     /'
                                                                          fails=$((fails + 1))
  fi
}

# --- `--effort` takes the one scale harnessd takes, and nothing else ---------------------
for level in off low medium high max; do
  out="$(run "$DEFAULT_ONLY" --effort "$level" "one question")"
  expect "--effort $level reaches the daemon" "$out" "^harnessd .*--effort $level( |\$)"
done
out="$(run "$DEFAULT_ONLY" --effort xhigh "one question")"
expect "--effort xhigh is refused naming the five words" "$out" "off, low, medium, high, max"
refuse "  ...and no daemon is started for it" "$out" "^harnessd "

# --- a verb left without its argument: what is missing, and where that verb is explained ---
for spec in "a session with no id|--session|ID" "a handover with no session|--handover|SESSION DIR" \
  "a revert with no backup|--revert|TRANSCRIPT BACKUP" "a note with no text|--note|TRANSCRIPT TEXT" \
  "a reattach with no id|--reattach|ID"; do
  name="${spec%%|*}"
                      rest="${spec#*|}"
                                         verb="${rest%%|*}"
                                                             what="${rest#*|}"
  out="$(run "$DEFAULT_ONLY" "$verb")"
                                        rc=$?
  refusal "$name names the $what and the verb's help" "$rc" "$out" \
    "^letibot: $verb needs $what" "run \`letibot $verb --help\`"
done

# The session verbs, and their old flags, which are refused under the new spelling.
for spec in "session delete|ID" "session show|ID" "session unarchive|ID" \
  "session rename|ID \[TITLE\]" "session archive|ID \[REASON\]" \
  "--delete|session delete needs ID" "--rename|session rename needs ID"; do
  verb="${spec%%|*}"
  what="${spec#*|}"
  # shellcheck disable=SC2086 # the verb is two words on purpose
  out="$(run "$DEFAULT_ONLY" $verb)"
  rc=$?
  case "$verb" in
    --*) refusal "$verb with no id is refused as its new spelling" "$rc" "$out" \
      "^note: .letibot $verb. is now" "$what" ;;
    *) refusal "$verb with no id names the $what and the verb's help" "$rc" "$out" \
      "^letibot: $verb needs $what" "run \`letibot $verb --help\`" ;;
  esac
done

# --- and `attach`, which has two words to be left without --------------------------------
out="$(run "$DEFAULT_ONLY" attach)"
                                     rc=$?
refusal "attach with no source names the source" "$rc" "$out" \
  "^letibot: attach needs a source" "letibot attach github <org\\|owner/repo>"
out="$(run "$DEFAULT_ONLY" attach github)"
                                            rc=$?
refusal "attach with no scope names the scope" "$rc" "$out" \
  "^letibot: attach needs a scope" "letibot attach github <org\\|owner/repo>"
out="$(run "$DEFAULT_ONLY" attach gitlab stroppy-io)"
                                                       rc=$?
refusal "attach with another source names the one it knows" "$rc" "$out" \
  "^letibot: attach knows one source and it is \`github\`, not gitlab" \
  "letibot attach github <org\\|owner/repo>"

# --- a flag nobody has, and a directory that is not there --------------------------------
out="$(run "$DEFAULT_ONLY" --halp)"
                                     rc=$?
refusal "an unknown flag is named, not run as a prompt" "$rc" "$out" \
  "^letibot: unknown flag: --halp" "letibot --help"

# A REMOVED FLAG FAILS NAMING ITS REPLACEMENT: the four this release deleted.
# Dying here rather than falling through to the one-shot is the difference between a script
# learning where the flag went and a billed turn answering a sentence nobody asked.
for spec in "--daemons|--ls" "--flash|\[default\] model" "--writes|writes-allowed" "--bash|--no-bash"; do
  flag="${spec%%|*}"
                     want="${spec#*|}"
  out="$(run "$DEFAULT_ONLY" "$flag")"
                                        rc=$?
  refusal "$flag is refused by name, with what replaced it" "$rc" "$out" \
    "^letibot: $flag is not a flag any more" "$want"
done

# A SETTING WITH A CONFIG KEY SAYS WHICH KEY — one line on stderr, and the flag still works
# for this release. `--provider` with no value is the cheapest way to read the notice.
out="$(run "$DEFAULT_ONLY" --provider)"
expect "--provider names the key it now lives at" "$out" \
  "\[default\] provider.*providers\.toml"
expect "  ...and --help all lists it under the settings that live in a file" \
  "$(run "$DEFAULT_ONLY" --help all)" "SETTINGS THAT LIVE IN A FILE"
out="$(run "$DEFAULT_ONLY" --stop /no/such/folder)"
                                                     rc=$?
refusal "--stop with a directory that is not there says so" "$rc" "$out" \
  "^letibot: no such directory: /no/such/folder" "letibot --ls"

# --- a daemon that is not running: what is missing, and the command that starts one ------
out="$(run "$DEFAULT_ONLY" --attach)"
                                       rc=$?
refusal "--attach with no daemon says so and names the command that starts one" "$rc" "$out" \
  "^letibot: no harnessd for" "run \`letibot\` there to start one"

# --- a verb's own page, not the wall ------------------------------------------------------
for spec in "--delete|letibot session delete ID" "--session|letibot --session ID" \
  "--rename|letibot session rename ID" "--sessions|letibot session list" "--migrate|--to HOST" "--reattach|letibot --reattach SESSION" \
  "--stop|--force" "--restart|letibot --restart" "--room|--stop" "--handover|letibot --handover SESSION DIR" \
  "--note-continued|letibot --note-continued" "--list-claude|letibot --list-claude" \
  "--ls|letibot --ls" "--attach|letibot --attach" \
  "--status|letibot --status" "--where|letibot --where [FOLDER]" "attach|letibot attach github"; do
  verb="${spec%%|*}"
                      want="${spec#*|}"
  out="$(run "$DEFAULT_ONLY" "$verb" --help)"
                                               rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "FAIL $verb --help exited $rc:"
                                          printf '%s\n' "$out" | sed 's/^/     /'
                                                                                   fails=$((fails + 1))
  elif ! printf '%s\n' "$out" | grep -qF -- "$want"; then
    echo "FAIL $verb --help should say '$want':"
                                                  printf '%s\n' "$out" | sed 's/^/     /'
                                                                                           fails=$((fails + 1))
  elif ! printf '%s\n' "$out" | grep -q "example"; then
    echo "FAIL $verb --help gives no example:"
                                                printf '%s\n' "$out" | sed 's/^/     /'
                                                                                         fails=$((fails + 1))
  elif printf '%s\n' "$out" | grep -q "one command to start chatting"; then
    echo "FAIL $verb --help printed the wall instead of its own page"
                                                                       fails=$((fails + 1))
  else echo "ok   $verb --help is that verb's page, with an example"; fi
done

# --- and the wall itself lists every verb it answers for ----------------------------------
# `--help` IS THE DAILY SET; `--help all` IS THE REST, so both pages are walked:
# a flag this parser reads and no page names is the same disagreement from the other side.
out="$(run "$DEFAULT_ONLY" --help all)"
for verb in --continue --session --sessions --new --rename --delete --migrate --reattach \
  --handover --note --note-continued --revert --list-claude --stop --restart --room --status \
  --ls --attach --glm --provider --host --vm --tsv \
  --web-fetch --no-web-fetch --effort --vm-copy --listen --where \
  --list --list-all --claude-sessions --no-bash --providers; do
  if printf '%s\n' "$out" | grep -q -- "$verb"; then
                                                     echo "ok   --help all names $verb"
  else
       echo "FAIL --help all does not name $verb, which the launcher accepts"
                                                                           fails=$((fails + 1))
  fi
done

# No provider, no local model block, nothing serving: started, not refused. The daemon
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

# A model asked for by name with no home is SAID, and still started — the flag meant
# something the file does not carry, which is worth a sentence and not a refusal.
out="$(run '' --dense "one question")"
expect "a flag's model with no home is named" "$out" "no endpoint for this run: --dense asked"
expect "  ...and the daemon is started anyway" "$out" "^harnessd "

# THE LAUNCHER RAISES THE OPEN-FILE LIMIT, AND ITS CHILD IS WHAT SHOWS IT.
# Darwin's soft 256 is too few for the daemon, and only the parent can raise it. A child sees at
# least the request, or the launcher names both numbers. The hard limit is untouched, and a soft
# limit already above the request is left alone. The request is read from the launcher.
asked="$(sed -n 's/^OPEN_FILES_WANTED=\([0-9][0-9]*\).*/\1/p' "$LAUNCHER" | head -1)"
# limit_case NAME SOFT OUTPUT — rule 1, once, for the one-shot path and the daemon path.
limit_case() { # limit_case NAME SOFT OUTPUT
  local name="$1" soft="$2" out="$3" ok=0
  case "$soft" in
    '')  ok=0 ;;
    *[!0-9]*) ok=1 ;; # `unlimited`
    *) if [ "$soft" -ge "$asked" ]; then ok=1; fi ;;
  esac
  if [ "$ok" = 1 ]; then
    echo "ok   $name: a child of the launcher gets $soft open files (it asked for $asked)"
  elif printf '%s' "$out" | grep -qE "open files: got [0-9]+, not the $asked asked for"; then
    echo "ok   $name: this box gave less than $asked, and the launcher SAID so"
  else
    echo "FAIL $name: a child got '${soft:-nothing}' open files, the launcher asked for $asked,"
    echo "     and nothing said so:"
    printf '%s\n' "$out" | sed 's/^/     /'
    fails=$((fails + 1))
  fi
}
if [ -z "$asked" ]; then
  echo "FAIL the launcher no longer assigns OPEN_FILES_WANTED=<n>, so this case cannot ask it"
  fails=$((fails + 1))
else
  out="$(run "$DEFAULT_ONLY" "one question")"
  limit_case "the launcher raises the open-file limit for the daemon it starts" \
    "$(printf '%s\n' "$out" | sed -n 's/^openfiles //p' | head -1)" "$out"
  # The HARD limit must be exactly what this shell has, because the launcher runs as this
  # shell's child (`env -i` carries no rlimits): anything else means the raise moved it too.
  own_hard="$(ulimit -Hn 2> /dev/null || true)"
  child_hard="$(printf '%s\n' "$out" | sed -n 's/^openfiles-hard //p' | head -1)"
  if [ "$child_hard" = "$own_hard" ]; then
    echo "ok   ...and the HARD limit is left where the box set it ($own_hard)"
  else
    echo "FAIL the launcher moved the HARD open-file limit: $own_hard -> ${child_hard:-nothing}"
    fails=$((fails + 1))
  fi
  # Never lower one. A shell already above the request keeps its own number.
  higher=$((asked + 1000))
  if ( ulimit -Sn "$higher" ) > /dev/null 2>&1; then
    out="$( ulimit -Sn "$higher" && run "$DEFAULT_ONLY" "one question" )"
    soft="$(printf '%s\n' "$out" | sed -n 's/^openfiles //p' | head -1)"
    if [ "$soft" = "$higher" ]; then
      echo "ok   ...and a soft limit already above the request ($higher) is left alone"
    else
      echo "FAIL a soft limit of $higher was changed to '${soft:-nothing}' by the launcher"
      fails=$((fails + 1))
    fi
  else
    echo "skip the never-lower case: this box's hard limit will not give $higher"
    echo "     NOTHING WAS CHECKED about lowering — that is a fact about this box."
  fi
fi

# `--host NAME` is forwarded to the daemon, which resolves the name against `hosts.toml`. The
# launcher passes the pair through intact and consumes the value so it is not the prompt.
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

# The health gate is gone too: a home whose server is not answering, and no provider —
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
# A daemon that is LISTENING but speaks another protocol is refused with the sentence naming the
# way out, not handed a head that dies on the bare refusal. Needs a socket really listening
# (`live` is `ss`, since a stopping daemon unlinks the file) and a head that refuses as an older one.
key12() { if command -v sha256sum > /dev/null 2>&1; then sha256sum; else shasum -a 256; fi | cut -c1-12; }
if command -v python3 > /dev/null 2>&1; then
  cat > "$T/bin/letibot-tui" << 'STUB'
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
  # `$T/run/letibot`, not `$T/run`: the launcher's RUNDIR is `$XDG_RUNTIME_DIR/letibot`.
  mkdir -p "$T/run/letibot"
  SOCK_P="$T/run/letibot/$(printf '%s' "$WS_P" | key12).sock"
  python3 -c 'import socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.bind(sys.argv[1])
s.listen(2)
time.sleep(120)' "$SOCK_P" &
  _listener=$!
  sleep 0.3

  # Can this box show the launcher a listening socket at all? Asked with the launcher's own
  # reader first, so a harness that cannot answer skips loudly instead of reporting failures.
  listens() { # listens SOCKET-PATH
    if [ "$(uname -s)" = Darwin ]; then
      lsof -nP -U 2> /dev/null | grep -qF "$1"
    else
      ss -lxH 2> /dev/null | grep -qF "$1"
    fi
  }
  if ! listens "$SOCK_P"; then
    echo "skip the protocol-skew cases: nothing here reports a listening socket"
    echo "     (ss on Linux, lsof on macOS) so the launcher cannot be shown one either."
    echo "     NOTHING WAS CHECKED — this is a fact about the harness, not a pass."
    kill "$_listener" 2> /dev/null || true
  else
    # The number the refusal quotes is ASKED of the resolver (`letibot where --json`), because a
    # daemon that accepts a differing head says nothing about a version in its prose.
    echo 22 > "$T/stub-protocol"
    out="$(run "$DEFAULT_ONLY")"
    expect "a daemon speaking another protocol is refused, not attached to" "$out" "speaks protocol 22 and this build speaks 36"
    expect "  ...and the sentence names the way out" "$out" "letibot --stop"
    expect "  ...and says nothing is lost" "$out" "on disk"
    refuse "  ...and no head is handed to it" "$out" "THE HEAD RAN"

    out="$(run "$DEFAULT_ONLY" --attach)"
    expect "--attach refuses in the same words" "$out" "speaks protocol 22 and this build speaks 36"
    refuse "  ...and does not claim to have attached" "$out" "attached to harnessd"

    rm -f "$T/stub-protocol"
    kill "$_listener" 2> /dev/null || true
  fi
else
  echo "skip the protocol-skew cases: no python3 to listen on a unix socket"
fi

# `pid_started` must read `/proc/<pid>/stat` field 22, not the procfs directory's mtime, which
# is not per-process and equals the start time only for a young process, so only the source can
# show it. Comments are stripped before the grep, since the reader's own comment may quote it.
if grep -v '^[[:space:]]*#' "$LAUNCHER" | grep -q 'stat -c %Y "/proc/'; then
  echo "FAIL a process start time is read from a /proc DIRECTORY's mtime:"
  grep -v '^[[:space:]]*#' "$LAUNCHER" | grep -n 'stat -c %Y "/proc/' | sed 's/^/     /'
  fails=$((fails + 1))
else
  echo "ok   no reader takes a /proc directory's mtime for a process start time"
fi

# A daemon that is still going is WAITED FOR, not started over. A stopping daemon drops its
# socket while finishing its turn, and only the record survives that window. Constructed, not
# raced: the stub listens, so the start path runs its real `live` poll, and goes when let go.
if command -v python3 > /dev/null 2>&1; then
  cat > "$T/bin/harnessd" << 'STUB'
#!/usr/bin/env python3
import os, socket, sys, time
# Only a start names a socket; the launcher's other calls (a store query) answer and exit.
if "--socket" not in sys.argv:
    print("harnessd", *sys.argv[1:], flush=True)
    sys.exit(0)
p = sys.argv[sys.argv.index("--socket") + 1]
try: os.unlink(p)
except FileNotFoundError: pass
s = socket.socket(socket.AF_UNIX); s.bind(p); s.listen(1)
print("harnessd", *sys.argv[1:], flush=True)
def owner_alive():
    o = os.environ.get("LBT_OWNER")
    if not o:
        return True
    try:
        os.kill(int(o), 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass
    return True
while owner_alive():
    time.sleep(0.2)
STUB
  chmod +x "$T/bin/harnessd"

  # The folder as the launcher names it, resolved: macOS `mktemp -d` is under the `/var` symlink
  # and the launcher keys its record by `pwd -P`.
  ws_p="$(cd "$T/ws" && pwd -P)"
  key="$(printf '%s' "$ws_p" | key12)"
  mkdir -p "$T/run/letibot"
  sock="$T/run/letibot/$key.sock"
  # A daemon of THIS folder, on its way out: its argv names our socket, which is how the
  # launcher tells it from a recycled pid that is somebody else's harnessd, and it lives
  # until this test lets it go.
  bash -c 'while [ ! -e "$1" ]; do sleep 0.05; done' _ "$T/let-go" bash --socket "$sock" &
  going=$!
  printf '{"pid": %s, "workspace": "%s", "socket": "%s"}' "$going" "$ws_p" "$sock" \
    > "$T/run/letibot/$key.json"

  ( run "$DEFAULT_ONLY" --session s-1 > "$T/going.out" 2>&1 ) &
  launched=$!
  # Let it reach the wait, then let the daemon go. The bound is the point of the case: it
  # must not have started a second daemon in that window. Released on the launcher's own
  # line, not after a fixed sleep: a loaded host reaches the check later than any sleep.
  for _ in $(seq 1 600); do
    grep -q "is still going" "$T/going.out" 2> /dev/null && break
    sleep 0.1
  done
  : > "$T/let-go"
  wait "$launched" 2> /dev/null || true
  out="$(cat "$T/going.out")"
  expect "a daemon that is still going is waited for" "$out" "is still going - waiting for it"
  refuse "  ...and nothing is started over it" "$out" "already served by a live daemon"
  expect "  ...and the session opens once it is gone" "$out" "THE HEAD RAN"

  # No endpoint gate: nothing serving and no provider, and the head runs. A folder of its own,
  # so the waiting case's record is not this one's.
  mkdir -p "$T/bare"
  bare_out="$(RUN_WS="$T/bare" run '')"
  refuse "a bare box is not refused for nothing serving" "$bare_out" "nothing serving"
  refuse "  ...nor asked to pick a provider in the shell" "$bare_out" "1\\) deepseek"
  refuse "  ...nor for a missing model home" "$bare_out" "no endpoint for this run"
  expect "  ...and the head runs" "$bare_out" "THE HEAD RAN"
  bare_key="$(printf '%s' "$(cd "$T/bare" && pwd -P)" | key12)"
  kill "$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    "$T/run/letibot/$bare_key.json" 2> /dev/null | head -1)" 2> /dev/null || true

  # The stub daemon the launcher started is a real one for this case; it does not exit.
  # (Read with the same sed the launcher's own `json_num` is, because that is a function of
  # the launcher and not of this harness.)
  kill "$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    "$T/run/letibot/$key.json" | head -1)" 2> /dev/null || true
  kill "$going" 2> /dev/null || true
else
  echo "skip the going-daemon case: no python3 to listen on a unix socket"
fi

# `--stop --force N` stops daemon N, not this folder's: `--force` before the number must not
# drop it. One record, for another
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
# WHAT A START OPENS, AND WHAT A HEAD'S EXIT MAY NOT DO.
# 1. A plain start passes no `--session`: only `--continue`/`--session` name one. Pinned on argv.
# 2. A head's exit leaves its daemon listening, and `--new` on a live folder starts no second one.
# 3. `--new` with no daemon up asks the head for no second session.
if command -v python3 > /dev/null 2>&1; then
  # A daemon stub that LISTENS (so the launcher's start poll succeeds), writes every invocation's
  # argv down, and stays until this test kills it.
  cat > "$T/bin/harnessd" << 'STUB'
#!/usr/bin/env python3
import os, resource, socket, sys, time
with open(os.environ["STUB_ARGV"], "a") as log:
    log.write("harnessd " + " ".join(sys.argv[1:]) + "\n")
    # What the launcher gave this process. The launcher reaches a daemon through
    # `detach` (perl's setsid + exec on a Mac, setsid under a systemd scope on Linux),
    # so this is the only place that says the limit survived that path.
    log.write("openfiles %d\n" % resource.getrlimit(resource.RLIMIT_NOFILE)[0])
if "--latest-session" in sys.argv:
    print("s-latest")
    sys.exit(0)
if "--find-session" in sys.argv:
    name = sys.argv[sys.argv.index("--find-session") + 1]
    if name == "my-work":
        print("here\ts-named-1")
        sys.exit(0)
    far = os.path.join(os.path.dirname(os.environ["STUB_ARGV"]), "far-ws")
    if name == "far-name":
        print("there\ts-far-1\tfar-name\t" + open(far).read().strip() + "\t3h")
        sys.exit(0)
    if name == "twin":
        ws = open(far).read().strip()
        print("there\ts-twin-1\ttwin-a\t" + ws + "\t3h")
        print("there\ts-twin-2\ttwin-b\t" + ws + "\t5d")
        sys.exit(0)
    sys.stderr.write("no session named %r\n" % name)
    sys.exit(1)
p = sys.argv[sys.argv.index("--socket") + 1]
try:
    os.unlink(p)
except FileNotFoundError:
    pass
s = socket.socket(socket.AF_UNIX)
s.bind(p)
s.listen(4)
def owner_alive():
    o = os.environ.get("LBT_OWNER")
    if not o:
        return True
    try:
        os.kill(int(o), 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass
    return True
while owner_alive():
    time.sleep(0.2)
STUB
  # The head the operator quits: it records what it was handed and exits at once. `--probe` is the
  # launcher's own question and answers 0 ("a daemon here, speaking this build").
  cat > "$T/bin/letibot-tui" << 'STUB'
#!/usr/bin/env bash
case " $* " in *" --probe "*) exit 0 ;; esac
printf 'tui %s\n' "$*" >> "$STUB_ARGV"
echo "letibot-tui: THE HEAD RAN"
STUB
  chmod +x "$T/bin/harnessd" "$T/bin/letibot-tui"

  # A `--resolve-session` call is the launcher's store query, not a daemon start.
  daemon_argv() { grep '^harnessd ' "$T/argv.log" 2> /dev/null | grep -v -e '--find-session' || true; }
  head_argv() { grep '^tui ' "$T/argv.log" 2> /dev/null || true; }
  # `printf '%s'`, no newline: the launcher hashes the bare workspace path, and a piped
  # `$(pwd -P)` would hash a different one.
  key_of() { printf '%s' "$(cd "$1" && pwd -P)" | key12; }
  pid_of() { # pid_of DIR -- the pid in that folder's record
    sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
      "$T/run/letibot/$(key_of "$1").json" 2> /dev/null | head -1
  }
  fresh() {
            : > "$T/argv.log"
                               rm -f "$T/run/letibot/"*.json
  }
  # run_tty ANSWER ARGS... -- the launcher on a pseudo-terminal that types ANSWER at its prompt.
  run_tty() {
    local ans="$1"
    shift
    printf '%s' "$DEFAULT_ONLY" > "$T/home/.config/letibot/providers.toml"
    (cd "${RUN_WS:-$T/ws}" && ANS="$ans" T_HOME="$T/home" T_RUN="$T/run" T_BIN="$T/bin" \
      T_ARGV="$T/argv.log" T_ROOM="$ROOM_SOCK" T_ROOM_ARGV="$T/room-argv.log" \
      T_BASH="$(command -v bash)" T_LAUNCHER="$LAUNCHER" PATH="$RUN_PATH" python3 - "$@" << 'PYTTY'
import os, pty, sys, time
env = {"PATH": os.environ["PATH"], "HOME": os.environ["T_HOME"], "XDG_RUNTIME_DIR": os.environ["T_RUN"],
       "LETIBOT_BIN": os.environ["T_BIN"], "LETIBOT_ENDPOINT": "", "STUB_ARGV": os.environ["T_ARGV"],
       "LETIBOT_ROOM_SOCKET": os.environ["T_ROOM"], "ROOM_ARGV": os.environ["T_ROOM_ARGV"]}
pid, fd = pty.fork()
if pid == 0:
    os.execvpe(os.environ["T_BASH"], [os.environ["T_BASH"], os.environ["T_LAUNCHER"]] + sys.argv[1:], env)
out = b""
sent = False
deadline = time.time() + 30
while time.time() < deadline:
    try:
        d = os.read(fd, 4096)
    except OSError:
        break
    if not d:
        break
    out += d
    if not sent and (b"[Y/n]" in out or b"cancels]" in out):
        os.write(fd, os.environ["ANS"].encode() + b"\n")
        sent = True
sys.stdout.write(out.decode(errors="replace").replace("\r", ""))
PYTTY
    )
  }
  stop_case() { # stop_case DIR -- this block's own daemon, and nothing else
    local p
    p="$(pid_of "$1")"
    [ -n "$p" ] && kill "$p" 2> /dev/null || true
  }

  # 1. A plain start opens a FRESH session. No `--session` on the daemon's command line is
  # what makes `harnessd` mint an id; a launcher that "helpfully" passed the newest would be the
  # operator's report, and nothing else in the start would look wrong.
  mkdir -p "$T/plain"
  fresh
  out="$(RUN_WS="$T/plain" run "$DEFAULT_ONLY")"
  refuse "a plain start does not pass the newest session to the daemon it starts" \
    "$(daemon_argv)" "--session"
  expect "  ...and the head runs" "$out" "THE HEAD RAN"
  refuse "  ...and the head is given no --resume of its own" "$(head_argv)" "--resume"
  # And the limit survives `detach` — the one-shot case above proves the launcher raised
  # its own limit; this proves the daemon it detached still has it (perl's `setsid`+`exec` on a
  # Mac, `systemd-run --scope setsid` on Linux, neither of which touches rlimits).
  limit_case "the daemon the launcher detaches gets the raised limit too" \
    "$(sed -n 's/^openfiles //p' "$T/argv.log" 2> /dev/null | head -1)" "$out"

  # 2. `--continue` is the verb that names one — and it names it on the daemon's command line,
  # where the resume (and its hash-chain check) happens at startup.
  mkdir -p "$T/cont"
  fresh
  out="$(RUN_WS="$T/cont" run "$DEFAULT_ONLY" --continue)"
  expect "--continue starts the daemon ON the session it picked" "$(daemon_argv)" \
    "--session s-latest"
  expect "  ...and the head runs" "$out" "THE HEAD RAN"

  # 3. `--new` with no daemon opens ONE session, not two. The start's first session IS the
  # fresh one, so the head must not also ask for one.
  mkdir -p "$T/new"
  fresh
  out="$(RUN_WS="$T/new" run "$DEFAULT_ONLY" --new)"
  refuse "a --new start does not name a session to resume" "$(daemon_argv)" "--session"
  refuse "  ...and the head is not asked for a second fresh session" "$(head_argv)" \
    "--new-session"
  expect "  ...and the head runs" "$out" "THE HEAD RAN"

  # 4. `--new` against a LIVE folder starts nothing and leaves the daemon alone. The argv log is
  # emptied; the record is kept, since it names the pid that must survive.
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
  if kill -0 "$live_pid" 2> /dev/null && listens "$T/run/letibot/$(key_of "$T/new").sock"; then
    echo "ok   ...and the daemon survives the head's exit"
  else
    echo "FAIL ...and the daemon survives the head's exit: pid $live_pid is gone or its socket went"
    fails=$((fails + 1))
  fi

  # 5. `--continue ID` is `--session ID`, and anything else after `--continue` is refused. An id
  # falling through would run a one-shot into the newest session. Pinned on argv (`--session`,
  # never `--prompt`); the id forms are the store's, `s-<digits>` with an optional `#tN`.
  for pair in "contid:s-1791394076177451000" "contid-t:s-1791394076177451000#t17"; do
    d="$T/${pair%%:*}"
                        want="${pair#*:}"
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

  # A session NAME is resolved by the daemon binary's store query, for both verbs, and the id it
  # answers is what the daemon is started on. An unresolvable one is refused and starts nothing.
  for verb in --continue --session; do
    d="$T/named${verb#--}"
    mkdir -p "$d"
    fresh
    out="$(RUN_WS="$d" run "$DEFAULT_ONLY" "$verb" my-work)"
    expect "$verb my-work opens the session that name means" "$(daemon_argv)" \
      "--session s-named-1( |\$)"
    refuse "  ...and the name itself is not passed on" "$(daemon_argv)" "--session my-work"
    stop_case "$d"
  done

  # An argument that is not a session id or a name is refused and nothing is started: a
  # one-shot's prompt is the FIRST argument and only the first.
  mkdir -p "$T/contbad"
  fresh
  out="$(RUN_WS="$T/contbad" run "$DEFAULT_ONLY" --continue please-reopen-it)"
  rc=$?
  expect "an unknown name after --continue is refused" "$out" "no session named 'please-reopen-it'"
  if [ "$rc" -ne 0 ]; then echo "ok   ...and exits non-zero"; else
    echo "FAIL ...and exits non-zero: exit $rc"
    fails=$((fails + 1))
  fi
  if [ -z "$(daemon_argv)" ]; then echo "ok   ...and starts no daemon with it as a prompt"; else
    echo "FAIL ...and starts no daemon with it as a prompt:"
    daemon_argv | sed 's/^/     /'
    fails=$((fails + 1))
  fi

  # A NAME FROM ANOTHER FOLDER IS OFFERED, NEVER TAKEN (`--continue NAME` anywhere). The stub
  # answers `far-name` with one session in $T/farws, and `twin` with two there.
  mkdir -p "$T/hereg"
  for verb in --continue --session; do
    # A folder per verb: the previous one's daemon may still be exiting.
    mkdir -p "$T/farws${verb#--}"
    printf '%s\n' "$T/farws${verb#--}" > "$T/far-ws"
    fresh
    out="$(RUN_WS="$T/hereg" run_tty y "$verb" far-name)"
    expect "$verb NAME from another folder asks, with the folder" "$out" \
      "far-name lives in .*farws[a-z]*\. Open it there\? \[Y/n\]"
    expect "  ...yes starts that folder's daemon on the session" "$(daemon_argv)" \
      "--workspace [^ ]*farws[a-z]* .*--session s-far-1( |\$)"
    expect "  ...and prints the cd line, saying why" "$out" "cannot change its parent shell.*cd [^ ]*farws[a-z]*"
    expect "  ...and the head runs" "$out" "THE HEAD RAN"
    stop_case "$T/farws${verb#--}"
  done
  mkdir -p "$T/farws"
  printf '%s\n' "$T/farws" > "$T/far-ws"
  fresh
  out="$(RUN_WS="$T/hereg" run_tty n --continue far-name)"
  expect "answering n opens nothing" "$out" "nothing opened"
  if [ -z "$(daemon_argv)" ] && [ -z "$(head_argv)" ]; then echo "ok   ...and starts no daemon or head"; else
    echo "FAIL ...and starts no daemon or head:"
    { daemon_argv; head_argv; } | sed 's/^/     /'
    fails=$((fails + 1))
  fi
  fresh
  out="$(RUN_WS="$T/hereg" run "$DEFAULT_ONLY" --session far-name)"
  rc=$?
  expect "with no terminal the folder is named and nothing opened" "$out" \
    "far-name lives in .*farws.*Nothing was opened"
  expect "  ...with the command to run there" "$out" \
    "cd [^ ]*farws && (leticode|letibot) --session s-far-1"
  if [ "$rc" -ne 0 ] && [ -z "$(daemon_argv)" ]; then echo "ok   ...exit non-zero, nothing started"; else
    echo "FAIL ...exit non-zero, nothing started: exit $rc"
    fails=$((fails + 1))
  fi
  fresh
  out="$(RUN_WS="$T/hereg" run "$DEFAULT_ONLY" --continue twin)"
  expect "several folders with no terminal: refused, with the list" "$out" "names 2 sessions"
  expect "  ...numbered, with folder and age" "$out" "1\) twin-a  .*farws  3h ago"
  expect "  ...every row" "$out" "2\) twin-b  .*farws  5d ago"
  expect "  ...and nothing opened" "$out" "nothing was opened"
  if [ -z "$(daemon_argv)" ]; then echo "ok   ...and nothing started"; else
    echo "FAIL ...and nothing started"
    fails=$((fails + 1))
  fi
  fresh
  out="$(RUN_WS="$T/hereg" run_tty 2 --continue twin)"
  expect "on a terminal the list asks which" "$out" "Open which\? \[1-2"
  expect "  ...and the pick opens that session" "$(daemon_argv)" "--session s-twin-2( |\$)"
  stop_case "$T/farws"

  for d in "$T/plain" "$T/cont" "$T/new"; do stop_case "$d"; done
else
  echo "skip the start/--new cases: no python3 to listen on a unix socket"
fi

# restart_keeps_model_and_provider: a restart opens the daemon it stopped, so the model and the
# provider it was STARTED with come off the record. The record's pid is one no process can have,
# so nothing is stopped and the case is the start alone. This is above the stop cases because they
# replace the printing harnessd stub with a python daemon that never prints its argv.
mkdir -p "$T/restart-model" "$T/home/logs" "$T/run/letibot"
_rm_ws="$(cd "$T/restart-model" && pwd -P)"
_rm_key="$(printf '%s' "$_rm_ws" | key12)"
printf '{"pid": 4000000000, "workspace": "%s", "socket": "%s", "model": "glm-5.1", "provider": "glm"}' \
  "$_rm_ws" "$T/run/letibot/$_rm_key.sock" > "$T/run/letibot/$_rm_key.json"
out="$(RUN_WS="$T/restart-model" run "$ONESHOT_MODELS" --restart)"
# The daemon the start ran records its own argv (`STUB_ARGV`), which is where a started daemon's
# flags are read: the launcher starts it behind `detach`, so its stdout is in its log.
_rm_started="$(grep '^harnessd ' "$T/argv.log" 2> /dev/null | tail -1 || true)"
expect "a restart keeps the provider the daemon was started with" "$_rm_started" \
  "^harnessd .*--provider glm( |$)"
expect "  ...and the model" "$_rm_started" "^harnessd .*--model glm-5\.1( |$)"
refuse "  ...and it did not stop the daemon a stale pid named" "$out" "^stopped "
kill "$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
  "$T/run/letibot/$_rm_key.json" 2> /dev/null | head -1)" 2> /dev/null || true
rm -f "$T/run/letibot/"*.json "$T/argv.log"

# ---------------------------------------------------------------------------
# A DAEMON THAT IS SHUTTING DOWN IS NOT A DAEMON IGNORING SIGTERM. Its orderly stop takes
# seconds and prints a `  stopping: ...` line per step; `--stop` waits thirty seconds relaying them.
if command -v python3 > /dev/null 2>&1; then
  mkdir -p "$T/home/logs" "$T/run/letibot" "$T/slow" "$T/stuck"
  # A daemon on its way out: it logs what it waits for where the launcher reads, takes seven
  # seconds, past the old five-second bound, and listens, since the start case needs one up.
  cat > "$T/bin/harnessd" << 'STUB'
#!/usr/bin/env python3
import hashlib, os, signal, socket, sys, time
if "--latest-session" in sys.argv:
    print("s-restart-1")
    sys.exit(0)
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
    # A daemon of this build reads the restart marker, which is what keeps its jobs.
    try:
        os.unlink(sock + ".restart")
    except FileNotFoundError:
        pass
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
def owner_alive():
    o = os.environ.get("LBT_OWNER")
    if not o:
        return True
    try:
        os.kill(int(o), 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass
    return True
while owner_alive():
    time.sleep(0.2)
STUB
  chmod +x "$T/bin/harnessd"
  # The tui stub: `--stop` calls it for the interrupt and the start path for its probe — both
  # answer 0 and say nothing — and any other invocation is the head, which says it ran.
  cat > "$T/bin/letibot-tui" << 'STUB'
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
      --workspace "$ws_p" --socket "$sock" ${extra:+"$extra"} > /dev/null 2>&1 &
    printf '{"pid": %s, "workspace": "%s", "socket": "%s"}' "$!" "$ws_p" "$sock" \
      > "$T/run/letibot/$key.json"
    echo "$!"
  }
  slow_daemon "$T/slow" > /dev/null
  out="$(RUN_WS="$T/slow" run "$DEFAULT_ONLY" --stop)"
  expect "a daemon that takes seven seconds to stop is waited for, not called stuck" \
    "$out" "^stopped "
  expect "  ...and the launcher relays what IT says it is waiting for" "$out" \
    "stopping: waiting up to 5s for subagent threads to leave"
  refuse "  ...and it is never called 'ignoring SIGTERM'" "$out" "ignoring SIGTERM"

  # AND THE TIMEOUT STILL COMES, with the advice only after it. A daemon wedged INSIDE a step
  # is exactly the case `--force` exists for; thirty seconds of patience is the difference
  # between a slow stop and a stuck one, so this case spends them.
  slow_daemon "$T/stuck" --stuck > /dev/null
  stuck_pid="$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    "$T/run/letibot/$(printf '%s' "$(cd "$T/stuck" && pwd -P)" | key12).json" | head -1)"
  out="$(RUN_WS="$T/stuck" run "$DEFAULT_ONLY" --stop)"
  expect "a daemon wedged in a step is given its whole thirty seconds" "$out" \
    "still running 30s after SIGTERM"
  expect "  ...its own last words are said" "$out" \
    "it last said: stopping: waiting up to 5s for subagent threads"
  expect "  ...and only THEN is --force offered" "$out" "sends SIGKILL"
  kill -9 "$stuck_pid" 2> /dev/null || true
  rm -f "$T/run/letibot/"*.json

  # A daemon on its way out for twelve seconds. `sleep 12; :`, not `sleep 12`: bash execs a
  # single-command string, and the argv naming the socket would go with it.
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
  kill "$going" 2> /dev/null || true
  kill "$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    "$T/run/letibot/$key.json" 2> /dev/null | head -1)" 2> /dev/null || true
  rm -f "$T/run/letibot/"*.json

  # A restart says each step as it happens, one line per step, in order, and a slow interrupt and a slow exit each say what they
  # wait for. Off a terminal, so no live redraws.
  cat > "$T/bin/letibot-tui" << 'STUB'
#!/usr/bin/env bash
for a in "$@"; do
  case "$a" in
    --probe) exit 0 ;;
    --interrupt-all)
      echo "  s-restart-1 · interrupted"
      sleep 2
      echo "all turns stopped"
      exit 0
      ;;
  esac
done
echo "letibot-tui: THE HEAD RAN"
STUB
  chmod +x "$T/bin/letibot-tui"
  mkdir -p "$T/restart"
  restart_pid="$(slow_daemon "$T/restart")"
  out="$(RUN_WS="$T/restart" run "$DEFAULT_ONLY" --restart)"
  in_order "--restart says its steps in order" "$out" \
    "^restart: interrupting 1 running turn \(s-restart-1\)$" \
    "^restart: interrupting 1 running turn \(s-restart-1\) · stopped \([0-9]+\.[0-9]s\)$" \
    "^restart: waiting for the daemon to exit \(pid $restart_pid\)" \
    "^restart: the daemon exited \(pid $restart_pid, [0-9]+\.[0-9]s\)$" \
    "^restart: starting harnessd \(built [0-9]{2}:[0-9]{2}\) · up \(" \
    "^restart: attaching$" \
    "THE HEAD RAN"
  expect "  ...and the slow exit names what the daemon waits on" "$out" \
    "^restart: waiting for the daemon to exit \(pid $restart_pid\) - waiting up to 5s for subagent threads"
  refuse "  ...and none of the plain stop's and start's lines" "$out" \
    "^stopped |no harnessd for|all turns stopped|predates --restart"
  kill "$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    "$T/run/letibot/$(printf '%s' "$(cd "$T/restart" && pwd -P)" | key12).json" 2> /dev/null | head -1)" 2> /dev/null || true
  rm -f "$T/run/letibot/"*.json
else
  echo "skip the stop and start-wait cases: no python3"
fi

# ---------------------------------------------------------------------------
# install_and_launcher_name_the_same_binary: `~/.local/bin/letibot` links into a checkout with
# its own target/release, and the binaries beside the command typed are the ones run.
mkdir -p "$T/co/scripts" "$T/co/target/release" "$T/inst"
cp "$LAUNCHER" "$T/co/scripts/letibot"
for d in co/target/release inst; do
  printf '#!/bin/sh\n[ $# -eq 0 ] && exit 0\necho "harnessd from %s"\n' "$d" > "$T/$d/harnessd"
  chmod +x "$T/$d/harnessd"
  cp "$T/bin/letibot-multi.default" "$T/$d/letibot-multi"
  cp "$T/bin/where-json" "$T/$d/where-json"
done
ln -sf "$T/co/scripts/letibot" "$T/inst/letibot"
out="$(cd "$T/ws" && env -i PATH="$RUN_PATH" HOME="$T/home" XDG_RUNTIME_DIR="$T/run" \
  LETIBOT_ROOM_SOCKET="$ROOM_SOCK" "$(command -v bash)" "$T/inst/letibot" "q" 2>&1 < /dev/null)"
expect "install_and_launcher_name_the_same_binary: a linked launcher runs the installed build" \
  "$out" "^harnessd from inst$"
out="$(cd "$T/ws" && env -i PATH="$RUN_PATH" HOME="$T/home" XDG_RUNTIME_DIR="$T/run" \
  LETIBOT_ROOM_SOCKET="$ROOM_SOCK" "$(command -v bash)" "$T/co/scripts/letibot" "q" 2>&1 < /dev/null)"
expect "  ...and the checkout's own script runs its target/release" "$out" "^harnessd from co/target/release$"

# A SESSION THAT LIVES ON A HOST (`docs/remote-hosts.md` RH4): the verb passes through, and an
# attach goes to the forwarded socket the rundir record names, not to this folder's daemon.

# Which socket this folder's daemon listens on, for programs that drive letibot. The launcher
# hands the folder to the binary's resolver (`crates/letibot/tests/where.rs` holds the equality).
out="$(run "$DEFAULT_ONLY" --where --ensure --json)"
expect "--where is the resolver verb's, with this folder and both modifiers" "$out" \
  "^letibot-multi where .*/ws --ensure --json$"
refuse "  ...and the launcher starts nothing of its own for it" "$out" "^harnessd "
out="$(run "$DEFAULT_ONLY" --where)"
expect "--where with no modifier asks the same question" "$out" "^letibot-multi where .*/ws$"
for modifier in --ensure --json; do
  out="$(run "$DEFAULT_ONLY" "$modifier")"
                                                  rc=$?
  refusal "$modifier alone is refused by name, as --force is" "$rc" "$out" \
    "^letibot: $modifier only means something with --where" "--where"
done

out="$(run "$DEFAULT_ONLY" --migrate s-4242 --to lab2x1)"
expect "--migrate is handed to the migration verb with this box's store" "$out" \
  "^letibot-multi migrate s-4242 --to lab2x1 --store "
refuse "  ...and the launcher never starts a daemon for it" "$out" "^harnessd "

out="$(run "$DEFAULT_ONLY" --reattach s-4242)"
expect "--reattach is the same verb's other spelling" "$out" \
  "^letibot-multi migrate --reattach s-4242 --store "

# `attach github S` is a verb too, and the reason is the same: a word the launcher does not
# know is sent to the model as a prompt, which is a billed turn for a typo.
out="$(run "$DEFAULT_ONLY" attach github stroppy-io)"
expect "attach github is handed to the reviewer seat" "$out" \
  "^letibot-multi attach github stroppy-io$"
refuse "  ...and the launcher starts nothing for it" "$out" "^harnessd "

out="$(run "$DEFAULT_ONLY" attach gitlab stroppy-io)"
expect "  ...and a source that is not github is refused by name" "$out" \
  "attach knows one source and it is .github"

# The record a move writes. Its socket does not exist here, so the launcher re-establishes the
# forward first and then hands the head the forwarded socket.
mkdir -p "$T/run/letibot"
# A head stub that says what it was handed — the last case above leaves one that only says
# it ran, and what this case is about is the ARGUMENTS.
cat > "$T/bin/letibot-tui" << 'STUB'
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

# ---------------------------------------------------------------------------
# THE LAUNCHER STARTS THE BOX'S ROOM, before a session opens, since `room` is seated at open.
# A run starts one and a second finds it; `--daemons`/`--ls` see it; a failed start prints the
# coordinator's words and the run goes on; `--room --stop` and `--stop --all` stop it.
# One per box is the lease's guarantee, measured in `crates/room/src/lease.rs`.
if command -v python3 > /dev/null 2>&1; then
  room_argv() { grep '^room ' "$T/room-argv.log" 2> /dev/null || true; }
  # The room-capable daemon stub, restored — the sections above replace `$T/bin/harnessd`
  # (one of them with a daemon that LISTENS and never exits, which would hang a one-shot here).
  room_stub() {
    cat > "$T/bin/harnessd" << 'STUB'
#!/usr/bin/env bash
if [ "${1:-}" = "--room" ]; then
  exec -a harnessd python3 "$(dirname "$0")/harnessd-room.py" --room
fi
printf 'harnessd'; printf ' %q' "$@"; printf '\n'
STUB
    chmod +x "$T/bin/harnessd"
  }
  # A room of this case's own. The suite's shared one has been up since its first case, and
  # "the launcher starts one" cannot be seen against a socket that is already serving — the old
  # binder holds it even after its file is removed, which is exactly the property `live` documents.
  room_kill() {
    local p i=0
    p="$(cat "$T/room/pid" 2> /dev/null || true)"
    if [ -n "$p" ]; then
      kill "$p" 2> /dev/null || true
      while [ "$i" -lt 40 ] && kill -0 "$p" 2> /dev/null; do
                                                            sleep 0.05
                                                                        i=$((i + 1))
      done
    fi
    rm -f "$T/room/room.sock" "$T/room/room.lease" "$T/room/pid" "$T/run/letibot/room.json"
  }
  room_stub
  room_kill
  : > "$T/room-argv.log"
  mkdir -p "$T/roomws"
  out="$(RUN_WS="$T/roomws" run "$DEFAULT_ONLY" --room)"
  expect "--room starts the box's coordinator" "$out" "^letibot: the room is up"
  expect "  ...and says which socket it serves" "$out" "socket +$ROOM_SOCK"
  expect "  ...and names the lease the coordinator holds" "$out" "lease  *$T/room/room.lease"
  expect "  ...and is handed --room and nothing else" "$(room_argv)" "^room --room$"
  # The record, in the launcher's own shape — `role: room` is what tells it from a folder's.
  if grep -q '"role": "room"' "$T/run/letibot/room.json" 2> /dev/null; then
    echo "ok   ...and writes the record \`--daemons\` reads, marked role room"
  else
    echo "FAIL ...and writes the record \`--daemons\` reads:"
    sed 's/^/     /' "$T/run/letibot/room.json" 2> /dev/null || echo "     (no record at all)"
    fails=$((fails + 1))
  fi
  # And it outlives the run that asked. The launcher has exited by the time this reads the
  # pid — that is the whole of "a coordinator that dies with its asker is worse than none".
  room_pid="$(sed -n 's/.*"pid": *\([0-9][0-9]*\).*/\1/p' "$T/run/letibot/room.json" 2> /dev/null | head -1)"
  if [ -n "$room_pid" ] && kill -0 "$room_pid" 2> /dev/null; then
    echo "ok   ...and the coordinator is still alive after the launcher exited (pid $room_pid)"
  else
    echo "FAIL ...and the coordinator is still alive after the launcher exited (pid '${room_pid:-none}')"
    fails=$((fails + 1))
  fi

  # A second run starts NOTHING — one per box, and the check is the coordinator's own argv log.
  : > "$T/room-argv.log"
  out="$(RUN_WS="$T/roomws" run "$DEFAULT_ONLY" --room)"
  refuse "a second run does not start a second coordinator" "$out" "no room on this box"
  if [ -z "$(room_argv)" ]; then echo "ok   ...and nothing was started over it"; else
    echo "FAIL ...and nothing was started over it:"
                                                     room_argv | sed 's/^/     /'
    fails=$((fails + 1))
  fi
  out="$(RUN_WS="$T/roomws" run "$DEFAULT_ONLY" "one question")"
  refuse "  ...nor does a plain run" "$out" "no room on this box"
  if [ -z "$(room_argv)" ]; then echo "ok   ...and a plain run starts nothing either"; else
    echo "FAIL ...and a plain run starts nothing either:"
                                                           room_argv | sed 's/^/     /'
    fails=$((fails + 1))
  fi

  # The launcher's own tools see it: a daemon the operator's tools cannot see is a daemon they cannot stop. `--ls` is where that shows: one
  # numbered row for the box's room, as for any folder's daemon.
  out="$(RUN_WS="$T/roomws" run "$DEFAULT_ONLY" --ls)"
  expect "--ls numbers it as the box's room" "$out" "the box's room +up"

  # A START THAT FAILS IS SAID, NOT SWALLOWED: the loser of the lease exits with the holder's
  # name, the launcher prints it and the run goes on.
  room_kill
  cat > "$T/bin/harnessd" << 'STUB'
#!/usr/bin/env bash
if [ "${1:-}" = "--room" ]; then
  echo "harnessd: another room coordinator is alive here: box/4242/1 (pid 4242) holds /x/room.lease and beat 3s ago. Two coordinators are two rooms with one name, so this one will not start." >&2
  exit 1
fi
printf 'harnessd'; printf ' %q' "$@"; printf '\n'
STUB
  chmod +x "$T/bin/harnessd"
  : > "$T/room-argv.log"
  out="$(RUN_WS="$T/roomws" run "$DEFAULT_ONLY" "one question")"
  expect "a coordinator that will not start is SAID" "$out" "the box's room did not start"
  expect "  ...in the coordinator's own words, naming the holder" "$out" \
    "another room coordinator is alive here: box/4242/1"
  expect "  ...and the run goes on regardless" "$out" "^harnessd .*--prompt one"
  refuse "  ...and the tool's refusal is not replaced by silence" "$out" "the room is up"

  # And `--room --stop` stops it — the record goes with the process it names.
  room_stub
  RUN_WS="$T/roomws" run "$DEFAULT_ONLY" --room > /dev/null 2>&1
  room_pid="$(sed -n 's/.*"pid": *\([0-9][0-9]*\).*/\1/p' "$T/run/letibot/room.json" 2> /dev/null | head -1)"
  out="$(RUN_WS="$T/roomws" run "$DEFAULT_ONLY" --room --stop)"
  expect "--room --stop stops the box's room" "$out" "^stopped the room"
  if [ ! -f "$T/run/letibot/room.json" ]; then echo "ok   ...and its record goes with it"; else
    echo "FAIL ...and its record goes with it"
                                                fails=$((fails + 1))
  fi
  if [ -n "$room_pid" ] && kill -0 "$room_pid" 2> /dev/null; then
    echo "FAIL ...and the process is gone (pid $room_pid is still there)"
                                                                           fails=$((fails + 1))
  else
    echo "ok   ...and the process is gone"
  fi

  # `--stop --all` sees it too - a stop must not leave a daemon running.
  RUN_WS="$T/roomws" run "$DEFAULT_ONLY" --room > /dev/null 2>&1
  out="$(RUN_WS="$T/roomws" run "$DEFAULT_ONLY" --stop --all)"
  expect "--stop --all stops the box's room" "$out" "stopped .*room-workspace"
  if [ ! -f "$T/run/letibot/room.json" ]; then echo "ok   ...and its record goes with it"; else
    echo "FAIL ...and its record goes with it"
                                                fails=$((fails + 1))
  fi

  # `--stop N` stops it by the number `--ls` printed. The room's record is `room-<host key>.json`,
  # not workspace-keyed, so `retarget` must read the record the number names.
  RUN_WS="$T/roomws" run "$DEFAULT_ONLY" --room > /dev/null 2>&1
  n="$(RUN_WS="$T/roomws" run "$DEFAULT_ONLY" --ls | grep -F "the box's room" | awk '{ print $1; exit }')"
  if [ -n "$n" ]; then echo "ok   ...and --ls gives it a number ($n)"; else
    echo "FAIL ...and --ls gives it a number: no room row in:"
    RUN_WS="$T/roomws" run "$DEFAULT_ONLY" --ls | sed 's/^/     /'
                                                                    fails=$((fails + 1))
  fi
  # A `letibot-tui` that refuses, because the real one does: a coordinator's socket speaks the
  # room's protocol, so the session protocol's interrupt comes back a refusal. That is what makes
  # the check below a measurement rather than a sentence the suite cannot falsify.
  cat > "$T/bin/letibot-tui" << 'STUB'
#!/usr/bin/env bash
echo "letibot-tui: this socket does not speak the session protocol" >&2
exit 1
STUB
  chmod +x "$T/bin/letibot-tui"
  out="$(RUN_WS="$T/roomws" run "$DEFAULT_ONLY" --stop "$n")"
  expect "--stop N stops the box's room by its number" "$out" "^stopped the box's room"
  refuse "  ...without sending it the session protocol's interrupt" "$out" "interrupt did not go through"
  if [ ! -f "$T/run/letibot/room.json" ]; then echo "ok   ...and its record goes with it"; else
    echo "FAIL ...and its record goes with it"
                                                fails=$((fails + 1))
  fi
  cp "$T/bin/harnessd" "$T/bin/letibot-tui"

  # The host key against the table `letibot_room::lease` pins: a disagreeing key would look for
  # a socket no coordinator binds and start one every run.
  mkdir -p "$T/unamebin"
  cat > "$T/unamebin/uname" << 'STUB'
#!/usr/bin/env bash
if [ "${1:-}" = "-n" ]; then printf '%s\n' "MacBook-Pro"; exit 0; fi
exec /usr/bin/uname "$@"
STUB
  chmod +x "$T/unamebin/uname"
  out="$(ROOM_SOCK_OVERRIDE='' RUN_PATH_EXTRA="$T/unamebin:" RUN_WS="$T/roomws" \
    run "$DEFAULT_ONLY" --room --stop 2>&1)"
  # `--room --stop` with no `LETIBOT_ROOM_SOCKET` is the derivation and nothing else: it names the
  # socket the launcher would look at on a box whose `uname -n` is MacBook-Pro.
  expect "the launcher derives the room's socket from the host, as the Rust does" "$out" \
    "no room on this box \(.*/room-macbook-pro-45a1c8db.sock\)"
else
  echo "skip the room cases: no python3 to bind a unix socket"
fi

# A VERB OF THE BINARY IS A VERB, NOT A PROMPT: `stats`, `fb`, `boundary` and `migrate` belong
# to `letibot-multi`; without an arm they would become a billed one-shot.
out="$(run "$DEFAULT_ONLY" stats --since 1d)"
expect "stats is handed to the metrics verb" "$out" "^letibot-multi stats --since 1d$"
refuse "  ...and no daemon is started for it" "$out" "^harnessd "
refuse "  ...and nothing is sent as a prompt" "$out" "--prompt"

out="$(run "$DEFAULT_ONLY" fb --session s-1 + good)"
expect "fb marks the turn through the verb" "$out" "^letibot-multi fb --session s-1 \+ good$"

out="$(run "$DEFAULT_ONLY" boundary --mode read-only)"
expect "boundary is the boundary verb's" "$out" "^letibot-multi boundary --mode read-only$"

# launcher_routes_hold_release_where_to_multicall: `letibot hold s-123` is what the idle-close
# notice tells the operator to type. Every verb the help names reaches the multicall, never harnessd.
for spec in "holds --json" "gc --log" "where" "hold s-123" "release s-123" "notes path" "doctor --json" "store forget-import s-123"; do
  v="${spec%% *}"
  # shellcheck disable=SC2086 # each spec is a word list on purpose
  out="$(run "$DEFAULT_ONLY" $spec)"
  expect "$spec is the multicall's $v verb" "$out" "^letibot-multi $v( |$)"
  refuse "  ...and $v is not sent as a prompt" "$out" "--prompt"
done

# letibot-multi_answers_the_verb_list_the_launcher_reads: the multicall owns the list of words
# that are verbs, and the launcher refuses one it has no arm for instead of billing it as a turn.
out="$(run "$DEFAULT_ONLY" --verbs)"
expect "--verbs is handed to the multicall" "$out" "^letibot-multi --verbs$"
refuse "  ...and is not a prompt" "$out" "--prompt"
cat > "$T/bin/letibot-multi.verbs" << 'STUB'
#!/usr/bin/env bash
if [ "${1:-}" = --verbs ]; then printf 'hold\nfrobnicate\n'; exit 0; fi
exec "$(dirname "$0")/letibot-multi.default" "$@"
STUB
chmod +x "$T/bin/letibot-multi.verbs"
cp "$T/bin/letibot-multi.verbs" "$T/bin/letibot-multi"
out="$(run "$DEFAULT_ONLY" frobnicate)"
rc=$?
refusal "a verb the launcher has no arm for is refused, not billed as a prompt" "$rc" "$out" \
  "^letibot: .frobnicate. is a verb of " "no arm for it"
refuse "  ...and nothing was run" "$out" "--prompt"
cp "$T/bin/letibot-multi.default" "$T/bin/letibot-multi"

# The store follows XDG_DATA_HOME, as gc, hold and the bridge do: one store for the daemon and
# the verbs that act on its owners.
out="$(RUN_XDG_DATA_HOME="$T/xdgdata" run "$DEFAULT_ONLY" "q")"
expect "a one-shot's --store is under XDG_DATA_HOME" "$out" "--store $T/xdgdata/letibot( |$)"
out="$(RUN_XDG_DATA_HOME="$T/xdgdata" run "$DEFAULT_ONLY" hold s-123)"
expect "  ...and hold is handed that store" "$out" "^store=$T/xdgdata/letibot$"
out="$(RUN_XDG_DATA_HOME="relative" run "$DEFAULT_ONLY" "q")"
expect "  ...and a relative XDG_DATA_HOME is unset, as lifecycle::data_dir has it" "$out" \
  "--store $T/home/.local/share/letibot( |$)"

out="$(run "$DEFAULT_ONLY" --config-reference --markdown providers.toml)"
expect "--config-reference is the multicall's" "$out" "^letibot-multi --config-reference --markdown providers.toml$"
refuse "  ...and no daemon is started for it" "$out" "^harnessd "
out="$(run "$DEFAULT_ONLY" --config-header leticode.toml --as leticode.toml)"
expect "--config-header is the multicall's" "$out" "^letibot-multi --config-header leticode.toml --as leticode.toml$"

out="$(run "$DEFAULT_ONLY" migrate --help)"
expect "migrate reaches its own help, with this box's store" "$out" \
  "^letibot-multi migrate --help --store "

# `--tsv` IS THE LISTING'S OTHER FORM, NOT A FLAG TO DROP. The arm took no arguments, so
# `--sessions --tsv` answered the grouped view and said nothing about the flag.
out="$(run "$DEFAULT_ONLY" session list --tsv)"
expect "session list --tsv hands --tsv on to the listing" "$out" \
  "^letibot-multi session list --tsv --store \S+$"
refuse "  ...and starts no daemon for it" "$out" "^harnessd "

# EACH SESSION VERB, AND EACH OLD FLAG RUNS THE SAME LINE: the alias adds one
# note on stderr and nothing else.
for spec in "session list|--sessions" "session list|--list" "session list --archived|--sessions --archived" \
  "session rename s-1 t|--rename s-1 t" "session archive s-1 why|--archive s-1 why" \
  "session unarchive s-1|--unarchive s-1" "session delete s-1|--delete s-1" "session show s-1|"; do
  new="${spec%%|*}"
  old="${spec#*|}"
  # shellcheck disable=SC2086 # each side is a word list on purpose
  new_out="$(run "$DEFAULT_ONLY" $new)"
  expect "$new reaches the session verb" "$new_out" "^letibot-multi $new --store \S+$"
  [ -n "$old" ] || continue
  # shellcheck disable=SC2086
  old_out="$(run "$DEFAULT_ONLY" $old)"
  if [ "$(printf '%s\n' "$old_out" | grep -v '^note: ')" = "$new_out" ]; then
    echo "ok   $old runs the same line as $new"
  else
    echo "FAIL $old runs something else than $new:"
    printf '%s\n' "$old_out" "--- $new:" "$new_out" | sed 's/^/     /'
    fails=$((fails + 1))
  fi
  expect "  ...and says its new spelling" "$old_out" "^note: .letibot ${old%% *}. is now .letibot session "
done
out="$(run "$DEFAULT_ONLY" session bogus)"
expect "session with an unknown verb is refused by name" "$out" "^letibot: session has no verb bogus"

# `--help` FITS ON A SCREEN, AND EVERY FLAG IT NAMES PARSES, on both pages.
# Runs last: several flags start and stop a daemon, and a leftover record would reach the cases above.
lines=$(run "$DEFAULT_ONLY" --help | wc -l | tr -d ' ')
if [ "$lines" -le 25 ]; then
  echo "ok   --help fits in 25 lines ($lines)"
else
  echo "FAIL --help is $lines lines, and the item allows 25"
  fails=$((fails + 1))
fi
flags="$( (run "$DEFAULT_ONLY" --help; run "$DEFAULT_ONLY" --help all) | grep -oE -- '--[a-z][a-z-]*' | sort -u)"
if [ -z "$flags" ]; then
  echo "FAIL every flag --help names parsed: --help printed no flag at all"
  fails=$((fails + 1))
fi
bad=0
for f in $flags; do
  case "$f" in
    --mem | --vcpu | --add-dir | --resume) continue ;;
  esac
  out="$(run "$DEFAULT_ONLY" "$f")"
  case "$out" in
    *"unknown flag: $f"*)
      echo "FAIL every flag --help names parsed: the help offers $f and the parser refuses it:"
      printf '%s\n' "$out" | sed 's/^/     /'
      bad=$((bad + 1))
      ;;
  esac
done
if [ "$bad" -eq 0 ]; then
  echo "ok   every flag --help names is one the parser reads ($(printf '%s\n' "$flags" | wc -l | tr -d ' ') of them)"
else
  fails=$((fails + bad))
fi

# A SCOPE THAT CANNOT BE MADE IS THE MANAGER'S OWN SENTENCE, NOT A FALSE ABSENCE. A manager at
# its unit limit says `Argument list too long`, a pegged one says nothing, so the launcher says
# the first and bounds the second. Stubs shadow `systemd-run`, and the run is the one-shot.
if [ "$(uname -s)" = Linux ]; then
  mkdir -p "$T/noscope"
  cat > "$T/noscope/systemd-run" << 'STUB'
#!/usr/bin/env bash
# The manager's own last line, which is the reason systemd puts where a person looks.
echo "Failed to start transient scope unit: Argument list too long" >&2
exit 1
STUB
  chmod +x "$T/noscope/systemd-run"
  out="$(RUN_PATH_EXTRA="$T/noscope:" run "$DEFAULT_ONLY" "one question")"
  expect "a refused scope is the manager's own words" "$out" \
    "the user manager refused a scope: Failed to start transient scope unit: Argument list too long"
  refuse "  ...and not an absence of the manager that refused" "$out" "no systemd user manager here"
  expect "  ...and the run goes on without a scope" "$out" "^harnessd .*--prompt one"

  cat > "$T/noscope/systemd-run" << 'STUB'
#!/usr/bin/env bash
# A pegged manager: it takes the ask and never answers it.
exec sleep 60
STUB
  chmod +x "$T/noscope/systemd-run"
  started="$(date +%s)"
  out="$(RUN_PATH_EXTRA="$T/noscope:" run "$DEFAULT_ONLY" "one question")"
  spent=$(($(date +%s) - started))
  expect "a manager that never answers is given up on" "$out" \
    "the systemd user manager is not answering"
  if [ "$spent" -le 30 ]; then
    echo "ok   ...within seconds of the probe's bound (${spent}s)"
  else
    echo "FAIL ...within seconds of the probe's bound: the launcher waited ${spent}s"
                                                                        fails=$((fails + 1))
  fi
  expect "  ...and the run goes on there too" "$out" "^harnessd .*--prompt one"
fi

# THE SCOPE CARRIES THE SLICE, AND THE BOUND WHEN ONE IS ASKED FOR (`docs/lifecycle.md` d.7).
# The stub records the words it was given; reading the unit back needs a user manager, and is
# skipped with that said.
if [ "$(uname -s)" = Linux ]; then
  mkdir -p "$T/scope"
  cat > "$T/scope/systemd-run" << STUB
#!/usr/bin/env bash
# Every ask, one line: the probe, the bound's probe, and the real scope. It records and answers
# yes without running anything, so the scope's own words can be read and no daemon starts here.
printf '%s\\n' "\$*" >> "$T/scope/said"
exit 0
STUB
  chmod +x "$T/scope/systemd-run"
  : > "$T/scope/said"
  out="$(RUN_PATH_EXTRA="$T/scope:" LETIBOT_SCOPE_RUNTIME_MAX=25h run "$DEFAULT_ONLY" "one question")"
  scope_line="$(grep -c -- '--slice' "$T/scope/said" 2>/dev/null || true)"
  if [ "${scope_line:-0}" -gt 0 ]; then
    echo "ok   the daemon's scope names letibot.slice"
  else
    echo "FAIL the daemon's scope names letibot.slice; the stub was given:"
    sed 's/^/     /' "$T/scope/said"
    fails=$((fails + 1))
  fi
  if grep -q -- 'RuntimeMaxSec=25h' "$T/scope/said" 2>/dev/null; then
    echo "ok   ...and the bound its caller asked for"
  else
    echo "FAIL a package daemon's scope carries the runtime bound:"
    sed 's/^/     /' "$T/scope/said"
    fails=$((fails + 1))
  fi
  # And the bound is ASKED FOR, not assumed: a manager that refuses it leaves a daemon that still
  # starts, with a sentence saying the hard TTL is the reaper's.
  cat > "$T/scope/systemd-run" << STUB
#!/usr/bin/env bash
case "\$*" in
  *RuntimeMaxSec*) echo "Failed to start transient scope unit: Unknown assignment RuntimeMaxSec" >&2; exit 1 ;;
esac
exit 0
STUB
  chmod +x "$T/scope/systemd-run"
  out="$(RUN_PATH_EXTRA="$T/scope:" LETIBOT_SCOPE_RUNTIME_MAX=25h run "$DEFAULT_ONLY" "one question")"
  expect "a manager that refuses the bound is said, not fatal" "$out" \
    "would not put a runtime bound on a scope"
fi

if [ "$fails" -ne 0 ]; then
  echo "launcher: $fails failed"
  exit 1
fi
echo "launcher: all cases pass"
