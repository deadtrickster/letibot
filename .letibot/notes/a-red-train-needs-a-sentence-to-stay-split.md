<!-- abstract: A red train whose rows go back to `queued` re-forms and fails the same gate for ever, so the split needs a sentence on the row that only the row's own single gate clears. And a pass does not stop at a verdict. -->

# A red train needs a sentence to stay split, and a pass does not stop at a verdict

`[queue] gate_train` rebases N approved branches one onto the last. Dissolving a red train back into individual rows has a hole with no bottom: a `queued` row is a candidate for the next train, so the same rows re-form and fail the same gate for ever.

The guard is one sentence and one filter. Each row goes back to `Queued` with evidence leading with `TRAIN_SPLIT`, and `land_next` will not put a row carrying it into a train. Its own single gate overwrites the evidence, so the sentence lasts exactly until the branch has been judged alone - which is the one fact a red train cannot produce, WHICH branch is bad.

`pass` reaps the gate first and then carries on, so the pass that dissolves a train also starts the first split row's gate. "Every row is back in `queued` after a red train" is true of the history and not of the instant after the pass. `RepoQueue::settle` loops until no gate is in flight, so a test that wants the dissolution must drive `q.pass()` and stop at the pass that filed the review row.

The train's review row is a `Branch` row about `agent/train-<target>-<short rN>` at the final car's commit, so a reviewer works in the tree the gate judged.
