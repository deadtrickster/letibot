<!-- abstract: Sweeping a terminal queue row's worktree: a same-state `move_review` rewrites `state_ms` that two readers key on (use `Store::record_transition`), and a pruned `worktrees/<name>` makes the tree unreadable rather than removable. -->

# A terminal row's resources, and the three keys that keep a sweep quiet

Sweeping a terminal queue row's worktree and branch is safe only after `merge-base --is-ancestor
<branch> main`, and the dirty check is `git status` in the tree. Two things make the sweep lie.

A same-state `move_review` rewrites `state_ms`, and `state_ms` is load-bearing: `Store::cas` writes
it unconditionally and it is not in `REVIEW_SETTABLE`, so a `Move` cannot put it back. Two readers
key on it - `ui::panes::queue` shows a terminal row only while `now - state_ms <= RECENT_MS` (24
hours), and `serve::ring_the_projects` keys its once-only ring on `row:<id>:<state>:<state_ms>`. A
note on a terminal row goes through `Store::record_transition` (`from == to`, history only).

A pruned `worktrees/<name>` makes the tree unreadable, not removable: remove
`.git/worktrees/<name>` and leave the directory, and `git -C <wt> status` answers `fatal: not a git
repository`, so the dirty check cannot run and `git worktree remove` refuses. The rail that keeps a
fallback honest is the tree's own `.git` FILE (`gitdir: .../worktrees/<name>`), which the main
checkout can never pass because its `.git` is a directory. Keep and say, do not guess.

And one tree is one piece of news: `landed` and `supersede_fix_branches` both raise
`queue_branch_kept`, so the shared key is `kept:<repository>:<worktree>` through
`RepoQueue::first_time`, which survives a restart.
