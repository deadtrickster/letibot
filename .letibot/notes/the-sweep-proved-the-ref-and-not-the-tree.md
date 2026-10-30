<!-- abstract: MEASURED 2026-10-29 (branch agent/the-sweep-cannot-delete-unmerged-work): the queue's resource sweep DOES take `merge-base --is-ancestor` for the ROW's branch ref (a blocked landing is skipped outright — `MergeState::Blocked` is `is_open`), so the branch proof was already there; what was NOT proved is the WORKTREE's own commits — a tree on a detached HEAD, or under a branch the row does not name, is removed by `git worktree remove --force` with no proof at all, and `branch_head()==None` (which includes "git cannot read this repo") is spelled the same as "already gone" and read as permission. -->

# The sweep proved the ref, and not the tree — and "cannot name it" was read as "may go"

MEASURED 2026-10-29, branch `agent/the-sweep-cannot-delete-unmerged-work` (worktree of the same
name, `main` at `ed540f8e`), while chasing a report that the resource sweep deleted the branch of a
**blocked** landing (`agent/a-lost-reply-fails-fast`).

## What the tree actually does, and the probe that says so

`mergequeue/serve.rs`'s `release_terminal_resources` → `release_one` → `settle_tree` →
`branch_may_go`, and `branch_may_go` DOES take the proof:

```rust
match git::is_ancestor(&self.repo, &head, &tip) { Ok(true) => None /* may go */, Ok(false) => kept, Err => kept }
```

A fixture that reproduces the report exactly — a real branch with one commit, `enqueue`, a green
pass to claim it, `accept`, then a `KilledAtCapGate` landing pass → `MergeState::Blocked` → three
more passes — leaves **the tree and the ref alone, with no release note on either row**. And the
reason is not the proof: `MergeState::Blocked` is `is_open()` (`tokencore/src/store/queues.rs`), so
the sweep skips the row entirely, and the review row's `landing_open` skips it too.

## The two holes that are real

1. **The worktree is removed with no proof about the commits it holds.** `settle_tree` proves the
   row's BRANCH and then `git::remove_worktree` → `git worktree remove --force`. A tree whose HEAD
   is detached (a commit on no branch), or is checked out on a branch the row does not name, is
   deleted whole — the commits survive only in `.git`'s reflog and `fsck --lost-found`.
2. **"git cannot name this ref" is spelled the same as "the ref is gone".** `branch_head` is
   `rev_parse` and swallows every error, so an unreadable repository, an unreadable ref and an
   absent ref all answer `None`, and `branch_may_go` answers `None` — *may go* — for all three.
   Only "the ref is not there" is permission; the other two are a proof that could not be taken.

## The other ref-deleting paths, so nobody looks twice

`git::delete_branch` is the ONLY thing in the tree that deletes a ref (`git branch -D`), and it has
three callers: `settle_tree` (the proof above), `landed` → `retire_branch` (guarded by
`branch_head == landing.head` and a successful compare-and-swap — but no ancestry proof, so a
rebased landing deletes the pre-rebase commits on the strength of the row's `landed_sha`), and
`supersede_fix_branches` → `retire_if_empty` (`rev-list --count tip..head == 0`, which is the same
fact as ancestry in a different spelling).

## The rake for the next session

A sweep's "may I delete this?" must be answered about **every commit the thing holds** — the ref's
tip AND the worktree's HEAD — and "I could not tell" must be a THIRD answer, never folded into
"there is nothing there".
