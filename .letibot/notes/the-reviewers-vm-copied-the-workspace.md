<!-- abstract: The review queue's VM-placed reviewer was never exercised end to end; its copy source was the daemon's WORKSPACE (a 72.8 GiB folder of repositories), so every review failed at the 8 GiB ceiling and the queue could land nothing. -->

# The reviewer's VM was booted on the workspace, and no test had ever booted one

MEASURED 2026-10-10, from the queue's own words three times for two branches:
`io: refusing to copy /home/dead/Projects/letibot: it weighs 72.8 GiB after excluding
[target, .claude/worktrees, node_modules, .git/worktrees], and the ceiling is 8.0 GiB`.
The gatekeeper session is seated on the daemon's `--workspace`, which on this box is a FOLDER of
repositories; the repository the queue serves is `/home/dead/Projects/letibot/letibot`, whose own
tree is **1.2 GiB** after the same excludes. So the ceiling was right and the copy source was
wrong. A reviewer that cannot start is written as a FAILED ATTEMPT on the row, so the queue
landed nothing.

## Two findings, and the second is the larger one

1. `Harness::open` filled `FirecodeSpec::source` from `cfg.workspace` for every VM placement —
   there was no knob for *which tree to copy*. A `TaskSpec::vm_source` / `Config::vm_source`
   pair now carries it (`Config::vm_copy_source` is the one spelling), and the review queue sets
   it from `mergequeue::review_copy_source`: the entry's worktree's repository, then
   `repo_root(workspace)`, then `None` (the old behaviour, the workspace).
2. **A plain VM-placed reviewer had never been run end to end on this box.** The placement's
   tests assert what the pass ASKED FOR with a recording runner (`review_pass`), the tests that
   spawn a REAL reviewer say `gatekeeper_vm = false` in as many words (`session_channel`), and
   `tools/tests/firecode_live.rs` is behind `FIRECODE_LIVE=1` and tests the backend, never a
   reviewer's tree. The first time a reviewer was actually booted was the first time the queue
   tried to review — which is why a wrong copy source could sit under a placement that looked
   tested.

## What a copy of this repository weighs (for the ceiling's argument)

`du -sh --exclude=target --exclude=.claude/worktrees --exclude=node_modules
--exclude=.git/worktrees /home/dead/Projects/letibot/letibot` → **1.2 GiB** (`.git` included,
226 MiB of it). The folder above it: **72.8 GiB** measured by `rsync --dry-run --stats`. 8 GiB is
~6x a repository and 1/9th of the folder, which is the shape the ceiling exists to refuse.
