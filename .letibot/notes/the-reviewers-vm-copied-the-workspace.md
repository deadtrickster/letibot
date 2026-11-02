<!-- abstract: A VM placement copies one tree, and `Config::vm_copy_source` is the one answer: the caller's `vm_source`, else the repository the workspace is about, else the workspace. -->

# The reviewer's VM was booted on the workspace, and no test had ever booted one

`Harness::open` is the only reader of the copy source, so setting the tree on a review child
alone left every other opener copying the daemon's workspace - and the gatekeeper is seated on
`--workspace`, which on this box is a FOLDER of repositories (72.8 GiB) while the repository the
queue serves is 1.2 GiB. A reviewer that cannot start is written as a FAILED ATTEMPT on the row,
so the queue landed nothing.

`Config::vm_copy_source` now answers `vm_source`, else
`mergequeue::repo_of_workspace(&workspace)`, else the workspace. `repo_of_workspace` is
`repo_root` first, then the nested layout by NAME - the same-named child, then the one
repository child - and `repo_root` fails on a folder.

Rake: a refusal that quotes a failure after "was rung:" is not saying who was placed. The placed
session is the review CHILD; the rung session is the entry's root. Read
`merge_review.session_id` in the store, not the sentence.

And no test had booted a reviewer's VM before the queue did. `Config::for_this_box` states no
VM; a test that means to exercise a placement calls `Config::allow_vm_reviewers` by name.
