<!-- abstract: MEASURED 2026-10-29 building `[queue] reviewers = host|vm`: a reviewer's tree is decidable only in `mergequeue/review.rs::start_reviewer` (the one place a ReviewRow becomes a TaskSpec), so any fixture that starts a reviewer needs a REAL git repository — three didn't and were measuring "a reviewer that could not start"; the per-review clone was a 530 MB leak for BOTH placements; and `spec.worktree` drags in `worktree_build_env` + the `deliverable` nudge. -->

# A reviewer's tree is decided in exactly one function, and everything else follows

MEASURED 2026-10-29, branch `agent/reviewers-run-on-the-host-by-default` (the operator's ruling
that reviewers run on the host by default). Four findings that cost real time and will cost it
again.

## 1. There is no seam outside `mergequeue/review.rs` for *which tree a reviewer gets*

`ReviewRow` (the only thing holding `head_sha`) becomes a `TaskSpec` in exactly one place:
`mergequeue::review::start_reviewer`. A child's working directory comes from
`TaskSpec::worktree` / `TaskSpec::vm_source` at the spawn
(`run_to_completion`: `if let Some(wt) = &spec.worktree { sub_cfg.workspace = wt.path }`), and
review.rs leaves both `None` for a host placement — so the reviewer's `read`/`grep`/`glob`
defaulted to the DAEMON's workspace while its `bash` was told `cwd: <the operator's checkout>`.

No change to `ReviewerPlacement`, `Config` or `[queue]` settings can fix that: `ReviewerPlacement`
is the value review.rs consumes, and adding a variant or a field to its `Host` arm breaks
review.rs's match and forces the same edit. A brief that says "do not touch review.rs" and "a host
reviewer must work in a checkout of `head_sha`" is asking for two incompatible things — ask before
you pick.

## 2. A fixture that starts a reviewer now needs a REAL git repository

The host arm clones the row's repository at `head_sha` (`git::clone_branch`, the same helper the VM
arm uses). Three fixtures named `/the/repository` and `head_sha = "head-<id>"`, and they failed
with the row BLOCKED and *"git clone of `agent/x`: fatal: repository '/the/repository' does not
exist"* — i.e. they had been measuring the review pass without ever exercising the tree it judges.
Fixed by giving each fixture a repo (`mergequeue::serve::tests::{repo, branch}` for lib unit tests;
a hand-rolled `git init` for the integration test `tests/session_channel.rs`, where `cfg(test)` does
not reach). The assertions that named `"base...head-m-mine"` became `format!("...{head}")` — the
property is unchanged, the string was a made-up sha.

## 3. The per-review clone was a leak for BOTH placements — 530 MB, eleven dirs

`<tmp>/letibot-review-clones/review-<row id>` was removed by nobody: `clone_branch` clears a dir
only when the SAME row id is reviewed again (never, once a row has a verdict), and the firecode copy
that *is* removed (`FirecodeBackend::remove_copy`) is firecode's own project directory.
MEASURED on the operator's box: 11 dirs, 530 MB, the oldest four days old. The fix is a sweep at
the top of `review::serve` — remove `review-<id>` when no row is `waiting`/`reviewing` — which is
the only shape that also catches a WITHDRAWN row (moved in `mergequeue/serve.rs`) and what a dead
daemon left. Package K's sweep reclaims terminal rows' worktrees in the queue's pass; this one
reclaims clones in the review pass; neither can see the other's directory.

**The guard is a NAME, not a path**: `sweep_clones` refuses any directory not called
`CLONES_DIR` (`letibot-review-clones`). That kills the whole class in one rule — the temp root
itself (which three fixtures passed as `clones`), a parent of it, and any future caller's guess.
And the class is real: a test run of the *unfixed* suite swept the operator's real 530 MB before
the fixtures were pointed at directories of their own.

## 4. `spec.worktree` is not a neutral way to say "the child works here"

`TaskSpec::worktree` carries the `task_start` contract, so a reviewer given one also gets
`worktree_build_env` (harmless here: a clone's `.git` is its own, so `CARGO_TARGET_DIR` is the
clone's own `target/`) and `deliverable::Deliverable` — the end-of-turn `git status` check that
nudges a child to commit into its branch. A reviewer in a detached clone that leaves a scratch file
is nudged to "commit as you go" into nothing. Suppressing it needs a field on `TaskSpec` that means
*a tree the child may not commit into*; until then it is a comment in `start_reviewer` and noise a
reviewer may occasionally see.

## Also worth knowing

* `Config::review_clones` (like `Config::landing_root`) exists so a fixture driving the real
  `Harness::serve_reviews` does not write into — or sweep — the box's own directory. `cfg(test)`
  cannot do this job: it does not reach an integration test.
* `[queue] reviewers` is read once per DAEMON (`Config::for_the_daemon` → `settings::for_repository`
  of its workspace), not per row: `Pool::placement` is one value per pass. A daemon seated above
  several repositories has no one project file, so `--reviewers` is how an operator says it there.
