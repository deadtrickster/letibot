<!-- abstract: MEASURED 2026-10-27 in wt-fb on lab2x1: a file being edited through the placement went to 0 bytes twice after the write channel failed — the edit tool's "nothing was written" is not always true, so `wc -c` after every write, and `git checkout -- <file>` plus a redo is the recovery. -->

# A placed session's `edit` can truncate the file it was told not to touch

MEASURED 2026-10-27, wt-fb (branch `fanout-settles`), a session whose tools run on lab2x1 over the
git placement.

`crates/tools/src/builtins/fanout.rs` went to **0 bytes twice** in one afternoon. Both times the
preceding tool result said the file was fine:

* once, an `edit` that reported `io: writing … lost the connection before it answered
  (mm_send_fd: sendmsg(1): Message too long)` together with *"Nothing on disk changed: the write is a
  temporary file renamed over the target, so a failure leaves the original untouched"* — and a later
  `sed` showed the file still intact, so that one really was untouched;
* the second time the report was `[edit: found no match … Nothing was written]`, and the next `wc -c`
  said `0`.

So the claim above is not reliable across the placement: **a failed write can leave the target
truncated**, and the failure that leaves it is not the one that says so.

## What to do about it

1. **`wc -c` (or `wc -l`) after any write to a file you care about**, and after a *failed* one
   especially. A zero-byte file is silent: the next `grep` answers "no match", which reads like a
   wrong pattern.
2. Recovery is one line, and it is safe because the branch's commit is the base of truth:
   `git checkout -- <path>`, then redo the edits. Nothing else in the worktree is affected.
3. **Keep each write small.** The two failures came at payloads of ~20 KB and ~6 KB, so size is not
   the whole story — the channel was also dropping unrelated calls (`the process did not start …
   .pid was not written within 2 s`) in the same minutes. A big edit is still the one that costs the
   most to redo, and splitting one large insertion into a *new file* (`write`) plus small edits to the
   existing one survived every retry.

## And the shape this pushed me into, which is better anyway

The background fanout's machinery went into `crates/tools/src/builtins/fanout_bg.rs` — one file,
written once — instead of 500 lines inserted into `fanout.rs`. That is a module boundary the code
wanted: `fanout.rs` is the tool, `fanout_bg.rs` is a second *lifetime* over the same plan.
