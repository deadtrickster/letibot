<!-- abstract: `review::verdict` waits instead of writing when the desk entry's `cwd` is not a non-empty directory, and the VM arm of `start_reviewer` sets exactly that, so a VM reviewer's verdict can never be written. -->

# A desk entry's `cwd` is the tree the verdict is about, and `""` is a tree that is gone

`crates/harnessd/src/mergequeue/review.rs`'s `verdict()` asks, before writing anything, whether the
tree the reviewer was GIVEN still exists (`is_dir()` and a non-empty `read_dir`). If not it calls
`wait_for_tree`: the row moves to `waiting` with `waiting for the reviewer's tree`, spends no attempt,
and the next pass starts another reviewer, for ever.

`req_cwd` is the clone `start_reviewer` made for a host reviewer, and `String::new()` for a VM reviewer,
because inside the guest there is no absolute path to name. So a fixture that opens a desk with
`cwd: ""` never approves, and `[queue] reviewers = vm` is a config whose verdicts cannot be written on
this tree. If that is fixed, the fix belongs in `verdict()` - ask only when the placement gave the child
a path - and not in each fixture.

Rake: the failure reads as a verdict that did not land, and the reason is in the row's `failure`
sentence, which names the empty path - read the row the assert printed before deciding the rule is
wrong.
