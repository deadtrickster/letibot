<!-- abstract: A sweep's may-I-delete must be answered about every commit the thing holds - the ref's tip and the worktree's HEAD - and `Proof::Unproven` is a third answer, never folded into there is nothing there. -->

# The sweep proved the ref, and not the tree — and "cannot name it" was read as "may go"

`release_terminal_resources` → `release_one` → `settle_tree` → `branch_may_go`
(`crates/harnessd/src/mergequeue/serve.rs`), and the answer is three-valued:
`git::Proof::{On, Gone, Unproven}` out of `ref_proof`. `Gone` is permission; `Unproven` is not.
`settle_tree` decides on the commits and the tree follows them, and `git::head_of` is the
worktree's own HEAD, whose `Err` means it could not be read - which the sweep keeps for.

A blocked landing is skipped outright, because `MergeState::Blocked` is `is_open`, so a report
that the sweep deleted a blocked row's branch was not the proof's fault.

`git::delete_branch` is the only thing in the tree that deletes a ref, and it has three callers:
`settle_tree` (proved), `landed` → `retire_branch` (the row's `landed_sha`, with no ancestry
proof), and `supersede_fix_branches` → `retire_if_empty` (`rev-list --count tip..head == 0`, the
same fact in another spelling).
