#!/usr/bin/env bash
# **One Esc in a RUNNING subagent's session goes up — and does not arm the interrupt.**
# The operator, 2026-10-08: *"i went to subagent and then wanted to get back to the main
# session - pressed esc and it didnt work, pressed second time - subagent stopped"*. The
# child's turn is held open by the scripted model, so it is mid-turn when the spec walks in.
. "$(dirname -- "${BASH_SOURCE[0]}")/../lib.sh"
ACCEPTANCE_COLS="${ACCEPTANCE_COLS:-160}"
[ -x "$ROOT/target/release/harnessd" ] || { echo "acceptance: cargo build --release first"; exit 2; }

fake_model
start_session --new

spec "the parent starts a subagent that is still thinking"
TIMEOUT=30 wait_for "Type a question"
type_text "PARENT-SLOW IN-VM: start a helper"
press Enter
wait_for "parent: dispatched the slow subagent."

spec "ctrl-g, enter: into the running child"
press C-g
TIMEOUT=60 wait_for "CHILD-SLOW"
# A VM boots before its session can be attached to: the row says `opening` until then.
TIMEOUT=60 wait_for "· running"
press Enter
wait_for "subagent of"

spec "one esc goes up, while the child is still running"
press Escape
TIMEOUT=10 wait_gone "subagent of"
expect "CHILD-SLOW: think for a while."

done_spec
