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

## The refusal NAMED a session that was not the session being placed — and the store proves it

`mergequeue::no_gatekeeper`'s log line reads *"no gatekeeper for `<branch>` (entry `m-…`) — the
session `s-…` that `m-…` came from was rung: `<why>`"*, and `why` is the REVIEW's own last failure.
So `placing this session in a firecode VM: io: refusing to copy /home/dead/Projects/letibot` reads
as though the session that was RUNG (the entry's root, `s-1789462738453908838` — the daemon's own
`--session`) had been placed in a VM. It had not: the placed session is the review CHILD, a `task`
child of `s-gatekeeper`. Measured 2026-10-10 from the store, not from the sentence:

    sqlite3 /home/dead/.local/share/letibot/sessions.db \
      "select entry_id, session_id, attempts, failure from merge_review
        where branch like '%tests-do-not-boot%';"
    → session_id `s-gatekeeper`, attempts 3, failure = the firecode refusal verbatim

and `merge_queue.session_id` for that entry is `s-1789462738453908838-sub-1791667894381955618`
(the child that did the work), whose ROOT is the `s-…` the sentence names. **A sentence that quotes
a failure after "was rung:" is not saying who was placed.**

## And the running daemon was older than the fix

The daemon that refused was started 22:42 from a binary built 22:39; `a07523a` is 23:26. Nothing in
that refusal was measured against the fixed code, so it says nothing about whether `a07523a` works.
It would: that entry's worktree is `<repo>/.claude/worktrees/agent-tests-do-not-boot-vms`, so
`repo_of_worktree` names the repository and `review_copy_source`'s first arm fires.

## The resolution moved out of the review pass and into `Config::vm_copy_source`

`a07523a` set the tree only on a REVIEW child (`TaskSpec::vm_source`), so every other opener that
places a session in a VM still copied the workspace: the daemon's eagerly-opened first session
(`Sessions::open_first_with_seat`), a head's `ResumeSession` and the merge queue's project session
(both `Sessions::harness_as`), the review queue's own session (`Sessions::ensure_gatekeeper`, whose
`open_as` is `harness_as` one arm over), and every `task` child. `Harness::open` is the only reader
of the copy source, so the resolution belongs where it reads it: `Config::vm_copy_source` now
answers `vm_source`, else `mergequeue::repo_of_workspace(&workspace)`, else the workspace.
`repo_of_workspace` is `repo_root` first — which **FAILS** on `/home/dead/Projects/letibot` (the
queue's log says `serving /home/dead/Projects/letibot toward main`, which is that failure written
down) — and then the nested layout by NAME: the same-named child, then the one repository child.
Measured on this box: the folder has FIVE repository children (`letibot` + `wt-drill`, `wt-jobs`,
`wt-visibility`, `wt-yellow`), so the one-child arm cannot answer here and the same-named child
(`letibot/letibot`) is what does.
