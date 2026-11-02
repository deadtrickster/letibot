<!-- abstract: The ten branch refs this note recorded are gone, so what is left is what the batch proved: a semantic conflict is two sides both right, and a superset branch and the branch it contains must not both land. -->

# The wedged batch: ten branch refs put back on `main`, and what verified each

The refs are gone - `git show-ref` names none of the ten - so this is the rules the batch
proved, and
`a-plain-rebase-replays-the-old-target-and-onto-with-the-right-fork-still-replays-merges.md`
holds the mechanism.

A semantic conflict is not a textual one: both sides changed behaviour and both were right.
Main's new rule that a verdict about a tree that is gone is not a verdict met a fixture that
spelled "a reviewer with no tree" as an empty `cwd`, which the rule read as exactly that. The
fix is a decision about the fixture, and the operator's to take.

A rebase does not fix a red whose cause is a rule another commit added. `git range-diff` before
and after is the check: `=` everywhere means the patch is what the gate judged, so a red at the
rebased tip is a real red.

"Already landed" looks exactly like "conflicts", and a rebase cannot tell them apart. The test
is a patch comparison -
`diff <(git diff <old main> <branch>) <(git diff <the commit the rewrite landed at> <its tip>)`
- and byte-identical means there is nothing to land and the row should be withdrawn.

A superset branch and the branch it contains must not both land: the fix would arrive twice.
