<!-- abstract: MEASURED/DECIDED 2026-10-29: four reviews in twenty minutes died because `review-<row id>` was ONE path per row and `clone_branch` removed it before re-cloning — a re-seat re-clones while the previous reviewer is still alive (`kill` is an interrupt, the desk entry goes first). Fixed by one tree per ATTEMPT (`review-<id>-<pid>-<n>`), a sweep that keeps every live row's trees, a clone failure as a WAIT, and a verdict refused when the reviewer's tree is gone. -->

# A review's tree was one path per ROW, and three processes could remove it

MEASURED/DECIDED 2026-10-29, branch `agent/a-clone-cannot-be-pulled-out-from-under-itself`
(worktree of the same name, `main` at `f7397ad5`). Four reviews in twenty minutes died with their
clone gone from under them:

```text
git clone of `agent/a-one-line-citation-is-filled`:
  fatal: could not open '…/letibot-review-clones/review-s-…-sub-1793298319066201000/.git/objects/pack/tmp_pack_cgbKfF'
         for reading: No such file or directory
  fatal: fetch-pack: invalid index-pack output
```

and, for two more, a reviewer already working: `bash` answering *the process did not start: No such
file or directory*, `read`/`glob` reporting a session root with **0 entries**, and a `cannot_decide`
about a branch nobody had looked at.

## The cause: `review-<row id>` is ONE name, and three writers used it

`git::clone_branch` began with `if dir.exists() { remove_dir_all(dir) }` — "a second review of the
same row replaces the clone". Every writer of that name was therefore a remover:

* a re-seat of the row re-clones to the same path;
* `sweep_clones` removes it once the row is not `waiting`/`reviewing`;
* and the *same daemon* reaches the first of those with the previous reviewer still alive —
  `TaskRunner::kill` **submits an interrupt**, so the child's turn ends at its next round boundary,
  while `p.desk.keep(..)` has already dropped the desk entry for it. The next pass finds
  `reviewer: None`, clones, and deletes the tree the dying child is still reading.

That last one corrects an earlier reading of mine: `review::serve` is `&mut self` and one pass per
daemon, so a pass cannot overlap itself — but the window is ACROSS passes, and it is inside one
daemon. No second daemon is needed.

## What the fix is

* **One tree per ATTEMPT**, named `review-<row id>-<pid>-<n>`, made beside the name and renamed on
  (`git::clone_branch` returns the path). Nothing is ever removed there. The token is `<pid>-<n>`
  and not the reviewer's handle because the handle does not exist yet: a child's working directory
  is decided before it is spawned.
* **`sweep_clones` keeps every tree of a live row**, whatever its age — `waiting`/`reviewing` is the
  only state in which a reviewer can be standing in one. A dead row's trees go once older than
  `CLONE_GRACE` (10 min; the anchor is a MEASURED 1.2 s clone of this repo, so ~500×). The price,
  said plainly: a row re-seated twice keeps its earlier trees until it stops being live.
* **A clone that fails is a WAIT**: `CouldNotStart::Tree` → `Pool::wait_for_tree`, `failure` leading
  with `WAITING_FOR_TREE` (which `capacity::held_until` now reads, so the claim holds the row for
  1/2/5 minutes) and no attempt spent. Decision 4's three attempts are for a review that cannot RUN.
* **`start_reviewer` proves the tree** (it exists and `HEAD` is the row's head) before a child is
  given it; a tree that is not that is a wait.
* **`verdict` refuses a verdict about a tree that is not there** — gone, or **empty**, which is the
  shape `read`/`glob` reported — and writes *the review environment went away under the reviewer*
  on the row instead of a `cannot_decide` that reads like a judgement of the branch.

## The rake

**A path is a place two processes — or one process at two moments — can both be told to write.**
When a tree's own `.git` reports ENOENT for a file it just wrote, grep the `remove_dir_all`s that
name the same path, and then ask *which actor, at which moment* each belongs to before asking which
line is wrong. And: a `kill` that submits an interrupt is not a dead child; anything that removes
what a child was using has to outlive the child, not the handle.
