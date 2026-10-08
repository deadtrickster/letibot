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
type_text "PARENT-SLOW: start a helper"
press Enter
wait_for "parent: dispatched the slow subagent."

spec "ctrl-g, enter: into the running child"
press C-g
TIMEOUT=20 wait_for "CHILD-SLOW"
press Enter
wait_for "subagent of"

spec "one esc goes up, while the child is still running"
press Escape
TIMEOUT=10 wait_gone "subagent of"
expect "CHILD-SLOW: think for a while."

spec "with a draft in the composer, esc still goes up — and the child keeps running"
# The operator's own sequence: in the child, something in the composer, Esc did nothing and
# the second Esc stopped the child. The draft goes up with them; nothing is interrupted.
press Escape
press C-g
wait_for "CHILD-SLOW"
press Enter
wait_for "subagent of"
type_text "a draft"
press Escape
TIMEOUT=10 wait_gone "subagent of"
expect "a draft"
# Landed on the parent's subagents list, which says the child is still at work.
expect "· running"
expect_not "interrupted"

done_spec
