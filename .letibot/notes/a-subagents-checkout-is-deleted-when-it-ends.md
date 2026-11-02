<!-- abstract: MEASURED 2026-11-02 on lubuntu1: a `task` subagent runs in its OWN worktree, which is removed when its session ends, so uncommitted work is lost - one child's 36 edited notes vanished; a brief that says "do not commit" must say "commit before you finish" instead. -->

# A subagent's checkout is deleted when its session ends, so "do not commit" loses the work

MEASURED 2026-11-02 on lubuntu1, a docs pass fanned out to thirteen `task` children.

- A child does NOT share your working directory. It gets its own worktree of the same bare
  repository (`~/letibot-remote/<repo>/<placement id>`, branch `remote/<id>`) and its own model
  session. `pwd` inside the child is how you find out, and its answer is not your path.
- That worktree is REMOVED when the child's turn ends: `git worktree list` then shows it
  `prunable`, the directory is gone, and any uncommitted change with it. One child's 36 edited
  notes were lost that way, and the report it had already written still ended "36 uncommitted
  change(s) are still only on that machine".
- The branch it leaves behind has 0 commits on top of your base, so there is nothing to
  cherry-pick. `[subagent placement]` at the end of a child's report says exactly this, in the
  line AFTER the one that sounds like the work is safe.

So a brief that wants to collect a child's work must say commit, not "do not commit": `git add` the
named files (never `-A`), `git rm` the ones deleted, and commit on the child's own branch with the
right author. Harvest with `git log <child branch>` and cherry-pick onto your branch.

The rule: a child's work is only what its branch holds. Anything you cannot see in a
`git log <child branch>` does not exist, however complete the child's answer sounds.
