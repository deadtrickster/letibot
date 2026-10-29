<!-- abstract: MEASURED/DECIDED 2026-10-29: a reviewer's clone died on its own `.git/objects/pack/tmp_pack_*` because two removers wrote to the one name `review-<id>`; within a daemon `serve` is one `&mut` pass so the second writer is another daemon — `recover_queue` re-seats a `reviewing` row whose owner is not `me` with no liveness check, and a sweep reads the store. Fixed by clone-beside-then-rename, a 10-minute grace, and a clone failure being a WAIT (`WAITING_FOR_TREE`) rather than one of the three attempts. -->

# A review's clone had two removers, and both wrote to one name

MEASURED/DECIDED 2026-10-29, branch `agent/a-clone-cannot-be-pulled-out-from-under-itself`
(worktree of the same name, `main` at `f7397ad5`), from the operator's own notice:

```text
a reviewer for `agent/a-one-line-citation-is-filled` could not start:
git clone of `agent/a-one-line-citation-is-filled`:
  fatal: could not open '…/letibot-review-clones/review-s-…-sub-1793298319066201000/.git/objects/pack/tmp_pack_cgbKfF'
         for reading: No such file or directory
  fatal: fetch-pack: invalid index-pack output
```

## The one thing that narrows the search: a pass cannot overlap itself inside a daemon

`mergequeue::review::serve` takes `&Pool` and is reached through `Harness::serve_reviews`, which is
`&mut self` — one pass per `Harness` at a time — and the review queue's session is one `Court`, one
thread, which is also where `wake` and the round boundary call it. **So the sweep at the top of a
pass can never run while that same daemon is inside `start_reviewer`'s clone**, and neither can a
second `start_reviewer` for the same row. Every mechanism below therefore needs a SECOND PROCESS in
the same `$TMPDIR/letibot-review-clones` — which is the design (`Config::review_clones` is
`temp_dir()/letibot-review-clones` for every daemon, and `sweep_clones` reads every row on the
shared store, not `p.me`'s).

## The two removers, and what each needs

* `git::clone_branch`'s own `if dir.exists() { remove_dir_all(dir) }` — needs a **second clone of
  the same row**. `recover_queue` (`tokencore/src/store/queues.rs`) moves a `reviewing` row whose
  `owner != me` back to `waiting` **with no liveness check** — "the daemon whose reviewer was on it
  is gone" — so any lease handover (a restart, a lapsed lease taken over) lets the new owner claim
  the row and clone into the same `review-<id>` while the old owner's git is still writing. Nothing
  but a restart is required for this one.
* `review::sweep_clones` — needs the row to be **not `waiting`/`reviewing` while its clone is being
  written**: a veto, `supersede_older_tip`, a `block`. Rarer, and it has a second precondition (a
  second daemon sweeping), but it is real: the sweep reads the store, and the store's answer about
  a row is not a fact about a tree that is still being written.

The notice's path cannot tell them apart — both removers remove `<clones>/review-<id>` wholesale,
and git's `tmp_pack_*` lives inside it. The store can: `queue_transition` for that row says either
`reviewing → waiting` ("the daemon whose reviewer was on it … is gone") then a re-claim (the first),
or `reviewing → withdrawn|blocked` (the second).

## What the fix is, and the two numbers

* `clone_branch` clones into `<clones>/review-<id>.tmp-<pid>-<n>` and **renames** onto the name;
  a tree already there is moved aside first and removed after. Nothing writes into `review-<id>`.
* `sweep_clones` reads the id only up to the first `.` (so `.tmp-`/`.old-` are never swept by the
  row rule) and keeps anything younger than `CLONE_GRACE` = **10 minutes**. MEASURED for the
  anchor: `git clone --no-local --single-branch` of this repo at a branch head is **1.2 s**
  (52 MB tree, 226 MiB `.git`, warm cache); 10 min is ~500× that and 2× `REVIEWER_START_GRACE_MS`.
* A clone failure is now `CouldNotStart::Tree` → `Pool::wait_for_tree`: row back to `waiting`, no
  attempt spent, `failure` leading with `WAITING_FOR_TREE` so `capacity::held_until` (the claim's
  not-before reader — one clause wider now) holds it for 1/2/5 minutes. Unbounded on purpose: a
  repository that is gone leaves the row waiting with the clone's words on it, rather than being
  blocked as *a review that could not run* — which is a claim about the branch.

## The rake

**A name is a place two processes can both be told to write.** The clone directory is one global
directory, the row id is the only thing in the name, and two daemons can hold the same row at two
different moments of one lease handover. When a tree's own `.git` reports ENOENT for a file it just
wrote, look for the `remove_dir_all` calls that name the same path, and ask which PROCESS each one
belongs to before asking which line is wrong.
