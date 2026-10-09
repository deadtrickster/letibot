#!/usr/bin/env bash
# **The project's agent can see its repository's merge gate and the choices for one.**
#
# The operator, 2026-10-09: *"make the gate configurable per repo, and let main project agent
# manage it … all heuristics can be presented as choices"*. A real session, seated as leticode,
# calls `merge_gate` in a repository with a Makefile `test` target and a CI workflow, and what it
# gets back names both, says there is no gate yet, and gives the section to write.
. "$(dirname -- "${BASH_SOURCE[0]}")/../lib.sh"
[ -x "$ROOT/target/release/harnessd" ] || { echo "acceptance: cargo build --release first"; exit 2; }

fake_model
mkdir -p "$WORK/ws/.github/workflows"
printf 'test:\n\techo ok\n' >"$WORK/ws/Makefile"
printf 'on: push\n' >"$WORK/ws/.github/workflows/ci.yml"
start_session --new

spec "the agent asks for the gate"
TIMEOUT=30 wait_for "Type a question"
type_text "PARENT-GATE: what should this repo's merge gate be?"
press Enter
TIMEOUT=20 wait_for "parent: the gate choices are in."

spec "it was told what the repository suggests"
result="$(tail -1 "$WORK/model.log")"
for want in "no AGENTS.md" "make test" "Makefile has a \`test\` target" "act" "ci.yml" "## Merge gate" "a command the operator types"; do
  case "$result" in *"$want"*) pass "names: $want" ;; *) fail "missing: $want — $result" ;; esac
done

done_spec
