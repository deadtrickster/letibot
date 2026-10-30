<!-- abstract: MEASURED 2026-10-30: `review::verdict` (681d722f) turns a verdict into `waiting for the reviewer's tree` when the desk entry's `cwd` is not a non-empty directory — so a fixture that opens a desk with `cwd: ""` never approves, and the VM arm of `start_reviewer` sets exactly that, which leaves every VM reviewer's verdict unwritable on main's tip. -->

# A desk entry's `cwd` is the tree the verdict is about, and `""` is a tree that is gone

MEASURED 2026-10-30 on lab2x1, chasing why `agent/a-verdict-waiting-is-not-a-dead-reviewer`
failed the landing gate twice (23:21:41 on 2026-10-29, 12:39:28 today) with only the tail of the
step kept.

## The rule, and where it bites

`crates/harnessd/src/mergequeue/review.rs`'s `verdict()` (added by `681d722f`, 2026-10-29 22:32)
asks, before it writes anything, whether the tree the reviewer was GIVEN still exists:

```rust
let tree = Path::new(&req_cwd);
let there = tree.is_dir() && read_dir(tree).next().is_some();
if !there { p.wait_for_tree(r, why)?; return Ok(()); }
```

`req_cwd` is `ReviewRequest::cwd` — for a HOST reviewer, the clone `start_reviewer` made
(`dir.display()`); for a VM reviewer, `String::new()`, because inside the guest there is no
absolute path to name.

So a desk entry opened with `cwd: ""` is a reviewer whose tree is "gone": the row moves to
`waiting` with `waiting for the reviewer's tree: … the tree it was given (``) is gone`, no attempt
is spent, and the next pass starts another reviewer — for ever.

## Rake 1 — the failure reads as a verdict that did not land, not as a fixture bug

The branch's two tests assert `state == Approved` after a pass of `review::serve` and panic there:

```
panicked at crates/harnessd/src/mergequeue/serve/tests.rs:1894:5:
assertion `left == right` failed: the verdict landed: ReviewRow { … state: Waiting, … }
  left: Waiting
 right: Approved
```

The reason is not in the assert; it is in the row's `failure`/`evidence` sentence, which names the
empty path. **Read the row the assert printed before deciding the rule is wrong.** Main's own
fixture had been fixed with the rule (`review/tests.rs::judge`: *"a fixture naming `\"\"` would be
asserting a verdict about a tree that does not exist, which is now a WAIT"*) — a branch written
before `681d722f` and rebased onto it gets no such edit, and its tests fail deterministically
(0.13 s, both, every run, on any box).

## Rake 2 — the VM arm still sets `cwd: ""`, and nothing tests it

`start_reviewer`'s `ReviewerPlacement::Vm` arm sets `cwd: String::new()` deliberately. On main's
tip that means **a VM-placed reviewer's verdict can never be written**: `verdict()` sees no tree,
waits, re-seats, starts another reviewer. `[queue] reviewers = vm` is reachable config, and no
test in the tree covers a VM verdict (`grep ReviewerPlacement::Vm crates/harnessd/src/mergequeue/
review/tests.rs` → nothing). If that is fixed, the fix belongs in `verdict()` (ask only when the
placement gave the child a path) rather than in each fixture.

## Rake 3 — the gate's excerpt is the TAIL, so `--no-fail-fast` hides the name

With `--no-fail-fast` cargo keeps going after a failed target, so the failing target's
`failures:` block is in the MIDDLE of the step output and the tail is the last target's `ok`. The
queue stored 647510 bytes "dropped from the front" and therefore showed the queue nothing. The
full log is on the gate host, `~/letibot-remote/gate/<repo-hash>/run/step-N.out` (rewritten by
every run, so read it while the run is the one you care about).
