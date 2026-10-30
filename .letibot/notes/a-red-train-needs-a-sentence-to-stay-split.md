<!-- abstract: MEASURED 2026-10-31 building `[queue] gate_train`: a red train that puts its rows back in `queued` re-forms and fails again forever, so the split needs a marker; and a pass that applies a verdict goes on to TAKE the next row in the same pass — which is why "each row is queued after a red train" is only true in the history. -->

# A red train needs a sentence to stay split, and a pass does not stop at a verdict

MEASURED 2026-10-31, branch `agent/one-gate-for-the-whole-queue` (`crates/harnessd/src/mergequeue/serve.rs`), building `[queue] gate_train`: N approved rows rebased one onto the last, gated ONCE, landed one commit at a time.

## 1. "Back in line" is a loop unless the row says why it is there

The brief's shape — a red train dissolves and its rows go back to individual queue rows — has a hole with no bottom: a `queued` row is a candidate for the next train, so the same N rows re-form the same train and fail the same gate, 12 minutes at a time, for ever. The queue looks busy and lands nothing.

The guard is one sentence and one filter: each row goes back to `Queued` with evidence leading with `TRAIN_SPLIT` ("this branch was in a train whose gate was red"), and `land_next` will not put a row carrying it into a train. Its own single gate overwrites the evidence whatever it says, so the sentence lasts exactly until the branch has been judged alone — which is the one fact a red train could not produce (WHICH branch is bad). The next pass therefore gates the split rows one at a time, and a green one lands while the bad one ends `failed`.

## 2. A pass that applies a verdict does not stop there

`pass` reaps the gate FIRST and then carries on: `adopt_orphans`, `review_upkeep`, … `health`, `land_next`. So the pass that dissolves a red train also takes the first split row and starts its gate. "Each merge row is back in `queued` after a red train" is therefore true of the **history** and not of the instant after the pass — a test that asserts `state == Queued` on all three rows fails on the first row, which is already `gating`.

`RepoQueue::settle` (the tests' driver) is worse: it loops until no gate is in flight, so `settle` once runs the red train AND the three individual gates. The test that wants to look at the dissolution has to drive `q.pass()` itself, sleep 20 ms while a gate is in flight (a pass does not wait for its gate — twelve tight passes all land before a `git` step finishes), and stop at the pass that filed the train's review row.

## 3. The train's review row must name the tree the gate judged, or it cannot be reproduced

The failing evidence is about a tree that exists nowhere else: the rebases happen in the landing worktree, detached. So `red_train` creates `agent/train-<target>-<short rN>` at the final car's commit and the review row is a `Branch` row about it — a reviewer started on it works in the exact tree the gate judged. Naming the first branch instead (cheaper, no new ref) hands the reviewer a tree the failure does not reproduce in.

The row is `waiting`, so the review pool claims it and starts a gatekeeper on it; if it is approved the merge queue lands that branch, and by then the individual branches' own rebases come out empty (`patches_unchanged`'s `after.0 == after.1` arm) and land as no-ops — no duplicate commits. A rejected train's branch lingers: the resource sweep skips a row with no `worktree`, so nothing deletes it.

## The rake

**A row put back in a queue is a row that will be taken again.** Any dissolve-and-requeue needs to say, on the row, why it must not be re-batched — and the marker must be cleared by the path that finally judges it, not by a timer.
