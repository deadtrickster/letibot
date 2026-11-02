<!-- abstract: In a rebase the conflict markers read the other way round from a brief's usual telling - `<<<<<<< HEAD` is the commit being rebased onto and `>>>>>>>` is ours - and the breakage that costs the build can have no marker. -->

# Rebasing the box's main onto GitHub's: two stops, one inverted brief, and a breakage no conflict marker showed

In a rebase, `<<<<<<< HEAD` is the commit being rebased onto and `>>>>>>> <sha> <subject>` is ours. Read the other way, our own text looks like the remote's, and "take the remote's shape" deletes a seam the tree is built on: `mergequeue/gate_host.rs` imports `{Done, assemble, judge, StepEnd, StepMemory}` from `gate`, and the remote never had them. The resolution there was the remote's mechanics with our seam - keep both helpers, take the newer body around them.

The expensive breakage had no marker. A commit that added `GateControl.budget` patched the two construction sites that existed on our main, and the remote's two newer gate tests also construct `GateControl` - so the rebased tree failed with missing-field errors in a file git called clean. Fold such a fix into the commit that introduces the field, so every commit compiles.

Two facts worth keeping: `cargo test … | tail` reports `tail`'s exit status, so a red build reads as exited 0 unless you use `pipefail` or a log file; and a rebased branch cannot be fast-forwarded from the branch it rewrites, so the target has to be moved with `reset --hard` after a backup ref.
