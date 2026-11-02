<!-- abstract: `git rebase <onto>` bases itself on merge-base(branch, onto), which is a commit on the OLD target once the target is rewritten; `--onto` with the right fork fixes the linear case but still replays merges. -->

# A plain rebase replays the old target, and `--onto` with the right fork still replays merges

`git rebase <onto>` takes `merge-base(branch, onto)` for its base. After the target is rewritten that merge base is a commit on the old target, so the rebase replays the old target onto the new one and the branch's own commit rides along, conflicting in files it never touched.

The base a branch was cut from has to be remembered, because no property of the current graph names it. The row's `onto_sha`, the review row's `placement.base_sha`, and the target ref's reflog (`git rev-list -g refs/heads/<target>`) all remember it. The fork is the first of those on the branch's FIRST-PARENT spine, because a branch that merged the target has newer target commits in its ancestry than its own fork. There is no fallback to `merge-base` - that is the defect - so a branch whose base nobody remembers gets a sentence, not a guess.

`--onto` is still not "the branch's commits": its todo list is `rev-list --no-merges fork..head`, so what a merge of the old target brought in is replayed too. `rebase_in` (`crates/harnessd/src/mergequeue/git.rs`) compares that list with `--no-merges --first-parent --reverse fork..head` and cherry-picks one at a time when they differ.
