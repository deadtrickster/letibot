<!-- abstract: MEASURED 2026-10-29 (package K, branch agent/queue-releases-resources): the queue's resource census (57 terminal rows naming a worktree, nine dirs, 36 GB of which 32 GB was one tree), and the three rakes under sweeping them — a same-state `move_review` rewrites `state_ms` (a done row comes back in the pane for 24 h and a `cannot_decide` row re-rings its project), a pruned `worktrees/<name>` makes `git status` in the tree answer "not a git repository" (so the dirty check cannot run), and `git branch -d` succeeding is proof a branch was merged. -->

# A terminal row's resources, and the three keys that keep a sweep quiet

MEASURED 2026-10-29, branch `agent/queue-releases-resources` (package K), on the operator's Mac,
against `~/.local/share/letibot/sessions.db` (read-only) and the two worktree folders.

## The census, and what a sweep would actually free

`review_queue`, terminal rows naming a `worktree`: **57** (5 `approved`, 52 `withdrawn`; one
`rejected` names none). `~/Projects/letibot/worktrees` was **36 GB** — **32 GB of it ONE review's
tree** — and `~/Projects/stroppy-io/worktrees` another **5.9 GB**. The operator cleared eight by
hand, 36 GB → 95 MB.

After that, 5 of the 57 directories still exist, and the sweep's own rules decide them like this
(a read-only dry run: `merge-base --is-ancestor <branch> main`, plus `git status` in the tree):

| verdict | size | tree |
|---|---|---|
| MERGED | 5.4 / 5.5 / 5.9 MB | `stroppy-rust` × 3 → released |
| MERGED, **DIRTY** | 8.9 MB | `M crates/runtime/tests/backends.rs` → kept, raised |
| CARRIES 1 commit | 32 MB | `letibot/worktrees/agent-plugin-tool-mount` → kept, raised |

`git cherry main agent/plugin-tool-mount` answers `+` — the patch really is not upstream, so
keeping that 32 MB is the difference between a sweep and `rm -rf`.

**The eight the operator removed by hand were all merged**, and their own commands prove it:
`git branch -d` refuses an unmerged branch, and they used `-d`. So `merge-base --is-ancestor` is the
test that would have taken the whole 36 GB.

## Rake 1 — a same-state `move_review` rewrites `state_ms`, and `state_ms` is load-bearing

`Store::cas` writes `state_ms = now` unconditionally, and `state_ms` is not in `REVIEW_SETTABLE`
(so a `Move` cannot put it back). Two readers key on it:

* `ui::panes::queue::queue_rows` shows a terminal row only while `now - state_ms <= RECENT_MS`
  (**24 hours**) — a note written as a same-state move resurrects a two-day-old `withdrawn` row as
  "just finished";
* `serve::ring_the_projects` keys its once-only ring on `row:<id>:<state>:<state_ms>` — a
  `cannot_decide` row would wake its project session again.

So a note on a terminal row goes through **`Store::record_transition`** (`from == to`, history
only). The trap predates this package: `serve.rs`'s `merge_via_session` hand-over already writes
`Blocked → Blocked` with a new `why`, and pays both costs.

## Rake 2 — a pruned `worktrees/<name>` makes the tree unreadable, not removable

Remove `.git/worktrees/<name>` and leave the directory: `git -C <wt> status` answers
`fatal: not a git repository`, so the dirty check cannot run at all, and `git worktree remove`
refuses. The tempting fallback (`remove_dir_all`) would delete a tree nobody checked. The rail that
makes it safe is the tree's own `.git` FILE (`gitdir: <main>/.git/worktrees/<name>`) — which is
also why the main checkout can never pass it (its `.git` is a directory). Keep and say, do not
guess.

## Rake 3 — one tree is one piece of news, or `landed` says it twice

`landed` and `supersede_fix_branches` raise `queue_branch_kept` when they keep a tree, and the
sweep sees the same tree in the same or the next pass. The shared key is
`kept:<repository>:<worktree>` (through `RepoQueue::first_time`, so it survives a restart): whoever
says it first says it, and the existing tests that count `said.len() == 1` still count 1. Without
it, every landing whose child session is still parked raises twice.
