<!-- abstract: `review-<row id>` was one path per row and every writer of it was a remover, so a re-seat deleted the tree a dying reviewer was still reading; trees are now one per attempt. -->

# A review's tree was one path per ROW, and three processes could remove it

`review-<row id>` was one name and every writer of it was a remover: `clone_branch` began with `remove_dir_all`, a re-seat re-cloned to the same path, and `sweep_clones` removed it once the row was no longer live.

The same daemon reached the first of those with the previous reviewer still alive, because `TaskRunner::kill` submits an interrupt - the child's turn ends at its next round boundary - while the desk entry has already gone. So the next pass cloned and deleted the tree the dying child was still reading. Four reviews in twenty minutes died that way.

The fix is one tree per ATTEMPT, `review-<row id>-<pid>-<n>`, made beside the name and renamed on; nothing is removed there. `sweep_clones` keeps every tree of a live row whatever its age, and a dead row's trees go after `CLONE_GRACE`. A clone that fails is a WAIT (`WAITING_FOR_TREE`, no attempt spent), `start_reviewer` proves the tree's `HEAD` is the row's head, and `verdict` refuses a verdict about a tree that is gone or empty.

Rake: a path is a place two processes - or one process at two moments - can both be told to write. A `kill` that submits an interrupt is not a dead child.
