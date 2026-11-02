<!-- abstract: A reviewer's tree is decided in `start_reviewer` alone, the one place a `ReviewRow` becomes a `TaskSpec`, so a fixture that starts a reviewer needs a real git repository and `spec.worktree` is not neutral. -->

# A reviewer's tree is decided in exactly one function, and everything else follows

`ReviewRow` becomes a `TaskSpec` in one place, `mergequeue::review::start_reviewer`, and a child's working directory comes from `TaskSpec::worktree` / `vm_source` at the spawn.

For a host placement review.rs leaves both `None`, so the reviewer's `read`/`grep`/`glob` defaulted to the daemon's workspace while its `bash` was told the operator's checkout. No setting fixes that from outside: `ReviewerPlacement` is the value review.rs consumes, so adding a variant breaks its match, and a brief that says "do not touch review.rs" is asking for two incompatible things.

A fixture that starts a reviewer needs a real git repository, because the host arm clones the row's repository at `head_sha`; three fixtures named `/the/repository` had been measuring the review pass without exercising the tree it judges. `spec.worktree` is not neutral either: it carries the `task_start` contract, so the child also gets `worktree_build_env` and `deliverable::Deliverable`, which nudges it to commit into a branch.

`Config::review_clones` (like `Config::landing_root`) exists so a fixture driving the real `Harness::serve_reviews` does not write into the box's directory.
