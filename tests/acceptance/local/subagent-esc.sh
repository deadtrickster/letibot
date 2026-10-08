#!/usr/bin/env bash
# **One Esc in a subagent's session goes back up to the parent** — a FINISHED subagent's
# too, which the daemon no longer lists with its parent (`0c841de` was the first fix; this
# spec is the operator's *"Esc from subagent doesnt go up, we fixed it once already"*,
# 2026-10-08). The real launcher, daemon and head on the scripted model.
. "$(dirname -- "${BASH_SOURCE[0]}")/../lib.sh"
ACCEPTANCE_COLS="${ACCEPTANCE_COLS:-160}"
[ -x "$ROOT/target/release/harnessd" ] || { echo "acceptance: cargo build --release first"; exit 2; }

fake_model
start_session --new

spec "the parent starts a subagent"
TIMEOUT=30 wait_for "Type a question"
type_text "PARENT-HOST: start a helper"
press Enter
wait_for "parent: dispatched the subagent."

spec "it finishes, and ctrl-g still leads into its session"
press C-g
TIMEOUT=60 wait_for "finished (1)"
press Enter
wait_for "CHILD-SAY"
press Down
press Enter
TIMEOUT=20 wait_for "child-done: said hello from the subagent"
wait_for "subagent of"
wait_gone "parent: dispatched the subagent."

spec "one esc goes back up: the parent's session, on its subagents list"
press Escape
# Out of the child: its header's "subagent of …" is gone, and the parent's list is open
# on the child it came from.
TIMEOUT=15 wait_gone "subagent of"
expect "PARENT-HOST: start a helper"
expect "[x] CHILD-SAY"

spec "and esc again closes the list onto the parent's conversation"
press Escape
wait_for "parent: dispatched the subagent."

done_spec
