#!/usr/bin/env bash
# **A subagent in a firecode VM, end to end** — LOCAL ONLY: it boots a VM, which a CI
# runner cannot, and needs firecode on this box. `tests/acceptance/run.sh --local` runs it.
#
# The real launcher, daemon and head, on the scripted model (`fakemodel.py`): the parent
# calls `task(where: firecode)`, the child runs `uname -s` in its shell, and the parent says
# what the child reported. `Linux` coming back to a Mac is the proof the command ran in the
# guest and not on the host. Found two defects the day it was written: firecode had no
# `layer inherit`, and died silently with HOME cleared on macOS (no getent).
# Wide, so the task row keeps its placement in view rather than an ellipsis.
ACCEPTANCE_COLS="${ACCEPTANCE_COLS:-200}"
. "$(dirname -- "${BASH_SOURCE[0]}")/../lib.sh"

if ! command -v firecode >/dev/null 2>&1; then
  echo "  skip  no firecode on this box"
  exit 0
fi
[ -x "$ROOT/target/release/harnessd" ] || { echo "acceptance: cargo build --release first"; exit 2; }

fake_model
start_session --new

spec "a fresh session on the scripted model"
TIMEOUT=30 wait_for "Type a question"

spec "the parent hands the work to a subagent in a VM"
type_text "PARENT-FIRECODE: dispatch the uname check to a VM"
press Enter
wait_for "▸ task"
expect_line "▸ task" "firecode"
wait_for "parent: dispatched the subagent to a firecode VM."

spec "the child ran in the guest, and the parent says what it reported"
TIMEOUT=180 wait_for "parent: the subagent reported child-done: Linux"
expect "from-the-vm"

done_spec
